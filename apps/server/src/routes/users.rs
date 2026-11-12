//! More than one user (docs/M6A_DESIGN.md): the superadmin's list of users and
//! what they do to one, invite links, the switches of how people join, and
//! the ways in (an invite's page, open sign-up). The rules are in
//! `users.rs`.

use crate::auth::hash_password;
use crate::error::{ErrorBody, HttpError};
use crate::extract::ApiJson;
use crate::files::quota::Usage;
use crate::net::client_ip;
use crate::routes::auth::LoginStep;
use crate::routes::auth::{AuthUser, Superadmin, validate_email, validate_password};
use crate::routes::storage::Storage;
use crate::routes::totp::{Step, challenge_cookie, next_step};
use crate::session::{self, AuthSession};
use crate::state::AppState;
use crate::users;
use axum::Json;
use axum::extract::{ConnectInfo, Path, State};
use axum::http::{HeaderMap, StatusCode};
use axum::response::{IntoResponse, Response};
use axum_login::AuthnBackend as _;
use serde::{Deserialize, Serialize};
use std::net::SocketAddr;
use utoipa::ToSchema;
use utoipa_axum::router::OpenApiRouter;
use utoipa_axum::routes;
use uuid::Uuid;

/// The superadmin's: users, invites and the switches, nested under /superadmin.
pub fn superadmin_router() -> OpenApiRouter<AppState> {
    OpenApiRouter::new()
        .routes(routes!(list_users))
        .routes(routes!(update_user, remove_user))
        .routes(routes!(list_invites, create_invite))
        .routes(routes!(revoke_invite))
        .routes(routes!(get_joining, update_joining))
}

/// The ways in and the user's own storage, nested under /auth.
pub fn router() -> OpenApiRouter<AppState> {
    OpenApiRouter::new()
        .routes(routes!(get_invite, accept_invite))
        .routes(routes!(signup))
        .routes(routes!(own_storage))
}

/// A user, as the superadmin's list shows them.
#[derive(Serialize, ToSchema)]
pub struct UserEntry {
    pub id: Uuid,
    pub email: String,
    pub is_superadmin: bool,
    /// When a superadmin disabled them; null: enabled. Always present.
    #[schema(required = true)]
    pub disabled_at: Option<String>,
    /// When they were last active (to the minute); null: never. Always
    /// present.
    #[schema(required = true)]
    pub last_seen_at: Option<String>,
    pub created_at: String,
    /// Whether they sign in with a password.
    pub has_password: bool,
    /// Whether they are linked to an account at the single sign-on
    /// provider.
    pub single_sign_on: bool,
    /// What they take, and their limit.
    pub storage: Usage,
    /// Where their files are.
    pub files: Storage,
}

#[derive(sqlx::FromRow)]
struct UserRow {
    id: Uuid,
    email: String,
    is_superadmin: bool,
    disabled_at: Option<String>,
    last_seen_at: Option<String>,
    created_at: String,
    has_password: bool,
    single_sign_on: bool,
}

const USER_COLUMNS: &str = "id, email, is_superadmin, disabled_at, last_seen_at, created_at,
    password_hash IS NOT NULL AS has_password,
    EXISTS (SELECT 1 FROM user_identities i WHERE i.user_id = users.id) AS single_sign_on";

async fn entry(state: &AppState, row: UserRow) -> Result<UserEntry, HttpError> {
    Ok(UserEntry {
        storage: state.files.usage(row.id).await?,
        files: crate::routes::storage::storage(state, row.id).await?,
        id: row.id,
        email: row.email,
        is_superadmin: row.is_superadmin,
        disabled_at: row.disabled_at,
        last_seen_at: row.last_seen_at,
        created_at: row.created_at,
        has_password: row.has_password,
        single_sign_on: row.single_sign_on,
    })
}

async fn user_entry(state: &AppState, id: Uuid) -> Result<UserEntry, HttpError> {
    let row: UserRow = sqlx::query_as(sqlx::AssertSqlSafe(format!(
        "SELECT {USER_COLUMNS} FROM users WHERE id = ?1"
    )))
    .bind(id)
    .fetch_optional(state.db.reader())
    .await?
    .ok_or_else(HttpError::not_found)?;
    entry(state, row).await
}

#[utoipa::path(
    get,
    path = "/users",
    tag = "Superadmin",
    operation_id = "listUsers",
    summary = "The users",
    description = "Every user, oldest first, with their role, state, storage and files folder.",
    responses(
        (status = 200, description = "The users", body = Vec<UserEntry>),
        (status = 401, description = "Not signed in", body = ErrorBody),
        (status = 403, description = "Superadmin only, with a session (never a token) whose second factor was confirmed within 15 minutes: else `code` is `step_up_required` (confirm it, `/auth/step-up/…`) or `second_factor_required` (add one first)", body = ErrorBody),
    ),
    security(("session" = [])),
)]
async fn list_users(
    State(state): State<AppState>,
    _: Superadmin,
) -> Result<Json<Vec<UserEntry>>, HttpError> {
    let rows: Vec<UserRow> = sqlx::query_as(sqlx::AssertSqlSafe(format!(
        "SELECT {USER_COLUMNS} FROM users ORDER BY created_at, id"
    )))
    .fetch_all(state.db.reader())
    .await?;
    let mut users = Vec::with_capacity(rows.len());
    for row in rows {
        users.push(entry(&state, row).await?);
    }
    Ok(Json(users))
}

/// What to change of a user; a field that is left out stays as it is.
#[derive(Deserialize, ToSchema)]
pub struct UpdateUser {
    pub is_superadmin: Option<bool>,
    /// In bytes; null for no limit.
    #[serde(default, deserialize_with = "crate::util::present")]
    #[schema(value_type = Option<i64>, nullable)]
    pub storage_limit: Option<Option<i64>>,
    /// An existing folder on the server, apart from the data dir and every
    /// other user's; null for the default in the data dir.
    #[serde(default, deserialize_with = "crate::util::present")]
    #[schema(value_type = Option<String>, nullable)]
    pub files_root: Option<Option<String>>,
    /// True disables the user: they cannot sign in, and their sessions,
    /// API keys and device tokens stop working. Nothing is deleted.
    pub disabled: Option<bool>,
}

#[utoipa::path(
    patch,
    path = "/users/{id}",
    tag = "Superadmin",
    operation_id = "updateUser",
    summary = "Change a user",
    description = "Their role, storage limit, files folder, or whether they are disabled. There is always a superadmin who is not disabled: the last one cannot be demoted or disabled (409). A new files folder is indexed from scratch, as for the superadmin's own.",
    params(("id" = Uuid, Path, description = "The user")),
    request_body(content = UpdateUser, content_type = "application/json"),
    responses(
        (status = 200, description = "The user now", body = UserEntry),
        (status = 400, description = "The request is invalid, or the folder cannot be used", body = ErrorBody),
        (status = 401, description = "Not signed in", body = ErrorBody),
        (status = 403, description = "Superadmin only, with a session (never a token) whose second factor was confirmed within 15 minutes: else `code` is `step_up_required` (confirm it, `/auth/step-up/…`) or `second_factor_required` (add one first)", body = ErrorBody),
        (status = 404, description = "No such user", body = ErrorBody),
        (status = 409, description = "The last superadmin", body = ErrorBody),
        (status = 415, description = "The body must be JSON (Content-Type: application/json)", body = ErrorBody),
    ),
    security(("session" = [])),
)]
async fn update_user(
    State(state): State<AppState>,
    me: Superadmin,
    Path(id): Path<Uuid>,
    ApiJson(body): ApiJson<UpdateUser>,
) -> Result<Json<UserEntry>, HttpError> {
    if let Some(Some(limit)) = body.storage_limit
        && limit < 0
    {
        return Err(HttpError::bad_request(
            "Expected a storage limit of 0 bytes or more",
        ));
    }
    let mut tx = state.db.writer().begin().await?;
    let row: Option<(bool, bool)> =
        sqlx::query_as("SELECT is_superadmin, disabled_at IS NOT NULL FROM users WHERE id = ?1")
            .bind(id)
            .fetch_optional(&mut *tx)
            .await?;
    let (is_superadmin, disabled) = row.ok_or_else(HttpError::not_found)?;
    let stays_superadmin = body.is_superadmin.unwrap_or(is_superadmin);
    let stays_enabled = !body.disabled.unwrap_or(disabled);
    // An enabled superadmin who would no longer be one: not the last.
    if is_superadmin
        && !disabled
        && !(stays_superadmin && stays_enabled)
        && users::other_superadmins(&mut tx, id).await? == 0
    {
        return Err(last_superadmin());
    }
    if let Some(is_superadmin) = body.is_superadmin {
        sqlx::query("UPDATE users SET is_superadmin = ?1 WHERE id = ?2")
            .bind(is_superadmin)
            .bind(id)
            .execute(&mut *tx)
            .await?;
    }
    if let Some(limit) = body.storage_limit {
        sqlx::query("UPDATE drives SET storage_limit = ?1 WHERE id = ?2 AND kind = 'my'")
            .bind(limit)
            .bind(id)
            .execute(&mut *tx)
            .await?;
    }
    if let Some(disable) = body.disabled {
        // The time it was disabled is kept when it is disabled again.
        sqlx::query(
            "UPDATE users SET disabled_at = CASE WHEN ?1 THEN coalesce(disabled_at, ?2) END
             WHERE id = ?3",
        )
        .bind(disable)
        .bind(crate::auth::now_iso())
        .bind(id)
        .execute(&mut *tx)
        .await?;
    }
    // What the superadmin did, in the user's account activity.
    let actor = crate::activity::Actor::Superadmin(me.0.id);
    let mut done = Vec::new();
    if let Some(now) = body.is_superadmin
        && now != is_superadmin
    {
        done.push((
            if now {
                crate::activity::Action::SuperadminGranted
            } else {
                crate::activity::Action::SuperadminRevoked
            },
            None,
        ));
    }
    if let Some(limit) = body.storage_limit {
        done.push((crate::activity::Action::StorageLimit, limit));
    }
    if let Some(now) = body.disabled
        && now != disabled
    {
        done.push((
            if now {
                crate::activity::Action::Disabled
            } else {
                crate::activity::Action::Enabled
            },
            None,
        ));
    }
    if body.files_root.is_some() {
        done.push((crate::activity::Action::FilesFolder, None));
    }
    for (action, limit) in done {
        crate::activity::log(
            &mut *tx,
            id,
            actor,
            action,
            crate::activity::Detail {
                storage_limit: limit,
                ..Default::default()
            },
        )
        .await?;
    }
    tx.commit().await?;
    // The folder last: it waits for what runs on the old tree, and is
    // checked on the disk.
    if let Some(root) = body.files_root {
        let root = root
            .map(|r| r.trim().to_string())
            .filter(|r| !r.is_empty())
            .map(std::path::PathBuf::from);
        state.files.set_root(id, root.as_deref()).await?;
    }
    Ok(Json(user_entry(&state, id).await?))
}

fn last_superadmin() -> HttpError {
    HttpError::conflict(
        "This is the last superadmin who is not disabled: make someone else a superadmin first",
    )
}

#[utoipa::path(
    delete,
    path = "/users/{id}",
    tag = "Superadmin",
    operation_id = "removeUser",
    summary = "Remove a user",
    description = "The account, its sessions and keys, and Organizer's own data for it (the drop box, the trash, the versions). Its files folder is deleted only when it is the default one in the data dir; an existing folder (and `STORAGE_URL`) is never deleted. The last superadmin who is not disabled cannot be removed (409).",
    params(("id" = Uuid, Path, description = "The user")),
    responses(
        (status = 204, description = "Removed"),
        (status = 401, description = "Not signed in", body = ErrorBody),
        (status = 403, description = "Superadmin only, with a session (never a token) whose second factor was confirmed within 15 minutes: else `code` is `step_up_required` (confirm it, `/auth/step-up/…`) or `second_factor_required` (add one first)", body = ErrorBody),
        (status = 404, description = "No such user", body = ErrorBody),
        (status = 409, description = "The last superadmin", body = ErrorBody),
    ),
    security(("session" = [])),
)]
async fn remove_user(
    State(state): State<AppState>,
    _: Superadmin,
    Path(id): Path<Uuid>,
) -> Result<StatusCode, HttpError> {
    // Ends what runs on the tree; a request still in flight may open it
    // again, and finds no user once the row is gone.
    state.files.forget(id).await;
    // Who to tell on other servers once they are gone (`USER_REMOVED`):
    // read before their contacts and shares go with them.
    let removed = crate::federation::outgoing::removed_user(&state.files, id).await?;
    // The drives on their own storage go with it (docs/M7C_DESIGN.md §2;
    // the bucket keeps its objects), before the user's row takes the
    // backends: when the last superadmin is not refused below.
    {
        let mut conn = state.db.reader().acquire().await?;
        let row: Option<(bool, bool)> = sqlx::query_as(
            "SELECT is_superadmin, disabled_at IS NOT NULL FROM users WHERE id = ?1",
        )
        .bind(id)
        .fetch_optional(&mut *conn)
        .await?;
        let (is_superadmin, disabled) = row.ok_or_else(HttpError::not_found)?;
        if !(is_superadmin && !disabled && users::other_superadmins(&mut conn, id).await? == 0) {
            drop(conn);
            crate::routes::backends::forget_own_drives(&state, id).await?;
            // What other servers shared with them: declined (their servers
            // are told), its drives gone.
            crate::federation::incoming::decline_all(&state.files, id).await?;
        }
    }
    let mut tx = state.db.writer().begin().await?;
    let row: Option<(bool, bool)> =
        sqlx::query_as("SELECT is_superadmin, disabled_at IS NOT NULL FROM users WHERE id = ?1")
            .bind(id)
            .fetch_optional(&mut *tx)
            .await?;
    let (is_superadmin, disabled) = row.ok_or_else(HttpError::not_found)?;
    if is_superadmin && !disabled && users::other_superadmins(&mut tx, id).await? == 0 {
        return Err(last_superadmin());
    }
    // The uploads in progress, whose bytes are in the shared uploads
    // folder; the rest of their data is in their own folder.
    let uploads: Vec<Uuid> = sqlx::query_scalar("SELECT id FROM uploads WHERE user_id = ?1")
        .bind(id)
        .fetch_all(&mut *tx)
        .await?;
    // The search index's text (FTS has no foreign key), then the user:
    // every table of theirs goes with them (ON DELETE CASCADE).
    sqlx::query(
        "DELETE FROM search WHERE rowid IN (SELECT rowid FROM search_docs WHERE drive_id = ?1)",
    )
    .bind(id)
    .execute(&mut *tx)
    .await?;
    // What they shared and others placed goes from those others' files.
    crate::files::shares::end_mounts_on(&mut tx, id).await?;
    // Where their My drive is, for its Organizer folder below.
    let root = state
        .files
        .drive_root(crate::files::drives::my(id))
        .await
        .ok()
        .map(|(root, _)| root);
    sqlx::query("DELETE FROM users WHERE id = ?1")
        .bind(id)
        .execute(&mut *tx)
        .await?;
    tx.commit().await?;
    crate::federation::outgoing::tell_removed(&state.files, removed);
    state.files.notify(id);
    // The trash and versions of a folder chosen for them: Organizer's own,
    // the only part of that folder it removes.
    if let Some(root) = root {
        let own = root.join(crate::files::ORGANIZER_DIR);
        if let Err(e) = std::fs::remove_dir_all(&own)
            && e.kind() != std::io::ErrorKind::NotFound
        {
            tracing::warn!("cannot remove {}: {e}", own.display());
        }
    }
    for upload in uploads {
        let _ = std::fs::remove_file(state.files.uploads_dir().join(format!("{upload}.part")));
    }
    // Organizer's own folder of theirs: the drop box, the trash, the
    // versions, and the default files folder. An existing folder chosen
    // for them is outside the data dir (`check_root`), so never in here.
    let dir = state.files.user_dir(id);
    if let Err(e) = std::fs::remove_dir_all(&dir)
        && e.kind() != std::io::ErrorKind::NotFound
    {
        tracing::warn!("cannot remove {}: {e}", dir.display());
    }
    // Their embeddings.
    for kind in [
        crate::vectors::VectorKind::Folder,
        crate::vectors::VectorKind::Chunk,
    ] {
        let keys: Result<Vec<String>, _> = state
            .vectors
            .fingerprints(id, kind)
            .await
            .map(|all| all.into_iter().map(|(key, _)| key).collect());
        let removed = match keys {
            Ok(keys) if keys.is_empty() => Ok(()),
            Ok(keys) => state.vectors.remove(id, kind, &keys).await,
            Err(e) => Err(e),
        };
        if let Err(e) = removed {
            tracing::warn!("cannot remove the embeddings of user {id}: {e:#}");
        }
    }
    tracing::info!("user {id} was removed");
    Ok(StatusCode::NO_CONTENT)
}

/// An invite link, pending: not used, not expired.
#[derive(Serialize, ToSchema, sqlx::FromRow)]
pub struct Invite {
    pub id: Uuid,
    /// Only this email may use it; null: anyone with the link. Always
    /// present.
    #[schema(required = true)]
    pub email: Option<String>,
    /// The new user is a superadmin.
    pub is_superadmin: bool,
    pub created_at: String,
    pub expires_at: String,
    /// The token of the link, `/invite/<token>`.
    pub token: String,
}

/// A new invite.
#[derive(Deserialize, ToSchema)]
pub struct CreateInvite {
    /// Only this email may use it; left out (or empty): anyone with the
    /// link.
    #[serde(default)]
    pub email: Option<String>,
    /// The new user is a superadmin.
    #[serde(default, deserialize_with = "crate::util::null_as_default")]
    pub is_superadmin: bool,
}

const INVITE_COLUMNS: &str =
    "id, email, is_superadmin = 1 AS is_superadmin, created_at, expires_at, token";

#[utoipa::path(
    get,
    path = "/invites",
    tag = "Superadmin",
    operation_id = "listInvites",
    summary = "The pending invites",
    description = "The invites not used and not expired, newest first.",
    responses(
        (status = 200, description = "The invites", body = Vec<Invite>),
        (status = 401, description = "Not signed in", body = ErrorBody),
        (status = 403, description = "Superadmin only, with a session (never a token) whose second factor was confirmed within 15 minutes: else `code` is `step_up_required` (confirm it, `/auth/step-up/…`) or `second_factor_required` (add one first)", body = ErrorBody),
    ),
    security(("session" = [])),
)]
async fn list_invites(
    State(state): State<AppState>,
    _: Superadmin,
) -> Result<Json<Vec<Invite>>, HttpError> {
    let invites = sqlx::query_as(sqlx::AssertSqlSafe(format!(
        "SELECT {INVITE_COLUMNS} FROM invites
         WHERE used_at IS NULL AND token IS NOT NULL AND expires_at > ?1
         ORDER BY created_at DESC"
    )))
    .bind(crate::auth::now_iso())
    .fetch_all(state.db.reader())
    .await?;
    Ok(Json(invites))
}

#[utoipa::path(
    post,
    path = "/invites",
    tag = "Superadmin",
    operation_id = "createInvite",
    summary = "Make an invite link",
    description = "Good for 7 days, used once. Organizer sends nothing: the superadmin sends the link (`/invite/<token>`) themselves.",
    request_body(content = CreateInvite, content_type = "application/json"),
    responses(
        (status = 201, description = "The invite", body = Invite),
        (status = 400, description = "The request is invalid", body = ErrorBody),
        (status = 401, description = "Not signed in", body = ErrorBody),
        (status = 403, description = "Superadmin only, with a session (never a token) whose second factor was confirmed within 15 minutes: else `code` is `step_up_required` (confirm it, `/auth/step-up/…`) or `second_factor_required` (add one first)", body = ErrorBody),
        (status = 409, description = "A user has that email", body = ErrorBody),
        (status = 415, description = "The body must be JSON (Content-Type: application/json)", body = ErrorBody),
    ),
    security(("session" = [])),
)]
async fn create_invite(
    State(state): State<AppState>,
    user: Superadmin,
    ApiJson(body): ApiJson<CreateInvite>,
) -> Result<(StatusCode, Json<Invite>), HttpError> {
    let email = match body.email.as_deref().map(str::trim) {
        None | Some("") => None,
        Some(email) => Some(validate_email(email)?),
    };
    if let Some(email) = &email {
        let taken: Option<i64> = sqlx::query_scalar("SELECT 1 FROM users WHERE email = ?1")
            .bind(email)
            .fetch_optional(state.db.reader())
            .await?;
        if taken.is_some() {
            return Err(HttpError::conflict("A user has that email already"));
        }
    }
    let id = users::make_invite(&state.db, email.as_deref(), body.is_superadmin, user.0.id).await?;
    let invite = sqlx::query_as(sqlx::AssertSqlSafe(format!(
        "SELECT {INVITE_COLUMNS} FROM invites WHERE id = ?1"
    )))
    .bind(id)
    .fetch_one(state.db.reader())
    .await?;
    Ok((StatusCode::CREATED, Json(invite)))
}

#[utoipa::path(
    delete,
    path = "/invites/{id}",
    tag = "Superadmin",
    operation_id = "revokeInvite",
    summary = "Revoke an invite",
    description = "Its link stops working.",
    params(("id" = Uuid, Path, description = "The invite")),
    responses(
        (status = 204, description = "Revoked"),
        (status = 401, description = "Not signed in", body = ErrorBody),
        (status = 403, description = "Superadmin only, with a session (never a token) whose second factor was confirmed within 15 minutes: else `code` is `step_up_required` (confirm it, `/auth/step-up/…`) or `second_factor_required` (add one first)", body = ErrorBody),
        (status = 404, description = "No such invite", body = ErrorBody),
    ),
    security(("session" = [])),
)]
async fn revoke_invite(
    State(state): State<AppState>,
    _: Superadmin,
    Path(id): Path<Uuid>,
) -> Result<StatusCode, HttpError> {
    let deleted = sqlx::query("DELETE FROM invites WHERE id = ?1 AND used_at IS NULL")
        .bind(id)
        .execute(state.db.writer())
        .await?
        .rows_affected();
    if deleted == 0 {
        return Err(HttpError::not_found());
    }
    Ok(StatusCode::NO_CONTENT)
}

/// How people may join besides an invite, in effect, with the switches a
/// variable sets.
#[derive(Serialize, ToSchema)]
pub struct JoiningSettings {
    /// "Create an account" on the login page.
    pub open_signup: bool,
    /// Single sign-on makes an account for someone with a verified email
    /// no account has.
    pub oidc_create_users: bool,
    /// The switches set by a variable (`open_signup` by `SIGNUP_OPEN`,
    /// `oidc_create_users` by `OIDC_CREATE_USERS`), which cannot be
    /// changed here.
    pub locked: Vec<String>,
}

/// The switches to change; one that is left out stays as it is.
#[derive(Deserialize, ToSchema)]
pub struct UpdateJoining {
    pub open_signup: Option<bool>,
    pub oidc_create_users: Option<bool>,
}

async fn joining_view(state: &AppState) -> Result<JoiningSettings, HttpError> {
    let joining = users::joining(state).await?;
    Ok(JoiningSettings {
        open_signup: joining.open_signup,
        oidc_create_users: joining.oidc_create_users,
        locked: state
            .joining
            .locked()
            .into_iter()
            .map(String::from)
            .collect(),
    })
}

#[utoipa::path(
    get,
    path = "/signup",
    tag = "Superadmin",
    operation_id = "getJoining",
    summary = "How people may join",
    responses(
        (status = 200, description = "The switches in effect", body = JoiningSettings),
        (status = 401, description = "Not signed in", body = ErrorBody),
        (status = 403, description = "Superadmin only, with a session (never a token) whose second factor was confirmed within 15 minutes: else `code` is `step_up_required` (confirm it, `/auth/step-up/…`) or `second_factor_required` (add one first)", body = ErrorBody),
    ),
    security(("session" = [])),
)]
async fn get_joining(
    State(state): State<AppState>,
    _: Superadmin,
) -> Result<Json<JoiningSettings>, HttpError> {
    Ok(Json(joining_view(&state).await?))
}

#[utoipa::path(
    put,
    path = "/signup",
    tag = "Superadmin",
    operation_id = "updateJoining",
    summary = "Change how people may join",
    description = "A switch set by its variable (`SIGNUP_OPEN`, `OIDC_CREATE_USERS`) cannot be changed (400).",
    request_body(content = UpdateJoining, content_type = "application/json"),
    responses(
        (status = 200, description = "The switches now in effect", body = JoiningSettings),
        (status = 400, description = "A switch is set by its variable", body = ErrorBody),
        (status = 401, description = "Not signed in", body = ErrorBody),
        (status = 403, description = "Superadmin only, with a session (never a token) whose second factor was confirmed within 15 minutes: else `code` is `step_up_required` (confirm it, `/auth/step-up/…`) or `second_factor_required` (add one first)", body = ErrorBody),
        (status = 415, description = "The body must be JSON (Content-Type: application/json)", body = ErrorBody),
    ),
    security(("session" = [])),
)]
async fn update_joining(
    State(state): State<AppState>,
    _: Superadmin,
    ApiJson(body): ApiJson<UpdateJoining>,
) -> Result<Json<JoiningSettings>, HttpError> {
    let locked = state.joining.locked();
    for (field, value, variable) in [
        ("open_signup", body.open_signup, "SIGNUP_OPEN"),
        (
            "oidc_create_users",
            body.oidc_create_users,
            "OIDC_CREATE_USERS",
        ),
    ] {
        if value.is_some() && locked.contains(&field) {
            return Err(HttpError::bad_request(format!(
                "{field} is set by the {variable} environment variable"
            )));
        }
    }
    let mut stored = users::stored(&state.db).await?;
    if let Some(open_signup) = body.open_signup {
        stored.open_signup = open_signup;
    }
    if let Some(oidc_create_users) = body.oidc_create_users {
        stored.oidc_create_users = oidc_create_users;
    }
    crate::server_settings::save(state.db.writer(), "signup", &stored).await?;
    Ok(Json(joining_view(&state).await?))
}

/// What the invite page shows.
#[derive(Serialize, ToSchema)]
pub struct InviteInfo {
    /// The email the invite is for, which the new account takes; null:
    /// any. Always present.
    #[schema(required = true)]
    pub email: Option<String>,
    pub expires_at: String,
}

#[utoipa::path(
    get,
    path = "/invite/{token}",
    tag = "Auth",
    operation_id = "getInvite",
    summary = "An invite link's state",
    description = "The email it is for, if any. A token that is not a pending invite (used, expired, revoked or unknown) is a 404. Each request counts against the sign-in limit of the address.",
    params(("token" = String, Path, description = "The invite's token")),
    responses(
        (status = 200, description = "A pending invite", body = InviteInfo),
        (status = 404, description = "No pending invite", body = ErrorBody),
        (status = 429, description = "Too many attempts, try again later", body = ErrorBody),
    ),
)]
async fn get_invite(
    State(state): State<AppState>,
    ConnectInfo(addr): ConnectInfo<SocketAddr>,
    headers: HeaderMap,
    Path(token): Path<String>,
) -> Result<Json<InviteInfo>, HttpError> {
    let ip = client_ip(&headers, Some(addr.ip()), state.trust_proxy);
    state.limits.address(&ip)?;
    let invite: Option<(Option<String>, String)> = sqlx::query_as(
        "SELECT email, expires_at FROM invites
         WHERE token_hash = ?1 AND used_at IS NULL AND expires_at > ?2",
    )
    .bind(crate::util::sha256_hex(&token))
    .bind(crate::auth::now_iso())
    .fetch_optional(state.db.reader())
    .await?;
    let (email, expires_at) = invite.ok_or_else(invalid_invite)?;
    Ok(Json(InviteInfo { email, expires_at }))
}

fn invalid_invite() -> HttpError {
    HttpError::new(
        StatusCode::NOT_FOUND,
        "This invite link is used, expired or revoked: ask for a new one",
    )
}

/// The account to make: an email and a password.
#[derive(Deserialize, ToSchema)]
pub struct NewAccount {
    /// The invite's email when it names one.
    pub email: String,
    /// 10 to 200 characters.
    pub password: String,
}

#[utoipa::path(
    post,
    path = "/invite/{token}",
    tag = "Auth",
    operation_id = "acceptInvite",
    summary = "Accept an invite",
    description = "Makes the account (a superadmin if the invite says so) and signs it in: the session cookie is set. The invite is used up. With `OIDC_ONLY`, refused (403): accept it through single sign-on (`/auth/oidc/login?invite=<token>`). Counts against the sign-in limits, per address and per email.",
    params(("token" = String, Path, description = "The invite's token")),
    request_body(content = NewAccount, content_type = "application/json"),
    responses(
        (status = 200, description = "The account was made; setting up two-factor authentication is next (`TOTP_REQUIRED`)", body = LoginStep),
        (status = 201, description = "The account was made and signed in"),
        (status = 400, description = "The request is invalid", body = ErrorBody),
        (status = 403, description = "Password sign-in is turned off (`OIDC_ONLY`)", body = ErrorBody),
        (status = 404, description = "No pending invite, or one for another email", body = ErrorBody),
        (status = 409, description = "A user has that email", body = ErrorBody),
        (status = 415, description = "The body must be JSON (Content-Type: application/json)", body = ErrorBody),
        (status = 429, description = "Too many attempts, try again later", body = ErrorBody),
    ),
)]
async fn accept_invite(
    State(state): State<AppState>,
    mut auth: AuthSession,
    ConnectInfo(addr): ConnectInfo<SocketAddr>,
    headers: HeaderMap,
    Path(token): Path<String>,
    ApiJson(body): ApiJson<NewAccount>,
) -> Result<Response, HttpError> {
    if !state.oidc.passwords_allowed() {
        return Err(HttpError::passwords_off());
    }
    let email = validate_email(&body.email)?;
    let ip = client_ip(&headers, Some(addr.ip()), state.trust_proxy);
    state.limits.sign_in(&ip, &email)?;
    let password = validate_password(&body.password)?;
    // Hashed before the transaction: it takes a while.
    let hash = hash_password(&password);
    let mut tx = state.db.writer().begin().await?;
    let maker = users::invite_maker(&mut *tx, &token).await?;
    let Some(is_superadmin) = users::take_invite(&mut tx, &token, &email).await? else {
        return Err(invalid_invite());
    };
    let Some(id) = users::create(&mut tx, &email, Some(&hash), is_superadmin).await? else {
        return Err(email_taken());
    };
    users::note_invited(&mut tx, id, maker).await?;
    tx.commit().await?;
    tracing::info!("user {id} joined with an invite");
    signed_in(&state, &mut auth, id).await
}

fn email_taken() -> HttpError {
    HttpError::conflict("An account with that email exists already: sign in")
}

/// A new account is signed in, and its tree made and indexed. With
/// `TOTP_REQUIRED`, setting up two-factor authentication comes first, as
/// at a sign-in: 200 with the step and its cookie.
async fn signed_in(
    state: &AppState,
    auth: &mut AuthSession,
    id: Uuid,
) -> Result<Response, HttpError> {
    let files = state.files.clone();
    tokio::spawn(async move { files.open_and_scan(id).await });
    if let Some(step) = next_step(state, id).await? {
        return Ok((
            challenge_cookie(state, id, step),
            Json(LoginStep {
                totp_required: step == Step::Code,
                totp_setup_required: step == Step::Enroll,
            }),
        )
            .into_response());
    }
    let user = auth
        .backend
        .get_user(&id)
        .await?
        .ok_or_else(|| HttpError::internal("the new user is gone"))?;
    session::sign_in(auth, &user).await?;
    Ok(StatusCode::CREATED.into_response())
}

#[utoipa::path(
    post,
    path = "/signup",
    tag = "Auth",
    operation_id = "signup",
    summary = "Create an account",
    description = "Open sign-up, when a superadmin turned it on (`/auth/session` says `signup`). Makes a regular user and signs them in: the session cookie is set. Refused (403) while it is off, or with `OIDC_ONLY` (single sign-on makes the account then). Counts against the sign-in limits, per address and per email.",
    request_body(content = NewAccount, content_type = "application/json"),
    responses(
        (status = 200, description = "The account was made; setting up two-factor authentication is next (`TOTP_REQUIRED`)", body = LoginStep),
        (status = 201, description = "The account was made and signed in"),
        (status = 400, description = "The request is invalid", body = ErrorBody),
        (status = 403, description = "Sign-up is closed, or password sign-in is turned off (`OIDC_ONLY`)", body = ErrorBody),
        (status = 409, description = "A user has that email", body = ErrorBody),
        (status = 415, description = "The body must be JSON (Content-Type: application/json)", body = ErrorBody),
        (status = 429, description = "Too many attempts, try again later", body = ErrorBody),
    ),
)]
async fn signup(
    State(state): State<AppState>,
    mut auth: AuthSession,
    ConnectInfo(addr): ConnectInfo<SocketAddr>,
    headers: HeaderMap,
    ApiJson(body): ApiJson<NewAccount>,
) -> Result<Response, HttpError> {
    if !state.oidc.passwords_allowed() {
        return Err(HttpError::passwords_off());
    }
    if !users::password_signup(&state).await? {
        return Err(HttpError::new(
            StatusCode::FORBIDDEN,
            "Sign-up is closed: ask a superadmin for an invite",
        ));
    }
    let email = validate_email(&body.email)?;
    let ip = client_ip(&headers, Some(addr.ip()), state.trust_proxy);
    state.limits.sign_in(&ip, &email)?;
    let password = validate_password(&body.password)?;
    let hash = hash_password(&password);
    let mut tx = state.db.writer().begin().await?;
    // Nobody before the first superadmin: /setup makes them.
    let set_up: bool = sqlx::query_scalar("SELECT EXISTS (SELECT 1 FROM users)")
        .fetch_one(&mut *tx)
        .await?;
    if !set_up {
        return Err(HttpError::conflict("Not set up yet"));
    }
    let Some(id) = users::create(&mut tx, &email, Some(&hash), false).await? else {
        return Err(email_taken());
    };
    tx.commit().await?;
    tracing::info!("user {id} signed up");
    signed_in(&state, &mut auth, id).await
}

#[utoipa::path(
    get,
    path = "/storage",
    tag = "Account",
    operation_id = "getOwnStorage",
    summary = "Your storage",
    description = "What your files, trash, versions and the drop box's waiting items take, and your limit (null: none).",
    responses(
        (status = 200, description = "Used, and the limit", body = Usage),
        (status = 401, description = "Not signed in", body = ErrorBody),
    ),
    security(("session" = []), ("api_key" = [])),
)]
async fn own_storage(
    State(state): State<AppState>,
    user: AuthUser,
) -> Result<Json<Usage>, HttpError> {
    Ok(Json(state.files.usage(user.0.id).await?))
}
