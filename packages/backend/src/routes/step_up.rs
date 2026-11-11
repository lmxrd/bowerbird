//! Step-up (docs/M7A_DESIGN.md §4): the signed-in user confirms a second
//! factor again, a code of their authenticator app or one of their
//! passkeys, and the superadmin's endpoints answer for 15 minutes
//! (`routes::auth::Superadmin`). The time is kept in the session
//! (`session::step_up`); a sign-in that took a passkey or an app code is
//! one too.

use crate::error::{ErrorBody, HttpError};
use crate::extract::ApiJson;
use crate::net::client_ip;
use crate::passkeys::{self, Kind};
use crate::routes::auth::{SessionUser, iso_of};
use crate::routes::passkeys::{FinishSignIn, PasskeyChallenge, relying_party};
use crate::routes::totp::{TotpCode, use_app_code};
use crate::session::{self, AuthSession};
use crate::state::AppState;
use axum::Json;
use axum::extract::{ConnectInfo, State};
use axum::http::{HeaderMap, StatusCode};
use serde::Serialize;
use std::net::SocketAddr;
use utoipa::ToSchema;
use utoipa_axum::router::OpenApiRouter;
use utoipa_axum::routes;
use webauthn_rs::prelude::{PasskeyAuthentication, PublicKeyCredential};

/// Under /auth.
pub fn router() -> OpenApiRouter<AppState> {
    OpenApiRouter::new()
        .routes(routes!(step_up_totp))
        .routes(routes!(start_step_up_passkey))
        .routes(routes!(step_up_passkey))
}

/// A second factor confirmed.
#[derive(Serialize, ToSchema)]
pub struct SteppedUp {
    /// Until when the superadmin's endpoints answer without asking again.
    pub until: String,
}

#[utoipa::path(
    post,
    path = "/step-up/totp",
    tag = "Account",
    operation_id = "stepUpTotp",
    summary = "Confirm it's you with a two-factor code",
    description = "A code of the authenticator app (not a recovery code: those are for a lost app) opens the superadmin's endpoints for this session for 15 minutes. Counts against the user's code attempts (5, then 1 a minute; 429 with a Retry-After header).",
    request_body(content = TotpCode, content_type = "application/json"),
    responses(
        (status = 200, description = "Confirmed", body = SteppedUp),
        (status = 400, description = "Invalid code, or two-factor authentication is off", body = ErrorBody),
        (status = 401, description = "Not signed in", body = ErrorBody),
        (status = 403, description = "Not with an API key", body = ErrorBody),
        (status = 415, description = "The body must be JSON (Content-Type: application/json)", body = ErrorBody),
        (status = 429, description = "Too many attempts, try again later", body = ErrorBody),
    ),
    security(("session" = [])),
)]
async fn step_up_totp(
    State(state): State<AppState>,
    SessionUser(user): SessionUser,
    auth: AuthSession,
    ConnectInfo(addr): ConnectInfo<SocketAddr>,
    headers: HeaderMap,
    ApiJson(body): ApiJson<TotpCode>,
) -> Result<Json<SteppedUp>, HttpError> {
    let ip = client_ip(&headers, Some(addr.ip()), state.trust_proxy);
    state.limits.user(user.id, Some(&ip))?;
    if !use_app_code(&state, user.id, &body.code).await? {
        return Err(HttpError::bad_request("Invalid code"));
    }
    let until = session::step_up(&auth.session, user.id).await?;
    Ok(Json(SteppedUp {
        until: iso_of(until),
    }))
}

#[utoipa::path(
    post,
    path = "/step-up/passkey/start",
    tag = "Account",
    operation_id = "startStepUpPasskey",
    summary = "Start confirming it's you with a passkey",
    description = "The options for `navigator.credentials.get`, limited to your own passkeys. 403 without `PUBLIC_URL` or with `OIDC_ONLY`.",
    responses(
        (status = 200, description = "The ceremony", body = PasskeyChallenge),
        (status = 400, description = "You have no passkey", body = ErrorBody),
        (status = 401, description = "Not signed in", body = ErrorBody),
        (status = 403, description = "With an API key, no PUBLIC_URL, or single sign-on only", body = ErrorBody),
    ),
    security(("session" = [])),
)]
async fn start_step_up_passkey(
    State(state): State<AppState>,
    SessionUser(user): SessionUser,
) -> Result<Json<PasskeyChallenge>, HttpError> {
    let webauthn = relying_party(&state)?;
    let keys: Vec<_> = passkeys::of_user(&state.db, user.id)
        .await?
        .into_iter()
        .map(|(_, passkey)| passkey)
        .collect();
    if keys.is_empty() {
        return Err(HttpError::bad_request("You have no passkey"));
    }
    // Not discoverable: the browser is told which credentials are the
    // user's, so another account's passkey on the same device is not
    // offered (and would not be accepted).
    let (options, authentication) = webauthn
        .start_passkey_authentication(&keys)
        .map_err(|e| HttpError::internal(e.to_string()))?;
    let challenge_id =
        passkeys::save_challenge(&state.db, Kind::StepUp, Some(user.id), &authentication).await?;
    Ok(Json(PasskeyChallenge {
        challenge_id,
        options: serde_json::to_value(options).map_err(|e| HttpError::internal(e.to_string()))?,
    }))
}

#[utoipa::path(
    post,
    path = "/step-up/passkey",
    tag = "Account",
    operation_id = "stepUpPasskey",
    summary = "Confirm it's you with a passkey",
    description = "With the browser's answer to the options from `/step-up/passkey/start`: opens the superadmin's endpoints for this session for 15 minutes. Every attempt counts against the user's code attempts.",
    request_body(content = FinishSignIn, content_type = "application/json"),
    responses(
        (status = 200, description = "Confirmed", body = SteppedUp),
        (status = 401, description = "Not signed in, or the passkey was not accepted", body = ErrorBody),
        (status = 403, description = "With an API key, no PUBLIC_URL, or single sign-on only", body = ErrorBody),
        (status = 415, description = "The body must be JSON (Content-Type: application/json)", body = ErrorBody),
        (status = 429, description = "Too many attempts, try again later", body = ErrorBody),
    ),
    security(("session" = [])),
)]
async fn step_up_passkey(
    State(state): State<AppState>,
    SessionUser(user): SessionUser,
    auth: AuthSession,
    ConnectInfo(addr): ConnectInfo<SocketAddr>,
    headers: HeaderMap,
    ApiJson(body): ApiJson<FinishSignIn>,
) -> Result<Json<SteppedUp>, HttpError> {
    let webauthn = relying_party(&state)?;
    let ip = client_ip(&headers, Some(addr.ip()), state.trust_proxy);
    state.limits.user(user.id, Some(&ip))?;
    let refused = || HttpError::new(StatusCode::UNAUTHORIZED, "That passkey was not accepted");
    let authentication: PasskeyAuthentication =
        passkeys::take_challenge(&state.db, body.challenge_id, Kind::StepUp, Some(user.id))
            .await?
            .ok_or_else(refused)?;
    let credential: PublicKeyCredential =
        serde_json::from_value(body.credential).map_err(|_| refused())?;
    // The signature, the user's verification, that the credential is one
    // of the user's, and a counter that went back are checked here.
    let result = webauthn
        .finish_passkey_authentication(&credential, &authentication)
        .map_err(|_| refused())?;
    let (id, mut passkey) = passkeys::of_user(&state.db, user.id)
        .await?
        .into_iter()
        .find(|(_, p)| p.cred_id() == result.cred_id())
        .ok_or_else(refused)?;
    // The counter and backup state are kept up to date.
    passkey.update_credential(&result);
    sqlx::query(
        "UPDATE passkeys SET passkey = ?1,
           last_used_at = strftime('%Y-%m-%dT%H:%M:%fZ', 'now') WHERE id = ?2",
    )
    .bind(serde_json::to_string(&passkey).map_err(|e| HttpError::internal(e.to_string()))?)
    .bind(id)
    .execute(state.db.writer())
    .await?;
    let until = session::step_up(&auth.session, user.id).await?;
    Ok(Json(SteppedUp {
        until: iso_of(until),
    }))
}
