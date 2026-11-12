//! An app's sign-in (docs/M1_DESIGN.md §3.2): a device token, which the app
//! sends as `Authorization: Bearer`.
//!
//! - **With a password**: `POST /auth/device/password`, in one call (the
//!   code of the authenticator app in it, when the user has one).
//! - **Through the browser**, for single sign-on or any other way of
//!   signing in (RFC 8252, OAuth for native apps): the app opens
//!   `/api/auth/device/authorize` in the system browser with its redirect
//!   and a PKCE challenge. Someone not signed in goes through the web
//!   login first. The approval page (`/settings/devices/approve`) asks them to allow the
//!   device and posts the approval, which makes a one-time code; the
//!   browser is sent to the app's redirect with it, and the app exchanges
//!   it, with its PKCE verifier, at `/auth/device/token`.
//!
//! Redirects go only to the app (`organizer://auth`) or to this machine's
//! loopback address (a desktop app's local listener).

use crate::auth::{KeyKind, Scope};
use crate::error::{ErrorBody, HttpError};
use crate::extract::ApiJson;
use crate::net::client_ip;
use crate::routes::api_keys::{CreatedApiKey, issue};
use crate::routes::auth::{SessionUser, validate_email};
use crate::routes::totp::{Step, next_step, use_code};
use crate::session::{self, AuthSession};
use crate::state::AppState;
use axum::Json;
use axum::extract::{ConnectInfo, Query, State};
use axum::http::{HeaderMap, StatusCode};
use axum::response::Redirect;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::net::SocketAddr;
use std::time::{Duration, SystemTime};
use utoipa::{IntoParams, ToSchema};
use utoipa_axum::router::OpenApiRouter;
use utoipa_axum::routes;

pub fn router() -> OpenApiRouter<AppState> {
    OpenApiRouter::new()
        .routes(routes!(password))
        .routes(routes!(authorize_start, authorize))
        .routes(routes!(token))
        .routes(routes!(sign_out))
}

#[utoipa::path(
    delete,
    path = "/device",
    tag = "Auth",
    operation_id = "signOutDevice",
    summary = "Sign this device out",
    description = "Revokes the token sent as `Authorization: Bearer` (an app's device token, or any API key, read-only ones included): an app signing out. 204 when there is no such token any more.",
    responses(
        (status = 204, description = "The token is revoked"),
    ),
    security(("api_key" = [])),
)]
async fn sign_out(
    State(state): State<AppState>,
    headers: axum::http::HeaderMap,
) -> Result<axum::http::StatusCode, crate::error::HttpError> {
    if let Some((_, key)) = match crate::routes::auth::bearer(&headers) {
        Some(token) => crate::auth::key_user(state.db.reader(), token).await,
        None => None,
    } {
        sqlx::query("DELETE FROM api_keys WHERE id = ?1")
            .bind(key.id)
            .execute(state.db.writer())
            .await?;
    }
    Ok(axum::http::StatusCode::NO_CONTENT)
}

/// The app's own redirect, for the mobile apps.
const APP_REDIRECT: &str = "organizer://auth";
/// How long a one-time code is good for.
const CODE_TTL: Duration = Duration::from_secs(60);
/// What a device token may do: everything but the superadmin endpoints.
const DEVICE_SCOPE: Scope = Scope::Write;

/// Whether a page is one a sign-in may go on to: a path of this server
/// (`/settings/contacts?token=…`, the device approval page), never another
/// origin. A browser takes `//host` and `/\host` for another host, and
/// drops tabs and newlines, so none of them, nor anything but printable
/// ASCII (a path's other characters come percent-encoded).
pub fn allowed_next(next: &str) -> bool {
    let bytes = next.as_bytes();
    bytes.first() == Some(&b'/')
        && !matches!(bytes.get(1), Some(b'/' | b'\\'))
        && next.len() <= 2000
        && bytes.iter().all(|b| b.is_ascii_graphic() && *b != b'\\')
}

/// A value for a URL's query, percent-encoded.
pub fn percent_encode(value: &str) -> String {
    let mut out = String::new();
    for byte in value.bytes() {
        if byte.is_ascii_alphanumeric() || b"-._~".contains(&byte) {
            out.push(byte as char);
        } else {
            out.push_str(&format!("%{byte:02X}"));
        }
    }
    out
}

/// Whether an app may be sent a code at this redirect: the mobile apps'
/// scheme, or the loopback address of this machine on any port (RFC 8252
/// §7.3), never another host.
fn allowed_redirect(uri: &str) -> bool {
    if uri == APP_REDIRECT {
        return true;
    }
    let Some(rest) = uri.strip_prefix("http://") else {
        return false;
    };
    let host = rest.split(['/', '?', '#']).next().unwrap_or("");
    let host = host.rsplit_once(':').map(|(h, _)| h).unwrap_or(host);
    matches!(host, "127.0.0.1" | "[::1]") && !uri.contains(['\r', '\n'])
}

/// base64url without padding, for PKCE.
fn base64url(bytes: &[u8]) -> String {
    const ALPHABET: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789-_";
    let mut out = String::new();
    for chunk in bytes.chunks(3) {
        let b = [
            chunk[0],
            *chunk.get(1).unwrap_or(&0),
            *chunk.get(2).unwrap_or(&0),
        ];
        let n = (b[0] as u32) << 16 | (b[1] as u32) << 8 | b[2] as u32;
        let chars = chunk.len() + 1;
        for i in 0..chars {
            out.push(ALPHABET[(n >> (18 - 6 * i) & 63) as usize] as char);
        }
    }
    out
}

/// PKCE S256: the challenge of a verifier.
fn challenge_of(verifier: &str) -> String {
    base64url(&Sha256::digest(verifier.as_bytes()))
}

fn device_name(raw: &str) -> Result<String, HttpError> {
    let name = raw.trim();
    if name.is_empty() || name.chars().count() > 100 {
        return Err(HttpError::bad_request(
            "Expected a device name of 1 to 100 characters",
        ));
    }
    Ok(name.to_string())
}

/// A device's sign-in with a password.
#[derive(Deserialize, ToSchema)]
pub struct DevicePassword {
    pub email: String,
    pub password: String,
    /// What the device is called in the list of devices, e.g. "Anna's iPhone".
    pub device_name: String,
    /// The code of the authenticator app, or a recovery code, for a user
    /// with two-factor authentication on: left out at first, sent again
    /// with the credentials once the answer says it is required.
    #[serde(default)]
    pub code: Option<String>,
}

/// The answer to a device's password sign-in: the token, or that the code
/// of the authenticator app is next.
#[derive(Serialize, ToSchema)]
#[serde(untagged)]
pub enum DeviceSignIn {
    Signed(CreatedApiKey),
    Code(CodeRequired),
}

#[derive(Serialize, ToSchema)]
pub struct CodeRequired {
    /// Always true: send the credentials again with `code`.
    pub totp_required: bool,
}

#[utoipa::path(
    post,
    path = "/device/password",
    tag = "Auth",
    operation_id = "deviceSignInWithPassword",
    summary = "Sign a device in with a password",
    description = "For the apps: answers a device token (`Authorization: Bearer`), or `totp_required` when the user has two-factor authentication on (send everything again with `code`). With `TOTP_REQUIRED`, a user who has not set it up does so in the web app first. Refused with `OIDC_ONLY`: sign in through the browser instead. Limited like the web sign-in.",
    request_body(content = DevicePassword, content_type = "application/json"),
    responses(
        (status = 200, description = "The device token, or that the code is required", body = DeviceSignIn),
        (status = 400, description = "An invalid device name", body = ErrorBody),
        (status = 401, description = "Wrong email, password or code", body = ErrorBody),
        (status = 403, description = "Password sign-in is off, or two-factor authentication must be set up first", body = ErrorBody),
        (status = 429, description = "Too many attempts, try again later", body = ErrorBody),
    ),
)]
async fn password(
    State(state): State<AppState>,
    auth: AuthSession,
    ConnectInfo(addr): ConnectInfo<SocketAddr>,
    headers: HeaderMap,
    ApiJson(body): ApiJson<DevicePassword>,
) -> Result<Json<DeviceSignIn>, HttpError> {
    if !state.oidc.passwords_allowed() {
        return Err(HttpError::passwords_off());
    }
    let device = device_name(&body.device_name)?;
    let wrong = || HttpError::new(StatusCode::UNAUTHORIZED, "Invalid email or password");
    let email = validate_email(&body.email).map_err(|_| wrong())?;
    let ip = client_ip(&headers, Some(addr.ip()), state.trust_proxy);
    state.limits.sign_in(&ip, &email)?;
    let user = auth
        .authenticate(session::Credentials {
            email,
            password: body.password,
        })
        .await
        .map_err(anyhow::Error::new)?
        .ok_or_else(wrong)?;
    if user.disabled {
        return Err(HttpError::account_disabled());
    }
    match next_step(&state, user.id).await? {
        None => {}
        Some(Step::Enroll) => {
            return Err(HttpError::new(
                StatusCode::FORBIDDEN,
                "Set up two-factor authentication in the web app first",
            ));
        }
        Some(Step::Code) => {
            let Some(code) = body.code else {
                return Ok(Json(DeviceSignIn::Code(CodeRequired {
                    totp_required: true,
                })));
            };
            state.limits.user(user.id, Some(&ip))?;
            if !use_code(&state, user.id, &code).await? {
                return Err(HttpError::new(StatusCode::UNAUTHORIZED, "Invalid code"));
            }
        }
    }
    let created = issue(
        &state,
        user.id,
        KeyKind::Device,
        &device,
        DEVICE_SCOPE,
        None,
    )
    .await?;
    tracing::info!("user {} signed a device in: {device}", user.id);
    Ok(Json(DeviceSignIn::Signed(created)))
}

/// What an app asks for when it starts its sign-in in the browser.
#[derive(Deserialize, IntoParams, ToSchema)]
#[into_params(parameter_in = Query)]
pub struct Authorize {
    /// `organizer://auth`, or `http://127.0.0.1:<port>/...` (a desktop app).
    pub redirect_uri: String,
    /// PKCE: base64url(SHA-256(verifier)).
    pub code_challenge: String,
    /// What the device is called in the list of devices.
    pub device_name: String,
}

impl Authorize {
    fn check(&self) -> Result<(), HttpError> {
        if !allowed_redirect(&self.redirect_uri) {
            return Err(HttpError::bad_request(
                "redirect_uri must be organizer://auth or this machine's loopback address",
            ));
        }
        if !(43..=128).contains(&self.code_challenge.len())
            || !self
                .code_challenge
                .bytes()
                .all(|b| b.is_ascii_alphanumeric() || b == b'-' || b == b'_')
        {
            return Err(HttpError::bad_request("Invalid code_challenge (S256)"));
        }
        device_name(&self.device_name)?;
        Ok(())
    }

    fn query(&self) -> String {
        format!(
            "redirect_uri={}&code_challenge={}&device_name={}",
            percent_encode(&self.redirect_uri),
            percent_encode(&self.code_challenge),
            percent_encode(&self.device_name)
        )
    }
}

#[utoipa::path(
    get,
    path = "/device/authorize",
    tag = "Auth",
    operation_id = "deviceAuthorizeStart",
    summary = "Start a device's sign-in in the browser",
    description = "The app opens this in the system browser. It goes to the approval page (`/settings/devices/approve`), through the web login first for someone not signed in.",
    params(Authorize),
    responses(
        (status = 303, description = "To the approval page, or to the login page first"),
        (status = 400, description = "An invalid redirect, challenge or name", body = ErrorBody),
    ),
)]
async fn authorize_start(
    auth: AuthSession,
    Query(params): Query<Authorize>,
) -> Result<Redirect, HttpError> {
    params.check()?;
    let approval = format!("/settings/devices/approve?{}", params.query());
    Ok(if auth.user.is_some() {
        Redirect::to(&approval)
    } else {
        Redirect::to(&format!("/login?next={}", percent_encode(&approval)))
    })
}

/// Where to send the browser: the app's redirect, with the code.
#[derive(Serialize, ToSchema)]
pub struct Approved {
    pub redirect: String,
}

#[utoipa::path(
    post,
    path = "/device/authorize",
    tag = "Auth",
    operation_id = "deviceAuthorize",
    summary = "Allow a device",
    description = "The approval page posts this when the signed-in user allows the device. Answers where to send the browser: the app's redirect with a one-time code, good for a minute.",
    request_body(content = Authorize, content_type = "application/json"),
    responses(
        (status = 200, description = "Allowed", body = Approved),
        (status = 400, description = "An invalid redirect, challenge or name", body = ErrorBody),
        (status = 401, description = "Not signed in", body = ErrorBody),
        (status = 403, description = "Not with an API key", body = ErrorBody),
    ),
    security(("session" = [])),
)]
async fn authorize(
    State(state): State<AppState>,
    SessionUser(user): SessionUser,
    ApiJson(body): ApiJson<Authorize>,
) -> Result<Json<Approved>, HttpError> {
    body.check()?;
    let code = crate::util::random_hex(32);
    sqlx::query(
        "INSERT INTO device_codes (code_hash, user_id, code_challenge, redirect_uri, device_name, expires_at)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
    )
    .bind(crate::util::sha256_hex(&code))
    .bind(user.id)
    .bind(&body.code_challenge)
    .bind(&body.redirect_uri)
    .bind(device_name(&body.device_name)?)
    .bind(crate::auth::iso(SystemTime::now() + CODE_TTL))
    .execute(state.db.writer())
    .await?;
    let separator = if body.redirect_uri.contains('?') {
        '&'
    } else {
        '?'
    };
    Ok(Json(Approved {
        redirect: format!("{}{separator}code={code}", body.redirect_uri),
    }))
}

/// The app's exchange of its one-time code.
#[derive(Deserialize, ToSchema)]
pub struct DeviceToken {
    pub code: String,
    /// The PKCE verifier the challenge was made from.
    pub code_verifier: String,
}

#[utoipa::path(
    post,
    path = "/device/token",
    tag = "Auth",
    operation_id = "deviceToken",
    summary = "Exchange a device's one-time code for its token",
    description = "The code works once, for a minute, with the PKCE verifier of its challenge.",
    request_body(content = DeviceToken, content_type = "application/json"),
    responses(
        (status = 200, description = "The device token", body = CreatedApiKey),
        (status = 400, description = "An unknown, used or expired code, or the wrong verifier", body = ErrorBody),
    ),
)]
async fn token(
    State(state): State<AppState>,
    ApiJson(body): ApiJson<DeviceToken>,
) -> Result<Json<CreatedApiKey>, HttpError> {
    let invalid = || HttpError::bad_request("Invalid or expired code");
    // Used up whatever happens next: a code is good for one try.
    let row: Option<(uuid::Uuid, String, String, String)> = sqlx::query_as(
        "DELETE FROM device_codes WHERE code_hash = ?1
         RETURNING user_id, code_challenge, device_name, expires_at",
    )
    .bind(crate::util::sha256_hex(&body.code))
    .fetch_optional(state.db.writer())
    .await?;
    let (user_id, challenge, device, expires_at) = row.ok_or_else(invalid)?;
    if expires_at < crate::auth::now_iso() || challenge_of(&body.code_verifier) != challenge {
        return Err(invalid());
    }
    let created = issue(
        &state,
        user_id,
        KeyKind::Device,
        &device,
        DEVICE_SCOPE,
        None,
    )
    .await?;
    tracing::info!("user {user_id} signed a device in: {device}");
    Ok(Json(created))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn pkce_follows_rfc_7636() {
        // The example of RFC 7636, appendix B.
        assert_eq!(
            challenge_of("dBjftJeZ4CVP-mB92K27uhbUJU1p1r_wW1gFWFOEjXk"),
            "E9Melhoa2OwvFrEMTJguCHaoeK1t8URWbuGJSstw-cM"
        );
    }

    #[test]
    fn codes_go_only_to_the_app_or_this_machine() {
        for good in [
            "organizer://auth",
            "http://127.0.0.1:53123/callback",
            "http://127.0.0.1/cb",
            "http://[::1]:8080/",
        ] {
            assert!(allowed_redirect(good), "{good}");
        }
        for bad in [
            "https://evil.example.com/",
            "http://127.0.0.1.evil.example.com/",
            "http://localhost:8080/",
            "organizer://other",
            "javascript:alert(1)",
        ] {
            assert!(!allowed_redirect(bad), "{bad}");
        }
    }

    #[test]
    fn a_sign_in_goes_on_only_to_a_page_of_this_server() {
        for good in [
            "/",
            "/settings/devices/approve?redirect_uri=x",
            "/settings/contacts?token=abc&providerDomain=cloud.example.com",
            "/drives/1/Plans%20B",
        ] {
            assert!(allowed_next(good), "{good}");
        }
        for bad in [
            "",
            "https://evil.example.com",
            "//evil.example.com",
            "/\\evil.example.com",
            "\\\\evil.example.com",
            "/\t/evil.example.com",
            "/a\nb",
            "/a b",
            "settings",
            "javascript:alert(1)",
            "/caf\u{e9}",
        ] {
            assert!(!allowed_next(bad), "{bad}");
        }
    }
}
