use crate::auth::{KeyKind, KeyUse, Scope, User, hash_password, iso, key_user, verify_password};
use crate::error::{ErrorBody, HttpError};
use crate::extract::ApiJson;
use crate::net::client_ip;
use crate::routes::totp::{Step, challenge_cookie, next_step};
use crate::session::{self, AuthSession};
use crate::state::AppState;
use axum::Json;
use axum::extract::{ConnectInfo, FromRequestParts, State};
use axum::http::{HeaderMap, StatusCode};
use axum::response::{IntoResponse, Response};
use axum_login::AuthnBackend as _;
use serde::{Deserialize, Serialize};
use std::net::SocketAddr;
use std::time::{Duration, SystemTime};
use utoipa::ToSchema;
use utoipa_axum::router::OpenApiRouter;
use utoipa_axum::routes;
use uuid::Uuid;

pub fn router() -> OpenApiRouter<AppState> {
    OpenApiRouter::new()
        .routes(routes!(session))
        .routes(routes!(setup))
        .routes(routes!(login))
        .routes(routes!(logout))
        .routes(routes!(change_email))
        .routes(routes!(change_password))
}

/// How a request is signed in.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Access {
    /// The session cookie: everything the user may do.
    Session,
    /// An API key (`Authorization: Bearer`), capped by its scope.
    Key(Scope),
}

/// The signed-in user, as a request extractor: a valid session cookie, or
/// an API key, or a 401. A read key is refused anything but GET and HEAD
/// here (403). The account's own settings (email, password, two-factor
/// authentication, API keys) take a `SessionUser` instead, and the
/// superadmin's endpoints a [`Superadmin`]: no key reaches them.
pub struct AuthUser(pub User, pub Access);

impl AuthUser {
    /// What a write by this user is recorded as, with the version it
    /// replaces.
    pub fn source(&self) -> crate::files::versions::Source {
        match self.1 {
            Access::Session => crate::files::versions::Source::Web,
            Access::Key(_) => crate::files::versions::Source::Api,
        }
    }
}

impl FromRequestParts<AppState> for AuthUser {
    type Rejection = HttpError;

    async fn from_request_parts(
        parts: &mut axum::http::request::Parts,
        state: &AppState,
    ) -> Result<Self, Self::Rejection> {
        if let Some(token) = bearer(&parts.headers) {
            let (user, key) = key_user(state.db.reader(), token)
                .await
                // An app password is for WebDAV's Basic authentication only.
                .filter(|(_, key)| key.kind != KeyKind::AppPassword)
                .ok_or_else(HttpError::unauthorized)?;
            if key.scope == Scope::Read && !parts.method.is_safe() {
                return Err(HttpError::new(
                    StatusCode::FORBIDDEN,
                    "This API key can only read",
                ));
            }
            note_key_use(state, &key).await;
            crate::auth::note_seen(&state.db, &user).await;
            return Ok(AuthUser(user, Access::Key(key.scope)));
        }
        let user = auth_session(parts, state)
            .await?
            .user
            .ok_or_else(HttpError::unauthorized)?;
        crate::auth::note_seen(&state.db, &user).await;
        Ok(AuthUser(user, Access::Session))
    }
}

/// The request's session (axum-login loads the signed-in user, if any).
async fn auth_session(
    parts: &mut axum::http::request::Parts,
    state: &AppState,
) -> Result<AuthSession, HttpError> {
    AuthSession::from_request_parts(parts, state)
        .await
        .map_err(|(_, e)| HttpError::internal(format!("no session layer: {e}")))
}

/// The signed-in user of a session cookie only: an API key is refused
/// (403), so a leaked key cannot change the account or make more keys.
pub struct SessionUser(pub User);

impl FromRequestParts<AppState> for SessionUser {
    type Rejection = HttpError;

    async fn from_request_parts(
        parts: &mut axum::http::request::Parts,
        state: &AppState,
    ) -> Result<Self, Self::Rejection> {
        match AuthUser::from_request_parts(parts, state).await? {
            AuthUser(user, Access::Session) => Ok(SessionUser(user)),
            AuthUser(_, Access::Key(_)) => Err(HttpError::new(
                StatusCode::FORBIDDEN,
                "Not with an API key: sign in",
            )),
        }
    }
}

/// The token of an `Authorization: Bearer` header.
pub(crate) fn bearer(headers: &HeaderMap) -> Option<&str> {
    let value = headers.get("authorization")?.to_str().ok()?;
    let (scheme, token) = value.split_once(' ')?;
    scheme.eq_ignore_ascii_case("bearer").then(|| token.trim())
}

/// Records when a key was used, at most once a minute, so that a busy
/// script does not take the one writer for every request. A failure is
/// only logged: the request goes on.
async fn note_key_use(state: &AppState, key: &KeyUse) {
    let now = SystemTime::now();
    let stale = iso(now - Duration::from_secs(60));
    if key
        .last_used_at
        .as_deref()
        .is_some_and(|last| last > stale.as_str())
    {
        return;
    }
    if let Err(e) = sqlx::query("UPDATE api_keys SET last_used_at = ?1 WHERE id = ?2")
        .bind(iso(now))
        .bind(key.id)
        .execute(state.db.writer())
        .await
    {
        tracing::warn!("cannot record the use of API key {}: {e}", key.id);
    }
}

/// A superadmin, as the extractor of their endpoints: signed in with a
/// session (an API key, a device token or an app password cannot confirm a
/// second factor, so none is taken: 403), who confirmed their passkey or a
/// two-factor code within [`session::STEP_UP_WINDOW`] (docs/M7A_DESIGN.md
/// §4). The refusals the web acts on carry a `code`:
/// [`HttpError::step_up_required`] (it asks, then retries) and
/// [`HttpError::second_factor_required`] (no passkey nor app to ask for).
pub struct Superadmin(pub User);

impl FromRequestParts<AppState> for Superadmin {
    type Rejection = HttpError;

    async fn from_request_parts(
        parts: &mut axum::http::request::Parts,
        state: &AppState,
    ) -> Result<Self, Self::Rejection> {
        let user = match AuthUser::from_request_parts(parts, state).await? {
            AuthUser(user, Access::Session) => user,
            AuthUser(_, Access::Key(_)) => {
                return Err(HttpError::new(
                    StatusCode::FORBIDDEN,
                    "Not with a token: the superadmin's endpoints need a signed-in session",
                ));
            }
        };
        if !user.is_superadmin {
            return Err(HttpError::forbidden());
        }
        let auth = auth_session(parts, state).await?;
        if session::stepped_up_until(&auth.session, user.id)
            .await?
            .is_some()
        {
            return Ok(Superadmin(user));
        }
        let methods = second_factors(state, &user).await?;
        Err(if methods.totp || methods.passkey {
            HttpError::step_up_required()
        } else {
            HttpError::second_factor_required()
        })
    }
}

/// The second factors a user can confirm here.
pub(crate) struct SecondFactors {
    /// A code of their authenticator app.
    pub totp: bool,
    /// One of their passkeys (they have one, and passkeys are on here).
    pub passkey: bool,
}

pub(crate) async fn second_factors(
    state: &AppState,
    user: &User,
) -> Result<SecondFactors, HttpError> {
    let passkey = crate::routes::passkeys::passkeys_on(state)
        && sqlx::query_scalar::<_, bool>(
            "SELECT EXISTS (SELECT 1 FROM passkeys WHERE user_id = ?1)",
        )
        .bind(user.id)
        .fetch_one(state.db.reader())
        .await?;
    Ok(SecondFactors {
        totp: user.totp_enabled,
        passkey,
    })
}

pub(crate) fn validate_email(raw: &str) -> Result<String, HttpError> {
    let email = raw.trim().to_lowercase();
    let valid = email.len() <= 254
        && !email.contains(char::is_whitespace)
        && email.split('@').collect::<Vec<_>>().len() == 2
        && email
            .split_once('@')
            .map(|(local, domain)| {
                !local.is_empty()
                    && domain.contains('.')
                    && !domain.starts_with('.')
                    && !domain.ends_with('.')
            })
            .unwrap_or(false);
    if valid {
        Ok(email)
    } else {
        Err(HttpError::bad_request("Expected an email address"))
    }
}

pub(crate) fn validate_password(raw: &str) -> Result<String, HttpError> {
    if raw.len() >= 10 && raw.len() <= 200 {
        Ok(raw.to_string())
    } else {
        Err(HttpError::bad_request(
            "Expected a password of 10 to 200 characters",
        ))
    }
}

/// The session and first-run state; the frontend routes every page by it.
#[derive(Serialize, ToSchema)]
pub struct Session {
    /// True while there are no users at all: the next step is `/setup`.
    #[serde(rename = "needsSetup")]
    pub needs_setup: bool,
    /// The signed-in user: null when nobody is signed in. Always present.
    #[schema(required = true)]
    pub user: Option<User>,
    /// Single sign-on, when it is on: null when it is off. Always present.
    #[schema(required = true)]
    pub oidc: Option<OidcLogin>,
    /// False when single sign-on is the only way in (`OIDC_ONLY`): the
    /// login page offers no password, and passwords cannot be set.
    pub password_login: bool,
    /// Whether the signed-in user must have two-factor authentication on
    /// (`TOTP_REQUIRED`, or the `sign_in.two_factor_required` setting of
    /// the superadmin or one of their teams): it cannot be turned off.
    /// Signed out: `TOTP_REQUIRED`.
    pub totp_required: bool,
    /// Whether passkeys can be used here (`PUBLIC_URL` set, and not
    /// single sign-on only): the login page offers them.
    pub passkeys: bool,
    /// Whether anyone may create an account (open sign-up): the login page
    /// offers "Create an account". With `OIDC_ONLY`, through single
    /// sign-on.
    pub signup: bool,
    /// Where the session's user stands for the superadmin's endpoints:
    /// null when nobody is signed in with a session (a token never steps
    /// up). Always present.
    #[schema(required = true)]
    pub step_up: Option<StepUpState>,
}

/// The step-up of a session (docs/M7A_DESIGN.md §4): the superadmin's
/// endpoints answer while `until` is in the future; otherwise the web asks
/// for one of the methods first (neither: the user adds one).
#[derive(Serialize, ToSchema)]
pub struct StepUpState {
    /// Until when the last confirmation holds (15 minutes after it); null
    /// when there is none, or it is over. Always present.
    #[schema(required = true)]
    pub until: Option<String>,
    /// The user can confirm with a code of their authenticator app
    /// (`/auth/step-up/totp`).
    pub totp: bool,
    /// The user can confirm with one of their passkeys (they have one, and
    /// passkeys are on here: `/auth/step-up/passkey/start`).
    pub passkey: bool,
}

/// How the login page offers single sign-on.
#[derive(Serialize, ToSchema)]
pub struct OidcLogin {
    /// What the button says, e.g. "Single sign-on".
    pub label: String,
}

/// A new email address for the signed-in user.
#[derive(Deserialize, ToSchema)]
pub struct ChangeEmail {
    pub email: String,
}

#[derive(Deserialize, ToSchema)]
pub struct ChangePassword {
    /// Required when the user has a password; left out to set a first one.
    #[serde(default)]
    pub current_password: Option<String>,
    /// 10 to 200 characters.
    pub new_password: String,
}

async fn has_users(state: &AppState) -> Result<bool, HttpError> {
    let count: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM users")
        .fetch_one(state.db.reader())
        .await?;
    Ok(count > 0)
}

// Session + first-run state. The frontend uses this to route every page.
#[utoipa::path(
    get,
    path = "/session",
    tag = "Auth",
    operation_id = "getSession",
    summary = "The session and the first-run state",
    description = "Whether the first superadmin still has to be created, and who is signed in (null when nobody is): by the session cookie, or by the API key or device token sent as `Authorization: Bearer`.",
    responses(
        (status = 200, description = "The session", body = Session),
    ),
)]
async fn session(
    State(state): State<AppState>,
    auth: AuthSession,
    headers: HeaderMap,
) -> Result<Json<Session>, HttpError> {
    let needs_setup = !has_users(&state).await?;
    // The apps ask with their token: its user (not an app password's,
    // which is WebDAV's only).
    let mut step_up = None;
    let user = match (auth.user, bearer(&headers)) {
        _ if needs_setup => None,
        (Some(user), _) => {
            let methods = second_factors(&state, &user).await?;
            step_up = Some(StepUpState {
                until: session::stepped_up_until(&auth.session, user.id)
                    .await?
                    .map(iso_of),
                totp: methods.totp,
                passkey: methods.passkey,
            });
            Some(user)
        }
        (None, Some(token)) => key_user(state.db.reader(), token)
            .await
            .filter(|(_, key)| key.kind != KeyKind::AppPassword)
            .map(|(user, _)| user),
        (None, None) => None,
    };
    let settings = state.oidc.settings();
    let oidc = settings.enabled().then(|| OidcLogin {
        label: settings.label().to_string(),
    });
    let totp_required = match &user {
        Some(user) => crate::routes::totp::two_factor_required(&state, user.id).await?,
        None => state.totp_required,
    };
    Ok(Json(Session {
        needs_setup,
        user,
        step_up,
        oidc,
        password_login: state.oidc.passwords_allowed(),
        totp_required,
        passkeys: crate::routes::passkeys::passkeys_on(&state),
        signup: !needs_setup && crate::users::signup_offered(&state).await?,
    }))
}

/// A time as the API gives times (`YYYY-MM-DDTHH:MM:SS.mmmZ`).
pub(crate) fn iso_of(t: time::OffsetDateTime) -> String {
    iso(SystemTime::from(t))
}

/// An email and a password, to sign in or to create the first superadmin.
#[derive(Deserialize, ToSchema)]
pub struct Credentials {
    pub email: String,
    /// 10 to 200 characters when creating an account.
    pub password: String,
}

/// The first superadmin, and where their files are.
#[derive(Deserialize, ToSchema)]
pub struct Setup {
    pub email: String,
    /// 10 to 200 characters.
    pub password: String,
    /// The folder of the files, an absolute path on the server: an existing
    /// folder Organizer can write to, such as a share that already holds
    /// files. Left out: the default, under `FILES_DIR`.
    #[serde(default)]
    pub files_root: Option<String>,
}

// Creates the superadmin. Only possible while there are no users at all.
#[utoipa::path(
    post,
    path = "/setup",
    tag = "Auth",
    operation_id = "setup",
    summary = "Create the first superadmin",
    description = "Only possible while there are no users. Signs the new superadmin in: the session cookie is set. The files folder is indexed in the background.",
    request_body(content = Setup, content_type = "application/json"),
    responses(
        (status = 201, description = "The superadmin was created and signed in"),
        (status = 400, description = "The request is invalid", body = ErrorBody),
        (status = 409, description = "Conflict", body = ErrorBody),
        (status = 415, description = "The body must be JSON (Content-Type: application/json)", body = ErrorBody),
    ),
)]
async fn setup(
    State(state): State<AppState>,
    mut auth: AuthSession,
    ApiJson(body): ApiJson<Setup>,
) -> Result<Response, HttpError> {
    let email = validate_email(&body.email)?;
    let password = validate_password(&body.password)?;
    let files_root = match body.files_root.as_deref().map(str::trim) {
        None | Some("") => None,
        Some(root) => Some(state.files.check_root(std::path::Path::new(root))?),
    };
    let id = Uuid::now_v7();
    let inserted = sqlx::query(
        "INSERT INTO users (id, email, password_hash, is_superadmin, files_root)
         SELECT ?3, ?1, ?2, 1, ?4 WHERE NOT EXISTS (SELECT 1 FROM users)",
    )
    .bind(email)
    .bind(hash_password(&password))
    .bind(id)
    .bind(files_root.map(|r| r.to_string_lossy().into_owned()))
    .execute(state.db.writer())
    .await?;
    if inserted.rows_affected() == 0 {
        return Err(HttpError::new(StatusCode::CONFLICT, "Already set up"));
    }
    let user = auth
        .backend
        .get_user(&id)
        .await?
        .ok_or_else(|| HttpError::internal("the new superadmin is gone"))?;
    session::sign_in(&mut auth, &user).await?;
    // Index the tree in the background: a large one takes a while.
    let files = state.files.clone();
    tokio::spawn(async move { files.open_and_scan(id).await });
    Ok(StatusCode::CREATED.into_response())
}

#[utoipa::path(
    post,
    path = "/login",
    tag = "Auth",
    operation_id = "login",
    summary = "Sign in",
    description = "Sets the session cookie. For a user with two-factor authentication on it answers `totp_required` instead, with a cookie good for five minutes: `/login/totp` with a code finishes the sign-in. With `TOTP_REQUIRED`, a user without it gets `totp_setup_required`: `/login/totp/setup` and `/login/totp/enable` set the app up and finish the sign-in. Every attempt counts: 5 per email, then 1 a minute, and 30 per address, then 1 every 6 seconds; over that, 429 with a Retry-After header.",
    request_body(content = Credentials, content_type = "application/json"),
    responses(
        (status = 200, description = "The password is right; two-factor authentication is next", body = LoginStep),
        (status = 204, description = "Signed in"),
        (status = 401, description = "Not signed in", body = ErrorBody),
        (status = 403, description = "Password sign-in is turned off (`OIDC_ONLY`)", body = ErrorBody),
        (status = 429, description = "Too many attempts, try again later", body = ErrorBody),
    ),
)]
async fn login(
    State(state): State<AppState>,
    mut auth: AuthSession,
    ConnectInfo(addr): ConnectInfo<SocketAddr>,
    headers: HeaderMap,
    body: Result<ApiJson<Credentials>, HttpError>,
) -> Result<Response, HttpError> {
    if !state.oidc.passwords_allowed() {
        return Err(HttpError::passwords_off());
    }
    // A malformed body fails the same way as a wrong password.
    let Ok(ApiJson(creds)) = body else {
        return Err(HttpError::new(
            StatusCode::UNAUTHORIZED,
            "Invalid email or password",
        ));
    };
    let email = validate_email(&creds.email)
        .map_err(|_| HttpError::new(StatusCode::UNAUTHORIZED, "Invalid email or password"))?;

    // Every attempt counts, per address and per email (see `limits.rs`).
    let ip = client_ip(&headers, Some(addr.ip()), state.trust_proxy);
    state.limits.sign_in(&ip, &email)?;

    // The same work and the same answer whether or not the user exists,
    // or has a password (see `session::Backend::authenticate`).
    let user = auth
        .authenticate(session::Credentials {
            email,
            password: creds.password,
        })
        .await
        .map_err(anyhow::Error::new)?;
    let Some(user) = user else {
        return Err(HttpError::new(
            StatusCode::UNAUTHORIZED,
            "Invalid email or password",
        ));
    };
    if user.disabled {
        return Err(HttpError::account_disabled());
    }
    if let Some(step) = next_step(&state, user.id).await? {
        return Ok((
            challenge_cookie(&state, user.id, step),
            Json(LoginStep {
                totp_required: step == Step::Code,
                totp_setup_required: step == Step::Enroll,
            }),
        )
            .into_response());
    }
    session::sign_in(&mut auth, &user).await?;
    Ok(StatusCode::NO_CONTENT.into_response())
}

/// The password was right, and two-factor authentication is the next step.
/// Exactly one of the two is true.
#[derive(Serialize, ToSchema)]
pub struct LoginStep {
    /// A code from the authenticator app is next (`/login/totp`).
    pub totp_required: bool,
    /// Setting the app up is next (`TOTP_REQUIRED`): `/login/totp/setup`,
    /// then `/login/totp/enable`.
    pub totp_setup_required: bool,
}

#[utoipa::path(
    post,
    path = "/logout",
    tag = "Auth",
    operation_id = "logout",
    summary = "Sign out",
    responses(
        (status = 204, description = "Signed out; the session cookie is cleared"),
    ),
)]
async fn logout(mut auth: AuthSession) -> Result<Response, HttpError> {
    auth.logout().await.map_err(anyhow::Error::new)?;
    Ok(StatusCode::NO_CONTENT.into_response())
}

// Change the signed-in user's own email.
#[utoipa::path(
    post,
    path = "/email",
    tag = "Account",
    operation_id = "changeEmail",
    summary = "Change your own email",
    request_body(content = ChangeEmail, content_type = "application/json"),
    responses(
        (status = 204, description = "Changed"),
        (status = 400, description = "The request is invalid", body = ErrorBody),
        (status = 401, description = "Not signed in", body = ErrorBody),
        (status = 409, description = "Conflict", body = ErrorBody),
        (status = 415, description = "The body must be JSON (Content-Type: application/json)", body = ErrorBody),
    ),
    security(("session" = [])),
)]
async fn change_email(
    State(state): State<AppState>,
    user: SessionUser,
    ApiJson(body): ApiJson<ChangeEmail>,
) -> Result<Response, HttpError> {
    let email = validate_email(&body.email)?;
    let mut tx = state.db.writer().begin().await?;
    let taken: Option<i64> =
        sqlx::query_scalar("SELECT 1 FROM users WHERE email = ?1 AND id != ?2")
            .bind(&email)
            .bind(user.0.id)
            .fetch_optional(&mut *tx)
            .await?;
    if taken.is_some() {
        return Err(HttpError::new(StatusCode::CONFLICT, "Email already in use"));
    }
    sqlx::query("UPDATE users SET email = ?1 WHERE id = ?2")
        .bind(&email)
        .bind(user.0.id)
        .execute(&mut *tx)
        .await?;
    tx.commit().await?;
    Ok(StatusCode::NO_CONTENT.into_response())
}

// Change the signed-in user's own password, or set a first one.
#[utoipa::path(
    post,
    path = "/password",
    tag = "Account",
    operation_id = "changePassword",
    summary = "Change your own password",
    description = "The current password is required, except for a user who has none (single sign-on): they set a first one. The user's other sessions are signed out. Refused while password sign-in is turned off (`OIDC_ONLY`).",
    request_body(content = ChangePassword, content_type = "application/json"),
    responses(
        (status = 204, description = "Changed"),
        (status = 400, description = "The request is invalid", body = ErrorBody),
        (status = 401, description = "Not signed in", body = ErrorBody),
        (status = 403, description = "Password sign-in is turned off (`OIDC_ONLY`)", body = ErrorBody),
        (status = 415, description = "The body must be JSON (Content-Type: application/json)", body = ErrorBody),
    ),
    security(("session" = [])),
)]
async fn change_password(
    State(state): State<AppState>,
    user: SessionUser,
    mut auth: AuthSession,
    ApiJson(body): ApiJson<ChangePassword>,
) -> Result<Response, HttpError> {
    if !state.oidc.passwords_allowed() {
        return Err(HttpError::passwords_off());
    }
    // The current password is a guess like any other (see `limits.rs`).
    state.limits.user(user.0.id, None)?;
    let invalid = || HttpError::new(StatusCode::UNAUTHORIZED, "Invalid password");
    let current = body.current_password.unwrap_or_default();
    if current.len() > 200 {
        return Err(HttpError::bad_request("Invalid password"));
    }
    let new_password = validate_password(&body.new_password)?;
    // The hashing below takes a while: no connection is held meanwhile.
    let stored: Option<Option<String>> =
        sqlx::query_scalar("SELECT password_hash FROM users WHERE id = ?1")
            .bind(user.0.id)
            .fetch_optional(state.db.reader())
            .await?;
    let Some(stored) = stored else {
        return Err(invalid());
    };
    if let Some(stored) = &stored
        && !verify_password(&current, stored)
    {
        return Err(invalid());
    }
    // Only if the password is still the one checked (or still none): a
    // change in between fails like a wrong password.
    let updated =
        sqlx::query("UPDATE users SET password_hash = ?1 WHERE id = ?2 AND password_hash IS ?3")
            .bind(hash_password(&new_password))
            .bind(user.0.id)
            .bind(stored)
            .execute(state.db.writer())
            .await?
            .rows_affected();
    if updated == 0 {
        return Err(invalid());
    }
    // The user's other sessions are signed out; this one stays.
    session::refresh(&mut auth, user.0.id).await?;
    Ok(StatusCode::NO_CONTENT.into_response())
}
