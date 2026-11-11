//! Two-factor authentication: setting up an authenticator app, the second
//! step of a sign-in, recovery codes. A reset is on the command line
//! (`organizer two-factor reset`, `recovery.rs`).
//!
//! A sign-in (password or single sign-on) of a user with two-factor
//! authentication on does not start a session: it sets a short-lived signed
//! cookie that names the user (`challenge_cookie`), and `/login/totp` starts
//! the session once a code from the app, or a recovery code, is given. With
//! `TOTP_REQUIRED`, a user without it gets that cookie in its enrolment mode
//! instead: `/login/totp/setup` and `/login/totp/enable` set the app up, and
//! the latter starts the session.

use crate::error::{ErrorBody, HttpError};
use crate::extract::ApiJson;
use crate::net::client_ip;
use crate::routes::auth::SessionUser;
use crate::session::{self, AuthSession};
use crate::state::AppState;
use crate::totp;
use axum::Json;
use axum::extract::{ConnectInfo, State};
use axum::http::{HeaderMap, StatusCode};
use axum::response::{IntoResponse, Response};
use axum_extra::extract::cookie::{Cookie, SameSite};
use axum_extra::extract::{CookieJar, SignedCookieJar};
use axum_login::AuthnBackend as _;
use serde::{Deserialize, Serialize};
use std::net::SocketAddr;
use std::time::{Duration, SystemTime, UNIX_EPOCH};
use utoipa::ToSchema;
use utoipa_axum::router::OpenApiRouter;
use utoipa_axum::routes;
use uuid::Uuid;

pub fn router() -> OpenApiRouter<AppState> {
    OpenApiRouter::new()
        .routes(routes!(login_totp))
        .routes(routes!(login_setup))
        .routes(routes!(login_enable))
        .routes(routes!(setup))
        .routes(routes!(enable))
        .routes(routes!(disable))
        .routes(routes!(regenerate_recovery_codes))
        .routes(routes!(replace))
        .routes(routes!(confirm_replace))
}

/// How long the second step may wait after the password.
const CHALLENGE_TTL: Duration = Duration::from_secs(5 * 60);
const CHALLENGE_COOKIE: &str = "organizer_2fa";
const CHALLENGE_PATH: &str = "/api/auth/login";

/// The challenge cookie's name and path: like the single sign-on cookie, a
/// `__Host-` cookie over https (which needs the path `/`), so a sibling
/// subdomain cannot plant one; over plain http it goes to the login paths
/// only.
fn challenge_name(secure: bool) -> (&'static str, &'static str) {
    if secure {
        ("__Host-organizer_2fa", "/")
    } else {
        (CHALLENGE_COOKIE, CHALLENGE_PATH)
    }
}

fn challenge(value: String, secure: bool) -> Cookie<'static> {
    let (name, path) = challenge_name(secure);
    Cookie::build((name, value))
        .path(path)
        .http_only(true)
        .same_site(SameSite::Strict)
        .secure(secure)
        .max_age(time::Duration::seconds(CHALLENGE_TTL.as_secs() as i64))
        .build()
}

fn unix_now() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0)
}

/// What a sign-in still needs before its session.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Step {
    /// A code from the app (two-factor authentication is on).
    Code,
    /// Setting the app up (it is off, and `TOTP_REQUIRED` is set).
    Enroll,
}

impl Step {
    fn as_str(self) -> &'static str {
        match self {
            Step::Code => "code",
            Step::Enroll => "enroll",
        }
    }

    /// Where single sign-on sends the browser to take this step.
    pub(crate) fn page(self) -> &'static str {
        match self {
            Step::Code => "/login?step=totp",
            Step::Enroll => "/login?step=totp-setup",
        }
    }
}

/// The step a user who just proved who they are must take next, if any.
pub(crate) async fn next_step(state: &AppState, user_id: Uuid) -> sqlx::Result<Option<Step>> {
    let on: bool = sqlx::query_scalar("SELECT totp_secret IS NOT NULL FROM users WHERE id = ?1")
        .bind(user_id)
        .fetch_one(state.db.reader())
        .await?;
    Ok(if on {
        Some(Step::Code)
    } else if two_factor_required(state, user_id).await? {
        Some(Step::Enroll)
    } else {
        None
    })
}

/// Whether a user must have two-factor authentication on: `TOTP_REQUIRED`,
/// or the `sign_in.two_factor_required` setting (the superadmin's, or the
/// strictest of their teams', docs/M7B_DESIGN.md §3).
pub(crate) async fn two_factor_required(state: &AppState, user_id: Uuid) -> sqlx::Result<bool> {
    let env = crate::settings::Env::of_state(state);
    Ok(crate::settings::resolve(
        &state.db,
        &env,
        "sign_in.two_factor_required",
        crate::settings::For::user(user_id),
    )
    .await?
    .bool())
}

/// The signed cookie that lets this browser finish the sign-in of a user
/// who proved who they are: `<user id>.<expiry, unix seconds>.<step>`. It is
/// signed with its own key, made at start: a restart makes the user sign in
/// again.
pub(crate) fn challenge_cookie(state: &AppState, user_id: Uuid, step: Step) -> SignedCookieJar {
    let expires = unix_now() + CHALLENGE_TTL.as_secs();
    let value = format!("{user_id}.{expires}.{}", step.as_str());
    SignedCookieJar::new(state.challenge_key.clone()).add(challenge(value, state.secure_cookies))
}

/// The user of a valid, unexpired challenge cookie for this step, or a 401.
fn challenged_user(state: &AppState, headers: &HeaderMap, step: Step) -> Result<Uuid, HttpError> {
    let expired = || {
        HttpError::new(
            StatusCode::UNAUTHORIZED,
            "The sign-in expired, sign in again",
        )
    };
    let (name, _) = challenge_name(state.secure_cookies);
    let value = SignedCookieJar::from_headers(headers, state.challenge_key.clone())
        .get(name)
        .ok_or_else(expired)?
        .value()
        .to_string();
    let mut parts = value.split('.');
    let (Some(user), Some(expires), Some(kind), None) =
        (parts.next(), parts.next(), parts.next(), parts.next())
    else {
        return Err(expired());
    };
    let expires: u64 = expires.parse().map_err(|_| expired())?;
    if expires <= unix_now() || kind != step.as_str() {
        return Err(expired());
    }
    user.parse().map_err(|_| expired())
}

/// Sign in the user of a sign-in that took its last step: the session,
/// and the challenge cookie used up (the jar to answer with). `used` is the
/// code it took: one of the app's is a step-up too, a recovery code not
/// (see `use_app_code`).
async fn finish_sign_in(
    state: &AppState,
    auth: &mut AuthSession,
    user_id: Uuid,
    used: Used,
) -> Result<CookieJar, HttpError> {
    let user = auth
        .backend
        .get_user(&user_id)
        .await?
        .ok_or_else(HttpError::unauthorized)?;
    match used {
        Used::App => session::sign_in_confirmed(auth, &user).await?,
        Used::Recovery => session::sign_in(auth, &user).await?,
    }
    let mut removal = challenge(String::new(), state.secure_cookies);
    removal.make_removal();
    Ok(CookieJar::new().add(removal))
}

fn invalid_code() -> HttpError {
    HttpError::bad_request("Invalid code")
}

/// Which kind of code was used.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Used {
    /// A code of the authenticator app.
    App,
    /// A recovery code.
    Recovery,
}

/// Whether `code` is a valid code of the user's authenticator app or one of
/// their recovery codes; either is used up. False when two-factor
/// authentication is off. The caller counted the attempt (`state.limits`).
pub(crate) async fn use_code(
    state: &AppState,
    user_id: Uuid,
    code: &str,
) -> Result<bool, HttpError> {
    Ok(check_code(state, user_id, code, true).await?.is_some())
}

/// Whether `code` is a valid code of the user's authenticator app, used up;
/// a recovery code does not count. This is what confirms a second factor
/// again (step-up): a recovery code is for a lost app, and spending one
/// for that would leave the user without it when the app is lost.
pub(crate) async fn use_app_code(
    state: &AppState,
    user_id: Uuid,
    code: &str,
) -> Result<bool, HttpError> {
    Ok(check_code(state, user_id, code, false).await? == Some(Used::App))
}

/// The kind of code `code` was, used up, if it is one of the user's:
/// recovery codes only when `recovery` says so.
async fn check_code(
    state: &AppState,
    user_id: Uuid,
    code: &str,
    recovery: bool,
) -> Result<Option<Used>, HttpError> {
    if code.len() > 64 {
        return Ok(None);
    }
    let row: Option<(Option<String>, i64)> =
        sqlx::query_as("SELECT totp_secret, totp_last_step FROM users WHERE id = ?1")
            .bind(user_id)
            .fetch_optional(state.db.reader())
            .await?;
    let Some((Some(secret), last_step)) = row else {
        return Ok(None);
    };
    let used = if let Some(step) =
        totp::verify(&secret, code, totp::current_step(), last_step.max(0) as u64)
    {
        // Only if no other request used this step (or a later one) since,
        // and the secret is still the one checked.
        let used = sqlx::query(
            "UPDATE users SET totp_last_step = ?1
             WHERE id = ?2 AND totp_secret = ?3 AND totp_last_step < ?1",
        )
        .bind(step as i64)
        .bind(user_id)
        .bind(&secret)
        .execute(state.db.writer())
        .await?
        .rows_affected()
            == 1;
        used.then_some(Used::App)
    } else if !recovery {
        None
    } else {
        let used = sqlx::query(
            "DELETE FROM recovery_codes WHERE user_id = ?1 AND code_hash = ?2
             AND EXISTS (SELECT 1 FROM users WHERE id = ?1 AND totp_secret IS NOT NULL)",
        )
        .bind(user_id)
        .bind(totp::recovery_code_hash(code))
        .execute(state.db.writer())
        .await?
        .rows_affected()
            == 1;
        if used {
            tracing::info!("user {user_id} used a recovery code");
        }
        used.then_some(Used::Recovery)
    };
    Ok(used)
}

/// Replace a user's recovery codes with new ones, returning them.
async fn new_recovery_codes(
    tx: &mut sqlx::SqliteConnection,
    user_id: Uuid,
) -> Result<Vec<String>, HttpError> {
    sqlx::query("DELETE FROM recovery_codes WHERE user_id = ?1")
        .bind(user_id)
        .execute(&mut *tx)
        .await?;
    let codes = totp::new_recovery_codes();
    for code in &codes {
        sqlx::query("INSERT INTO recovery_codes (user_id, code_hash) VALUES (?1, ?2)")
            .bind(user_id)
            .bind(totp::recovery_code_hash(code))
            .execute(&mut *tx)
            .await?;
    }
    Ok(codes)
}

/// A code from the authenticator app (six digits), or a recovery code.
#[derive(Deserialize, ToSchema)]
pub struct TotpCode {
    pub code: String,
}

/// A new secret for an authenticator app, not in use until it is confirmed.
#[derive(Serialize, ToSchema)]
pub struct TotpSetup {
    /// The secret, base32, for typing into the app.
    pub secret: String,
    /// The `otpauth://` URI, for a QR code.
    pub uri: String,
}

/// Single-use recovery codes, shown this once.
#[derive(Serialize, ToSchema)]
pub struct RecoveryCodes {
    pub codes: Vec<String>,
}

#[utoipa::path(
    post,
    path = "/login/totp",
    tag = "Auth",
    operation_id = "loginTotp",
    summary = "Sign in: the second step",
    description = "After `/login` answered `totp_required` (or single sign-on sent the browser to `/login?step=totp`): a code from the authenticator app, or a recovery code (used up), sets the session cookie. Needs the cookie the first step set, good for five minutes. After five wrong codes for a user within 15 minutes, their codes are refused for a while (429, with a Retry-After header).",
    request_body(content = TotpCode, content_type = "application/json"),
    responses(
        (status = 204, description = "Signed in"),
        (status = 400, description = "Invalid code", body = ErrorBody),
        (status = 401, description = "No first step, or it expired: sign in again", body = ErrorBody),
        (status = 415, description = "The body must be JSON (Content-Type: application/json)", body = ErrorBody),
        (status = 429, description = "Too many attempts, try again later", body = ErrorBody),
    ),
)]
async fn login_totp(
    State(state): State<AppState>,
    mut auth: AuthSession,
    ConnectInfo(addr): ConnectInfo<SocketAddr>,
    headers: HeaderMap,
    ApiJson(body): ApiJson<TotpCode>,
) -> Result<Response, HttpError> {
    let user_id = challenged_user(&state, &headers, Step::Code)?;
    state.limits.user(
        user_id,
        Some(&client_ip(&headers, Some(addr.ip()), state.trust_proxy)),
    )?;
    let Some(used) = check_code(&state, user_id, &body.code, true).await? else {
        return Err(invalid_code());
    };
    let jar = finish_sign_in(&state, &mut auth, user_id, used).await?;
    Ok((StatusCode::NO_CONTENT, jar).into_response())
}

#[utoipa::path(
    post,
    path = "/login/totp/setup",
    tag = "Auth",
    operation_id = "loginTotpSetup",
    summary = "Sign in: set up the authenticator app",
    description = "With `TOTP_REQUIRED`, after `/login` answered `totp_setup_required` (or single sign-on sent the browser to `/login?step=totp-setup`): a new secret for the app, as `/totp/setup` gives. Starting again replaces it.",
    responses(
        (status = 200, description = "The secret", body = TotpSetup),
        (status = 401, description = "No first step, or it expired: sign in again", body = ErrorBody),
        (status = 409, description = "Two-factor authentication is already on", body = ErrorBody),
    ),
)]
async fn login_setup(
    State(state): State<AppState>,
    headers: HeaderMap,
) -> Result<Json<TotpSetup>, HttpError> {
    let user_id = challenged_user(&state, &headers, Step::Enroll)?;
    let email: String = sqlx::query_scalar("SELECT email FROM users WHERE id = ?1")
        .bind(user_id)
        .fetch_optional(state.db.reader())
        .await?
        .ok_or_else(HttpError::unauthorized)?;
    Ok(Json(start_setup(&state, user_id, &email).await?))
}

#[utoipa::path(
    post,
    path = "/login/totp/enable",
    tag = "Auth",
    operation_id = "loginTotpEnable",
    summary = "Sign in: turn two-factor authentication on",
    description = "Confirms the secret of `/login/totp/setup` with a code from the app, sets the session cookie, and answers the recovery codes, shown this once.",
    request_body(content = TotpCode, content_type = "application/json"),
    responses(
        (status = 200, description = "On, and signed in", body = RecoveryCodes),
        (status = 400, description = "Invalid code, or no setup started", body = ErrorBody),
        (status = 401, description = "No first step, or it expired: sign in again", body = ErrorBody),
        (status = 409, description = "Two-factor authentication is already on", body = ErrorBody),
        (status = 415, description = "The body must be JSON (Content-Type: application/json)", body = ErrorBody),
        (status = 429, description = "Too many attempts, try again later", body = ErrorBody),
    ),
)]
async fn login_enable(
    State(state): State<AppState>,
    mut auth: AuthSession,
    ConnectInfo(addr): ConnectInfo<SocketAddr>,
    headers: HeaderMap,
    ApiJson(body): ApiJson<TotpCode>,
) -> Result<Response, HttpError> {
    let user_id = challenged_user(&state, &headers, Step::Enroll)?;
    state.limits.user(
        user_id,
        Some(&client_ip(&headers, Some(addr.ip()), state.trust_proxy)),
    )?;
    let codes = confirm_setup(&state, user_id, &body.code).await?;
    let jar = finish_sign_in(&state, &mut auth, user_id, Used::App).await?;
    Ok((jar, Json(RecoveryCodes { codes })).into_response())
}

#[utoipa::path(
    post,
    path = "/totp/setup",
    tag = "Account",
    operation_id = "setupTotp",
    summary = "Start setting up two-factor authentication",
    description = "A new secret for an authenticator app. Nothing changes until `/totp/enable` confirms it with a code; starting again replaces it.",
    responses(
        (status = 200, description = "The secret", body = TotpSetup),
        (status = 401, description = "Not signed in", body = ErrorBody),
        (status = 409, description = "Two-factor authentication is already on", body = ErrorBody),
    ),
    security(("session" = [])),
)]
async fn setup(
    State(state): State<AppState>,
    SessionUser(user): SessionUser,
) -> Result<Json<TotpSetup>, HttpError> {
    Ok(Json(start_setup(&state, user.id, &user.email).await?))
}

/// A new pending secret for a user who has two-factor authentication off.
async fn start_setup(state: &AppState, user_id: Uuid, email: &str) -> Result<TotpSetup, HttpError> {
    let secret = totp::new_secret();
    let updated =
        sqlx::query("UPDATE users SET totp_pending = ?1 WHERE id = ?2 AND totp_secret IS NULL")
            .bind(&secret)
            .bind(user_id)
            .execute(state.db.writer())
            .await?
            .rows_affected();
    if updated == 0 {
        return Err(already_on());
    }
    let uri = totp::uri(&secret, email)
        .ok_or_else(|| HttpError::internal("Cannot make the authenticator URI"))?;
    Ok(TotpSetup { uri, secret })
}

fn already_on() -> HttpError {
    HttpError::new(
        StatusCode::CONFLICT,
        "Two-factor authentication is already on",
    )
}

#[utoipa::path(
    post,
    path = "/totp/enable",
    tag = "Account",
    operation_id = "enableTotp",
    summary = "Turn two-factor authentication on",
    description = "Confirms the secret of `/totp/setup` with a code from the app. Answers the recovery codes, shown this once.",
    request_body(content = TotpCode, content_type = "application/json"),
    responses(
        (status = 200, description = "On", body = RecoveryCodes),
        (status = 400, description = "Invalid code, or no setup started", body = ErrorBody),
        (status = 401, description = "Not signed in", body = ErrorBody),
        (status = 409, description = "Two-factor authentication is already on", body = ErrorBody),
        (status = 415, description = "The body must be JSON (Content-Type: application/json)", body = ErrorBody),
        (status = 429, description = "Too many attempts, try again later", body = ErrorBody),
    ),
    security(("session" = [])),
)]
async fn enable(
    State(state): State<AppState>,
    SessionUser(user): SessionUser,
    mut auth: AuthSession,
    ApiJson(body): ApiJson<TotpCode>,
) -> Result<Response, HttpError> {
    state.limits.user(user.id, None)?;
    let codes = confirm_setup(&state, user.id, &body.code).await?;
    // The secret is part of what sessions are checked against: the user's
    // other sessions are signed out, this one stays.
    session::refresh(&mut auth, user.id).await?;
    Ok(Json(RecoveryCodes { codes }).into_response())
}

/// Turn two-factor authentication on with the pending secret, confirmed by
/// a code of it: the new recovery codes. The caller counted the attempt
/// (`state.limits`).
async fn confirm_setup(
    state: &AppState,
    user_id: Uuid,
    code: &str,
) -> Result<Vec<String>, HttpError> {
    let mut tx = state.db.writer().begin().await?;
    let (secret, pending): (Option<String>, Option<String>) =
        sqlx::query_as("SELECT totp_secret, totp_pending FROM users WHERE id = ?1")
            .bind(user_id)
            .fetch_one(&mut *tx)
            .await?;
    if secret.is_some() {
        return Err(already_on());
    }
    let Some(pending) = pending else {
        return Err(HttpError::bad_request("Start the setup first"));
    };
    let Some(step) = totp::verify(&pending, code, totp::current_step(), 0) else {
        return Err(invalid_code());
    };
    sqlx::query(
        "UPDATE users SET totp_secret = totp_pending, totp_pending = NULL, totp_last_step = ?1
         WHERE id = ?2",
    )
    .bind(step as i64)
    .bind(user_id)
    .execute(&mut *tx)
    .await?;
    let codes = new_recovery_codes(&mut tx, user_id).await?;
    tx.commit().await?;
    Ok(codes)
}

#[utoipa::path(
    post,
    path = "/totp/disable",
    tag = "Account",
    operation_id = "disableTotp",
    summary = "Turn two-factor authentication off",
    description = "Needs a code from the app, or a recovery code. The recovery codes are deleted. Refused while `TOTP_REQUIRED` is set.",
    request_body(content = TotpCode, content_type = "application/json"),
    responses(
        (status = 204, description = "Off"),
        (status = 400, description = "Invalid code, or two-factor authentication is off", body = ErrorBody),
        (status = 401, description = "Not signed in", body = ErrorBody),
        (status = 403, description = "Two-factor authentication is required (`TOTP_REQUIRED`)", body = ErrorBody),
        (status = 415, description = "The body must be JSON (Content-Type: application/json)", body = ErrorBody),
        (status = 429, description = "Too many attempts, try again later", body = ErrorBody),
    ),
    security(("session" = [])),
)]
async fn disable(
    State(state): State<AppState>,
    SessionUser(user): SessionUser,
    mut auth: AuthSession,
    ApiJson(body): ApiJson<TotpCode>,
) -> Result<Response, HttpError> {
    if two_factor_required(&state, user.id).await? {
        return Err(HttpError::new(
            StatusCode::FORBIDDEN,
            "Two-factor authentication is required here",
        ));
    }
    state.limits.user(user.id, None)?;
    if !use_code(&state, user.id, &body.code).await? {
        return Err(invalid_code());
    }
    turn_off(&state, user.id).await?;
    session::refresh(&mut auth, user.id).await?;
    Ok(StatusCode::NO_CONTENT.into_response())
}

/// Two-factor authentication off for a user: no secret, no recovery codes.
/// False when there is no such user.
async fn turn_off(state: &AppState, user_id: Uuid) -> Result<bool, HttpError> {
    let mut tx = state.db.writer().begin().await?;
    let updated = sqlx::query(
        "UPDATE users SET totp_secret = NULL, totp_pending = NULL, totp_last_step = 0
         WHERE id = ?1",
    )
    .bind(user_id)
    .execute(&mut *tx)
    .await?
    .rows_affected();
    sqlx::query("DELETE FROM recovery_codes WHERE user_id = ?1")
        .bind(user_id)
        .execute(&mut *tx)
        .await?;
    tx.commit().await?;
    Ok(updated == 1)
}

#[utoipa::path(
    post,
    path = "/totp/replace",
    tag = "Account",
    operation_id = "replaceTotp",
    summary = "Start replacing the authenticator app",
    description = "For a new phone or app: a code from the current app, or a recovery code, gives a new secret. Two-factor authentication stays on, with the current app, until `/totp/replace/confirm` confirms the new one; so this works while it is required (`TOTP_REQUIRED`) too. Starting again replaces the new secret.",
    request_body(content = TotpCode, content_type = "application/json"),
    responses(
        (status = 200, description = "The new secret", body = TotpSetup),
        (status = 400, description = "Invalid code, or two-factor authentication is off", body = ErrorBody),
        (status = 401, description = "Not signed in", body = ErrorBody),
        (status = 403, description = "Not with an API key", body = ErrorBody),
        (status = 415, description = "The body must be JSON (Content-Type: application/json)", body = ErrorBody),
        (status = 429, description = "Too many attempts, try again later", body = ErrorBody),
    ),
    security(("session" = [])),
)]
async fn replace(
    State(state): State<AppState>,
    SessionUser(user): SessionUser,
    ApiJson(body): ApiJson<TotpCode>,
) -> Result<Response, HttpError> {
    state.limits.user(user.id, None)?;
    if !use_code(&state, user.id, &body.code).await? {
        return Err(invalid_code());
    }
    let secret = totp::new_secret();
    let updated =
        sqlx::query("UPDATE users SET totp_pending = ?1 WHERE id = ?2 AND totp_secret IS NOT NULL")
            .bind(&secret)
            .bind(user.id)
            .execute(state.db.writer())
            .await?
            .rows_affected();
    if updated == 0 {
        return Err(HttpError::bad_request("Two-factor authentication is off"));
    }
    let uri = totp::uri(&secret, &user.email)
        .ok_or_else(|| HttpError::internal("Cannot make the authenticator URI"))?;
    Ok(Json(TotpSetup { uri, secret }).into_response())
}

#[utoipa::path(
    post,
    path = "/totp/replace/confirm",
    tag = "Account",
    operation_id = "confirmReplaceTotp",
    summary = "Finish replacing the authenticator app",
    description = "A code from the new app: its secret replaces the old one, which stops working. The recovery codes are kept. A wrong code changes nothing.",
    request_body(content = TotpCode, content_type = "application/json"),
    responses(
        (status = 204, description = "Replaced"),
        (status = 400, description = "Invalid code, or no replacement started", body = ErrorBody),
        (status = 401, description = "Not signed in", body = ErrorBody),
        (status = 403, description = "Not with an API key", body = ErrorBody),
        (status = 415, description = "The body must be JSON (Content-Type: application/json)", body = ErrorBody),
        (status = 429, description = "Too many attempts, try again later", body = ErrorBody),
    ),
    security(("session" = [])),
)]
async fn confirm_replace(
    State(state): State<AppState>,
    SessionUser(user): SessionUser,
    mut auth: AuthSession,
    ApiJson(body): ApiJson<TotpCode>,
) -> Result<Response, HttpError> {
    state.limits.user(user.id, None)?;
    let mut tx = state.db.writer().begin().await?;
    let (secret, pending): (Option<String>, Option<String>) =
        sqlx::query_as("SELECT totp_secret, totp_pending FROM users WHERE id = ?1")
            .bind(user.id)
            .fetch_one(&mut *tx)
            .await?;
    let (Some(_), Some(pending)) = (secret, pending) else {
        return Err(HttpError::bad_request("Start the replacement first"));
    };
    let Some(step) = totp::verify(&pending, &body.code, totp::current_step(), 0) else {
        return Err(invalid_code());
    };
    sqlx::query(
        "UPDATE users SET totp_secret = totp_pending, totp_pending = NULL, totp_last_step = ?1
         WHERE id = ?2",
    )
    .bind(step as i64)
    .bind(user.id)
    .execute(&mut *tx)
    .await?;
    tx.commit().await?;
    session::refresh(&mut auth, user.id).await?;
    Ok(StatusCode::NO_CONTENT.into_response())
}

#[utoipa::path(
    post,
    path = "/totp/recovery-codes",
    tag = "Account",
    operation_id = "regenerateRecoveryCodes",
    summary = "New recovery codes",
    description = "Needs a code from the app, or a recovery code. The old recovery codes stop working.",
    request_body(content = TotpCode, content_type = "application/json"),
    responses(
        (status = 200, description = "The new codes, shown this once", body = RecoveryCodes),
        (status = 400, description = "Invalid code, or two-factor authentication is off", body = ErrorBody),
        (status = 401, description = "Not signed in", body = ErrorBody),
        (status = 415, description = "The body must be JSON (Content-Type: application/json)", body = ErrorBody),
        (status = 429, description = "Too many attempts, try again later", body = ErrorBody),
    ),
    security(("session" = [])),
)]
async fn regenerate_recovery_codes(
    State(state): State<AppState>,
    SessionUser(user): SessionUser,
    ApiJson(body): ApiJson<TotpCode>,
) -> Result<Response, HttpError> {
    state.limits.user(user.id, None)?;
    if !use_code(&state, user.id, &body.code).await? {
        return Err(invalid_code());
    }
    let mut tx = state.db.writer().begin().await?;
    let codes = new_recovery_codes(&mut tx, user.id).await?;
    tx.commit().await?;
    Ok(Json(RecoveryCodes { codes }).into_response())
}
