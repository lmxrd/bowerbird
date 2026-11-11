//! Federation for our clients (docs/M7E_DESIGN.md §3, §7): contacts on
//! other servers (`/contacts`), the invites that make them
//! (`/invites/federated`: made here and accepted here), looking an address
//! up before sharing with it (`/federation/lookup`), accepting a share from
//! another server (`/shared/{id}/accept`; declining and placing are the
//! usual `/shared` endpoints), and the superadmin's settings
//! (`/superadmin/federation`). The OCM endpoints other servers call are in
//! `routes/ocm.rs`.

use crate::error::{ErrorBody, HttpError};
use crate::extract::ApiJson;
use crate::federation::address::{Address, host_of, invite_string, parse_invite};
use crate::federation::outgoing::{self, REMOTE_REFUSED, REMOTE_UNREACHABLE, refusal};
use crate::federation::{Mode, Trust};
use crate::routes::auth::{AuthUser, Superadmin};
use crate::state::AppState;
use axum::Json;
use axum::extract::{Path, Query, State};
use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use serde::{Deserialize, Serialize};
use utoipa::{IntoParams, ToSchema};
use utoipa_axum::router::OpenApiRouter;
use utoipa_axum::routes;
use uuid::Uuid;

/// The `code` of an invite that is not valid (unknown, expired) at its
/// server, and of one accepted already.
pub const INVITE_INVALID: &str = "invite_invalid";
pub const INVITE_USED: &str = "invite_used";

/// How long an invite lives.
const INVITE_DAYS: u64 = 30;

/// Nested under /contacts.
pub fn contacts_router() -> OpenApiRouter<AppState> {
    OpenApiRouter::new()
        .routes(routes!(list_contacts))
        .routes(routes!(remove_contact))
}

/// Nested under /invites/federated.
pub fn invites_router() -> OpenApiRouter<AppState> {
    OpenApiRouter::new()
        .routes(routes!(list_invites, create_invite))
        .routes(routes!(revoke_invite))
        .routes(routes!(accept_invite))
}

/// Nested under /federation.
pub fn router() -> OpenApiRouter<AppState> {
    OpenApiRouter::new().routes(routes!(lookup))
}

/// Nested under /superadmin.
pub fn superadmin_router() -> OpenApiRouter<AppState> {
    OpenApiRouter::new().routes(routes!(get_federation, set_federation))
}

/// Nested under /shared.
pub fn shared_router() -> OpenApiRouter<AppState> {
    OpenApiRouter::new().routes(routes!(accept_shared))
}

/// Someone on another server, in the user's contacts.
#[derive(Serialize, ToSchema, sqlx::FromRow)]
pub struct Contact {
    pub id: Uuid,
    /// Their OCM address, `user@host`: what a grant's `address` takes.
    pub address: String,
    /// Their name, as their server gave it (may be empty).
    pub name: String,
    /// Their email, as their server gave it (informational; may be empty).
    pub email: String,
    /// Their server, `host[:port]`.
    pub server: String,
    /// The software their server says it is in its OCM discovery
    /// (`provider`: "Nextcloud", "Organizer" …); null when it names none.
    /// Kept when they became a contact, refreshed whenever their server's
    /// discovery is fetched again. Always present.
    #[schema(required = true)]
    pub provider: Option<String>,
    /// Their server is an Organizer server: it advertises our extensions
    /// (whole drives, encrypted folders, live changes).
    pub organizer: bool,
    /// Since when they are a contact.
    pub since: String,
}

/// The columns of a [`Contact`], from `contacts`.
const CONTACT: &str =
    "SELECT id, address, name, email, host AS server, provider, organizer, created_at AS since
     FROM contacts";

/// Add a contact (or refresh its name), inside a transaction, with what
/// their server's discovery says it is.
pub(crate) async fn add_contact(
    tx: &mut sqlx::SqliteConnection,
    user: Uuid,
    address: &Address,
    name: &str,
    email: &str,
    server: Option<&crate::federation::Discovery>,
) -> sqlx::Result<()> {
    let clean = |s: &str| -> String { s.chars().filter(|c| !c.is_control()).take(200).collect() };
    sqlx::query(
        "INSERT INTO contacts (id, user_id, address, host, name, email, provider, organizer)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8)
         ON CONFLICT (user_id, address) DO UPDATE SET name = excluded.name, email = excluded.email,
           provider = CASE WHEN ?9 THEN excluded.provider ELSE provider END,
           organizer = CASE WHEN ?9 THEN excluded.organizer ELSE organizer END",
    )
    .bind(Uuid::now_v7())
    .bind(user)
    .bind(address.to_string())
    .bind(&address.host)
    .bind(clean(name))
    .bind(clean(email))
    .bind(server.and_then(|d| d.provider_name()))
    .bind(server.is_some_and(|d| d.organizer()))
    .bind(server.is_some())
    .execute(tx)
    .await?;
    Ok(())
}

#[utoipa::path(
    get,
    path = "",
    tag = "Federation",
    operation_id = "listContacts",
    summary = "My contacts on other servers",
    description = "Made by invites accepted, in either direction; the newest first.",
    responses(
        (status = 200, description = "The contacts", body = Vec<Contact>),
        (status = 401, description = "Not signed in", body = ErrorBody),
    ),
    security(("session" = []), ("api_key" = [])),
)]
async fn list_contacts(
    State(state): State<AppState>,
    user: AuthUser,
) -> Result<Json<Vec<Contact>>, HttpError> {
    Ok(Json(
        sqlx::query_as(sqlx::AssertSqlSafe(format!(
            "{CONTACT} WHERE user_id = ?1 ORDER BY created_at DESC, id DESC"
        )))
        .bind(user.0.id)
        .fetch_all(state.db.reader())
        .await?,
    ))
}

#[utoipa::path(
    delete,
    path = "/{id}",
    tag = "Federation",
    operation_id = "removeContact",
    summary = "Remove a contact",
    description = "Without telling them (OCM §4.4.5). What was shared with them stays shared; sharing anew with them needs a new invite (unless their server is listed or any address is allowed).",
    params(("id" = Uuid, Path, description = "The contact")),
    responses(
        (status = 204, description = "Removed"),
        (status = 401, description = "Not signed in", body = ErrorBody),
        (status = 403, description = "A read-only key", body = ErrorBody),
        (status = 404, description = "No such contact", body = ErrorBody),
    ),
    security(("session" = []), ("api_key" = [])),
)]
async fn remove_contact(
    State(state): State<AppState>,
    user: AuthUser,
    Path(id): Path<Uuid>,
) -> Result<StatusCode, HttpError> {
    let removed = sqlx::query("DELETE FROM contacts WHERE id = ?1 AND user_id = ?2")
        .bind(id)
        .bind(user.0.id)
        .execute(state.db.writer())
        .await?
        .rows_affected();
    if removed == 0 {
        return Err(HttpError::not_found());
    }
    Ok(StatusCode::NO_CONTENT)
}

/// An invite for someone on another server.
#[derive(Serialize, ToSchema)]
pub struct FederatedInvite {
    pub id: Uuid,
    /// The invite string (OCM §4.4.6: the token and this server, base64url),
    /// to send them; null once accepted. Always present.
    #[schema(required = true)]
    pub invite: Option<String>,
    /// A link to this server's page for accepting an invite, for someone
    /// whose server shows one (`inviteAcceptDialog`); null once accepted.
    /// Always present.
    #[schema(required = true)]
    pub link: Option<String>,
    pub created_at: String,
    pub expires_at: String,
    /// When it was accepted; null while pending. Always present.
    #[schema(required = true)]
    pub accepted_at: Option<String>,
    /// Who accepted it: their address. Always present.
    #[schema(required = true)]
    pub accepted_by: Option<String>,
}

type InviteRow = (
    Uuid,
    Option<String>,
    String,
    String,
    Option<String>,
    Option<String>,
);

fn invite_view(state: &AppState, row: InviteRow) -> FederatedInvite {
    let (id, token, created_at, expires_at, accepted_at, accepted_by) = row;
    let federation = state.files.federation();
    let host = federation.host().unwrap_or_default();
    let base = federation.base().unwrap_or_default();
    FederatedInvite {
        id,
        invite: token.as_deref().map(|t| invite_string(t, &host)),
        link: token.as_deref().map(|t| {
            format!(
                "{base}/settings/contacts?token={}&providerDomain={}",
                crate::util::form_encode(t),
                crate::util::form_encode(&host)
            )
        }),
        created_at,
        expires_at,
        accepted_at,
        accepted_by,
    }
}

/// Refused unless federation is on.
async fn require_on(state: &AppState) -> Result<(), HttpError> {
    if state.files.federation().on().await? {
        return Ok(());
    }
    Err(refusal(
        StatusCode::FORBIDDEN,
        outgoing::FEDERATION_OFF,
        "Sharing with other servers is off on this server",
    ))
}

#[utoipa::path(
    get,
    path = "",
    tag = "Federation",
    operation_id = "listFederatedInvites",
    summary = "My invites for people on other servers",
    description = "Pending and accepted, the newest first.",
    responses(
        (status = 200, description = "The invites", body = Vec<FederatedInvite>),
        (status = 401, description = "Not signed in", body = ErrorBody),
    ),
    security(("session" = []), ("api_key" = [])),
)]
async fn list_invites(
    State(state): State<AppState>,
    user: AuthUser,
) -> Result<Json<Vec<FederatedInvite>>, HttpError> {
    let rows: Vec<InviteRow> = sqlx::query_as(
        "SELECT id, token, created_at, expires_at, accepted_at, accepted_by
         FROM federated_invites WHERE user_id = ?1 ORDER BY created_at DESC, id DESC",
    )
    .bind(user.0.id)
    .fetch_all(state.db.reader())
    .await?;
    Ok(Json(
        rows.into_iter().map(|r| invite_view(&state, r)).collect(),
    ))
}

#[utoipa::path(
    post,
    path = "",
    tag = "Federation",
    operation_id = "createFederatedInvite",
    summary = "Invite someone on another server",
    description = "An OCM invite (§4.4): send them the invite string (or the link); once they accept it on their server, each is the other's contact. Good for 30 days, once.",
    responses(
        (status = 201, description = "Made", body = FederatedInvite),
        (status = 401, description = "Not signed in", body = ErrorBody),
        (status = 403, description = "Federation is off (`code` `federation_off`), or a read-only key", body = ErrorBody),
    ),
    security(("session" = []), ("api_key" = [])),
)]
async fn create_invite(
    State(state): State<AppState>,
    user: AuthUser,
) -> Result<Response, HttpError> {
    require_on(&state).await?;
    let id = Uuid::now_v7();
    let token = Uuid::new_v4().to_string();
    let expires = crate::auth::iso(
        std::time::SystemTime::now() + std::time::Duration::from_secs(INVITE_DAYS * 86_400),
    );
    sqlx::query(
        "INSERT INTO federated_invites (id, user_id, token_hash, token, expires_at)
         VALUES (?1, ?2, ?3, ?4, ?5)",
    )
    .bind(id)
    .bind(user.0.id)
    .bind(crate::util::sha256_hex(&token))
    .bind(&token)
    .bind(&expires)
    .execute(state.db.writer())
    .await?;
    let row: InviteRow = sqlx::query_as(
        "SELECT id, token, created_at, expires_at, accepted_at, accepted_by
         FROM federated_invites WHERE id = ?1",
    )
    .bind(id)
    .fetch_one(state.db.reader())
    .await?;
    Ok((StatusCode::CREATED, Json(invite_view(&state, row))).into_response())
}

#[utoipa::path(
    delete,
    path = "/{id}",
    tag = "Federation",
    operation_id = "revokeFederatedInvite",
    summary = "Revoke an invite",
    description = "It can no longer be accepted (an accepted one is only removed from the list: the contact stays).",
    params(("id" = Uuid, Path, description = "The invite")),
    responses(
        (status = 204, description = "Revoked"),
        (status = 401, description = "Not signed in", body = ErrorBody),
        (status = 403, description = "A read-only key", body = ErrorBody),
        (status = 404, description = "No such invite", body = ErrorBody),
    ),
    security(("session" = []), ("api_key" = [])),
)]
async fn revoke_invite(
    State(state): State<AppState>,
    user: AuthUser,
    Path(id): Path<Uuid>,
) -> Result<StatusCode, HttpError> {
    let removed = sqlx::query("DELETE FROM federated_invites WHERE id = ?1 AND user_id = ?2")
        .bind(id)
        .bind(user.0.id)
        .execute(state.db.writer())
        .await?
        .rows_affected();
    if removed == 0 {
        return Err(HttpError::not_found());
    }
    Ok(StatusCode::NO_CONTENT)
}

/// An invite someone on another server sent the user.
#[derive(Deserialize, ToSchema)]
pub struct AcceptInvite {
    /// The invite string (or the link with `token` and `providerDomain`).
    pub invite: String,
}

#[utoipa::path(
    post,
    path = "/accept",
    tag = "Federation",
    operation_id = "acceptFederatedInvite",
    summary = "Accept an invite from someone on another server",
    description = "Their server is told (OCM's Invite Acceptance Request, signed); both become each other's contacts. Refusals carry a `code`: `federation_off`, `server_refused` (their server is not allowed here), `invite_invalid` (unknown or expired there), `invite_used` (accepted already), `remote_refused` (their server does not take ours), `remote_unreachable`.",
    request_body(content = AcceptInvite, content_type = "application/json"),
    responses(
        (status = 201, description = "Accepted: the new contact", body = Contact),
        (status = 400, description = "Not an invite, or `invite_invalid`", body = ErrorBody),
        (status = 401, description = "Not signed in", body = ErrorBody),
        (status = 403, description = "`federation_off`, `server_refused`, or a read-only key", body = ErrorBody),
        (status = 409, description = "`invite_used`", body = ErrorBody),
        (status = 415, description = "The body must be JSON (Content-Type: application/json)", body = ErrorBody),
        (status = 502, description = "`remote_refused` or `remote_unreachable`", body = ErrorBody),
    ),
    security(("session" = []), ("api_key" = [])),
)]
async fn accept_invite(
    State(state): State<AppState>,
    user: AuthUser,
    ApiJson(body): ApiJson<AcceptInvite>,
) -> Result<Response, HttpError> {
    let (token, host) = parse_invite(&body.invite)
        .ok_or_else(|| HttpError::bad_request("That is not an invite"))?;
    outgoing::trust_of(&state.files, &host).await?;
    let federation = state.files.federation();
    let discovery = outgoing::discover(&state.files, &host).await?;
    let ours = federation.host().unwrap_or_default();
    let request = serde_json::json!({
        "recipientProvider": ours,
        "token": token,
        "userID": user.0.email,
        "email": user.0.email,
        "name": user.0.email,
    });
    let (status, answer) = federation
        .post_json(&discovery, "/invite-accepted", &request)
        .await
        .map_err(|e| refusal(StatusCode::BAD_GATEWAY, REMOTE_UNREACHABLE, e))?;
    match status {
        200 | 201 => {}
        400 => {
            return Err(refusal(
                StatusCode::BAD_REQUEST,
                INVITE_INVALID,
                "The invite is not valid (unknown, or expired)",
            ));
        }
        409 => {
            return Err(refusal(
                StatusCode::CONFLICT,
                INVITE_USED,
                "The invite was accepted already",
            ));
        }
        _ => {
            return Err(refusal(
                StatusCode::BAD_GATEWAY,
                REMOTE_REFUSED,
                format!("{host} refused it ({status})"),
            ));
        }
    }
    #[derive(Deserialize)]
    struct Answer {
        #[serde(rename = "userID")]
        user_id: String,
        #[serde(default)]
        email: String,
        #[serde(default)]
        name: String,
    }
    let answer: Answer = serde_json::from_slice(&answer).map_err(|_| {
        refusal(
            StatusCode::BAD_GATEWAY,
            REMOTE_REFUSED,
            "Their server's answer is not valid",
        )
    })?;
    if answer.user_id.trim().is_empty() {
        return Err(refusal(
            StatusCode::BAD_GATEWAY,
            REMOTE_REFUSED,
            "Their server's answer is not valid",
        ));
    }
    let them = Address::new(answer.user_id.trim(), &host);
    let mut tx = state.db.writer().begin().await?;
    add_contact(
        &mut tx,
        user.0.id,
        &them,
        &answer.name,
        &answer.email,
        Some(&*discovery),
    )
    .await?;
    tx.commit().await?;
    let contact: Contact = sqlx::query_as(sqlx::AssertSqlSafe(format!(
        "{CONTACT} WHERE user_id = ?1 AND address = ?2"
    )))
    .bind(user.0.id)
    .bind(them.to_string())
    .fetch_one(state.db.reader())
    .await?;
    Ok((StatusCode::CREATED, Json(contact)).into_response())
}

#[derive(Deserialize, IntoParams)]
#[into_params(parameter_in = Query)]
pub struct LookupQuery {
    /// An address on another server, `user@host`.
    pub address: String,
}

/// What sharing with an address on another server would do.
#[derive(Serialize, ToSchema)]
pub struct AddressLookup {
    /// The address, as OCM writes it.
    pub address: String,
    /// Their server, `host[:port]`.
    pub server: String,
    /// They are the user's contact.
    pub contact: bool,
    /// Their server is in the superadmin's list (trusted).
    pub listed: bool,
    /// The user may share with them now.
    pub may_share: bool,
    /// Why not, for the client to act on: `federation_off`,
    /// `server_refused`, `contact_required` (invite them first),
    /// `remote_unreachable` (their server answers no OCM discovery); null
    /// when they may. Always present.
    #[schema(required = true)]
    pub refusal: Option<String>,
    /// Their server is an Organizer server with whole drives
    /// (`organizer-drives`).
    pub drives: bool,
    /// … with encrypted folders (`organizer-e2e`).
    pub encrypted_folders: bool,
}

#[utoipa::path(
    get,
    path = "/lookup",
    tag = "Federation",
    operation_id = "lookUpAddress",
    summary = "Look up an address on another server",
    description = "For the Share dialog: whether the user may share with it, and what its server can take. Asks the other server's OCM discovery.",
    params(LookupQuery),
    responses(
        (status = 200, description = "What sharing with it would do", body = AddressLookup),
        (status = 400, description = "Not an address on another server", body = ErrorBody),
        (status = 401, description = "Not signed in", body = ErrorBody),
    ),
    security(("session" = []), ("api_key" = [])),
)]
async fn lookup(
    State(state): State<AppState>,
    user: AuthUser,
    Query(query): Query<LookupQuery>,
) -> Result<Json<AddressLookup>, HttpError> {
    let address = Address::parse(&query.address)
        .ok_or_else(|| HttpError::bad_request("That is not an address (user@host)"))?;
    let contact: Option<i64> =
        sqlx::query_scalar("SELECT 1 FROM contacts WHERE user_id = ?1 AND address = ?2")
            .bind(user.0.id)
            .bind(address.to_string())
            .fetch_optional(state.db.reader())
            .await?;
    let mut out = AddressLookup {
        address: address.to_string(),
        server: address.host.clone(),
        contact: contact.is_some(),
        listed: false,
        may_share: false,
        refusal: None,
        drives: false,
        encrypted_folders: false,
    };
    let code = |e: &HttpError| match e {
        HttpError::Status {
            code: Some(code), ..
        } => Some(code.to_string()),
        _ => None,
    };
    let trust = match outgoing::trust_of(&state.files, &address.host).await {
        Ok(trust) => trust,
        Err(e) => {
            out.refusal = code(&e);
            return Ok(Json(out));
        }
    };
    out.listed = trust == (Trust::Allowed { listed: true });
    if let Err(e) = outgoing::may_share_with(&state.files, user.0.id, &address, trust).await {
        out.refusal = code(&e);
        return Ok(Json(out));
    }
    match state.files.federation().discover(&address.host).await {
        Ok(discovery) => {
            out.drives = discovery.has(crate::federation::DRIVES);
            out.encrypted_folders = discovery.has(crate::federation::E2E);
            out.may_share = true;
        }
        Err(_) => out.refusal = Some(REMOTE_UNREACHABLE.into()),
    }
    Ok(Json(out))
}

/// A server in the superadmin's lists.
#[derive(Serialize, Deserialize, ToSchema, Clone)]
pub struct FederationServer {
    /// Its host, `cloud.example.org` (with `:port` when not the default).
    pub host: String,
}

/// Federation on this server.
#[derive(Serialize, ToSchema)]
pub struct FederationSettings {
    pub mode: Mode,
    /// What the mode is in effect: `off` while `PUBLIC_URL` is not set,
    /// whatever is stored.
    pub effective_mode: Mode,
    /// This server's host for other servers (from `PUBLIC_URL`); null
    /// without it. Always present.
    #[schema(required = true)]
    pub host: Option<String>,
    /// Its OCM discovery, for other servers.
    #[schema(required = true)]
    pub discovery_url: Option<String>,
    /// The id of its signing key (in its JWK Set). Always present.
    #[schema(required = true)]
    pub key_id: Option<String>,
    /// With `listed`: the only servers federated with (trusted: no
    /// contact needed to share with their people).
    pub allowed: Vec<FederationServer>,
    /// With `open`: the servers refused.
    pub refused: Vec<FederationServer>,
}

async fn settings_view(state: &AppState) -> Result<FederationSettings, HttpError> {
    let federation = state.files.federation();
    let servers: Vec<(String, String)> =
        sqlx::query_as("SELECT host, list FROM federation_servers ORDER BY host")
            .fetch_all(state.db.reader())
            .await?;
    let list = |which: &str| {
        servers
            .iter()
            .filter(|(_, l)| l == which)
            .map(|(host, _)| FederationServer { host: host.clone() })
            .collect()
    };
    Ok(FederationSettings {
        mode: federation.stored_mode().await?,
        effective_mode: federation.mode().await?,
        host: federation.host(),
        discovery_url: federation.base().map(|b| format!("{b}/.well-known/ocm")),
        key_id: federation
            .kid()
            .ok()
            .filter(|_| federation.host().is_some()),
        allowed: list("allowed"),
        refused: list("refused"),
    })
}

#[utoipa::path(
    get,
    path = "/federation",
    tag = "Superadmin",
    operation_id = "getFederation",
    summary = "Federation with other servers",
    description = "Off, only the servers listed, or open (but the servers refused); this server's address and signing key.",
    responses(
        (status = 200, description = "The settings", body = FederationSettings),
        (status = 401, description = "Not signed in", body = ErrorBody),
        (status = 403, description = "Superadmin only, with a session (never a token) whose second factor was confirmed within 15 minutes (else `code` is `step_up_required` or `second_factor_required`)", body = ErrorBody),
    ),
    security(("session" = [])),
)]
async fn get_federation(
    State(state): State<AppState>,
    _: Superadmin,
) -> Result<Json<FederationSettings>, HttpError> {
    Ok(Json(settings_view(&state).await?))
}

/// The superadmin's choice.
#[derive(Deserialize, ToSchema)]
pub struct FederationChange {
    pub mode: Mode,
    /// The servers listed (with `listed`), by host; replaces the list.
    #[serde(default)]
    pub allowed: Vec<String>,
    /// The servers refused (with `open`), by host; replaces the list.
    #[serde(default)]
    pub refused: Vec<String>,
}

#[utoipa::path(
    put,
    path = "/federation",
    tag = "Superadmin",
    operation_id = "setFederation",
    summary = "Set federation with other servers",
    description = "The mode and both lists at once. Turning it off stops every share with other servers at once (their tokens and WebDAV refuse); turning it on again brings them back.",
    request_body(content = FederationChange, content_type = "application/json"),
    responses(
        (status = 200, description = "The settings now", body = FederationSettings),
        (status = 400, description = "A host that is not one, or on both lists", body = ErrorBody),
        (status = 401, description = "Not signed in", body = ErrorBody),
        (status = 403, description = "Superadmin only, with a session (never a token) whose second factor was confirmed within 15 minutes (else `code` is `step_up_required` or `second_factor_required`)", body = ErrorBody),
        (status = 415, description = "The body must be JSON (Content-Type: application/json)", body = ErrorBody),
    ),
    security(("session" = [])),
)]
async fn set_federation(
    State(state): State<AppState>,
    _: Superadmin,
    ApiJson(body): ApiJson<FederationChange>,
) -> Result<Json<FederationSettings>, HttpError> {
    let clean = |hosts: &[String]| -> Result<Vec<String>, HttpError> {
        let mut out: Vec<String> = Vec::new();
        for host in hosts {
            let host = host_of(host.trim().trim_start_matches("https://"))
                .ok_or_else(|| HttpError::bad_request(format!("{host} is not a server's host")))?;
            if !out.contains(&host) {
                out.push(host);
            }
        }
        Ok(out)
    };
    let (allowed, refused) = (clean(&body.allowed)?, clean(&body.refused)?);
    if allowed.iter().any(|h| refused.contains(h)) {
        return Err(HttpError::bad_request(
            "A server cannot be both listed and refused",
        ));
    }
    let mut tx = state.db.writer().begin().await?;
    sqlx::query("UPDATE federation_settings SET mode = ?1 WHERE id = 1")
        .bind(body.mode)
        .execute(&mut *tx)
        .await?;
    sqlx::query("DELETE FROM federation_servers")
        .execute(&mut *tx)
        .await?;
    for (hosts, list) in [(&allowed, "allowed"), (&refused, "refused")] {
        for host in hosts {
            sqlx::query("INSERT INTO federation_servers (host, list) VALUES (?1, ?2)")
                .bind(host)
                .bind(list)
                .execute(&mut *tx)
                .await?;
        }
    }
    tx.commit().await?;
    for host in allowed.iter().chain(&refused) {
        state.files.federation().forget(host);
    }
    Ok(Json(settings_view(&state).await?))
}

#[utoipa::path(
    post,
    path = "/{id}/accept",
    tag = "Sharing",
    operation_id = "acceptShared",
    summary = "Accept something shared with me from another server",
    description = "A share from another server (`SharedItem.remote`) waits for this before its files are listed; its server is told (OCM's SHARE_ACCEPTED). Placing it in my files accepts it too; declining is `DELETE /shared/{id}`.",
    params(("id" = Uuid, Path, description = "The shared folder or file id (`SharedItem.node.id`)")),
    responses(
        (status = 204, description = "Accepted"),
        (status = 401, description = "Not signed in", body = ErrorBody),
        (status = 403, description = "A read-only API key", body = ErrorBody),
        (status = 404, description = "No share from another server with that id for you", body = ErrorBody),
    ),
    security(("session" = []), ("api_key" = [])),
)]
async fn accept_shared(
    State(state): State<AppState>,
    user: AuthUser,
    Path(id): Path<Uuid>,
) -> Result<StatusCode, HttpError> {
    let received = received_for(&state, user.0.id, id)
        .await?
        .ok_or_else(HttpError::not_found)?;
    crate::federation::incoming::accept(&state.files, &received).await?;
    Ok(StatusCode::NO_CONTENT)
}

/// The share from another server whose top (or whole drive's root) a
/// node is, for its recipient.
pub(crate) async fn received_for(
    state: &AppState,
    user: Uuid,
    node: Uuid,
) -> Result<Option<crate::federation::incoming::Received>, HttpError> {
    let drive: Option<(Uuid, Option<Uuid>)> =
        sqlx::query_as("SELECT drive_id, parent_id FROM nodes WHERE id = ?1")
            .bind(node)
            .fetch_optional(state.db.reader())
            .await?;
    let Some((drive, None)) = drive else {
        return Ok(None);
    };
    Ok(
        crate::federation::incoming::received_of_drive(state.db.reader(), drive)
            .await?
            .filter(|r| r.user_id == user),
    )
}

#[cfg(test)]
mod tests {
    #[test]
    fn the_codes_are_distinct() {
        use crate::federation::outgoing::*;
        let codes = [
            super::INVITE_INVALID,
            super::INVITE_USED,
            FEDERATION_OFF,
            CONTACT_REQUIRED,
            FEDERATION_FORBIDDEN,
            SERVER_REFUSED,
            E2E_UNSUPPORTED,
            DRIVES_UNSUPPORTED,
            REMOTE_REFUSED,
            REMOTE_UNREACHABLE,
        ];
        for (i, a) in codes.iter().enumerate() {
            assert!(!codes[i + 1..].contains(a));
        }
    }
}
