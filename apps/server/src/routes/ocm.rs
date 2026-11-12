//! The endpoints other servers call (docs/M7E_DESIGN.md §2, §7):
//! discovery (`/.well-known/ocm`), WebFinger (`/.well-known/webfinger`,
//! RFC 7033) and the OCM API under `/ocm/` (draft-ietf-ocm-open-cloud-mesh-07):
//! `shares`, `notifications`, `invite-accepted`, `token` (the code flow),
//! `jwks`, and the shares' WebDAV at `/ocm/dav/`; and `organizer-e2e`'s
//! two (`/ocm/organizer-e2e/public-key`, `/ocm/organizer-e2e/keys`,
//! between Organizer servers only).
//!
//! Like WebDAV they are outside the OpenAPI document, which describes the
//! API of our own clients: these are OCM's, specified there. Every POST
//! must carry an HTTP Message Signature (we advertise `must-use-http-sig`);
//! answers to a refused request say only why, in OCM's statuses.

use crate::federation::address::{Address, host_of};
use crate::federation::{Trust, outgoing};
use crate::state::AppState;
use axum::Json;
use axum::Router;
use axum::extract::{ConnectInfo, Query, Request, State};
use axum::http::{HeaderValue, StatusCode, header};
use axum::response::{IntoResponse, Response};
use axum::routing::{any, get, post};
use serde::Deserialize;
use std::net::SocketAddr;

/// The routes, merged into the app outside `/api`.
pub fn router() -> Router<AppState> {
    let dav = crate::federation::DAV_PREFIX;
    Router::new()
        .route("/.well-known/ocm", get(discovery))
        .route("/.well-known/webfinger", get(webfinger))
        .route("/ocm/jwks", get(jwks))
        .route("/ocm/shares", post(shares))
        .route("/ocm/notifications", post(notifications))
        .route("/ocm/invite-accepted", post(invite_accepted))
        .route("/ocm/token", post(token))
        .route(
            &format!(
                "{}{}",
                crate::federation::OCM_PREFIX,
                crate::federation::e2e::PUBLIC_KEY_PATH
            ),
            post(e2e_public_key),
        )
        .route(
            &format!(
                "{}{}",
                crate::federation::OCM_PREFIX,
                crate::federation::e2e::KEYS_PATH
            ),
            post(e2e_keys),
        )
        .route(dav, any(crate::federation::dav::handle))
        .route(
            &format!("{dav}/{{*path}}"),
            any(crate::federation::dav::handle),
        )
}

/// A JSON answer.
fn json(status: StatusCode, body: serde_json::Value) -> Response {
    (status, Json(body)).into_response()
}

fn failed(e: impl std::fmt::Display) -> Response {
    tracing::error!("federation: {e}");
    json(
        StatusCode::INTERNAL_SERVER_ERROR,
        serde_json::json!({ "message": "Internal server error" }),
    )
}

/// A request's parts and its body (at most 1 MB).
async fn read(request: Request) -> Option<(axum::http::request::Parts, bytes::Bytes)> {
    let (parts, body) = request.into_parts();
    let body = axum::body::to_bytes(body, 1024 * 1024).await.ok()?;
    Some((parts, body))
}

async fn discovery(State(state): State<AppState>) -> Response {
    match state.files.federation().discovery().await {
        Ok(document) => json(StatusCode::OK, document),
        Err(e) => failed(e),
    }
}

async fn jwks(State(state): State<AppState>) -> Response {
    match state.files.federation().on().await {
        Ok(true) => {}
        Ok(false) => return StatusCode::NOT_FOUND.into_response(),
        Err(e) => return failed(e),
    }
    match state.files.federation().jwks() {
        Ok(set) => (
            StatusCode::OK,
            [(header::CONTENT_TYPE, "application/jwk-set+json")],
            set.to_string(),
        )
            .into_response(),
        Err(e) => failed(e),
    }
}

#[derive(Deserialize)]
struct WebFinger {
    resource: Option<String>,
}

/// WebFinger (RFC 7033) for `acct:` URIs of our users while federation is
/// on: the subject and where its OCM discovery is. Unknown accounts are
/// 404s; every look counts against the address's sign-in limit.
async fn webfinger(
    State(state): State<AppState>,
    ConnectInfo(addr): ConnectInfo<SocketAddr>,
    headers: axum::http::HeaderMap,
    Query(query): Query<WebFinger>,
) -> Response {
    let federation = state.files.federation();
    match federation.on().await {
        Ok(true) => {}
        Ok(false) => return StatusCode::NOT_FOUND.into_response(),
        Err(e) => return failed(e),
    }
    let ip = crate::net::client_ip(&headers, Some(addr.ip()), state.trust_proxy);
    if let Err(e) = state.limits.address(&ip) {
        return e.into_response();
    }
    let Some(resource) = query.resource.filter(|r| !r.is_empty()) else {
        return StatusCode::BAD_REQUEST.into_response();
    };
    let Some(account) = resource.strip_prefix("acct:") else {
        return StatusCode::NOT_FOUND.into_response();
    };
    let account = account.replace("%40", "@").replace("%2540", "@");
    let Some(address) = Address::parse(&account) else {
        return StatusCode::NOT_FOUND.into_response();
    };
    if federation.host().as_deref() != Some(address.host.as_str()) {
        return StatusCode::NOT_FOUND.into_response();
    }
    let user: Option<String> = match sqlx::query_scalar(
        "SELECT email FROM users WHERE email = ?1 COLLATE NOCASE AND disabled_at IS NULL",
    )
    .bind(&address.identifier)
    .fetch_optional(state.db.reader())
    .await
    {
        Ok(user) => user,
        Err(e) => return failed(e),
    };
    let Some(email) = user else {
        return StatusCode::NOT_FOUND.into_response();
    };
    let base = federation.base().unwrap_or_default();
    let body = serde_json::json!({
        "subject": format!("acct:{}", crate::federation::address_of(&email, &address.host)),
        "links": [{
            "rel": crate::federation::WEBFINGER_REL,
            "type": "application/json",
            "href": format!("{base}/.well-known/ocm"),
        }],
    });
    (
        StatusCode::OK,
        [
            (header::CONTENT_TYPE, "application/jrd+json"),
            (header::ACCESS_CONTROL_ALLOW_ORIGIN, "*"),
        ],
        body.to_string(),
    )
        .into_response()
}

async fn shares(State(state): State<AppState>, request: Request) -> Response {
    let (parts, body) = match read(request).await {
        Some(read) => read,
        None => return StatusCode::PAYLOAD_TOO_LARGE.into_response(),
    };
    match crate::federation::incoming::receive(&state.files, &parts, &body).await {
        Ok(((status, body), recipient)) => {
            // A new share: its recipient gets a push notice, as for a
            // local share (the words say only that something was shared).
            if let Some(user) = recipient {
                crate::push::notify(&state, user, crate::push::Kind::Shared);
            }
            json(status, body)
        }
        Err(e) => e.into_response(),
    }
}

/// `organizer-e2e` (docs/M7E_DESIGN.md §10): our user's public key, asked
/// by the server that shared an encrypted folder with them.
async fn e2e_public_key(State(state): State<AppState>, request: Request) -> Response {
    let (parts, body) = match read(request).await {
        Some(read) => read,
        None => return StatusCode::PAYLOAD_TOO_LARGE.into_response(),
    };
    match crate::federation::e2e::answer_public_key(&state.files, &parts, &body).await {
        Ok((status, body)) => json(status, body),
        Err(e) => e.into_response(),
    }
}

/// `organizer-e2e`: the keys wrapped for a member of a share of an
/// encrypted folder, asked by the member's server.
async fn e2e_keys(State(state): State<AppState>, request: Request) -> Response {
    let (parts, body) = match read(request).await {
        Some(read) => read,
        None => return StatusCode::PAYLOAD_TOO_LARGE.into_response(),
    };
    match crate::federation::e2e::answer_keys(&state.files, &parts, &body).await {
        Ok((status, body)) => json(status, body),
        Err(e) => e.into_response(),
    }
}

async fn notifications(State(state): State<AppState>, request: Request) -> Response {
    let (parts, body) = match read(request).await {
        Some(read) => read,
        None => return StatusCode::PAYLOAD_TOO_LARGE.into_response(),
    };
    match crate::federation::incoming::notification(&state.files, &parts, &body).await {
        Ok((status, body)) => json(status, body),
        Err(e) => e.into_response(),
    }
}

/// An Invite Acceptance Request's body (§4.4.3).
#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct InviteAccepted {
    recipient_provider: String,
    token: String,
    #[serde(rename = "userID")]
    user_id: String,
    #[serde(default)]
    email: String,
    #[serde(default)]
    name: String,
}

/// Someone on another server accepted an invite of one of our users
/// (§4.4): signed by their server, an invite that lives and was not
/// accepted yet; both get each other as contacts (this side here).
async fn invite_accepted(State(state): State<AppState>, request: Request) -> Response {
    let (parts, body) = match read(request).await {
        Some(read) => read,
        None => return StatusCode::PAYLOAD_TOO_LARGE.into_response(),
    };
    let message =
        |status: StatusCode, text: &str| json(status, serde_json::json!({ "message": text }));
    let federation = state.files.federation();
    match federation.on().await {
        Ok(true) => {}
        Ok(false) => return message(StatusCode::FORBIDDEN, "Federation is off on this server"),
        Err(e) => return failed(e),
    }
    let Ok(accepted) = serde_json::from_slice::<InviteAccepted>(&body) else {
        return message(StatusCode::BAD_REQUEST, "The request cannot be read");
    };
    let Some(host) = host_of(&accepted.recipient_provider) else {
        return message(StatusCode::BAD_REQUEST, "recipientProvider is not valid");
    };
    match federation.trust(&host).await {
        Ok(Trust::Refused) => {
            return message(
                StatusCode::FORBIDDEN,
                "This server does not take your server",
            );
        }
        Ok(_) => {}
        Err(e) => return failed(e),
    }
    if federation.check_signed(&parts, &body, &host).await.is_err() {
        return message(
            StatusCode::FORBIDDEN,
            "The request's signature does not verify",
        );
    }
    let identifier = accepted.user_id.trim();
    if identifier.is_empty() || identifier.len() > 300 {
        return message(StatusCode::BAD_REQUEST, "userID is not valid");
    }
    let them = Address::new(identifier, &host);
    let invite: Option<(uuid::Uuid, uuid::Uuid, String, Option<String>)> = match sqlx::query_as(
        "SELECT i.id, i.user_id, i.expires_at, i.accepted_at FROM federated_invites i
         JOIN users u ON u.id = i.user_id AND u.disabled_at IS NULL
         WHERE i.token_hash = ?1",
    )
    .bind(crate::util::sha256_hex(accepted.token.trim()))
    .fetch_optional(state.db.reader())
    .await
    {
        Ok(invite) => invite,
        Err(e) => return failed(e),
    };
    let Some((invite, user, expires_at, accepted_at)) = invite else {
        return message(StatusCode::BAD_REQUEST, "No such invite");
    };
    if accepted_at.is_some() {
        return message(StatusCode::CONFLICT, "The invite was accepted already");
    }
    if expires_at < crate::auth::now_iso() {
        return message(StatusCode::BAD_REQUEST, "The invite has expired");
    }
    // What their server is, for the contact (kept from the signature's check).
    let server = federation.discover(&host).await.ok();
    let email: String = match sqlx::query_scalar("SELECT email FROM users WHERE id = ?1")
        .bind(user)
        .fetch_one(state.db.reader())
        .await
    {
        Ok(email) => email,
        Err(e) => return failed(e),
    };
    let done = async {
        let mut tx = state.db.writer().begin().await?;
        let taken = sqlx::query(
            "UPDATE federated_invites SET accepted_at = ?1, accepted_by = ?2, token = NULL
             WHERE id = ?3 AND accepted_at IS NULL",
        )
        .bind(crate::auth::now_iso())
        .bind(them.to_string())
        .bind(invite)
        .execute(&mut *tx)
        .await?
        .rows_affected();
        if taken == 1 {
            crate::routes::federation::add_contact(
                &mut tx,
                user,
                &them,
                &accepted.name,
                &accepted.email,
                server.as_deref(),
            )
            .await?;
        }
        tx.commit().await?;
        Ok::<_, sqlx::Error>(taken == 1)
    };
    match done.await {
        Ok(true) => {}
        Ok(false) => return message(StatusCode::CONFLICT, "The invite was accepted already"),
        Err(e) => return failed(e),
    }
    json(
        StatusCode::OK,
        serde_json::json!({ "userID": email, "email": email, "name": email }),
    )
}

/// The code flow's token request (§10.1).
#[derive(Deserialize)]
struct TokenRequest {
    grant_type: Option<String>,
    client_id: Option<String>,
    code: Option<String>,
}

/// The code flow (§10): another server exchanges a share's secret for a
/// short-lived bearer token, signed, as the share's recipient's server.
async fn token(State(state): State<AppState>, request: Request) -> Response {
    let (parts, body) = match read(request).await {
        Some(read) => read,
        None => return StatusCode::PAYLOAD_TOO_LARGE.into_response(),
    };
    let error = |code: &str| {
        (
            StatusCode::BAD_REQUEST,
            [(header::CACHE_CONTROL, "no-store")],
            Json(serde_json::json!({ "error": code })),
        )
            .into_response()
    };
    let federation = state.files.federation();
    match federation.on().await {
        Ok(true) => {}
        Ok(false) => return error("unauthorized_client"),
        Err(e) => return failed(e),
    }
    let form_type = parts
        .headers
        .get(header::CONTENT_TYPE)
        .and_then(|v| v.to_str().ok())
        .is_some_and(|t| t.starts_with("application/x-www-form-urlencoded"));
    if !form_type {
        return error("invalid_request");
    }
    let form = axum::http::Request::builder()
        .method("POST")
        .header(header::CONTENT_TYPE, "application/x-www-form-urlencoded")
        .body(axum::body::Body::from(body.clone()))
        .expect("a request");
    let parsed =
        <axum::Form<TokenRequest> as axum::extract::FromRequest<()>>::from_request(form, &()).await;
    let Ok(axum::Form(form)) = parsed else {
        return error("invalid_request");
    };
    let (Some(grant_type), Some(client), Some(code)) = (form.grant_type, form.client_id, form.code)
    else {
        return error("invalid_request");
    };
    if grant_type != "authorization_code" {
        return error("unsupported_grant_type");
    }
    let Some(client) = host_of(&client) else {
        return error("invalid_client");
    };
    match federation.trust(&client).await {
        Ok(Trust::Refused) => return error("unauthorized_client"),
        Ok(_) => {}
        Err(e) => return failed(e),
    }
    if federation
        .check_signed(&parts, &body, &client)
        .await
        .is_err()
    {
        return error("invalid_client");
    }
    let share: Option<uuid::Uuid> =
        match sqlx::query_scalar("SELECT id FROM ocm_shares WHERE secret_hash = ?1")
            .bind(crate::util::sha256_hex(code.trim()))
            .fetch_optional(state.db.reader())
            .await
        {
            Ok(share) => share,
            Err(e) => return failed(e),
        };
    let share = match share {
        Some(id) => match outgoing::share_row(state.db.reader(), id).await {
            Ok(share) => share,
            Err(e) => return failed(e),
        },
        None => None,
    };
    let Some(share) =
        share.filter(|s| s.host == client && s.state != outgoing::ShareState::Declined)
    else {
        return error("invalid_grant");
    };
    match outgoing::drive_allows(&state.files, share.drive_id).await {
        Ok(true) => {}
        Ok(false) => return error("invalid_grant"),
        Err(e) => return failed(e),
    }
    let token = federation.issue(share.id);
    let mut response = Json(serde_json::json!({
        "access_token": token,
        "token_type": "Bearer",
        "expires_in": crate::federation::TOKEN_SECS,
    }))
    .into_response();
    response
        .headers_mut()
        .insert(header::CACHE_CONTROL, HeaderValue::from_static("no-store"));
    response
}
