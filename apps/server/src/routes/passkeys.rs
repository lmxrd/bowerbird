//! Passkeys (`passkeys.rs`): the signed-in user's list, adding and removing
//! one, and signing in with one.

use crate::error::{ErrorBody, HttpError};
use crate::extract::ApiJson;
use crate::net::client_ip;
use crate::passkeys::{self, Kind};
use crate::routes::auth::SessionUser;
use crate::session::{self, AuthSession};
use crate::state::AppState;
use axum::Json;
use axum::extract::{ConnectInfo, Path, State};
use axum::http::{HeaderMap, StatusCode};
use axum_login::AuthnBackend as _;
use serde::{Deserialize, Serialize};
use std::net::SocketAddr;
use utoipa::ToSchema;
use utoipa_axum::router::OpenApiRouter;
use utoipa_axum::routes;
use uuid::Uuid;
use webauthn_rs::prelude::{
    DiscoverableAuthentication, PasskeyRegistration, PublicKeyCredential,
    RegisterPublicKeyCredential, Webauthn,
};

/// Under /auth.
pub fn router() -> OpenApiRouter<AppState> {
    OpenApiRouter::new()
        .routes(routes!(list_passkeys))
        .routes(routes!(delete_passkey))
        .routes(routes!(start_registration))
        .routes(routes!(finish_registration))
        .routes(routes!(start_sign_in))
        .routes(routes!(finish_sign_in))
}

/// A passkey of the signed-in user.
#[derive(Serialize, ToSchema, sqlx::FromRow)]
pub struct PasskeyItem {
    pub id: Uuid,
    /// What the user called it ("MacBook", "YubiKey").
    pub name: String,
    pub created_at: String,
    #[schema(required = true)]
    pub last_used_at: Option<String>,
}

/// A ceremony started: the options for the browser
/// (`PublicKeyCredential.parseCreationOptionsFromJSON` /
/// `parseRequestOptionsFromJSON` of their `publicKey`), and the id to send
/// back with its answer, within five minutes.
#[derive(Serialize, ToSchema)]
pub struct PasskeyChallenge {
    pub challenge_id: Uuid,
    /// WebAuthn's options, as JSON (`{ "publicKey": { … } }`).
    #[schema(value_type = Object)]
    pub options: serde_json::Value,
}

/// The browser's answer to a registration.
#[derive(Deserialize, ToSchema)]
pub struct FinishRegistration {
    pub challenge_id: Uuid,
    /// What to call it; 1 to 64 characters.
    pub name: String,
    /// The new credential, as `PublicKeyCredential.toJSON()` gives it.
    #[schema(value_type = Object)]
    pub credential: serde_json::Value,
}

/// The browser's answer to a sign-in.
#[derive(Deserialize, ToSchema)]
pub struct FinishSignIn {
    pub challenge_id: Uuid,
    /// The assertion, as `PublicKeyCredential.toJSON()` gives it.
    #[schema(value_type = Object)]
    pub credential: serde_json::Value,
}

/// Whether passkeys can be used here (`PUBLIC_URL` set, and not single
/// sign-on only).
pub(crate) fn passkeys_on(state: &AppState) -> bool {
    relying_party(state).is_ok()
}

/// The relying party, or why there is none.
pub(crate) fn relying_party(state: &AppState) -> Result<Webauthn, HttpError> {
    if !state.oidc.passwords_allowed() {
        return Err(HttpError::new(
            StatusCode::FORBIDDEN,
            "Passkeys are turned off: single sign-on is the only way in",
        ));
    }
    passkeys::webauthn(state.public_url.as_deref()).ok_or_else(|| {
        HttpError::new(
            StatusCode::FORBIDDEN,
            "Passkeys need PUBLIC_URL: the server's address",
        )
    })
}

fn refused() -> HttpError {
    HttpError::new(StatusCode::UNAUTHORIZED, "That passkey was not accepted")
}

#[utoipa::path(
    get,
    path = "/passkeys",
    tag = "Account",
    operation_id = "listPasskeys",
    summary = "Your passkeys",
    responses(
        (status = 200, description = "The passkeys, the newest first", body = Vec<PasskeyItem>),
        (status = 401, description = "Not signed in", body = ErrorBody),
        (status = 403, description = "With an API key", body = ErrorBody),
    ),
    security(("session" = [])),
)]
async fn list_passkeys(
    State(state): State<AppState>,
    SessionUser(user): SessionUser,
) -> Result<Json<Vec<PasskeyItem>>, HttpError> {
    Ok(Json(
        sqlx::query_as(
            "SELECT id, name, created_at, last_used_at FROM passkeys
             WHERE user_id = ?1 ORDER BY created_at DESC",
        )
        .bind(user.id)
        .fetch_all(state.db.reader())
        .await?,
    ))
}

#[utoipa::path(
    delete,
    path = "/passkeys/{id}",
    tag = "Account",
    operation_id = "deletePasskey",
    summary = "Remove a passkey",
    description = "It no longer signs in. The authenticator still holds it: remove it there too.",
    params(("id" = Uuid, Path, description = "The passkey id")),
    responses(
        (status = 204, description = "Removed"),
        (status = 401, description = "Not signed in", body = ErrorBody),
        (status = 403, description = "With an API key", body = ErrorBody),
        (status = 404, description = "Not found", body = ErrorBody),
    ),
    security(("session" = [])),
)]
async fn delete_passkey(
    State(state): State<AppState>,
    SessionUser(user): SessionUser,
    Path(id): Path<Uuid>,
) -> Result<StatusCode, HttpError> {
    let deleted = sqlx::query("DELETE FROM passkeys WHERE id = ?1 AND user_id = ?2")
        .bind(id)
        .bind(user.id)
        .execute(state.db.writer())
        .await?
        .rows_affected();
    if deleted == 0 {
        return Err(HttpError::not_found());
    }
    Ok(StatusCode::NO_CONTENT)
}

#[utoipa::path(
    post,
    path = "/passkeys/register",
    tag = "Account",
    operation_id = "startPasskeyRegistration",
    summary = "Start adding a passkey",
    description = "The options for `navigator.credentials.create`. 403 without `PUBLIC_URL` (a passkey belongs to a domain) or with `OIDC_ONLY`.",
    responses(
        (status = 200, description = "The ceremony", body = PasskeyChallenge),
        (status = 401, description = "Not signed in", body = ErrorBody),
        (status = 403, description = "With an API key, no PUBLIC_URL, or single sign-on only", body = ErrorBody),
    ),
    security(("session" = [])),
)]
async fn start_registration(
    State(state): State<AppState>,
    SessionUser(user): SessionUser,
) -> Result<Json<PasskeyChallenge>, HttpError> {
    let webauthn = relying_party(&state)?;
    let existing = passkeys::of_user(&state.db, user.id).await?;
    let exclude = existing.iter().map(|(_, p)| p.cred_id().clone()).collect();
    let (options, registration) = webauthn
        .start_passkey_registration(user.id, &user.email, &user.email, Some(exclude))
        .map_err(|e| HttpError::internal(e.to_string()))?;
    let challenge_id =
        passkeys::save_challenge(&state.db, Kind::Register, Some(user.id), &registration).await?;
    Ok(Json(PasskeyChallenge {
        challenge_id,
        options: serde_json::to_value(options).map_err(|e| HttpError::internal(e.to_string()))?,
    }))
}

#[utoipa::path(
    post,
    path = "/passkeys",
    tag = "Account",
    operation_id = "finishPasskeyRegistration",
    summary = "Add a passkey",
    description = "With the browser's answer to the options from `/passkeys/register`.",
    request_body(content = FinishRegistration, content_type = "application/json"),
    responses(
        (status = 201, description = "Added", body = PasskeyItem),
        (status = 400, description = "Not accepted, expired, or a bad name", body = ErrorBody),
        (status = 401, description = "Not signed in", body = ErrorBody),
        (status = 403, description = "With an API key, or single sign-on only", body = ErrorBody),
    ),
    security(("session" = [])),
)]
async fn finish_registration(
    State(state): State<AppState>,
    SessionUser(user): SessionUser,
    ApiJson(body): ApiJson<FinishRegistration>,
) -> Result<(StatusCode, Json<PasskeyItem>), HttpError> {
    let webauthn = relying_party(&state)?;
    let name = body.name.trim().to_string();
    if name.is_empty() || name.chars().count() > 64 {
        return Err(HttpError::bad_request(
            "The name must be 1 to 64 characters",
        ));
    }
    let registration: PasskeyRegistration =
        passkeys::take_challenge(&state.db, body.challenge_id, Kind::Register, Some(user.id))
            .await?
            .ok_or_else(|| HttpError::bad_request("This took too long: try again"))?;
    let credential: RegisterPublicKeyCredential = serde_json::from_value(body.credential)
        .map_err(|e| HttpError::bad_request(format!("Not a credential: {e}")))?;
    let passkey = webauthn
        .finish_passkey_registration(&credential, &registration)
        .map_err(|e| HttpError::bad_request(format!("The passkey was not accepted: {e}")))?;
    let id = Uuid::now_v7();
    sqlx::query(
        "INSERT INTO passkeys (id, user_id, credential_id, name, passkey) VALUES (?1, ?2, ?3, ?4, ?5)",
    )
    .bind(id)
    .bind(user.id)
    .bind(passkey.cred_id().as_slice())
    .bind(&name)
    .bind(serde_json::to_string(&passkey).map_err(|e| HttpError::internal(e.to_string()))?)
    .execute(state.db.writer())
    .await?;
    let item =
        sqlx::query_as("SELECT id, name, created_at, last_used_at FROM passkeys WHERE id = ?1")
            .bind(id)
            .fetch_one(state.db.reader())
            .await?;
    Ok((StatusCode::CREATED, Json(item)))
}

#[utoipa::path(
    post,
    path = "/login/passkey/start",
    tag = "Auth",
    operation_id = "startPasskeySignIn",
    summary = "Start signing in with a passkey",
    description = "The options for `navigator.credentials.get`: any passkey of this server, no email needed. 403 without `PUBLIC_URL` or with `OIDC_ONLY`.",
    responses(
        (status = 200, description = "The ceremony", body = PasskeyChallenge),
        (status = 403, description = "No PUBLIC_URL, or single sign-on only", body = ErrorBody),
    ),
)]
async fn start_sign_in(State(state): State<AppState>) -> Result<Json<PasskeyChallenge>, HttpError> {
    let webauthn = relying_party(&state)?;
    let (options, authentication) = webauthn
        .start_discoverable_authentication()
        .map_err(|e| HttpError::internal(e.to_string()))?;
    let challenge_id =
        passkeys::save_challenge(&state.db, Kind::SignIn, None, &authentication).await?;
    Ok(Json(PasskeyChallenge {
        challenge_id,
        options: serde_json::to_value(options).map_err(|e| HttpError::internal(e.to_string()))?,
    }))
}

#[utoipa::path(
    post,
    path = "/login/passkey",
    tag = "Auth",
    operation_id = "finishPasskeySignIn",
    summary = "Sign in with a passkey",
    description = "Sets the session cookie. A passkey is two factors already: no code follows. Every attempt counts against the address and the user, as with passwords.",
    request_body(content = FinishSignIn, content_type = "application/json"),
    responses(
        (status = 204, description = "Signed in"),
        (status = 401, description = "Not accepted, or expired", body = ErrorBody),
        (status = 403, description = "Single sign-on only", body = ErrorBody),
        (status = 429, description = "Too many attempts, try again later", body = ErrorBody),
    ),
)]
async fn finish_sign_in(
    State(state): State<AppState>,
    mut auth: AuthSession,
    ConnectInfo(addr): ConnectInfo<SocketAddr>,
    headers: HeaderMap,
    ApiJson(body): ApiJson<FinishSignIn>,
) -> Result<StatusCode, HttpError> {
    let webauthn = relying_party(&state)?;
    let authentication: DiscoverableAuthentication =
        passkeys::take_challenge(&state.db, body.challenge_id, Kind::SignIn, None)
            .await?
            .ok_or_else(refused)?;
    let credential: PublicKeyCredential =
        serde_json::from_value(body.credential).map_err(|_| refused())?;
    let (user_id, credential_id) = webauthn
        .identify_discoverable_authentication(&credential)
        .map_err(|_| refused())?;
    let ip = client_ip(&headers, Some(addr.ip()), state.trust_proxy);
    state.limits.user(user_id, Some(&ip))?;
    let (id, mut passkey) = passkeys::of_user(&state.db, user_id)
        .await?
        .into_iter()
        .find(|(_, p)| p.cred_id().as_slice() == credential_id)
        .ok_or_else(refused)?;
    // The signature, the user's verification, and a counter that went
    // back (a cloned authenticator) are checked here.
    let result = webauthn
        .finish_discoverable_authentication_v2(&credential, authentication, &passkey)
        .map_err(|_| refused())?;
    // The counter and backup state are kept up to date.
    {
        passkey.update_credential(&result);
        sqlx::query(
            "UPDATE passkeys SET passkey = ?1,
               last_used_at = strftime('%Y-%m-%dT%H:%M:%fZ', 'now') WHERE id = ?2",
        )
        .bind(serde_json::to_string(&passkey).map_err(|e| HttpError::internal(e.to_string()))?)
        .bind(id)
        .execute(state.db.writer())
        .await?;
    }
    let user = auth
        .backend
        .get_user(&user_id)
        .await
        .map_err(anyhow::Error::new)?
        .ok_or_else(refused)?;
    // A passkey is two factors: the sign-in is a step-up too.
    session::sign_in_confirmed(&mut auth, &user).await?;
    Ok(StatusCode::NO_CONTENT)
}
