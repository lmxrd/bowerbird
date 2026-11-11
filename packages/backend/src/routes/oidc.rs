//! Single sign-on: the login and the provider's callback, and the superadmin
//! settings of the provider. The protocol is in `oidc.rs`; who may sign in
//! is decided here: a user linked to the provider account, else the user
//! with the provider's verified email (who is linked from then on, unless
//! they are linked to another account at the provider already), else a new
//! user without a password: with the invite the login was started from, or
//! when the superadmin lets single sign-on make accounts (`users.rs`). A
//! disabled user is refused. Nobody signs in this way before the first
//! superadmin exists.

use crate::error::{ErrorBody, HttpError};
use crate::extract::ApiJson;
use crate::oidc::{CALLBACK_PATH, Failure, Identity, LOGIN_COOKIE, LOGIN_TTL, Settings};
use crate::routes::auth::{Superadmin, validate_email};
use crate::routes::device::{allowed_next, percent_encode};
use crate::routes::totp::{Step, challenge_cookie, next_step};
use crate::session::{self, AuthSession};
use crate::state::AppState;
use axum::Json;
use axum::extract::{Query, State};
use axum::http::HeaderMap;
use axum::response::{IntoResponse, Redirect, Response};
use axum_extra::extract::cookie::{Cookie, SameSite};
use axum_extra::extract::{CookieJar, SignedCookieJar};
use axum_login::AuthnBackend as _;
use serde::{Deserialize, Serialize};
use utoipa::{IntoParams, ToSchema};
use utoipa_axum::router::OpenApiRouter;
use utoipa_axum::routes;
use uuid::Uuid;

/// The login and the callback, nested under /auth.
pub fn router() -> OpenApiRouter<AppState> {
    OpenApiRouter::new()
        .routes(routes!(login))
        .routes(routes!(callback))
}

/// The provider settings, nested under /superadmin.
pub fn superadmin_router() -> OpenApiRouter<AppState> {
    OpenApiRouter::new().routes(routes!(get_settings, update_settings))
}

/// The login cookie's name and path. Over https it is a `__Host-` cookie,
/// which a sibling subdomain cannot set: one that could would plant its own
/// login and sign the browser in as someone else. That prefix needs the
/// path `/`; over plain http the cookie goes to the callback only.
fn login_cookie_name(secure: bool) -> (&'static str, &'static str) {
    if secure {
        ("__Host-organizer_oidc", "/")
    } else {
        (LOGIN_COOKIE, CALLBACK_PATH)
    }
}

/// The login cookie. Lax, not Strict: the callback is a navigation that
/// comes from the provider's site.
fn login_cookie(value: String, secure: bool) -> Cookie<'static> {
    let (name, path) = login_cookie_name(secure);
    Cookie::build((name, value))
        .path(path)
        .http_only(true)
        .same_site(SameSite::Lax)
        .secure(secure)
        .max_age(time::Duration::seconds(LOGIN_TTL.as_secs() as i64))
        .build()
}

/// Back to the login page, which says what went wrong.
fn failed(failure: Failure) -> Redirect {
    Redirect::to(&format!("/login?error={}", failure.code()))
}

/// Where to go after signing in.
#[derive(Deserialize, IntoParams)]
#[into_params(parameter_in = Query)]
pub struct LoginParams {
    /// The page of this server to go on to once signed in (a path, such as
    /// `/settings/contacts?token=…` or an app's device approval page); any
    /// other value (another origin) is ignored.
    pub next: Option<String>,
    /// The token of an invite (`/invite/<token>`): the account is made
    /// with it, if the provider's verified email has none.
    pub invite: Option<String>,
}

#[utoipa::path(
    get,
    path = "/oidc/login",
    tag = "Auth",
    operation_id = "oidcLogin",
    summary = "Sign in with single sign-on",
    description = "Redirects the browser to the provider, or to `/login?error=unavailable` when single sign-on is off or the provider cannot be reached (it is not asked again for 30 seconds), or to `/login?error=misconfigured` when the callback URL is not on the scheme (http or https) of `PUBLIC_URL`. Sets a short-lived signed cookie that holds the login, so that only this browser can finish it.",
    params(LoginParams),
    responses(
        (status = 303, description = "To the provider, or back to the login page"),
    ),
)]
async fn login(
    State(state): State<AppState>,
    headers: HeaderMap,
    Query(params): Query<LoginParams>,
) -> Response {
    let secure = state.secure_cookies;
    let redirect_url = state
        .oidc
        .callback_url(&headers, state.public_url.as_deref());
    // The login cookie is `__Host-` and Secure when PUBLIC_URL is https,
    // and the callback must be on that same scheme, or it could never read
    // it: a callback on the other scheme is a setup mistake, refused before
    // the provider with a reason that points at it.
    if redirect_url.starts_with("https://") != secure {
        tracing::warn!(
            "single sign-on: PUBLIC_URL is {} but the callback URL is {redirect_url}; \
             check PUBLIC_URL and OIDC_REDIRECT_URL",
            state.public_url.as_deref().unwrap_or("not set")
        );
        return failed(Failure::Misconfigured).into_response();
    }
    let next = params.next.filter(|n| allowed_next(n));
    let invite = params.invite.filter(|t| !t.is_empty() && t.len() <= 200);
    match state.oidc.start(redirect_url, next, invite).await {
        Ok((url, login)) => (
            SignedCookieJar::new(state.oidc.key().clone()).add(login_cookie(login, secure)),
            Redirect::to(&url),
        )
            .into_response(),
        Err(failure) => failed(failure).into_response(),
    }
}

/// What the provider sends back.
#[derive(Deserialize, IntoParams)]
#[into_params(parameter_in = Query)]
pub struct CallbackParams {
    /// The authorization code.
    code: Option<String>,
    /// The state of the login.
    state: Option<String>,
    /// Set when the provider refused, e.g. `access_denied`.
    error: Option<String>,
}

#[utoipa::path(
    get,
    path = "/oidc/callback",
    tag = "Auth",
    operation_id = "oidcCallback",
    summary = "The provider's callback",
    description = "Where the provider sends the browser back. Signs the user in (the session cookie is set) and redirects to `/` (or to the `next` the login was started with, a path of this server); or, for two-factor authentication first, sets the short-lived challenge cookie and redirects to `/login?step=totp` (a code; with that `next`) or `/login?step=totp-setup` (setting the app up, with `TOTP_REQUIRED`); or redirects to `/login?error=<code>`: `unavailable` (also before the first superadmin is set up), `state`, `provider`, `token`, `email`, `unknown_user`, `linked` (the user with that email is linked to another account at the provider), `disabled` (a superadmin disabled the account) or `invite` (the invite is used, expired or for another email).",
    params(CallbackParams),
    responses(
        (status = 303, description = "Signed in, to `/`; or back to the login page"),
        (status = 500, description = "The database failed", body = ErrorBody),
    ),
)]
async fn callback(
    State(state): State<AppState>,
    jar: CookieJar,
    mut auth: AuthSession,
    headers: HeaderMap,
    Query(params): Query<CallbackParams>,
) -> Response {
    // The login cookie, if this server signed it. Any cookie of that name
    // is used up whatever happens next, errors included, and one this
    // server did not sign (from before a restart, say) too.
    let secure = state.secure_cookies;
    let (name, _) = login_cookie_name(secure);
    let login = SignedCookieJar::from_headers(&headers, state.oidc.key().clone())
        .get(name)
        .map(|c| c.value().to_string());
    let logins = if jar.get(name).is_some() {
        // With the attributes it was set with: a browser ignores a
        // `__Host-` cookie without Secure, the removal included.
        let mut removal = login_cookie(String::new(), secure);
        removal.make_removal();
        CookieJar::new().add(removal)
    } else {
        CookieJar::new()
    };
    match sign_in(&state, &mut auth, &headers, params, login).await {
        // Only to `/`, the login page, or a page of this server (`next`).
        Ok(Ok(SignedIn::Session(next))) => {
            let to = next
                .filter(|n| allowed_next(n))
                .unwrap_or_else(|| "/".into());
            (logins, Redirect::to(&to)).into_response()
        }
        Ok(Ok(SignedIn::Step(challenge, step, next))) => {
            let to = match next.filter(|n| allowed_next(n)) {
                Some(next) => format!("{}&next={}", step.page(), percent_encode(&next)),
                None => step.page().to_string(),
            };
            (logins, challenge, Redirect::to(&to)).into_response()
        }
        Ok(Err(failure)) => (logins, failed(failure)).into_response(),
        Err(error) => (logins, error).into_response(),
    }
}

/// The callback's work: the session cookie of the user signed in, or why
/// nobody is.
async fn sign_in(
    state: &AppState,
    auth: &mut AuthSession,
    _headers: &HeaderMap,
    params: CallbackParams,
    login: Option<String>,
) -> Result<Result<SignedIn, Failure>, HttpError> {
    let Some(login_state) = params.state else {
        return Ok(Err(Failure::State));
    };
    if params.error.is_some() {
        return Ok(Err(Failure::Provider));
    }
    let identity = match state
        .oidc
        .finish(&login_state, login.as_deref(), params.code)
        .await
    {
        Ok(identity) => identity,
        Err(failure) => return Ok(Err(failure)),
    };

    // Read before the transaction: one writer connection.
    let create_users = crate::users::oidc_creates_users(state).await?;
    let mut tx = state.db.writer().begin().await?;
    let (user_id, created) = match user_for(&mut tx, &identity, create_users).await? {
        Ok(found) => found,
        Err(failure) => return Ok(Err(failure)),
    };
    // Two-factor authentication is asked for after single sign-on too: the
    // login page takes the step, with the challenge cookie.
    tx.commit().await?;
    let step = next_step(state, user_id).await?;
    if created {
        // A new user's tree is made and indexed now that they exist.
        let files = state.files.clone();
        tokio::spawn(async move { files.open_and_scan(user_id).await });
    }
    if let Some(step) = step {
        return Ok(Ok(SignedIn::Step(
            challenge_cookie(state, user_id, step),
            step,
            identity.next,
        )));
    }
    // Linked and committed: the session is the store's write, after
    // the transaction (one writer connection).
    let user = auth
        .backend
        .get_user(&user_id)
        .await?
        .ok_or_else(|| HttpError::internal("the signed-in user is gone"))?;
    session::sign_in(auth, &user).await?;
    Ok(Ok(SignedIn::Session(identity.next)))
}

/// What a callback that proved who the user is leads to.
enum SignedIn {
    /// Signed in: the session cookie is set. Then to the page the login
    /// was for, if any.
    Session(Option<String>),
    /// Two-factor authentication first, on the login page.
    Step(SignedCookieJar, Step, Option<String>),
}

/// The user a provider account signs in as (and whether they were made
/// now), linking them on the first sign-in. Without a user of the verified
/// email, one is made with the invite the login came from, or when
/// `create_users` (the superadmin's switch) says so.
async fn user_for(
    tx: &mut sqlx::SqliteConnection,
    identity: &Identity,
    create_users: bool,
) -> sqlx::Result<Result<(Uuid, bool), Failure>> {
    // Nobody before the first superadmin: a user made now would take /setup
    // away and leave the instance without one.
    let set_up: bool = sqlx::query_scalar("SELECT EXISTS (SELECT 1 FROM users)")
        .fetch_one(&mut *tx)
        .await?;
    if !set_up {
        return Ok(Err(Failure::Unavailable));
    }
    let linked: Option<Uuid> = sqlx::query_scalar(
        "SELECT user_id FROM user_identities WHERE issuer = ?1 AND subject = ?2",
    )
    .bind(&identity.issuer)
    .bind(&identity.subject)
    .fetch_optional(&mut *tx)
    .await?;
    if let Some(id) = linked {
        if disabled(tx, id).await? {
            return Ok(Err(Failure::Disabled));
        }
        return Ok(Ok((id, false)));
    }

    // Not linked yet: only a verified email may find the user.
    let Some(email) = identity
        .verified_email
        .as_deref()
        .and_then(|email| validate_email(email).ok())
    else {
        return Ok(Err(Failure::Email));
    };
    let existing: Option<Uuid> = sqlx::query_scalar("SELECT id FROM users WHERE email = ?1")
        .bind(&email)
        .fetch_optional(&mut *tx)
        .await?;
    let (id, created) = match existing {
        // Linked to another account at this provider: a second account
        // with the same email (a recycled address, say) is not the same
        // person, so it does not get in.
        Some(id) if linked_to(tx, id, &identity.issuer).await? => {
            tracing::warn!(
                "single sign-on: user {id} is linked to another account at the provider"
            );
            return Ok(Err(Failure::Linked));
        }
        Some(id) if disabled(tx, id).await? => return Ok(Err(Failure::Disabled)),
        Some(id) => (id, false),
        // Nobody has that email: an account is made with the invite, or
        // when the superadmin lets single sign-on make them.
        None => {
            let maker = match &identity.invite {
                Some(token) => crate::users::invite_maker(&mut *tx, token).await?,
                None => None,
            };
            let is_superadmin = match &identity.invite {
                Some(token) => match crate::users::take_invite(tx, token, &email).await? {
                    Some(is_superadmin) => is_superadmin,
                    None => return Ok(Err(Failure::Invite)),
                },
                None if create_users => false,
                None => return Ok(Err(Failure::UnknownUser)),
            };
            let Some(id) = crate::users::create(tx, &email, None, is_superadmin).await? else {
                return Ok(Err(Failure::UnknownUser));
            };
            crate::users::note_invited(tx, id, maker).await?;
            tracing::info!("single sign-on made the account of user {id}");
            (id, true)
        }
    };
    sqlx::query("INSERT INTO user_identities (issuer, subject, user_id) VALUES (?1, ?2, ?3)")
        .bind(&identity.issuer)
        .bind(&identity.subject)
        .bind(id)
        .execute(&mut *tx)
        .await?;
    Ok(Ok((id, created)))
}

/// Whether a superadmin disabled a user.
async fn disabled(tx: &mut sqlx::SqliteConnection, user: Uuid) -> sqlx::Result<bool> {
    sqlx::query_scalar("SELECT disabled_at IS NOT NULL FROM users WHERE id = ?1")
        .bind(user)
        .fetch_one(&mut *tx)
        .await
}

/// Whether a user is linked to an account at this provider.
async fn linked_to(
    tx: &mut sqlx::SqliteConnection,
    user: Uuid,
    issuer: &str,
) -> sqlx::Result<bool> {
    sqlx::query_scalar(
        "SELECT EXISTS (SELECT 1 FROM user_identities WHERE user_id = ?1 AND issuer = ?2)",
    )
    .bind(user)
    .bind(issuer)
    .fetch_one(&mut *tx)
    .await
}

/// The single sign-on settings in effect. The client secret is never
/// sent back.
#[derive(Serialize, ToSchema)]
pub struct OidcSettings {
    /// Whether single sign-on is on: an issuer and a client id are set.
    pub enabled: bool,
    /// The provider's issuer URL, e.g. `https://id.example.com/realms/main`.
    pub issuer: String,
    pub client_id: String,
    /// Whether a client secret is set.
    pub client_secret_set: bool,
    /// Space separated; empty is `openid email`.
    pub scopes: String,
    /// What the login button says; empty is "Single sign-on".
    pub label: String,
    /// The callback URL, when one is set; empty is the one the request
    /// came to.
    pub redirect_url: String,
    /// The callback URL to register with the provider.
    pub callback_url: String,
    /// The fields set by an environment variable (`issuer` by `OIDC_ISSUER`,
    /// and so on), which cannot be changed here.
    pub locked: Vec<String>,
}

/// The settings to change; a field that is left out stays as it is.
#[derive(Deserialize, ToSchema)]
pub struct UpdateOidcSettings {
    pub issuer: Option<String>,
    pub client_id: Option<String>,
    /// An empty string removes the secret. A new issuer removes it too,
    /// unless one is sent with it: the secret is for one provider only.
    pub client_secret: Option<String>,
    pub scopes: Option<String>,
    pub label: Option<String>,
    pub redirect_url: Option<String>,
}

fn view(state: &AppState, headers: &HeaderMap) -> OidcSettings {
    let settings = state.oidc.settings();
    OidcSettings {
        enabled: settings.enabled(),
        client_secret_set: !settings.client_secret.is_empty(),
        callback_url: state
            .oidc
            .callback_url(headers, state.public_url.as_deref()),
        locked: state
            .oidc
            .env()
            .locked()
            .into_iter()
            .map(String::from)
            .collect(),
        issuer: settings.issuer,
        client_id: settings.client_id,
        scopes: settings.scopes,
        label: settings.label,
        redirect_url: settings.redirect_url,
    }
}

#[utoipa::path(
    get,
    path = "/oidc",
    tag = "Superadmin",
    operation_id = "getOidcSettings",
    summary = "The single sign-on settings",
    responses(
        (status = 200, description = "The settings in effect", body = OidcSettings),
        (status = 401, description = "Not signed in", body = ErrorBody),
        (status = 403, description = "Superadmin only, with a session (never a token) whose second factor was confirmed within 15 minutes: else `code` is `step_up_required` (confirm it, `/auth/step-up/…`) or `second_factor_required` (add one first)", body = ErrorBody),
    ),
    security(("session" = [])),
)]
async fn get_settings(
    State(state): State<AppState>,
    _: Superadmin,
    headers: HeaderMap,
) -> Result<Json<OidcSettings>, HttpError> {
    Ok(Json(view(&state, &headers)))
}

#[utoipa::path(
    put,
    path = "/oidc",
    tag = "Superadmin",
    operation_id = "updateOidcSettings",
    summary = "Change the single sign-on settings",
    description = "Takes effect at once. With an issuer and a client id set, the provider must answer its discovery document, or nothing changes (400). A field set by an environment variable cannot be changed (400). A new issuer removes the client secret unless one is sent with it, and is refused (400) while the secret comes from `OIDC_CLIENT_SECRET` and the issuer does not.",
    request_body(content = UpdateOidcSettings, content_type = "application/json"),
    responses(
        (status = 200, description = "The settings now in effect", body = OidcSettings),
        (status = 400, description = "The request is invalid, or the provider cannot be used", body = ErrorBody),
        (status = 401, description = "Not signed in", body = ErrorBody),
        (status = 403, description = "Superadmin only, with a session (never a token) whose second factor was confirmed within 15 minutes: else `code` is `step_up_required` (confirm it, `/auth/step-up/…`) or `second_factor_required` (add one first)", body = ErrorBody),
        (status = 415, description = "The body must be JSON (Content-Type: application/json)", body = ErrorBody),
    ),
    security(("session" = [])),
)]
async fn update_settings(
    State(state): State<AppState>,
    _: Superadmin,
    headers: HeaderMap,
    ApiJson(body): ApiJson<UpdateOidcSettings>,
) -> Result<Json<OidcSettings>, HttpError> {
    // A task of its own, which runs to the end even if this request is
    // dropped half way (the client went away): the row is never stored
    // without being put in effect.
    tokio::spawn(save(state.clone(), body))
        .await
        .map_err(|e| HttpError::internal(format!("saving failed: {e}")))??;
    Ok(Json(view(&state, &headers)))
}

/// Store new settings and put them in effect, one save at a time.
async fn save(state: AppState, body: UpdateOidcSettings) -> Result<(), HttpError> {
    let _saving = state.oidc.lock_saves().await;
    let env = state.oidc.env();
    let locked = env.locked();
    let mut stored = crate::oidc::stored(&state.db).await?;
    let issuer_before = env.apply(&stored).issuer;
    let new_secret = body.client_secret.is_some();
    let fields = [
        ("issuer", body.issuer, &mut stored.issuer),
        ("client_id", body.client_id, &mut stored.client_id),
        (
            "client_secret",
            body.client_secret,
            &mut stored.client_secret,
        ),
        ("scopes", body.scopes, &mut stored.scopes),
        ("label", body.label, &mut stored.label),
        ("redirect_url", body.redirect_url, &mut stored.redirect_url),
    ];
    for (field, value, stored) in fields {
        let Some(value) = value else { continue };
        if locked.contains(&field) {
            return Err(locked_field(field));
        }
        *stored = value.trim().to_string();
    }
    // The secret goes to the provider's token endpoint: a new issuer, which
    // names its own, must not receive the old provider's secret.
    if env.apply(&stored).issuer != issuer_before {
        if env.client_secret.is_some() {
            return Err(HttpError::bad_request(
                "The client secret is set by OIDC_CLIENT_SECRET: set the issuer with OIDC_ISSUER too",
            ));
        }
        if !new_secret {
            stored.client_secret.clear();
        }
    }

    // The provider is asked before anything is stored, and no connection
    // is held while it answers.
    let settings: Settings = env.apply(&stored);
    state
        .oidc
        .check(&settings)
        .await
        .map_err(HttpError::bad_request)?;
    sqlx::query(
        "UPDATE oidc_settings SET issuer = ?1, client_id = ?2, client_secret = ?3, scopes = ?4,
           label = ?5, redirect_url = ?6
         WHERE id = 1",
    )
    .bind(&stored.issuer)
    .bind(&stored.client_id)
    .bind(&stored.client_secret)
    .bind(&stored.scopes)
    .bind(&stored.label)
    .bind(&stored.redirect_url)
    .execute(state.db.writer())
    .await?;
    state.oidc.replace(settings);
    Ok(())
}

fn locked_field(field: &str) -> HttpError {
    HttpError::bad_request(format!(
        "{field} is set by the OIDC_{} environment variable",
        field.to_uppercase()
    ))
}
