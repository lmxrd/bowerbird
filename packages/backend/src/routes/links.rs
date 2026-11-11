//! Public links (docs/M6B_DESIGN.md §3): a file or folder reached without
//! signing in, at `/s/<token>` (a page of the web app, which calls
//! `/api/s/<token>/…`).
//!
//! - **view**: the file (preview and download), or the folder's listing,
//!   its files and the folder as a zip;
//! - **edit**: the same, and upload, new folder, rename and delete in it;
//! - **drop**: upload only, nothing of the folder shown.
//!
//! A link is found by the SHA-256 of its token. It stops working when it
//! expires, while its owner is disabled, and when its file or folder (or
//! its owner) goes: the row goes with them. A password (argon2) is asked
//! once per browser session: the right one is remembered in a signed
//! session cookie (`organizer_links`), until the password changes; every
//! try counts against the sign-in limits of the address. Everything a
//! link does runs in its owner's tree, on the owner's limit, and stays
//! inside the shared folder. Files are served as downloads are: `nosniff`
//! and a sandbox CSP.

use crate::error::{ErrorBody, HttpError};
use crate::extract::ApiJson;
use crate::files::shares::Need;
use crate::files::{Kind, NODE_COLUMNS, Node, ops::WriteTarget};
use crate::routes::auth::AuthUser;
use crate::routes::files::{access, content_disposition, serve_file, temp_upload, write_field};
use crate::routes::shares::Person;
use crate::state::AppState;
use crate::util::sha256_hex;
use axum::Json;
use axum::body::Body;
use axum::extract::{ConnectInfo, Multipart, Path, Query, State};
use axum::http::{HeaderMap, HeaderValue, StatusCode, header};
use axum::response::{IntoResponse, Response};
use axum_extra::extract::cookie::{Cookie, SameSite, SignedCookieJar};
use serde::{Deserialize, Serialize};
use std::net::SocketAddr;
use utoipa::{IntoParams, ToSchema};
use utoipa_axum::router::OpenApiRouter;
use utoipa_axum::routes;
use uuid::Uuid;

/// Nested under /files: a file's or folder's links.
pub fn files_router() -> OpenApiRouter<AppState> {
    OpenApiRouter::new().routes(routes!(list_links, create_link))
}

/// Nested under /links.
pub fn router() -> OpenApiRouter<AppState> {
    OpenApiRouter::new().routes(routes!(update_link, delete_link))
}

/// Nested under /s: the link pages' own API, without signing in.
pub fn public_router() -> OpenApiRouter<AppState> {
    OpenApiRouter::new()
        .routes(routes!(link_info))
        .routes(routes!(unlock_link))
        .routes(routes!(link_children))
        .routes(routes!(link_download))
        .routes(routes!(link_zip))
        .routes(routes!(link_upload))
        .routes(routes!(link_new_folder))
        .routes(routes!(link_rename, link_delete))
        // Uploads check their own limit while they stream.
        .layer(axum::extract::DefaultBodyLimit::disable())
}

/// What a link lets anyone with it do.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize, ToSchema, sqlx::Type)]
#[serde(rename_all = "lowercase")]
#[sqlx(type_name = "TEXT", rename_all = "lowercase")]
pub enum LinkKind {
    /// See and download.
    View,
    /// See, download, upload, make folders, rename and delete.
    Edit,
    /// Upload only; nothing is shown.
    Drop,
}

/// A link, as its owner (or whoever made it) sees it.
#[derive(Serialize, ToSchema)]
pub struct Link {
    pub id: Uuid,
    pub node_id: Uuid,
    pub kind: LinkKind,
    /// The page is `/s/<token>`.
    pub token: String,
    pub has_password: bool,
    /// When it stops working; null: never. Always present.
    #[schema(required = true)]
    pub expires_at: Option<String>,
    pub created_at: String,
    /// Who made it. Always present.
    #[schema(required = true)]
    pub created_by: Option<Person>,
    /// Whether the user may change or remove it.
    pub editable: bool,
}

#[derive(sqlx::FromRow)]
struct LinkRow {
    id: Uuid,
    node_id: Uuid,
    drive_id: Uuid,
    created_by: Option<Uuid>,
    token: String,
    kind: LinkKind,
    password_hash: Option<String>,
    expires_at: Option<String>,
    created_at: String,
}

const LINK_COLUMNS: &str =
    "id, node_id, drive_id, created_by, token, kind, password_hash, expires_at, created_at";

/// The link settings of a drive for a user (docs/M7B_DESIGN.md §4):
/// whether links may be made, whether they need a password, and their
/// longest life in days (0: no limit).
async fn link_rules(
    state: &AppState,
    user: Option<Uuid>,
    drive: Uuid,
) -> Result<(bool, bool, i64), HttpError> {
    let env = crate::settings::Env::of_state(state);
    let to = crate::settings::For {
        user,
        drive: Some(drive),
    };
    let get = |key: &'static str| {
        let env = env.clone();
        async move { crate::settings::resolve(&state.db, &env, key, to).await }
    };
    Ok((
        get("links.allowed").await?.bool(),
        get("links.password_required").await?.bool(),
        get("links.max_days").await?.int(),
    ))
}

/// A link as the drive's link settings allow it: refused when links are
/// off there (403), without a password where one is required, or living
/// longer than the longest allowed (400).
async fn check_rules(
    state: &AppState,
    user: Uuid,
    drive: Uuid,
    has_password: bool,
    expires_at: Option<&str>,
) -> Result<(), HttpError> {
    let (allowed, password, max_days) = link_rules(state, Some(user), drive).await?;
    if !allowed {
        return Err(HttpError::new(
            StatusCode::FORBIDDEN,
            "Public links are turned off here",
        ));
    }
    if password && !has_password {
        return Err(HttpError::bad_request(
            "A public link needs a password here",
        ));
    }
    if max_days > 0 {
        let latest = format!(
            "{:.3}",
            jiff::Timestamp::now() + jiff::SignedDuration::from_hours(24 * max_days)
        );
        match expires_at {
            Some(at) if at <= latest.as_str() => {}
            _ => {
                return Err(HttpError::bad_request(format!(
                    "A public link lives at most {max_days} days here: give it an expiry"
                )));
            }
        }
    }
    Ok(())
}

async fn link(state: &AppState, me: Uuid, row: LinkRow) -> Result<Link, HttpError> {
    let created_by: Option<Person> = match row.created_by {
        Some(id) => {
            sqlx::query_as("SELECT id, email FROM users WHERE id = ?1")
                .bind(id)
                .fetch_optional(state.db.reader())
                .await?
        }
        None => None,
    };
    Ok(Link {
        editable: row.created_by == Some(me)
            || state
                .files
                .access(me, row.node_id)
                .await
                .is_ok_and(|a| a.allows(Need::Share)),
        id: row.id,
        node_id: row.node_id,
        kind: row.kind,
        token: row.token,
        has_password: row.password_hash.is_some(),
        expires_at: row.expires_at,
        created_at: row.created_at,
        created_by,
    })
}

/// A time a link expires at: RFC 3339, stored as the times are.
fn expiry(value: &str) -> Result<String, HttpError> {
    let at: jiff::Timestamp = value
        .parse()
        .map_err(|_| HttpError::bad_request("expires_at: expected a date and time (RFC 3339)"))?;
    Ok(format!("{at:.3}"))
}

/// A password for a link: at least 4 characters, hashed with argon2.
fn link_password(password: &str) -> Result<String, HttpError> {
    if password.chars().count() < 4 {
        return Err(HttpError::bad_request(
            "A link's password is at least 4 characters",
        ));
    }
    Ok(crate::auth::hash_password(password))
}

/// Whether a link of a kind can be made on a node by someone with this
/// access: an editor or above (`Need::Link`), never giving more than they
/// have.
fn check_kind(kind: LinkKind, node: &crate::files::shares::Access) -> Result<(), HttpError> {
    let folder = node.node.kind == Kind::Folder;
    let ok = match kind {
        LinkKind::View => true,
        LinkKind::Edit => {
            folder
                && node.allows(Need::Create)
                && node.allows(Need::Change)
                && node.allows(Need::Delete)
        }
        LinkKind::Drop => folder && node.allows(Need::Create),
    };
    if ok {
        return Ok(());
    }
    if !folder {
        return Err(HttpError::bad_request(
            "Edit and file drop links are for folders",
        ));
    }
    Err(HttpError::new(
        StatusCode::FORBIDDEN,
        "You cannot give more than you have",
    ))
}

#[utoipa::path(
    get,
    path = "/{id}/links",
    tag = "Sharing",
    operation_id = "listLinks",
    summary = "A file's or folder's public links",
    description = "For its editors and above.",
    params(("id" = Uuid, Path, description = "The file or folder id")),
    responses(
        (status = 200, description = "Its links, the oldest first", body = Vec<Link>),
        (status = 401, description = "Not signed in", body = ErrorBody),
        (status = 403, description = "A viewer or upload only", body = ErrorBody),
        (status = 404, description = "Not found", body = ErrorBody),
    ),
    security(("session" = []), ("api_key" = [])),
)]
async fn list_links(
    State(state): State<AppState>,
    user: AuthUser,
    Path(id): Path<Uuid>,
) -> Result<Json<Vec<Link>>, HttpError> {
    access(&state, &user, id, Need::Link).await?;
    let rows: Vec<LinkRow> = sqlx::query_as(sqlx::AssertSqlSafe(format!(
        "SELECT {LINK_COLUMNS} FROM links WHERE node_id = ?1 ORDER BY created_at, id"
    )))
    .bind(id)
    .fetch_all(state.db.reader())
    .await?;
    let mut out = Vec::with_capacity(rows.len());
    for row in rows {
        out.push(link(&state, user.0.id, row).await?);
    }
    Ok(Json(out))
}

/// A new link.
#[derive(Deserialize, ToSchema)]
pub struct NewLink {
    pub kind: LinkKind,
    /// Asked once per browser session; none by default.
    pub password: Option<String>,
    /// When it stops working (RFC 3339); never by default.
    pub expires_at: Option<String>,
}

#[utoipa::path(
    post,
    path = "/{id}/links",
    tag = "Sharing",
    operation_id = "createLink",
    summary = "Make a public link",
    description = "View (a file or a folder), edit or file drop (a folder), by an editor or above, giving at most what they have; as the drive's link settings allow (`links.allowed`, `links.password_required`, `links.max_days`).",
    params(("id" = Uuid, Path, description = "The file or folder id")),
    request_body(content = NewLink, content_type = "application/json"),
    responses(
        (status = 201, description = "Made", body = Link),
        (status = 400, description = "Edit or file drop on a file, a short password, an invalid time, no password or expiry where the settings need one", body = ErrorBody),
        (status = 401, description = "Not signed in", body = ErrorBody),
        (status = 403, description = "A viewer, more than you have, or links are turned off there", body = ErrorBody),
        (status = 404, description = "Not found", body = ErrorBody),
        (status = 415, description = "The body must be JSON (Content-Type: application/json)", body = ErrorBody),
    ),
    security(("session" = []), ("api_key" = [])),
)]
async fn create_link(
    State(state): State<AppState>,
    user: AuthUser,
    Path(id): Path<Uuid>,
    ApiJson(body): ApiJson<NewLink>,
) -> Result<Response, HttpError> {
    let node = access(&state, &user, id, Need::Link).await?;
    // Another server's file is shared by its server only (OCM §8.3).
    if node.node.remote.is_some() {
        return Err(HttpError::bad_request(
            "This is on another server: only its owner's server can share it",
        ));
    }
    crate::files::e2e::refuse_encrypted(&node.node, "public links")?;
    check_kind(body.kind, &node)?;
    let expires_at = body.expires_at.as_deref().map(expiry).transpose()?;
    check_rules(
        &state,
        user.0.id,
        node.owner,
        body.password.is_some(),
        expires_at.as_deref(),
    )
    .await?;
    let password_hash = body.password.as_deref().map(link_password).transpose()?;
    let token = crate::util::random_hex(24);
    let link_id = Uuid::now_v7();
    sqlx::query(
        "INSERT INTO links (id, node_id, drive_id, created_by, token_hash, token, kind,
           password_hash, expires_at, created_at)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10)",
    )
    .bind(link_id)
    .bind(id)
    .bind(node.owner)
    .bind(user.0.id)
    .bind(sha256_hex(&token))
    .bind(&token)
    .bind(body.kind)
    .bind(password_hash)
    .bind(expires_at)
    .bind(crate::auth::now_iso())
    .execute(state.db.writer())
    .await?;
    let row = load(&state, link_id).await?;
    Ok((
        StatusCode::CREATED,
        Json(link(&state, user.0.id, row).await?),
    )
        .into_response())
}

async fn load(state: &AppState, id: Uuid) -> Result<LinkRow, HttpError> {
    sqlx::query_as(sqlx::AssertSqlSafe(format!(
        "SELECT {LINK_COLUMNS} FROM links WHERE id = ?1"
    )))
    .bind(id)
    .fetch_optional(state.db.reader())
    .await?
    .ok_or_else(HttpError::not_found)
}

/// A link the user may change: one they made (and still may make), or any
/// for a manager of it.
async fn editable(
    state: &AppState,
    user: &AuthUser,
    id: Uuid,
) -> Result<(LinkRow, crate::files::shares::Access), HttpError> {
    let row = load(state, id).await?;
    let node = access(state, user, row.node_id, Need::Link).await?;
    if !(node.allows(Need::Share) || row.created_by == Some(user.0.id)) {
        return Err(HttpError::new(
            StatusCode::FORBIDDEN,
            "Only a manager or who made it can change this link",
        ));
    }
    Ok((row, node))
}

/// What to change of a link; what is left out stays.
#[derive(Deserialize, ToSchema)]
pub struct LinkChange {
    pub kind: Option<LinkKind>,
    /// A new password; null: none.
    #[serde(default, deserialize_with = "crate::util::present")]
    #[schema(value_type = Option<String>, nullable)]
    pub password: Option<Option<String>>,
    /// A new expiry (RFC 3339); null: never.
    #[serde(default, deserialize_with = "crate::util::present")]
    #[schema(value_type = Option<String>, nullable)]
    pub expires_at: Option<Option<String>>,
}

#[utoipa::path(
    patch,
    path = "/{id}",
    tag = "Sharing",
    operation_id = "updateLink",
    summary = "Change a public link",
    description = "Its kind, password or expiry. A new password asks every browser for it again.",
    params(("id" = Uuid, Path, description = "The link id")),
    request_body(content = LinkChange, content_type = "application/json"),
    responses(
        (status = 200, description = "Changed", body = Link),
        (status = 400, description = "A short password, an invalid time, edit or drop on a file", body = ErrorBody),
        (status = 401, description = "Not signed in", body = ErrorBody),
        (status = 403, description = "Not the owner or who made it, or more than they have", body = ErrorBody),
        (status = 404, description = "Not found", body = ErrorBody),
        (status = 415, description = "The body must be JSON (Content-Type: application/json)", body = ErrorBody),
    ),
    security(("session" = []), ("api_key" = [])),
)]
async fn update_link(
    State(state): State<AppState>,
    user: AuthUser,
    Path(id): Path<Uuid>,
    ApiJson(body): ApiJson<LinkChange>,
) -> Result<Json<Link>, HttpError> {
    let (row, node) = editable(&state, &user, id).await?;
    let kind = body.kind.unwrap_or(row.kind);
    check_kind(kind, &node)?;
    let password_hash = match &body.password {
        None => row.password_hash,
        Some(None) => None,
        Some(Some(password)) => Some(link_password(password)?),
    };
    let expires_at = match &body.expires_at {
        None => row.expires_at,
        Some(None) => None,
        Some(Some(at)) => Some(expiry(at)?),
    };
    check_rules(
        &state,
        user.0.id,
        row.drive_id,
        password_hash.is_some(),
        expires_at.as_deref(),
    )
    .await?;
    sqlx::query("UPDATE links SET kind = ?1, password_hash = ?2, expires_at = ?3 WHERE id = ?4")
        .bind(kind)
        .bind(password_hash)
        .bind(expires_at)
        .bind(id)
        .execute(state.db.writer())
        .await?;
    let row = load(&state, id).await?;
    Ok(Json(link(&state, user.0.id, row).await?))
}

#[utoipa::path(
    delete,
    path = "/{id}",
    tag = "Sharing",
    operation_id = "deleteLink",
    summary = "Remove a public link",
    description = "It stops working at once.",
    params(("id" = Uuid, Path, description = "The link id")),
    responses(
        (status = 204, description = "Removed"),
        (status = 401, description = "Not signed in", body = ErrorBody),
        (status = 403, description = "Not the owner or who made it", body = ErrorBody),
        (status = 404, description = "Not found", body = ErrorBody),
    ),
    security(("session" = []), ("api_key" = [])),
)]
async fn delete_link(
    State(state): State<AppState>,
    user: AuthUser,
    Path(id): Path<Uuid>,
) -> Result<StatusCode, HttpError> {
    editable(&state, &user, id).await?;
    sqlx::query("DELETE FROM links WHERE id = ?1")
        .bind(id)
        .execute(state.db.writer())
        .await?;
    Ok(StatusCode::NO_CONTENT)
}

// ---------------------------------------------------------------------------
// The link pages' API
// ---------------------------------------------------------------------------

/// The cookie that remembers the passwords given in this browser session.
fn cookie_name(secure: bool) -> &'static str {
    if secure {
        "__Host-organizer_links"
    } else {
        "organizer_links"
    }
}

/// What the cookie holds for a link: its id and a tag of its password
/// hash, so that a new password asks again.
fn unlock_tag(link: &LinkRow) -> String {
    let hash = link.password_hash.as_deref().unwrap_or_default();
    format!("{}:{}", link.id.simple(), &sha256_hex(hash)[..16])
}

/// The links unlocked in this browser session, from the signed cookie.
fn unlocked(state: &AppState, headers: &HeaderMap) -> Vec<String> {
    SignedCookieJar::from_headers(headers, state.challenge_key.clone())
        .get(cookie_name(state.secure_cookies))
        .map(|c| c.value().split(',').map(String::from).collect())
        .unwrap_or_default()
}

/// A link that works: found, not expired, its owner not disabled.
async fn find(state: &AppState, token: &str) -> Result<LinkRow, HttpError> {
    let row: Option<LinkRow> = sqlx::query_as(sqlx::AssertSqlSafe(format!(
        "SELECT {LINK_COLUMNS} FROM links l
         WHERE token_hash = ?1
           AND EXISTS (SELECT 1 FROM users u WHERE u.disabled_at IS NULL AND u.id =
                         coalesce((SELECT user_id FROM drives WHERE id = l.drive_id), l.created_by))"
    )))
    .bind(sha256_hex(token))
    .fetch_optional(state.db.reader())
    .await?;
    let gone = || HttpError::new(StatusCode::NOT_FOUND, "This link does not work (any more)");
    let row = row.ok_or_else(gone)?;
    if row
        .expires_at
        .as_deref()
        .is_some_and(|at| at <= crate::auth::now_iso().as_str())
    {
        return Err(gone());
    }
    // Links turned off where it is (by the superadmin, its team or its
    // drive): it stops working.
    let (allowed, _, _) = link_rules(state, None, row.drive_id).await?;
    if !allowed {
        return Err(gone());
    }
    Ok(row)
}

/// A working link whose password, if it has one, was given in this browser
/// session; refused (401) otherwise.
async fn open(state: &AppState, token: &str, headers: &HeaderMap) -> Result<LinkRow, HttpError> {
    let row = find(state, token).await?;
    if row.password_hash.is_some() && !unlocked(state, headers).contains(&unlock_tag(&row)) {
        return Err(HttpError::new(
            StatusCode::UNAUTHORIZED,
            "This link needs its password",
        ));
    }
    Ok(row)
}

/// A node of the link's folder (or the link's file), or not found.
async fn inside(state: &AppState, link: &LinkRow, id: Uuid) -> Result<Node, HttpError> {
    let node = state.files.node(link.drive_id, id).await?;
    if id != link.node_id && !state.files.is_within(id, link.node_id).await? {
        return Err(HttpError::not_found());
    }
    // An encrypted folder in a linked one is not part of the link.
    if node.e2e_root.is_some() {
        return Err(HttpError::not_found());
    }
    Ok(node)
}

/// Refused unless the link is of one of these kinds.
fn allow(link: &LinkRow, kinds: &[LinkKind]) -> Result<(), HttpError> {
    if kinds.contains(&link.kind) {
        Ok(())
    } else {
        Err(HttpError::new(
            StatusCode::FORBIDDEN,
            "This link does not allow that",
        ))
    }
}

/// What a link page shows first.
#[derive(Serialize, ToSchema)]
pub struct LinkInfo {
    pub kind: LinkKind,
    /// A password is needed and was not given in this browser session yet:
    /// nothing else is said.
    pub locked: bool,
    /// The file or folder's name; null while locked. Always present.
    #[schema(required = true)]
    pub name: Option<String>,
    /// The file or folder (view and edit links); null while locked and for
    /// a file drop. Always present.
    #[schema(required = true)]
    pub node: Option<Node>,
    #[schema(required = true)]
    pub expires_at: Option<String>,
}

#[utoipa::path(
    get,
    path = "/{token}",
    tag = "Links",
    operation_id = "getLink",
    summary = "What a public link is",
    params(("token" = String, Path, description = "The link's token")),
    responses(
        (status = 200, description = "The link", body = LinkInfo),
        (status = 404, description = "No such link, or it stopped working", body = ErrorBody),
    ),
)]
async fn link_info(
    State(state): State<AppState>,
    Path(token): Path<String>,
    headers: HeaderMap,
) -> Result<Json<LinkInfo>, HttpError> {
    let row = find(&state, &token).await?;
    let locked =
        row.password_hash.is_some() && !unlocked(&state, &headers).contains(&unlock_tag(&row));
    if locked {
        return Ok(Json(LinkInfo {
            kind: row.kind,
            locked,
            name: None,
            node: None,
            expires_at: row.expires_at,
        }));
    }
    let node = state.files.node(row.drive_id, row.node_id).await?;
    Ok(Json(LinkInfo {
        kind: row.kind,
        locked,
        name: Some(node.name.clone()),
        node: (row.kind != LinkKind::Drop).then_some(node),
        expires_at: row.expires_at,
    }))
}

/// A link's password.
#[derive(Deserialize, ToSchema)]
pub struct LinkPassword {
    pub password: String,
}

#[utoipa::path(
    post,
    path = "/{token}/unlock",
    tag = "Links",
    operation_id = "unlockLink",
    summary = "Give a public link's password",
    description = "Right: it is remembered for this browser session. Every try counts against the sign-in limits of the address.",
    params(("token" = String, Path, description = "The link's token")),
    request_body(content = LinkPassword, content_type = "application/json"),
    responses(
        (status = 204, description = "Unlocked for this browser session"),
        (status = 403, description = "The wrong password", body = ErrorBody),
        (status = 404, description = "No such link, or it stopped working", body = ErrorBody),
        (status = 415, description = "The body must be JSON (Content-Type: application/json)", body = ErrorBody),
        (status = 429, description = "Too many tries, try again later", body = ErrorBody),
    ),
)]
async fn unlock_link(
    State(state): State<AppState>,
    ConnectInfo(addr): ConnectInfo<SocketAddr>,
    Path(token): Path<String>,
    headers: HeaderMap,
    ApiJson(body): ApiJson<LinkPassword>,
) -> Result<Response, HttpError> {
    let ip = crate::net::client_ip(&headers, Some(addr.ip()), state.trust_proxy);
    state.limits.address(&ip)?;
    let row = find(&state, &token).await?;
    let Some(hash) = row.password_hash.clone() else {
        return Ok(StatusCode::NO_CONTENT.into_response());
    };
    let password = body.password;
    let right = tokio::task::spawn_blocking(move || crate::auth::verify_password(&password, &hash))
        .await
        .map_err(|e| HttpError::internal(e.to_string()))?;
    if !right {
        return Err(HttpError::new(
            StatusCode::FORBIDDEN,
            "That is not the password",
        ));
    }
    // The links unlocked before stay unlocked; the newest few are kept.
    let tag = unlock_tag(&row);
    let mut tags: Vec<String> = unlocked(&state, &headers)
        .into_iter()
        .filter(|t| !t.starts_with(&tag[..32]))
        .collect();
    tags.push(tag);
    let keep = tags.len().saturating_sub(20);
    let cookie = Cookie::build((cookie_name(state.secure_cookies), tags[keep..].join(",")))
        .path("/")
        .http_only(true)
        .same_site(SameSite::Strict)
        .secure(state.secure_cookies)
        .build();
    let jar = SignedCookieJar::new(state.challenge_key.clone()).add(cookie);
    Ok((jar, StatusCode::NO_CONTENT).into_response())
}

#[derive(Deserialize, IntoParams)]
#[into_params(parameter_in = Query)]
pub struct LinkFolderParams {
    /// A folder inside the link's; the link's own by default.
    pub folder: Option<Uuid>,
}

/// What is in a link's folder.
#[derive(Serialize, ToSchema)]
pub struct LinkChildren {
    /// The folder listed.
    pub folder: Node,
    /// From the link's folder down to this one, its own name first.
    pub trail: Vec<Node>,
    /// Folders first, then by name.
    pub items: Vec<Node>,
}

#[utoipa::path(
    get,
    path = "/{token}/children",
    tag = "Links",
    operation_id = "listLinkChildren",
    summary = "What is in a public link's folder",
    description = "View and edit links of a folder; at most 5,000 items.",
    params(("token" = String, Path, description = "The link's token"), LinkFolderParams),
    responses(
        (status = 200, description = "The folder's items", body = LinkChildren),
        (status = 400, description = "Not a folder", body = ErrorBody),
        (status = 401, description = "The password is needed", body = ErrorBody),
        (status = 403, description = "A file drop link", body = ErrorBody),
        (status = 404, description = "No such link, or not in its folder", body = ErrorBody),
    ),
)]
async fn link_children(
    State(state): State<AppState>,
    Path(token): Path<String>,
    Query(params): Query<LinkFolderParams>,
    headers: HeaderMap,
) -> Result<Json<LinkChildren>, HttpError> {
    let link = open(&state, &token, &headers).await?;
    allow(&link, &[LinkKind::View, LinkKind::Edit])?;
    let folder = inside(&state, &link, params.folder.unwrap_or(link.node_id)).await?;
    if folder.kind != Kind::Folder {
        return Err(HttpError::bad_request("Not a folder"));
    }
    let items: Vec<Node> = sqlx::query_as(sqlx::AssertSqlSafe(format!(
        "SELECT {NODE_COLUMNS} FROM nodes WHERE parent_id = ?1 AND e2e_root IS NULL
         ORDER BY CASE kind WHEN 'folder' THEN 0 ELSE 1 END, name, id LIMIT 5000"
    )))
    .bind(folder.id)
    .fetch_all(state.db.reader())
    .await?;
    let mut trail = vec![folder.clone()];
    let mut at = folder.clone();
    while at.id != link.node_id {
        let Some(parent) = at.parent_id else { break };
        at = state.files.node(link.drive_id, parent).await?;
        trail.push(at.clone());
    }
    trail.reverse();
    Ok(Json(LinkChildren {
        folder,
        trail,
        items,
    }))
}

#[derive(Deserialize, IntoParams)]
#[into_params(parameter_in = Query)]
pub struct LinkDownloadParams {
    /// Shown in the browser rather than saved.
    #[serde(default)]
    pub inline: bool,
}

#[utoipa::path(
    get,
    path = "/{token}/files/{id}/content",
    tag = "Links",
    operation_id = "downloadLinkFile",
    summary = "Download a file of a public link",
    description = "Served as downloads are: ranges, and headers that keep it from running as a page.",
    params(
        ("token" = String, Path, description = "The link's token"),
        ("id" = Uuid, Path, description = "The file: the link's, or one in its folder"),
        LinkDownloadParams,
    ),
    responses(
        (status = 200, description = "The file", content_type = "application/octet-stream"),
        (status = 206, description = "The range asked for", content_type = "application/octet-stream"),
        (status = 400, description = "Not a file", body = ErrorBody),
        (status = 401, description = "The password is needed", body = ErrorBody),
        (status = 403, description = "A file drop link", body = ErrorBody),
        (status = 404, description = "No such link, or not in it", body = ErrorBody),
    ),
)]
async fn link_download(
    State(state): State<AppState>,
    Path((token, id)): Path<(String, Uuid)>,
    Query(params): Query<LinkDownloadParams>,
    headers: HeaderMap,
) -> Result<Response, HttpError> {
    let link = open(&state, &token, &headers).await?;
    allow(&link, &[LinkKind::View, LinkKind::Edit])?;
    let node = inside(&state, &link, id).await?;
    if node.kind != Kind::File {
        return Err(HttpError::bad_request("Not a file"));
    }
    let storage = state.files.storage(link.drive_id).await?;
    let path = state.files.rel_path(id).await?;
    let etag = format!("\"{}\"", node.version);
    serve_file(
        &storage,
        &path,
        &node.name,
        node.mime.as_deref(),
        &etag,
        params.inline,
        &headers,
    )
    .await
}

#[utoipa::path(
    get,
    path = "/{token}/folders/{id}/zip",
    tag = "Links",
    operation_id = "downloadLinkFolder",
    summary = "Download a folder of a public link as a zip",
    params(
        ("token" = String, Path, description = "The link's token"),
        ("id" = Uuid, Path, description = "The folder: the link's, or one in it"),
    ),
    responses(
        (status = 200, description = "The folder as a zip", content_type = "application/zip"),
        (status = 400, description = "Not a folder", body = ErrorBody),
        (status = 401, description = "The password is needed", body = ErrorBody),
        (status = 403, description = "A file drop link", body = ErrorBody),
        (status = 404, description = "No such link, or not in it", body = ErrorBody),
    ),
)]
async fn link_zip(
    State(state): State<AppState>,
    Path((token, id)): Path<(String, Uuid)>,
    headers: HeaderMap,
) -> Result<Response, HttpError> {
    let link = open(&state, &token, &headers).await?;
    allow(&link, &[LinkKind::View, LinkKind::Edit])?;
    let folder = inside(&state, &link, id).await?;
    if folder.kind != Kind::Folder {
        return Err(HttpError::bad_request("Not a folder"));
    }
    let storage = state.files.storage(link.drive_id).await?;
    let base = state.files.rel_path(id).await?;
    let entries = zip_entries(&state, id).await?;
    let (sender, receiver) = tokio::sync::mpsc::channel::<std::io::Result<bytes::Bytes>>(8);
    // The zip is written on a thread of its own, into the response as it
    // goes: nothing is kept on disk. The files are read through the
    // drive's storage, on this runtime.
    let runtime = tokio::runtime::Handle::current();
    std::thread::spawn(move || {
        let error = sender.clone();
        let files = ZipSource {
            runtime,
            storage,
            base,
        };
        if let Err(e) = write_zip(&files, &entries, ChannelWriter(sender)) {
            tracing::info!("a link's zip broke off: {e}");
            let _ = error.blocking_send(Err(std::io::Error::other(e.to_string())));
        }
    });
    let mut response = Body::from_stream(Received(receiver)).into_response();
    let headers = response.headers_mut();
    headers.insert(
        header::CONTENT_TYPE,
        HeaderValue::from_static("application/zip"),
    );
    headers.insert(
        header::CONTENT_DISPOSITION,
        content_disposition(&format!("{}.zip", folder.name), false),
    );
    headers.insert(
        header::X_CONTENT_TYPE_OPTIONS,
        HeaderValue::from_static("nosniff"),
    );
    headers.insert(
        header::CONTENT_SECURITY_POLICY,
        HeaderValue::from_static("sandbox"),
    );
    headers.insert(
        header::CACHE_CONTROL,
        HeaderValue::from_static("private, no-cache"),
    );
    Ok(response)
}

/// What goes in a folder's zip: each path under it, a folder or not, and
/// a file's size; the parents before their children.
async fn zip_entries(state: &AppState, id: Uuid) -> Result<Vec<(String, bool, i64)>, HttpError> {
    let rows: Vec<(String, String, i64)> = sqlx::query_as(
        "WITH RECURSIVE down (id, path, kind, size, depth) AS (
           SELECT id, name, kind, size, 0 FROM nodes WHERE parent_id = ?1 AND e2e_root IS NULL
           UNION ALL
           SELECT n.id, down.path || '/' || n.name, n.kind, n.size, down.depth + 1
           FROM nodes n JOIN down ON n.parent_id = down.id
           WHERE n.e2e_root IS NULL
         )
         SELECT path, kind, size FROM down ORDER BY depth, path",
    )
    .bind(id)
    .fetch_all(state.db.reader())
    .await?;
    Ok(rows
        .into_iter()
        .map(|(path, kind, size)| (path, kind == "folder", size))
        .collect())
}

/// Where a zip's files are read from: a folder of a drive's storage.
struct ZipSource {
    runtime: tokio::runtime::Handle,
    storage: crate::storage::Storage,
    base: String,
}

impl ZipSource {
    /// Copy a file under the folder into the zip, piece by piece.
    fn copy(&self, path: &str, out: &mut impl std::io::Write) -> std::io::Result<()> {
        let rel = crate::storage::join(&self.base, path);
        let (reader, size) = self.runtime.block_on(self.storage.reader(&rel))?;
        let mut at = 0;
        while at < size {
            let end = (at + 8 * 1024 * 1024).min(size);
            let piece = self
                .runtime
                .block_on(reader.read(at..end))
                .map_err(std::io::Error::other)?;
            for chunk in piece {
                out.write_all(&chunk)?;
            }
            at = end;
        }
        Ok(())
    }
}

fn write_zip(
    files: &ZipSource,
    entries: &[(String, bool, i64)],
    out: ChannelWriter,
) -> zip::result::ZipResult<()> {
    use zip::write::SimpleFileOptions;
    let mut zip = zip::ZipWriter::new_stream(out);
    for (path, folder, size) in entries {
        let options = SimpleFileOptions::default()
            .compression_method(zip::CompressionMethod::Deflated)
            .large_file(*size >= u32::MAX as i64);
        if *folder {
            zip.add_directory(path.as_str(), options)?;
            continue;
        }
        // A file gone since it was listed is left out.
        let rel = crate::storage::join(&files.base, path);
        if !files
            .runtime
            .block_on(files.storage.exists(&rel))
            .unwrap_or(false)
        {
            continue;
        }
        zip.start_file(path.as_str(), options)?;
        files.copy(path, &mut zip)?;
    }
    zip.finish()?;
    Ok(())
}

/// The zip's bytes into the response, from the writing thread.
struct ChannelWriter(tokio::sync::mpsc::Sender<std::io::Result<bytes::Bytes>>);

impl std::io::Write for ChannelWriter {
    fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
        self.0
            .blocking_send(Ok(bytes::Bytes::copy_from_slice(buf)))
            .map_err(|_| std::io::Error::other("the download was cancelled"))?;
        Ok(buf.len())
    }

    fn flush(&mut self) -> std::io::Result<()> {
        Ok(())
    }
}

/// The receiving end, as the response body's stream.
struct Received(tokio::sync::mpsc::Receiver<std::io::Result<bytes::Bytes>>);

impl futures_core::Stream for Received {
    type Item = std::io::Result<bytes::Bytes>;

    fn poll_next(
        mut self: std::pin::Pin<&mut Self>,
        cx: &mut std::task::Context<'_>,
    ) -> std::task::Poll<Option<Self::Item>> {
        self.0.poll_recv(cx)
    }
}

/// A name free in a folder: the name, else `name (2).ext`, ...
async fn free_name(state: &AppState, parent: Uuid, name: &str) -> Result<String, HttpError> {
    let name = crate::files::names::new_name(name).map_err(HttpError::bad_request)?;
    let (stem, ext) = match name.rsplit_once('.') {
        Some((stem, ext)) if !stem.is_empty() => (stem.to_string(), format!(".{ext}")),
        _ => (name.clone(), String::new()),
    };
    for n in 1..1000 {
        let candidate = if n == 1 {
            name.clone()
        } else {
            format!("{stem} ({n}){ext}")
        };
        if state
            .files
            .check_free(parent, &candidate, None)
            .await
            .is_ok()
        {
            return Ok(candidate);
        }
    }
    Err(HttpError::conflict(format!("{name} is taken")))
}

/// A file uploaded through a link.
#[derive(Serialize, ToSchema)]
pub struct LinkUploaded {
    /// Its name in the folder (a file drop's gets a number when the name
    /// is taken).
    pub name: String,
}

#[utoipa::path(
    post,
    path = "/{token}/files",
    tag = "Links",
    operation_id = "uploadLinkFile",
    summary = "Upload a file through a public link",
    description = "Edit and file drop links. A multipart form with the file in the part `file`; at most 100 MB. Through a file drop, a taken name gets a number; through edit, it is a conflict.",
    params(("token" = String, Path, description = "The link's token"), LinkFolderParams),
    request_body(content = crate::routes::files::UploadForm, content_type = "multipart/form-data"),
    responses(
        (status = 201, description = "Uploaded", body = LinkUploaded),
        (status = 400, description = "No file part, an invalid name, or not a folder", body = ErrorBody),
        (status = 401, description = "The password is needed", body = ErrorBody),
        (status = 403, description = "A view link", body = ErrorBody),
        (status = 404, description = "No such link, or not in it", body = ErrorBody),
        (status = 409, description = "The name is taken", body = ErrorBody),
        (status = 413, description = "Over 100 MB", body = ErrorBody),
        (status = 507, description = "The owner's storage is full", body = ErrorBody),
    ),
)]
async fn link_upload(
    State(state): State<AppState>,
    Path(token): Path<String>,
    Query(params): Query<LinkFolderParams>,
    headers: HeaderMap,
    mut form: Multipart,
) -> Result<Response, HttpError> {
    let link = open(&state, &token, &headers).await?;
    allow(&link, &[LinkKind::Edit, LinkKind::Drop])?;
    // A file drop puts everything in the link's folder.
    let folder_id = match link.kind {
        LinkKind::Drop => link.node_id,
        _ => params.folder.unwrap_or(link.node_id),
    };
    let folder = inside(&state, &link, folder_id).await?;
    if folder.kind != Kind::Folder {
        return Err(HttpError::bad_request("Not a folder"));
    }
    let invalid = || HttpError::bad_request("Expected a multipart form with the file in `file`");
    while let Some(mut field) = form.next_field().await.map_err(|_| invalid())? {
        if field.name() != Some("file") {
            continue;
        }
        let name = field.file_name().map(String::from).ok_or_else(invalid)?;
        let name = match link.kind {
            LinkKind::Drop => free_name(&state, folder.id, &name).await?,
            _ => name,
        };
        let temp = temp_upload(&state)?;
        if let Err(e) = write_field(&mut field, &temp).await {
            let _ = std::fs::remove_file(&temp);
            return Err(e);
        }
        let node = state
            .files
            .write_file(
                link.drive_id,
                WriteTarget::New {
                    parent: folder.id,
                    name,
                },
                &temp,
            )
            .await?;
        return Ok((StatusCode::CREATED, Json(LinkUploaded { name: node.name })).into_response());
    }
    Err(invalid())
}

/// A new folder through an edit link.
#[derive(Deserialize, ToSchema)]
pub struct LinkNewFolder {
    /// The folder to make it in: the link's, or one in it.
    pub parent_id: Uuid,
    pub name: String,
}

#[utoipa::path(
    post,
    path = "/{token}/folders",
    tag = "Links",
    operation_id = "createLinkFolder",
    summary = "Make a folder through a public link",
    description = "Edit links.",
    params(("token" = String, Path, description = "The link's token")),
    request_body(content = LinkNewFolder, content_type = "application/json"),
    responses(
        (status = 201, description = "Made", body = Node),
        (status = 400, description = "An invalid name, or not a folder", body = ErrorBody),
        (status = 401, description = "The password is needed", body = ErrorBody),
        (status = 403, description = "Not an edit link", body = ErrorBody),
        (status = 404, description = "No such link, or not in it", body = ErrorBody),
        (status = 409, description = "The name is taken", body = ErrorBody),
        (status = 415, description = "The body must be JSON (Content-Type: application/json)", body = ErrorBody),
    ),
)]
async fn link_new_folder(
    State(state): State<AppState>,
    Path(token): Path<String>,
    headers: HeaderMap,
    ApiJson(body): ApiJson<LinkNewFolder>,
) -> Result<Response, HttpError> {
    let link = open(&state, &token, &headers).await?;
    allow(&link, &[LinkKind::Edit])?;
    inside(&state, &link, body.parent_id).await?;
    let node = state
        .files
        .create_folder(link.drive_id, body.parent_id, &body.name)
        .await?;
    Ok((StatusCode::CREATED, Json(node)).into_response())
}

/// A rename through an edit link.
#[derive(Deserialize, ToSchema)]
pub struct LinkRename {
    pub name: String,
    pub if_version: i64,
}

#[utoipa::path(
    patch,
    path = "/{token}/nodes/{id}",
    tag = "Links",
    operation_id = "renameLinkNode",
    summary = "Rename through a public link",
    description = "Edit links; anything in the folder, not the folder itself.",
    params(
        ("token" = String, Path, description = "The link's token"),
        ("id" = Uuid, Path, description = "A file or folder in the link's folder"),
    ),
    request_body(content = LinkRename, content_type = "application/json"),
    responses(
        (status = 200, description = "Renamed", body = Node),
        (status = 400, description = "An invalid name", body = ErrorBody),
        (status = 401, description = "The password is needed", body = ErrorBody),
        (status = 403, description = "Not an edit link, or the link's folder itself", body = ErrorBody),
        (status = 404, description = "No such link, or not in it", body = ErrorBody),
        (status = 409, description = "Changed since that version, or the name is taken", body = ErrorBody),
        (status = 415, description = "The body must be JSON (Content-Type: application/json)", body = ErrorBody),
    ),
)]
async fn link_rename(
    State(state): State<AppState>,
    Path((token, id)): Path<(String, Uuid)>,
    headers: HeaderMap,
    ApiJson(body): ApiJson<LinkRename>,
) -> Result<Json<Node>, HttpError> {
    let link = open(&state, &token, &headers).await?;
    allow(&link, &[LinkKind::Edit])?;
    inside(&state, &link, id).await?;
    if id == link.node_id {
        return Err(HttpError::forbidden());
    }
    Ok(Json(
        state
            .files
            .move_node(link.drive_id, id, None, Some(&body.name), body.if_version)
            .await?,
    ))
}

#[utoipa::path(
    delete,
    path = "/{token}/nodes/{id}",
    tag = "Links",
    operation_id = "deleteLinkNode",
    summary = "Delete through a public link",
    description = "Edit links; anything in the folder, not the folder itself. It goes to the owner's trash.",
    params(
        ("token" = String, Path, description = "The link's token"),
        ("id" = Uuid, Path, description = "A file or folder in the link's folder"),
        crate::routes::files::VersionParams,
    ),
    responses(
        (status = 204, description = "In the owner's trash"),
        (status = 401, description = "The password is needed", body = ErrorBody),
        (status = 403, description = "Not an edit link, or the link's folder itself", body = ErrorBody),
        (status = 404, description = "No such link, or not in it", body = ErrorBody),
        (status = 409, description = "Changed since that version", body = ErrorBody),
    ),
)]
async fn link_delete(
    State(state): State<AppState>,
    Path((token, id)): Path<(String, Uuid)>,
    Query(params): Query<crate::routes::files::VersionParams>,
    headers: HeaderMap,
) -> Result<StatusCode, HttpError> {
    let link = open(&state, &token, &headers).await?;
    allow(&link, &[LinkKind::Edit])?;
    inside(&state, &link, id).await?;
    if id == link.node_id {
        return Err(HttpError::forbidden());
    }
    state
        .files
        .trash(link.drive_id, id, params.if_version)
        .await?;
    Ok(StatusCode::NO_CONTENT)
}
