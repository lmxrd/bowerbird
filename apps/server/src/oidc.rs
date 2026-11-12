//! Single sign-on with an OpenID Connect provider: the authorization code
//! flow with PKCE and a nonce, through the `openidconnect` crate, which also
//! checks the ID token's signature against the provider's keys (JWKS).
//!
//! The settings are made in the superadmin settings (the `oidc` document of
//! `server_settings`) or
//! with OIDC_* environment variables; a variable that is set wins over its
//! field, which the superadmin settings then show as locked. They are kept here,
//! in memory, and replaced when a superadmin saves new ones: no restart.
//!
//! A login in progress (state, nonce, PKCE verifier, callback URL) is kept
//! in the browser that started it, in a cookie signed with a key made at
//! start: nothing is stored on the server, so there is nothing for a flood
//! of logins to fill, and a callback in another browser, or after ten
//! minutes, fails. A restart makes a new key, which only loses the logins in
//! progress. A replayed callback fails at the provider: a code is used once.
//! Who may sign in is decided in `routes/oidc.rs`.

use crate::db::Db;
use axum::http::HeaderMap;
use axum_extra::extract::cookie::Key;
use openidconnect::core::{
    CoreAuthenticationFlow, CoreClient, CoreIdTokenClaims, CoreProviderMetadata,
};
use openidconnect::url::Url;
use openidconnect::{
    AsyncHttpClient, AuthorizationCode, ClaimsVerificationError, ClientId, ClientSecret, CsrfToken,
    HttpRequest, HttpResponse, IssuerUrl, Nonce, PkceCodeChallenge, PkceCodeVerifier, RedirectUrl,
    Scope, SignatureVerificationError, TokenResponse,
};
use serde::{Deserialize, Serialize};
use std::pin::Pin;
use std::sync::{Mutex, RwLock};
use std::time::{Duration, Instant};

/// The signed cookie that holds a login in progress (`__Host-` prefixed
/// over https, see `routes/oidc.rs`).
pub const LOGIN_COOKIE: &str = "organizer_oidc";
/// The path of the callback, which the provider redirects back to.
pub const CALLBACK_PATH: &str = "/api/auth/oidc/callback";
/// How long a login in progress waits for the provider's callback.
pub const LOGIN_TTL: Duration = Duration::from_secs(10 * 60);
/// The provider's metadata and keys are fetched again after this long; a
/// token they cannot check fetches them at once (see REFETCH_EVERY).
const METADATA_TTL: Duration = Duration::from_secs(60 * 60);
/// After the provider failed to answer, logins fail at once for this long
/// instead of each asking it again.
const RETRY_AFTER: Duration = Duration::from_secs(30);
/// A token whose signature the cached keys cannot check fetches them again,
/// at most this often per issuer: a provider that signs wrongly does not
/// cost a fetch per login.
const REFETCH_EVERY: Duration = Duration::from_secs(60);
/// The most the provider may answer: its documents are a few kilobytes.
const MAX_RESPONSE: usize = 1024 * 1024;
const DEFAULT_LABEL: &str = "Single sign-on";

/// The settings, as stored and as in effect. An empty string is a field
/// that is not set.
#[derive(Clone, Default, PartialEq, Serialize, Deserialize, sqlx::FromRow)]
pub struct Settings {
    pub issuer: String,
    pub client_id: String,
    pub client_secret: String,
    /// Space separated; `openid` is always asked for.
    pub scopes: String,
    pub label: String,
    /// The callback URL registered with the provider. Empty: the one the
    /// request came to (see [`Oidc::callback_url`]).
    pub redirect_url: String,
}

impl Settings {
    pub fn enabled(&self) -> bool {
        !self.issuer.is_empty() && !self.client_id.is_empty()
    }

    /// What the login button says.
    pub fn label(&self) -> &str {
        if self.label.is_empty() {
            DEFAULT_LABEL
        } else {
            &self.label
        }
    }

    fn scopes(&self) -> Vec<String> {
        let scopes = if self.scopes.trim().is_empty() {
            "email"
        } else {
            &self.scopes
        };
        scopes
            .split_whitespace()
            // The crate always adds openid.
            .filter(|s| *s != "openid")
            .map(String::from)
            .collect()
    }
}

/// The OIDC_* environment variables. Each one that is set (and not empty)
/// overrides its field of the stored settings.
#[derive(Clone, Default)]
pub struct Env {
    pub issuer: Option<String>,
    pub client_id: Option<String>,
    pub client_secret: Option<String>,
    pub scopes: Option<String>,
    pub label: Option<String>,
    pub redirect_url: Option<String>,
    /// `OIDC_ONLY`: single sign-on is the only way in, no password sign-in.
    pub only: bool,
}

impl Env {
    /// The OIDC_* variables. A yes/no value that is not understood is an
    /// error, which stops the start rather than quietly meaning "off".
    pub fn from_env() -> Result<Self, String> {
        use crate::config::{flag, var};
        Ok(Env {
            issuer: var("OIDC_ISSUER"),
            client_id: var("OIDC_CLIENT_ID"),
            client_secret: var("OIDC_CLIENT_SECRET"),
            scopes: var("OIDC_SCOPES"),
            label: var("OIDC_LABEL"),
            redirect_url: var("OIDC_REDIRECT_URL"),
            only: flag("OIDC_ONLY")?.unwrap_or(false),
        })
    }

    /// The stored settings with the variables that are set on top.
    pub fn apply(&self, stored: &Settings) -> Settings {
        let pick = |env: &Option<String>, stored: &String| env.clone().unwrap_or(stored.clone());
        Settings {
            issuer: pick(&self.issuer, &stored.issuer),
            client_id: pick(&self.client_id, &stored.client_id),
            client_secret: pick(&self.client_secret, &stored.client_secret),
            scopes: pick(&self.scopes, &stored.scopes),
            label: pick(&self.label, &stored.label),
            redirect_url: pick(&self.redirect_url, &stored.redirect_url),
        }
    }

    /// The fields set by a variable (`issuer` is `OIDC_ISSUER`, and so on).
    pub fn locked(&self) -> Vec<&'static str> {
        [
            ("issuer", self.issuer.is_some()),
            ("client_id", self.client_id.is_some()),
            ("client_secret", self.client_secret.is_some()),
            ("scopes", self.scopes.is_some()),
            ("label", self.label.is_some()),
            ("redirect_url", self.redirect_url.is_some()),
        ]
        .into_iter()
        .filter(|(_, locked)| *locked)
        .map(|(field, _)| field)
        .collect()
    }
}

/// Why a sign-in failed, as the code the login page shows a message for.
/// Nothing more is said to the browser; the details are logged. The codes
/// are listed again in `apps/frontend/src (the login page)`: change both.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum Failure {
    /// Single sign-on is off, the provider cannot be reached, or there is
    /// no superadmin yet.
    Unavailable,
    /// The callback matches no login in progress of this browser: no
    /// cookie, a cookie not signed by this server, another state, or a
    /// login older than ten minutes.
    State,
    /// The provider refused, or the code exchange failed.
    Provider,
    /// The ID token is missing or invalid.
    Token,
    /// No verified email to find the user by.
    Email,
    /// No user has that email, and users are not created.
    UnknownUser,
    /// The user with that email is linked to another account at this
    /// provider.
    Linked,
    /// A superadmin disabled the account.
    Disabled,
    /// The invite the sign-in was for is used, expired, revoked or for
    /// another email.
    Invite,
    /// The callback URL is on another scheme than the login (http and
    /// https): the setup is wrong (see `routes/oidc.rs` `login`).
    Misconfigured,
}

impl Failure {
    pub fn code(self) -> &'static str {
        match self {
            Failure::Unavailable => "unavailable",
            Failure::State => "state",
            Failure::Provider => "provider",
            Failure::Token => "token",
            Failure::Email => "email",
            Failure::UnknownUser => "unknown_user",
            Failure::Linked => "linked",
            Failure::Disabled => "disabled",
            Failure::Invite => "invite",
            Failure::Misconfigured => "misconfigured",
        }
    }
}

/// Who the provider says signed in.
pub struct Identity {
    /// Where to go after signing in, from the login that started it.
    pub next: Option<String>,
    /// The token of the invite the login was started from, if any: a new
    /// account is made with it.
    pub invite: Option<String>,
    pub issuer: String,
    pub subject: String,
    /// The email, when the provider says it is verified.
    pub verified_email: Option<String>,
}

/// A login in progress, as the signed cookie holds it.
#[derive(Serialize, Deserialize)]
struct Pending {
    state: String,
    nonce: String,
    verifier: String,
    redirect_url: String,
    /// Where to go after signing in: the device approval page, for an app's
    /// sign-in (`routes/device.rs`); none for the web app's.
    #[serde(default)]
    next: Option<String>,
    /// The invite page's token, when the login was started there.
    #[serde(default)]
    invite: Option<String>,
    /// Seconds since this server started: the key the cookie is signed
    /// with is this process's too, and this clock never goes back.
    started: u64,
}

impl Pending {
    /// The login of a cookie's value, if it is the one of this callback's
    /// state and not too old. The signature was checked by the cookie jar.
    fn of(cookie: &str, state: &str, now: u64) -> Option<Pending> {
        let pending: Pending = serde_json::from_str(cookie).ok()?;
        let fresh = now.saturating_sub(pending.started) < LOGIN_TTL.as_secs();
        (pending.state == state && fresh).then_some(pending)
    }
}

/// A provider's metadata (with its keys), by issuer.
struct Provider {
    issuer: String,
    metadata: CoreProviderMetadata,
    fetched: Instant,
}

pub struct Oidc {
    env: Env,
    settings: RwLock<Settings>,
    /// The metadata of the last two issuers fetched: a save checks a new
    /// issuer while logins may still fetch the one in effect, and neither
    /// pushes the other out.
    providers: Mutex<Vec<Provider>>,
    /// The issuer that last failed to answer, and when.
    failed: Mutex<Option<(String, Instant)>>,
    /// The issuer whose keys were last fetched again for a token they
    /// could not check, and when.
    refetched: Mutex<Option<(String, Instant)>>,
    /// One fetch of the metadata at a time, for a login or a save: the
    /// logins that wait for it use its result rather than asking the
    /// provider too.
    fetching: tokio::sync::Mutex<()>,
    /// One save of the settings at a time (see [`Oidc::lock_saves`]).
    saving: tokio::sync::Mutex<()>,
    /// Signs the cookies of the logins in progress.
    key: Key,
    started: Instant,
    http: Http,
}

impl Oidc {
    /// The settings in effect: the stored ones with the environment on top.
    /// Nothing is fetched from the provider until the first login, so a
    /// provider that is down does not keep the server from starting.
    pub async fn load(db: &Db, env: Env) -> Self {
        let stored = stored(db)
            .await
            .unwrap_or_else(|e| panic!("cannot read the single sign-on settings: {e}"));
        // The HTTP client below needs it (main installs it too; this makes
        // the tests that build an Oidc of their own not depend on order).
        crate::install_tls_provider();
        Oidc {
            settings: RwLock::new(env.apply(&stored)),
            env,
            providers: Mutex::new(Vec::new()),
            failed: Mutex::new(None),
            refetched: Mutex::new(None),
            fetching: tokio::sync::Mutex::new(()),
            saving: tokio::sync::Mutex::new(()),
            key: Key::generate(),
            started: Instant::now(),
            http: Http(
                reqwest::Client::builder()
                    // The crate's advice: following a redirect would let a
                    // provider send these requests anywhere.
                    .redirect(reqwest::redirect::Policy::none())
                    .timeout(Duration::from_secs(10))
                    .build()
                    .expect("an HTTP client"),
            ),
        }
    }

    /// Whether passwords sign anyone in: not with `OIDC_ONLY`.
    pub fn passwords_allowed(&self) -> bool {
        !self.env.only
    }

    pub fn env(&self) -> &Env {
        &self.env
    }

    pub fn settings(&self) -> Settings {
        self.settings.read().unwrap().clone()
    }

    /// The key the login cookies are signed with.
    pub fn key(&self) -> &Key {
        &self.key
    }

    fn now(&self) -> u64 {
        self.started.elapsed().as_secs()
    }

    /// The callback URL: the configured one (`OIDC_REDIRECT_URL` or the
    /// superadmin settings), else the one under `PUBLIC_URL`, else plain http on
    /// the host this request came to (development). Forwarded headers are
    /// not trusted for it: behind a proxy, set `PUBLIC_URL`.
    pub fn callback_url(&self, headers: &HeaderMap, public_url: Option<&str>) -> String {
        let settings = self.settings();
        if !settings.redirect_url.is_empty() {
            return settings.redirect_url;
        }
        if let Some(public_url) = public_url {
            return format!("{public_url}{CALLBACK_PATH}");
        }
        let host = headers
            .get("host")
            .and_then(|v| v.to_str().ok())
            .unwrap_or("localhost");
        format!("http://{host}{CALLBACK_PATH}")
    }

    /// Hold while saving the settings: a save reads the stored row, asks
    /// the provider, writes the row and puts it in effect, and a second
    /// save in between would undo the first one's fields, or leave in
    /// effect what is not stored.
    pub async fn lock_saves(&self) -> tokio::sync::MutexGuard<'_, ()> {
        self.saving.lock().await
    }

    /// Check new settings against their provider (always asked, even
    /// right after a failure: a superadmin is retrying). The provider's
    /// metadata fetched for the check is kept, so the next login needs no
    /// fetch; the settings take effect with [`Oidc::replace`].
    pub async fn check(&self, settings: &Settings) -> Result<(), String> {
        if !settings.enabled() {
            return Ok(());
        }
        let _fetching = self.fetching.lock().await;
        let metadata = self.discover(settings).await?;
        if !settings.redirect_url.is_empty() {
            RedirectUrl::new(settings.redirect_url.clone())
                .map_err(|_| "The callback URL is not a valid URL".to_string())?;
        }
        self.keep(&settings.issuer, metadata);
        Ok(())
    }

    /// Put checked settings in effect (after they are stored).
    pub fn replace(&self, settings: Settings) {
        *self.settings.write().unwrap() = settings;
    }

    /// Start a login: where to send the browser, and the value of the login
    /// cookie to set (signed by the caller with [`Oidc::key`]).
    pub async fn start(
        &self,
        redirect_url: String,
        next: Option<String>,
        invite: Option<String>,
    ) -> Result<(String, String), Failure> {
        let settings = self.settings();
        let client = self.client(&settings, &redirect_url).await?;
        let (challenge, verifier) = PkceCodeChallenge::new_random_sha256();
        let mut request = client.authorize_url(
            CoreAuthenticationFlow::AuthorizationCode,
            CsrfToken::new_random,
            Nonce::new_random,
        );
        for scope in settings.scopes() {
            request = request.add_scope(Scope::new(scope));
        }
        let (url, state, nonce) = request.set_pkce_challenge(challenge).url();
        let pending = Pending {
            state: state.secret().clone(),
            nonce: nonce.secret().clone(),
            verifier: verifier.secret().clone(),
            redirect_url,
            next,
            invite,
            started: self.now(),
        };
        let cookie = serde_json::to_string(&pending).expect("a login serializes");
        Ok((url.to_string(), cookie))
    }

    /// Finish a login: the provider's callback, with the login cookie of
    /// the browser it came to (its signature already checked).
    pub async fn finish(
        &self,
        state: &str,
        cookie: Option<&str>,
        code: Option<String>,
    ) -> Result<Identity, Failure> {
        let pending = cookie
            .and_then(|cookie| Pending::of(cookie, state, self.now()))
            .ok_or(Failure::State)?;
        let code = code.ok_or(Failure::Provider)?;
        let settings = self.settings();
        let client = self.client(&settings, &pending.redirect_url).await?;
        let token = client
            .exchange_code(AuthorizationCode::new(code))
            .map_err(|e| warn(Failure::Provider, error_chain(&e)))?
            .set_pkce_verifier(PkceCodeVerifier::new(pending.verifier))
            .request_async(&self.http)
            .await
            .map_err(|e| warn(Failure::Provider, error_chain(&e)))?;
        let id_token = token.id_token().ok_or(Failure::Token)?;
        let nonce = Nonce::new(pending.nonce);
        let verified: Result<CoreIdTokenClaims, _> =
            match id_token.claims(&client.id_token_verifier(), &nonce) {
                // Signed with a key the cached ones do not have (a new key
                // id), or do not check (a key replaced under the same id):
                // the provider rotated its keys. The token came from the
                // provider itself, so nobody else can make this fetch.
                Err(ClaimsVerificationError::SignatureVerification(
                    SignatureVerificationError::NoMatchingKey
                    | SignatureVerificationError::CryptoError(_),
                )) if self.may_refetch(&settings.issuer) => {
                    self.forget(&settings.issuer);
                    let client = self.client(&settings, &pending.redirect_url).await?;
                    id_token
                        .claims(&client.id_token_verifier(), &nonce)
                        .cloned()
                }
                other => other.cloned(),
            };
        let claims = verified.map_err(|e| warn(Failure::Token, error_chain(&e)))?;
        Ok(Identity {
            next: pending.next,
            invite: pending.invite,
            issuer: claims.issuer().to_string(),
            subject: claims.subject().to_string(),
            verified_email: claims
                .email()
                .filter(|_| claims.email_verified() == Some(true))
                .map(|email| email.to_string()),
        })
    }

    async fn client(
        &self,
        settings: &Settings,
        redirect_url: &str,
    ) -> Result<
        CoreClient<
            openidconnect::EndpointSet,
            openidconnect::EndpointNotSet,
            openidconnect::EndpointNotSet,
            openidconnect::EndpointNotSet,
            openidconnect::EndpointMaybeSet,
            openidconnect::EndpointMaybeSet,
        >,
        Failure,
    > {
        if !settings.enabled() {
            return Err(Failure::Unavailable);
        }
        let metadata = self.metadata(settings).await?;
        let secret = (!settings.client_secret.is_empty())
            .then(|| ClientSecret::new(settings.client_secret.clone()));
        let redirect_url = RedirectUrl::new(redirect_url.to_string())
            .map_err(|e| warn(Failure::Unavailable, error_chain(&e)))?;
        Ok(CoreClient::from_provider_metadata(
            metadata,
            ClientId::new(settings.client_id.clone()),
            secret,
        )
        .set_redirect_uri(redirect_url))
    }

    /// The provider's metadata, fetched again when it is old.
    async fn metadata(&self, settings: &Settings) -> Result<CoreProviderMetadata, Failure> {
        let issuer = &settings.issuer;
        if let Some(metadata) = self.cached(issuer) {
            return Ok(metadata);
        }
        let _fetching = self.fetching.lock().await;
        // Fetched, or failed, while this login waited.
        if let Some(metadata) = self.cached(issuer) {
            return Ok(metadata);
        }
        if let Some((failed, at)) = &*self.failed.lock().unwrap()
            && failed == issuer
            && at.elapsed() < RETRY_AFTER
        {
            return Err(Failure::Unavailable);
        }
        match self.discover(settings).await {
            Ok(metadata) => {
                self.keep(issuer, metadata.clone());
                Ok(metadata)
            }
            Err(e) => {
                *self.failed.lock().unwrap() = Some((issuer.clone(), Instant::now()));
                Err(warn(Failure::Unavailable, e))
            }
        }
    }

    fn cached(&self, issuer: &str) -> Option<CoreProviderMetadata> {
        let providers = self.providers.lock().unwrap();
        providers
            .iter()
            .find(|p| p.issuer == issuer && p.fetched.elapsed() < METADATA_TTL)
            .map(|p| p.metadata.clone())
    }

    fn keep(&self, issuer: &str, metadata: CoreProviderMetadata) {
        let mut providers = self.providers.lock().unwrap();
        providers.retain(|p| p.issuer != issuer);
        providers.insert(
            0,
            Provider {
                issuer: issuer.to_string(),
                metadata,
                fetched: Instant::now(),
            },
        );
        providers.truncate(2);
        let mut failed = self.failed.lock().unwrap();
        if failed.as_ref().is_some_and(|(f, _)| f == issuer) {
            *failed = None;
        }
    }

    /// Whether the keys of an issuer may be fetched again for a token they
    /// could not check, and if so, from now on not for a while.
    fn may_refetch(&self, issuer: &str) -> bool {
        let mut refetched = self.refetched.lock().unwrap();
        if let Some((last, at)) = &*refetched
            && last == issuer
            && at.elapsed() < REFETCH_EVERY
        {
            return false;
        }
        *refetched = Some((issuer.to_string(), Instant::now()));
        true
    }

    fn forget(&self, issuer: &str) {
        self.providers
            .lock()
            .unwrap()
            .retain(|p| p.issuer != issuer);
    }

    async fn discover(&self, settings: &Settings) -> Result<CoreProviderMetadata, String> {
        let issuer = IssuerUrl::new(settings.issuer.clone())
            .map_err(|_| "The issuer is not a valid URL".to_string())?;
        if !tls_or_local(issuer.url()) {
            return Err("The issuer must be an https:// URL".into());
        }
        let metadata = CoreProviderMetadata::discover_async(issuer, &self.http)
            .await
            .map_err(|e| format!("Cannot reach the provider: {}", error_chain(&e)))?;
        // The keys, the code exchange (with the client secret) and the
        // login page go to the addresses the provider names: TLS for them
        // too.
        let endpoints = [
            Some(metadata.authorization_endpoint().url()),
            metadata.token_endpoint().map(|url| url.url()),
            Some(metadata.jwks_uri().url()),
        ];
        if let Some(url) = endpoints
            .into_iter()
            .flatten()
            .find(|url| !tls_or_local(url))
        {
            return Err(format!(
                "The provider's endpoint {url} must be an https:// URL"
            ));
        }
        Ok(metadata)
    }
}

/// The provider's keys and the client secret travel over TLS. Plain HTTP
/// is for a provider on this machine (development, the tests).
fn tls_or_local(url: &Url) -> bool {
    let local = matches!(url.host_str(), Some("localhost" | "127.0.0.1" | "[::1]"));
    url.scheme() == "https" || (url.scheme() == "http" && local)
}

/// The stored settings.
pub async fn stored(db: &Db) -> sqlx::Result<Settings> {
    crate::server_settings::load(db.reader(), "oidc").await
}

/// Our reqwest, as the HTTP client of the `openidconnect` crate. A type of
/// its own rather than a closure: the future must be `Send` for any
/// lifetime, which a closure's cannot promise.
struct Http(reqwest::Client);

impl<'c> AsyncHttpClient<'c> for Http {
    type Error = FetchError;
    type Future = Pin<Box<dyn Future<Output = Result<HttpResponse, FetchError>> + Send + 'c>>;

    fn call(&'c self, request: HttpRequest) -> Self::Future {
        Box::pin(send(&self.0, request))
    }
}

#[derive(Debug)]
enum FetchError {
    Request(reqwest::Error),
    TooLarge,
}

impl std::fmt::Display for FetchError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            FetchError::Request(e) => write!(f, "{e}"),
            FetchError::TooLarge => write!(f, "the answer is larger than {MAX_RESPONSE} bytes"),
        }
    }
}

impl std::error::Error for FetchError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            FetchError::Request(e) => e.source(),
            FetchError::TooLarge => None,
        }
    }
}

async fn send(http: &reqwest::Client, request: HttpRequest) -> Result<HttpResponse, FetchError> {
    let (parts, body) = request.into_parts();
    let mut response = http
        .request(parts.method, parts.uri.to_string())
        .headers(parts.headers)
        .body(body)
        .send()
        .await
        .map_err(FetchError::Request)?;
    let status = response.status();
    let headers = response.headers().clone();
    let mut body = Vec::new();
    while let Some(chunk) = response.chunk().await.map_err(FetchError::Request)? {
        if body.len() + chunk.len() > MAX_RESPONSE {
            return Err(FetchError::TooLarge);
        }
        body.extend_from_slice(&chunk);
    }
    let mut answer = HttpResponse::new(body);
    *answer.status_mut() = status;
    *answer.headers_mut() = headers;
    Ok(answer)
}

fn warn(failure: Failure, message: impl std::fmt::Display) -> Failure {
    tracing::warn!("single sign-on failed ({}): {message}", failure.code());
    failure
}

/// An error with its causes, which the crate's errors keep the details in.
fn error_chain(error: &dyn std::error::Error) -> String {
    let mut message = error.to_string();
    let mut source = error.source();
    while let Some(cause) = source {
        message.push_str(": ");
        message.push_str(&cause.to_string());
        source = cause.source();
    }
    message
}

#[cfg(test)]
mod tests {
    use super::*;

    fn cookie(state: &str, started: u64) -> String {
        serde_json::to_string(&Pending {
            state: state.into(),
            nonce: "n".into(),
            verifier: "v".into(),
            redirect_url: String::new(),
            next: None,
            invite: None,
            started,
        })
        .unwrap()
    }

    #[test]
    fn a_login_cookie_is_for_its_state_and_ten_minutes() {
        let now = 1_000_000;
        assert!(Pending::of(&cookie("s", now), "s", now).is_some());
        assert!(Pending::of(&cookie("s", now - 599), "s", now).is_some());
        assert!(
            Pending::of(&cookie("s", now - 600), "s", now).is_none(),
            "expired"
        );
        assert!(
            Pending::of(&cookie("s", now), "other", now).is_none(),
            "another state"
        );
        assert!(Pending::of("not json", "s", now).is_none());
    }

    #[test]
    fn the_environment_wins_and_locks_its_fields() {
        let env = Env {
            issuer: Some("https://id.example.com".into()),
            label: Some("Company account".into()),
            ..Env::default()
        };
        let stored = Settings {
            issuer: "https://other.example.com".into(),
            client_id: "organizer".into(),
            label: "Stored label".into(),
            ..Settings::default()
        };
        let settings = env.apply(&stored);
        assert_eq!(settings.issuer, "https://id.example.com");
        assert_eq!(settings.client_id, "organizer");
        assert_eq!(settings.label, "Company account");
        assert_eq!(env.locked(), ["issuer", "label"]);
    }

    #[test]
    fn openid_is_asked_for_once() {
        let settings = Settings {
            scopes: "openid email profile".into(),
            ..Settings::default()
        };
        assert_eq!(settings.scopes(), ["email", "profile"]);
        assert_eq!(Settings::default().scopes(), ["email"]);
    }

    #[tokio::test]
    async fn the_callback_is_under_the_public_url_and_forwarded_headers_do_not_count() {
        let oidc = Oidc::load(&crate::db::open(":memory:").await.unwrap(), Env::default()).await;
        let mut headers = HeaderMap::new();
        headers.insert("host", "files.example.com".parse().unwrap());
        headers.insert("x-forwarded-proto", "https".parse().unwrap());
        headers.insert("x-forwarded-host", "elsewhere.example.com".parse().unwrap());
        assert_eq!(
            oidc.callback_url(&headers, None),
            "http://files.example.com/api/auth/oidc/callback"
        );
        assert_eq!(
            oidc.callback_url(&headers, Some("https://files.example.org")),
            "https://files.example.org/api/auth/oidc/callback"
        );
    }

    #[tokio::test]
    async fn keys_are_fetched_again_at_most_once_a_minute_per_issuer() {
        let oidc = Oidc::load(&crate::db::open(":memory:").await.unwrap(), Env::default()).await;
        assert!(oidc.may_refetch("https://a.example.com"));
        assert!(!oidc.may_refetch("https://a.example.com"));
        assert!(oidc.may_refetch("https://b.example.com"));
        *oidc.refetched.lock().unwrap() = Some((
            "https://a.example.com".into(),
            Instant::now() - REFETCH_EVERY - Duration::from_secs(1),
        ));
        assert!(oidc.may_refetch("https://a.example.com"));
    }

    #[test]
    fn only_tls_or_this_machine() {
        let ok = |url: &str| tls_or_local(&Url::parse(url).unwrap());
        assert!(ok("https://id.example.com/keys"));
        assert!(ok("http://127.0.0.1:8080/keys"));
        assert!(ok("http://localhost/keys"));
        assert!(!ok("http://id.example.com/keys"));
        assert!(!ok("http://10.0.0.1/keys"));
    }
}
