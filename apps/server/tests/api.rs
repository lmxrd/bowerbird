//! Integration tests: the app on axum-test's TestServer, over a real local
//! socket (the app reads the client's address from the connection), against
//! an in-memory database.

use axum::http::{Method, StatusCode};
use axum_test::{TestResponse as Response, TestServer};
use organizer::{app, auth, db, oidc, state::AppState};
use serde_json::{Value, json};
use std::sync::Arc;

const UA: &str = "Mozilla/5.0 (Macintosh; Intel Mac OS X 15_0) AppleWebKit/605.1.15 (KHTML, like Gecko) Version/18.0 Safari/605.1.15";

struct TestApp {
    server: TestServer,
    /// Where the server listens, e.g. `http://127.0.0.1:41234`.
    base: String,
    /// The server's state, for what the API does not reach (the hourly
    /// sweep, the variables).
    state: AppState,
}

async fn spawn_app() -> TestApp {
    spawn_app_with(oidc::Env::default()).await
}

/// The app, with OIDC_* environment variables as given.
async fn spawn_app_with(oidc_env: oidc::Env) -> TestApp {
    spawn_app_full(oidc_env, false).await
}

/// The app, with OIDC_* environment variables and TOTP_REQUIRED as given.
async fn spawn_app_full(oidc_env: oidc::Env, totp_required: bool) -> TestApp {
    spawn(Options {
        oidc: oidc_env,
        totp_required,
        ..Options::default()
    })
    .await
}

/// What a test app is started with.
struct Options {
    oidc: oidc::Env,
    totp_required: bool,
    /// `PUBLIC_URL`; https makes every cookie Secure and `__Host-`.
    public_url: Option<String>,
    /// The login limits: large ones, but for the tests of the limits.
    limits: organizer::limits::Limits,
    /// `SWAGGER_UI`.
    swagger_ui: bool,
    /// `STORAGE_URL`.
    storage_url: Option<std::path::PathBuf>,
    /// The AI_* variables.
    ai: organizer::ai::Env,
    /// `LINKS_ALLOW_PRIVATE`: the tests' pages are on this machine.
    links_allow_private: bool,
    backup: organizer::backup::Env,
    /// `SIGNUP_OPEN` and `OIDC_CREATE_USERS`.
    joining: organizer::users::Env,
    /// `VAPID_PRIVATE_KEY` and the `APNS_*` variables.
    push: organizer::push::Env,
}

impl Default for Options {
    fn default() -> Self {
        Options {
            oidc: oidc::Env::default(),
            totp_required: false,
            public_url: None,
            limits: organizer::limits::Limits::with_bursts(1_000, 1_000, 1_000),
            swagger_ui: false,
            storage_url: None,
            ai: organizer::ai::Env::default(),
            links_allow_private: false,
            backup: organizer::backup::Env::default(),
            joining: organizer::users::Env::default(),
            push: organizer::push::Env::default(),
        }
    }
}

async fn spawn(options: Options) -> TestApp {
    organizer::install_tls_provider();
    let db = db::open(":memory:").await.unwrap();
    let data_dir = std::env::temp_dir().join(format!("organizer-api-{}", uuid::Uuid::now_v7()));
    std::fs::create_dir_all(&data_dir).unwrap();
    let state = AppState {
        push: Arc::new(
            organizer::push::Push::load(&db, options.push, options.links_allow_private)
                .await
                .unwrap(),
        ),
        oidc: Arc::new(oidc::Oidc::load(&db, options.oidc).await),
        files: organizer::files::Files::new(
            db.clone(),
            data_dir.clone(),
            options
                .storage_url
                .map(organizer::config::StorageUrl::Local)
                .unwrap_or_else(|| {
                    organizer::config::storage_url(&data_dir)
                        .expect("a valid STORAGE_URL in the tests")
                }),
        ),
        db: db.clone(),
        trust_proxy: true,
        swagger_ui: options.swagger_ui,
        limits: Arc::new(options.limits),
        secure_cookies: options
            .public_url
            .as_deref()
            .is_some_and(|u| u.starts_with("https://")),
        public_url: options.public_url,
        challenge_key: axum_extra::extract::cookie::Key::generate(),
        totp_required: options.totp_required,
        ai: Arc::new(organizer::ai::Ai::load(&db, options.ai).await),
        jobs: organizer::jobs::Jobs::new(db.clone(), organizer::handlers()),
        links_allow_private: options.links_allow_private,
        vectors: organizer::vectors::open("turso::memory:").await.unwrap(),
        backup: Arc::new(organizer::backup::Backup::load(&db, options.backup).await),
        joining: options.joining,
    };
    state.jobs.start(state.clone()).await.unwrap();
    tokio::spawn(organizer::search::run(state.clone()));
    let server = TestServer::builder()
        .http_transport()
        .build(app(state.clone()).into_make_service_with_connect_info::<std::net::SocketAddr>());
    let base = server
        .server_address()
        .expect("an http transport")
        .to_string()
        .trim_end_matches('/')
        .to_string();
    TestApp {
        server,
        base,
        state,
    }
}

/// The session cookie of a response.
fn cookie(response: &Response) -> String {
    let set_cookie = response
        .headers()
        .get("set-cookie")
        .expect("a set-cookie header")
        .to_str()
        .unwrap()
        .to_string();
    set_cookie
        .split(';')
        .next()
        .expect("a cookie name and value")
        .to_string()
}

async fn request(
    app: &TestApp,
    method: Method,
    path: &str,
    session: Option<&str>,
    body: Option<Value>,
) -> Response {
    let mut builder = app
        .server
        .method(method, path)
        .add_header("user-agent", UA)
        .add_header("x-forwarded-for", "203.0.113.5");
    if let Some(session) = session {
        builder = builder.add_header("cookie", session);
    }
    if let Some(body) = body {
        builder = builder.json(&body);
    }
    builder.await
}

async fn json(response: Response) -> Value {
    response.json()
}

#[tokio::test]
async fn the_health_endpoint_answers_ok() {
    let response = spawn_app().await.server.get("/api/health").await;
    response.assert_status_ok();
    response.assert_text("OK");
}

#[tokio::test]
async fn an_unknown_api_path_is_a_json_404() {
    let response = spawn_app().await.server.get("/api/nothing-here").await;
    response.assert_status(StatusCode::NOT_FOUND);
    response.assert_json(&json!({ "error": "Not found" }));
}

#[tokio::test]
async fn other_paths_get_the_frontend() {
    let response = spawn_app().await.server.get("/some/client/route").await;
    response.assert_status_ok();
    assert!(response.text().contains("<div id=\"root\">"));
    assert_eq!(response.header("cache-control"), "no-cache");
}

#[tokio::test]
async fn the_spec_is_served_only_with_swagger_ui_set() {
    // Off by default: the paths fall through to the frontend, never the spec.
    let response = spawn_app().await.server.get(organizer::SPEC_PATH).await;
    assert!(!response.text().contains("\"openapi\""));

    let app = spawn(Options {
        swagger_ui: true,
        ..Options::default()
    })
    .await;
    let spec: Value = app.server.get(organizer::SPEC_PATH).await.json();
    assert_eq!(spec["info"]["title"], "Organizer API");
    assert!(spec["paths"]["/api/health"]["get"].is_object());
    app.server
        .get(&format!("{}/", organizer::SWAGGER_UI_PATH))
        .await
        .assert_status_ok();
}

#[tokio::test]
async fn every_documented_operation_is_routed() {
    let app = spawn_app().await;
    let spec: Value = serde_json::from_str(&organizer::openapi::spec_json()).unwrap();
    let mut checked = 0;
    for (path, item) in spec["paths"].as_object().unwrap() {
        for method in item.as_object().unwrap().keys() {
            let url = path.replace("{id}", "1");
            let response = request(
                &app,
                Method::from_bytes(method.to_uppercase().as_bytes()).unwrap(),
                &url,
                None,
                None,
            )
            .await;
            let status = response.status_code().as_u16();
            assert_ne!(status, 405, "{method} {path} is documented but not routed");
            if !path.contains('{') {
                assert_ne!(status, 404, "{method} {path} is documented but not routed");
            }
            checked += 1;
        }
    }
    assert!(checked >= 20, "only {checked} operations documented");
}

async fn post_raw(app: &TestApp, path: &str, content_type: &str, body: &str) -> Response {
    app.server
        .post(path)
        .add_header("user-agent", UA)
        .add_header("content-type", content_type)
        .bytes(body.to_string().into())
        .await
}

/// Bodies are JSON and nothing else.
#[tokio::test]
async fn bodies_are_json_only() {
    let app = spawn_app().await;
    let credentials = r#"{"email":"a@example.com","password":"correct horse battery"}"#;

    // Valid JSON in another content type is a 415, in the API's error shape.
    let response = post_raw(&app, "/api/auth/setup", "text/plain", credentials).await;
    assert_eq!(response.status_code(), 415);
    assert_eq!(
        json(response).await["error"],
        json!("Expected a JSON body (Content-Type: application/json)")
    );
    // Invalid JSON, or JSON of the wrong shape, is a 400.
    for body in ["{nope", r#"{"email": 1}"#, ""] {
        let response = post_raw(&app, "/api/auth/setup", "application/json", body).await;
        assert_eq!(response.status_code(), 400, "{body:?}");
        assert_eq!(json(response).await["error"], json!("Invalid request body"));
    }
    // Nothing was created by any of those; the real thing works.
    let response = post_raw(&app, "/api/auth/setup", "application/json", credentials).await;
    assert_eq!(response.status_code(), 201);

    // A login with a body the API cannot read fails like a wrong password.
    for (content_type, body) in [("text/plain", credentials), ("application/json", "{nope")] {
        let response = post_raw(&app, "/api/auth/login", content_type, body).await;
        assert_eq!(response.status_code(), 401, "{content_type} {body}");
    }
}

mod provider {
    use axum::extract::{Form, State};
    use axum::routing::{get, post};
    use axum::{Json, Router};
    use chrono::{Duration, Utc};
    use openidconnect::core::{
        CoreIdToken, CoreIdTokenClaims, CoreJsonWebKeySet, CoreJwsSigningAlgorithm,
        CoreRsaPrivateSigningKey,
    };
    use openidconnect::{
        Audience, EmptyAdditionalClaims, EndUserEmail, IssuerUrl, JsonWebKeyId, Nonce,
        PrivateSigningKey, StandardClaims, SubjectIdentifier,
    };
    use serde_json::{Value, json};
    use std::collections::HashMap;
    use std::sync::atomic::{AtomicBool, Ordering};
    use std::sync::{Arc, Mutex};

    pub const CLIENT_ID: &str = "organizer";
    pub const CLIENT_SECRET: &str = "the client secret";

    pub struct Provider {
        pub issuer: String,
        key: Arc<CoreRsaPrivateSigningKey>,
        other_key: Arc<CoreRsaPrivateSigningKey>,
        /// The other key under the first one's id.
        other_key_same_id: Arc<CoreRsaPrivateSigningKey>,
        /// Whether the provider has rotated to the other key: it publishes
        /// and signs with that one now.
        rotated: Arc<AtomicBool>,
        /// Whether it kept the first key's id for it.
        kept_id: Arc<AtomicBool>,
        codes: Arc<Mutex<HashMap<String, String>>>,
    }

    /// What an ID token says; [`Claims::new`] is a valid one.
    pub struct Claims {
        pub issuer: String,
        pub audience: String,
        pub subject: String,
        pub email: Option<String>,
        pub email_verified: bool,
        pub nonce: String,
        pub expires_in: Duration,
        /// Signed with the key the provider does not publish.
        pub foreign_key: bool,
    }

    impl Claims {
        pub fn new(provider: &Provider, subject: &str, email: &str, nonce: &str) -> Self {
            Claims {
                issuer: provider.issuer.clone(),
                audience: CLIENT_ID.into(),
                subject: subject.into(),
                email: Some(email.into()),
                email_verified: true,
                nonce: nonce.into(),
                expires_in: Duration::minutes(5),
                foreign_key: false,
            }
        }
    }

    fn signing_key(pem: &str, id: &str) -> CoreRsaPrivateSigningKey {
        CoreRsaPrivateSigningKey::from_pem(pem, Some(JsonWebKeyId::new(id.into()))).unwrap()
    }

    pub async fn spawn() -> Provider {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let issuer = format!("http://{}", listener.local_addr().unwrap());
        let key = Arc::new(signing_key(include_str!("fixtures/oidc-key.pem"), "first"));
        let other_key = Arc::new(signing_key(
            include_str!("fixtures/oidc-other-key.pem"),
            "second",
        ));
        let other_key_same_id = Arc::new(signing_key(
            include_str!("fixtures/oidc-other-key.pem"),
            "first",
        ));
        let rotated = Arc::new(AtomicBool::new(false));
        let kept_id = Arc::new(AtomicBool::new(false));
        let codes: Arc<Mutex<HashMap<String, String>>> = Arc::default();
        let discovery = json!({
            "issuer": issuer,
            "authorization_endpoint": format!("{issuer}/authorize"),
            "token_endpoint": format!("{issuer}/token"),
            "jwks_uri": format!("{issuer}/jwks"),
            "response_types_supported": ["code"],
            "subject_types_supported": ["public"],
            "id_token_signing_alg_values_supported": ["RS256"],
        });
        let jwks = {
            let keys = (
                key.clone(),
                other_key.clone(),
                other_key_same_id.clone(),
                rotated.clone(),
                kept_id.clone(),
            );
            move || {
                let (key, other_key, other_key_same_id, rotated, kept_id) = &keys;
                let published = match (
                    rotated.load(Ordering::SeqCst),
                    kept_id.load(Ordering::SeqCst),
                ) {
                    (false, _) => key,
                    (true, false) => other_key,
                    (true, true) => other_key_same_id,
                };
                let keys = CoreJsonWebKeySet::new(vec![published.as_verification_key()]);
                async move { Json(serde_json::to_value(keys).unwrap()) }
            }
        };
        let app = Router::new()
            .route(
                "/.well-known/openid-configuration",
                get(move || async move { Json(discovery) }),
            )
            .route("/jwks", get(jwks))
            .route("/token", post(token))
            .with_state(codes.clone());
        tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
        Provider {
            issuer,
            key,
            other_key,
            other_key_same_id,
            rotated,
            kept_id,
            codes,
        }
    }

    async fn token(
        State(codes): State<Arc<Mutex<HashMap<String, String>>>>,
        Form(form): Form<HashMap<String, String>>,
    ) -> Result<Json<Value>, (axum::http::StatusCode, Json<Value>)> {
        let code = form.get("code").cloned().unwrap_or_default();
        let id_token = codes.lock().unwrap().remove(&code).ok_or((
            axum::http::StatusCode::BAD_REQUEST,
            Json(json!({ "error": "invalid_grant" })),
        ))?;
        Ok(Json(json!({
            "access_token": "an access token",
            "token_type": "Bearer",
            "expires_in": 300,
            "id_token": id_token,
        })))
    }

    impl Provider {
        /// Publish and sign with the other key from now on.
        pub fn rotate(&self) {
            self.rotated.store(true, Ordering::SeqCst);
        }

        /// The same, but under the first key's id.
        pub fn rotate_keeping_id(&self) {
            self.kept_id.store(true, Ordering::SeqCst);
            self.rotate();
        }

        /// A code the token endpoint answers with an ID token of these claims.
        pub fn code_for(&self, claims: Claims) -> String {
            let now = Utc::now();
            let standard = StandardClaims::new(SubjectIdentifier::new(claims.subject))
                .set_email(claims.email.map(EndUserEmail::new))
                .set_email_verified(Some(claims.email_verified));
            let token_claims = CoreIdTokenClaims::new(
                IssuerUrl::new(claims.issuer).unwrap(),
                vec![Audience::new(claims.audience)],
                now + claims.expires_in,
                now - Duration::minutes(1),
                standard,
                EmptyAdditionalClaims {},
            )
            .set_nonce(Some(Nonce::new(claims.nonce)));
            let rotated = self.rotated.load(Ordering::SeqCst);
            let key = match (
                claims.foreign_key != rotated,
                self.kept_id.load(Ordering::SeqCst),
            ) {
                (false, _) => &*self.key,
                (true, true) if rotated => &*self.other_key_same_id,
                (true, _) => &*self.other_key,
            };
            let id_token = CoreIdToken::new(
                token_claims,
                key,
                CoreJwsSigningAlgorithm::RsaSsaPkcs1V15Sha256,
                None,
                None,
            )
            .unwrap();
            let code = organizer::util::random_hex(8);
            self.codes
                .lock()
                .unwrap()
                .insert(code.clone(), id_token.to_string());
            code
        }
    }
}

/// A cookie value as a browser sends it.
fn urlencoding(value: &str) -> String {
    value
        .bytes()
        .map(|b| match b {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' => (b as char).to_string(),
            _ => format!("%{b:02X}"),
        })
        .collect()
}

/// The value of a cookie a response sets, as a `cookie` header value.
fn set_cookie(response: &Response, name: &str) -> Option<String> {
    response
        .headers()
        .get_all("set-cookie")
        .iter()
        .filter_map(|v| v.to_str().ok())
        .filter_map(|v| v.split(';').next())
        .find(|pair| pair.starts_with(&format!("{name}=")) && pair.len() > name.len() + 1)
        .map(String::from)
}

fn location(response: &Response) -> String {
    response.headers()["location"].to_str().unwrap().to_string()
}

/// A login in progress: where the app sent the browser.
struct Login {
    url: reqwest::Url,
    state: String,
    nonce: String,
    /// The login cookie, as a `cookie` header value.
    cookie: String,
}

async fn start_login(app: &TestApp) -> Login {
    start_login_at(app, "/api/auth/oidc/login").await
}

async fn start_login_at(app: &TestApp, path: &str) -> Login {
    let response = app.server.get(path).await;
    assert_eq!(response.status_code(), 303);
    let url = reqwest::Url::parse(&location(&response)).expect("a redirect to the provider");
    let param = |name: &str| {
        url.query_pairs()
            .find(|(k, _)| k == name)
            .map(|(_, v)| v.to_string())
            .unwrap_or_default()
    };
    Login {
        state: param("state"),
        nonce: param("nonce"),
        cookie: set_cookie(&response, "organizer_oidc").expect("the login cookie"),
        url,
    }
}

/// The provider's redirect back to the app.
async fn callback(app: &TestApp, query: &str, cookie: Option<&str>) -> Response {
    let mut request = app.server.get(&format!("/api/auth/oidc/callback?{query}"));
    if let Some(cookie) = cookie {
        request = request.add_header("cookie", cookie);
    }
    request.await
}

/// A whole login with these claims (the nonce is the login's): the
/// session cookie, or where the browser was sent instead.
async fn sign_in(
    app: &TestApp,
    provider: &provider::Provider,
    claims: impl FnOnce(&Login) -> provider::Claims,
) -> Result<String, String> {
    let login = start_login(app).await;
    let code = provider.code_for(claims(&login));
    let response = callback(
        app,
        &format!("code={code}&state={}", login.state),
        Some(&login.cookie),
    )
    .await;
    assert_eq!(response.status_code(), 303);
    match set_cookie(&response, "organizer_session") {
        Some(session) => {
            assert_eq!(location(&response), "/");
            Ok(session)
        }
        None => Err(location(&response)),
    }
}

/// The first admin (the superadmin), signed in, and stepped up so that the
/// superadmin's endpoints answer (`stepped_up`; the step-up itself is
/// tested in `superadmin_endpoints_need_a_second_factor_confirmed_lately`).
async fn admin_session(app: &TestApp) -> String {
    let response = request(
        app,
        Method::POST,
        "/api/auth/setup",
        None,
        Some(json!({ "email": "admin@example.com", "password": "correct horse battery" })),
    )
    .await;
    let session = cookie(&response);
    stepped_up(app, &session, 0).await;
    session
}

async fn save_oidc(app: &TestApp, session: &str, body: Value) -> Response {
    request(
        app,
        Method::PUT,
        "/api/superadmin/oidc",
        Some(session),
        Some(body),
    )
    .await
}

async fn configure(app: &TestApp, session: &str, provider: &provider::Provider, extra: Value) {
    let mut body = json!({
        "issuer": provider.issuer,
        "client_id": provider::CLIENT_ID,
        "client_secret": provider::CLIENT_SECRET,
    });
    body.as_object_mut()
        .unwrap()
        .extend(extra.as_object().unwrap().clone());
    let response = save_oidc(app, session, body).await;
    assert_eq!(response.status_code(), 200);
}

async fn session_of(app: &TestApp, session: Option<&str>) -> Value {
    json(request(app, Method::GET, "/api/auth/session", session, None).await).await
}

/// Single sign-on is off until an admin sets a provider that
/// answers; only admins see or change the settings; a user without a
/// password needs single sign-on to be on.
#[tokio::test]
async fn single_sign_on_is_off_until_a_working_provider_is_set() {
    let app = spawn_app().await;
    let admin = admin_session(&app).await;
    assert_eq!(session_of(&app, None).await["oidc"], json!(null));
    let response = app.server.get("/api/auth/oidc/login").await;
    assert_eq!(response.status_code(), 303);
    assert_eq!(location(&response), "/login?error=unavailable");

    // A provider that does not answer, or not over TLS, is refused, and
    // nothing changes.
    for issuer in ["http://127.0.0.1:9", "http://id.example.com"] {
        let response = save_oidc(
            &app,
            &admin,
            json!({ "issuer": issuer, "client_id": "organizer" }),
        )
        .await;
        assert_eq!(response.status_code(), 400, "{issuer}");
    }
    let settings = json(
        request(
            &app,
            Method::GET,
            "/api/superadmin/oidc",
            Some(&admin),
            None,
        )
        .await,
    )
    .await;
    assert_eq!(settings["enabled"], json!(false));
    assert_eq!(settings["issuer"], json!(""));
    assert_eq!(
        settings["callback_url"],
        json!(format!("{}/api/auth/oidc/callback", app.base))
    );

    // Signed in only.
    assert_eq!(
        request(&app, Method::GET, "/api/superadmin/oidc", None, None)
            .await
            .status_code(),
        401
    );

    // With a working provider it is on, at once, and the secret is never
    // sent back.
    let provider = provider::spawn().await;
    configure(&app, &admin, &provider, json!({ "label": "Acme ID" })).await;
    let settings = request(
        &app,
        Method::GET,
        "/api/superadmin/oidc",
        Some(&admin),
        None,
    )
    .await;
    let settings = settings.text();
    assert!(!settings.contains(provider::CLIENT_SECRET), "{settings}");
    let settings: Value = serde_json::from_str(&settings).unwrap();
    assert_eq!(settings["enabled"], json!(true));
    assert_eq!(settings["client_secret_set"], json!(true));
    assert_eq!(
        session_of(&app, None).await["oidc"],
        json!({ "label": "Acme ID" })
    );

    // Leaving a field out keeps it; an empty secret removes it.
    let response = save_oidc(&app, &admin, json!({ "client_secret": "" })).await;
    let settings = json(response).await;
    assert_eq!(settings["client_secret_set"], json!(false));
    assert_eq!(settings["label"], json!("Acme ID"));
}

/// The login asks for a code with PKCE and a nonce; the user is found by
/// their verified email, then by the provider account, whatever the email
/// becomes.
#[tokio::test]
async fn single_sign_on_signs_the_user_in() {
    let app = spawn_app().await;
    let admin = admin_session(&app).await;
    let provider = provider::spawn().await;
    configure(&app, &admin, &provider, json!({})).await;

    let login = start_login(&app).await;
    assert_eq!(
        login.url.as_str().split('?').next(),
        Some(&*format!("{}/authorize", provider.issuer))
    );
    let params: std::collections::HashMap<_, _> = login.url.query_pairs().into_owned().collect();
    assert_eq!(params["response_type"], "code");
    assert_eq!(params["client_id"], provider::CLIENT_ID);
    assert_eq!(params["code_challenge_method"], "S256");
    assert!(!params["code_challenge"].is_empty());
    assert!(!login.nonce.is_empty() && !login.state.is_empty());
    assert_eq!(params["scope"], "openid email");
    assert_eq!(
        params["redirect_uri"],
        format!("{}/api/auth/oidc/callback", app.base)
    );

    let session = sign_in(&app, &provider, |login| {
        provider::Claims::new(&provider, "account-1", "Admin@Example.com", &login.nonce)
    })
    .await
    .expect("signed in");
    let user = &session_of(&app, Some(&session)).await["user"];
    assert_eq!(user["email"], json!("admin@example.com"));
    assert_eq!(user["has_password"], json!(true));

    // Linked now: the account signs in as the admin, whatever its email says.
    let session = sign_in(&app, &provider, |login| {
        provider::Claims::new(&provider, "account-1", "renamed@example.com", &login.nonce)
    })
    .await
    .expect("signed in");
    assert_eq!(
        session_of(&app, Some(&session)).await["user"]["email"],
        json!("admin@example.com")
    );
}

/// Every callback that is not the provider's answer to a
/// login of this browser signs nobody in.
#[tokio::test]
async fn single_sign_on_refuses_bad_callbacks() {
    let app = spawn_app().await;
    let admin = admin_session(&app).await;
    let provider = provider::spawn().await;
    configure(&app, &admin, &provider, json!({})).await;
    let valid = |login: &Login| {
        provider::Claims::new(&provider, "account-1", "admin@example.com", &login.nonce)
    };
    let refused = |response: Response, error: &str| {
        assert_eq!(response.status_code(), 303);
        assert_eq!(location(&response), format!("/login?error={error}"));
        assert_eq!(set_cookie(&response, "organizer_session"), None);
    };

    // The state: missing, unknown, from another browser, or used before.
    refused(callback(&app, "code=x", None).await, "state");
    let login = start_login(&app).await;
    let code = provider.code_for(valid(&login));
    refused(
        callback(
            &app,
            &format!("code={code}&state=unknown"),
            Some(&login.cookie),
        )
        .await,
        "state",
    );
    refused(
        callback(&app, &format!("code={code}&state={}", login.state), None).await,
        "state",
    );
    let login = start_login(&app).await;
    let other = start_login(&app).await;
    let code = provider.code_for(valid(&login));
    let query = format!("code={code}&state={}", login.state);
    refused(callback(&app, &query, Some(&other.cookie)).await, "state");
    // A login cookie this server did not sign, made from what the
    // provider's URL shows: the state and the nonce.
    let forged = json!({
        "state": login.state,
        "nonce": login.nonce,
        "verifier": "a guess",
        "redirect_url": format!("{}/api/auth/oidc/callback", app.base),
        "started": jiff::Timestamp::now().as_second(),
    });
    let forged = format!("organizer_oidc={}", urlencoding(&forged.to_string()));
    refused(callback(&app, &query, Some(&forged)).await, "state");

    // The provider refused, or does not know the code.
    let login = start_login(&app).await;
    let query = format!("error=access_denied&state={}", login.state);
    refused(
        callback(&app, &query, Some(&login.cookie)).await,
        "provider",
    );
    let login = start_login(&app).await;
    let query = format!("code=unknown&state={}", login.state);
    refused(
        callback(&app, &query, Some(&login.cookie)).await,
        "provider",
    );

    // An ID token that is not for this login, this client, now, or not
    // signed by the provider.
    type Change = fn(&mut provider::Claims);
    let changes: [(&str, Change); 5] = [
        ("nonce", |c| c.nonce = "another nonce".into()),
        ("audience", |c| c.audience = "another client".into()),
        ("issuer", |c| c.issuer = "http://127.0.0.1:9".into()),
        ("expiry", |c| c.expires_in = chrono::Duration::minutes(-5)),
        ("signature", |c| c.foreign_key = true),
    ];
    for (what, change) in changes {
        let result = sign_in(&app, &provider, |login| {
            let mut claims = valid(login);
            change(&mut claims);
            claims
        })
        .await;
        assert_eq!(result, Err("/login?error=token".into()), "{what}");
    }

    // No verified email for an account that is not linked yet, or nobody
    // with that email.
    let result = sign_in(&app, &provider, |login| provider::Claims {
        email_verified: false,
        ..valid(login)
    })
    .await;
    assert_eq!(result, Err("/login?error=email".into()));
    let result = sign_in(&app, &provider, |login| provider::Claims {
        email: None,
        ..valid(login)
    })
    .await;
    assert_eq!(result, Err("/login?error=email".into()));
    let result = sign_in(&app, &provider, |login| {
        provider::Claims::new(&provider, "account-2", "nobody@example.com", &login.nonce)
    })
    .await;
    assert_eq!(result, Err("/login?error=unknown_user".into()));

    // None of that linked anything.
    assert_eq!(
        session_of(&app, Some(&admin)).await["user"]["single_sign_on"],
        json!(false)
    );
    // And the valid login still works, once: a replay of its callback
    // fails at the provider, which takes a code once.
    let login = start_login(&app).await;
    let code = provider.code_for(valid(&login));
    let query = format!("code={code}&state={}", login.state);
    let response = callback(&app, &query, Some(&login.cookie)).await;
    assert!(set_cookie(&response, "organizer_session").is_some());
    refused(
        callback(&app, &query, Some(&login.cookie)).await,
        "provider",
    );
}

/// An OIDC_* variable turns single sign-on on by itself, and its
/// field cannot be changed in the admin settings.
#[tokio::test]
async fn environment_variables_lock_their_fields() {
    let provider = provider::spawn().await;
    let app = spawn_app_with(oidc::Env {
        issuer: Some(provider.issuer.clone()),
        client_id: Some(provider::CLIENT_ID.into()),
        label: Some("Env ID".into()),
        ..oidc::Env::default()
    })
    .await;
    let admin = admin_session(&app).await;
    assert_eq!(
        session_of(&app, None).await["oidc"],
        json!({ "label": "Env ID" })
    );

    let settings = json(
        request(
            &app,
            Method::GET,
            "/api/superadmin/oidc",
            Some(&admin),
            None,
        )
        .await,
    )
    .await;
    assert_eq!(settings["locked"], json!(["issuer", "client_id", "label"]));
    assert_eq!(settings["issuer"], json!(provider.issuer));
    let response = save_oidc(&app, &admin, json!({ "label": "Other" })).await;
    assert_eq!(response.status_code(), 400);
    assert!(
        json(response).await["error"]
            .as_str()
            .unwrap()
            .contains("OIDC_LABEL")
    );
    let response = save_oidc(&app, &admin, json!({ "scopes": "openid email profile" })).await;
    assert_eq!(response.status_code(), 200);
    assert_eq!(
        session_of(&app, None).await["oidc"],
        json!({ "label": "Env ID" })
    );

    // Signing in works on the variables alone.
    let session = sign_in(&app, &provider, |login| {
        provider::Claims::new(&provider, "account-1", "admin@example.com", &login.nonce)
    })
    .await
    .expect("signed in");
    assert_eq!(
        session_of(&app, Some(&session)).await["user"]["email"],
        json!("admin@example.com")
    );
}

/// Before the first admin is created on /setup, single sign-on signs nobody
/// in: there is nobody to sign in as yet, and /setup comes first.
#[tokio::test]
async fn single_sign_on_signs_nobody_in_before_setup() {
    let provider = provider::spawn().await;
    let app = spawn_app_with(oidc::Env {
        issuer: Some(provider.issuer.clone()),
        client_id: Some(provider::CLIENT_ID.into()),
        ..oidc::Env::default()
    })
    .await;
    let result = sign_in(&app, &provider, |login| {
        provider::Claims::new(&provider, "account-1", "someone@example.com", &login.nonce)
    })
    .await;
    assert_eq!(result, Err("/login?error=unavailable".into()));
    assert_eq!(session_of(&app, None).await["needsSetup"], json!(true));
    let response = request(
        &app,
        Method::POST,
        "/api/auth/setup",
        None,
        Some(json!({ "email": "admin@example.com", "password": "correct horse battery" })),
    )
    .await;
    assert_eq!(response.status_code(), 201);
}

/// Two admins saving different fields at once both keep their change, and
/// what is in effect is what is stored.
#[tokio::test]
async fn concurrent_saves_of_the_settings_keep_both_changes() {
    let app = spawn_app().await;
    let admin = admin_session(&app).await;
    let provider = provider::spawn().await;
    configure(&app, &admin, &provider, json!({})).await;
    for round in 0..5 {
        let label = format!("Label {round}");
        let scopes = format!("openid email scope{round}");
        let (a, b) = tokio::join!(
            save_oidc(&app, &admin, json!({ "label": label })),
            save_oidc(&app, &admin, json!({ "scopes": scopes })),
        );
        assert_eq!(
            (a.status_code().as_u16(), b.status_code().as_u16()),
            (200, 200)
        );
        let settings = json(
            request(
                &app,
                Method::GET,
                "/api/superadmin/oidc",
                Some(&admin),
                None,
            )
            .await,
        )
        .await;
        assert_eq!(
            (settings["label"].clone(), settings["scopes"].clone()),
            (json!(label), json!(scopes)),
            "round {round}"
        );
        assert_eq!(session_of(&app, None).await["oidc"]["label"], json!(label));
    }
}

/// A provider that is down is not asked again by every login: one failed
/// lookup answers the logins of the next half minute.
#[tokio::test]
async fn a_provider_that_is_down_is_not_asked_by_every_login() {
    use std::sync::atomic::{AtomicUsize, Ordering};
    let asked = Arc::new(AtomicUsize::new(0));
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let issuer = format!("http://{}", listener.local_addr().unwrap());
    let counter = asked.clone();
    let down = axum::Router::new().fallback(move || {
        counter.fetch_add(1, Ordering::SeqCst);
        async { axum::http::StatusCode::SERVICE_UNAVAILABLE }
    });
    tokio::spawn(async move { axum::serve(listener, down).await.unwrap() });

    let app = spawn_app_with(oidc::Env {
        issuer: Some(issuer),
        client_id: Some(provider::CLIENT_ID.into()),
        ..oidc::Env::default()
    })
    .await;
    admin_session(&app).await;
    for _ in 0..3 {
        let response = app.server.get("/api/auth/oidc/login").await;
        assert_eq!(location(&response), "/login?error=unavailable");
    }
    assert_eq!(asked.load(Ordering::SeqCst), 1);
}

/// The whole `set-cookie` header for a cookie, attributes included.
fn set_cookie_header(response: &Response, name: &str) -> Option<String> {
    response
        .headers()
        .get_all("set-cookie")
        .iter()
        .filter_map(|v| v.to_str().ok())
        .find(|v| v.starts_with(&format!("{name}=")))
        .map(String::from)
}

/// The login cookie goes to the callback only, cannot be read by scripts,
/// survives the provider's cross-site redirect back, and is used up by the
/// callback. Over https it is a `__Host-` cookie, which a sibling subdomain
/// cannot plant.
#[tokio::test]
async fn the_login_cookie_is_scoped_and_used_up() {
    let provider = provider::spawn().await;
    let start = |app: &TestApp| app.server.get("/api/auth/oidc/login");

    // Over plain http (no PUBLIC_URL): a plain cookie, for the callback only.
    let app = spawn_app().await;
    let admin = admin_session(&app).await;
    configure(&app, &admin, &provider, json!({})).await;
    let response = start(&app).await;
    let cookie = set_cookie_header(&response, "organizer_oidc").expect("the login cookie");
    for attribute in [
        "HttpOnly",
        "SameSite=Lax",
        "Path=/api/auth/oidc/callback",
        "Max-Age=600",
    ] {
        assert!(cookie.contains(attribute), "{attribute} in {cookie}");
    }
    assert!(!cookie.contains("Secure"), "{cookie}");

    // Whatever the callback answers, the cookie is gone after it.
    let login = start_login(&app).await;
    for query in [
        format!("state={}&error=access_denied", login.state),
        format!("state={}&code=unknown", login.state),
    ] {
        let response = callback(&app, &query, Some(&login.cookie)).await;
        let cleared = set_cookie_header(&response, "organizer_oidc").expect("the cookie cleared");
        assert!(cleared.starts_with("organizer_oidc=;"), "{cleared}");
        assert!(cleared.contains("Max-Age=0"), "{cleared}");
    }

    // With an https PUBLIC_URL: `__Host-`, Secure, the whole site's path.
    let https = spawn(Options {
        public_url: Some("https://files.example.com".into()),
        ..Options::default()
    })
    .await;
    let admin = secure_admin_session(&https).await;
    configure(&https, &admin, &provider, json!({})).await;
    let response = start(&https).await;
    let header = set_cookie_header(&response, "__Host-organizer_oidc").expect("the login cookie");
    for attribute in ["HttpOnly", "SameSite=Lax", "Path=/;", "Secure"] {
        assert!(header.contains(attribute), "{attribute} in {header}");
    }

    // There, the removal is Secure too (a browser ignores a `__Host-`
    // removal without it); and a cookie this server did not sign (from
    // before a restart, say) is cleared as well.
    let cookie = set_cookie(&response, "__Host-organizer_oidc").expect("the login cookie");
    for cookie in [cookie.as_str(), "__Host-organizer_oidc=not-signed-here"] {
        let response = https
            .server
            .get("/api/auth/oidc/callback?state=x&code=y")
            .add_header("cookie", cookie)
            .await;
        let cleared =
            set_cookie_header(&response, "__Host-organizer_oidc").expect("the cookie cleared");
        for attribute in [
            "__Host-organizer_oidc=;",
            "Max-Age=0",
            "Secure",
            "Path=/;",
            "HttpOnly",
        ] {
            assert!(cleared.contains(attribute), "{attribute} in {cleared}");
        }
    }
}

/// The first admin of an app with an https PUBLIC_URL: its session cookie
/// is `__Host-organizer_session` (as a `cookie` header value).
async fn secure_admin_session(app: &TestApp) -> String {
    let response = request(
        app,
        Method::POST,
        "/api/auth/setup",
        None,
        Some(json!({ "email": "admin@example.com", "password": "correct horse battery" })),
    )
    .await;
    let header =
        set_cookie_header(&response, "__Host-organizer_session").expect("the session cookie");
    for attribute in ["HttpOnly", "SameSite=Strict", "Path=/", "Secure"] {
        assert!(header.contains(attribute), "{attribute} in {header}");
    }
    let session = set_cookie(&response, "__Host-organizer_session").unwrap();
    stepped_up(app, &session, 0).await;
    session
}

/// A second account at the provider with the email of a user who is
/// linked already (a recycled address, say) does not get that user.
#[tokio::test]
async fn a_linked_user_is_not_taken_by_a_second_account() {
    let app = spawn_app().await;
    let admin = admin_session(&app).await;
    let provider = provider::spawn().await;
    configure(&app, &admin, &provider, json!({})).await;
    let as_b = |subject: &'static str| {
        let provider = &provider;
        move |login: &Login| {
            provider::Claims::new(provider, subject, "admin@example.com", &login.nonce)
        }
    };
    assert!(sign_in(&app, &provider, as_b("account-1")).await.is_ok());
    let result = sign_in(&app, &provider, as_b("account-2")).await;
    assert_eq!(result, Err("/login?error=linked".into()));
    assert!(sign_in(&app, &provider, as_b("account-1")).await.is_ok());
}

/// The client secret goes to one provider only: a new issuer without a new
/// secret drops the old one, and a secret from the environment pins the
/// issuer.
#[tokio::test]
async fn a_new_issuer_does_not_get_the_old_secret() {
    let app = spawn_app().await;
    let admin = admin_session(&app).await;
    let first = provider::spawn().await;
    let second = provider::spawn().await;
    configure(&app, &admin, &first, json!({})).await;

    let settings = json(save_oidc(&app, &admin, json!({ "issuer": second.issuer })).await).await;
    assert_eq!(settings["client_secret_set"], json!(false));
    let settings = json(
        save_oidc(
            &app,
            &admin,
            json!({ "issuer": first.issuer, "client_secret": "another secret" }),
        )
        .await,
    )
    .await;
    assert_eq!(settings["client_secret_set"], json!(true));
    // Other changes keep it.
    let settings = json(save_oidc(&app, &admin, json!({ "label": "Acme" })).await).await;
    assert_eq!(settings["client_secret_set"], json!(true));

    let app = spawn_app_with(oidc::Env {
        client_secret: Some("from the environment".into()),
        ..oidc::Env::default()
    })
    .await;
    let admin = admin_session(&app).await;
    let response = save_oidc(
        &app,
        &admin,
        json!({ "issuer": first.issuer, "client_id": provider::CLIENT_ID }),
    )
    .await;
    assert_eq!(response.status_code(), 400);
    assert!(
        json(response).await["error"]
            .as_str()
            .unwrap()
            .contains("OIDC_ISSUER")
    );
}

/// A provider whose discovery document says this, at an address of its
/// own on this machine.
async fn spawn_discovery(document: impl Fn(&str) -> String + Send + 'static) -> String {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let issuer = format!("http://{}", listener.local_addr().unwrap());
    let body = document(&issuer);
    let app = axum::Router::new()
        .route(
            "/.well-known/openid-configuration",
            axum::routing::get(
                move || async move { ([("content-type", "application/json")], body) },
            ),
        )
        .route(
            "/jwks",
            axum::routing::get(|| async { axum::Json(json!({ "keys": [] })) }),
        );
    tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
    issuer
}

/// The provider's own endpoints must use TLS too, and its answers are
/// small: one that is not is refused.
#[tokio::test]
async fn a_provider_must_use_tls_and_answer_briefly() {
    let app = spawn_app().await;
    let admin = admin_session(&app).await;
    let plain_token_endpoint = spawn_discovery(|issuer| {
        json!({
            "issuer": issuer,
            "authorization_endpoint": format!("{issuer}/authorize"),
            "token_endpoint": "http://id.example.com/token",
            "jwks_uri": format!("{issuer}/jwks"),
            "response_types_supported": ["code"],
            "subject_types_supported": ["public"],
            "id_token_signing_alg_values_supported": ["RS256"],
        })
        .to_string()
    })
    .await;
    let response = save_oidc(
        &app,
        &admin,
        json!({ "issuer": plain_token_endpoint, "client_id": "organizer" }),
    )
    .await;
    assert_eq!(response.status_code(), 400);
    let error = json(response).await["error"].as_str().unwrap().to_string();
    assert!(error.contains("http://id.example.com/token"), "{error}");

    let huge = spawn_discovery(|_| format!("{{\"padding\": \"{}\"}}", "x".repeat(2 << 20))).await;
    let response = save_oidc(
        &app,
        &admin,
        json!({ "issuer": huge, "client_id": "organizer" }),
    )
    .await;
    assert_eq!(response.status_code(), 400);
    let error = json(response).await["error"].as_str().unwrap().to_string();
    assert!(error.contains("larger than"), "{error}");
}

/// A provider that rotated its signing key is fetched again at once, not
/// after the hour its keys are kept.
#[tokio::test]
async fn a_rotated_signing_key_is_fetched_at_once() {
    let app = spawn_app().await;
    let admin = admin_session(&app).await;
    let provider = provider::spawn().await;
    configure(&app, &admin, &provider, json!({})).await;
    let valid = |login: &Login| {
        provider::Claims::new(&provider, "account-1", "admin@example.com", &login.nonce)
    };
    assert!(sign_in(&app, &provider, valid).await.is_ok());
    provider.rotate();
    assert!(sign_in(&app, &provider, valid).await.is_ok());
}

/// The same when the provider replaced its key but kept its key id: the
/// cached key then fails the signature instead of being missing.
#[tokio::test]
async fn a_signing_key_replaced_under_the_same_id_is_fetched_at_once() {
    let app = spawn_app().await;
    let admin = admin_session(&app).await;
    let provider = provider::spawn().await;
    configure(&app, &admin, &provider, json!({})).await;
    let valid = |login: &Login| {
        provider::Claims::new(&provider, "account-1", "admin@example.com", &login.nonce)
    };
    assert!(sign_in(&app, &provider, valid).await.is_ok());
    provider.rotate_keeping_id();
    assert!(sign_in(&app, &provider, valid).await.is_ok());
    // A token the fresh keys do not check either is refused.
    let foreign = |login: &Login| provider::Claims {
        foreign_key: true,
        ..valid(login)
    };
    assert_eq!(
        sign_in(&app, &provider, foreign).await,
        Err("/login?error=token".into())
    );
}

/// A login whose callback is on another scheme than the page (http and
/// https) could never finish: it is refused at once, with a reason that
/// points at the setup rather than at the user.
#[tokio::test]
async fn a_callback_on_another_scheme_is_a_misconfiguration() {
    // An https callback, but no https PUBLIC_URL: the login cookie would
    // be a plain one the callback could not be trusted with.
    let provider = provider::spawn().await;
    let callback = json!({ "redirect_url": "https://files.example.com/api/auth/oidc/callback" });
    let plain = spawn_app().await;
    let admin = admin_session(&plain).await;
    configure(&plain, &admin, &provider, callback.clone()).await;
    let start = |app: &TestApp| app.server.get("/api/auth/oidc/login");
    let response = start(&plain).await;
    assert_eq!(location(&response), "/login?error=misconfigured");
    assert_eq!(set_cookie(&response, "organizer_oidc"), None);

    // With PUBLIC_URL on the same scheme it goes to the provider.
    let https = spawn(Options {
        public_url: Some("https://files.example.com".into()),
        ..Options::default()
    })
    .await;
    let admin = secure_admin_session(&https).await;
    configure(&https, &admin, &provider, callback).await;
    let response = start(&https).await;
    assert!(location(&response).starts_with(&provider.issuer));
}

/// The code an authenticator app shows now, `offset` steps away.
fn totp_code(secret: &str, offset: i64) -> String {
    let step = (organizer::totp::current_step() as i64 + offset) as u64;
    organizer::totp::code_at(secret, step).unwrap()
}

/// Sign in with a password: the response, and the cookie it set (the session,
/// or the challenge of the second step).
async fn password_login(app: &TestApp, email: &str) -> Response {
    request(
        app,
        Method::POST,
        "/api/auth/login",
        None,
        Some(json!({ "email": email, "password": "correct horse battery" })),
    )
    .await
}

async fn second_step(app: &TestApp, challenge: Option<&str>, code: &str) -> Response {
    request(
        app,
        Method::POST,
        "/api/auth/login/totp",
        challenge,
        Some(json!({ "code": code })),
    )
    .await
}

async fn post(app: &TestApp, path: &str, session: &str, body: Option<Value>) -> Response {
    request(app, Method::POST, path, Some(session), body).await
}

/// Turn two-factor authentication on for the user of a session: the secret.
async fn enable(app: &TestApp, session: &str) -> String {
    let setup = json(post(app, "/api/auth/totp/setup", session, None).await).await;
    let secret = setup["secret"].as_str().unwrap().to_string();
    let body = json!({ "code": totp_code(&secret, 0) });
    let response = post(app, "/api/auth/totp/enable", session, Some(body)).await;
    assert_eq!(response.status_code(), 200);
    secret
}

#[tokio::test]
async fn two_factor_authentication() {
    let app = spawn_app().await;
    let admin = admin_session(&app).await;

    // Off at first: a password signs in.
    assert_eq!(
        session_of(&app, Some(&admin)).await["user"]["totp_enabled"],
        json!(false)
    );
    assert_eq!(
        password_login(&app, "admin@example.com")
            .await
            .status_code(),
        204
    );

    // Enabling needs a setup first, then a code of its secret.
    let response = post(
        &app,
        "/api/auth/totp/enable",
        &admin,
        Some(json!({ "code": "123456" })),
    )
    .await;
    assert_eq!(response.status_code(), 400);
    let setup = json(post(&app, "/api/auth/totp/setup", &admin, None).await).await;
    let secret = setup["secret"].as_str().unwrap().to_string();
    let uri = setup["uri"].as_str().unwrap();
    assert!(
        uri.starts_with("otpauth://totp/Organizer:admin%40example.com?"),
        "{uri}"
    );
    let wrong = if totp_code(&secret, 0) == "000000" {
        "111111"
    } else {
        "000000"
    };
    let response = post(
        &app,
        "/api/auth/totp/enable",
        &admin,
        Some(json!({ "code": wrong })),
    )
    .await;
    assert_eq!(response.status_code(), 400);
    let response = post(
        &app,
        "/api/auth/totp/enable",
        &admin,
        Some(json!({ "code": totp_code(&secret, 0) })),
    )
    .await;
    assert_eq!(response.status_code(), 200);
    let codes: Vec<String> = serde_json::from_value(json(response).await["codes"].clone()).unwrap();
    assert_eq!(codes.len(), 10);
    assert_eq!(
        session_of(&app, Some(&admin)).await["user"]["totp_enabled"],
        json!(true)
    );
    // On already: no second setup.
    assert_eq!(
        post(&app, "/api/auth/totp/setup", &admin, None)
            .await
            .status_code(),
        409
    );

    // The password alone does not sign in any more.
    let response = password_login(&app, "admin@example.com").await;
    assert_eq!(response.status_code(), 200);
    assert_eq!(set_cookie(&response, auth::COOKIE), None);
    let challenge = set_cookie(&response, "organizer_2fa").expect("the challenge cookie");
    assert_eq!(
        json(response).await,
        json!({ "totp_required": true, "totp_setup_required": false })
    );

    // No challenge, or a forged one: sign in again.
    assert_eq!(
        second_step(&app, None, &totp_code(&secret, 0))
            .await
            .status_code(),
        401
    );
    assert_eq!(
        second_step(
            &app,
            Some("organizer_2fa=1.99999999999"),
            &totp_code(&secret, 0)
        )
        .await
        .status_code(),
        401
    );
    // The code used to enable it (this step) is used up; the next step's
    // code is in the allowed skew and works, once.
    assert_eq!(
        second_step(&app, Some(&challenge), wrong)
            .await
            .status_code(),
        400
    );
    let next = totp_code(&secret, 1);
    let response = second_step(&app, Some(&challenge), &next).await;
    assert_eq!(response.status_code(), 204);
    let session = set_cookie(&response, auth::COOKIE).expect("the session cookie");
    assert_eq!(
        session_of(&app, Some(&session)).await["user"]["email"],
        json!("admin@example.com")
    );
    let response = password_login(&app, "admin@example.com").await;
    let challenge = set_cookie(&response, "organizer_2fa").unwrap();
    assert_eq!(
        second_step(&app, Some(&challenge), &next)
            .await
            .status_code(),
        400
    );
    assert_eq!(
        second_step(&app, Some(&challenge), &totp_code(&secret, -1))
            .await
            .status_code(),
        400
    );

    // A recovery code works once, typed loosely.
    let typed = codes[0].to_uppercase().replace('-', " ");
    assert_eq!(
        second_step(&app, Some(&challenge), &typed)
            .await
            .status_code(),
        204
    );
    assert_eq!(
        second_step(&app, Some(&challenge), &codes[0])
            .await
            .status_code(),
        400
    );

    // New recovery codes replace the old ones.
    let response = post(
        &app,
        "/api/auth/totp/recovery-codes",
        &admin,
        Some(json!({ "code": codes[1] })),
    )
    .await;
    assert_eq!(response.status_code(), 200);
    let new_codes: Vec<String> =
        serde_json::from_value(json(response).await["codes"].clone()).unwrap();
    assert_eq!(
        second_step(&app, Some(&challenge), &codes[2])
            .await
            .status_code(),
        400
    );

    // The new codes work (the limits have their own test).
    assert_eq!(
        second_step(&app, Some(&challenge), &new_codes[0])
            .await
            .status_code(),
        204
    );
}

#[tokio::test]
async fn attempts_are_limited_per_email_address_and_user() {
    // The real limits: 5 per email, 30 per address, 5 codes per user.
    let app = spawn(Options {
        limits: organizer::limits::Limits::default(),
        ..Options::default()
    })
    .await;
    let admin = admin_session(&app).await;
    let attempt = |password: &'static str| {
        request(
            &app,
            Method::POST,
            "/api/auth/login",
            None,
            Some(json!({ "email": "admin@example.com", "password": password })),
        )
    };
    // Right or wrong, every attempt counts: the sixth is refused.
    assert_eq!(attempt("wrong password").await.status_code(), 401);
    for _ in 0..3 {
        assert_eq!(attempt("correct horse battery").await.status_code(), 204);
    }
    assert_eq!(attempt("wrong password").await.status_code(), 401);
    let response = attempt("correct horse battery").await;
    assert_eq!(response.status_code(), 429);
    let wait: u64 = response.headers()["retry-after"]
        .to_str()
        .unwrap()
        .parse()
        .unwrap();
    assert!((1..=60).contains(&wait), "{wait}");
    assert_eq!(
        json(response).await,
        json!({ "error": "Too many attempts, try again later" })
    );
    // Nobody is locked out: the session goes on, and another email can
    // still try (from the same address, under its 30).
    assert!(session_of(&app, Some(&admin)).await["user"].is_object());
    let other = request(
        &app,
        Method::POST,
        "/api/auth/login",
        None,
        Some(json!({ "email": "b@example.com", "password": "wrong password" })),
    )
    .await;
    assert_eq!(other.status_code(), 401);

    // Codes: five per user, then 429 even for a right one.
    let secret = enable(&app, &admin).await;
    for _ in 0..4 {
        let body = json!({ "code": "000000" });
        let response = post(&app, "/api/auth/totp/recovery-codes", &admin, Some(body)).await;
        assert_eq!(response.status_code(), 400);
    }
    let body = json!({ "code": totp_code(&secret, 1) });
    let response = post(&app, "/api/auth/totp/recovery-codes", &admin, Some(body)).await;
    assert_eq!(response.status_code(), 429);
}

/// The user turns two-factor authentication off with a code; a wrong one
/// changes nothing.
#[tokio::test]
async fn two_factor_authentication_is_turned_off_by_the_user() {
    let app = spawn_app().await;
    let user = admin_session(&app).await;
    let secret = enable(&app, &user).await;
    assert_eq!(
        session_of(&app, Some(&user)).await["user"]["totp_enabled"],
        json!(true)
    );
    let response = post(
        &app,
        "/api/auth/totp/disable",
        &user,
        Some(json!({ "code": "abc" })),
    )
    .await;
    assert_eq!(response.status_code(), 400);
    assert_eq!(
        password_login(&app, "admin@example.com")
            .await
            .status_code(),
        200
    );
    let response = post(
        &app,
        "/api/auth/totp/disable",
        &user,
        Some(json!({ "code": totp_code(&secret, 1) })),
    )
    .await;
    assert_eq!(response.status_code(), 204);
    assert_eq!(
        password_login(&app, "admin@example.com")
            .await
            .status_code(),
        204
    );
}

/// Writes into the stored session a second factor confirmed `ago` seconds
/// ago, as `/auth/step-up/…` does: the tests' clock, since a confirmation
/// 16 minutes old cannot be waited for.
async fn stepped_up(app: &TestApp, session: &str, ago: i64) {
    let user = session_of(app, Some(session)).await["user"]["id"]
        .as_str()
        .unwrap()
        .to_string();
    let (_, id) = session.split_once('=').unwrap();
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_secs() as i64;
    let updated = sqlx::query(
        "UPDATE sessions SET data = json_set(data, '$.\"organizer.step_up\"', json(?1))
         WHERE id_hash = ?2",
    )
    .bind(json!({ "user": user, "at": now - ago }).to_string())
    .bind(organizer::util::sha256_hex(id))
    .execute(app.state.db.writer())
    .await
    .unwrap()
    .rows_affected();
    assert_eq!(updated, 1);
}

/// A superadmin endpoint's answer to a session: its status and `code`.
async fn superadmin_answer(app: &TestApp, path: &str, session: &str) -> (u16, Value) {
    let response = request(app, Method::GET, path, Some(session), None).await;
    let status = response.status_code().as_u16();
    let code = if status == 200 {
        Value::Null
    } else {
        json(response).await["code"].clone()
    };
    (status, code)
}

/// The superadmin's endpoints take a session whose second factor was
/// confirmed within 15 minutes (a code of the app, not a recovery code; a
/// sign-in with one counts), and refuse it otherwise, saying which of the
/// two the web must ask for. `/api/admin` still answers, outside the spec.
#[tokio::test]
async fn superadmin_endpoints_need_a_second_factor_confirmed_lately() {
    let app = spawn_app().await;
    let response = request(
        &app,
        Method::POST,
        "/api/auth/setup",
        None,
        Some(json!({ "email": "admin@example.com", "password": "correct horse battery" })),
    )
    .await;
    let fresh = cookie(&response);
    let me = session_of(&app, Some(&fresh)).await;
    assert_eq!(me["user"]["is_superadmin"], json!(true));
    assert!(me["user"].get("is_admin").is_none());
    assert_eq!(
        me["step_up"],
        json!({ "until": null, "totp": false, "passkey": false })
    );

    // Neither a passkey nor the app: add one first.
    assert_eq!(
        superadmin_answer(&app, "/api/superadmin/users", &fresh).await,
        (403, json!("second_factor_required"))
    );

    // With the app on: confirm it first.
    let setup = json(post(&app, "/api/auth/totp/setup", &fresh, None).await).await;
    let secret = setup["secret"].as_str().unwrap().to_string();
    let body = json!({ "code": totp_code(&secret, 0) });
    let enabled = json(post(&app, "/api/auth/totp/enable", &fresh, Some(body)).await).await;
    let recovery = enabled["codes"][0].as_str().unwrap().to_string();
    assert_eq!(
        session_of(&app, Some(&fresh)).await["step_up"]["totp"],
        json!(true)
    );
    for path in ["/api/superadmin/users", "/api/admin/users"] {
        assert_eq!(
            superadmin_answer(&app, path, &fresh).await,
            (403, json!("step_up_required"))
        );
    }

    // A wrong code, or a recovery code, does not confirm it.
    for code in ["123456", recovery.as_str()] {
        let response = post(
            &app,
            "/api/auth/step-up/totp",
            &fresh,
            Some(json!({ "code": code })),
        )
        .await;
        assert_eq!(response.status_code(), 400, "{code}");
    }
    assert_eq!(
        superadmin_answer(&app, "/api/superadmin/users", &fresh)
            .await
            .0,
        403
    );

    // A code of the app does, for 15 minutes; the old path answers too.
    let response = post(
        &app,
        "/api/auth/step-up/totp",
        &fresh,
        Some(json!({ "code": totp_code(&secret, 1) })),
    )
    .await;
    assert_eq!(response.status_code(), 200);
    let until = json(response).await["until"].as_str().unwrap().to_string();
    assert_eq!(
        session_of(&app, Some(&fresh)).await["step_up"]["until"],
        json!(until)
    );
    for path in ["/api/superadmin/users", "/api/admin/users"] {
        assert_eq!(superadmin_answer(&app, path, &fresh).await.0, 200, "{path}");
    }

    // 16 minutes later, it asks again.
    stepped_up(&app, &fresh, 16 * 60).await;
    assert_eq!(
        superadmin_answer(&app, "/api/superadmin/users", &fresh).await,
        (403, json!("step_up_required"))
    );
    assert_eq!(
        session_of(&app, Some(&fresh)).await["step_up"]["until"],
        Value::Null
    );
    stepped_up(&app, &fresh, 14 * 60).await;
    assert_eq!(
        superadmin_answer(&app, "/api/superadmin/users", &fresh)
            .await
            .0,
        200
    );

    // A sign-in with a code of the app is a confirmation; with a recovery
    // code it is not. (A code works once, and the window holds three: the
    // used ones are forgotten to take one more.)
    sqlx::query("UPDATE users SET totp_last_step = 0")
        .execute(app.state.db.writer())
        .await
        .unwrap();
    let login = password_login(&app, "admin@example.com").await;
    let response = second_step(&app, Some(&cookie(&login)), &totp_code(&secret, 0)).await;
    let with_app = set_cookie(&response, auth::COOKIE).unwrap();
    assert_eq!(
        superadmin_answer(&app, "/api/superadmin/users", &with_app)
            .await
            .0,
        200
    );
    let login = password_login(&app, "admin@example.com").await;
    let response = second_step(&app, Some(&cookie(&login)), &recovery).await;
    let with_recovery = set_cookie(&response, auth::COOKIE).unwrap();
    assert_eq!(
        superadmin_answer(&app, "/api/superadmin/users", &with_recovery).await,
        (403, json!("step_up_required"))
    );

    // No token reaches them, not even a superadmin's who stepped up.
    let key = create_key(&app, &with_app, "write").await["token"]
        .as_str()
        .unwrap()
        .to_string();
    for path in ["/api/superadmin/users", "/api/admin/users"] {
        let response = with_key(&app, Method::GET, path, &key, None).await;
        assert_eq!(response.status_code(), 403, "{path}");
    }
    // Nor anyone signed out (the old path is answered, not a 404).
    for path in ["/api/superadmin/users", "/api/admin/users"] {
        let response = request(&app, Method::GET, path, None, None).await;
        assert_eq!(response.status_code(), 401, "{path}");
    }

    // A regular user is no superadmin, stepped up or not.
    let (bea, _) = invited_user(&app, &with_app, "bea@example.com").await;
    assert_eq!(
        session_of(&app, Some(&bea)).await["user"]["is_superadmin"],
        json!(false)
    );
    stepped_up(&app, &bea, 0).await;
    assert_eq!(
        superadmin_answer(&app, "/api/superadmin/users", &bea).await,
        (403, Value::Null)
    );

    // The spec lists the new paths only.
    let spec: Value = serde_json::from_str(&organizer::openapi::spec_json()).unwrap();
    let paths = spec["paths"].as_object().unwrap();
    assert!(paths.keys().any(|p| p.starts_with("/api/superadmin/")));
    assert!(!paths.keys().any(|p| p.starts_with("/api/admin")));
}

/// A passkey confirms it too: one of the user's own, and a sign-in with
/// one counts.
#[tokio::test]
async fn a_passkey_confirms_the_superadmin_again() {
    use webauthn_authenticator_rs::WebauthnAuthenticator;
    use webauthn_authenticator_rs::softpasskey::SoftPasskey;
    use webauthn_rs::prelude::{CreationChallengeResponse, RequestChallengeResponse, Url};

    let origin = "http://localhost:8000";
    let app = spawn(Options {
        public_url: Some(origin.into()),
        ..Options::default()
    })
    .await;
    let admin = admin_session(&app).await;
    let origin = Url::parse(origin).unwrap();
    let mut authenticator = SoftPasskey::new(true);
    // No passkey yet: nothing to start.
    let response = post(&app, "/api/auth/step-up/passkey/start", &admin, None).await;
    assert_eq!(response.status_code(), 400);

    let started = json(post(&app, "/api/auth/passkeys/register", &admin, None).await).await;
    let options: CreationChallengeResponse =
        serde_json::from_value(started["options"].clone()).unwrap();
    let credential = authenticator
        .do_registration(origin.clone(), options)
        .unwrap();
    let body = json!({
        "challenge_id": started["challenge_id"],
        "name": "Laptop",
        "credential": serde_json::to_value(&credential).unwrap(),
    });
    assert_eq!(
        post(&app, "/api/auth/passkeys", &admin, Some(body))
            .await
            .status_code(),
        201
    );
    let me = session_of(&app, Some(&admin)).await;
    assert_eq!(me["step_up"]["passkey"], json!(true));
    assert_eq!(me["step_up"]["totp"], json!(false));

    stepped_up(&app, &admin, 16 * 60).await;
    assert_eq!(
        superadmin_answer(&app, "/api/superadmin/users", &admin).await,
        (403, json!("step_up_required"))
    );
    let started = json(post(&app, "/api/auth/step-up/passkey/start", &admin, None).await).await;
    // Only the user's own passkeys are offered.
    assert_eq!(
        started["options"]["publicKey"]["allowCredentials"]
            .as_array()
            .unwrap()
            .len(),
        1
    );
    let options: RequestChallengeResponse =
        serde_json::from_value(started["options"].clone()).unwrap();
    let assertion = authenticator
        .do_authentication(origin.clone(), options)
        .unwrap();
    let body = json!({
        "challenge_id": started["challenge_id"],
        "credential": serde_json::to_value(&assertion).unwrap(),
    });
    let response = post(
        &app,
        "/api/auth/step-up/passkey",
        &admin,
        Some(body.clone()),
    )
    .await;
    assert_eq!(response.status_code(), 200, "{}", response.text());
    assert!(json(response).await["until"].is_string());
    assert_eq!(
        superadmin_answer(&app, "/api/superadmin/users", &admin)
            .await
            .0,
        200
    );
    // The answer is used once.
    let replay = post(&app, "/api/auth/step-up/passkey", &admin, Some(body)).await;
    assert_eq!(replay.status_code(), 401);
}

/// `organizer two-factor reset` signs the user out everywhere, even when
/// nothing their sessions are checked against changed (passkeys only);
/// `superadmin revoke` takes effect at the next request.
#[tokio::test]
async fn the_recovery_commands_act_at_once() {
    let app = spawn_app().await;
    let admin = admin_session(&app).await;
    let (bea, _) = invited_user(&app, &admin, "bea@example.com").await;
    organizer::recovery::grant(&app.state.db, "bea@example.com")
        .await
        .unwrap();
    stepped_up(&app, &bea, 0).await;
    assert_eq!(
        superadmin_answer(&app, "/api/superadmin/users", &bea)
            .await
            .0,
        200
    );
    organizer::recovery::revoke(&app.state.db, "bea@example.com")
        .await
        .unwrap();
    assert_eq!(
        superadmin_answer(&app, "/api/superadmin/users", &bea)
            .await
            .0,
        403
    );

    organizer::recovery::reset_two_factor(&app.state.db, "admin@example.com")
        .await
        .unwrap();
    assert!(session_of(&app, Some(&admin)).await["user"].is_null());
    assert!(session_of(&app, Some(&bea)).await["user"].is_object());
}

#[tokio::test]
async fn single_sign_on_only_refuses_passwords() {
    let provider = provider::spawn().await;
    let app = spawn_app_with(oidc::Env {
        issuer: Some(provider.issuer.clone()),
        client_id: Some(provider::CLIENT_ID.into()),
        only: true,
        ..oidc::Env::default()
    })
    .await;
    // The first admin is still made on /setup, with a password: nobody signs
    // in through the provider before there is one.
    let admin = admin_session(&app).await;
    let session = session_of(&app, None).await;
    assert_eq!(session["password_login"], json!(false));
    assert!(session["oidc"].is_object());

    // No password signs in, not even the right one.
    let response = password_login(&app, "admin@example.com").await;
    assert_eq!(response.status_code(), 403);
    assert_eq!(set_cookie(&response, auth::COOKIE), None);
    // The code step is for single sign-on too: without its cookie, a 401.
    assert_eq!(second_step(&app, None, "123456").await.status_code(), 401);

    // Passwords cannot be set.
    let body = json!({ "new_password": "another horse battery", "current_password": "correct horse battery" });
    assert_eq!(
        post(&app, "/api/auth/password", &admin, Some(body))
            .await
            .status_code(),
        403
    );

    // Single sign-on signs the user in.
    let session = sign_in(&app, &provider, |login| {
        provider::Claims::new(&provider, "account-1", "admin@example.com", &login.nonce)
    })
    .await
    .expect("signed in");
    assert_eq!(
        session_of(&app, Some(&session)).await["user"]["email"],
        json!("admin@example.com")
    );
}

/// A request with an API key instead of the session.
async fn with_key(
    app: &TestApp,
    method: Method,
    path: &str,
    key: &str,
    body: Option<Value>,
) -> Response {
    let mut builder = app
        .server
        .method(method, path)
        .add_header("authorization", format!("Bearer {key}"));
    if let Some(body) = body {
        builder = builder.json(&body);
    }
    builder.await
}

async fn create_key(app: &TestApp, session: &str, scope: &str) -> Value {
    let body = json!({ "name": format!("{scope} key"), "scope": scope });
    let response = post(app, "/api/auth/api-keys", session, Some(body)).await;
    assert_eq!(response.status_code(), 201);
    json(response).await
}

#[tokio::test]
async fn api_keys_are_capped_by_their_scope() {
    let app = spawn_app().await;
    let admin = admin_session(&app).await;

    let read = create_key(&app, &admin, "read").await;
    let token = read["token"].as_str().unwrap().to_string();
    assert!(token.starts_with("org_") && token.len() == 68);
    assert_eq!(read["key"]["prefix"], json!(&token[..10]));
    assert_eq!(read["key"]["scope"], json!("read"));
    let write = create_key(&app, &admin, "write").await["token"]
        .as_str()
        .unwrap()
        .to_string();
    // The old `admin` scope is taken as `write`.
    let old_admin = create_key(&app, &admin, "admin").await;
    assert_eq!(old_admin["key"]["scope"], json!("write"));
    let admin_key = old_admin["token"].as_str().unwrap().to_string();

    // Read: GET only, so never a change, and no admin endpoints.
    let label = json!({ "label": "Company account" });
    let response = with_key(
        &app,
        Method::PUT,
        "/api/superadmin/oidc",
        &token,
        Some(label.clone()),
    )
    .await;
    assert_eq!(response.status_code(), 403);
    assert_eq!(
        json(response).await["error"],
        json!("This API key can only read")
    );
    assert_eq!(
        with_key(&app, Method::GET, "/api/superadmin/oidc", &token, None)
            .await
            .status_code(),
        403
    );
    // Write: changes, but no superadmin endpoints; nor a key that was an
    // admin key, of a superadmin who stepped up.
    for key in [&write, &admin_key] {
        assert_eq!(
            with_key(&app, Method::GET, "/api/superadmin/oidc", key, None)
                .await
                .status_code(),
            403
        );
        assert_eq!(
            with_key(
                &app,
                Method::PUT,
                "/api/superadmin/oidc",
                key,
                Some(label.clone())
            )
            .await
            .status_code(),
            403
        );
    }

    // No key reaches the account: not its keys, password or 2FA.
    for key in [&token, &write, &admin_key] {
        assert_eq!(
            with_key(&app, Method::GET, "/api/auth/api-keys", key, None)
                .await
                .status_code(),
            403
        );
        let body = json!({ "name": "more", "scope": "read" });
        assert_eq!(
            with_key(&app, Method::POST, "/api/auth/api-keys", key, Some(body))
                .await
                .status_code(),
            403
        );
        assert_eq!(
            with_key(&app, Method::POST, "/api/auth/totp/setup", key, None)
                .await
                .status_code(),
            403
        );
    }

    // Used: the list says when; the token is never listed.
    let keys =
        json(request(&app, Method::GET, "/api/auth/api-keys", Some(&admin), None).await).await;
    assert_eq!(keys.as_array().unwrap().len(), 3);
    assert!(keys[2]["last_used_at"].is_string());
    assert!(!keys.to_string().contains(&token));

    // Revoked, unknown or malformed: 401.
    let id = read["key"]["id"].as_str().unwrap().to_string();
    let path = format!("/api/auth/api-keys/{id}");
    assert_eq!(
        request(&app, Method::DELETE, &path, Some(&admin), None)
            .await
            .status_code(),
        204
    );
    assert_eq!(
        request(&app, Method::DELETE, &path, Some(&admin), None)
            .await
            .status_code(),
        404
    );
    for key in [token.as_str(), "org_nope", "nope"] {
        assert_eq!(
            with_key(&app, Method::GET, "/api/superadmin/oidc", key, None)
                .await
                .status_code(),
            401,
            "{key}"
        );
    }

    // Bad requests.
    for body in [
        json!({ "name": "", "scope": "read" }),
        json!({ "name": "x", "scope": "owner" }),
        json!({ "name": "x", "scope": "read", "expires_in_days": 0 }),
    ] {
        assert_eq!(
            post(&app, "/api/auth/api-keys", &admin, Some(body))
                .await
                .status_code(),
            400
        );
    }
    let body = json!({ "name": "x", "scope": "read", "expires_in_days": 30 });
    let key = json(post(&app, "/api/auth/api-keys", &admin, Some(body)).await).await;
    assert!(key["key"]["expires_at"].is_string());

    // Devices are made by the apps' sign-in only.
    let body = json!({ "name": "x", "scope": "write", "kind": "device" });
    assert_eq!(
        post(&app, "/api/auth/api-keys", &admin, Some(body))
            .await
            .status_code(),
        400
    );
    // An app password is listed as one, and is no bearer token: it is the
    // password of WebDAV's Basic authentication, and only that.
    let body = json!({ "name": "rclone", "scope": "admin", "kind": "app_password" });
    let created = json(post(&app, "/api/auth/api-keys", &admin, Some(body)).await).await;
    assert_eq!(created["key"]["kind"], json!("app_password"));
    let app_password = created["token"].as_str().unwrap().to_string();
    assert_eq!(
        with_key(
            &app,
            Method::GET,
            "/api/superadmin/oidc",
            &app_password,
            None
        )
        .await
        .status_code(),
        401
    );
}

/// A single sign-on login that stops at a step: where it sent the browser,
/// and the challenge cookie.
async fn sso_step(app: &TestApp, provider: &provider::Provider, email: &str) -> (String, String) {
    let login = start_login(app).await;
    let code = provider.code_for(provider::Claims::new(provider, email, email, &login.nonce));
    let response = callback(
        app,
        &format!("code={code}&state={}", login.state),
        Some(&login.cookie),
    )
    .await;
    assert_eq!(response.status_code(), 303);
    assert_eq!(set_cookie(&response, auth::COOKIE), None);
    let challenge = set_cookie(&response, "organizer_2fa").expect("the challenge cookie");
    (location(&response), challenge)
}

#[tokio::test]
async fn single_sign_on_asks_for_the_code_too() {
    let app = spawn_app().await;
    let admin = admin_session(&app).await;
    let provider = provider::spawn().await;
    configure(&app, &admin, &provider, json!({})).await;
    let secret = enable(&app, &admin).await;

    let (page, challenge) = sso_step(&app, &provider, "admin@example.com").await;
    assert_eq!(page, "/login?step=totp");
    // Not the enrolment step: that is for a user without the app.
    let response = request(
        &app,
        Method::POST,
        "/api/auth/login/totp/setup",
        Some(&challenge),
        None,
    )
    .await;
    assert_eq!(response.status_code(), 401);
    let response = second_step(&app, Some(&challenge), &totp_code(&secret, 1)).await;
    assert_eq!(response.status_code(), 204);
    let session = set_cookie(&response, auth::COOKIE).expect("the session cookie");
    assert_eq!(
        session_of(&app, Some(&session)).await["user"]["email"],
        json!("admin@example.com")
    );
}

#[tokio::test]
async fn totp_required_makes_everyone_set_it_up() {
    let provider = provider::spawn().await;
    let app = spawn_app_full(
        oidc::Env {
            issuer: Some(provider.issuer.clone()),
            client_id: Some(provider::CLIENT_ID.into()),
            ..oidc::Env::default()
        },
        true,
    )
    .await;
    // The session /setup made is not touched.
    let admin = admin_session(&app).await;
    assert_eq!(session_of(&app, None).await["totp_required"], json!(true));

    // The password leads to setting the app up, not to a session.
    let response = password_login(&app, "admin@example.com").await;
    assert_eq!(response.status_code(), 200);
    assert_eq!(set_cookie(&response, auth::COOKIE), None);
    let challenge = set_cookie(&response, "organizer_2fa").unwrap();
    assert_eq!(
        json(response).await,
        json!({ "totp_required": false, "totp_setup_required": true })
    );
    // Not the code step: there is no app yet.
    assert_eq!(
        second_step(&app, Some(&challenge), "123456")
            .await
            .status_code(),
        401
    );
    let enroll = |path: &'static str, body: Option<Value>| {
        request(&app, Method::POST, path, Some(&challenge), body)
    };
    let response = enroll(
        "/api/auth/login/totp/enable",
        Some(json!({ "code": "123456" })),
    )
    .await;
    assert_eq!(response.status_code(), 400);
    let setup = json(enroll("/api/auth/login/totp/setup", None).await).await;
    let secret = setup["secret"].as_str().unwrap().to_string();
    let body = json!({ "code": totp_code(&secret, 0) });
    let response = enroll("/api/auth/login/totp/enable", Some(body)).await;
    assert_eq!(response.status_code(), 200);
    let session = set_cookie(&response, auth::COOKIE).expect("the session cookie");
    assert_eq!(json(response).await["codes"].as_array().unwrap().len(), 10);
    assert_eq!(
        session_of(&app, Some(&session)).await["user"]["totp_enabled"],
        json!(true)
    );
    // Turning it on changed the user's credentials: the session /setup
    // made is signed out.
    assert_eq!(session_of(&app, Some(&admin)).await["user"], Value::Null);
    let admin = session;
    // It cannot be turned off.
    let body = json!({ "code": totp_code(&secret, 1) });
    assert_eq!(
        post(&app, "/api/auth/totp/disable", &admin, Some(body))
            .await
            .status_code(),
        403
    );

    // Single sign-on asks for the code of the app now set up.
    let (page, _) = sso_step(&app, &provider, "admin@example.com").await;
    assert_eq!(page, "/login?step=totp");
}

#[tokio::test]
async fn the_authenticator_app_can_be_replaced() {
    let app = spawn_app_full(oidc::Env::default(), true).await;
    let admin = admin_session(&app).await;
    // With TOTP_REQUIRED it cannot be turned off, but it can be replaced.
    let old = enable(&app, &admin).await;
    let key = create_key(&app, &admin, "admin").await["token"]
        .as_str()
        .unwrap()
        .to_string();
    let body = json!({ "code": totp_code(&old, 1) });
    let response = with_key(
        &app,
        Method::POST,
        "/api/auth/totp/replace",
        &key,
        Some(body),
    )
    .await;
    assert_eq!(response.status_code(), 403);

    // Confirming needs a replacement started, which needs a current code.
    let body = json!({ "code": "123456" });
    assert_eq!(
        post(&app, "/api/auth/totp/replace/confirm", &admin, Some(body))
            .await
            .status_code(),
        400
    );
    let body = json!({ "code": "abc" });
    assert_eq!(
        post(&app, "/api/auth/totp/replace", &admin, Some(body))
            .await
            .status_code(),
        400
    );
    let body = json!({ "code": totp_code(&old, 1) });
    let response = post(&app, "/api/auth/totp/replace", &admin, Some(body)).await;
    assert_eq!(response.status_code(), 200);
    let new = json(response).await["secret"].as_str().unwrap().to_string();
    assert_ne!(new, old);

    // A wrong code from the new app changes nothing.
    let wrong = if totp_code(&new, 0) == "000000" {
        "111111"
    } else {
        "000000"
    };
    let body = json!({ "code": wrong });
    assert_eq!(
        post(&app, "/api/auth/totp/replace/confirm", &admin, Some(body))
            .await
            .status_code(),
        400
    );
    // Confirmed: the new app works, the old one does not.
    let body = json!({ "code": totp_code(&new, 0) });
    assert_eq!(
        post(&app, "/api/auth/totp/replace/confirm", &admin, Some(body))
            .await
            .status_code(),
        204
    );
    let response = password_login(&app, "admin@example.com").await;
    let challenge = set_cookie(&response, "organizer_2fa").unwrap();
    let old_code = totp_code(&old, 1);
    if old_code != totp_code(&new, 1) {
        assert_eq!(
            second_step(&app, Some(&challenge), &old_code)
                .await
                .status_code(),
            400
        );
    }
    let response = second_step(&app, Some(&challenge), &totp_code(&new, 1)).await;
    assert_eq!(response.status_code(), 204);
}

#[tokio::test]
async fn sessions_end_with_a_password_change_or_a_logout() {
    let app = spawn_app().await;
    let setup = admin_session(&app).await;
    let laptop = cookie(&password_login(&app, "admin@example.com").await);
    let phone = cookie(&password_login(&app, "admin@example.com").await);
    assert_ne!(laptop, phone);
    let signed_in = |session: String| {
        let app = &app;
        async move { session_of(app, Some(&session)).await["user"] != Value::Null }
    };
    assert!(signed_in(laptop.clone()).await && signed_in(phone.clone()).await);

    // A new password signs the other sessions out; this one stays.
    let body = json!({ "current_password": "correct horse battery", "new_password": "another horse battery" });
    assert_eq!(
        post(&app, "/api/auth/password", &laptop, Some(body))
            .await
            .status_code(),
        204
    );
    assert!(signed_in(laptop.clone()).await);
    assert!(!signed_in(phone).await);
    assert!(!signed_in(setup).await);

    // Signing out ends the session.
    assert_eq!(
        post(&app, "/api/auth/logout", &laptop, None)
            .await
            .status_code(),
        204
    );
    assert!(!signed_in(laptop).await);
}

/// A POST as a browser on another origin would send it.
async fn from_site(app: &TestApp, path: &str, session: Option<&str>, site: &str) -> Response {
    let mut builder = app
        .server
        .post(path)
        .add_header("sec-fetch-site", site)
        .json(&json!({}));
    if let Some(session) = session {
        builder = builder.add_header("cookie", session);
    }
    builder.await
}

#[tokio::test]
async fn requests_from_other_origins_are_refused() {
    let app = spawn_app().await;
    let admin = admin_session(&app).await;
    let new_key = json!({ "name": "x", "scope": "read" });

    // A sibling subdomain (same-site) or another site cannot change
    // anything, signed in or not; the login and logout included.
    for site in ["same-site", "cross-site"] {
        for path in ["/api/auth/api-keys", "/api/auth/logout", "/api/auth/login"] {
            let response = from_site(&app, path, Some(&admin), site).await;
            assert_eq!(response.status_code(), 403, "{site} {path}");
        }
    }
    assert!(session_of(&app, Some(&admin)).await["user"].is_object());

    // A multipart upload (no CORS preflight) from a sibling subdomain is
    // refused before anything reads it.
    let form = "--b\r\nContent-Disposition: form-data; name=\"file\"\r\n\r\nhello\r\n--b--\r\n";
    let response = app
        .server
        .post("/api/auth/api-keys")
        .add_header("cookie", &admin)
        .add_header("sec-fetch-site", "same-site")
        .add_header("content-type", "multipart/form-data; boundary=b")
        .bytes(form.into())
        .await;
    assert_eq!(response.status_code(), 403);

    // Without Fetch Metadata, the Origin decides.
    let response = app
        .server
        .post("/api/auth/api-keys")
        .add_header("cookie", &admin)
        .add_header("origin", "https://blog.example.com")
        .json(&new_key)
        .await;
    assert_eq!(response.status_code(), 403);

    // Organizer's own pages and API keys pass.
    let response = app
        .server
        .post("/api/auth/api-keys")
        .add_header("cookie", &admin)
        .add_header("sec-fetch-site", "same-origin")
        .add_header("origin", &app.base)
        .json(&new_key)
        .await;
    assert_eq!(response.status_code(), 201);
    let key = create_key(&app, &admin, "write").await["token"]
        .as_str()
        .unwrap()
        .to_string();
    let response = app
        .server
        .put("/api/notifications/settings")
        .add_header("authorization", format!("Bearer {key}"))
        .add_header("sec-fetch-site", "cross-site")
        .json(&json!({ "review": true, "shared": false, "backup": true }))
        .await;
    assert_eq!(response.status_code(), 200);
}

/// A folder of its own in the temporary directory, removed with the value.
struct TempDir(std::path::PathBuf);

impl TempDir {
    fn new() -> Self {
        let dir = std::env::temp_dir().join(format!("organizer-share-{}", uuid::Uuid::now_v7()));
        std::fs::create_dir_all(&dir).unwrap();
        TempDir(dir.canonicalize().unwrap())
    }
}

impl Drop for TempDir {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

/// The files folder: the default, one chosen at setup (a share that already
/// holds files), changed by the admin. STORAGE_URL is only the base the
/// default is under; it locks nothing.
#[tokio::test]
async fn the_files_folder_is_chosen_at_setup_and_changed_by_the_admin() {
    let share = TempDir::new();
    std::fs::write(share.0.join("existing.txt"), "already here").unwrap();
    let app = spawn_app().await;
    let setup = |files_root: &str| {
        request(
            &app,
            Method::POST,
            "/api/auth/setup",
            None,
            Some(json!({
                "email": "admin@example.com",
                "password": "correct horse battery",
                "files_root": files_root,
            })),
        )
    };
    // Not a folder Organizer can use: nothing is set up.
    for bad in ["relative/path", "/nonexistent/organizer/share"] {
        assert_eq!(setup(bad).await.status_code(), 400, "{bad}");
    }
    assert_eq!(session_of(&app, None).await["needsSetup"], json!(true));
    let admin = cookie(&setup(share.0.to_str().unwrap()).await);
    stepped_up(&app, &admin, 0).await;
    let storage = json(
        request(
            &app,
            Method::GET,
            "/api/superadmin/storage",
            Some(&admin),
            None,
        )
        .await,
    )
    .await;
    assert_eq!(
        storage,
        json!({
            "files_root": share.0.to_str().unwrap(),
            "default": false,
            "available": true,
        })
    );

    // Back to the default, then another share.
    let set = |root: Value| {
        request(
            &app,
            Method::PATCH,
            "/api/superadmin/storage",
            Some(&admin),
            Some(json!({ "files_root": root })),
        )
    };
    let storage = json(set(Value::Null).await).await;
    assert_eq!(storage["default"], json!(true));
    let other = TempDir::new();
    let storage = json(set(json!(other.0.to_str().unwrap())).await).await;
    assert_eq!(storage["files_root"], json!(other.0.to_str().unwrap()));
    assert_eq!(set(json!("/nonexistent/x")).await.status_code(), 400);
    assert_eq!(
        request(&app, Method::GET, "/api/superadmin/storage", None, None)
            .await
            .status_code(),
        401
    );

    // STORAGE_URL is the base of the default folder, and locks nothing: the
    // admin can still choose another one.
    let base = TempDir::new();
    let app = spawn(Options {
        storage_url: Some(base.0.clone()),
        ..Options::default()
    })
    .await;
    let admin = admin_session(&app).await;
    let storage = json(
        request(
            &app,
            Method::GET,
            "/api/superadmin/storage",
            Some(&admin),
            None,
        )
        .await,
    )
    .await;
    let users = users_by_email(&app, &admin).await;
    let admin_id = users["admin@example.com"]["id"].as_str().unwrap();
    let default_root = base
        .0
        .join("users")
        .join(admin_id)
        .join("files")
        .to_str()
        .unwrap()
        .to_string();
    assert_eq!(storage["default"], json!(true));
    assert_eq!(storage["files_root"], json!(default_root));
    let response = request(
        &app,
        Method::PATCH,
        "/api/superadmin/storage",
        Some(&admin),
        Some(json!({ "files_root": share.0.to_str().unwrap() })),
    )
    .await;
    assert_eq!(response.status_code(), 200);
}

/// Set up with a files folder that already holds files, and wait until it
/// is indexed. Answers the admin's session and the share.
async fn with_files<C: AsRef<[u8]>>(app: &TestApp, files: &[(&str, C)]) -> (String, TempDir) {
    let share = TempDir::new();
    for (path, content) in files {
        let path = share.0.join(path);
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(path, content).unwrap();
    }
    let response = request(
        app,
        Method::POST,
        "/api/auth/setup",
        None,
        Some(json!({
            "email": "admin@example.com",
            "password": "correct horse battery",
            "files_root": share.0.to_str().unwrap(),
        })),
    )
    .await;
    assert_eq!(response.status_code(), 201);
    let session = cookie(&response);
    stepped_up(app, &session, 0).await;
    // The index is made in the background; the root's listing shows it.
    for _ in 0..100 {
        let root =
            json(request(app, Method::GET, "/api/files/root", Some(&session), None).await).await;
        let path = format!("/api/files/{}/children", root["id"].as_str().unwrap());
        let children = json(request(app, Method::GET, &path, Some(&session), None).await).await;
        if children["items"].as_array().unwrap().len() >= files.len().min(1) {
            break;
        }
        tokio::time::sleep(std::time::Duration::from_millis(20)).await;
    }
    (session, share)
}

async fn by_path(app: &TestApp, session: &str, path: &str) -> Response {
    request(
        app,
        Method::GET,
        &format!("/api/files/by-path?path={}", urlencoding(path)),
        Some(session),
        None,
    )
    .await
}

fn multipart(name: &str, content: &str) -> (String, Vec<u8>) {
    let body = format!(
        "--b\r\nContent-Disposition: form-data; name=\"file\"; filename=\"{name}\"\r\nContent-Type: text/plain\r\n\r\n{content}\r\n--b--\r\n"
    );
    ("multipart/form-data; boundary=b".into(), body.into_bytes())
}

/// Browse by id and by path, make a folder, move, download (with ranges and
/// ETags), upload, replace at a version, delete to the trash and restore.
#[tokio::test]
async fn files_are_browsed_and_changed_over_the_api() {
    let app = spawn_app().await;
    let (admin, share) = with_files(
        &app,
        &[("Docs/a.txt", "hello world"), ("photo.jpg", "jpeg")],
    )
    .await;
    let get = |path: String| {
        let app = &app;
        let admin = &admin;
        async move { request(app, Method::GET, &path, Some(admin), None).await }
    };

    let root = json(get("/api/files/root".into()).await).await;
    assert_eq!(root["path"], json!("/"));
    assert_eq!(root["kind"], json!("folder"));
    let root_id = root["id"].as_str().unwrap().to_string();
    let children = json(get(format!("/api/files/{root_id}/children")).await).await;
    let names: Vec<_> = children["items"]
        .as_array()
        .unwrap()
        .iter()
        .map(|n| n["name"].as_str().unwrap())
        .collect();
    assert_eq!(names, ["Docs", "photo.jpg"]);
    assert_eq!(children["next"], Value::Null);

    let a = json(by_path(&app, &admin, "/Docs/a.txt").await).await;
    assert_eq!(a["path"], json!("/Docs/a.txt"));
    assert_eq!(a["size"], json!(11));
    assert_eq!(a["mime"], json!("text/plain"));
    let a_id = a["id"].as_str().unwrap().to_string();
    assert_eq!(by_path(&app, &admin, "/Docs/nope").await.status_code(), 404);

    // A folder, and the file moved into it.
    let response = request(
        &app,
        Method::POST,
        &format!("/api/files/{root_id}/folders"),
        Some(&admin),
        Some(json!({ "name": "Archive" })),
    )
    .await;
    assert_eq!(response.status_code(), 201);
    let archive = json(response).await["id"].as_str().unwrap().to_string();
    assert!(share.0.join("Archive").is_dir());
    let response = request(
        &app,
        Method::PATCH,
        &format!("/api/files/{a_id}"),
        Some(&admin),
        Some(json!({ "parent_id": archive, "if_version": a["version"] })),
    )
    .await;
    assert_eq!(response.status_code(), 200);
    assert!(share.0.join("Archive/a.txt").is_file());
    let a = json(get(format!("/api/files/{a_id}")).await).await;
    assert_eq!(a["path"], json!("/Archive/a.txt"));

    // Download: whole, a range, and not again while unchanged.
    let content = format!("/api/files/{a_id}/content");
    let response = get(content.clone()).await;
    assert_eq!(response.status_code(), 200);
    assert_eq!(response.text(), "hello world");
    let etag = response.headers()["etag"].to_str().unwrap().to_string();
    assert_eq!(etag, format!("\"{}\"", a["version"]));
    assert!(
        response.headers()["content-disposition"]
            .to_str()
            .unwrap()
            .starts_with("attachment; filename=\"a.txt\"")
    );
    let response = app
        .server
        .get(&content)
        .add_header("cookie", &admin)
        .add_header("range", "bytes=6-10")
        .await;
    assert_eq!(response.status_code(), 206);
    assert_eq!(response.text(), "world");
    let response = app
        .server
        .get(&content)
        .add_header("cookie", &admin)
        .add_header("if-none-match", &etag)
        .await;
    assert_eq!(response.status_code(), 304);

    // Upload a new file; a taken name is a conflict.
    let (content_type, body) = multipart("b.txt", "bee");
    let upload = format!("/api/files/{archive}/files");
    let response = app
        .server
        .post(&upload)
        .add_header("cookie", &admin)
        .add_header("content-type", &content_type)
        .bytes(body.clone().into())
        .await;
    assert_eq!(response.status_code(), 201);
    let b = json(response).await;
    assert_eq!(
        std::fs::read_to_string(share.0.join("Archive/b.txt")).unwrap(),
        "bee"
    );
    let response = app
        .server
        .post(&upload)
        .add_header("cookie", &admin)
        .add_header("content-type", &content_type)
        .bytes(body.into())
        .await;
    assert_eq!(response.status_code(), 409);

    // Replace: If-Match is required, and must be the current version.
    let b_content = format!("/api/files/{}/content", b["id"].as_str().unwrap());
    let put = |if_match: Option<String>| {
        let mut request = app
            .server
            .put(&b_content)
            .add_header("cookie", &admin)
            .bytes("buzz".into());
        if let Some(if_match) = if_match {
            request = request.add_header("if-match", if_match);
        }
        request
    };
    assert_eq!(put(None).await.status_code(), 428);
    assert_eq!(put(Some("\"99\"".into())).await.status_code(), 409);
    let response = put(Some(format!("\"{}\"", b["version"]))).await;
    assert_eq!(response.status_code(), 200);
    let b = json(response).await;
    assert_eq!(b["version"], json!(2));
    assert_eq!(b["size"], json!(4));
    assert_eq!(
        std::fs::read_to_string(share.0.join("Archive/b.txt")).unwrap(),
        "buzz"
    );

    // Delete to the trash, restore, delete again and purge.
    let b_id = b["id"].as_str().unwrap().to_string();
    let delete = format!("/api/files/{b_id}?if_version=2");
    let response = request(&app, Method::DELETE, &delete, Some(&admin), None).await;
    assert_eq!(response.status_code(), 200);
    assert!(!share.0.join("Archive/b.txt").exists());
    let trash = json(get("/api/trash".into()).await).await;
    assert_eq!(trash[0]["original_path"], json!("Archive/b.txt"));
    let response = request(
        &app,
        Method::POST,
        &format!("/api/trash/{b_id}/restore"),
        Some(&admin),
        None,
    )
    .await;
    assert_eq!(response.status_code(), 200);
    assert!(share.0.join("Archive/b.txt").is_file());
    let b = json(get(format!("/api/files/{b_id}")).await).await;
    let delete = format!("/api/files/{b_id}?if_version={}", b["version"]);
    assert_eq!(
        request(&app, Method::DELETE, &delete, Some(&admin), None)
            .await
            .status_code(),
        200
    );
    let purge = format!("/api/trash/{b_id}");
    assert_eq!(
        request(&app, Method::DELETE, &purge, Some(&admin), None)
            .await
            .status_code(),
        204
    );
    assert_eq!(json(get("/api/trash".into()).await).await, json!([]));

    // A read-only key reads, and changes nothing.
    let key = create_key(&app, &admin, "read").await["token"]
        .as_str()
        .unwrap()
        .to_string();
    assert_eq!(
        with_key(&app, Method::GET, &format!("/api/files/{a_id}"), &key, None)
            .await
            .status_code(),
        200
    );
    assert_eq!(
        with_key(
            &app,
            Method::POST,
            &format!("/api/files/{root_id}/folders"),
            &key,
            Some(json!({ "name": "x" }))
        )
        .await
        .status_code(),
        403
    );
    // Nobody signed in: nothing.
    assert_eq!(
        request(&app, Method::GET, "/api/files/root", None, None)
            .await
            .status_code(),
        401
    );
}

/// A folder's children come in pages: folders first, then by name.
#[tokio::test]
async fn children_are_paged() {
    let app = spawn_app().await;
    let (admin, _share) = with_files(
        &app,
        &[
            ("e.txt", ""),
            ("d.txt", ""),
            ("B/x", ""),
            ("A/x", ""),
            ("c.txt", ""),
        ],
    )
    .await;
    let root = json(request(&app, Method::GET, "/api/files/root", Some(&admin), None).await).await;
    let base = format!(
        "/api/files/{}/children?limit=2",
        root["id"].as_str().unwrap()
    );
    let mut names = Vec::new();
    let mut url = base.clone();
    loop {
        let page = json(request(&app, Method::GET, &url, Some(&admin), None).await).await;
        names.extend(
            page["items"]
                .as_array()
                .unwrap()
                .iter()
                .map(|n| n["name"].as_str().unwrap().to_string()),
        );
        match page["next"].as_str() {
            Some(next) => url = format!("{base}&after={next}"),
            None => break,
        }
    }
    assert_eq!(names, ["A", "B", "c.txt", "d.txt", "e.txt"]);
}

/// The change feed: the cursor of now, then what changed since; a cursor
/// from before the files folder changed is gone (410).
#[tokio::test]
async fn the_change_feed_lists_what_changed_since_a_cursor() {
    let app = spawn_app().await;
    let (admin, _share) = with_files(&app, &[("a.txt", "a")]).await;
    let feed = |cursor: Option<&str>| {
        let url = match cursor {
            Some(cursor) => format!("/api/changes?cursor={cursor}"),
            None => "/api/changes".into(),
        };
        let app = &app;
        let admin = &admin;
        async move { request(app, Method::GET, &url, Some(admin), None).await }
    };
    let now = json(feed(None).await).await;
    assert_eq!(now["changes"], json!([]));
    let cursor = now["cursor"].as_str().unwrap().to_string();

    let root = json(request(&app, Method::GET, "/api/files/root", Some(&admin), None).await).await;
    let folder = json(
        request(
            &app,
            Method::POST,
            &format!("/api/files/{}/folders", root["id"].as_str().unwrap()),
            Some(&admin),
            Some(json!({ "name": "New" })),
        )
        .await,
    )
    .await;
    // Renamed twice: one change in the feed, the last.
    for (name, version) in [("Newer", 1), ("Newest", 2)] {
        request(
            &app,
            Method::PATCH,
            &format!("/api/files/{}", folder["id"].as_str().unwrap()),
            Some(&admin),
            Some(json!({ "name": name, "if_version": version })),
        )
        .await;
    }
    let page = json(feed(Some(&cursor)).await).await;
    assert_eq!(
        page["changes"],
        json!([{
            "node_id": folder["id"],
            "drive_id": root["drive_id"],
            "op": "move",
            "kind": "folder",
            "parent_id": root["id"],
            "name": "Newest",
            "version": 3,
            "e2e_root": null,
        }])
    );
    assert_eq!(page["has_more"], json!(false));
    let next = page["cursor"].as_str().unwrap().to_string();
    assert_eq!(json(feed(Some(&next)).await).await["changes"], json!([]));
    assert_eq!(feed(Some("x")).await.status_code(), 400);

    // Another files folder: every old cursor is gone.
    let other = TempDir::new();
    request(
        &app,
        Method::PATCH,
        "/api/superadmin/storage",
        Some(&admin),
        Some(json!({ "files_root": other.0.to_str().unwrap() })),
    )
    .await;
    assert_eq!(feed(Some(&next)).await.status_code(), 410);
}

/// A resumable upload in pieces (interop version 6): created with the
/// first bytes, the offset asked, a wrong offset refused with the right
/// one, the rest appended, and the file placed whole.
#[tokio::test]
async fn resumable_uploads_continue_where_they_stopped() {
    let app = spawn_app().await;
    let (admin, share) = with_files(&app, &[("Docs/x.txt", "x")]).await;
    let docs = json(by_path(&app, &admin, "/Docs").await).await;
    let docs_id = docs["id"].as_str().unwrap();

    let response = app
        .server
        .post(&format!("/api/uploads?parent_id={docs_id}&name=big.bin"))
        .add_header("cookie", &admin)
        .add_header("upload-complete", "?0")
        .add_header("upload-draft-interop-version", "6")
        .bytes("hello ".into())
        .await;
    assert_eq!(response.status_code(), 201);
    assert_eq!(response.headers()["upload-complete"], "?0");
    assert_eq!(response.headers()["upload-offset"], "6");
    assert_eq!(response.headers()["upload-draft-interop-version"], "6");
    let location = response.headers()["location"].to_str().unwrap().to_string();
    assert!(location.starts_with("/api/uploads/"));
    // Not in the tree until it is complete.
    assert!(!share.0.join("Docs/big.bin").exists());

    let response = app
        .server
        .method(Method::HEAD, &location)
        .add_header("cookie", &admin)
        .await;
    assert_eq!(response.status_code(), 204);
    assert_eq!(response.headers()["upload-offset"], "6");
    assert_eq!(response.headers()["cache-control"], "no-store");

    let append = |offset: &str, complete: &str, body: &'static str| {
        app.server
            .patch(&location)
            .add_header("cookie", &admin)
            .add_header("content-type", "application/partial-upload")
            .add_header("upload-offset", offset)
            .add_header("upload-complete", complete)
            .bytes(body.into())
    };
    // Another offset: 409, with the right one.
    let response = append("3", "?0", "nope").await;
    assert_eq!(response.status_code(), 409);
    assert_eq!(response.headers()["upload-offset"], "6");
    // Without the media type: 415.
    let response = app
        .server
        .patch(&location)
        .add_header("cookie", &admin)
        .add_header("upload-offset", "6")
        .add_header("upload-complete", "?0")
        .bytes("x".into())
        .await;
    assert_eq!(response.status_code(), 415);

    let response = append("6", "?0", "resumable ").await;
    assert_eq!(response.status_code(), 204);
    assert_eq!(response.headers()["upload-offset"], "16");
    let response = append("16", "?1", "world").await;
    assert_eq!(response.status_code(), 201);
    assert_eq!(response.headers()["upload-complete"], "?1");
    let node = json(response).await;
    assert_eq!(node["name"], json!("big.bin"));
    assert_eq!(node["size"], json!(21));
    assert_eq!(
        std::fs::read_to_string(share.0.join("Docs/big.bin")).unwrap(),
        "hello resumable world"
    );
    // Done: the upload is gone.
    let response = app
        .server
        .method(Method::HEAD, &location)
        .add_header("cookie", &admin)
        .await;
    assert_eq!(response.status_code(), 404);
}

/// An upload in one request, one that replaces a file at its version, one
/// for a name that is taken, and a cancelled one.
#[tokio::test]
async fn resumable_uploads_create_replace_and_cancel() {
    let app = spawn_app().await;
    let (admin, share) = with_files(&app, &[("a.txt", "old")]).await;
    let root = json(request(&app, Method::GET, "/api/files/root", Some(&admin), None).await).await;
    let root_id = root["id"].as_str().unwrap().to_string();
    let a = json(by_path(&app, &admin, "/a.txt").await).await;

    let create = |query: String, complete: &'static str, body: &'static str| {
        app.server
            .post(&format!("/api/uploads?{query}"))
            .add_header("cookie", &admin)
            .add_header("upload-complete", complete)
            .bytes(body.into())
    };
    // Whole in one request.
    let response = create(format!("parent_id={root_id}&name=b.txt"), "?1", "bee").await;
    assert_eq!(response.status_code(), 201);
    assert_eq!(
        std::fs::read_to_string(share.0.join("b.txt")).unwrap(),
        "bee"
    );
    // Replacing a file at its version; then at a stale one.
    let query = format!(
        "replaces={}&if_version={}",
        a["id"].as_str().unwrap(),
        a["version"]
    );
    let response = create(query.clone(), "?1", "new").await;
    assert_eq!(response.status_code(), 201);
    assert_eq!(json(response).await["version"], json!(2));
    assert_eq!(
        std::fs::read_to_string(share.0.join("a.txt")).unwrap(),
        "new"
    );
    assert_eq!(create(query, "?1", "newer").await.status_code(), 409);
    // A taken name is refused before anything is uploaded; so is a bad one.
    let taken = create(format!("parent_id={root_id}&name=B.TXT"), "?0", "").await;
    assert_eq!(taken.status_code(), 409);
    let bad = create(format!("parent_id={root_id}&name=a:b"), "?0", "").await;
    assert_eq!(bad.status_code(), 400);
    // No Upload-Complete: 400.
    let response = app
        .server
        .post(&format!("/api/uploads?parent_id={root_id}&name=c.txt"))
        .add_header("cookie", &admin)
        .bytes("c".into())
        .await;
    assert_eq!(response.status_code(), 400);

    // Cancelled: what was received is gone.
    let response = create(format!("parent_id={root_id}&name=c.txt"), "?0", "partial").await;
    let location = response.headers()["location"].to_str().unwrap().to_string();
    let response = app
        .server
        .delete(&location)
        .add_header("cookie", &admin)
        .await;
    assert_eq!(response.status_code(), 204);
    let response = app
        .server
        .method(Method::HEAD, &location)
        .add_header("cookie", &admin)
        .await;
    assert_eq!(response.status_code(), 404);
    assert!(!share.0.join("c.txt").exists());
}

/// A resumable upload to the drop box: created with its note and
/// read-contents choice, broken off, resumed at the offset the server has,
/// and made an item as a multipart drop would (the user's read-contents
/// setting applied to the choice).
#[tokio::test]
async fn resumable_uploads_go_to_the_drop_box() {
    let app = spawn_app().await;
    let admin = admin_session(&app).await;
    let mut settings = json(
        request(
            &app,
            Method::GET,
            "/api/drop-box/settings",
            Some(&admin),
            None,
        )
        .await,
    )
    .await;
    settings.as_object_mut().unwrap().remove("recipient");
    settings["read_contents"] = json!("default_on");
    assert_eq!(
        put(&app, "/api/drop-box/settings", &admin, settings)
            .await
            .status_code(),
        200
    );

    let response = app
        .server
        .post("/api/uploads?drop_box=true&name=scan.pdf&note=%20for%20the%20taxes%20&read_contents=false")
        .add_header("cookie", &admin)
        .add_header("upload-complete", "?0")
        .add_header("upload-length", "21")
        .bytes("hello ".into())
        .await;
    assert_eq!(response.status_code(), 201);
    let location = response.headers()["location"].to_str().unwrap().to_string();
    // Nothing in the drop box until it is complete.
    let list =
        json(request(&app, Method::GET, "/api/drop-box/items", Some(&admin), None).await).await;
    assert_eq!(list, json!([]));

    let append = |offset: &str, complete: &str, body: &'static str| {
        app.server
            .patch(&location)
            .add_header("cookie", &admin)
            .add_header("content-type", "application/partial-upload")
            .add_header("upload-offset", offset)
            .add_header("upload-complete", complete)
            .bytes(body.into())
    };
    assert_eq!(append("6", "?0", "resumable ").await.status_code(), 204);
    // The connection broke off: the client asks where the upload is, and a
    // stale offset is answered with the right one.
    let response = app
        .server
        .method(Method::HEAD, &location)
        .add_header("cookie", &admin)
        .await;
    assert_eq!(response.headers()["upload-offset"], "16");
    let response = append("6", "?1", "resumable world").await;
    assert_eq!(response.status_code(), 409);
    assert_eq!(response.headers()["upload-offset"], "16");

    let response = append("16", "?1", "world").await;
    assert_eq!(response.status_code(), 201);
    assert_eq!(response.headers()["upload-complete"], "?1");
    let item = json(response).await;
    assert_eq!(item["kind"], "file");
    assert_eq!(item["name"], "scan.pdf");
    assert_eq!(item["note"], "for the taxes");
    assert_eq!(item["size"], 21);
    assert_eq!(item["mime_type"], "application/pdf");
    assert_eq!(item["read_contents"], false);
    let item = settled(&app, &admin, item["id"].as_str().unwrap()).await;
    assert_eq!(item["status"], "waiting");
    // Done: the upload is gone.
    let response = app
        .server
        .method(Method::HEAD, &location)
        .add_header("cookie", &admin)
        .await;
    assert_eq!(response.status_code(), 404);

    // In one request, without a choice: the setting's default (on).
    let response = app
        .server
        .post("/api/uploads?drop_box=true&name=notes.txt")
        .add_header("cookie", &admin)
        .add_header("upload-complete", "?1")
        .bytes("some notes".into())
        .await;
    assert_eq!(response.status_code(), 201);
    let item = json(response).await;
    assert_eq!(item["name"], "notes.txt");
    assert_eq!(item["read_contents"], true);
    assert_eq!(item["note"], json!(null));
    let list =
        json(request(&app, Method::GET, "/api/drop-box/items", Some(&admin), None).await).await;
    assert_eq!(list.as_array().unwrap().len(), 2);
}

/// What a drop-box upload is refused for, before anything is uploaded: no
/// name or a bad one, a folder or a file with it, a note too long, and the
/// drop box's options without the drop box.
#[tokio::test]
async fn resumable_drop_box_uploads_refuse_what_does_not_fit() {
    let app = spawn_app().await;
    let (admin, _share) = with_files(&app, &[("a.txt", "a")]).await;
    let root = json(request(&app, Method::GET, "/api/files/root", Some(&admin), None).await).await;
    let root_id = root["id"].as_str().unwrap().to_string();
    let a = json(by_path(&app, &admin, "/a.txt").await).await;
    let a_id = a["id"].as_str().unwrap().to_string();
    let long = "x".repeat(2_001);

    for query in [
        "drop_box=true".to_string(),
        "drop_box=true&name=a:b".to_string(),
        "drop_box=true&name=.DS_Store".to_string(),
        format!("drop_box=true&name=b.txt&parent_id={root_id}"),
        format!("drop_box=true&name=b.txt&replaces={a_id}&if_version=1"),
        format!("drop_box=true&replaces={a_id}&if_version=1"),
        format!("drop_box=true&name=b.txt&note={long}"),
        "drop_box=true&name=b.txt&read_contents=maybe".to_string(),
        format!("parent_id={root_id}&name=b.txt&note=hi"),
        format!("parent_id={root_id}&name=b.txt&read_contents=true"),
        format!("replaces={a_id}&if_version=1&note=hi"),
        "name=b.txt&note=hi".to_string(),
    ] {
        let response = app
            .server
            .post(&format!("/api/uploads?{query}"))
            .add_header("cookie", &admin)
            .add_header("upload-complete", "?1")
            .bytes("b".into())
            .await;
        assert_eq!(response.status_code(), 400, "{query}");
    }
    // `drop_box=false` is the folder upload as before.
    let response = app
        .server
        .post(&format!(
            "/api/uploads?drop_box=false&parent_id={root_id}&name=b.txt"
        ))
        .add_header("cookie", &admin)
        .add_header("upload-complete", "?1")
        .bytes("b".into())
        .await;
    assert_eq!(response.status_code(), 201);
    assert_eq!(json(response).await["name"], "b.txt");

    // A read-only key cannot drop.
    let key = create_key(&app, &admin, "read").await;
    let response = app
        .server
        .post("/api/uploads?drop_box=true&name=c.txt")
        .add_header(
            "authorization",
            format!("Bearer {}", key["token"].as_str().unwrap()),
        )
        .add_header("upload-complete", "?1")
        .bytes("c".into())
        .await;
    assert_eq!(response.status_code(), 403);

    let list =
        json(request(&app, Method::GET, "/api/drop-box/items", Some(&admin), None).await).await;
    assert_eq!(list, json!([]));
}

/// An app signs in with a password: one call, the code of the
/// authenticator app in it when the user has one. The token is a device's.
#[tokio::test]
async fn a_device_signs_in_with_a_password() {
    let app = spawn_app().await;
    let admin = admin_session(&app).await;
    let sign_in = |code: Option<String>| {
        let mut body = json!({
            "email": "admin@example.com",
            "password": "correct horse battery",
            "device_name": "Anna's iPhone",
        });
        if let Some(code) = code {
            body["code"] = json!(code);
        }
        request(
            &app,
            Method::POST,
            "/api/auth/device/password",
            None,
            Some(body),
        )
    };
    let response = sign_in(None).await;
    assert_eq!(response.status_code(), 200);
    let created = json(response).await;
    assert_eq!(created["key"]["kind"], json!("device"));
    assert_eq!(created["key"]["scope"], json!("write"));
    assert_eq!(created["key"]["name"], json!("Anna's iPhone"));
    let token = created["token"].as_str().unwrap().to_string();
    assert_eq!(
        with_key(&app, Method::GET, "/api/files/root", &token, None)
            .await
            .status_code(),
        200
    );
    // A device is no admin.
    assert_eq!(
        with_key(&app, Method::GET, "/api/superadmin/oidc", &token, None)
            .await
            .status_code(),
        403
    );
    let wrong = json!({ "email": "admin@example.com", "password": "nope", "device_name": "x" });
    assert_eq!(
        request(
            &app,
            Method::POST,
            "/api/auth/device/password",
            None,
            Some(wrong)
        )
        .await
        .status_code(),
        401
    );

    // With two-factor authentication on: the code is asked for.
    let secret = enable(&app, &admin).await;
    assert_eq!(
        json(sign_in(None).await).await,
        json!({ "totp_required": true })
    );
    assert_eq!(sign_in(Some("000000".into())).await.status_code(), 401);
    let response = sign_in(Some(totp_code(&secret, 1))).await;
    assert_eq!(response.status_code(), 200);
    assert!(json(response).await["token"].is_string());
}

/// An app signs in through the browser: approved by a signed-in user, a
/// one-time code to the app's redirect, exchanged with the PKCE verifier.
#[tokio::test]
async fn a_device_signs_in_through_the_browser() {
    let app = spawn_app().await;
    // The challenge of RFC 7636's example verifier.
    let verifier = "dBjftJeZ4CVP-mB92K27uhbUJU1p1r_wW1gFWFOEjXk";
    let challenge = "E9Melhoa2OwvFrEMTJguCHaoeK1t8URWbuGJSstw-cM";
    let query = format!(
        "redirect_uri={}&code_challenge={challenge}&device_name=Laptop",
        urlencoding("organizer://auth")
    );
    let start = format!("/api/auth/device/authorize?{query}");

    // Not signed in: the login page first, then the approval page.
    let response = app.server.get(&start).await;
    assert_eq!(response.status_code(), 303);
    let approval = format!("/settings/devices/approve?{query}");
    assert_eq!(
        location(&response),
        format!("/login?next={}", urlencoding(&approval))
    );
    let admin = admin_session(&app).await;
    let response = app.server.get(&start).add_header("cookie", &admin).await;
    assert_eq!(location(&response), approval);
    // Another redirect: refused.
    let bad = format!(
        "/api/auth/device/authorize?redirect_uri={}&code_challenge={challenge}&device_name=x",
        urlencoding("https://evil.example.com/")
    );
    assert_eq!(app.server.get(&bad).await.status_code(), 400);

    let approve = || {
        request(
            &app,
            Method::POST,
            "/api/auth/device/authorize",
            Some(&admin),
            Some(json!({
                "redirect_uri": "organizer://auth",
                "code_challenge": challenge,
                "device_name": "Laptop",
            })),
        )
    };
    let code_of = |approved: Value| {
        approved["redirect"]
            .as_str()
            .unwrap()
            .strip_prefix("organizer://auth?code=")
            .expect("the app's redirect with the code")
            .to_string()
    };
    let exchange = |code: String, verifier: &'static str| {
        request(
            &app,
            Method::POST,
            "/api/auth/device/token",
            None,
            Some(json!({ "code": code, "code_verifier": verifier })),
        )
    };
    // The wrong verifier: refused, and the code is used up.
    let code = code_of(json(approve().await).await);
    assert_eq!(
        exchange(code.clone(), "wrong-verifier").await.status_code(),
        400
    );
    assert_eq!(exchange(code, verifier).await.status_code(), 400);
    // The right one: the device's token, once.
    let code = code_of(json(approve().await).await);
    let response = exchange(code.clone(), verifier).await;
    assert_eq!(response.status_code(), 200);
    let created = json(response).await;
    assert_eq!(created["key"]["kind"], json!("device"));
    assert_eq!(created["key"]["name"], json!("Laptop"));
    assert_eq!(exchange(code, verifier).await.status_code(), 400);
    // An API key cannot approve a device.
    let token = created["token"].as_str().unwrap().to_string();
    let body = json!({ "redirect_uri": "organizer://auth", "code_challenge": challenge, "device_name": "x" });
    assert_eq!(
        with_key(
            &app,
            Method::POST,
            "/api/auth/device/authorize",
            &token,
            Some(body)
        )
        .await
        .status_code(),
        403
    );
}

/// Single sign-on goes on to the page it was started from (an app's
/// approval page, an invite's accept dialog), never to another origin; so
/// does its two-factor step.
#[tokio::test]
async fn single_sign_on_goes_on_to_the_page_it_came_from() {
    let app = spawn_app().await;
    let admin = admin_session(&app).await;
    let provider = provider::spawn().await;
    configure(&app, &admin, &provider, json!({})).await;
    let contacts = "/settings/contacts?token=abc&providerDomain=cloud.example.com";
    for (next, expected) in [
        (
            "/settings/devices/approve?redirect_uri=organizer%3A%2F%2Fauth",
            "/settings/devices/approve?redirect_uri=organizer%3A%2F%2Fauth",
        ),
        (contacts, contacts),
        ("https://evil.example.com/", "/"),
        ("//evil.example.com/", "/"),
        ("/\\evil.example.com/", "/"),
    ] {
        let login = start_login_at(
            &app,
            &format!("/api/auth/oidc/login?next={}", urlencoding(next)),
        )
        .await;
        let code = provider.code_for(provider::Claims::new(
            &provider,
            "account-1",
            "admin@example.com",
            &login.nonce,
        ));
        let response = callback(
            &app,
            &format!("code={code}&state={}", login.state),
            Some(&login.cookie),
        )
        .await;
        assert_eq!(location(&response), expected, "{next}");
    }

    // With two-factor authentication, the login page's step carries it on.
    enable(&app, &admin).await;
    for (next, expected) in [
        (
            contacts,
            format!("/login?step=totp&next={}", urlencoding(contacts)),
        ),
        ("//evil.example.com/", "/login?step=totp".to_string()),
    ] {
        let login = start_login_at(
            &app,
            &format!("/api/auth/oidc/login?next={}", urlencoding(next)),
        )
        .await;
        let code = provider.code_for(provider::Claims::new(
            &provider,
            "account-1",
            "admin@example.com",
            &login.nonce,
        ));
        let response = callback(
            &app,
            &format!("code={code}&state={}", login.state),
            Some(&login.cookie),
        )
        .await;
        assert_eq!(location(&response), expected, "{next}");
    }
}

/// Basic authentication with a password, for WebDAV.
fn basic(password: &str) -> String {
    const ALPHABET: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
    let input = format!("me:{password}");
    let bytes = input.as_bytes();
    let mut out = String::new();
    for chunk in bytes.chunks(3) {
        let b = [
            chunk[0],
            *chunk.get(1).unwrap_or(&0),
            *chunk.get(2).unwrap_or(&0),
        ];
        let n = (b[0] as u32) << 16 | (b[1] as u32) << 8 | b[2] as u32;
        for i in 0..4 {
            if i <= chunk.len() {
                out.push(ALPHABET[(n >> (18 - 6 * i) & 63) as usize] as char);
            } else {
                out.push('=');
            }
        }
    }
    format!("Basic {out}")
}

/// WebDAV with an app password: listing, PUT, MKCOL, MOVE (the id stays),
/// COPY, overwrite, and DELETE to the trash, all in the index.
#[tokio::test]
async fn webdav_changes_go_through_the_index() {
    let app = spawn_app().await;
    let (admin, share) = with_files(&app, &[("Docs/a.txt", "hello")]).await;
    let body = json!({ "name": "rclone", "scope": "write", "kind": "app_password" });
    let password = json(post(&app, "/api/auth/api-keys", &admin, Some(body)).await).await["token"]
        .as_str()
        .unwrap()
        .to_string();
    let auth = basic(&password);
    let dav = |method: &str, path: &str| {
        app.server
            .method(Method::from_bytes(method.as_bytes()).unwrap(), path)
            .add_header("authorization", &auth)
    };

    // Who may: an app password; not nobody, not an API key.
    assert_eq!(app.server.get("/dav/").await.status_code(), 401);
    let key = create_key(&app, &admin, "write").await["token"]
        .as_str()
        .unwrap()
        .to_string();
    let response = app
        .server
        .get("/dav/Docs/a.txt")
        .add_header("authorization", basic(&key))
        .await;
    assert_eq!(response.status_code(), 401);

    let response = dav("PROPFIND", "/dav/").add_header("depth", "1").await;
    assert_eq!(response.status_code(), 207);
    assert!(response.text().contains("Docs"));
    assert_eq!(dav("GET", "/dav/Docs/a.txt").await.text(), "hello");

    // PUT a new file: on disk and in the index.
    let response = dav("PUT", "/dav/Docs/new.txt").bytes("fresh".into()).await;
    assert_eq!(response.status_code(), 201);
    assert_eq!(
        std::fs::read_to_string(share.0.join("Docs/new.txt")).unwrap(),
        "fresh"
    );
    let new = json(by_path(&app, &admin, "/Docs/new.txt").await).await;
    assert_eq!(new["size"], json!(5));
    // A decomposed name (Finder's) is stored in NFC.
    let response = dav("PUT", "/dav/Cafe%CC%81.txt").bytes("x".into()).await;
    assert_eq!(response.status_code(), 201);
    assert_eq!(by_path(&app, &admin, "/Café.txt").await.status_code(), 200);

    // MKCOL, then MOVE into it: the same id.
    assert_eq!(dav("MKCOL", "/dav/Folder").await.status_code(), 201);
    assert!(share.0.join("Folder").is_dir());
    let response = dav("MOVE", "/dav/Docs/new.txt")
        .add_header("destination", format!("{}/dav/Folder/moved.txt", app.base))
        .await;
    assert_eq!(response.status_code(), 201);
    let moved = json(by_path(&app, &admin, "/Folder/moved.txt").await).await;
    assert_eq!(moved["id"], new["id"]);

    // Overwrite: a new version of the same file.
    let response = dav("PUT", "/dav/Folder/moved.txt")
        .bytes("changed".into())
        .await;
    assert_eq!(response.status_code(), 204);
    let changed = json(by_path(&app, &admin, "/Folder/moved.txt").await).await;
    assert_eq!(changed["id"], new["id"]);
    assert_eq!(
        changed["version"],
        json!(moved["version"].as_i64().unwrap() + 1)
    );

    // COPY: indexed.
    let response = dav("COPY", "/dav/Folder/moved.txt")
        .add_header("destination", format!("{}/dav/copy.txt", app.base))
        .await;
    assert_eq!(response.status_code(), 201);
    assert_eq!(by_path(&app, &admin, "/copy.txt").await.status_code(), 200);

    // DELETE: to the trash, not gone.
    assert_eq!(dav("DELETE", "/dav/Folder").await.status_code(), 204);
    assert!(!share.0.join("Folder").exists());
    let trash = json(request(&app, Method::GET, "/api/trash", Some(&admin), None).await).await;
    assert_eq!(trash[0]["name"], json!("Folder"));

    // A read-only app password reads, and changes nothing.
    let body = json!({ "name": "viewer", "scope": "read", "kind": "app_password" });
    let reader = json(post(&app, "/api/auth/api-keys", &admin, Some(body)).await).await["token"]
        .as_str()
        .unwrap()
        .to_string();
    let response = app
        .server
        .put("/dav/nope.txt")
        .add_header("authorization", basic(&reader))
        .bytes("x".into())
        .await;
    assert_eq!(response.status_code(), 403);
}

/// A stand-in for an OpenAI-compatible model server (`/v1/chat/completions`,
/// with images as vision models get them, `/v1/embeddings` and
/// `/v1/audio/transcriptions`), which keeps the `Authorization` header of
/// every request it gets.
mod model_server {
    use axum::Json;
    use axum::extract::Multipart;
    use axum::http::HeaderMap;
    use axum::routing::post;
    use serde_json::{Value, json};
    use std::collections::VecDeque;
    use std::sync::{Arc, Mutex};

    type Shared<T> = Arc<Mutex<T>>;

    pub struct ModelServer {
        /// The base URL to set as the endpoint, e.g. `http://127.0.0.1:41234/v1`.
        pub endpoint: String,
        /// The `Authorization` header of each request, `None` without one.
        pub auth: Shared<Vec<Option<String>>>,
        /// The answers to give, in order; "OK" once they are used up.
        pub answers: Shared<VecDeque<String>>,
        /// The last message of each chat request without an image: what the
        /// model was told.
        pub prompts: Shared<Vec<String>>,
        /// The answers to a request with an image, in order; a description
        /// once they are used up.
        pub sights: Shared<VecDeque<String>>,
        /// The image of each request with one: its `data:` URL's type and the
        /// length of its base64.
        pub images: Shared<Vec<(String, usize)>>,
        /// Each transcription asked for: the file's name, its length, the
        /// model.
        pub recordings: Shared<Vec<(String, usize, String)>>,
    }

    impl ModelServer {
        pub fn answer(&self, answer: serde_json::Value) {
            self.answers.lock().unwrap().push_back(answer.to_string());
        }

        pub fn answer_text(&self, answer: &str) {
            self.answers.lock().unwrap().push_back(answer.to_string());
        }

        /// What the next image is said to show.
        pub fn see(&self, answer: &str) {
            self.sights.lock().unwrap().push_back(answer.to_string());
        }

        pub fn images(&self) -> Vec<(String, usize)> {
            self.images.lock().unwrap().clone()
        }

        pub fn recordings(&self) -> Vec<(String, usize, String)> {
            self.recordings.lock().unwrap().clone()
        }
    }

    /// A stand-in for meaning: how many words of each group a text has
    /// (teeth, money, travel), and a constant.
    pub fn meaning(text: &str) -> Vec<f32> {
        let groups: [&[&str]; 3] = [
            &["teeth", "tooth", "dentist", "dental"],
            &["money", "bank", "account", "invoice"],
            &["travel", "flight", "train", "ticket"],
        ];
        let words: Vec<String> = text
            .to_lowercase()
            .split(|c: char| !c.is_alphanumeric())
            .map(String::from)
            .collect();
        let mut v: Vec<f32> = groups
            .iter()
            .map(|g| words.iter().filter(|w| g.contains(&w.as_str())).count() as f32)
            .collect();
        v.push(0.1);
        v
    }

    /// The text and the image (its `data:` URL) of a message's content.
    fn parts(content: &Value) -> (String, Option<String>) {
        match content {
            Value::String(text) => (text.clone(), None),
            Value::Array(parts) => {
                let text = parts
                    .iter()
                    .filter_map(|p| p["text"].as_str())
                    .collect::<Vec<_>>()
                    .join("\n");
                let image = parts
                    .iter()
                    .find_map(|p| p["image_url"]["url"].as_str().map(String::from));
                (text, image)
            }
            _ => (String::new(), None),
        }
    }

    pub async fn spawn() -> ModelServer {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let endpoint = format!("http://{}/v1", listener.local_addr().unwrap());
        let auth: Shared<Vec<Option<String>>> = Arc::default();
        let seen = |auth: Shared<Vec<Option<String>>>| {
            move |headers: &HeaderMap| {
                auth.lock().unwrap().push(
                    headers
                        .get("authorization")
                        .map(|v| v.to_str().unwrap().to_string()),
                );
            }
        };
        let (chat_seen, embed_seen, audio_seen) =
            (seen(auth.clone()), seen(auth.clone()), seen(auth.clone()));
        let answers: Shared<VecDeque<String>> = Arc::default();
        let prompts: Shared<Vec<String>> = Arc::default();
        let sights: Shared<VecDeque<String>> = Arc::default();
        let images: Shared<Vec<(String, usize)>> = Arc::default();
        // The words of every transcription.
        let transcript: Shared<String> = Arc::new(Mutex::new("Hello from the recording".into()));
        let recordings: Shared<Vec<(String, usize, String)>> = Arc::default();
        let (chat_answers, chat_prompts, chat_sights, chat_images) = (
            answers.clone(),
            prompts.clone(),
            sights.clone(),
            images.clone(),
        );
        let (audio_transcript, audio_recordings) = (transcript.clone(), recordings.clone());
        let router = axum::Router::new()
            .route(
                "/v1/chat/completions",
                post(move |headers: HeaderMap, Json(body): Json<Value>| async move {
                    chat_seen(&headers);
                    let (last, image) = body["messages"]
                        .as_array()
                        .and_then(|m| m.last())
                        .map(|m| parts(&m["content"]))
                        .unwrap_or_default();
                    let answer = match image {
                        Some(url) => {
                            let (kind, data) = url
                                .strip_prefix("data:")
                                .and_then(|u| u.split_once(";base64,"))
                                .unwrap_or(("", ""));
                            chat_images
                                .lock()
                                .unwrap()
                                .push((kind.to_string(), data.len()));
                            chat_sights
                                .lock()
                                .unwrap()
                                .pop_front()
                                .unwrap_or_else(|| "Description: a picture".into())
                        }
                        None => {
                            chat_prompts.lock().unwrap().push(last);
                            chat_answers
                                .lock()
                                .unwrap()
                                .pop_front()
                                .unwrap_or_else(|| "OK".into())
                        }
                    };
                    Json(json!({
                        "id": "chatcmpl-1",
                        "object": "chat.completion",
                        "created": 0,
                        "model": body["model"],
                        "choices": [{
                            "index": 0,
                            "message": { "role": "assistant", "content": answer },
                            "finish_reason": "stop"
                        }],
                        "usage": { "prompt_tokens": 5, "completion_tokens": 1, "total_tokens": 6 }
                    }))
                }),
            )
            .route(
                "/v1/embeddings",
                post(move |headers: HeaderMap, Json(body): Json<Value>| async move {
                    embed_seen(&headers);
                    let inputs: Vec<String> = match &body["input"] {
                        Value::Array(inputs) => inputs
                            .iter()
                            .map(|i| i.as_str().unwrap_or_default().to_string())
                            .collect(),
                        other => vec![other.as_str().unwrap_or_default().to_string()],
                    };
                    let data: Vec<Value> = inputs
                        .iter()
                        .enumerate()
                        .map(|(i, text)| json!({ "object": "embedding", "index": i, "embedding": meaning(text) }))
                        .collect();
                    Json(json!({
                        "object": "list",
                        "data": data,
                        "model": body["model"],
                        "usage": { "prompt_tokens": 1, "total_tokens": 1 }
                    }))
                }),
            )
            .route(
                "/v1/audio/transcriptions",
                post(move |headers: HeaderMap, mut form: Multipart| async move {
                    audio_seen(&headers);
                    let (mut name, mut length, mut model) = (String::new(), 0, String::new());
                    while let Some(field) = form.next_field().await.unwrap() {
                        match field.name() {
                            Some("file") => {
                                name = field.file_name().unwrap_or_default().to_string();
                                length = field.bytes().await.unwrap().len();
                            }
                            Some("model") => model = field.text().await.unwrap(),
                            _ => {}
                        }
                    }
                    audio_recordings.lock().unwrap().push((name, length, model));
                    let text = audio_transcript.lock().unwrap().clone();
                    // A second segment after the first hour, which is not kept.
                    Json(json!({
                        "text": text,
                        "segments": [
                            { "start": 0.0, "end": 2.0, "text": text },
                            { "start": 3700.0, "end": 3702.0, "text": "after the first hour" }
                        ]
                    }))
                }),
            );
        tokio::spawn(async move { axum::serve(listener, router).await.unwrap() });
        ModelServer {
            endpoint,
            auth,
            answers,
            prompts,
            sights,
            images,
            recordings,
        }
    }
}

async fn put(app: &TestApp, path: &str, session: &str, body: Value) -> Response {
    request(app, Method::PUT, path, Some(session), Some(body)).await
}

/// Register a model at a server, as an admin; answers it.
async fn add_model(
    app: &TestApp,
    session: &str,
    server: &model_server::ModelServer,
    name: &str,
    capabilities: &[&str],
) -> Value {
    let response = post(
        app,
        "/api/superadmin/ai/models",
        session,
        Some(json!({
            "name": name,
            "provider": "openai",
            "endpoint": server.endpoint,
            "model": name.to_lowercase(),
            "capabilities": capabilities,
        })),
    )
    .await;
    let status = response.status_code();
    let model = json(response).await;
    assert_eq!(status, 201, "{model}");
    model
}

/// Choose the models of some work, as an admin; answers the settings.
async fn choose(app: &TestApp, session: &str, choices: Value) -> Value {
    let response = put(app, "/api/superadmin/ai", session, choices).await;
    let status = response.status_code();
    let settings = json(response).await;
    assert_eq!(status, 200, "{settings}");
    settings
}

/// Models are registered once they answered, for each thing they can do;
/// the first chat model is the default, each work takes a model that can do
/// it, and a key is never sent back nor sent to another server.
#[tokio::test]
async fn ai_models_are_registered_tested_and_chosen() {
    let app = spawn_app().await;
    let admin = admin_session(&app).await;
    let settings =
        json(request(&app, Method::GET, "/api/superadmin/ai", Some(&admin), None).await).await;
    assert_eq!(settings["enabled"], false);
    assert_eq!(settings["models"], json!([]));
    assert_eq!(settings["recipient"]["enabled"], false);
    assert!(
        settings["providers"]
            .as_array()
            .unwrap()
            .contains(&json!("ollama"))
    );

    // A model that does not answer, a provider genai does not know, a model
    // that can do nothing, speech without an OpenAI-compatible endpoint:
    // refused, and nothing is stored.
    let model = |changes: Value| {
        let mut body = json!({
            "name": "Local", "provider": "openai", "endpoint": "http://127.0.0.1:9/v1",
            "model": "qwen3", "capabilities": ["chat"],
        });
        for (k, v) in changes.as_object().unwrap() {
            body[k] = v.clone();
        }
        body
    };
    for body in [
        model(json!({})),
        model(json!({ "provider": "skynet" })),
        model(json!({ "capabilities": [] })),
        model(json!({ "endpoint": "file:///etc" })),
        model(json!({ "provider": "anthropic", "capabilities": ["speech"] })),
        json!({ "provider": "openai", "model": "qwen3" }),
    ] {
        let response = post(
            &app,
            "/api/superadmin/ai/models",
            &admin,
            Some(body.clone()),
        )
        .await;
        assert_eq!(response.status_code(), 400, "{body}");
    }
    let settings =
        json(request(&app, Method::GET, "/api/superadmin/ai", Some(&admin), None).await).await;
    assert_eq!(settings["models"], json!([]));

    // One that answers is registered, with its key, which is never sent back,
    // and is the default.
    let server = model_server::spawn().await;
    let response = post(
        &app,
        "/api/superadmin/ai/models",
        &admin,
        Some(json!({
            "name": "Local",
            "provider": "openai",
            "endpoint": server.endpoint,
            "api_key": "sk-secret",
            "model": "qwen3",
            "capabilities": ["chat", "embeddings"],
        })),
    )
    .await;
    assert_eq!(response.status_code(), 201);
    let local = json(response).await;
    assert_eq!(local["api_key_set"], true);
    assert_eq!(local["from_environment"], false);
    assert_eq!(local["sends_to"], format!("{}/", server.endpoint));
    assert_eq!(local["capabilities"], json!(["chat", "embeddings"]));
    assert!(!local.to_string().contains("sk-secret"));
    // Its chat and its embeddings were both asked, with the key.
    assert_eq!(
        *server.auth.lock().unwrap(),
        [
            Some("Bearer sk-secret".into()),
            Some("Bearer sk-secret".into())
        ]
    );
    let local_id = local["id"].as_str().unwrap().to_string();
    let settings =
        json(request(&app, Method::GET, "/api/superadmin/ai", Some(&admin), None).await).await;
    assert_eq!(settings["enabled"], true);
    assert_eq!(settings["default_model"], local_id);
    // Embeddings are off until chosen; images, scans and audio too.
    assert_eq!(
        settings["work"],
        json!({ "proposals": null, "embeddings": null, "images": null, "scans": null, "audio": null })
    );
    assert_eq!(settings["recipient"]["model"], "qwen3");
    assert_eq!(settings["recipient"]["embeddings_model"], "");
    assert!(!settings.to_string().contains("sk-secret"));

    // Another of the same name is refused.
    let response = post(
        &app,
        "/api/superadmin/ai/models",
        &admin,
        Some(json!({
            "name": "local", "provider": "openai", "endpoint": server.endpoint,
            "model": "x", "capabilities": ["chat"],
        })),
    )
    .await;
    assert_eq!(response.status_code(), 409);

    // A vision model is shown a tiny image; a speech model transcribes a
    // tenth of a second of silence.
    let eyes = add_model(&app, &admin, &server, "Eyes", &["vision"]).await;
    assert_eq!(server.images().len(), 1);
    assert_eq!(server.images()[0].0, "image/png");
    let ears = add_model(&app, &admin, &server, "Ears", &["speech"]).await;
    assert_eq!(server.recordings().len(), 1);
    assert_eq!(server.recordings()[0].2, "ears");

    // Each work takes a model that can do it.
    let settings = choose(
        &app,
        &admin,
        json!({
            "embeddings": local_id, "images": eyes["id"], "scans": eyes["id"], "audio": ears["id"],
        }),
    )
    .await;
    assert_eq!(settings["work"]["embeddings"], local_id);
    assert_eq!(settings["work"]["images"], eyes["id"]);
    assert_eq!(settings["work"]["audio"], ears["id"]);
    assert_eq!(settings["recipient"]["embeddings_model"], "qwen3");
    assert_eq!(settings["recipient"]["images"]["name"], "Eyes");
    assert_eq!(settings["recipient"]["audio"]["model"], "ears");
    for body in [
        json!({ "images": ears["id"] }),
        json!({ "proposals": eyes["id"] }),
        json!({ "default_model": ears["id"] }),
        json!({ "audio": "nope" }),
        json!({ "concurrency": 0 }),
    ] {
        let response = put(&app, "/api/superadmin/ai", &admin, body.clone()).await;
        assert_eq!(response.status_code(), 400, "{body}");
    }

    // A test of a registered model gives its vectors' length.
    let response = post(
        &app,
        "/api/superadmin/ai/models/test",
        &admin,
        Some(json!({ "id": local_id })),
    )
    .await;
    assert_eq!(response.status_code(), 200);
    assert_eq!(json(response).await["embedding_dimensions"], 4);

    // Another endpoint does not get the key: it is removed with the move.
    let other = model_server::spawn().await;
    let response = put(
        &app,
        &format!("/api/superadmin/ai/models/{local_id}"),
        &admin,
        json!({ "endpoint": other.endpoint }),
    )
    .await;
    let status = response.status_code();
    let changed = json(response).await;
    assert_eq!(status, 200, "{changed}");
    assert_eq!(changed["api_key_set"], false);
    let seen = other.auth.lock().unwrap().clone();
    assert_eq!(seen.len(), 2);
    assert!(
        seen.iter()
            .all(|auth| auth.as_deref().is_none_or(|a| a.trim() == "Bearer")),
        "{seen:?}"
    );

    // Taking a capability away takes the model off that work.
    let response = put(
        &app,
        &format!("/api/superadmin/ai/models/{local_id}"),
        &admin,
        json!({ "capabilities": ["chat"] }),
    )
    .await;
    assert_eq!(response.status_code(), 200);
    let settings =
        json(request(&app, Method::GET, "/api/superadmin/ai", Some(&admin), None).await).await;
    assert_eq!(settings["work"]["embeddings"], json!(null));
    assert_eq!(settings["recipient"]["embeddings_endpoint"], "");

    // A removed model's work is off again; the default's, nothing.
    let response = request(
        &app,
        Method::DELETE,
        &format!("/api/superadmin/ai/models/{}", eyes["id"].as_str().unwrap()),
        Some(&admin),
        None,
    )
    .await;
    assert_eq!(response.status_code(), 204);
    let response = request(
        &app,
        Method::DELETE,
        &format!("/api/superadmin/ai/models/{local_id}"),
        Some(&admin),
        None,
    )
    .await;
    assert_eq!(response.status_code(), 204);
    let settings =
        json(request(&app, Method::GET, "/api/superadmin/ai", Some(&admin), None).await).await;
    assert_eq!(settings["work"]["images"], json!(null));
    assert_eq!(settings["work"]["scans"], json!(null));
    assert_eq!(settings["work"]["audio"], ears["id"]);
    assert_eq!(settings["default_model"], json!(null));
    assert_eq!(settings["enabled"], false);
    assert_eq!(settings["models"].as_array().unwrap().len(), 1);

    // Admins only: a key without the admin scope is refused.
    let key = create_key(&app, &admin, "write").await;
    let key = key["token"].as_str().unwrap();
    let response = with_key(&app, Method::GET, "/api/superadmin/ai", key, None).await;
    assert_eq!(response.status_code(), 403);
}

/// The AI_* variables are one locked model, the default, and
/// AI_EMBEDDINGS_MODEL a locked embeddings model; AI_MODEL_* lock a work's
/// model by its registered name.
#[tokio::test]
async fn ai_variables_are_a_locked_model() {
    let server = model_server::spawn().await;
    let app = spawn(Options {
        ai: organizer::ai::Env {
            provider: Some("openai".into()),
            endpoint: Some(server.endpoint.clone()),
            model: Some("qwen3".into()),
            embeddings_model: Some("nomic-embed-text".into()),
            images: Some("Eyes".into()),
            timeout_seconds: Some(30),
            ..organizer::ai::Env::default()
        },
        ..Options::default()
    })
    .await;
    let admin = admin_session(&app).await;
    let settings =
        json(request(&app, Method::GET, "/api/superadmin/ai", Some(&admin), None).await).await;
    assert_eq!(settings["enabled"], true);
    assert_eq!(settings["timeout_seconds"], 30);
    assert_eq!(settings["default_model"], "env");
    assert_eq!(settings["work"]["embeddings"], "env-embeddings");
    let env = &settings["models"][0];
    assert_eq!(env["id"], "env");
    assert_eq!(env["from_environment"], true);
    assert_eq!(env["model"], "qwen3");
    assert_eq!(
        env["variables"],
        json!(["AI_PROVIDER", "AI_ENDPOINT", "AI_MODEL"])
    );
    assert_eq!(settings["models"][1]["capabilities"], json!(["embeddings"]));
    assert_eq!(
        settings["locked"],
        json!([
            { "key": "default", "variable": "AI_MODEL", "value": null },
            { "key": "embeddings", "variable": "AI_EMBEDDINGS_MODEL", "value": null },
            { "key": "images", "variable": "AI_MODEL_IMAGES", "value": "Eyes" },
            { "key": "timeout_seconds", "variable": "AI_TIMEOUT", "value": null },
        ])
    );
    assert_eq!(
        settings["recipient"]["embeddings_endpoint"],
        format!("{}/", server.endpoint)
    );

    // The locked choices and models cannot be changed.
    for (body, variable) in [
        (json!({ "default_model": "" }), "AI_MODEL"),
        (json!({ "embeddings": "" }), "AI_EMBEDDINGS_MODEL"),
        (json!({ "images": "" }), "AI_MODEL_IMAGES"),
        (json!({ "timeout_seconds": 60 }), "AI_TIMEOUT"),
    ] {
        let response = put(&app, "/api/superadmin/ai", &admin, body.clone()).await;
        assert_eq!(response.status_code(), 400, "{body}");
        let error = json(response).await["error"].as_str().unwrap().to_string();
        assert!(error.contains(variable), "{error}");
    }
    let response = put(
        &app,
        "/api/superadmin/ai/models/env",
        &admin,
        json!({ "model": "llama3" }),
    )
    .await;
    assert_eq!(response.status_code(), 400);
    let response = request(
        &app,
        Method::DELETE,
        "/api/superadmin/ai/models/env",
        Some(&admin),
        None,
    )
    .await;
    assert_eq!(response.status_code(), 400);
    // It can be tested.
    let response = post(
        &app,
        "/api/superadmin/ai/models/test",
        &admin,
        Some(json!({ "id": "env" })),
    )
    .await;
    assert_eq!(response.status_code(), 200);

    // The model the variable names does the work once it is registered, and
    // cannot be removed.
    let eyes = add_model(&app, &admin, &server, "Eyes", &["vision"]).await;
    let settings =
        json(request(&app, Method::GET, "/api/superadmin/ai", Some(&admin), None).await).await;
    assert_eq!(settings["work"]["images"], eyes["id"]);
    assert_eq!(settings["recipient"]["images"]["name"], "Eyes");
    let response = request(
        &app,
        Method::DELETE,
        &format!("/api/superadmin/ai/models/{}", eyes["id"].as_str().unwrap()),
        Some(&admin),
        None,
    )
    .await;
    assert_eq!(response.status_code(), 400);

    // What is not locked is still the admin's.
    let settings = choose(
        &app,
        &admin,
        json!({ "concurrency": 3, "scans": eyes["id"] }),
    )
    .await;
    assert_eq!(settings["concurrency"], 3);
    assert_eq!(settings["work"]["scans"], eyes["id"]);
}

/// A server with one AI connection (the AI_* variables) has it registered:
/// its chat model is the default, its embeddings model the embeddings model.
#[tokio::test]
async fn the_ai_connection_of_the_environment_becomes_registered_models() {
    let dir = TempDir::new();
    let path = dir.0.join("organizer.db");
    let db = db::open(path.to_str().unwrap()).await.unwrap();
    let env = organizer::ai::Env {
        provider: Some("openai".into()),
        endpoint: Some("http://gpu:11434/v1".into()),
        api_key: Some("sk-old".into()),
        model: Some("qwen3".into()),
        embeddings_model: Some("nomic-embed-text".into()),
        embeddings_endpoint: Some("http://embedder:8080/v1".into()),
        concurrency: Some(2),
        ..Default::default()
    };
    let ai = organizer::ai::Ai::load(&db, env).await;
    let registry = ai.registry();
    assert_eq!(registry.models.len(), 2);
    assert_eq!(registry.choices.concurrency, 2);
    let chat = ai.model_for(organizer::ai::Work::Proposals).unwrap();
    assert_eq!(
        (
            chat.model.as_str(),
            chat.endpoint.as_str(),
            chat.api_key.as_str(),
            chat.chat
        ),
        ("qwen3", "http://gpu:11434/v1", "sk-old", true)
    );
    assert_eq!(registry.default_model().unwrap().id, chat.id);
    let embeddings = ai.model_for(organizer::ai::Work::Embeddings).unwrap();
    assert_eq!(
        (
            embeddings.model.as_str(),
            embeddings.endpoint.as_str(),
            embeddings.api_key.as_str(),
            embeddings.chat
        ),
        (
            "nomic-embed-text",
            "http://embedder:8080/v1",
            "sk-old",
            false
        )
    );
    assert!(!ai.on(organizer::ai::Work::Images));
    assert!(!ai.on(organizer::ai::Work::Audio));
}

/// Each user's drop box settings, with their defaults, and the
/// sensitive-file rules.
#[tokio::test]
async fn the_drop_box_settings_and_rules_are_each_users() {
    let app = spawn_app().await;
    let admin = admin_session(&app).await;
    let settings = json(
        request(
            &app,
            Method::GET,
            "/api/drop-box/settings",
            Some(&admin),
            None,
        )
        .await,
    )
    .await;
    assert_eq!(settings["pending_mode"], "move_pending");
    assert_eq!(settings["read_contents"], "always");
    assert_eq!(settings["folder_freedom"], "balanced");
    assert_eq!(settings["allowed_top_folders"], json!(null));
    assert_eq!(settings["folders"], "whole");
    assert_eq!(settings["links"], "snapshot");
    assert_eq!(settings["recipient"]["enabled"], false);

    let mut body = settings.clone();
    body.as_object_mut().unwrap().remove("recipient");
    body["pending_mode"] = json!("confidence");
    body["accept_after_hours"] = json!(5);
    body["allowed_top_folders"] = json!(["Bills", " Bills ", "Health"]);
    let response = put(&app, "/api/drop-box/settings", &admin, body.clone()).await;
    assert_eq!(response.status_code(), 200);
    let saved = json(response).await;
    // The hours belong to `timeout` only; the threshold has its default.
    assert_eq!(saved["accept_after_hours"], json!(null));
    assert_eq!(saved["confidence_threshold"], 0.8);
    assert_eq!(saved["allowed_top_folders"], json!(["Bills", "Health"]));
    let again = json(
        request(
            &app,
            Method::GET,
            "/api/drop-box/settings",
            Some(&admin),
            None,
        )
        .await,
    )
    .await;
    assert_eq!(again["pending_mode"], "confidence");
    assert_eq!(again["allowed_top_folders"], json!(["Bills", "Health"]));

    for (field, value, status) in [
        ("pending_mode", json!("later"), 400),
        ("allowed_top_folders", json!([]), 400),
        ("allowed_top_folders", json!(["a/b"]), 400),
        ("confidence_threshold", json!(1.5), 400),
    ] {
        let mut bad = body.clone();
        bad[field] = value;
        let response = put(&app, "/api/drop-box/settings", &admin, bad).await;
        assert_eq!(response.status_code(), status, "{field}");
    }
    let mut timeout = body.clone();
    timeout["pending_mode"] = json!("timeout");
    timeout["accept_after_hours"] = json!(0);
    let response = put(&app, "/api/drop-box/settings", &admin, timeout).await;
    assert_eq!(response.status_code(), 400);

    // The rules.
    let rule = |kind: &str, pattern: &str, action: &str| json!({ "kind": kind, "pattern": pattern, "action": action });
    let response = post(
        &app,
        "/api/drop-box/guard-rules",
        &admin,
        Some(rule("name", "*.kdbx", "by_hand")),
    )
    .await;
    assert_eq!(response.status_code(), 201);
    let first = json(response).await;
    let response = post(
        &app,
        "/api/drop-box/guard-rules",
        &admin,
        Some(rule("keyword", " passport ", "name_only")),
    )
    .await;
    assert_eq!(json(response).await["pattern"], "passport");
    let response = post(
        &app,
        "/api/drop-box/guard-rules",
        &admin,
        Some(rule("name", "  ", "by_hand")),
    )
    .await;
    assert_eq!(response.status_code(), 400);
    let response = post(
        &app,
        "/api/drop-box/guard-rules",
        &admin,
        Some(rule("name", "report-[0-9", "by_hand")),
    )
    .await;
    assert_eq!(response.status_code(), 400);
    let rules = json(
        request(
            &app,
            Method::GET,
            "/api/drop-box/guard-rules",
            Some(&admin),
            None,
        )
        .await,
    )
    .await;
    assert_eq!(rules.as_array().unwrap().len(), 2);
    assert_eq!(rules[0]["pattern"], "*.kdbx");
    let path = format!(
        "/api/drop-box/guard-rules/{}",
        first["id"].as_str().unwrap()
    );
    let response = request(&app, Method::DELETE, &path, Some(&admin), None).await;
    assert_eq!(response.status_code(), 204);
    let response = request(&app, Method::DELETE, &path, Some(&admin), None).await;
    assert_eq!(response.status_code(), 404);

    // A read-only key reads them, and changes nothing.
    let key = create_key(&app, &admin, "read").await;
    let key = key["token"].as_str().unwrap();
    let response = with_key(&app, Method::GET, "/api/drop-box/settings", key, None).await;
    assert_eq!(response.status_code(), 200);
    let response = with_key(
        &app,
        Method::POST,
        "/api/drop-box/guard-rules",
        key,
        Some(rule("name", "x", "by_hand")),
    )
    .await;
    assert_eq!(response.status_code(), 403);
}

/// A multipart form for the drop box: text fields, then files.
fn drop_form(fields: &[(&str, &str)], files: &[(&str, &str)]) -> Vec<u8> {
    let mut body = String::new();
    for (name, value) in fields {
        body.push_str(&format!(
            "--b\r\nContent-Disposition: form-data; name=\"{name}\"\r\n\r\n{value}\r\n"
        ));
    }
    for (name, content) in files {
        body.push_str(&format!(
            "--b\r\nContent-Disposition: form-data; name=\"file\"; filename=\"{name}\"\r\nContent-Type: application/octet-stream\r\n\r\n{content}\r\n"
        ));
    }
    body.push_str("--b--\r\n");
    body.into_bytes()
}

async fn drop_files(
    app: &TestApp,
    session: &str,
    fields: &[(&str, &str)],
    files: &[(&str, &str)],
) -> Response {
    app.server
        .post("/api/drop-box/files")
        .add_header("cookie", session)
        .add_header("content-type", "multipart/form-data; boundary=b")
        .bytes(drop_form(fields, files).into())
        .await
}

/// An item once its analysis has settled (not queued nor analysing).
async fn settled(app: &TestApp, session: &str, id: &str) -> Value {
    for _ in 0..200 {
        let item = json(
            request(
                app,
                Method::GET,
                &format!("/api/drop-box/items/{id}"),
                Some(session),
                None,
            )
            .await,
        )
        .await;
        if !["queued", "analysing"].contains(&item["status"].as_str().unwrap()) {
            return item;
        }
        tokio::time::sleep(std::time::Duration::from_millis(25)).await;
    }
    panic!("the item {id} was not analysed");
}

async fn connect_ai(app: &TestApp, session: &str, server: &model_server::ModelServer) {
    add_model(app, session, server, "Qwen3", &["chat"]).await;
}

/// Files dropped without an AI connection wait for one, and are analysed
/// once it is set up: their text is read, unless a rule says otherwise.
#[tokio::test]
async fn dropped_files_are_staged_and_analysed() {
    let app = spawn_app().await;
    let admin = admin_session(&app).await;
    for rule in [
        json!({ "kind": "name", "pattern": "*.kdbx", "action": "by_hand" }),
        json!({ "kind": "keyword", "pattern": "passport number", "action": "name_only" }),
    ] {
        let response = post(&app, "/api/drop-box/guard-rules", &admin, Some(rule)).await;
        assert_eq!(response.status_code(), 201);
    }

    let response = drop_files(
        &app,
        &admin,
        &[("note", "  bills from the flat  ")],
        &[
            ("power.txt", "Electricity bill. Amount due: 120 EUR"),
            ("vault.kdbx", "secret"),
            ("travel.txt", "Passport number: X123"),
        ],
    )
    .await;
    assert_eq!(response.status_code(), 201);
    let items = json(response).await;
    let ids: Vec<String> = items
        .as_array()
        .unwrap()
        .iter()
        .map(|i| i["id"].as_str().unwrap().to_string())
        .collect();
    assert_eq!(items[0]["name"], "power.txt");
    assert_eq!(items[0]["note"], "bills from the flat");
    assert_eq!(items[0]["kind"], "file");
    assert_eq!(items[0]["size"], 37);
    assert_eq!(items[0]["mime_type"], "text/plain");
    assert_eq!(items[0]["read_contents"], true);

    // No AI yet: the rule that forbids sending is applied, the rest wait.
    let power = settled(&app, &admin, &ids[0]).await;
    assert_eq!(power["status"], "waiting");
    let vault = settled(&app, &admin, &ids[1]).await;
    assert_eq!(vault["status"], "needs_manual");
    assert_eq!(vault["guard"], "by_hand");

    // With a connection, the waiting ones are analysed.
    let server = model_server::spawn().await;
    connect_ai(&app, &admin, &server).await;
    let power = settled(&app, &admin, &ids[0]).await;
    assert_eq!(power["status"], "needs_manual");
    assert_eq!(power["guard"], json!(null));
    assert_eq!(power["text_length"], 37);
    let travel = settled(&app, &admin, &ids[2]).await;
    assert_eq!(travel["guard"], "name_only");
    assert_eq!(travel["text_length"], json!(null));
    let vault = settled(&app, &admin, &ids[1]).await;
    assert_eq!(vault["guard"], "by_hand");

    // The newest first, by status, a page at a time.
    let list =
        json(request(&app, Method::GET, "/api/drop-box/items", Some(&admin), None).await).await;
    assert_eq!(list.as_array().unwrap().len(), 3);
    assert_eq!(list[0]["name"], "travel.txt");
    let page = json(
        request(
            &app,
            Method::GET,
            &format!("/api/drop-box/items?limit=1&before={}", ids[2]),
            Some(&admin),
            None,
        )
        .await,
    )
    .await;
    assert_eq!(page.as_array().unwrap().len(), 1);
    assert_eq!(page[0]["name"], "vault.kdbx");
    let response = request(
        &app,
        Method::GET,
        "/api/drop-box/items?status=failed",
        Some(&admin),
        None,
    )
    .await;
    assert_eq!(json(response).await, json!([]));
    let response = request(
        &app,
        Method::GET,
        &format!("/api/drop-box/items/{}", uuid::Uuid::now_v7()),
        Some(&admin),
        None,
    )
    .await;
    assert_eq!(response.status_code(), 404);
}

/// What is read follows the user's setting and their choice per item; a
/// form without files, a bad name or a long note adds nothing.
#[tokio::test]
async fn reading_contents_follows_the_settings() {
    let app = spawn_app().await;
    let admin = admin_session(&app).await;
    let server = model_server::spawn().await;
    connect_ai(&app, &admin, &server).await;
    let settings = json(
        request(
            &app,
            Method::GET,
            "/api/drop-box/settings",
            Some(&admin),
            None,
        )
        .await,
    )
    .await;
    let mut body = settings.clone();
    body.as_object_mut().unwrap().remove("recipient");
    body["read_contents"] = json!("per_item");
    assert_eq!(
        put(&app, "/api/drop-box/settings", &admin, body)
            .await
            .status_code(),
        200
    );

    let response = drop_files(&app, &admin, &[], &[("a.txt", "alpha")]).await;
    let a = json(response).await[0].clone();
    assert_eq!(a["read_contents"], false);
    let response = drop_files(
        &app,
        &admin,
        &[("read_contents", "true")],
        &[("b.txt", "beta")],
    )
    .await;
    let b = json(response).await[0].clone();
    assert_eq!(b["read_contents"], true);
    let a = settled(&app, &admin, a["id"].as_str().unwrap()).await;
    assert_eq!(a["text_length"], json!(null));
    let b = settled(&app, &admin, b["id"].as_str().unwrap()).await;
    assert_eq!(b["text_length"], 4);

    let long = "x".repeat(2_001);
    for (fields, files) in [
        (vec![], vec![]),
        (vec![("note", long.as_str())], vec![("c.txt", "c")]),
        (vec![("read_contents", "maybe")], vec![("c.txt", "c")]),
        (vec![], vec![("../c.txt", "c")]),
    ] {
        let response = drop_files(&app, &admin, &fields, &files).await;
        assert_eq!(response.status_code(), 400, "{fields:?} {files:?}");
    }
    let list =
        json(request(&app, Method::GET, "/api/drop-box/items", Some(&admin), None).await).await;
    assert_eq!(list.as_array().unwrap().len(), 2);
}

/// A link becomes an item, with the user's link setting unless another is
/// asked for.
#[tokio::test]
async fn links_are_dropped() {
    let app = spawn_app().await;
    let admin = admin_session(&app).await;
    let response = post(
        &app,
        "/api/drop-box/links",
        &admin,
        Some(json!({ "url": "https://example.com/recipes/bread/?x=1", "note": "to bake" })),
    )
    .await;
    assert_eq!(response.status_code(), 201);
    let item = json(response).await;
    assert_eq!(item["kind"], "link");
    assert_eq!(item["name"], "example.com/recipes/bread");
    assert_eq!(item["url"], "https://example.com/recipes/bread/?x=1");
    assert_eq!(item["link_mode"], "snapshot");
    let response = post(
        &app,
        "/api/drop-box/links",
        &admin,
        Some(json!({ "url": "https://example.com/a.pdf", "mode": "download" })),
    )
    .await;
    assert_eq!(json(response).await["link_mode"], "download");
    for url in ["ftp://example.com/x", "not a url", "javascript:alert(1)"] {
        let response = post(
            &app,
            "/api/drop-box/links",
            &admin,
            Some(json!({ "url": url })),
        )
        .await;
        assert_eq!(response.status_code(), 400, "{url}");
    }
}

/// The id of a node by its path.
async fn node_id(app: &TestApp, session: &str, path: &str) -> String {
    let response = by_path(app, session, path).await;
    assert_eq!(response.status_code(), 200, "{path}");
    json(response).await["id"].as_str().unwrap().to_string()
}

async fn item_action(
    app: &TestApp,
    session: &str,
    id: &str,
    action: &str,
    body: Option<Value>,
) -> Response {
    post(
        app,
        &format!("/api/drop-box/items/{id}/{action}"),
        session,
        body,
    )
    .await
}

async fn set_drop_box(app: &TestApp, session: &str, changes: Value) {
    let mut settings = json(
        request(
            app,
            Method::GET,
            "/api/drop-box/settings",
            Some(session),
            None,
        )
        .await,
    )
    .await;
    settings.as_object_mut().unwrap().remove("recipient");
    settings
        .as_object_mut()
        .unwrap()
        .extend(changes.as_object().unwrap().clone());
    let response = put(app, "/api/drop-box/settings", session, settings).await;
    assert_eq!(response.status_code(), 200);
}

/// The model proposes a place, the item is moved there at once and marked
/// pending; confirming keeps it, another place is a correction the next
/// prompt shows, a rejection or an undo takes it back to the drop box.
#[tokio::test]
async fn proposals_are_placed_confirmed_changed_and_undone() {
    let app = spawn_app().await;
    let (admin, share) = with_files(
        &app,
        &[
            ("Home/Bills/water 2025.pdf", "w"),
            ("Health/vaccines.pdf", "v"),
        ],
    )
    .await;
    let server = model_server::spawn().await;
    connect_ai(&app, &admin, &server).await;

    // Proposed and placed, under a new folder and a new name.
    server.answer(json!({
        "folder": "Home/Bills", "new_folders": ["Power"], "name": "Power bill January",
        "reason": "An electricity invoice.", "confidence": 0.9
    }));
    let response = drop_files(
        &app,
        &admin,
        &[("note", "from the flat")],
        &[("scan_0042.txt", "Electricity invoice, January")],
    )
    .await;
    let id = json(response).await[0]["id"].as_str().unwrap().to_string();
    let item = settled(&app, &admin, &id).await;
    assert_eq!(item["status"], "proposed", "{item}");
    assert_eq!(
        item["proposal"]["path"],
        "Home/Bills/Power/Power bill January.txt"
    );
    assert_eq!(item["proposal"]["confidence"], 0.9);
    assert_eq!(item["proposal"]["model"], "qwen3");
    assert_eq!(
        item["placement"]["path"],
        "Home/Bills/Power/Power bill January.txt"
    );
    assert_eq!(
        std::fs::read_to_string(share.0.join("Home/Bills/Power/Power bill January.txt")).unwrap(),
        "Electricity invoice, January"
    );
    // What the model was told: the item, its note and text, the folders.
    let prompt = server.prompts.lock().unwrap().last().unwrap().clone();
    for part in [
        "scan_0042.txt",
        "from the flat",
        "Electricity invoice, January",
        "Home/Bills  (e.g. water 2025.pdf)",
        "Health",
    ] {
        assert!(prompt.contains(part), "{part} in {prompt}");
    }

    // Confirmed as it is.
    let response = item_action(&app, &admin, &id, "confirm", None).await;
    assert_eq!(response.status_code(), 200);
    assert_eq!(json(response).await["status"], "confirmed");
    let response = item_action(&app, &admin, &id, "confirm", None).await;
    assert_eq!(response.status_code(), 409);

    // Another item, put elsewhere: a correction, shown to the model next.
    server.answer(json!({ "folder": "Home/Bills", "reason": "A bill.", "confidence": 0.6 }));
    let response = drop_files(
        &app,
        &admin,
        &[],
        &[("dentist.txt", "Dental check-up invoice")],
    )
    .await;
    let id = json(response).await[0]["id"].as_str().unwrap().to_string();
    let item = settled(&app, &admin, &id).await;
    assert_eq!(item["placement"]["path"], "Home/Bills/dentist.txt");
    let health = node_id(&app, &admin, "Health").await;
    let response = item_action(
        &app,
        &admin,
        &id,
        "confirm",
        Some(json!({ "folder_id": health, "new_folders": ["Dentist"] })),
    )
    .await;
    assert_eq!(response.status_code(), 200);
    let item = json(response).await;
    assert_eq!(item["status"], "changed");
    assert_eq!(item["placement"]["path"], "Health/Dentist/dentist.txt");
    assert!(share.0.join("Health/Dentist/dentist.txt").exists());
    assert!(!share.0.join("Home/Bills/dentist.txt").exists());

    server.answer(json!({ "folder": "Health/Dentist", "reason": "Dental.", "confidence": 0.8 }));
    let response = drop_files(&app, &admin, &[], &[("xray.txt", "Dental x-ray")]).await;
    let id = json(response).await[0]["id"].as_str().unwrap().to_string();
    settled(&app, &admin, &id).await;
    let prompt = server.prompts.lock().unwrap().last().unwrap().clone();
    assert!(
        prompt.contains(
            "dentist.txt: proposed Home/Bills/dentist.txt, chosen Health/Dentist/dentist.txt"
        ),
        "{prompt}"
    );

    // Rejected: back in the drop box; the folder made for it goes too.
    server.answer(json!({ "folder": "Health", "new_folders": ["Insurance"], "reason": "r", "confidence": 0.5 }));
    let response = drop_files(&app, &admin, &[], &[("policy.txt", "Policy")]).await;
    let id = json(response).await[0]["id"].as_str().unwrap().to_string();
    let item = settled(&app, &admin, &id).await;
    assert_eq!(item["placement"]["path"], "Health/Insurance/policy.txt");
    let response = item_action(&app, &admin, &id, "reject", None).await;
    let item = json(response).await;
    assert_eq!(item["status"], "rejected", "{item}");
    assert_eq!(item["placement"], json!(null));
    assert!(!share.0.join("Health/Insurance").exists());
    assert_eq!(
        by_path(&app, &admin, "Health/Insurance")
            .await
            .status_code(),
        404
    );

    // Placed by hand, then undone from where it is.
    let home = node_id(&app, &admin, "Home").await;
    let response = item_action(
        &app,
        &admin,
        &id,
        "place",
        Some(json!({ "folder_id": home, "name": "Insurance policy.txt" })),
    )
    .await;
    assert_eq!(response.status_code(), 200);
    let item = json(response).await;
    assert_eq!(item["status"], "changed");
    assert_eq!(item["placement"]["path"], "Home/Insurance policy.txt");
    let undo = format!(
        "/api/placements/{}/undo",
        item["placement"]["id"].as_str().unwrap()
    );
    let response = post(&app, &undo, &admin, None).await;
    assert_eq!(response.status_code(), 200);
    assert_eq!(json(response).await["status"], "undone");
    assert!(!share.0.join("Home/Insurance policy.txt").exists());
    assert_eq!(post(&app, &undo, &admin, None).await.status_code(), 404);
    // And placed again by hand, under a name that is taken: numbered.
    let (content_type, body) = multipart("policy.txt", "another");
    let response = app
        .server
        .post(&format!("/api/files/{home}/files"))
        .add_header("cookie", &admin)
        .add_header("content-type", &content_type)
        .bytes(body.into())
        .await;
    assert_eq!(response.status_code(), 201);
    let response = item_action(
        &app,
        &admin,
        &id,
        "place",
        Some(json!({ "folder_id": home })),
    )
    .await;
    let item = json(response).await;
    assert_eq!(item["placement"]["path"], "Home/policy (2).txt", "{item}");
    assert_eq!(
        std::fs::read_to_string(share.0.join("Home/policy (2).txt")).unwrap(),
        "Policy"
    );
    assert_eq!(
        std::fs::read_to_string(share.0.join("Home/policy.txt")).unwrap(),
        "another"
    );
}

/// An answer that does not fit the tree is asked again once, then left to
/// the user; a retry asks again.
#[tokio::test]
async fn unusable_answers_leave_the_item_to_the_user() {
    let app = spawn_app().await;
    let (admin, _share) = with_files(&app, &[("Home/a.txt", "a")]).await;
    let server = model_server::spawn().await;
    connect_ai(&app, &admin, &server).await;
    server.answer_text("I think it goes in Work.");
    server.answer(json!({ "folder": "Work", "reason": "r", "confidence": 0.9 }));
    let response = drop_files(&app, &admin, &[], &[("b.txt", "b")]).await;
    let id = json(response).await[0]["id"].as_str().unwrap().to_string();
    let item = settled(&app, &admin, &id).await;
    assert_eq!(item["status"], "needs_manual");
    assert!(
        item["error"]
            .as_str()
            .unwrap()
            .contains("not one of those listed")
    );
    // The second try was told what was wrong.
    let prompts = server.prompts.lock().unwrap().clone();
    assert!(
        prompts
            .last()
            .unwrap()
            .contains("could not be used: The answer is not a JSON object")
    );

    server.answer(json!({ "folder": "Home", "reason": "r", "confidence": 0.9 }));
    let response = item_action(&app, &admin, &id, "retry", None).await;
    assert_eq!(response.status_code(), 200);
    let item = settled(&app, &admin, &id).await;
    assert_eq!(item["status"], "proposed");
    assert_eq!(item["error"], json!(null));
    assert_eq!(
        item_action(&app, &admin, &id, "retry", None)
            .await
            .status_code(),
        409
    );
    assert_eq!(
        item_action(
            &app,
            &admin,
            &id,
            "place",
            Some(json!({ "folder_id": null }))
        )
        .await
        .status_code(),
        409
    );
}

/// The pending modes: left in the drop box until confirmed, or accepted at
/// once from a confidence.
#[tokio::test]
async fn the_pending_mode_decides_what_happens_to_a_proposal() {
    let app = spawn_app().await;
    let (admin, share) = with_files(&app, &[("Home/a.txt", "a")]).await;
    let server = model_server::spawn().await;
    connect_ai(&app, &admin, &server).await;

    set_drop_box(&app, &admin, json!({ "pending_mode": "stay" })).await;
    server.answer(json!({ "folder": "Home", "reason": "r", "confidence": 0.99 }));
    let response = drop_files(&app, &admin, &[], &[("b.txt", "b")]).await;
    let id = json(response).await[0]["id"].as_str().unwrap().to_string();
    let item = settled(&app, &admin, &id).await;
    assert_eq!(item["status"], "proposed");
    assert_eq!(item["placement"], json!(null));
    assert!(!share.0.join("Home/b.txt").exists());
    let item = json(item_action(&app, &admin, &id, "confirm", Some(json!(null))).await).await;
    assert_eq!(item["status"], "confirmed", "{item}");
    assert_eq!(item["placement"]["path"], "Home/b.txt");

    set_drop_box(
        &app,
        &admin,
        json!({ "pending_mode": "confidence", "confidence_threshold": 0.8 }),
    )
    .await;
    server.answer(json!({ "folder": "Home", "reason": "r", "confidence": 0.85 }));
    server.answer(json!({ "folder": "Home", "reason": "r", "confidence": 0.5 }));
    let response = drop_files(&app, &admin, &[], &[("c.txt", "c")]).await;
    let sure = json(response).await[0]["id"].as_str().unwrap().to_string();
    let sure = settled(&app, &admin, &sure).await;
    let response = drop_files(&app, &admin, &[], &[("d.txt", "d")]).await;
    let unsure = json(response).await[0]["id"].as_str().unwrap().to_string();
    let unsure = settled(&app, &admin, &unsure).await;
    assert_eq!(sure["status"], "auto_accepted");
    assert_eq!(sure["placement"]["path"], "Home/c.txt");
    assert_eq!(unsure["status"], "proposed");
    assert_eq!(unsure["placement"], json!(null));
}

async fn search_for(app: &TestApp, session: &str, q: &str) -> Value {
    json(
        request(
            app,
            Method::GET,
            &format!("/api/search?q={}", urlencoding(q)),
            Some(session),
            None,
        )
        .await,
    )
    .await
}

/// What a search finds once the index has caught up.
async fn found(app: &TestApp, session: &str, q: &str, until: impl Fn(&Value) -> bool) -> Value {
    for _ in 0..200 {
        let hits = search_for(app, session, q).await;
        if until(&hits) {
            return hits;
        }
        tokio::time::sleep(std::time::Duration::from_millis(25)).await;
    }
    panic!(
        "the search for {q} never found what was expected: {}",
        search_for(app, session, q).await
    );
}

/// Names, paths and the text of files are found, and the index follows a
/// move and a delete.
#[tokio::test]
async fn search_finds_names_paths_and_text() {
    let app = spawn_app().await;
    let (admin, _share) = with_files(
        &app,
        &[
            (
                "Home/Bills/power 2025.txt",
                "Electricity invoice, amount due 120",
            ),
            ("Health/vaccines.txt", "Yellow fever, 2019"),
            ("Private/diary.txt", "secret thoughts about electricity"),
        ],
    )
    .await;
    let rule = json!({ "kind": "path", "pattern": "Private/*", "action": "by_hand" });
    assert_eq!(
        post(&app, "/api/drop-box/guard-rules", &admin, Some(rule))
            .await
            .status_code(),
        201
    );

    // By name, by path, by a word's start, and by text.
    let hits = found(&app, &admin, "vaccines", |h| {
        h.as_array().unwrap().len() == 1
    })
    .await;
    assert_eq!(hits[0]["name"], "vaccines.txt");
    assert_eq!(hits[0]["path"], "Health");
    let hits = found(&app, &admin, "bills", |h| h.as_array().unwrap().len() == 2).await;
    // The folder by its name first, then the file in it by its path.
    assert_eq!(hits[0]["name"], "Bills");
    assert_eq!(hits[0]["kind"], "folder");
    assert_eq!(hits[1]["name"], "power 2025.txt");
    let hits = found(&app, &admin, "elec amount", |h| {
        h.as_array().unwrap().len() == 1
    })
    .await;
    assert_eq!(hits[0]["name"], "power 2025.txt");
    assert!(
        hits[0]["snippet"]
            .as_str()
            .unwrap()
            .contains("\u{1}Electricity\u{2}"),
        "{hits}"
    );
    // A file a rule keeps from being read is found by its name only.
    let hits = search_for(&app, &admin, "thoughts").await;
    assert_eq!(hits, json!([]));
    assert_eq!(
        search_for(&app, &admin, "diary")
            .await
            .as_array()
            .unwrap()
            .len(),
        1
    );
    assert_eq!(search_for(&app, &admin, "   ").await, json!([]));
    assert_eq!(search_for(&app, &admin, "\"").await, json!([]));

    // Moved: found under its new path; deleted: gone.
    let file = node_id(&app, &admin, "Health/vaccines.txt").await;
    let home = node_id(&app, &admin, "Home").await;
    let node = json(
        request(
            &app,
            Method::GET,
            &format!("/api/files/{file}"),
            Some(&admin),
            None,
        )
        .await,
    )
    .await;
    let response = request(
        &app,
        Method::PATCH,
        &format!("/api/files/{file}"),
        Some(&admin),
        Some(json!({ "parent_id": home, "if_version": node["version"] })),
    )
    .await;
    assert_eq!(response.status_code(), 200, "{}", response.text());
    let hits = found(&app, &admin, "vaccines", |h| h[0]["path"] == "Home").await;
    assert_eq!(hits.as_array().unwrap().len(), 1);
    // Its text is kept through the move.
    found(&app, &admin, "yellow fever", |h| {
        h.as_array().unwrap().len() == 1
    })
    .await;
    let node = json(
        request(
            &app,
            Method::GET,
            &format!("/api/files/{file}"),
            Some(&admin),
            None,
        )
        .await,
    )
    .await;
    let response = request(
        &app,
        Method::DELETE,
        &format!("/api/files/{file}?if_version={}", node["version"]),
        Some(&admin),
        None,
    )
    .await;
    assert_eq!(response.status_code(), 200);
    found(&app, &admin, "vaccines", |h| h == &json!([])).await;
}

/// A local web server with a page and a file, for links.
async fn page_server() -> String {
    use axum::routing::get;
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let base = format!("http://{}", listener.local_addr().unwrap());
    let router = axum::Router::new()
        .route(
            "/recipe",
            get(|| async {
                axum::response::Html(
                    "<html><head><title>Sourdough bread</title></head><body><h1>Sourdough</h1><p>Flour, water, salt.</p></body></html>",
                )
            }),
        )
        .route(
            "/get",
            get(|| async {
                (
                    [
                        ("content-type", "text/plain"),
                        ("content-disposition", "attachment; filename=\"tax return 2025.txt\""),
                    ],
                    "Tax return for 2025",
                )
            }),
        )
        .route("/old", get(|| async { axum::response::Redirect::temporary("/recipe") }))
        .route(
            "/styled",
            get(|| async {
                axum::response::Html(
                    "<html><head><title>Rye bread</title><link rel=\"stylesheet\" href=\"/style.css\"><script>track()</script></head>\
                     <body><h1>Rye bread</h1><p>Rye, water, <a href=\"/salt\">salt</a>.</p><img src=\"/logo.png\"></body></html>",
                )
            }),
        )
        .route(
            "/style.css",
            get(|| async { ([("content-type", "text/css")], "body{background:url(bg.png)}") }),
        )
        .route("/logo.png", get(|| async { ([("content-type", "image/png")], vec![137u8, 80, 78, 71]) }))
        .route("/bg.png", get(|| async { ([("content-type", "image/png")], vec![1u8, 2, 3]) }));
    tokio::spawn(async move { axum::serve(listener, router).await.unwrap() });
    base
}

/// A link becomes a bookmark named by the page's title (with its text for
/// the model) or the file it points to, and is placed like any file.
#[tokio::test]
async fn links_become_bookmarks_or_downloads() {
    let app = spawn(Options {
        links_allow_private: true,
        ..Options::default()
    })
    .await;
    let (admin, share) = with_files(&app, &[("Food/a.txt", "a"), ("Taxes/b.txt", "b")]).await;
    let server = model_server::spawn().await;
    connect_ai(&app, &admin, &server).await;
    let pages = page_server().await;

    server.answer(json!({ "folder": "Food", "reason": "A recipe.", "confidence": 0.9 }));
    let response = post(
        &app,
        "/api/drop-box/links",
        &admin,
        Some(json!({ "url": format!("{pages}/old"), "mode": "bookmark" })),
    )
    .await;
    let id = json(response).await["id"].as_str().unwrap().to_string();
    let item = settled(&app, &admin, &id).await;
    assert_eq!(item["status"], "proposed", "{item}");
    assert_eq!(item["name"], "Sourdough bread.url");
    assert_eq!(item["placement"]["path"], "Food/Sourdough bread.url");
    assert_eq!(
        std::fs::read_to_string(share.0.join("Food/Sourdough bread.url")).unwrap(),
        format!("[InternetShortcut]\r\nURL={pages}/old\r\n")
    );
    let prompt = server.prompts.lock().unwrap().last().unwrap().clone();
    assert!(prompt.contains("Flour, water, salt."), "{prompt}");
    assert!(prompt.contains(&format!("- link: {pages}/old")), "{prompt}");

    server.answer(json!({ "folder": "Taxes", "reason": "A tax return.", "confidence": 0.9 }));
    let response = post(
        &app,
        "/api/drop-box/links",
        &admin,
        Some(json!({ "url": format!("{pages}/get"), "mode": "download" })),
    )
    .await;
    let id = json(response).await["id"].as_str().unwrap().to_string();
    let item = settled(&app, &admin, &id).await;
    assert_eq!(item["name"], "tax return 2025.txt");
    assert_eq!(item["text_length"], 19);
    assert_eq!(item["placement"]["path"], "Taxes/tax return 2025.txt");
    assert_eq!(
        std::fs::read_to_string(share.0.join("Taxes/tax return 2025.txt")).unwrap(),
        "Tax return for 2025"
    );
}

/// Without LINKS_ALLOW_PRIVATE, a local address is not fetched: the
/// bookmark is named by the address, and nothing of the page is read.
#[tokio::test]
async fn links_to_local_addresses_are_not_fetched() {
    let app = spawn_app().await;
    let (admin, _share) = with_files(&app, &[("Food/a.txt", "a")]).await;
    let server = model_server::spawn().await;
    connect_ai(&app, &admin, &server).await;
    let pages = page_server().await;
    server.answer(json!({ "folder": "Food", "reason": "r", "confidence": 0.9 }));
    let response = post(
        &app,
        "/api/drop-box/links",
        &admin,
        Some(json!({ "url": format!("{pages}/recipe") })),
    )
    .await;
    let id = json(response).await["id"].as_str().unwrap().to_string();
    let item = settled(&app, &admin, &id).await;
    assert_eq!(item["name"], "127.0.0.1 recipe.url");
    assert_eq!(item["text_length"], json!(null));
    let prompt = server.prompts.lock().unwrap().last().unwrap().clone();
    assert!(!prompt.contains("Flour"), "{prompt}");
}

/// An item once analysed, waiting longer than `settled` (the WebDAV drop
/// box delays its analysis).
async fn analysed(app: &TestApp, session: &str, name: &str) -> Value {
    for _ in 0..300 {
        let items =
            json(request(app, Method::GET, "/api/drop-box/items", Some(session), None).await).await;
        if let Some(item) = items.as_array().unwrap().iter().find(|i| i["name"] == name)
            && !["queued", "analysing"].contains(&item["status"].as_str().unwrap())
        {
            return item.clone();
        }
        tokio::time::sleep(std::time::Duration::from_millis(50)).await;
    }
    panic!("{name} was not analysed");
}

/// The WebDAV drop box: files written into it become items, shown there
/// while they wait; `review/` waits for the user, `auto/` places at once.
#[tokio::test]
async fn the_webdav_drop_box_takes_files() {
    let app = spawn_app().await;
    let (admin, share) = with_files(&app, &[("Home/a.txt", "a")]).await;
    let server = model_server::spawn().await;
    connect_ai(&app, &admin, &server).await;
    let body = json!({ "name": "Finder", "scope": "write", "kind": "app_password" });
    let password = json(post(&app, "/api/auth/api-keys", &admin, Some(body)).await).await["token"]
        .as_str()
        .unwrap()
        .to_string();
    let auth = basic(&password);
    let dav = |method: &str, path: &str| {
        app.server
            .method(Method::from_bytes(method.as_bytes()).unwrap(), path)
            .add_header("authorization", &auth)
    };
    assert_eq!(
        app.server
            .method(Method::from_bytes(b"PROPFIND").unwrap(), "/dav-inbox/")
            .await
            .status_code(),
        401
    );

    let response = dav("PROPFIND", "/dav-inbox/")
        .add_header("depth", "1")
        .await;
    assert_eq!(response.status_code(), 207);
    let listing = response.text();
    assert!(
        listing.contains("/dav-inbox/review/") && listing.contains("/dav-inbox/auto/"),
        "{listing}"
    );

    // Finder's way: empty first, then the content; and its own files.
    assert_eq!(
        dav("PUT", "/dav-inbox/review/bill.txt")
            .bytes("".into())
            .await
            .status_code(),
        201
    );
    let response = dav("PUT", "/dav-inbox/review/bill.txt")
        .bytes("Electricity bill".into())
        .await;
    assert!(
        response.status_code() == 204 || response.status_code() == 201,
        "{}",
        response.status_code()
    );
    assert_eq!(
        dav("PUT", "/dav-inbox/review/._bill.txt")
            .bytes("finder".into())
            .await
            .status_code(),
        201
    );
    let listing = dav("PROPFIND", "/dav-inbox/review/")
        .add_header("depth", "1")
        .await
        .text();
    assert!(
        listing.contains("bill.txt") && !listing.contains("._bill.txt"),
        "{listing}"
    );
    assert_eq!(
        dav("GET", "/dav-inbox/review/bill.txt").await.text(),
        "Electricity bill"
    );

    // One item, which waits for the user although the mode would place it.
    server.answer(json!({ "folder": "Home", "reason": "r", "confidence": 0.99 }));
    let item = analysed(&app, &admin, "bill.txt").await;
    assert_eq!(item["status"], "proposed", "{item}");
    assert_eq!(item["placement"], json!(null));
    assert_eq!(item["size"], 16);
    let items =
        json(request(&app, Method::GET, "/api/drop-box/items", Some(&admin), None).await).await;
    assert_eq!(items.as_array().unwrap().len(), 1);
    // Still shown in the drive while it waits; removed from there, it goes.
    assert!(
        dav("PROPFIND", "/dav-inbox/review/")
            .add_header("depth", "1")
            .await
            .text()
            .contains("bill.txt")
    );
    assert_eq!(
        dav("DELETE", "/dav-inbox/review/bill.txt")
            .await
            .status_code(),
        204
    );
    let items =
        json(request(&app, Method::GET, "/api/drop-box/items", Some(&admin), None).await).await;
    assert_eq!(items, json!([]));

    // `auto/`: placed and accepted at once, gone from the drive.
    server.answer(json!({ "folder": "Home", "reason": "r", "confidence": 0.2 }));
    assert_eq!(
        dav("PUT", "/dav-inbox/auto/notes.txt")
            .bytes("notes".into())
            .await
            .status_code(),
        201
    );
    let item = analysed(&app, &admin, "notes.txt").await;
    assert_eq!(item["status"], "auto_accepted", "{item}");
    assert_eq!(
        std::fs::read_to_string(share.0.join("Home/notes.txt")).unwrap(),
        "notes"
    );
    assert!(
        !dav("PROPFIND", "/dav-inbox/auto/")
            .add_header("depth", "1")
            .await
            .text()
            .contains("notes.txt")
    );

    // No renames; nothing into a folder that is not there.
    assert_eq!(
        dav("MOVE", "/dav-inbox/review/x.txt")
            .add_header("destination", "/dav-inbox/review/y.txt")
            .await
            .status_code()
            .as_u16()
            / 100,
        4
    );
    // A folder that is not there: WebDAV's "no such collection".
    assert_eq!(
        dav("PUT", "/dav-inbox/other/x.txt")
            .bytes("x".into())
            .await
            .status_code(),
        409
    );
}

/// A file of `tests/fixtures`.
fn fixture(name: &str) -> Vec<u8> {
    std::fs::read(
        std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("tests/fixtures")
            .join(name),
    )
    .unwrap()
}

/// A short recording: a tenth of a second of silence, as WAV.
fn recording() -> Vec<u8> {
    let data = 3_200u32;
    let mut wav = b"RIFF".to_vec();
    wav.extend_from_slice(&(36 + data).to_le_bytes());
    wav.extend_from_slice(b"WAVEfmt ");
    wav.extend_from_slice(&16u32.to_le_bytes());
    for n in [1u16, 1] {
        wav.extend_from_slice(&n.to_le_bytes());
    }
    for n in [16_000u32, 32_000] {
        wav.extend_from_slice(&n.to_le_bytes());
    }
    for n in [2u16, 16] {
        wav.extend_from_slice(&n.to_le_bytes());
    }
    wav.extend_from_slice(b"data");
    wav.extend_from_slice(&data.to_le_bytes());
    wav.resize(44 + data as usize, 0);
    wav
}

/// Drop files of any bytes; answers the id of the first item.
async fn drop_bytes(
    app: &TestApp,
    session: &str,
    fields: &[(&str, &str)],
    files: &[(&str, &[u8])],
) -> String {
    let mut body = Vec::new();
    for (name, value) in fields {
        body.extend_from_slice(
            format!("--b\r\nContent-Disposition: form-data; name=\"{name}\"\r\n\r\n{value}\r\n")
                .as_bytes(),
        );
    }
    for (name, content) in files {
        body.extend_from_slice(
            format!("--b\r\nContent-Disposition: form-data; name=\"file\"; filename=\"{name}\"\r\nContent-Type: application/octet-stream\r\n\r\n")
                .as_bytes(),
        );
        body.extend_from_slice(content);
        body.extend_from_slice(b"\r\n");
    }
    body.extend_from_slice(b"--b--\r\n");
    let response = app
        .server
        .post("/api/drop-box/files")
        .add_header("cookie", session)
        .add_header("content-type", "multipart/form-data; boundary=b")
        .bytes(body.into())
        .await;
    assert_eq!(response.status_code(), 201);
    json(response).await[0]["id"].as_str().unwrap().to_string()
}

/// The last prompt the proposals' model was given.
fn last_prompt(server: &model_server::ModelServer) -> String {
    server
        .prompts
        .lock()
        .unwrap()
        .last()
        .cloned()
        .unwrap_or_default()
}

/// Photos, scanned PDFs and recordings are placed by name until a model is
/// chosen for them; then a vision model reads photos and scanned pages (sent
/// as they are), a speech model transcribes recordings (their first hour),
/// and the proposal is made from what they read, once per content.
#[tokio::test]
async fn photos_scans_and_recordings_are_read_by_their_models() {
    let app = spawn_app().await;
    let admin = admin_session(&app).await;
    let server = model_server::spawn().await;
    connect_ai(&app, &admin, &server).await;
    let photo = fixture("photo.jpg");

    // Off: placed by name, nothing sent but the name.
    let id = drop_bytes(&app, &admin, &[], &[("receipt.jpg", &photo)]).await;
    let item = settled(&app, &admin, &id).await;
    assert_eq!(item["text_length"], json!(null));
    assert!(server.images().is_empty());
    assert!(last_prompt(&server).contains("receipt.jpg"));

    let eyes = add_model(&app, &admin, &server, "Eyes", &["vision"]).await;
    let ears = add_model(&app, &admin, &server, "Ears", &["speech"]).await;
    choose(
        &app,
        &admin,
        json!({ "images": eyes["id"], "scans": eyes["id"], "audio": ears["id"] }),
    )
    .await;
    // The tests of the two models.
    assert_eq!(server.images().len(), 1);
    assert_eq!(server.recordings().len(), 1);

    // A photo: sent as it is, its text and description are what the
    // proposal is made from.
    server.see("Dentist invoice 2026\nDescription: a receipt on a table");
    let id = drop_bytes(&app, &admin, &[], &[("dentist.jpg", &photo)]).await;
    let item = settled(&app, &admin, &id).await;
    assert!(item["text_length"].as_i64().unwrap() > 0, "{item}");
    let images = server.images();
    assert_eq!(images.len(), 2);
    assert_eq!(images[1].0, "image/jpeg");
    assert_eq!(images[1].1, photo.len().div_ceil(3) * 4);
    let prompt = last_prompt(&server);
    assert!(prompt.contains("Dentist invoice 2026"), "{prompt}");
    assert!(prompt.contains("Read from the image"), "{prompt}");

    // The same photo again, under another name: not read again.
    let id = drop_bytes(&app, &admin, &[], &[("copy.jpg", &photo)]).await;
    settled(&app, &admin, &id).await;
    assert_eq!(server.images().len(), 2);
    assert!(last_prompt(&server).contains("Dentist invoice 2026"));

    // A scanned PDF: its page, as the image it holds.
    server.see("Home insurance policy, page one");
    let id = drop_bytes(&app, &admin, &[], &[("scan.pdf", &fixture("scan.pdf"))]).await;
    settled(&app, &admin, &id).await;
    let images = server.images();
    assert_eq!(images.len(), 3);
    assert!(images[2].0.starts_with("image/"), "{images:?}");
    let prompt = last_prompt(&server);
    assert!(prompt.contains("Home insurance policy"), "{prompt}");
    assert!(prompt.contains("scanned pages"), "{prompt}");

    // A recording: its first hour, transcribed.
    let wav = recording();
    let id = drop_bytes(&app, &admin, &[], &[("memo.wav", &wav)]).await;
    settled(&app, &admin, &id).await;
    let recordings = server.recordings();
    assert_eq!(recordings.len(), 2);
    assert_eq!(
        recordings[1],
        ("memo.wav".to_string(), wav.len(), "ears".to_string())
    );
    let prompt = last_prompt(&server);
    assert!(prompt.contains("Hello from the recording"), "{prompt}");
    assert!(!prompt.contains("after the first hour"), "{prompt}");

    // What Organizer sends says where each goes.
    let settings = json(
        request(
            &app,
            Method::GET,
            "/api/drop-box/settings",
            Some(&admin),
            None,
        )
        .await,
    )
    .await;
    assert_eq!(settings["recipient"]["images"]["name"], "Eyes");
    assert_eq!(settings["recipient"]["scans"]["name"], "Eyes");
    assert_eq!(settings["recipient"]["audio"]["name"], "Ears");
}

/// A photo a sensitive-file rule holds, or one dropped without reading its
/// contents, is never sent to a model.
#[tokio::test]
async fn photos_follow_the_rules_and_the_reading_setting() {
    let app = spawn_app().await;
    let admin = admin_session(&app).await;
    let server = model_server::spawn().await;
    connect_ai(&app, &admin, &server).await;
    let eyes = add_model(&app, &admin, &server, "Eyes", &["vision"]).await;
    choose(&app, &admin, json!({ "images": eyes["id"] })).await;
    let tested = server.images().len();
    let rule = json!({ "kind": "name", "pattern": "passport*", "action": "name_only" });
    let response = post(&app, "/api/drop-box/guard-rules", &admin, Some(rule)).await;
    assert_eq!(response.status_code(), 201);
    let photo = fixture("photo.jpg");

    let id = drop_bytes(&app, &admin, &[], &[("passport.jpg", &photo)]).await;
    let item = settled(&app, &admin, &id).await;
    assert_eq!(item["guard"], "name_only");
    assert_eq!(item["text_length"], json!(null));

    // Reading on by default, off for this one.
    let settings = json(
        request(
            &app,
            Method::GET,
            "/api/drop-box/settings",
            Some(&admin),
            None,
        )
        .await,
    )
    .await;
    let mut body = settings.clone();
    body.as_object_mut().unwrap().remove("recipient");
    body["read_contents"] = json!("default_on");
    let response = put(&app, "/api/drop-box/settings", &admin, body).await;
    assert_eq!(response.status_code(), 200);
    let id = drop_bytes(
        &app,
        &admin,
        &[("read_contents", "false")],
        &[("holiday.jpg", &photo)],
    )
    .await;
    let item = settled(&app, &admin, &id).await;
    assert_eq!(item["read_contents"], false);
    assert_eq!(item["text_length"], json!(null));
    assert_eq!(server.images().len(), tested);
}

/// Photos in a tree are read for search by `read` jobs, once per content,
/// when a model is chosen for them; those indexed before are read when
/// asked to ("Read existing files again").
#[tokio::test]
async fn photos_in_the_tree_are_read_for_search() {
    let app = spawn_app().await;
    let photo = fixture("photo.jpg");
    let (admin, _share) = with_files(
        &app,
        &[
            ("Photos/receipt.jpg", photo.as_slice()),
            ("Photos/copy.jpg", photo.as_slice()),
        ],
    )
    .await;
    let server = model_server::spawn().await;
    connect_ai(&app, &admin, &server).await;
    // Indexed, hashed and looked at, unread.
    for _ in 0..200 {
        let looked: i64 = sqlx::query_scalar(
            "SELECT count(*) FROM search_docs WHERE kind = 'file' AND text_hash IS NOT NULL",
        )
        .fetch_one(app.state.db.reader())
        .await
        .unwrap();
        if looked == 2 {
            break;
        }
        tokio::time::sleep(std::time::Duration::from_millis(25)).await;
    }
    found(&app, &admin, "receipt", |h| {
        h.as_array().unwrap().len() == 1
    })
    .await;
    let eyes = add_model(&app, &admin, &server, "Eyes", &["vision"]).await;
    choose(&app, &admin, json!({ "images": eyes["id"] })).await;
    let tested = server.images().len();
    // What was indexed before is not read by itself.
    tokio::time::sleep(std::time::Duration::from_millis(300)).await;
    assert_eq!(server.images().len(), tested);
    assert_eq!(search_for(&app, &admin, "dentist").await, json!([]));

    server.see("Dentist invoice\nDescription: a receipt");
    let response = post(&app, "/api/superadmin/ai/read-again", &admin, None).await;
    assert_eq!(response.status_code(), 200);
    assert_eq!(json(response).await["queued"], 2);
    let hits = found(&app, &admin, "dentist", |h| {
        h.as_array().unwrap().len() == 2
    })
    .await;
    assert!(
        hits[0]["snippet"].as_str().unwrap().contains("Dentist"),
        "{hits}"
    );
    // The two copies were read once.
    assert_eq!(server.images().len(), tested + 1);
    // Asked again, nothing is left to read.
    let response = post(&app, "/api/superadmin/ai/read-again", &admin, None).await;
    assert_eq!(json(response).await["queued"], 0);
}

/// With an embeddings model, a search finds files by meaning too, after
/// those found by their words; the trees are embedded once it is set up.
#[tokio::test]
async fn semantic_search_finds_files_by_meaning() {
    let app = spawn_app().await;
    let (admin, _share) = with_files(
        &app,
        &[
            ("Health/appointment.txt", "Dentist appointment on Monday"),
            ("Money/statement.txt", "Bank account statement for May"),
        ],
    )
    .await;
    // Indexed by words first.
    found(&app, &admin, "statement", |h| {
        h.as_array().unwrap().len() == 1
    })
    .await;
    assert_eq!(search_for(&app, &admin, "teeth").await, json!([]));

    let server = model_server::spawn().await;
    let model = add_model(&app, &admin, &server, "Local", &["chat", "embeddings"]).await;
    choose(&app, &admin, json!({ "embeddings": model["id"] })).await;

    let mut hits = json!([]);
    for _ in 0..200 {
        hits = json(
            request(
                &app,
                Method::GET,
                "/api/search?q=teeth&semantic=true",
                Some(&admin),
                None,
            )
            .await,
        )
        .await;
        if !hits.as_array().unwrap().is_empty() {
            break;
        }
        tokio::time::sleep(std::time::Duration::from_millis(25)).await;
    }
    assert_eq!(hits[0]["name"], "appointment.txt", "{hits}");
    assert_eq!(hits[0]["semantic"], true);
    assert!(
        hits[0]["snippet"]
            .as_str()
            .unwrap()
            .contains("Dentist appointment")
    );
    // Without asking for meaning, the words alone.
    assert_eq!(search_for(&app, &admin, "teeth").await, json!([]));
    // A match by words comes first, and is not repeated.
    let hits = json(
        request(
            &app,
            Method::GET,
            "/api/search?q=bank&semantic=true",
            Some(&admin),
            None,
        )
        .await,
    )
    .await;
    assert_eq!(hits[0]["name"], "statement.txt");
    assert_eq!(hits[0]["semantic"], false);
    assert_eq!(
        hits.as_array()
            .unwrap()
            .iter()
            .filter(|h| h["name"] == "statement.txt")
            .count(),
        1
    );
}

/// A link in snapshot mode becomes the page saved whole (self-contained
/// HTML) with its Markdown beside it; both are placed, and undone, together.
#[tokio::test]
async fn snapshots_are_self_contained_with_markdown_beside() {
    let app = spawn(Options {
        links_allow_private: true,
        ..Options::default()
    })
    .await;
    let (admin, share) = with_files(&app, &[("Food/a.txt", "a")]).await;
    let server = model_server::spawn().await;
    connect_ai(&app, &admin, &server).await;
    let pages = page_server().await;
    server.answer(json!({ "folder": "Food", "reason": "A recipe.", "confidence": 0.9 }));
    let response = post(
        &app,
        "/api/drop-box/links",
        &admin,
        Some(json!({ "url": format!("{pages}/styled"), "mode": "snapshot" })),
    )
    .await;
    let id = json(response).await["id"].as_str().unwrap().to_string();
    let item = settled(&app, &admin, &id).await;
    assert_eq!(item["name"], "Rye bread.html", "{item}");
    assert_eq!(item["placement"]["path"], "Food/Rye bread.html");
    let html = std::fs::read_to_string(share.0.join("Food/Rye bread.html")).unwrap();
    assert!(!html.contains("track()"), "{html}");
    assert!(html.contains("Content-Security-Policy"));
    assert!(html.contains("data:image/png;base64,iVBORw=="), "{html}");
    assert!(
        html.contains("url(\"data:image/png;base64,AQID\")"),
        "{html}"
    );
    assert!(html.contains(&format!("href=\"{pages}/salt\"")), "{html}");
    let markdown = std::fs::read_to_string(share.0.join("Food/Rye bread.md")).unwrap();
    assert!(
        markdown.starts_with("# Rye bread\n\nSource: <"),
        "{markdown}"
    );
    assert!(markdown.contains("Rye, water"), "{markdown}");

    // Undone: both go back.
    let undo = format!(
        "/api/placements/{}/undo",
        item["placement"]["id"].as_str().unwrap()
    );
    assert_eq!(post(&app, &undo, &admin, None).await.status_code(), 200);
    assert!(!share.0.join("Food/Rye bread.html").exists());
    assert!(!share.0.join("Food/Rye bread.md").exists());
    // And placed by hand elsewhere, together again.
    let food = node_id(&app, &admin, "Food").await;
    let response = item_action(
        &app,
        &admin,
        &id,
        "place",
        Some(json!({ "folder_id": food, "new_folders": ["Bread"], "name": "Rye.html" })),
    )
    .await;
    assert_eq!(response.status_code(), 200);
    assert!(share.0.join("Food/Bread/Rye.html").exists());
    assert!(share.0.join("Food/Bread/Rye.md").exists());
}

/// A dropped folder is one item, placed whole and undone whole; split, it
/// is an item per file, the path rules applied inside it.
#[tokio::test]
async fn dropped_folders_are_kept_whole_or_split() {
    let app = spawn_app().await;
    let (admin, share) = with_files(&app, &[("Travel/a.txt", "a")]).await;
    let server = model_server::spawn().await;
    connect_ai(&app, &admin, &server).await;

    server.answer(json!({ "folder": "Travel", "reason": "A trip.", "confidence": 0.9 }));
    let response = drop_files(
        &app,
        &admin,
        &[],
        &[
            (
                "Japan trip/tickets/train.txt",
                "Shinkansen ticket, Tokyo to Kyoto",
            ),
            ("Japan trip/hotel.txt", "Hotel booking in Kyoto"),
            ("Japan trip/.DS_Store", "finder"),
        ],
    )
    .await;
    assert_eq!(response.status_code(), 201);
    let items = json(response).await;
    assert_eq!(items.as_array().unwrap().len(), 1);
    assert_eq!(items[0]["kind"], "folder");
    assert_eq!(items[0]["name"], "Japan trip");
    let id = items[0]["id"].as_str().unwrap().to_string();
    let item = settled(&app, &admin, &id).await;
    assert_eq!(item["status"], "proposed", "{item}");
    assert_eq!(item["placement"]["path"], "Travel/Japan trip");
    assert_eq!(
        std::fs::read_to_string(share.0.join("Travel/Japan trip/tickets/train.txt")).unwrap(),
        "Shinkansen ticket, Tokyo to Kyoto"
    );
    // The index has what came in.
    assert_eq!(
        by_path(&app, &admin, "Travel/Japan trip/hotel.txt")
            .await
            .status_code(),
        200
    );
    let prompt = server.prompts.lock().unwrap().last().unwrap().clone();
    for part in [
        "Files in it:",
        "- tickets/train.txt",
        "Hotel booking in Kyoto",
    ] {
        assert!(prompt.contains(part), "{part} in {prompt}");
    }
    let undo = format!(
        "/api/placements/{}/undo",
        item["placement"]["id"].as_str().unwrap()
    );
    assert_eq!(post(&app, &undo, &admin, None).await.status_code(), 200);
    assert!(!share.0.join("Travel/Japan trip").exists());
    assert_eq!(
        by_path(&app, &admin, "Travel/Japan trip")
            .await
            .status_code(),
        404
    );

    // Split, with a path rule inside it.
    let rule = json!({ "kind": "path", "pattern": "*/private/*", "action": "by_hand" });
    assert_eq!(
        post(&app, "/api/drop-box/guard-rules", &admin, Some(rule))
            .await
            .status_code(),
        201
    );
    let response = drop_files(
        &app,
        &admin,
        &[("folders", "split")],
        &[
            ("Box/private/diary.txt", "dear diary"),
            ("Box/notes.txt", "notes"),
        ],
    )
    .await;
    let items = json(response).await;
    assert_eq!(items.as_array().unwrap().len(), 2);
    assert_eq!(items[0]["name"], "diary.txt");
    assert_eq!(items[0]["source_path"], "Box/private/diary.txt");
    let diary = settled(&app, &admin, items[0]["id"].as_str().unwrap()).await;
    assert_eq!(diary["guard"], "by_hand");
    assert_eq!(diary["status"], "needs_manual");

    // A whole folder with a file a rule keeps from the AI is placed by hand.
    let response = drop_files(&app, &admin, &[], &[("Old/private/x.txt", "x")]).await;
    let id = json(response).await[0]["id"].as_str().unwrap().to_string();
    let item = settled(&app, &admin, &id).await;
    assert_eq!(item["guard"], "by_hand");
    // Bad names are refused, and nothing is left behind.
    let response = drop_files(&app, &admin, &[], &[("Bad/../x.txt", "x")]).await;
    assert_eq!(response.status_code(), 400);
    let response = drop_files(
        &app,
        &admin,
        &[("folders", "sideways")],
        &[("A/x.txt", "x")],
    )
    .await;
    assert_eq!(response.status_code(), 400);
}

/// A folder made in the WebDAV drop box is one item, filled by the files
/// written into it, shown while it waits, and placed whole.
#[tokio::test]
async fn the_webdav_drop_box_takes_folders() {
    let app = spawn_app().await;
    let (admin, share) = with_files(&app, &[("Travel/a.txt", "a")]).await;
    let server = model_server::spawn().await;
    connect_ai(&app, &admin, &server).await;
    let body = json!({ "name": "Finder", "scope": "write", "kind": "app_password" });
    let password = json(post(&app, "/api/auth/api-keys", &admin, Some(body)).await).await["token"]
        .as_str()
        .unwrap()
        .to_string();
    let auth = basic(&password);
    let dav = |method: &str, path: &str| {
        app.server
            .method(Method::from_bytes(method.as_bytes()).unwrap(), path)
            .add_header("authorization", &auth)
    };
    assert_eq!(dav("MKCOL", "/dav-inbox/Trip/").await.status_code(), 201);
    assert_eq!(
        dav("MKCOL", "/dav-inbox/Trip/day%201/").await.status_code(),
        201
    );
    assert_eq!(
        dav("PUT", "/dav-inbox/Trip/day%201/ticket.txt")
            .bytes("Train ticket".into())
            .await
            .status_code(),
        201
    );
    assert_eq!(
        dav("PUT", "/dav-inbox/Trip/._ticket.txt")
            .bytes("x".into())
            .await
            .status_code(),
        201
    );
    assert_eq!(
        dav("PUT", "/dav-inbox/Trip/notes.txt")
            .bytes("Notes".into())
            .await
            .status_code(),
        201
    );
    let listing = dav("PROPFIND", "/dav-inbox/Trip/")
        .add_header("depth", "1")
        .await
        .text();
    assert!(
        listing.contains("day%201") && listing.contains("notes.txt"),
        "{listing}"
    );
    assert!(!listing.contains("._ticket"), "{listing}");
    assert_eq!(
        dav("GET", "/dav-inbox/Trip/day%201/ticket.txt")
            .await
            .text(),
        "Train ticket"
    );
    assert_eq!(dav("MKCOL", "/dav-inbox/Trip/").await.status_code(), 405);

    server.answer(json!({ "folder": "Travel", "reason": "A trip.", "confidence": 0.9 }));
    let item = analysed(&app, &admin, "Trip").await;
    assert_eq!(item["kind"], "folder");
    assert_eq!(item["size"], 17);
    assert_eq!(item["placement"]["path"], "Travel/Trip", "{item}");
    assert_eq!(
        std::fs::read_to_string(share.0.join("Travel/Trip/day 1/ticket.txt")).unwrap(),
        "Train ticket"
    );
    // Once analysed, it takes nothing more.
    assert!(
        dav("PUT", "/dav-inbox/Trip/late.txt")
            .bytes("x".into())
            .await
            .status_code()
            .as_u16()
            >= 400
    );
}

/// Replace a file's contents over the API, at its current version.
async fn replace(app: &TestApp, session: &str, id: &str, content: &str) -> Value {
    let node = json(
        request(
            app,
            Method::GET,
            &format!("/api/files/{id}"),
            Some(session),
            None,
        )
        .await,
    )
    .await;
    let response = app
        .server
        .put(&format!("/api/files/{id}/content"))
        .add_header("cookie", session)
        .add_header("if-match", format!("\"{}\"", node["version"]))
        .bytes(content.to_string().into())
        .await;
    assert_eq!(response.status_code(), 200);
    json(response).await
}

async fn versions_of(app: &TestApp, session: &str, id: &str) -> Vec<Value> {
    let response = request(
        app,
        Method::GET,
        &format!("/api/files/{id}/versions"),
        Some(session),
        None,
    )
    .await;
    assert_eq!(response.status_code(), 200);
    json(response).await.as_array().unwrap().clone()
}

/// A replaced file keeps its earlier contents: listed, downloaded,
/// restored (which keeps the current one in turn) and deleted; they go to
/// the trash with the file or its folder and come back with a restore.
#[tokio::test]
async fn versions_are_kept_restored_and_follow_the_trash() {
    let app = spawn_app().await;
    let (admin, share) = with_files(&app, &[("Docs/a.txt", "one")]).await;
    let a = json(by_path(&app, &admin, "/Docs/a.txt").await).await;
    let id = a["id"].as_str().unwrap().to_string();

    replace(&app, &admin, &id, "two").await;
    // The same bytes again: nothing to keep.
    replace(&app, &admin, &id, "two").await;
    let current = replace(&app, &admin, &id, "three!").await;
    let versions = versions_of(&app, &admin, &id).await;
    assert_eq!(versions.len(), 2, "{versions:?}");
    assert_eq!(versions[0]["size"], json!(3));
    assert_eq!(versions[0]["source"], json!("web"));
    let oldest = versions[1]["id"].as_str().unwrap().to_string();
    let content = app
        .server
        .get(&format!("/api/files/{id}/versions/{oldest}/content"))
        .add_header("cookie", &admin)
        .await;
    assert_eq!(content.status_code(), 200);
    assert_eq!(content.text(), "one");
    assert!(
        content.headers()["content-disposition"]
            .to_str()
            .unwrap()
            .contains("a.txt")
    );

    // A restore at an old version is refused; at the current one it puts
    // "one" back and keeps "three!".
    let restore = format!("/api/files/{id}/versions/{oldest}/restore");
    let stale = request(
        &app,
        Method::POST,
        &format!("{restore}?if_version=1"),
        Some(&admin),
        None,
    )
    .await;
    assert_eq!(stale.status_code(), 409);
    let response = request(
        &app,
        Method::POST,
        &format!("{restore}?if_version={}", current["version"]),
        Some(&admin),
        None,
    )
    .await;
    assert_eq!(response.status_code(), 200);
    assert_eq!(
        std::fs::read_to_string(share.0.join("Docs/a.txt")).unwrap(),
        "one"
    );
    let versions = versions_of(&app, &admin, &id).await;
    assert_eq!(versions.len(), 3);
    assert_eq!(versions[0]["size"], json!(6));

    let delete = format!(
        "/api/files/{id}/versions/{}",
        versions[0]["id"].as_str().unwrap()
    );
    assert_eq!(
        request(&app, Method::DELETE, &delete, Some(&admin), None)
            .await
            .status_code(),
        204
    );
    assert_eq!(
        request(&app, Method::DELETE, &delete, Some(&admin), None)
            .await
            .status_code(),
        404
    );
    assert_eq!(versions_of(&app, &admin, &id).await.len(), 2);

    // The folder to the trash: the file's versions with it. Restored, the
    // file is indexed anew and finds them again.
    let docs = json(by_path(&app, &admin, "/Docs").await).await;
    let docs_id = docs["id"].as_str().unwrap().to_string();
    let path = format!("/api/files/{docs_id}?if_version={}", docs["version"]);
    assert_eq!(
        request(&app, Method::DELETE, &path, Some(&admin), None)
            .await
            .status_code(),
        200
    );
    let gone = request(
        &app,
        Method::GET,
        &format!("/api/files/{id}/versions"),
        Some(&admin),
        None,
    )
    .await;
    assert_eq!(gone.status_code(), 404);
    let path = format!("/api/trash/{docs_id}/restore");
    assert_eq!(
        request(&app, Method::POST, &path, Some(&admin), None)
            .await
            .status_code(),
        200
    );
    let a = json(by_path(&app, &admin, "/Docs/a.txt").await).await;
    let id = a["id"].as_str().unwrap().to_string();
    assert_eq!(versions_of(&app, &admin, &id).await.len(), 2);

    // The file to the trash and the trash emptied: its versions go for good.
    let path = format!("/api/files/{id}?if_version={}", a["version"]);
    assert_eq!(
        request(&app, Method::DELETE, &path, Some(&admin), None)
            .await
            .status_code(),
        200
    );
    let emptied = json(request(&app, Method::DELETE, "/api/trash", Some(&admin), None).await).await;
    assert_eq!(emptied["deleted"], json!(1));
    let left: i64 = sqlx::query_scalar("SELECT count(*) FROM versions")
        .fetch_one(app.state.db.reader())
        .await
        .unwrap();
    assert_eq!(left, 0);
}

/// Each user's trash and version rules, over the admin's defaults, under
/// the variables; the sweep applies them.
#[tokio::test]
async fn retention_rules_are_set_locked_and_applied() {
    let app = spawn_app().await;
    let (admin, _share) = with_files(&app, &[("old.txt", "old"), ("new.txt", "new")]).await;
    let view = json(request(&app, Method::GET, "/api/retention", Some(&admin), None).await).await;
    assert_eq!(view["rules"]["trash_days"], json!(30));
    assert_eq!(view["locked"], json!([]));

    // The admin's default, then the user's own, then back to the default.
    let response = request(
        &app,
        Method::PUT,
        "/api/superadmin/retention",
        Some(&admin),
        Some(json!({ "trash_days": 14 })),
    )
    .await;
    assert_eq!(response.status_code(), 200);
    let view = json(request(&app, Method::GET, "/api/retention", Some(&admin), None).await).await;
    assert_eq!(view["rules"]["trash_days"], json!(14));
    assert_eq!(view["defaults"]["trash_days"], json!(14));
    let mine = |body: Value| {
        request(
            &app,
            Method::PUT,
            "/api/retention",
            Some(&admin),
            Some(body),
        )
    };
    let view = json(mine(json!({ "trash_days": 2, "versions_months": 0 })).await).await;
    assert_eq!(view["rules"]["trash_days"], json!(2));
    assert_eq!(view["rules"]["versions_hours"], json!(24));
    assert_eq!(mine(json!({ "trash_days": -1 })).await.status_code(), 400);
    assert_eq!(mine(json!({ "nonsense": 1 })).await.status_code(), 400);
    let view = json(mine(json!({ "trash_days": null })).await).await;
    assert_eq!(view["rules"]["trash_days"], json!(14));
    assert_eq!(view["rules"]["versions_months"], json!(0));
    json(mine(json!({ "trash_days": 1 })).await).await;

    // The sweep deletes what was trashed more than a day ago.
    for name in ["/old.txt", "/new.txt"] {
        let node = json(by_path(&app, &admin, name).await).await;
        let path = format!(
            "/api/files/{}?if_version={}",
            node["id"].as_str().unwrap(),
            node["version"]
        );
        assert_eq!(
            request(&app, Method::DELETE, &path, Some(&admin), None)
                .await
                .status_code(),
            200
        );
    }
    sqlx::query("UPDATE trash SET deleted_at = '2020-01-01T00:00:00.000Z' WHERE name = 'old.txt'")
        .execute(app.state.db.writer())
        .await
        .unwrap();
    organizer::retention::sweep(&app.state).await.unwrap();
    let trash = json(request(&app, Method::GET, "/api/trash", Some(&admin), None).await).await;
    let names: Vec<_> = trash
        .as_array()
        .unwrap()
        .iter()
        .map(|t| t["name"].as_str().unwrap())
        .collect();
    assert_eq!(names, ["new.txt"]);

    // A variable wins over both and locks the rule.
    app.state
        .files
        .set_retention_env(organizer::retention::Partial {
            trash_days: Some(7),
            ..Default::default()
        });
    let view = json(request(&app, Method::GET, "/api/retention", Some(&admin), None).await).await;
    assert_eq!(view["rules"]["trash_days"], json!(7));
    assert_eq!(view["locked"], json!(["trash_days"]));
    assert_eq!(mine(json!({ "trash_days": 3 })).await.status_code(), 400);
    assert_eq!(mine(json!({ "versions_days": 3 })).await.status_code(), 200);
}

/// Backups into a local repository: set up and tested by the admin, run,
/// listed, browsed and restored from by a user, next to the current file.
#[tokio::test]
async fn backups_are_made_browsed_and_restored_from() {
    let app = spawn_app().await;
    let (admin, share) =
        with_files(&app, &[("Taxes/return.pdf", "first"), ("notes.txt", "n")]).await;
    let repo = TempDir::new();
    let repository = repo.0.join("repo");

    // No backups yet: nothing to list.
    assert_eq!(
        request(&app, Method::GET, "/api/backups", Some(&admin), None)
            .await
            .status_code(),
        404
    );
    let put = |body: Value| {
        request(
            &app,
            Method::PUT,
            "/api/superadmin/backup",
            Some(&admin),
            Some(body),
        )
    };
    assert_eq!(
        put(json!({ "repository": "relative/path", "password": "pw" }))
            .await
            .status_code(),
        400
    );
    assert_eq!(
        put(json!({ "schedule": "weekly" })).await.status_code(),
        400
    );
    let test = request(
        &app,
        Method::POST,
        "/api/superadmin/backup/test",
        Some(&admin),
        Some(json!({ "repository": repository.to_str().unwrap(), "password": "correct horse" })),
    )
    .await;
    assert_eq!(test.status_code(), 200, "{}", test.text());
    assert_eq!(json(test).await["exists"], json!(false));
    let view = json(
        put(json!({ "repository": repository.to_str().unwrap(), "password": "correct horse" }))
            .await,
    )
    .await;
    assert_eq!(view["enabled"], json!(true));
    assert_eq!(view["password_set"], json!(true));
    assert!(view.get("password").is_none());
    assert!(view["next_run"].is_string());

    organizer::backup::run::backup(&app.state).await.unwrap();
    let view = json(
        request(
            &app,
            Method::GET,
            "/api/superadmin/backup",
            Some(&admin),
            None,
        )
        .await,
    )
    .await;
    assert_eq!(view["status"]["last_error"], Value::Null);
    assert_eq!(view["status"]["snapshots"], json!(1));
    assert!(view["status"]["last_check"].is_string());
    // A wrong password does not open it.
    let wrong = request(
        &app,
        Method::POST,
        "/api/superadmin/backup/test",
        Some(&admin),
        Some(json!({ "password": "wrong" })),
    )
    .await;
    assert_eq!(wrong.status_code(), 400);

    let snapshots =
        json(request(&app, Method::GET, "/api/backups", Some(&admin), None).await).await;
    let snapshot = snapshots[0]["id"].as_str().unwrap().to_string();
    let root = json(
        request(
            &app,
            Method::GET,
            &format!("/api/backups/{snapshot}/files"),
            Some(&admin),
            None,
        )
        .await,
    )
    .await;
    let names: Vec<_> = root
        .as_array()
        .unwrap()
        .iter()
        .map(|e| e["name"].as_str().unwrap())
        .collect();
    assert_eq!(names, ["Taxes", "notes.txt"]);
    let taxes = json(
        request(
            &app,
            Method::GET,
            &format!("/api/backups/{snapshot}/files?path=Taxes"),
            Some(&admin),
            None,
        )
        .await,
    )
    .await;
    assert_eq!(taxes[0]["name"], json!("return.pdf"));
    assert_eq!(taxes[0]["size"], json!(5));
    let up = request(
        &app,
        Method::GET,
        &format!("/api/backups/{snapshot}/files?path=../x"),
        Some(&admin),
        None,
    )
    .await;
    assert_eq!(up.status_code(), 400);

    // Changed since: the backup's copy comes back beside it.
    std::fs::write(share.0.join("Taxes/return.pdf"), "second").unwrap();
    let restore_path = format!("/api/backups/{snapshot}/restore");
    let restore = |path: &str| {
        request(
            &app,
            Method::POST,
            &restore_path,
            Some(&admin),
            Some(json!({ "path": path })),
        )
    };
    let response = restore("Taxes/return.pdf").await;
    assert_eq!(response.status_code(), 200, "{}", response.text());
    let date = jiff::Zoned::now().strftime("%Y-%m-%d").to_string();
    let restored = json(response).await;
    assert_eq!(restored["name"], json!(format!("return ({date}).pdf")));
    assert_eq!(
        std::fs::read_to_string(share.0.join(format!("Taxes/return ({date}).pdf"))).unwrap(),
        "first"
    );
    assert_eq!(
        std::fs::read_to_string(share.0.join("Taxes/return.pdf")).unwrap(),
        "second"
    );
    // Again: another name. A folder comes back whole.
    let again = json(restore("Taxes/return.pdf").await).await;
    assert_eq!(again["name"], json!(format!("return ({date} 2).pdf")));
    let folder = json(restore("Taxes").await).await;
    assert_eq!(folder["kind"], json!("folder"));
    assert_eq!(
        std::fs::read_to_string(share.0.join(format!("Taxes ({date})/return.pdf"))).unwrap(),
        "first"
    );
    assert_eq!(restore("nope.txt").await.status_code(), 400);

    // A variable locks its field.
    let locked = spawn(Options {
        backup: organizer::backup::Env {
            schedule: Some("every 6h".into()),
            ..Default::default()
        },
        ..Options::default()
    })
    .await;
    let session = admin_session(&locked).await;
    let view = json(
        request(
            &locked,
            Method::GET,
            "/api/superadmin/backup",
            Some(&session),
            None,
        )
        .await,
    )
    .await;
    assert_eq!(view["schedule"], json!("every 6h"));
    assert_eq!(view["locked"], json!(["schedule"]));
    let response = request(
        &locked,
        Method::PUT,
        "/api/superadmin/backup",
        Some(&session),
        Some(json!({ "schedule": "daily 01:00" })),
    )
    .await;
    assert_eq!(response.status_code(), 400);
}

/// Passkeys: added by a signed-in user, then signing in alone (no code),
/// once per challenge; listed, removed; off without PUBLIC_URL.
#[tokio::test]
async fn passkeys_are_added_and_sign_in_alone() {
    use webauthn_authenticator_rs::WebauthnAuthenticator;
    use webauthn_authenticator_rs::softpasskey::SoftPasskey;
    use webauthn_rs::prelude::{
        CreationChallengeResponse, PublicKeyCredential, RequestChallengeResponse, Url,
    };

    // Without PUBLIC_URL: none.
    let plain = spawn_app().await;
    let session = json(request(&plain, Method::GET, "/api/auth/session", None, None).await).await;
    assert_eq!(session["passkeys"], json!(false));
    let start = request(
        &plain,
        Method::POST,
        "/api/auth/login/passkey/start",
        None,
        None,
    )
    .await;
    assert_eq!(start.status_code(), 403);

    let origin = "http://localhost:8000";
    let app = spawn(Options {
        public_url: Some(origin.into()),
        ..Options::default()
    })
    .await;
    let admin = admin_session(&app).await;
    let me = json(request(&app, Method::GET, "/api/auth/session", Some(&admin), None).await).await;
    assert_eq!(me["passkeys"], json!(true));
    let user_id: uuid::Uuid = me["user"]["id"].as_str().unwrap().parse().unwrap();
    let origin = Url::parse(origin).unwrap();
    let mut authenticator = SoftPasskey::new(true);

    // Add one.
    let started = json(
        request(
            &app,
            Method::POST,
            "/api/auth/passkeys/register",
            Some(&admin),
            None,
        )
        .await,
    )
    .await;
    let options: CreationChallengeResponse =
        serde_json::from_value(started["options"].clone()).unwrap();
    let credential = authenticator
        .do_registration(origin.clone(), options)
        .unwrap();
    let credential_id = serde_json::to_value(&credential).unwrap()["rawId"].clone();
    let finish = |name: &str| {
        json!({
            "challenge_id": started["challenge_id"],
            "name": name,
            "credential": serde_json::to_value(&credential).unwrap(),
        })
    };
    let added = request(
        &app,
        Method::POST,
        "/api/auth/passkeys",
        Some(&admin),
        Some(finish("Laptop")),
    )
    .await;
    assert_eq!(added.status_code(), 201, "{}", added.text());
    let passkey_id = json(added).await["id"].as_str().unwrap().to_string();
    // The challenge is used once.
    let again = request(
        &app,
        Method::POST,
        "/api/auth/passkeys",
        Some(&admin),
        Some(finish("Again")),
    )
    .await;
    assert_eq!(again.status_code(), 400);

    // Sign in with it, as a discoverable passkey answers: the credential
    // and the user's handle (not covered by the signature).
    let sign_in = |authenticator: &mut SoftPasskey, started: &Value| {
        let mut options = started["options"].clone();
        options["publicKey"]["allowCredentials"] =
            json!([{ "type": "public-key", "id": credential_id.clone() }]);
        let options: RequestChallengeResponse = serde_json::from_value(options).unwrap();
        let mut assertion: PublicKeyCredential = authenticator
            .do_authentication(origin.clone(), options)
            .unwrap();
        assertion.response.user_handle = Some(user_id.as_bytes().to_vec());
        json!({ "challenge_id": started["challenge_id"], "credential": serde_json::to_value(&assertion).unwrap() })
    };
    let started = json(
        request(
            &app,
            Method::POST,
            "/api/auth/login/passkey/start",
            None,
            None,
        )
        .await,
    )
    .await;
    let body = sign_in(&mut authenticator, &started);
    let response = request(
        &app,
        Method::POST,
        "/api/auth/login/passkey",
        None,
        Some(body.clone()),
    )
    .await;
    assert_eq!(response.status_code(), 204, "{}", response.text());
    let session = cookie(&response);
    let who =
        json(request(&app, Method::GET, "/api/auth/session", Some(&session), None).await).await;
    assert_eq!(who["user"]["email"], json!("admin@example.com"));
    // A replayed answer is refused.
    let replay = request(
        &app,
        Method::POST,
        "/api/auth/login/passkey",
        None,
        Some(body),
    )
    .await;
    assert_eq!(replay.status_code(), 401);

    let list =
        json(request(&app, Method::GET, "/api/auth/passkeys", Some(&admin), None).await).await;
    assert_eq!(list[0]["name"], json!("Laptop"));
    assert!(list[0]["last_used_at"].is_string());

    // Removed, it signs in no more.
    let path = format!("/api/auth/passkeys/{passkey_id}");
    assert_eq!(
        request(&app, Method::DELETE, &path, Some(&admin), None)
            .await
            .status_code(),
        204
    );
    let started = json(
        request(
            &app,
            Method::POST,
            "/api/auth/login/passkey/start",
            None,
            None,
        )
        .await,
    )
    .await;
    let body = sign_in(&mut authenticator, &started);
    let response = request(
        &app,
        Method::POST,
        "/api/auth/login/passkey",
        None,
        Some(body),
    )
    .await;
    assert_eq!(response.status_code(), 401);
}

/// A link waiting for the AI (none is set up) is placed by hand: its file is
/// made then, as analysis would have. A null `new_folders` is no folders.
#[tokio::test]
async fn a_waiting_link_is_placed_by_hand() {
    let app = spawn_app().await;
    let (admin, share) = with_files(&app, &[("Recipes/notes.txt", "n")]).await;
    let response = post(
        &app,
        "/api/drop-box/links",
        &admin,
        Some(json!({ "url": "https://example.com/recipes/bread", "mode": "bookmark" })),
    )
    .await;
    assert_eq!(response.status_code(), 201);
    let id = json(response).await["id"].as_str().unwrap().to_string();
    // Analysis leaves it waiting.
    for _ in 0..100 {
        let item = json(
            request(
                &app,
                Method::GET,
                &format!("/api/drop-box/items/{id}"),
                Some(&admin),
                None,
            )
            .await,
        )
        .await;
        if item["status"] == "waiting" {
            break;
        }
        tokio::time::sleep(std::time::Duration::from_millis(20)).await;
    }
    let folder = node_id(&app, &admin, "/Recipes").await;
    let response = item_action(
        &app,
        &admin,
        &id,
        "place",
        Some(json!({ "folder_id": folder, "new_folders": null })),
    )
    .await;
    assert_eq!(response.status_code(), 200, "{}", response.text());
    let placed: Vec<_> = std::fs::read_dir(share.0.join("Recipes"))
        .unwrap()
        .map(|e| e.unwrap().file_name().to_string_lossy().into_owned())
        .filter(|n| n.ends_with(".url"))
        .collect();
    assert_eq!(placed.len(), 1, "{placed:?}");
    let text = std::fs::read_to_string(share.0.join("Recipes").join(&placed[0])).unwrap();
    assert!(text.contains("https://example.com/recipes/bread"), "{text}");
}

/// An app knows who it signed in as with its token, and signs out by
/// revoking it, read-only tokens included.
#[tokio::test]
async fn an_app_sees_its_user_and_revokes_its_own_token() {
    let app = spawn_app().await;
    let admin = admin_session(&app).await;
    for scope in ["read", "write"] {
        let key = create_key(&app, &admin, scope).await["token"]
            .as_str()
            .unwrap()
            .to_string();
        let session =
            json(with_key(&app, Method::GET, "/api/auth/session", &key, None).await).await;
        assert_eq!(
            session["user"]["email"],
            json!("admin@example.com"),
            "{scope}"
        );
        let response = with_key(&app, Method::DELETE, "/api/auth/device", &key, None).await;
        assert_eq!(response.status_code(), 204, "{scope}");
        let session =
            json(with_key(&app, Method::GET, "/api/auth/session", &key, None).await).await;
        assert_eq!(session["user"], Value::Null, "{scope}");
        let files = with_key(&app, Method::GET, "/api/files/root", &key, None).await;
        assert_eq!(files.status_code(), 401, "{scope}");
    }
    // Without a token: nothing to do.
    let response = request(&app, Method::DELETE, "/api/auth/device", None, None).await;
    assert_eq!(response.status_code(), 204);
}

// ---------------------------------------------------------------------------
// More than one user (docs/M6A_DESIGN.md)
// ---------------------------------------------------------------------------

/// A new invite of the admin's: its JSON (with the token).
async fn invite(app: &TestApp, admin: &str, body: Value) -> Value {
    let response = post(app, "/api/superadmin/invites", admin, Some(body)).await;
    assert_eq!(response.status_code(), 201);
    json(response).await
}

/// Accept an invite with an email and the test password.
async fn accept(app: &TestApp, token: &str, email: &str) -> Response {
    request(
        app,
        Method::POST,
        &format!("/api/auth/invite/{token}"),
        None,
        Some(json!({ "email": email, "password": "correct horse battery" })),
    )
    .await
}

/// A regular user made through an invite, signed in: their session and id.
async fn invited_user(app: &TestApp, admin: &str, email: &str) -> (String, String) {
    let made = invite(app, admin, json!({ "email": email })).await;
    let response = accept(app, made["token"].as_str().unwrap(), email).await;
    assert_eq!(response.status_code(), 201);
    let session = cookie(&response);
    let id = session_of(app, Some(&session)).await["user"]["id"]
        .as_str()
        .unwrap()
        .to_string();
    (session, id)
}

/// The admin's list, by email.
async fn users_by_email(app: &TestApp, admin: &str) -> std::collections::HashMap<String, Value> {
    let response = request(app, Method::GET, "/api/superadmin/users", Some(admin), None).await;
    assert_eq!(response.status_code(), 200);
    json(response)
        .await
        .as_array()
        .unwrap()
        .iter()
        .map(|u| (u["email"].as_str().unwrap().to_string(), u.clone()))
        .collect()
}

async fn patch_user(app: &TestApp, admin: &str, id: &str, body: Value) -> Response {
    request(
        app,
        Method::PATCH,
        &format!("/api/superadmin/users/{id}"),
        Some(admin),
        Some(body),
    )
    .await
}

/// An invite link works once, for 7 days, for its email when it names one,
/// and makes an admin when it says so; revoked, it stops working. Only an
/// admin makes them.
#[tokio::test]
async fn invites_make_accounts_once() {
    let app = spawn_app().await;
    let admin = admin_session(&app).await;

    let for_bea = invite(&app, &admin, json!({ "email": "Bea@Example.com" })).await;
    assert_eq!(for_bea["email"], json!("bea@example.com"));
    assert_eq!(for_bea["is_superadmin"], json!(false));
    let token = for_bea["token"].as_str().unwrap().to_string();
    let expires: jiff::Timestamp = for_bea["expires_at"].as_str().unwrap().parse().unwrap();
    let days = (expires - jiff::Timestamp::now())
        .total(jiff::Unit::Hour)
        .unwrap()
        / 24.0;
    assert!((6.9..=7.0).contains(&days), "{days}");
    // An email that has an account already is refused.
    let response = post(
        &app,
        "/api/superadmin/invites",
        &admin,
        Some(json!({ "email": "admin@example.com" })),
    )
    .await;
    assert_eq!(response.status_code(), 409);

    // The page knows its email; nobody is signed in by looking.
    let info = json(
        request(
            &app,
            Method::GET,
            &format!("/api/auth/invite/{token}"),
            None,
            None,
        )
        .await,
    )
    .await;
    assert_eq!(info["email"], json!("bea@example.com"));
    // Another email, or a short password: refused, and the invite still works.
    let response = accept(&app, &token, "eve@example.com").await;
    assert_eq!(response.status_code(), 404);
    let response = request(
        &app,
        Method::POST,
        &format!("/api/auth/invite/{token}"),
        None,
        Some(json!({ "email": "bea@example.com", "password": "short" })),
    )
    .await;
    assert_eq!(response.status_code(), 400);
    let response = accept(&app, &token, "bea@example.com").await;
    assert_eq!(response.status_code(), 201);
    let bea = cookie(&response);
    let user = &session_of(&app, Some(&bea)).await["user"];
    assert_eq!(user["email"], json!("bea@example.com"));
    assert_eq!(user["is_superadmin"], json!(false));
    // Used: gone from the page, the list and the API.
    let response = request(
        &app,
        Method::GET,
        &format!("/api/auth/invite/{token}"),
        None,
        None,
    )
    .await;
    assert_eq!(response.status_code(), 404);
    assert_eq!(
        accept(&app, &token, "bea@example.com").await.status_code(),
        404
    );
    // Bea has a tree of her own, apart from the admin's.
    let root = json(request(&app, Method::GET, "/api/files/root", Some(&bea), None).await).await;
    let admin_root =
        json(request(&app, Method::GET, "/api/files/root", Some(&admin), None).await).await;
    assert_ne!(root["id"], admin_root["id"]);

    // Anyone with the link, as an admin.
    let anyone = invite(&app, &admin, json!({ "is_superadmin": true })).await;
    assert_eq!(anyone["email"], Value::Null);
    let pending = json(
        request(
            &app,
            Method::GET,
            "/api/superadmin/invites",
            Some(&admin),
            None,
        )
        .await,
    )
    .await;
    assert_eq!(pending.as_array().unwrap().len(), 1);
    assert_eq!(pending[0]["token"], anyone["token"]);
    let response = accept(&app, anyone["token"].as_str().unwrap(), "cy@example.com").await;
    assert_eq!(response.status_code(), 201);
    let cy = cookie(&response);
    assert_eq!(
        session_of(&app, Some(&cy)).await["user"]["is_superadmin"],
        json!(true)
    );

    // Revoked: the link stops working.
    let revoked = invite(&app, &admin, json!({})).await;
    let response = request(
        &app,
        Method::DELETE,
        &format!(
            "/api/superadmin/invites/{}",
            revoked["id"].as_str().unwrap()
        ),
        Some(&admin),
        None,
    )
    .await;
    assert_eq!(response.status_code(), 204);
    assert_eq!(
        accept(&app, revoked["token"].as_str().unwrap(), "dee@example.com")
            .await
            .status_code(),
        404
    );

    // A regular user makes no invites and sees no users.
    let response = post(&app, "/api/superadmin/invites", &bea, Some(json!({}))).await;
    assert_eq!(response.status_code(), 403);
    let response = request(&app, Method::GET, "/api/superadmin/users", Some(&bea), None).await;
    assert_eq!(response.status_code(), 403);
}

/// Open sign-up follows its switch, which `SIGNUP_OPEN` locks; the login
/// page is told. With `OIDC_ONLY`, no password makes an account.
#[tokio::test]
async fn open_signup_follows_its_switch_and_variable() {
    let app = spawn_app().await;
    let signup = |email: &'static str| {
        request(
            &app,
            Method::POST,
            "/api/auth/signup",
            None,
            Some(json!({ "email": email, "password": "correct horse battery" })),
        )
    };
    // Nobody before the first admin.
    assert_eq!(signup("early@example.com").await.status_code(), 403);
    let admin = admin_session(&app).await;
    assert_eq!(session_of(&app, None).await["signup"], json!(false));
    assert_eq!(signup("dan@example.com").await.status_code(), 403);

    let joining = json(
        request(
            &app,
            Method::GET,
            "/api/superadmin/signup",
            Some(&admin),
            None,
        )
        .await,
    )
    .await;
    assert_eq!(
        joining,
        json!({ "open_signup": false, "oidc_create_users": false, "locked": [] })
    );
    let response = put(
        &app,
        "/api/superadmin/signup",
        &admin,
        json!({ "open_signup": true }),
    )
    .await;
    assert_eq!(response.status_code(), 200);
    assert_eq!(json(response).await["open_signup"], json!(true));
    assert_eq!(session_of(&app, None).await["signup"], json!(true));

    let response = signup("dan@example.com").await;
    assert_eq!(response.status_code(), 201);
    let dan = cookie(&response);
    let user = &session_of(&app, Some(&dan)).await["user"];
    assert_eq!(user["email"], json!("dan@example.com"));
    assert_eq!(user["is_superadmin"], json!(false));
    assert_eq!(signup("dan@example.com").await.status_code(), 409);
    // Only an admin turns it off.
    let response = put(
        &app,
        "/api/superadmin/signup",
        &dan,
        json!({ "open_signup": false }),
    )
    .await;
    assert_eq!(response.status_code(), 403);

    // The variable wins and locks the switch; the other stays free.
    let app = spawn(Options {
        joining: organizer::users::Env {
            open_signup: Some(false),
            oidc_create_users: None,
        },
        ..Options::default()
    })
    .await;
    let admin = admin_session(&app).await;
    let response = put(
        &app,
        "/api/superadmin/signup",
        &admin,
        json!({ "open_signup": true }),
    )
    .await;
    assert_eq!(response.status_code(), 400);
    assert!(
        json(response).await["error"]
            .as_str()
            .unwrap()
            .contains("SIGNUP_OPEN")
    );
    let response = put(
        &app,
        "/api/superadmin/signup",
        &admin,
        json!({ "oidc_create_users": true }),
    )
    .await;
    assert_eq!(
        json(response).await,
        json!({ "open_signup": false, "oidc_create_users": true, "locked": ["open_signup"] })
    );

    // Open, but passwords off: no password makes an account, invites
    // included.
    let app = spawn(Options {
        oidc: oidc::Env {
            only: true,
            ..oidc::Env::default()
        },
        joining: organizer::users::Env {
            open_signup: Some(true),
            oidc_create_users: None,
        },
        ..Options::default()
    })
    .await;
    let admin = admin_session(&app).await;
    let response = request(
        &app,
        Method::POST,
        "/api/auth/signup",
        None,
        Some(json!({ "email": "dan@example.com", "password": "correct horse battery" })),
    )
    .await;
    assert_eq!(response.status_code(), 403);
    let made = invite(&app, &admin, json!({})).await;
    let response = accept(&app, made["token"].as_str().unwrap(), "dan@example.com").await;
    assert_eq!(response.status_code(), 403);
    // Without single sign-on set up, the login page offers no sign-up.
    assert_eq!(session_of(&app, None).await["signup"], json!(false));
}

/// Single sign-on makes an account for a verified email nobody has when
/// the admin allows it, or with the invite the login started from (for its
/// email); otherwise it refuses, as before. With `OIDC_ONLY`, open sign-up
/// is single sign-on making accounts.
#[tokio::test]
async fn single_sign_on_makes_accounts_when_allowed_or_invited() {
    let app = spawn_app().await;
    let admin = admin_session(&app).await;
    let provider = provider::spawn().await;
    configure(&app, &admin, &provider, json!({})).await;
    let p = &provider;
    let new_account = move |subject: &'static str, email: &'static str| {
        move |login: &Login| provider::Claims::new(p, subject, email, &login.nonce)
    };

    let refused = sign_in(&app, &provider, new_account("acct-n", "new@example.com")).await;
    assert_eq!(refused, Err("/login?error=unknown_user".into()));

    put(
        &app,
        "/api/superadmin/signup",
        &admin,
        json!({ "oidc_create_users": true }),
    )
    .await;
    let session = sign_in(&app, &provider, new_account("acct-n", "new@example.com"))
        .await
        .expect("an account made");
    let user = &session_of(&app, Some(&session)).await["user"];
    assert_eq!(user["email"], json!("new@example.com"));
    assert_eq!(user["is_superadmin"], json!(false));
    assert_eq!(user["has_password"], json!(false));
    assert_eq!(user["single_sign_on"], json!(true));
    // An unverified email makes nothing.
    let unverified = sign_in(&app, &provider, |login| provider::Claims {
        email_verified: false,
        ..provider::Claims::new(&provider, "acct-u", "unverified@example.com", &login.nonce)
    })
    .await;
    assert_eq!(unverified, Err("/login?error=email".into()));
    put(
        &app,
        "/api/superadmin/signup",
        &admin,
        json!({ "oidc_create_users": false }),
    )
    .await;

    // An invite, through single sign-on: an admin, as it says.
    let made = invite(
        &app,
        &admin,
        json!({ "email": "inv@example.com", "is_superadmin": true }),
    )
    .await;
    let token = made["token"].as_str().unwrap();
    let with_invite = |token: String, subject: &'static str, email: &'static str| {
        let app = &app;
        let provider = &provider;
        async move {
            let login = start_login_at(app, &format!("/api/auth/oidc/login?invite={token}")).await;
            let code = provider.code_for(provider::Claims::new(
                provider,
                subject,
                email,
                &login.nonce,
            ));
            let response = callback(
                app,
                &format!("code={code}&state={}", login.state),
                Some(&login.cookie),
            )
            .await;
            match set_cookie(&response, "organizer_session") {
                Some(session) => Ok(session),
                None => Err(location(&response)),
            }
        }
    };
    // For another email: refused, and the invite is kept.
    assert_eq!(
        with_invite(token.into(), "acct-x", "other@example.com").await,
        Err("/login?error=invite".into())
    );
    let session = with_invite(token.into(), "acct-i", "inv@example.com")
        .await
        .expect("the invited account");
    let user = &session_of(&app, Some(&session)).await["user"];
    assert_eq!(user["email"], json!("inv@example.com"));
    assert_eq!(user["is_superadmin"], json!(true));
    // Used up.
    assert_eq!(
        with_invite(token.into(), "acct-j", "inv2@example.com").await,
        Err("/login?error=invite".into())
    );

    // A disabled user is refused by the provider's sign-in too.
    let id = user["id"].as_str().unwrap().to_string();
    patch_user(&app, &admin, &id, json!({ "disabled": true })).await;
    let refused = sign_in(&app, &provider, new_account("acct-i", "inv@example.com")).await;
    assert_eq!(refused, Err("/login?error=disabled".into()));

    // OIDC_ONLY: open sign-up is single sign-on making accounts.
    let app = spawn(Options {
        oidc: oidc::Env {
            only: true,
            ..oidc::Env::default()
        },
        ..Options::default()
    })
    .await;
    let admin = admin_session(&app).await;
    configure(&app, &admin, &provider, json!({})).await;
    assert_eq!(session_of(&app, None).await["signup"], json!(false));
    let refused = sign_in(&app, &provider, new_account("acct-o", "only@example.com")).await;
    assert_eq!(refused, Err("/login?error=unknown_user".into()));
    put(
        &app,
        "/api/superadmin/signup",
        &admin,
        json!({ "open_signup": true }),
    )
    .await;
    assert_eq!(session_of(&app, None).await["signup"], json!(true));
    let session = sign_in(&app, &provider, new_account("acct-o", "only@example.com"))
        .await
        .expect("an account made");
    assert_eq!(
        session_of(&app, Some(&session)).await["user"]["email"],
        json!("only@example.com")
    );
}

/// The admin's list, and what an admin does to a user: make admin, remove
/// admin, disable and enable. There is always an admin who is not disabled.
#[tokio::test]
async fn the_last_admin_cannot_be_demoted_disabled_or_removed() {
    let app = spawn_app().await;
    let admin = admin_session(&app).await;
    let (_bea, bea_id) = invited_user(&app, &admin, "bea@example.com").await;
    let users = users_by_email(&app, &admin).await;
    assert_eq!(users.len(), 2);
    let admin_id = users["admin@example.com"]["id"]
        .as_str()
        .unwrap()
        .to_string();
    let bea = &users["bea@example.com"];
    assert_eq!(bea["is_superadmin"], json!(false));
    assert_eq!(bea["disabled_at"], Value::Null);
    assert_eq!(bea["storage"], json!({ "used": 0, "limit": null }));
    assert_eq!(bea["files"]["default"], json!(true));
    assert!(bea["last_seen_at"].is_string(), "seen when she signed in");

    // The only admin: not demoted, disabled or removed.
    for body in [
        json!({ "is_superadmin": false }),
        json!({ "disabled": true }),
    ] {
        let response = patch_user(&app, &admin, &admin_id, body).await;
        assert_eq!(response.status_code(), 409);
    }
    let response = request(
        &app,
        Method::DELETE,
        &format!("/api/superadmin/users/{admin_id}"),
        Some(&admin),
        None,
    )
    .await;
    assert_eq!(response.status_code(), 409);

    // With a second admin, the first may step down.
    let response = patch_user(&app, &admin, &bea_id, json!({ "is_superadmin": true })).await;
    assert_eq!(response.status_code(), 200);
    assert_eq!(json(response).await["is_superadmin"], json!(true));
    let response = patch_user(&app, &admin, &admin_id, json!({ "is_superadmin": false })).await;
    assert_eq!(response.status_code(), 200);
    // Now Bea is the last, and the former admin is refused the list.
    let response = request(
        &app,
        Method::GET,
        "/api/superadmin/users",
        Some(&admin),
        None,
    )
    .await;
    assert_eq!(response.status_code(), 403);
    let bea = cookie(&password_login(&app, "bea@example.com").await);
    stepped_up(&app, &bea, 0).await;
    let response = patch_user(&app, &bea, &bea_id, json!({ "is_superadmin": false })).await;
    assert_eq!(response.status_code(), 409);
    // A disabled admin does not count.
    patch_user(&app, &bea, &admin_id, json!({ "is_superadmin": true })).await;
    let response = patch_user(&app, &bea, &admin_id, json!({ "disabled": true })).await;
    assert_eq!(response.status_code(), 200);
    assert!(json(response).await["disabled_at"].is_string());
    let response = patch_user(&app, &bea, &bea_id, json!({ "is_superadmin": false })).await;
    assert_eq!(response.status_code(), 409);
    let response = patch_user(&app, &bea, &admin_id, json!({ "disabled": false })).await;
    assert_eq!(json(response).await["disabled_at"], Value::Null);
    let response = patch_user(&app, &bea, &bea_id, json!({ "is_superadmin": false })).await;
    assert_eq!(response.status_code(), 200);
    // Nobody unknown.
    let response = patch_user(
        &app,
        &admin,
        &uuid::Uuid::now_v7().to_string(),
        json!({ "is_superadmin": true }),
    )
    .await;
    assert_eq!(response.status_code(), 404);
}

/// A disabled user cannot sign in, and their sessions, API keys, device
/// tokens and app passwords stop working; enabled again, all of it works.
#[tokio::test]
async fn a_disabled_users_sessions_and_tokens_are_refused() {
    let app = spawn_app().await;
    let admin = admin_session(&app).await;
    let (bea, bea_id) = invited_user(&app, &admin, "bea@example.com").await;
    let key = create_key(&app, &bea, "write").await["token"]
        .as_str()
        .unwrap()
        .to_string();
    let body = json!({ "name": "rclone", "scope": "write", "kind": "app_password" });
    let password = json(post(&app, "/api/auth/api-keys", &bea, Some(body)).await).await["token"]
        .as_str()
        .unwrap()
        .to_string();
    let device = |email: &str| {
        request(
            &app,
            Method::POST,
            "/api/auth/device/password",
            None,
            Some(json!({
                "email": email,
                "password": "correct horse battery",
                "device_name": "Bea's phone",
            })),
        )
    };
    let device_token = json(device("bea@example.com").await).await["token"]
        .as_str()
        .unwrap()
        .to_string();
    let works = |status: u16| status == 200 || status == 207;
    let statuses = || {
        let (app, bea, key, device_token, password) = (&app, &bea, &key, &device_token, &password);
        async move {
            [
                request(app, Method::GET, "/api/files/root", Some(bea), None)
                    .await
                    .status_code()
                    .as_u16(),
                with_key(app, Method::GET, "/api/files/root", key, None)
                    .await
                    .status_code()
                    .as_u16(),
                with_key(app, Method::GET, "/api/files/root", device_token, None)
                    .await
                    .status_code()
                    .as_u16(),
                app.server
                    .method(Method::from_bytes(b"PROPFIND").unwrap(), "/dav/")
                    .add_header("authorization", basic(password))
                    .add_header("depth", "0")
                    .await
                    .status_code()
                    .as_u16(),
            ]
        }
    };
    assert!(statuses().await.into_iter().all(works));

    patch_user(&app, &admin, &bea_id, json!({ "disabled": true })).await;
    assert_eq!(statuses().await, [401, 401, 401, 401]);
    assert_eq!(session_of(&app, Some(&bea)).await["user"], Value::Null);
    let response = password_login(&app, "bea@example.com").await;
    assert_eq!(response.status_code(), 403);
    assert!(
        json(response).await["error"]
            .as_str()
            .unwrap()
            .contains("disabled")
    );
    assert_eq!(device("bea@example.com").await.status_code(), 403);
    // A wrong password is told nothing more.
    let response = request(
        &app,
        Method::POST,
        "/api/auth/login",
        None,
        Some(json!({ "email": "bea@example.com", "password": "wrong password!" })),
    )
    .await;
    assert_eq!(response.status_code(), 401);

    // Enabled: everything works again (nothing was deleted).
    patch_user(&app, &admin, &bea_id, json!({ "disabled": false })).await;
    assert!(statuses().await.into_iter().all(works));
    assert_eq!(
        password_login(&app, "bea@example.com").await.status_code(),
        204
    );
}

/// A storage limit refuses writes through Organizer that would go past it
/// (507): plain and resumable uploads, replacing a file (its old contents
/// stay as a version), the drop box (multipart, resumable, WebDAV) and
/// WebDAV; the trash and the versions count. The user sees used of limit.
#[tokio::test]
async fn storage_limits_refuse_writes_past_them() {
    let app = spawn_app().await;
    let admin = admin_session(&app).await;
    let (bea, bea_id) = invited_user(&app, &admin, "bea@example.com").await;
    let set_limit =
        |limit: Value| patch_user(&app, &admin, &bea_id, json!({ "storage_limit": limit }));
    assert_eq!(set_limit(json!(-1)).await.status_code(), 400);
    assert_eq!(set_limit(json!(10)).await.status_code(), 200);
    let usage = || {
        let (app, bea) = (&app, &bea);
        async move { json(request(app, Method::GET, "/api/auth/storage", Some(bea), None).await).await }
    };
    assert_eq!(usage().await, json!({ "used": 0, "limit": 10 }));

    let root = json(request(&app, Method::GET, "/api/files/root", Some(&bea), None).await).await;
    let root_id = root["id"].as_str().unwrap().to_string();
    let upload = |name: &str, content: &str| {
        let (content_type, body) = multipart(name, content);
        app.server
            .post(&format!("/api/files/{root_id}/files"))
            .add_header("cookie", &bea)
            .add_header("content-type", content_type)
            .bytes(body.into())
    };
    let response = upload("a.txt", "12345").await;
    assert_eq!(response.status_code(), 201);
    let a = json(response).await;
    let a_id = a["id"].as_str().unwrap().to_string();
    assert_eq!(usage().await["used"], json!(5));
    // Plain: 5 + 6 > 10.
    let response = upload("b.txt", "123456").await;
    assert_eq!(response.status_code(), 507);
    assert!(
        json(response).await["error"]
            .as_str()
            .unwrap()
            .contains("storage limit")
    );
    // A replace counts whole: the old contents stay, as a version.
    let response = app
        .server
        .put(&format!("/api/files/{a_id}/content"))
        .add_header("cookie", &bea)
        .add_header("if-match", format!("\"{}\"", a["version"]))
        .bytes("123456".into())
        .await;
    assert_eq!(response.status_code(), 507);
    // Resumable: with a length up front, before any byte; without, at the end.
    let resumable = |query: String, length: Option<&str>, body: &'static str| {
        let mut request = app
            .server
            .post(&format!("/api/uploads?{query}"))
            .add_header("cookie", &bea)
            .add_header("upload-complete", "?1");
        if let Some(length) = length {
            request = request.add_header("upload-length", length);
        }
        request.bytes(body.into())
    };
    let response = resumable(
        format!("parent_id={root_id}&name=big.bin"),
        Some("1000"),
        "",
    )
    .await;
    assert_eq!(response.status_code(), 507);
    let response = resumable(format!("parent_id={root_id}&name=big.bin"), None, "123456").await;
    assert_eq!(response.status_code(), 507);
    let response = resumable("drop_box=true&name=big.bin".into(), None, "123456").await;
    assert_eq!(response.status_code(), 507);
    // The drop box, and both WebDAV drives.
    let response = drop_files(&app, &bea, &[], &[("big.txt", "123456")]).await;
    assert_eq!(response.status_code(), 507);
    let body = json!({ "name": "rclone", "scope": "write", "kind": "app_password" });
    let password = json(post(&app, "/api/auth/api-keys", &bea, Some(body)).await).await["token"]
        .as_str()
        .unwrap()
        .to_string();
    for path in ["/dav/big.txt", "/dav-inbox/big.txt"] {
        let response = app
            .server
            .put(path)
            .add_header("authorization", basic(&password))
            .bytes("123456".into())
            .await;
        assert_eq!(response.status_code(), 507, "{path}");
    }
    // Nothing of those was kept.
    assert_eq!(usage().await["used"], json!(5));
    // What fits goes in.
    assert_eq!(upload("c.txt", "1234").await.status_code(), 201);
    assert_eq!(usage().await["used"], json!(9));

    // The trash counts: deleting frees nothing until it is emptied.
    let response = request(
        &app,
        Method::DELETE,
        &format!("/api/files/{a_id}?if_version={}", a["version"]),
        Some(&bea),
        None,
    )
    .await;
    assert_eq!(response.status_code(), 200);
    assert_eq!(usage().await["used"], json!(9));
    let response = request(&app, Method::DELETE, "/api/trash", Some(&bea), None).await;
    assert!(response.status_code().is_success());
    assert_eq!(usage().await["used"], json!(4));

    // Versions count: no limit, a replace keeps the old contents.
    assert_eq!(set_limit(Value::Null).await.status_code(), 200);
    let c = json(by_path(&app, &bea, "/c.txt").await).await;
    replace(&app, &bea, c["id"].as_str().unwrap(), "123456789").await;
    assert_eq!(usage().await, json!({ "used": 13, "limit": null }));
    // The admin's list says the same.
    assert_eq!(
        users_by_email(&app, &admin).await["bea@example.com"]["storage"],
        json!({ "used": 13, "limit": null })
    );
}

/// Each user's files folder: by default their own in the data dir, or an
/// existing folder, never another user's. Removing a user deletes
/// Organizer's data for them and their default folder, but never an
/// existing folder.
#[tokio::test]
async fn removing_a_user_deletes_the_default_folder_but_keeps_an_existing_one() {
    let app = spawn_app().await;
    let admin = admin_session(&app).await;
    let base = app.state.files.local_base().to_path_buf();

    // Bea's default folder, with a file.
    let (bea, bea_id) = invited_user(&app, &admin, "bea@example.com").await;
    let root = json(request(&app, Method::GET, "/api/files/root", Some(&bea), None).await).await;
    let (content_type, body) = multipart("a.txt", "hello");
    let response = app
        .server
        .post(&format!(
            "/api/files/{}/files",
            root["id"].as_str().unwrap()
        ))
        .add_header("cookie", &bea)
        .add_header("content-type", content_type)
        .bytes(body.into())
        .await;
    assert_eq!(response.status_code(), 201);
    let bea_dir = base.join("users").join(&bea_id);
    assert!(bea_dir.join("files/a.txt").is_file());

    // Cy's existing folder, set by the admin.
    let (cy, cy_id) = invited_user(&app, &admin, "cy@example.com").await;
    let share = TempDir::new();
    std::fs::write(share.0.join("kept.txt"), "mine").unwrap();
    let response = patch_user(
        &app,
        &admin,
        &cy_id,
        json!({ "files_root": share.0.to_str().unwrap() }),
    )
    .await;
    assert_eq!(response.status_code(), 200);
    let files = &json(response).await["files"];
    assert_eq!(files["default"], json!(false));
    assert_eq!(files["files_root"], json!(share.0.to_str().unwrap()));
    // Nobody else may take it, or a folder inside it.
    let response = patch_user(
        &app,
        &admin,
        &bea_id,
        json!({ "files_root": share.0.to_str().unwrap() }),
    )
    .await;
    assert_eq!(response.status_code(), 400);
    let inside = share.0.join("inside");
    std::fs::create_dir_all(&inside).unwrap();
    let response = patch_user(
        &app,
        &admin,
        &bea_id,
        json!({ "files_root": inside.to_str().unwrap() }),
    )
    .await;
    assert_eq!(response.status_code(), 400);
    // Cy sees it.
    for _ in 0..100 {
        if by_path(&app, &cy, "/kept.txt").await.status_code() == 200 {
            break;
        }
        tokio::time::sleep(std::time::Duration::from_millis(20)).await;
    }
    assert_eq!(by_path(&app, &cy, "/kept.txt").await.status_code(), 200);

    // Removed: Bea's data dir goes with her default folder; Cy's folder
    // stays, and both are signed out.
    for id in [&bea_id, &cy_id] {
        let response = request(
            &app,
            Method::DELETE,
            &format!("/api/superadmin/users/{id}"),
            Some(&admin),
            None,
        )
        .await;
        assert_eq!(response.status_code(), 204);
    }
    assert!(!bea_dir.exists());
    assert_eq!(
        std::fs::read_to_string(share.0.join("kept.txt")).unwrap(),
        "mine"
    );
    assert_eq!(session_of(&app, Some(&bea)).await["user"], Value::Null);
    assert_eq!(session_of(&app, Some(&cy)).await["user"], Value::Null);
    let users = users_by_email(&app, &admin).await;
    assert_eq!(users.len(), 1);
    // Their emails are free again.
    let made = invite(&app, &admin, json!({ "email": "bea@example.com" })).await;
    assert_eq!(made["email"], json!("bea@example.com"));
    // Nobody unknown.
    let response = request(
        &app,
        Method::DELETE,
        &format!("/api/superadmin/users/{bea_id}"),
        Some(&admin),
        None,
    )
    .await;
    assert_eq!(response.status_code(), 404);
}

/// `STORAGE_URL` is the base of the file content: every user's default folder
/// is under it (the first admin's included), and the admin may still choose
/// another folder for anyone.
#[tokio::test]
async fn storage_url_is_the_base_of_every_users_files() {
    let base = TempDir::new();
    let app = spawn(Options {
        storage_url: Some(base.0.clone()),
        ..Options::default()
    })
    .await;
    let admin = admin_session(&app).await;
    let (_bea, bea_id) = invited_user(&app, &admin, "bea@example.com").await;
    let users = users_by_email(&app, &admin).await;
    let root_of = |email: &str| {
        let id = users[email]["id"].as_str().unwrap();
        base.0
            .join("users")
            .join(id)
            .join("files")
            .to_str()
            .unwrap()
            .to_string()
    };
    let first = &users["admin@example.com"]["files"];
    assert_eq!(first["files_root"], json!(root_of("admin@example.com")));
    assert_eq!(first["default"], json!(true));
    let bea = &users["bea@example.com"]["files"];
    assert_eq!(bea["files_root"], json!(root_of("bea@example.com")));
    assert_eq!(bea["default"], json!(true));
    // The admin may still choose another folder for anyone.
    let share = TempDir::new();
    let admin_id = users["admin@example.com"]["id"]
        .as_str()
        .unwrap()
        .to_string();
    let response = patch_user(&app, &admin, &admin_id, json!({ "files_root": null })).await;
    assert_eq!(response.status_code(), 200);
    let response = patch_user(
        &app,
        &admin,
        &bea_id,
        json!({ "files_root": share.0.to_str().unwrap() }),
    )
    .await;
    assert_eq!(response.status_code(), 200);
}

/// Invites and sign-ups count against the sign-in limits: per address
/// for the invite page, per email for the rest.
#[tokio::test]
async fn invites_and_signups_are_limited_as_sign_ins() {
    let app = spawn(Options {
        limits: organizer::limits::Limits::with_bursts(2, 1_000, 1_000),
        joining: organizer::users::Env {
            open_signup: Some(true),
            oidc_create_users: None,
        },
        ..Options::default()
    })
    .await;
    admin_session(&app).await;
    let signup = |password: &'static str| {
        request(
            &app,
            Method::POST,
            "/api/auth/signup",
            None,
            Some(json!({ "email": "eve@example.com", "password": password })),
        )
    };
    assert_eq!(signup("short").await.status_code(), 400);
    assert_eq!(signup("short").await.status_code(), 400);
    let response = signup("correct horse battery").await;
    assert_eq!(response.status_code(), 429);
    assert!(response.headers().contains_key("retry-after"));
    for _ in 0..2 {
        let response = request(
            &app,
            Method::POST,
            "/api/auth/invite/nope",
            None,
            Some(json!({
                "email": "mallory@example.com", "password": "correct horse battery",
            })),
        )
        .await;
        assert_eq!(response.status_code(), 404);
    }
    let response = request(
        &app,
        Method::POST,
        "/api/auth/invite/nope",
        None,
        Some(json!({
            "email": "mallory@example.com", "password": "correct horse battery",
        })),
    )
    .await;
    assert_eq!(response.status_code(), 429);

    // The invite page, per address.
    let app = spawn(Options {
        limits: organizer::limits::Limits::with_bursts(1_000, 3, 1_000),
        ..Options::default()
    })
    .await;
    for _ in 0..3 {
        let response = request(&app, Method::GET, "/api/auth/invite/nope", None, None).await;
        assert_eq!(response.status_code(), 404);
    }
    let response = request(&app, Method::GET, "/api/auth/invite/nope", None, None).await;
    assert_eq!(response.status_code(), 429);
}

// ---------------------------------------------------------------------------
// Sharing (docs/M6B_DESIGN.md)
// ---------------------------------------------------------------------------

/// The id of the node at a path of the user's files.
async fn id_at(app: &TestApp, session: &str, path: &str) -> String {
    let response = by_path(app, session, path).await;
    assert_eq!(response.status_code(), 200, "{path}: {}", response.text());
    json(response).await["id"].as_str().unwrap().to_string()
}

/// A share of a node with a user or group, with its permissions.
async fn share_with(
    app: &TestApp,
    session: &str,
    node: &str,
    to: Value,
    permissions: Value,
) -> Response {
    let mut body = to;
    body["permissions"] = permissions;
    post(
        app,
        &format!("/api/files/{node}/shares"),
        session,
        Some(body),
    )
    .await
}

/// GET a path with a session: its status.
async fn status_of(app: &TestApp, session: &str, path: &str) -> u16 {
    request(app, Method::GET, path, Some(session), None)
        .await
        .status_code()
        .as_u16()
}

/// Upload a small file into a folder: the response.
async fn upload(app: &TestApp, session: &str, folder: &str, name: &str, content: &str) -> Response {
    let (content_type, body) = multipart(name, content);
    app.server
        .post(&format!("/api/files/{folder}/files"))
        .add_header("cookie", session)
        .add_header("content-type", &content_type)
        .bytes(body.into())
        .await
}

/// A node's current version, as the user sees it.
async fn version_of(app: &TestApp, session: &str, id: &str) -> i64 {
    json(
        request(
            app,
            Method::GET,
            &format!("/api/files/{id}"),
            Some(session),
            None,
        )
        .await,
    )
    .await["version"]
        .as_i64()
        .unwrap()
}

async fn delete_node(app: &TestApp, session: &str, id: &str) -> Response {
    let version = version_of(app, session, id).await;
    request(
        app,
        Method::DELETE,
        &format!("/api/files/{id}?if_version={version}"),
        Some(session),
        None,
    )
    .await
}

async fn rename(app: &TestApp, session: &str, id: &str, name: &str) -> Response {
    let version = version_of(app, session, id).await;
    request(
        app,
        Method::PATCH,
        &format!("/api/files/{id}"),
        Some(session),
        Some(json!({ "name": name, "if_version": version })),
    )
    .await
}

async fn replace_as(app: &TestApp, session: &str, id: &str, content: &str) -> Response {
    let version = version_of(app, session, id).await;
    app.server
        .put(&format!("/api/files/{id}/content"))
        .add_header("cookie", session)
        .add_header("if-match", format!("\"{version}\""))
        .bytes(content.to_string().into())
        .await
}

/// M6b's shares, answered for one release over the roles (§2): read only
/// is a viewer, with create, change or delete an editor, with re-share a
/// manager; each role allows what it gives and nothing more, and a
/// re-share never gives more than its maker has, and goes when theirs
/// does.
#[tokio::test]
async fn shares_allow_what_they_give_and_nothing_more() {
    let app = spawn_app().await;
    let (admin, share) = with_files(
        &app,
        &[
            ("Docs/a.txt", "alpha"),
            ("Docs/Sub/b.txt", "beta"),
            ("Other/c.txt", "gamma"),
        ],
    )
    .await;
    let (bob, bob_id) = invited_user(&app, &admin, "bob@example.com").await;
    let (carol, carol_id) = invited_user(&app, &admin, "carol@example.com").await;
    let docs = id_at(&app, &admin, "/Docs").await;
    let sub = id_at(&app, &admin, "/Docs/Sub").await;
    let a = id_at(&app, &admin, "/Docs/a.txt").await;
    let other = id_at(&app, &admin, "/Other").await;

    // Not shared: as if it did not exist.
    assert_eq!(
        status_of(&app, &bob, &format!("/api/files/{docs}")).await,
        404
    );

    // Read only.
    let response = share_with(&app, &admin, &docs, json!({ "user_id": bob_id }), json!({})).await;
    assert_eq!(response.status_code(), 201, "{}", response.text());
    let bobs_share = json(response).await;
    let share_id = bobs_share["id"].as_str().unwrap().to_string();
    assert_eq!(bobs_share["user"]["email"], "bob@example.com");
    let entry = json(
        request(
            &app,
            Method::GET,
            &format!("/api/files/{docs}"),
            Some(&bob),
            None,
        )
        .await,
    )
    .await;
    assert_eq!(entry["permissions"]["own"], false);
    assert_eq!(entry["permissions"]["create"], false);
    assert_eq!(entry["path"], "", "not placed: no path in bob's files");
    assert_eq!(
        status_of(&app, &bob, &format!("/api/files/{docs}/children")).await,
        200
    );
    let response = request(
        &app,
        Method::GET,
        &format!("/api/files/{a}/content"),
        Some(&bob),
        None,
    )
    .await;
    assert_eq!(response.text(), "alpha");
    assert_eq!(response.header("content-security-policy"), "sandbox");
    assert_eq!(
        status_of(&app, &bob, &format!("/api/files/{other}")).await,
        404
    );
    assert_eq!(
        status_of(&app, &carol, &format!("/api/files/{docs}")).await,
        404
    );
    let folder = post(
        &app,
        &format!("/api/files/{docs}/folders"),
        &bob,
        Some(json!({ "name": "New" })),
    )
    .await;
    assert_eq!(folder.status_code(), 403);
    assert_eq!(
        upload(&app, &bob, &docs, "up.txt", "x").await.status_code(),
        403
    );
    assert_eq!(
        replace_as(&app, &bob, &a, "changed").await.status_code(),
        403
    );
    assert_eq!(
        rename(&app, &bob, &a, "renamed.txt").await.status_code(),
        403
    );
    assert_eq!(delete_node(&app, &bob, &a).await.status_code(), 403);
    assert_eq!(
        status_of(&app, &bob, &format!("/api/files/{docs}/shares")).await,
        403
    );
    let response = share_with(&app, &bob, &docs, json!({ "user_id": carol_id }), json!({})).await;
    assert_eq!(response.status_code(), 403, "no re-share");
    // Only the owner (or its maker) changes a share.
    let path = format!("/api/files/{docs}/shares/{share_id}");
    let response = request(
        &app,
        Method::PATCH,
        &path,
        Some(&bob),
        Some(json!({ "permissions": { "create": true } })),
    )
    .await;
    assert_eq!(response.status_code(), 403);

    // An editor (a share with create, change or delete): adds, changes,
    // renames, moves inside the share and deletes to the owner's trash.
    let set = |perms: Value| {
        let (app, admin, path) = (&app, &admin, &path);
        async move {
            let response = request(
                app,
                Method::PATCH,
                path,
                Some(admin),
                Some(json!({ "permissions": perms })),
            )
            .await;
            assert_eq!(response.status_code(), 200, "{}", response.text());
            json(response).await["role"].as_str().unwrap().to_string()
        }
    };
    assert_eq!(set(json!({ "create": true })).await, "editor");
    let folder = post(
        &app,
        &format!("/api/files/{docs}/folders"),
        &bob,
        Some(json!({ "name": "New" })),
    )
    .await;
    assert_eq!(folder.status_code(), 201);
    assert_eq!(json(folder).await["my_role"], "editor");
    assert!(share.0.join("Docs/New").is_dir(), "in the owner's tree");
    assert_eq!(
        upload(&app, &bob, &docs, "up.txt", "upped")
            .await
            .status_code(),
        201
    );
    assert_eq!(
        std::fs::read_to_string(share.0.join("Docs/up.txt")).unwrap(),
        "upped"
    );
    assert_eq!(
        replace_as(&app, &bob, &a, "changed").await.status_code(),
        200
    );
    assert_eq!(
        std::fs::read_to_string(share.0.join("Docs/a.txt")).unwrap(),
        "changed"
    );
    assert_eq!(
        rename(&app, &bob, &a, "renamed.txt").await.status_code(),
        200
    );
    assert!(share.0.join("Docs/renamed.txt").exists());
    // The shared folder itself is its owner's to rename.
    assert_eq!(rename(&app, &bob, &docs, "Mine").await.status_code(), 403);
    // Not into bob's own files, nor from them.
    let bob_root = json(request(&app, Method::GET, "/api/files/root", Some(&bob), None).await)
        .await["id"]
        .as_str()
        .unwrap()
        .to_string();
    let version = version_of(&app, &bob, &a).await;
    let response = request(
        &app,
        Method::PATCH,
        &format!("/api/files/{a}"),
        Some(&bob),
        Some(json!({ "parent_id": bob_root, "if_version": version })),
    )
    .await;
    assert_eq!(response.status_code(), 400);
    let response = request(
        &app,
        Method::PATCH,
        &format!("/api/files/{a}"),
        Some(&bob),
        Some(json!({ "parent_id": sub, "if_version": version })),
    )
    .await;
    assert_eq!(response.status_code(), 200, "{}", response.text());
    assert!(share.0.join("Docs/Sub/renamed.txt").exists());
    assert_eq!(delete_node(&app, &bob, &a).await.status_code(), 200);
    let trash = json(request(&app, Method::GET, "/api/trash", Some(&admin), None).await).await;
    assert!(
        trash
            .as_array()
            .unwrap()
            .iter()
            .any(|t| t["id"] == json!(a))
    );
    let trash = json(request(&app, Method::GET, "/api/trash", Some(&bob), None).await).await;
    assert!(trash.as_array().unwrap().is_empty());
    // Not the shared folder itself.
    assert_eq!(delete_node(&app, &bob, &docs).await.status_code(), 403);
    // An editor does not decide who has access.
    assert_eq!(
        status_of(&app, &bob, &format!("/api/files/{docs}/shares")).await,
        403
    );

    // A manager (re-share): who has access, never more than they have.
    assert_eq!(
        set(json!({ "create": true, "share": true })).await,
        "manager"
    );
    assert_eq!(
        status_of(&app, &bob, &format!("/api/files/{docs}/shares")).await,
        200
    );
    let response = share_with(
        &app,
        &bob,
        &docs,
        json!({ "user_id": carol_id }),
        json!({ "create": true }),
    )
    .await;
    assert_eq!(response.status_code(), 201);
    let carols = json(response).await;
    assert_eq!(carols["created_by"]["email"], "bob@example.com");
    assert_eq!(carols["role"], "editor");
    assert_eq!(carols["editable"], true);
    let folder = post(
        &app,
        &format!("/api/files/{docs}/folders"),
        &carol,
        Some(json!({ "name": "Carol" })),
    )
    .await;
    assert_eq!(folder.status_code(), 201);
    // Not twice, not with themselves.
    let response = share_with(&app, &bob, &docs, json!({ "user_id": carol_id }), json!({})).await;
    assert_eq!(response.status_code(), 409);
    let response = share_with(&app, &bob, &sub, json!({ "user_id": bob_id }), json!({})).await;
    assert_eq!(response.status_code(), 400);
    // Bob's own access shrinks to viewer: carol's (his grant) with it.
    assert_eq!(set(json!({})).await, "viewer");
    let folder = post(
        &app,
        &format!("/api/files/{docs}/folders"),
        &carol,
        Some(json!({ "name": "Carol 2" })),
    )
    .await;
    assert_eq!(folder.status_code(), 403);
    assert_eq!(
        status_of(&app, &carol, &format!("/api/files/{docs}")).await,
        200
    );
    // Bob's share goes: carol's gives nothing.
    let response = request(&app, Method::DELETE, &path, Some(&admin), None).await;
    assert_eq!(response.status_code(), 204);
    assert_eq!(
        status_of(&app, &bob, &format!("/api/files/{docs}")).await,
        404
    );
    assert_eq!(
        status_of(&app, &carol, &format!("/api/files/{docs}")).await,
        404
    );
    let shared = json(request(&app, Method::GET, "/api/shared", Some(&carol), None).await).await;
    assert!(shared.as_array().unwrap().is_empty());
}

/// A group's share reaches its members as they are now: one who joins sees
/// it, one who leaves loses it, from their files too.
#[tokio::test]
async fn group_shares_follow_the_members() {
    let app = spawn_app().await;
    let (admin, _share) = with_files(&app, &[("Team/plan.txt", "plan")]).await;
    let (bob, bob_id) = invited_user(&app, &admin, "bob@example.com").await;
    let (carol, carol_id) = invited_user(&app, &admin, "carol@example.com").await;
    let team = id_at(&app, &admin, "/Team").await;

    // Admins only.
    let response = post(
        &app,
        "/api/superadmin/groups",
        &bob,
        Some(json!({ "name": "Staff" })),
    )
    .await;
    assert_eq!(response.status_code(), 403);
    let response = post(
        &app,
        "/api/superadmin/groups",
        &admin,
        Some(json!({ "name": "Staff", "members": [bob_id] })),
    )
    .await;
    assert_eq!(response.status_code(), 201);
    let group = json(response).await;
    let group_id = group["id"].as_str().unwrap().to_string();
    assert_eq!(group["members"][0]["email"], "bob@example.com");
    let response = post(
        &app,
        "/api/superadmin/groups",
        &admin,
        Some(json!({ "name": "staff" })),
    )
    .await;
    assert_eq!(
        response.status_code(),
        409,
        "names are unique, ignoring case"
    );
    let recipients = json(
        request(
            &app,
            Method::GET,
            "/api/shared/recipients",
            Some(&bob),
            None,
        )
        .await,
    )
    .await;
    assert_eq!(recipients["groups"][0]["name"], "Staff");
    assert!(
        recipients["users"]
            .as_array()
            .unwrap()
            .iter()
            .all(|u| u["email"] != "bob@example.com")
    );

    let response = share_with(
        &app,
        &admin,
        &team,
        json!({ "group_id": group_id }),
        json!({ "create": true }),
    )
    .await;
    assert_eq!(response.status_code(), 201);
    let shared = json(request(&app, Method::GET, "/api/shared", Some(&bob), None).await).await;
    assert_eq!(shared[0]["node"]["name"], "Team");
    assert_eq!(shared[0]["group"], "Staff");
    assert_eq!(shared[0]["owner"]["email"], "admin@example.com");
    assert_eq!(shared[0]["permissions"]["create"], true);
    assert_eq!(shared[0]["new"], true);
    assert_eq!(
        status_of(&app, &carol, &format!("/api/files/{team}")).await,
        404
    );
    // Seen: no longer new.
    assert_eq!(
        post(&app, "/api/shared/seen", &bob, None)
            .await
            .status_code(),
        204
    );
    let shared = json(request(&app, Method::GET, "/api/shared", Some(&bob), None).await).await;
    assert_eq!(shared[0]["new"], false);

    // Bob places it; carol joins; bob leaves.
    let bob_root = json(request(&app, Method::GET, "/api/files/root", Some(&bob), None).await)
        .await["id"]
        .as_str()
        .unwrap()
        .to_string();
    let response = post(
        &app,
        &format!("/api/shared/{team}/place"),
        &bob,
        Some(json!({ "parent_id": bob_root })),
    )
    .await;
    assert_eq!(response.status_code(), 201);
    let cursor =
        json(request(&app, Method::GET, "/api/changes", Some(&bob), None).await).await["cursor"]
            .as_str()
            .unwrap()
            .to_string();
    let response = request(
        &app,
        Method::PATCH,
        &format!("/api/superadmin/groups/{group_id}"),
        Some(&admin),
        Some(json!({ "members": [carol_id] })),
    )
    .await;
    assert_eq!(response.status_code(), 200);
    assert_eq!(
        status_of(&app, &carol, &format!("/api/files/{team}")).await,
        200
    );
    assert_eq!(
        status_of(&app, &bob, &format!("/api/files/{team}")).await,
        404
    );
    assert_eq!(by_path(&app, &bob, "/Team").await.status_code(), 404);
    let changes = json(
        request(
            &app,
            Method::GET,
            &format!("/api/changes?cursor={cursor}"),
            Some(&bob),
            None,
        )
        .await,
    )
    .await;
    assert_eq!(changes["changes"][0]["node_id"], json!(team));
    assert_eq!(changes["changes"][0]["op"], "delete");

    // Renamed, then deleted: its shares go with it.
    let response = request(
        &app,
        Method::PATCH,
        &format!("/api/superadmin/groups/{group_id}"),
        Some(&admin),
        Some(json!({ "name": "Crew" })),
    )
    .await;
    assert_eq!(json(response).await["name"], "Crew");
    let response = request(
        &app,
        Method::DELETE,
        &format!("/api/superadmin/groups/{group_id}"),
        Some(&admin),
        None,
    )
    .await;
    assert_eq!(response.status_code(), 204);
    assert_eq!(
        status_of(&app, &carol, &format!("/api/files/{team}")).await,
        404
    );
}

/// A placed share is a folder of the recipient's files that is the
/// owner's: listed, found by path, written through (into the owner's tree,
/// on the owner's limit), renamed and taken out as the recipient's own
/// entry. No loops: not inside another share, not nested, and the owner's
/// own placed shares are not shared on.
#[tokio::test]
async fn placed_shares_are_the_owners_folders_in_the_recipients_files() {
    let app = spawn_app().await;
    let (admin, share) =
        with_files(&app, &[("Docs/a.txt", "alpha"), ("Docs/Sub/b.txt", "beta")]).await;
    let (bob, bob_id) = invited_user(&app, &admin, "bob@example.com").await;
    let (carol, carol_id) = invited_user(&app, &admin, "carol@example.com").await;
    let docs = id_at(&app, &admin, "/Docs").await;
    let sub = id_at(&app, &admin, "/Docs/Sub").await;
    let admin_id = session_of(&app, Some(&admin)).await["user"]["id"]
        .as_str()
        .unwrap()
        .to_string();
    let perms = json!({ "create": true, "change": true, "delete": true });
    share_with(
        &app,
        &admin,
        &docs,
        json!({ "user_id": bob_id }),
        perms.clone(),
    )
    .await;
    let bob_root = id_at(&app, &bob, "/").await;
    let mine = json(
        post(
            &app,
            &format!("/api/files/{bob_root}/folders"),
            &bob,
            Some(json!({ "name": "Work" })),
        )
        .await,
    )
    .await;
    let work = mine["id"].as_str().unwrap().to_string();

    // Not someone else's folder, not one's own share.
    let response = post(
        &app,
        &format!("/api/shared/{docs}/place"),
        &bob,
        Some(json!({ "parent_id": sub })),
    )
    .await;
    assert_eq!(response.status_code(), 404);
    let response = post(
        &app,
        &format!("/api/shared/{work}/place"),
        &bob,
        Some(json!({ "parent_id": bob_root })),
    )
    .await;
    assert_eq!(response.status_code(), 400);
    let response = post(
        &app,
        &format!("/api/shared/{docs}/place"),
        &bob,
        Some(json!({ "parent_id": work, "name": "Team" })),
    )
    .await;
    assert_eq!(response.status_code(), 201, "{}", response.text());
    let placed = json(response).await;
    assert_eq!(placed["path"], "/Work/Team");
    assert_eq!(placed["parent_id"], json!(work));
    assert_eq!(placed["permissions"]["mount"], true);
    let shared = json(request(&app, Method::GET, "/api/shared", Some(&bob), None).await).await;
    assert_eq!(shared[0]["placed"], "/Work/Team");

    // Listed in bob's folder, marked; found by path; its files by path.
    let children = json(
        request(
            &app,
            Method::GET,
            &format!("/api/files/{work}/children"),
            Some(&bob),
            None,
        )
        .await,
    )
    .await;
    assert_eq!(children["items"][0]["name"], "Team");
    assert_eq!(children["mounts"], json!([docs]));
    let inside = json(by_path(&app, &bob, "/Work/Team/Sub/b.txt").await).await;
    assert_eq!(inside["path"], "/Work/Team/Sub/b.txt");
    assert_eq!(id_at(&app, &bob, "/Work/Team").await, docs);
    // The owner's listing is not changed, and shows what they shared.
    let children = json(
        request(
            &app,
            Method::GET,
            &format!("/api/files/{}/children", id_at(&app, &admin, "/").await),
            Some(&admin),
            None,
        )
        .await,
    )
    .await;
    assert_eq!(children["shared"], json!([docs]));
    // The name is taken in bob's folder.
    let response = post(
        &app,
        &format!("/api/files/{work}/folders"),
        &bob,
        Some(json!({ "name": "team" })),
    )
    .await;
    assert_eq!(response.status_code(), 409);

    // Written through: into the owner's tree, on the owner's limit.
    assert_eq!(
        upload(&app, &bob, &docs, "new.txt", "fresh")
            .await
            .status_code(),
        201
    );
    assert_eq!(
        std::fs::read_to_string(share.0.join("Docs/new.txt")).unwrap(),
        "fresh"
    );
    assert!(
        !app.state
            .files
            .default_root(bob_id.parse().unwrap())
            .join("Work/Team")
            .exists()
    );
    let bob_usage =
        json(request(&app, Method::GET, "/api/auth/storage", Some(&bob), None).await).await;
    assert_eq!(bob_usage["used"], 0);
    let response = patch_user(&app, &admin, &admin_id, json!({ "storage_limit": 10 })).await;
    assert_eq!(response.status_code(), 200);
    assert_eq!(
        upload(&app, &bob, &docs, "big.txt", "more than ten bytes")
            .await
            .status_code(),
        507
    );
    patch_user(&app, &admin, &admin_id, json!({ "storage_limit": null })).await;

    // Renamed and moved: bob's entry, not the owner's folder.
    assert_eq!(
        rename(&app, &bob, &docs, "Shared plans")
            .await
            .status_code(),
        200
    );
    assert_eq!(
        by_path(&app, &bob, "/Work/Shared plans")
            .await
            .status_code(),
        200
    );
    assert_eq!(by_path(&app, &admin, "/Docs").await.status_code(), 200);
    let version = version_of(&app, &bob, &docs).await;
    let response = request(
        &app,
        Method::PATCH,
        &format!("/api/files/{docs}"),
        Some(&bob),
        Some(json!({ "parent_id": sub, "if_version": version })),
    )
    .await;
    assert_eq!(response.status_code(), 400, "not into a share");
    let response = request(
        &app,
        Method::PATCH,
        &format!("/api/files/{docs}"),
        Some(&bob),
        Some(json!({ "parent_id": bob_root, "if_version": version })),
    )
    .await;
    assert_eq!(response.status_code(), 200);
    assert_eq!(
        by_path(&app, &bob, "/Shared plans/a.txt")
            .await
            .status_code(),
        200
    );
    // Bob's own files do not move into it.
    let version = version_of(&app, &bob, &work).await;
    let response = request(
        &app,
        Method::PATCH,
        &format!("/api/files/{work}"),
        Some(&bob),
        Some(json!({ "parent_id": docs, "if_version": version })),
    )
    .await;
    assert_eq!(response.status_code(), 400);

    // Not nested: a folder in it, shared on its own, is placed already.
    share_with(&app, &admin, &sub, json!({ "user_id": bob_id }), json!({})).await;
    let response = post(
        &app,
        &format!("/api/shared/{sub}/place"),
        &bob,
        Some(json!({ "parent_id": bob_root })),
    )
    .await;
    assert_eq!(response.status_code(), 409);
    // Twice: no.
    let response = post(
        &app,
        &format!("/api/shared/{docs}/place"),
        &bob,
        Some(json!({ "parent_id": work })),
    )
    .await;
    assert_eq!(response.status_code(), 409);

    // Bob shares his folder holding the placed share with carol: she sees
    // his own files only, not the share in it.
    let version = version_of(&app, &bob, &docs).await;
    request(
        &app,
        Method::PATCH,
        &format!("/api/files/{docs}"),
        Some(&bob),
        Some(json!({ "parent_id": work, "if_version": version })),
    )
    .await;
    share_with(&app, &bob, &work, json!({ "user_id": carol_id }), json!({})).await;
    let children = json(
        request(
            &app,
            Method::GET,
            &format!("/api/files/{work}/children"),
            Some(&carol),
            None,
        )
        .await,
    )
    .await;
    assert!(
        children["items"].as_array().unwrap().is_empty(),
        "{children}"
    );
    // And she cannot place hers inside the share in bob's files.
    let carol_root = id_at(&app, &carol, "/").await;
    let response = post(
        &app,
        &format!("/api/shared/{work}/place"),
        &carol,
        Some(json!({ "parent_id": carol_root })),
    )
    .await;
    assert_eq!(response.status_code(), 201);
    let response = post(
        &app,
        &format!("/api/files/{work}/folders"),
        &carol,
        Some(json!({ "name": "x" })),
    )
    .await;
    assert_eq!(response.status_code(), 403);

    // Deleting the entry takes it out of bob's files; the owner's stays.
    let response = delete_node(&app, &bob, &docs).await;
    assert_eq!(response.status_code(), 200);
    assert_eq!(
        by_path(&app, &bob, "/Work/Shared plans")
            .await
            .status_code(),
        404
    );
    assert!(share.0.join("Docs/a.txt").exists());
    let trash = json(request(&app, Method::GET, "/api/trash", Some(&admin), None).await).await;
    assert!(trash.as_array().unwrap().is_empty());
    let shared = json(request(&app, Method::GET, "/api/shared", Some(&bob), None).await).await;
    let docs_item = shared
        .as_array()
        .unwrap()
        .iter()
        .find(|s| s["node"]["id"] == json!(docs))
        .unwrap();
    assert_eq!(docs_item["placed"], Value::Null);
    // Placed again, then unplaced through /shared, then declined.
    let response = post(
        &app,
        &format!("/api/shared/{docs}/place"),
        &bob,
        Some(json!({ "parent_id": bob_root })),
    )
    .await;
    assert_eq!(response.status_code(), 201);
    let response = request(
        &app,
        Method::DELETE,
        &format!("/api/shared/{docs}/place"),
        Some(&bob),
        None,
    )
    .await;
    assert_eq!(response.status_code(), 204);
    assert_eq!(by_path(&app, &bob, "/Docs").await.status_code(), 404);
    post(
        &app,
        &format!("/api/shared/{docs}/place"),
        &bob,
        Some(json!({ "parent_id": bob_root })),
    )
    .await;
    let response = request(
        &app,
        Method::DELETE,
        &format!("/api/shared/{docs}"),
        Some(&bob),
        None,
    )
    .await;
    assert_eq!(response.status_code(), 204);
    assert_eq!(by_path(&app, &bob, "/Docs").await.status_code(), 404);
    assert_eq!(
        status_of(&app, &bob, &format!("/api/files/{docs}")).await,
        404
    );
    let shared = json(request(&app, Method::GET, "/api/shared", Some(&bob), None).await).await;
    assert!(
        shared
            .as_array()
            .unwrap()
            .iter()
            .all(|s| s["node"]["id"] != json!(docs))
    );
}

/// The owner's changes reach the change feed of a recipient who placed the
/// share: what is made, changed, moved in and out of it, deleted; the
/// placed folder keeps the recipient's name.
#[tokio::test]
async fn the_change_feed_follows_placed_shares() {
    let app = spawn_app().await;
    let (admin, _share) = with_files(
        &app,
        &[
            ("Docs/a.txt", "alpha"),
            ("Docs/Sub/b.txt", "beta"),
            ("Other/c.txt", "gamma"),
        ],
    )
    .await;
    let (bob, bob_id) = invited_user(&app, &admin, "bob@example.com").await;
    let docs = id_at(&app, &admin, "/Docs").await;
    let a = id_at(&app, &admin, "/Docs/a.txt").await;
    let sub = id_at(&app, &admin, "/Docs/Sub").await;
    let b = id_at(&app, &admin, "/Docs/Sub/b.txt").await;
    let other = id_at(&app, &admin, "/Other").await;
    let c = id_at(&app, &admin, "/Other/c.txt").await;
    share_with(
        &app,
        &admin,
        &docs,
        json!({ "user_id": bob_id }),
        json!({ "create": true }),
    )
    .await;
    let bob_root = id_at(&app, &bob, "/").await;
    let mut cursor = json(request(&app, Method::GET, "/api/changes", Some(&bob), None).await).await
        ["cursor"]
        .as_str()
        .unwrap()
        .to_string();
    let next = async |cursor: &mut String| -> Vec<Value> {
        let page = json(
            request(
                &app,
                Method::GET,
                &format!("/api/changes?cursor={cursor}"),
                Some(&bob),
                None,
            )
            .await,
        )
        .await;
        *cursor = page["cursor"].as_str().unwrap().to_string();
        page["changes"].as_array().unwrap().clone()
    };

    // Placed: the folder and everything in it, parents first.
    post(
        &app,
        &format!("/api/shared/{docs}/place"),
        &bob,
        Some(json!({ "parent_id": bob_root, "name": "Team" })),
    )
    .await;
    let changes = next(&mut cursor).await;
    let ids: Vec<&Value> = changes.iter().map(|c| &c["node_id"]).collect();
    assert_eq!(changes[0]["node_id"], json!(docs));
    assert_eq!(changes[0]["op"], "create");
    assert_eq!(changes[0]["parent_id"], json!(bob_root));
    assert_eq!(changes[0]["name"], "Team");
    for id in [&a, &sub, &b] {
        assert!(ids.contains(&&json!(id)), "{id} in {changes:?}");
    }
    let sub_at = ids.iter().position(|i| **i == json!(sub)).unwrap();
    let b_at = ids.iter().position(|i| **i == json!(b)).unwrap();
    assert!(sub_at < b_at);

    // The owner makes a file in it, and one elsewhere.
    upload(&app, &admin, &docs, "new.txt", "n").await;
    upload(&app, &admin, &other, "elsewhere.txt", "e").await;
    let changes = next(&mut cursor).await;
    assert_eq!(changes.len(), 1, "{changes:?}");
    assert_eq!(changes[0]["op"], "create");
    assert_eq!(changes[0]["name"], "new.txt");
    assert_eq!(changes[0]["parent_id"], json!(docs));

    // A change, a move in, a move out, a delete.
    replace_as(&app, &admin, &a, "changed").await;
    let version = version_of(&app, &admin, &c).await;
    request(
        &app,
        Method::PATCH,
        &format!("/api/files/{c}"),
        Some(&admin),
        Some(json!({ "parent_id": docs, "if_version": version })),
    )
    .await;
    let version = version_of(&app, &admin, &b).await;
    request(
        &app,
        Method::PATCH,
        &format!("/api/files/{b}"),
        Some(&admin),
        Some(json!({ "parent_id": other, "if_version": version })),
    )
    .await;
    delete_node(&app, &admin, &sub).await;
    let changes = next(&mut cursor).await;
    let op = |id: &str| {
        changes
            .iter()
            .find(|c| c["node_id"] == json!(id))
            .map(|c| c["op"].as_str().unwrap().to_string())
    };
    assert_eq!(op(&a).as_deref(), Some("update"));
    assert_eq!(op(&c).as_deref(), Some("create"), "moved into the share");
    assert_eq!(op(&b).as_deref(), Some("delete"), "moved out of the share");
    assert_eq!(op(&sub).as_deref(), Some("delete"));

    // The owner renames the shared folder: bob's keeps his name.
    rename(&app, &admin, &docs, "Documents").await;
    let changes = next(&mut cursor).await;
    assert_eq!(changes[0]["op"], "update");
    assert_eq!(changes[0]["name"], "Team");
    assert_eq!(changes[0]["parent_id"], json!(bob_root));

    // Bob's own writes through it are in his feed too, and in the owner's.
    upload(&app, &bob, &docs, "bobs.txt", "b").await;
    let changes = next(&mut cursor).await;
    assert_eq!(changes[0]["name"], "bobs.txt");

    // The owner deletes it: gone from bob's files.
    delete_node(&app, &admin, &docs).await;
    let changes = next(&mut cursor).await;
    assert_eq!(changes.last().unwrap()["node_id"], json!(docs));
    assert_eq!(changes.last().unwrap()["op"], "delete");
    assert_eq!(by_path(&app, &bob, "/Team").await.status_code(), 404);
}

/// WebDAV walks into a placed share: listed, read, written into the
/// owner's tree as the role allows; upload only hides what others added.
#[tokio::test]
async fn webdav_walks_into_placed_shares() {
    let app = spawn_app().await;
    let (admin, share) = with_files(&app, &[("Docs/a.txt", "alpha")]).await;
    let (bob, bob_id) = invited_user(&app, &admin, "bob@example.com").await;
    let docs = id_at(&app, &admin, "/Docs").await;
    // Upload only: bob adds, and sees only what he added.
    let response = post(
        &app,
        &format!("/api/files/{docs}/access"),
        &admin,
        Some(json!({ "user_id": bob_id, "role": "upload_only" })),
    )
    .await;
    assert_eq!(response.status_code(), 201, "{}", response.text());
    let bob_root = id_at(&app, &bob, "/").await;
    post(
        &app,
        &format!("/api/shared/{docs}/place"),
        &bob,
        Some(json!({ "parent_id": bob_root, "name": "Team" })),
    )
    .await;
    let body = json!({ "name": "dav", "scope": "write", "kind": "app_password" });
    let password = json(post(&app, "/api/auth/api-keys", &bob, Some(body)).await).await["token"]
        .as_str()
        .unwrap()
        .to_string();
    let auth = basic(&password);
    let dav = |method: &str, path: &str| {
        app.server
            .method(Method::from_bytes(method.as_bytes()).unwrap(), path)
            .add_header("authorization", &auth)
    };

    let response = dav("PROPFIND", "/dav/").add_header("depth", "1").await;
    assert_eq!(response.status_code(), 207);
    assert!(response.text().contains("Team"), "{}", response.text());
    let response = dav("PROPFIND", "/dav/Team/").add_header("depth", "1").await;
    assert_eq!(response.status_code(), 207);
    assert!(!response.text().contains("a.txt"), "{}", response.text());
    assert_eq!(dav("GET", "/dav/Team/a.txt").await.status_code(), 404);

    // Create: yes, into the owner's tree; change and delete: no.
    let response = dav("PUT", "/dav/Team/new.txt").bytes("fresh".into()).await;
    assert_eq!(response.status_code(), 201);
    assert_eq!(
        std::fs::read_to_string(share.0.join("Docs/new.txt")).unwrap(),
        "fresh"
    );
    assert_eq!(
        by_path(&app, &bob, "/Team/new.txt").await.status_code(),
        200
    );
    assert_eq!(dav("MKCOL", "/dav/Team/Folder").await.status_code(), 201);
    assert!(share.0.join("Docs/Folder").is_dir());
    let response = dav("PUT", "/dav/Team/a.txt").bytes("over".into()).await;
    assert!(
        response.status_code().is_client_error(),
        "{}",
        response.status_code()
    );
    assert_eq!(
        std::fs::read_to_string(share.0.join("Docs/a.txt")).unwrap(),
        "alpha"
    );
    assert!(
        dav("DELETE", "/dav/Team/a.txt")
            .await
            .status_code()
            .is_client_error()
    );
    assert!(share.0.join("Docs/a.txt").exists());
    // What he added he sees.
    let response = dav("PROPFIND", "/dav/Team/").add_header("depth", "1").await;
    assert!(response.text().contains("new.txt"), "{}", response.text());
    // Copied out into bob's own files.
    let response = dav("COPY", "/dav/Team/new.txt")
        .add_header("destination", format!("{}/dav/copy.txt", app.base))
        .await;
    assert_eq!(response.status_code(), 201);
    assert_eq!(by_path(&app, &bob, "/copy.txt").await.status_code(), 200);
    // Deleting the share's folder takes it out of bob's files only.
    assert_eq!(dav("DELETE", "/dav/Team").await.status_code(), 204);
    assert!(share.0.join("Docs/a.txt").exists());
    assert_eq!(by_path(&app, &bob, "/Team").await.status_code(), 404);
}

/// The drop box places into a share the user placed and may add to, into
/// the owner's tree; not into one only in "Shared with me", nor one
/// without create.
#[tokio::test]
async fn the_drop_box_places_into_placed_shares() {
    let app = spawn_app().await;
    let (admin, share) = with_files(&app, &[("Docs/a.txt", "alpha"), ("Read/r.txt", "r")]).await;
    let (bob, bob_id) = invited_user(&app, &admin, "bob@example.com").await;
    let docs = id_at(&app, &admin, "/Docs").await;
    let read = id_at(&app, &admin, "/Read").await;
    share_with(
        &app,
        &admin,
        &docs,
        json!({ "user_id": bob_id }),
        json!({ "create": true }),
    )
    .await;
    share_with(&app, &admin, &read, json!({ "user_id": bob_id }), json!({})).await;
    let bob_root = id_at(&app, &bob, "/").await;
    post(
        &app,
        &format!("/api/shared/{read}/place"),
        &bob,
        Some(json!({ "parent_id": bob_root })),
    )
    .await;

    let drop = |name: &'static str| {
        let (app, bob) = (&app, &bob);
        async move {
            let response = drop_files(app, bob, &[], &[(name, "dropped")]).await;
            assert_eq!(response.status_code(), 201, "{}", response.text());
            let item = json(response).await;
            let id = item[0]["id"].as_str().unwrap().to_string();
            settled(app, bob, &id).await;
            id
        }
    };
    let place = |item: String, folder: String| {
        let (app, bob) = (&app, &bob);
        async move {
            post(
                app,
                &format!("/api/drop-box/items/{item}/place"),
                bob,
                Some(json!({ "folder_id": folder })),
            )
            .await
        }
    };
    // Not placed in bob's files: no.
    let item = drop("one.txt").await;
    let response = place(item.clone(), docs.clone()).await;
    assert_eq!(response.status_code(), 403, "{}", response.text());
    // Placed but read only: no.
    let response = place(item.clone(), read.clone()).await;
    assert_eq!(response.status_code(), 403);
    // Placed, with create: into the owner's tree.
    post(
        &app,
        &format!("/api/shared/{docs}/place"),
        &bob,
        Some(json!({ "parent_id": bob_root })),
    )
    .await;
    let response = place(item.clone(), docs.clone()).await;
    assert_eq!(response.status_code(), 200, "{}", response.text());
    let placed = json(response).await;
    assert_eq!(placed["placement"]["path"], "Docs/one.txt");
    assert_eq!(
        std::fs::read_to_string(share.0.join("Docs/one.txt")).unwrap(),
        "dropped"
    );
    // Undone: back from the owner's tree into bob's drop box.
    let placement = placed["placement"]["id"].as_str().unwrap();
    let response = post(
        &app,
        &format!("/api/placements/{placement}/undo"),
        &bob,
        None,
    )
    .await;
    assert_eq!(response.status_code(), 200, "{}", response.text());
    assert!(!share.0.join("Docs/one.txt").exists());
}

/// A request to a link page's API, without signing in (with the cookie of
/// an unlock, when given).
async fn public(
    app: &TestApp,
    method: Method,
    path: &str,
    cookie: Option<&str>,
    body: Option<Value>,
) -> Response {
    let mut builder = app
        .server
        .method(method, path)
        .add_header("user-agent", UA)
        .add_header("x-forwarded-for", "198.51.100.7");
    if let Some(cookie) = cookie {
        builder = builder.add_header("cookie", cookie);
    }
    if let Some(body) = body {
        builder = builder.json(&body);
    }
    builder.await
}

async fn make_link(app: &TestApp, session: &str, node: &str, body: Value) -> Response {
    post(
        app,
        &format!("/api/files/{node}/links"),
        session,
        Some(body),
    )
    .await
}

/// Upload a file through a link: the response.
async fn link_upload(
    app: &TestApp,
    token: &str,
    cookie: Option<&str>,
    name: &str,
    content: &str,
) -> Response {
    let (content_type, body) = multipart(name, content);
    let mut builder = app
        .server
        .post(&format!("/api/s/{token}/files"))
        .add_header("content-type", &content_type);
    if let Some(cookie) = cookie {
        builder = builder.add_header("cookie", cookie);
    }
    builder.bytes(body.into()).await
}

/// A view link with a password: asked once per browser session, wrong ones
/// refused; the folder listed, its files served safely, the folder as a
/// zip; nothing changed through it.
#[tokio::test]
async fn view_links_show_and_download_behind_their_password() {
    let app = spawn_app().await;
    let (admin, _share) = with_files(
        &app,
        &[
            ("Photos/a.txt", "alpha"),
            ("Photos/Trip/b.txt", "beta"),
            ("Private/p.txt", "p"),
        ],
    )
    .await;
    let photos = id_at(&app, &admin, "/Photos").await;
    let a = id_at(&app, &admin, "/Photos/a.txt").await;
    let trip = id_at(&app, &admin, "/Photos/Trip").await;
    let private = id_at(&app, &admin, "/Private/p.txt").await;

    let response = make_link(
        &app,
        &admin,
        &photos,
        json!({ "kind": "view", "password": "abc" }),
    )
    .await;
    assert_eq!(response.status_code(), 400, "too short a password");
    let response = make_link(
        &app,
        &admin,
        &photos,
        json!({ "kind": "view", "password": "open sesame" }),
    )
    .await;
    assert_eq!(response.status_code(), 201, "{}", response.text());
    let link = json(response).await;
    let token = link["token"].as_str().unwrap().to_string();
    assert_eq!(link["has_password"], true);
    let listed = json(
        request(
            &app,
            Method::GET,
            &format!("/api/files/{photos}/links"),
            Some(&admin),
            None,
        )
        .await,
    )
    .await;
    assert_eq!(listed[0]["token"], json!(token));
    let children = json(
        request(
            &app,
            Method::GET,
            &format!("/api/files/{}/children", id_at(&app, &admin, "/").await),
            Some(&admin),
            None,
        )
        .await,
    )
    .await;
    assert!(
        children["shared"]
            .as_array()
            .unwrap()
            .contains(&json!(photos))
    );

    // Locked: nothing said, nothing served.
    let info = json(public(&app, Method::GET, &format!("/api/s/{token}"), None, None).await).await;
    assert_eq!(info["locked"], true);
    assert_eq!(info["name"], Value::Null);
    let response = public(
        &app,
        Method::GET,
        &format!("/api/s/{token}/children"),
        None,
        None,
    )
    .await;
    assert_eq!(response.status_code(), 401);
    let response = public(
        &app,
        Method::GET,
        &format!("/api/s/{token}/files/{a}/content"),
        None,
        None,
    )
    .await;
    assert_eq!(response.status_code(), 401);
    let response = public(
        &app,
        Method::POST,
        &format!("/api/s/{token}/unlock"),
        None,
        Some(json!({ "password": "wrong" })),
    )
    .await;
    assert_eq!(response.status_code(), 403);
    assert_eq!(
        public(&app, Method::GET, "/api/s/nope", None, None)
            .await
            .status_code(),
        404
    );

    let response = public(
        &app,
        Method::POST,
        &format!("/api/s/{token}/unlock"),
        None,
        Some(json!({ "password": "open sesame" })),
    )
    .await;
    assert_eq!(response.status_code(), 204);
    let unlocked = cookie(&response);
    assert!(unlocked.starts_with("organizer_links="));
    let info = json(
        public(
            &app,
            Method::GET,
            &format!("/api/s/{token}"),
            Some(&unlocked),
            None,
        )
        .await,
    )
    .await;
    assert_eq!(info["locked"], false);
    assert_eq!(info["node"]["name"], "Photos");
    let listing = json(
        public(
            &app,
            Method::GET,
            &format!("/api/s/{token}/children"),
            Some(&unlocked),
            None,
        )
        .await,
    )
    .await;
    let names: Vec<&str> = listing["items"]
        .as_array()
        .unwrap()
        .iter()
        .map(|n| n["name"].as_str().unwrap())
        .collect();
    assert_eq!(names, ["Trip", "a.txt"]);
    let inner = json(
        public(
            &app,
            Method::GET,
            &format!("/api/s/{token}/children?folder={trip}"),
            Some(&unlocked),
            None,
        )
        .await,
    )
    .await;
    assert_eq!(inner["trail"].as_array().unwrap().len(), 2);
    let response = public(
        &app,
        Method::GET,
        &format!("/api/s/{token}/files/{a}/content?inline=true"),
        Some(&unlocked),
        None,
    )
    .await;
    assert_eq!(response.text(), "alpha");
    assert_eq!(response.header("content-security-policy"), "sandbox");
    assert_eq!(response.header("x-content-type-options"), "nosniff");
    // Not outside the folder.
    let response = public(
        &app,
        Method::GET,
        &format!("/api/s/{token}/files/{private}/content"),
        Some(&unlocked),
        None,
    )
    .await;
    assert_eq!(response.status_code(), 404);
    // The folder as a zip.
    let response = public(
        &app,
        Method::GET,
        &format!("/api/s/{token}/folders/{photos}/zip"),
        Some(&unlocked),
        None,
    )
    .await;
    assert_eq!(response.status_code(), 200);
    assert_eq!(response.header("content-type"), "application/zip");
    let bytes = response.as_bytes().to_vec();
    assert_eq!(&bytes[..2], b"PK");
    let text = String::from_utf8_lossy(&bytes);
    assert!(text.contains("Trip/b.txt") && text.contains("a.txt"));
    // Nothing changed through a view link.
    assert_eq!(
        link_upload(&app, &token, Some(&unlocked), "x.txt", "x")
            .await
            .status_code(),
        403
    );
    let version = version_of(&app, &admin, &a).await;
    let response = public(
        &app,
        Method::DELETE,
        &format!("/api/s/{token}/nodes/{a}?if_version={version}"),
        Some(&unlocked),
        None,
    )
    .await;
    assert_eq!(response.status_code(), 403);

    // A new password asks again.
    let id = link["id"].as_str().unwrap();
    let response = request(
        &app,
        Method::PATCH,
        &format!("/api/links/{id}"),
        Some(&admin),
        Some(json!({ "password": "another one" })),
    )
    .await;
    assert_eq!(response.status_code(), 200);
    let response = public(
        &app,
        Method::GET,
        &format!("/api/s/{token}/children"),
        Some(&unlocked),
        None,
    )
    .await;
    assert_eq!(response.status_code(), 401);
    // No password: open to anyone.
    request(
        &app,
        Method::PATCH,
        &format!("/api/links/{id}"),
        Some(&admin),
        Some(json!({ "password": null })),
    )
    .await;
    assert_eq!(
        public(
            &app,
            Method::GET,
            &format!("/api/s/{token}/children"),
            None,
            None
        )
        .await
        .status_code(),
        200
    );
    // Removed: gone.
    let response = request(
        &app,
        Method::DELETE,
        &format!("/api/links/{id}"),
        Some(&admin),
        None,
    )
    .await;
    assert_eq!(response.status_code(), 204);
    assert_eq!(
        public(&app, Method::GET, &format!("/api/s/{token}"), None, None)
            .await
            .status_code(),
        404
    );

    // A file's view link: its content only.
    let link = json(make_link(&app, &admin, &a, json!({ "kind": "view" })).await).await;
    let token = link["token"].as_str().unwrap();
    let info = json(public(&app, Method::GET, &format!("/api/s/{token}"), None, None).await).await;
    assert_eq!(info["node"]["kind"], "file");
    let response = public(
        &app,
        Method::GET,
        &format!("/api/s/{token}/files/{a}/content"),
        None,
        None,
    )
    .await;
    assert_eq!(response.text(), "alpha");
    let response = make_link(&app, &admin, &a, json!({ "kind": "edit" })).await;
    assert_eq!(response.status_code(), 400, "edit links are for folders");
}

/// An edit link uploads, makes folders, renames and deletes (to the
/// owner's trash) inside its folder, not the folder itself; a file drop
/// only uploads, under a free name, and shows nothing.
#[tokio::test]
async fn edit_and_file_drop_links_write_into_the_owners_folder() {
    let app = spawn_app().await;
    let (admin, share) = with_files(&app, &[("Shared/a.txt", "alpha"), ("Inbox/x.txt", "x")]).await;
    let shared = id_at(&app, &admin, "/Shared").await;
    let a = id_at(&app, &admin, "/Shared/a.txt").await;
    let inbox = id_at(&app, &admin, "/Inbox").await;

    let token =
        json(make_link(&app, &admin, &shared, json!({ "kind": "edit" })).await).await["token"]
            .as_str()
            .unwrap()
            .to_string();
    let response = link_upload(&app, &token, None, "up.txt", "upped").await;
    assert_eq!(response.status_code(), 201);
    assert_eq!(
        std::fs::read_to_string(share.0.join("Shared/up.txt")).unwrap(),
        "upped"
    );
    assert_eq!(
        link_upload(&app, &token, None, "up.txt", "again")
            .await
            .status_code(),
        409
    );
    let response = public(
        &app,
        Method::POST,
        &format!("/api/s/{token}/folders"),
        None,
        Some(json!({ "parent_id": shared, "name": "New" })),
    )
    .await;
    assert_eq!(response.status_code(), 201);
    assert!(share.0.join("Shared/New").is_dir());
    let response = public(
        &app,
        Method::POST,
        &format!("/api/s/{token}/folders"),
        None,
        Some(json!({ "parent_id": inbox, "name": "Out" })),
    )
    .await;
    assert_eq!(response.status_code(), 404, "not outside its folder");
    let version = version_of(&app, &admin, &a).await;
    let response = public(
        &app,
        Method::PATCH,
        &format!("/api/s/{token}/nodes/{a}"),
        None,
        Some(json!({ "name": "b.txt", "if_version": version })),
    )
    .await;
    assert_eq!(response.status_code(), 200);
    assert!(share.0.join("Shared/b.txt").exists());
    let version = version_of(&app, &admin, &a).await;
    let response = public(
        &app,
        Method::DELETE,
        &format!("/api/s/{token}/nodes/{a}?if_version={version}"),
        None,
        None,
    )
    .await;
    assert_eq!(response.status_code(), 204);
    let trash = json(request(&app, Method::GET, "/api/trash", Some(&admin), None).await).await;
    assert!(
        trash
            .as_array()
            .unwrap()
            .iter()
            .any(|t| t["id"] == json!(a))
    );
    let version = version_of(&app, &admin, &shared).await;
    let response = public(
        &app,
        Method::DELETE,
        &format!("/api/s/{token}/nodes/{shared}?if_version={version}"),
        None,
        None,
    )
    .await;
    assert_eq!(response.status_code(), 403, "not the link's folder itself");

    // File drop: upload only, nothing shown, a taken name gets a number.
    let token =
        json(make_link(&app, &admin, &inbox, json!({ "kind": "drop" })).await).await["token"]
            .as_str()
            .unwrap()
            .to_string();
    let info = json(public(&app, Method::GET, &format!("/api/s/{token}"), None, None).await).await;
    assert_eq!(info["kind"], "drop");
    assert_eq!(info["node"], Value::Null);
    assert_eq!(
        public(
            &app,
            Method::GET,
            &format!("/api/s/{token}/children"),
            None,
            None
        )
        .await
        .status_code(),
        403
    );
    let x = id_at(&app, &admin, "/Inbox/x.txt").await;
    let response = public(
        &app,
        Method::GET,
        &format!("/api/s/{token}/files/{x}/content"),
        None,
        None,
    )
    .await;
    assert_eq!(response.status_code(), 403);
    let response = link_upload(&app, &token, None, "x.txt", "dropped").await;
    assert_eq!(response.status_code(), 201);
    assert_eq!(json(response).await["name"], "x (2).txt");
    assert_eq!(
        std::fs::read_to_string(share.0.join("Inbox/x (2).txt")).unwrap(),
        "dropped"
    );
    assert_eq!(
        std::fs::read_to_string(share.0.join("Inbox/x.txt")).unwrap(),
        "x"
    );
    // On the owner's limit.
    let admin_id = session_of(&app, Some(&admin)).await["user"]["id"]
        .as_str()
        .unwrap()
        .to_string();
    patch_user(&app, &admin, &admin_id, json!({ "storage_limit": 5 })).await;
    assert_eq!(
        link_upload(&app, &token, None, "big.txt", "more than five")
            .await
            .status_code(),
        507
    );
}

/// Links stop working when they expire and while their owner is disabled;
/// a recipient with re-share makes links giving no more than they have.
#[tokio::test]
async fn links_stop_when_expired_or_the_owner_is_disabled() {
    let app = spawn_app().await;
    let (admin, _share) = with_files(&app, &[("Docs/a.txt", "alpha")]).await;
    let (bob, bob_id) = invited_user(&app, &admin, "bob@example.com").await;
    let docs = id_at(&app, &admin, "/Docs").await;
    let shared =
        json(share_with(&app, &admin, &docs, json!({ "user_id": bob_id }), json!({})).await).await;

    // Bob, a viewer: no link at all (links are an editor's and above's).
    for kind in ["view", "edit", "drop"] {
        assert_eq!(
            make_link(&app, &bob, &docs, json!({ "kind": kind }))
                .await
                .status_code(),
            403,
            "{kind}"
        );
    }
    // An editor makes one.
    let response = request(
        &app,
        Method::PATCH,
        &format!(
            "/api/files/{docs}/shares/{}",
            shared["id"].as_str().unwrap()
        ),
        Some(&admin),
        Some(json!({ "permissions": { "change": true } })),
    )
    .await;
    assert_eq!(response.status_code(), 200);
    assert_eq!(json(response).await["role"], "editor");
    let response = make_link(
        &app,
        &bob,
        &docs,
        json!({ "kind": "view", "expires_at": "2999-12-01T00:00:00Z" }),
    )
    .await;
    assert_eq!(response.status_code(), 201);
    let link = json(response).await;
    let token = link["token"].as_str().unwrap().to_string();
    assert_eq!(
        public(
            &app,
            Method::GET,
            &format!("/api/s/{token}/children"),
            None,
            None
        )
        .await
        .status_code(),
        200
    );

    // Expired.
    let id = link["id"].as_str().unwrap();
    let response = request(
        &app,
        Method::PATCH,
        &format!("/api/links/{id}"),
        Some(&bob),
        Some(json!({ "expires_at": "2001-01-01T00:00:00Z" })),
    )
    .await;
    assert_eq!(response.status_code(), 200);
    assert_eq!(
        public(&app, Method::GET, &format!("/api/s/{token}"), None, None)
            .await
            .status_code(),
        404
    );
    request(
        &app,
        Method::PATCH,
        &format!("/api/links/{id}"),
        Some(&bob),
        Some(json!({ "expires_at": null })),
    )
    .await;
    assert_eq!(
        public(&app, Method::GET, &format!("/api/s/{token}"), None, None)
            .await
            .status_code(),
        200
    );

    // The owner disabled, then enabled.
    let (other_admin, _) = invited_user(&app, &admin, "root@example.com").await;
    let other_id = session_of(&app, Some(&other_admin)).await["user"]["id"]
        .as_str()
        .unwrap()
        .to_string();
    patch_user(&app, &admin, &other_id, json!({ "is_superadmin": true })).await;
    stepped_up(&app, &other_admin, 0).await;
    let admin_id = session_of(&app, Some(&admin)).await["user"]["id"]
        .as_str()
        .unwrap()
        .to_string();
    let response = patch_user(&app, &other_admin, &admin_id, json!({ "disabled": true })).await;
    assert_eq!(response.status_code(), 200);
    assert_eq!(
        public(&app, Method::GET, &format!("/api/s/{token}"), None, None)
            .await
            .status_code(),
        404
    );
    patch_user(&app, &other_admin, &admin_id, json!({ "disabled": false })).await;
    assert_eq!(
        public(&app, Method::GET, &format!("/api/s/{token}"), None, None)
            .await
            .status_code(),
        200
    );

    // The folder gone: so is the link.
    delete_node(&app, &admin, &docs).await;
    assert_eq!(
        public(&app, Method::GET, &format!("/api/s/{token}"), None, None)
            .await
            .status_code(),
        404
    );
}

/// Every password try counts against the sign-in limits of the address.
#[tokio::test]
async fn link_passwords_are_limited_as_sign_ins() {
    let app = spawn(Options {
        limits: organizer::limits::Limits::with_bursts(1_000, 4, 1_000),
        ..Options::default()
    })
    .await;
    let (admin, _share) = with_files(&app, &[("Docs/a.txt", "alpha")]).await;
    let docs = id_at(&app, &admin, "/Docs").await;
    let token = json(
        make_link(
            &app,
            &admin,
            &docs,
            json!({ "kind": "view", "password": "open sesame" }),
        )
        .await,
    )
    .await["token"]
        .as_str()
        .unwrap()
        .to_string();
    for _ in 0..4 {
        let response = public(
            &app,
            Method::POST,
            &format!("/api/s/{token}/unlock"),
            None,
            Some(json!({ "password": "guess" })),
        )
        .await;
        assert_eq!(response.status_code(), 403);
    }
    let response = public(
        &app,
        Method::POST,
        &format!("/api/s/{token}/unlock"),
        None,
        Some(json!({ "password": "open sesame" })),
    )
    .await;
    assert_eq!(response.status_code(), 429);
}

/// A stand-in push service (Web Push, or APNs over HTTP/2): it keeps what
/// it is sent, and answers 410 for an endpoint or token that starts with
/// `gone`/`dead`.
mod push_service {
    use axum::body::Bytes;
    use axum::extract::{Path, State};
    use axum::http::{HeaderMap, StatusCode, Version};
    use std::sync::{Arc, Mutex};

    #[derive(Clone, Debug)]
    pub struct Received {
        pub path: String,
        pub version: Version,
        pub headers: HeaderMap,
        pub body: Vec<u8>,
    }

    #[derive(Clone, Default)]
    pub struct PushService {
        pub base: String,
        pub received: Arc<Mutex<Vec<Received>>>,
    }

    impl PushService {
        pub fn received(&self) -> Vec<Received> {
            self.received.lock().unwrap().clone()
        }
    }

    async fn receive(
        State(service): State<PushService>,
        Path(id): Path<String>,
        version: Version,
        headers: HeaderMap,
        body: Bytes,
    ) -> (StatusCode, &'static str) {
        service.received.lock().unwrap().push(Received {
            path: id.clone(),
            version,
            headers,
            body: body.to_vec(),
        });
        if id.starts_with("gone") || id.starts_with("dead") {
            (StatusCode::GONE, r#"{"reason":"Unregistered"}"#)
        } else {
            (StatusCode::CREATED, "")
        }
    }

    pub async fn spawn() -> PushService {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let service = PushService {
            base: format!("http://{}", listener.local_addr().unwrap()),
            ..Default::default()
        };
        let router = axum::Router::new()
            .route("/push/{id}", axum::routing::post(receive))
            .route("/3/device/{id}", axum::routing::post(receive))
            .with_state(service.clone());
        // axum's server takes HTTP/1.1 and HTTP/2 without TLS (h2c), so
        // the APNs client's HTTP/2 reaches it.
        tokio::spawn(async move { axum::serve(listener, router).await.unwrap() });
        service
    }
}

/// A browser's subscription: its P-256 secret and auth secret.
struct Browser {
    secret: web_push_native::p256::SecretKey,
    auth: [u8; 16],
}

impl Browser {
    fn new() -> Browser {
        loop {
            if let Ok(secret) =
                web_push_native::p256::SecretKey::from_slice(&rand::random::<[u8; 32]>())
            {
                return Browser {
                    secret,
                    auth: rand::random(),
                };
            }
        }
    }

    /// The body it registers with.
    fn subscription(&self, endpoint: &str, name: &str) -> Value {
        use web_push_native::p256::elliptic_curve::sec1::ToEncodedPoint;
        let public = self.secret.public_key().to_encoded_point(false);
        json!({
            "kind": "web",
            "name": name,
            "endpoint": endpoint,
            "keys": {
                "p256dh": organizer::push::b64url(public.as_bytes()),
                "auth": organizer::push::b64url(&self.auth),
            },
        })
    }

    /// A push's body, decrypted as the browser would (RFC 8291).
    fn decrypt(&self, body: &[u8]) -> Vec<u8> {
        let mut auth = web_push_native::Auth::default();
        auth.copy_from_slice(&self.auth);
        web_push_native::decrypt(body.to_vec(), &self.secret, &auth).unwrap()
    }
}

async fn user_id(app: &TestApp, session: &str) -> uuid::Uuid {
    session_of(app, Some(session)).await["user"]["id"]
        .as_str()
        .unwrap()
        .parse()
        .unwrap()
}

/// Move the last notice of items to review back in time.
async fn notified_ago(app: &TestApp, user: uuid::Uuid, minutes: u64) {
    sqlx::query("UPDATE notification_settings SET review_notified_at = ?2 WHERE user_id = ?1")
        .bind(user)
        .bind(organizer::auth::iso(
            std::time::SystemTime::now() - std::time::Duration::from_secs(minutes * 60),
        ))
        .execute(app.state.db.writer())
        .await
        .unwrap();
}

async fn notifications(app: &TestApp, session: &str) -> Value {
    let response = request(app, Method::GET, "/api/notifications", Some(session), None).await;
    assert_eq!(response.status_code(), 200);
    json(response).await
}

async fn news(app: &TestApp, session: &str) -> Value {
    let response = request(
        app,
        Method::GET,
        "/api/notifications/news",
        Some(session),
        None,
    )
    .await;
    assert_eq!(response.status_code(), 200);
    json(response).await
}

async fn subscribe(app: &TestApp, session: &str, body: Value) -> Response {
    post(app, "/api/notifications/subscriptions", session, Some(body)).await
}

/// The switches and the devices: defaults, changes, what a device may
/// register with, and that nobody removes another user's device.
#[tokio::test]
async fn notification_settings_and_devices() {
    let app = spawn_app().await;
    let admin = admin_session(&app).await;
    let got = notifications(&app, &admin).await;
    assert_eq!(
        got["settings"],
        json!({ "review": true, "shared": true, "backup": true })
    );
    assert_eq!(got["subscriptions"], json!([]));
    assert_eq!(got["apns"], false);
    // The VAPID public key: an uncompressed P-256 point, the same at the
    // next start (it is stored).
    let key = got["vapid_public_key"].as_str().unwrap().to_string();
    assert_eq!(organizer::push::from_b64url(&key).unwrap().len(), 65);
    let again = organizer::push::Push::load(&app.state.db, Default::default(), false)
        .await
        .unwrap();
    assert_eq!(again.vapid_public_key(), key);

    let changed = json!({ "review": false, "shared": true, "backup": false });
    let response = put(&app, "/api/notifications/settings", &admin, changed.clone()).await;
    assert_eq!(response.status_code(), 200);
    assert_eq!(notifications(&app, &admin).await["settings"], changed);

    let browser = Browser::new();
    // https on a public address only (without LINKS_ALLOW_PRIVATE).
    for endpoint in [
        "http://push.example.com/x",
        "https://127.0.0.1/x",
        "https://10.0.0.1/x",
        "not a url",
    ] {
        let response = subscribe(&app, &admin, browser.subscription(endpoint, "Firefox")).await;
        assert_eq!(response.status_code(), 400, "{endpoint}");
    }
    let mut bad_keys = browser.subscription("https://push.example.com/x", "Firefox");
    bad_keys["keys"]["auth"] = json!("c2hvcnQ");
    assert_eq!(subscribe(&app, &admin, bad_keys).await.status_code(), 400);
    let no_keys =
        json!({ "kind": "web", "name": "Firefox", "endpoint": "https://push.example.com/x" });
    assert_eq!(subscribe(&app, &admin, no_keys).await.status_code(), 400);

    let response = subscribe(
        &app,
        &admin,
        browser.subscription("https://push.example.com/x", "Firefox on Linux"),
    )
    .await;
    assert_eq!(response.status_code(), 201);
    let web = json(response).await;
    assert_eq!(web["kind"], "web");
    assert_eq!(web["available"], true);
    assert_eq!(web["last_notice_at"], Value::Null);
    // The same endpoint again is the same device, renamed.
    let response = subscribe(
        &app,
        &admin,
        browser.subscription("https://push.example.com/x", "Firefox"),
    )
    .await;
    assert_eq!(json(response).await["id"], web["id"]);

    // An iPhone: kept, but not available while APNs is not set up.
    for bad in ["xyz", "abc"] {
        let body = json!({ "kind": "apns", "name": "iPhone", "endpoint": bad });
        assert_eq!(subscribe(&app, &admin, body).await.status_code(), 400);
    }
    let body = json!({ "kind": "apns", "name": "iPhone", "endpoint": "a".repeat(64) });
    let response = subscribe(&app, &admin, body).await;
    assert_eq!(response.status_code(), 201);
    let phone = json(response).await;
    assert_eq!(phone["available"], false);
    let got = notifications(&app, &admin).await;
    let names: Vec<&str> = got["subscriptions"]
        .as_array()
        .unwrap()
        .iter()
        .map(|s| s["name"].as_str().unwrap())
        .collect();
    assert_eq!(names, ["iPhone", "Firefox"]);

    // Another user neither sees nor removes them.
    let (user, _) = invited_user(&app, &admin, "kari@example.com").await;
    assert_eq!(notifications(&app, &user).await["subscriptions"], json!([]));
    let path = format!(
        "/api/notifications/subscriptions/{}",
        phone["id"].as_str().unwrap()
    );
    let response = request(&app, Method::DELETE, &path, Some(&user), None).await;
    assert_eq!(response.status_code(), 404);
    let response = request(&app, Method::DELETE, &path, Some(&admin), None).await;
    assert_eq!(response.status_code(), 204);
    let got = notifications(&app, &admin).await;
    assert_eq!(got["subscriptions"].as_array().unwrap().len(), 1);
}

/// Items waiting for review wake the user's browsers: a VAPID-signed,
/// encrypted push whose content is nothing, at most once a quarter hour,
/// with the count in "what's new"; a device the push service says is gone
/// is removed; each kind has its switch, and a failed backup is for admins.
#[tokio::test]
async fn pushes_wake_the_browsers_with_nothing_in_them() {
    use organizer::push::{Kind, PAYLOAD, check_review, notify_now, notify_superadmins};
    let service = push_service::spawn().await;
    let app = spawn(Options {
        links_allow_private: true,
        public_url: Some("https://organizer.example.com".into()),
        ..Options::default()
    })
    .await;
    let admin = admin_session(&app).await;
    let admin_id = user_id(&app, &admin).await;
    let browser = Browser::new();
    let firefox = format!("{}/push/firefox", service.base);
    let response = subscribe(&app, &admin, browser.subscription(&firefox, "Firefox")).await;
    assert_eq!(response.status_code(), 201);
    let gone = format!("{}/push/gone", service.base);
    let response = subscribe(
        &app,
        &admin,
        Browser::new().subscription(&gone, "Old laptop"),
    )
    .await;
    assert_eq!(response.status_code(), 201);
    let vapid_key = notifications(&app, &admin).await["vapid_public_key"]
        .as_str()
        .unwrap()
        .to_string();

    // Nothing waits: no notice.
    check_review(&app.state, admin_id).await.unwrap();
    assert!(service.received().is_empty());

    // A dropped file waits (there is no AI): a notice.
    let response = drop_files(&app, &admin, &[], &[("bill.txt", "Electricity")]).await;
    let id = json(response).await[0]["id"].as_str().unwrap().to_string();
    assert_eq!(settled(&app, &admin, &id).await["status"], "waiting");
    check_review(&app.state, admin_id).await.unwrap();
    let received = service.received();
    assert_eq!(received.len(), 2);
    let push = received.iter().find(|r| r.path == "firefox").unwrap();
    assert_eq!(push.headers["content-encoding"], "aes128gcm");
    assert_eq!(push.headers["ttl"], "43200");
    assert_eq!(push.headers["topic"], "organizer");
    // VAPID (RFC 8292): signed by the server's key, for the push service's
    // origin, with the public URL as the contact.
    {
        use web_push_native::jwt_simple::prelude::*;
        let authorization = push.headers["authorization"].to_str().unwrap();
        let rest = authorization.strip_prefix("vapid t=").unwrap();
        let (token, k) = rest.split_once(", k=").unwrap();
        assert_eq!(k, vapid_key);
        let key = ES256PublicKey::from_bytes(&organizer::push::from_b64url(k).unwrap()).unwrap();
        let claims = key.verify_token::<NoCustomClaims>(token, None).unwrap();
        assert_eq!(
            claims.subject.as_deref(),
            Some("https://organizer.example.com")
        );
        assert_eq!(
            claims.audiences.unwrap().into_string().unwrap(),
            "http://127.0.0.1"
        );
    }
    // RFC 8291: the browser decrypts it, and it says nothing.
    assert_eq!(browser.decrypt(&push.body), PAYLOAD);
    // The device that is gone was removed; the other got its notice.
    let got = notifications(&app, &admin).await;
    let devices = got["subscriptions"].as_array().unwrap();
    assert_eq!(devices.len(), 1);
    assert!(devices[0]["last_notice_at"].is_string());
    let now = news(&app, &admin).await;
    assert_eq!(now["review"], 1);
    assert_eq!(now["notices"][0]["kind"], "review");
    assert_eq!(now["notices"][0]["body"], "1 item to review");
    assert_eq!(now["notices"][0]["path"], "/drop-box");

    // Within the quarter hour: nothing more, even with another item.
    let response = drop_files(&app, &admin, &[], &[("rent.txt", "Rent")]).await;
    let id = json(response).await[0]["id"].as_str().unwrap().to_string();
    settled(&app, &admin, &id).await;
    check_review(&app.state, admin_id).await.unwrap();
    assert_eq!(service.received().len(), 2);
    // Once it is over: one notice, with both.
    notified_ago(&app, admin_id, 16).await;
    check_review(&app.state, admin_id).await.unwrap();
    assert_eq!(service.received().len(), 3);
    assert_eq!(
        news(&app, &admin).await["notices"][0]["body"],
        "2 items to review"
    );
    // Nothing new since: nothing, however long after.
    sqlx::query("UPDATE inbox_items SET updated_at = ?1")
        .bind(organizer::auth::iso(
            std::time::SystemTime::now() - std::time::Duration::from_secs(7200),
        ))
        .execute(app.state.db.writer())
        .await
        .unwrap();
    notified_ago(&app, admin_id, 60).await;
    check_review(&app.state, admin_id).await.unwrap();
    assert_eq!(service.received().len(), 3);

    // Something shared: a notice of its own, the same empty push.
    notify_now(&app.state, admin_id, Kind::Shared)
        .await
        .unwrap();
    let received = service.received();
    assert_eq!(received.len(), 4);
    assert_eq!(browser.decrypt(&received[3].body), PAYLOAD);
    assert_eq!(news(&app, &admin).await["notices"][0]["kind"], "shared");

    // A failed backup: the admins only.
    let (user, _) = invited_user(&app, &admin, "kari@example.com").await;
    let kari = user_id(&app, &user).await;
    let kari_browser = Browser::new();
    let endpoint = format!("{}/push/kari", service.base);
    let response = subscribe(
        &app,
        &user,
        kari_browser.subscription(&endpoint, "Kari's phone"),
    )
    .await;
    assert_eq!(response.status_code(), 201);
    notify_superadmins(&app.state, Kind::Backup).await.unwrap();
    notify_now(&app.state, kari, Kind::Backup).await.unwrap();
    let received = service.received();
    assert_eq!(received.len(), 5);
    assert_eq!(received[4].path, "firefox");
    let notice = &news(&app, &admin).await["notices"][0];
    assert_eq!(notice["kind"], "backup");
    assert_eq!(notice["path"], "/settings/backups");
    assert_eq!(news(&app, &user).await["notices"], json!([]));

    // Each kind has its switch.
    let off = json!({ "review": false, "shared": false, "backup": false });
    let response = put(&app, "/api/notifications/settings", &admin, off).await;
    assert_eq!(response.status_code(), 200);
    let response = drop_files(&app, &admin, &[], &[("tax.txt", "Tax")]).await;
    let id = json(response).await[0]["id"].as_str().unwrap().to_string();
    settled(&app, &admin, &id).await;
    notified_ago(&app, admin_id, 60).await;
    for kind in [Kind::Review, Kind::Shared, Kind::Backup] {
        notify_now(&app.state, admin_id, kind).await.unwrap();
    }
    assert_eq!(service.received().len(), 5);
    // The other user's switches are their own.
    notify_now(&app.state, kari, Kind::Shared).await.unwrap();
    let received = service.received();
    assert_eq!(received.len(), 6);
    assert_eq!(kari_browser.decrypt(&received[5].body), PAYLOAD);
}

/// A backup's last failed attempt tells the admins.
#[tokio::test]
async fn a_backup_that_failed_for_good_tells_the_admins() {
    use organizer::jobs::Handler;
    let service = push_service::spawn().await;
    let app = spawn(Options {
        links_allow_private: true,
        ..Options::default()
    })
    .await;
    let admin = admin_session(&app).await;
    let endpoint = format!("{}/push/admin", service.base);
    let response = subscribe(
        &app,
        &admin,
        Browser::new().subscription(&endpoint, "Firefox"),
    )
    .await;
    assert_eq!(response.status_code(), 201);
    let job = organizer::jobs::Job {
        id: 1,
        kind: "backup".into(),
        payload: "null".into(),
        attempts: 2,
    };
    organizer::backup::BackupJob
        .give_up(&app.state, job, "the repository is gone".into())
        .await;
    assert_eq!(service.received().len(), 1);
    assert_eq!(news(&app, &admin).await["notices"][0]["kind"], "backup");
}

/// The iPhone app is woken through APNs once it is set up: HTTP/2, a
/// provider token signed with the team's key, the same fixed alert for
/// every notice; a token APNs says is gone is removed.
#[tokio::test]
async fn apns_wakes_the_iphone_app() {
    use web_push_native::jwt_simple::prelude::*;
    let service = push_service::spawn().await;
    let key = ES256KeyPair::generate();
    let app = spawn(Options {
        push: organizer::push::Env {
            vapid_private_key: None,
            apns: Some(organizer::push::ApnsEnv {
                key: key.to_pem().unwrap(),
                key_id: "KEY1234567".into(),
                team_id: "TEAM123456".into(),
                topic: "eu.vardalabs.organizer".into(),
                base_url: service.base.clone(),
            }),
        },
        ..Options::default()
    })
    .await;
    let admin = admin_session(&app).await;
    let admin_id = user_id(&app, &admin).await;
    let token = "0123456789ABCDEF".repeat(4);
    for endpoint in [token.clone(), "dead".repeat(16)] {
        let body = json!({ "kind": "apns", "name": "iPhone", "endpoint": endpoint });
        let response = subscribe(&app, &admin, body).await;
        assert_eq!(response.status_code(), 201);
        assert_eq!(json(response).await["available"], true);
    }

    organizer::push::notify_now(&app.state, admin_id, organizer::push::Kind::Shared)
        .await
        .unwrap();
    let received = service.received();
    assert_eq!(received.len(), 2);
    let push = received
        .iter()
        .find(|r| r.path == token.to_lowercase())
        .unwrap();
    assert_eq!(push.version, axum::http::Version::HTTP_2);
    assert_eq!(push.headers["apns-topic"], "eu.vardalabs.organizer");
    assert_eq!(push.headers["apns-push-type"], "alert");
    let authorization = push.headers["authorization"].to_str().unwrap();
    let bearer = authorization.strip_prefix("Bearer ").unwrap();
    let metadata = Token::decode_metadata(bearer).unwrap();
    assert_eq!(metadata.key_id(), Some("KEY1234567"));
    let claims = key
        .public_key()
        .verify_token::<NoCustomClaims>(bearer, None)
        .unwrap();
    assert_eq!(claims.issuer.as_deref(), Some("TEAM123456"));
    let body: Value = serde_json::from_slice(&push.body).unwrap();
    assert_eq!(body, organizer::push::apns_payload());

    // The token APNs said is gone was removed.
    let got = notifications(&app, &admin).await;
    assert_eq!(got["apns"], true);
    let devices = got["subscriptions"].as_array().unwrap();
    assert_eq!(devices.len(), 1);
    assert!(devices[0]["last_notice_at"].is_string());
}

/// Sharing with someone leaves them a notice (what a push wakes their
/// apps for), pointing at Shared with me; the sharer gets none.
#[tokio::test]
async fn a_share_leaves_the_recipient_a_notice() {
    let app = spawn_app().await;
    let (admin, _share) = with_files(&app, &[("Docs/a.txt", "alpha")]).await;
    let (bob, bob_id) = invited_user(&app, &admin, "bob@example.com").await;
    let docs = id_at(&app, &admin, "/Docs").await;
    let response = share_with(
        &app,
        &admin,
        &docs,
        json!({ "user_id": bob_id }),
        json!({ "create": false, "change": false, "delete": false, "share": false }),
    )
    .await;
    assert_eq!(response.status_code(), 201, "{}", response.text());
    // The notice is written in the background.
    let mut notices = Value::Null;
    for _ in 0..100 {
        notices = news(&app, &bob).await["notices"].clone();
        if notices.as_array().is_some_and(|n| !n.is_empty()) {
            break;
        }
        tokio::time::sleep(std::time::Duration::from_millis(20)).await;
    }
    assert_eq!(notices[0]["kind"], "shared", "{notices}");
    assert_eq!(notices[0]["path"], "/shared");
    assert_eq!(news(&app, &admin).await["notices"], json!([]));
}

/// Unpadded base64url of `len` bytes, the first `first` (a version byte
/// for the wrapped keys): stand-ins for keys the server cannot open.
fn opaque(len: usize, first: u8) -> String {
    const ALPHABET: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789-_";
    let mut bytes: Vec<u8> = (0..len).map(|i| (i * 7 + 3) as u8).collect();
    if let Some(b) = bytes.first_mut() {
        *b = first;
    }
    let mut out = String::new();
    for chunk in bytes.chunks(3) {
        let b = [
            chunk[0],
            *chunk.get(1).unwrap_or(&0),
            *chunk.get(2).unwrap_or(&0),
        ];
        let n = (b[0] as u32) << 16 | (b[1] as u32) << 8 | b[2] as u32;
        for i in 0..=chunk.len() {
            out.push(ALPHABET[(n >> (18 - 6 * i) & 63) as usize] as char);
        }
    }
    out
}

/// Set up encryption for a user, with stand-in keys.
async fn set_up_encryption(app: &TestApp, session: &str) -> Response {
    post(
        app,
        "/api/encryption",
        session,
        Some(json!({
            "public_key": opaque(32, 9),
            "passphrase": { "salt": opaque(16, 0), "iterations": 600_000, "wrapped": opaque(61, 1) },
            "recovery_wrapped": opaque(61, 1),
        })),
    )
    .await
}

async fn folder_keys(app: &TestApp, session: &str, id: &str) -> Response {
    request(
        app,
        Method::GET,
        &format!("/api/encryption/folders/{id}"),
        Some(session),
        None,
    )
    .await
}

async fn post_keys(app: &TestApp, session: &str, id: &str, keys: Value) -> Response {
    post(
        app,
        &format!("/api/encryption/folders/{id}/keys"),
        session,
        Some(json!({ "keys": keys })),
    )
    .await
}

async fn move_to(app: &TestApp, session: &str, id: &str, parent: &str) -> Response {
    let version = version_of(app, session, id).await;
    request(
        app,
        Method::PATCH,
        &format!("/api/files/{id}"),
        Some(session),
        Some(json!({ "parent_id": parent, "if_version": version })),
    )
    .await
}

async fn pending(app: &TestApp, session: &str) -> Value {
    json(
        request(
            app,
            Method::GET,
            "/api/encryption/pending",
            Some(session),
            None,
        )
        .await,
    )
    .await
}

async fn root_id(app: &TestApp, session: &str) -> String {
    json(request(app, Method::GET, "/api/files/root", Some(session), None).await).await["id"]
        .as_str()
        .unwrap()
        .to_string()
}

/// An encrypted folder made in `parent`: its id.
async fn encrypted_folder(app: &TestApp, session: &str, parent: &str, name: &str) -> String {
    let response = post(
        app,
        &format!("/api/files/{parent}/folders"),
        session,
        Some(json!({ "name": name, "encrypted": true })),
    )
    .await;
    assert_eq!(response.status_code(), 201, "{}", response.text());
    json(response).await["id"].as_str().unwrap().to_string()
}

/// Keys: set up once (session only), read with a key, the passphrase
/// changed. An encrypted folder: made only once set up, its first key
/// posted by the owner's device, what goes in it encrypted (`e2e_root`),
/// nothing moved across its edge, no folder of its kind inside it.
#[tokio::test]
async fn encryption_keys_and_encrypted_folders() {
    let app = spawn_app().await;
    let (admin, _share) = with_files(&app, &[("Plain/a.txt", "alpha")]).await;
    let admin_id = user_id(&app, &admin).await.to_string();
    let plain = id_at(&app, &admin, "/Plain").await;
    let a = id_at(&app, &admin, "/Plain/a.txt").await;

    let keys = json(request(&app, Method::GET, "/api/encryption", Some(&admin), None).await).await;
    assert_eq!(keys["set_up"], false);
    let response = post(
        &app,
        &format!("/api/files/{plain}/folders"),
        &admin,
        Some(json!({ "name": "Secret", "encrypted": true })),
    )
    .await;
    assert_eq!(response.status_code(), 400, "not set up yet");

    // Malformed keys are refused; good ones once.
    let response = post(
        &app,
        "/api/encryption",
        &admin,
        Some(json!({
            "public_key": opaque(31, 0),
            "passphrase": { "salt": opaque(16, 0), "iterations": 600_000, "wrapped": opaque(61, 1) },
            "recovery_wrapped": opaque(61, 1),
        })),
    )
    .await;
    assert_eq!(response.status_code(), 400);
    let response = post(
        &app,
        "/api/encryption",
        &admin,
        Some(json!({
            "public_key": opaque(32, 0),
            "passphrase": { "salt": opaque(16, 0), "iterations": 1000, "wrapped": opaque(61, 1) },
            "recovery_wrapped": opaque(61, 1),
        })),
    )
    .await;
    assert_eq!(response.status_code(), 400, "too few iterations");
    let response = set_up_encryption(&app, &admin).await;
    assert_eq!(response.status_code(), 201, "{}", response.text());
    assert_eq!(set_up_encryption(&app, &admin).await.status_code(), 409);
    // Read with an API key; set up and changed with a session only.
    let key = create_key(&app, &admin, "write").await["token"]
        .as_str()
        .unwrap()
        .to_string();
    let keys = json(with_key(&app, Method::GET, "/api/encryption", &key, None).await).await;
    assert_eq!(keys["set_up"], true);
    assert_eq!(keys["public_key"], opaque(32, 9));
    assert_eq!(keys["passphrase"]["iterations"], 600_000);
    let new_wrap =
        json!({ "salt": opaque(16, 5), "iterations": 700_000, "wrapped": opaque(61, 1) });
    let response = with_key(
        &app,
        Method::PUT,
        "/api/encryption/passphrase",
        &key,
        Some(new_wrap.clone()),
    )
    .await;
    assert!(response.status_code().is_client_error());
    let response = put(&app, "/api/encryption/passphrase", &admin, new_wrap).await;
    assert_eq!(response.status_code(), 200, "{}", response.text());
    assert_eq!(json(response).await["passphrase"]["iterations"], 700_000);

    // An encrypted folder: its own name plain, its id its e2e_root.
    let secret_id = encrypted_folder(&app, &admin, &plain, "Secret").await;
    let secret = json(
        request(
            &app,
            Method::GET,
            &format!("/api/files/{secret_id}"),
            Some(&admin),
            None,
        )
        .await,
    )
    .await;
    assert_eq!(secret["name"], "Secret");
    assert_eq!(secret["e2e_root"], secret_id.as_str());
    let keys = json(folder_keys(&app, &admin, &secret_id).await).await;
    assert_eq!(keys["generation"], 0);
    assert_eq!(keys["needs_new_key"], true);
    assert_eq!(keys["members"][0]["user_id"], admin_id.as_str());
    let tasks = pending(&app, &admin).await;
    assert_eq!(tasks[0]["folder_id"], secret_id.as_str());
    assert_eq!(tasks[0]["needs_new_key"], true);
    // A plain folder's keys are a 400.
    assert_eq!(folder_keys(&app, &admin, &plain).await.status_code(), 400);

    // The first key: generation 1, not 2; of a known format.
    let wrapped = opaque(93, 1);
    let response = post_keys(
        &app,
        &admin,
        &secret_id,
        json!([{ "user_id": admin_id, "generation": 2, "wrapped": wrapped }]),
    )
    .await;
    assert_eq!(response.status_code(), 400);
    let response = post_keys(
        &app,
        &admin,
        &secret_id,
        json!([{ "user_id": admin_id, "generation": 1, "wrapped": opaque(93, 2) }]),
    )
    .await;
    assert_eq!(response.status_code(), 400, "an unknown format version");
    let response = post_keys(
        &app,
        &admin,
        &secret_id,
        json!([{ "user_id": admin_id, "generation": 1, "wrapped": wrapped }]),
    )
    .await;
    assert_eq!(response.status_code(), 200, "{}", response.text());
    let keys = json(response).await;
    assert_eq!(keys["generation"], 1);
    assert_eq!(keys["needs_new_key"], false);
    assert_eq!(keys["keys"][0]["wrapped"], wrapped);
    assert_eq!(pending(&app, &admin).await, json!([]));

    // Not one inside another; a plain folder inside is encrypted.
    let response = post(
        &app,
        &format!("/api/files/{secret_id}/folders"),
        &admin,
        Some(json!({ "name": "Inner", "encrypted": true })),
    )
    .await;
    assert_eq!(response.status_code(), 400);
    let response = post(
        &app,
        &format!("/api/files/{secret_id}/folders"),
        &admin,
        Some(json!({ "name": opaque(40, 1) })),
    )
    .await;
    assert_eq!(response.status_code(), 201);
    let sub = json(response).await;
    let sub_id = sub["id"].as_str().unwrap().to_string();
    assert_eq!(sub["e2e_root"], secret_id.as_str());
    let response = upload(&app, &admin, &sub_id, &opaque(45, 1), "opaque bytes").await;
    assert_eq!(response.status_code(), 201);
    let file = json(response).await;
    let file_id = file["id"].as_str().unwrap().to_string();
    assert_eq!(file["e2e_root"], secret_id.as_str());
    assert_eq!(file["mime"], json!(null));
    let response = request(
        &app,
        Method::GET,
        &format!("/api/files/{file_id}/content"),
        Some(&admin),
        None,
    )
    .await;
    assert_eq!(response.text(), "opaque bytes");
    // The feed says which are encrypted.
    let changes = json(
        request(
            &app,
            Method::GET,
            "/api/changes?cursor=0.0",
            Some(&admin),
            None,
        )
        .await,
    )
    .await;
    let change = changes["changes"]
        .as_array()
        .unwrap()
        .iter()
        .find(|c| c["node_id"] == file["id"])
        .expect("the upload's change");
    assert_eq!(change["e2e_root"], secret_id.as_str());

    // Nothing crosses the edge; the top goes anywhere plain.
    assert_eq!(
        move_to(&app, &admin, &a, &secret_id).await.status_code(),
        400
    );
    let root = root_id(&app, &admin).await;
    assert_eq!(
        move_to(&app, &admin, &file_id, &root).await.status_code(),
        400
    );
    assert_eq!(
        move_to(&app, &admin, &file_id, &secret_id)
            .await
            .status_code(),
        200
    );
    assert_eq!(
        move_to(&app, &admin, &secret_id, &root).await.status_code(),
        200
    );
    assert_eq!(
        move_to(&app, &admin, &secret_id, &plain)
            .await
            .status_code(),
        200
    );
    let keys = json(folder_keys(&app, &admin, &file_id).await).await;
    assert_eq!(keys["folder_id"], secret_id.as_str(), "found from inside");
}

/// The server cannot read encrypted folders: no links, no search, no
/// WebDAV; a plain folder's link and zip leave them out.
#[tokio::test]
async fn encrypted_folders_are_left_out_of_what_the_server_reads() {
    let app = spawn_app().await;
    let (admin, share) = with_files(&app, &[("Plain/needle.txt", "alpha")]).await;
    assert_eq!(set_up_encryption(&app, &admin).await.status_code(), 201);
    let plain = id_at(&app, &admin, "/Plain").await;
    let secret_id = encrypted_folder(&app, &admin, &plain, "needle vault").await;
    let inside = upload(&app, &admin, &secret_id, "needle inside.txt", "x").await;
    assert_eq!(inside.status_code(), 201);
    assert!(
        share
            .0
            .join("Plain/needle vault/needle inside.txt")
            .is_file()
    );

    // Links: refused on it; a plain folder's link leaves it out.
    let response = make_link(&app, &admin, &secret_id, json!({ "kind": "view" })).await;
    assert_eq!(response.status_code(), 403, "{}", response.text());
    let response = make_link(&app, &admin, &plain, json!({ "kind": "edit" })).await;
    assert_eq!(response.status_code(), 201, "{}", response.text());
    let token = json(response).await["token"].as_str().unwrap().to_string();
    let listing = json(app.server.get(&format!("/api/s/{token}/children")).await).await;
    let names: Vec<&str> = listing["items"]
        .as_array()
        .unwrap()
        .iter()
        .map(|i| i["name"].as_str().unwrap())
        .collect();
    assert_eq!(names, ["needle.txt"]);
    let response = app
        .server
        .get(&format!("/api/s/{token}/children?folder={secret_id}"))
        .await;
    assert_eq!(response.status_code(), 404);
    let zip = app
        .server
        .get(&format!("/api/s/{token}/folders/{plain}/zip"))
        .await;
    assert_eq!(zip.status_code(), 200);
    let bytes = zip.as_bytes().to_vec();
    let text = String::from_utf8_lossy(&bytes);
    assert!(text.contains("needle.txt") && !text.contains("needle vault"));

    // Search: the plain file, nothing of the encrypted folder.
    found(&app, &admin, "needle", |h| {
        !h.as_array().unwrap().is_empty()
    })
    .await;
    // A moment more for the rest to be indexed, if it were (the text of
    // the plain file may come in meanwhile: only the names are compared).
    tokio::time::sleep(std::time::Duration::from_millis(300)).await;
    let hits = search_for(&app, &admin, "needle").await;
    let names: Vec<&str> = hits
        .as_array()
        .unwrap()
        .iter()
        .map(|h| h["name"].as_str().unwrap())
        .collect();
    assert_eq!(names, ["needle.txt"]);

    // WebDAV: hidden.
    let body = json!({ "name": "rclone", "scope": "write", "kind": "app_password" });
    let password = json(post(&app, "/api/auth/api-keys", &admin, Some(body)).await).await["token"]
        .as_str()
        .unwrap()
        .to_string();
    let auth = basic(&password);
    let dav = |method: &str, path: &str| {
        app.server
            .method(Method::from_bytes(method.as_bytes()).unwrap(), path)
            .add_header("authorization", &auth)
    };
    let response = dav("PROPFIND", "/dav/Plain/")
        .add_header("depth", "1")
        .await;
    assert_eq!(response.status_code(), 207);
    assert!(response.text().contains("needle.txt"));
    assert!(!response.text().contains("vault"), "{}", response.text());
    assert_eq!(
        dav("GET", "/dav/Plain/needle%20vault/needle%20inside.txt")
            .await
            .status_code(),
        404
    );
    assert_eq!(
        dav("PUT", "/dav/Plain/needle%20vault/new.txt")
            .bytes("x".into())
            .await
            .status_code(),
        404
    );
    let response = dav("MOVE", "/dav/Plain/needle.txt")
        .add_header(
            "destination",
            format!("{}/dav/Plain/needle%20vault/needle.txt", app.base),
        )
        .await;
    assert_eq!(response.status_code(), 404);
}

/// Members: the owner and those it is shared with. One without keys
/// waits; once set up, the owner's device wraps for them. A removed member
/// loses their keys and the folder needs a new one, which must cover every
/// member left.
#[tokio::test]
async fn encrypted_folders_are_shared_and_rekeyed() {
    let app = spawn_app().await;
    let (admin, _share) = with_files(&app, &[("Plain/a.txt", "alpha")]).await;
    let admin_id = user_id(&app, &admin).await.to_string();
    let (bob, bob_id) = invited_user(&app, &admin, "bob@example.com").await;
    assert_eq!(set_up_encryption(&app, &admin).await.status_code(), 201);
    let root = root_id(&app, &admin).await;
    let secret_id = encrypted_folder(&app, &admin, &root, "Secret").await;
    let first = json!([{ "user_id": admin_id, "generation": 1, "wrapped": opaque(93, 1) }]);
    assert_eq!(
        post_keys(&app, &admin, &secret_id, first)
            .await
            .status_code(),
        200
    );
    let file = json(upload(&app, &admin, &secret_id, &opaque(40, 1), "x").await).await;
    let file_id = file["id"].as_str().unwrap().to_string();

    // Not what is in it: the folder whole.
    let response = share_with(
        &app,
        &admin,
        &file_id,
        json!({ "user_id": bob_id }),
        json!({}),
    )
    .await;
    assert_eq!(response.status_code(), 400);
    let response = share_with(
        &app,
        &admin,
        &secret_id,
        json!({ "user_id": bob_id }),
        json!({ "create": true }),
    )
    .await;
    assert_eq!(response.status_code(), 201, "{}", response.text());
    let share_id = json(response).await["id"].as_str().unwrap().to_string();

    // Bob has no keys yet: waiting, and no key can be posted for him.
    let keys = json(folder_keys(&app, &admin, &secret_id).await).await;
    assert_eq!(keys["members"][1]["user_id"], bob_id.as_str());
    assert_eq!(keys["members"][1]["public_key"], json!(null));
    assert_eq!(keys["members"][1]["missing"], json!([]));
    let for_bob = json!([{ "user_id": bob_id, "generation": 1, "wrapped": opaque(93, 1) }]);
    assert_eq!(
        post_keys(&app, &admin, &secret_id, for_bob.clone())
            .await
            .status_code(),
        400
    );
    // Bob sets up: the owner's device is told to wrap for him.
    assert_eq!(set_up_encryption(&app, &bob).await.status_code(), 201);
    let tasks = pending(&app, &admin).await;
    assert_eq!(tasks[0]["waiting"], json!([bob_id]));
    assert_eq!(
        post_keys(&app, &admin, &secret_id, for_bob)
            .await
            .status_code(),
        200
    );
    let bobs = json(folder_keys(&app, &bob, &secret_id).await).await;
    assert_eq!(bobs["keys"].as_array().unwrap().len(), 1);
    assert_eq!(bobs["members"][1]["missing"], json!([]));
    // Bob may place it, but not inside an encrypted folder of his own.
    let bob_root = root_id(&app, &bob).await;
    let his = encrypted_folder(&app, &bob, &bob_root, "Mine").await;
    let response = post(
        &app,
        &format!("/api/shared/{secret_id}/place"),
        &bob,
        Some(json!({ "parent_id": his })),
    )
    .await;
    assert_eq!(response.status_code(), 400, "{}", response.text());
    // Nor make an encrypted folder in someone else's.
    let response = post(
        &app,
        &format!("/api/files/{secret_id}/folders"),
        &bob,
        Some(json!({ "name": "Nested", "encrypted": true })),
    )
    .await;
    assert_eq!(response.status_code(), 400);

    // Bob leaves: his keys go, and a new key is needed.
    let response = request(
        &app,
        Method::DELETE,
        &format!("/api/files/{secret_id}/shares/{share_id}"),
        Some(&admin),
        None,
    )
    .await;
    assert!(response.status_code().is_success(), "{}", response.text());
    assert_eq!(folder_keys(&app, &bob, &secret_id).await.status_code(), 404);
    let keys = json(folder_keys(&app, &admin, &secret_id).await).await;
    assert_eq!(keys["needs_new_key"], true);
    assert_eq!(keys["members"].as_array().unwrap().len(), 1);
    let held: i64 = sqlx::query_scalar(
        "SELECT COUNT(*) FROM e2e_folder_keys
         WHERE user_id = (SELECT id FROM users WHERE email = 'bob@example.com')",
    )
    .fetch_one(app.state.db.reader())
    .await
    .unwrap();
    assert_eq!(held, 0);
    assert_eq!(pending(&app, &admin).await[0]["needs_new_key"], true);
    let second = json!([{ "user_id": admin_id, "generation": 2, "wrapped": opaque(93, 1) }]);
    let keys = json(post_keys(&app, &admin, &secret_id, second).await).await;
    assert_eq!(keys["generation"], 2);
    assert_eq!(keys["needs_new_key"], false);
    assert_eq!(
        keys["keys"].as_array().unwrap().len(),
        2,
        "the old one kept"
    );
}

/// The trash keeps an encrypted folder's ids (names inside are sealed with
/// their parent's), and its keys until it is purged. Something inside goes
/// back only into its own folder.
#[tokio::test]
async fn encrypted_folders_come_back_from_the_trash_with_their_ids() {
    let app = spawn_app().await;
    let (admin, _share) = with_files(&app, &[("Plain/a.txt", "alpha")]).await;
    let admin_id = user_id(&app, &admin).await.to_string();
    assert_eq!(set_up_encryption(&app, &admin).await.status_code(), 201);
    let plain = id_at(&app, &admin, "/Plain").await;
    let secret_id = encrypted_folder(&app, &admin, &plain, "Secret").await;
    let first = json!([{ "user_id": admin_id, "generation": 1, "wrapped": opaque(93, 1) }]);
    assert_eq!(
        post_keys(&app, &admin, &secret_id, first)
            .await
            .status_code(),
        200
    );
    let sub = json(
        post(
            &app,
            &format!("/api/files/{secret_id}/folders"),
            &admin,
            Some(json!({ "name": opaque(40, 1) })),
        )
        .await,
    )
    .await;
    let sub_id = sub["id"].as_str().unwrap().to_string();
    let file = json(upload(&app, &admin, &sub_id, &opaque(41, 1), "x").await).await;
    let file_id = file["id"].as_str().unwrap().to_string();
    let other = json(upload(&app, &admin, &secret_id, &opaque(42, 1), "y").await).await;
    let other_id = other["id"].as_str().unwrap().to_string();

    // The plain folder above it, whole: back with every id.
    assert_eq!(delete_node(&app, &admin, &plain).await.status_code(), 200);
    let response = post(&app, &format!("/api/trash/{plain}/restore"), &admin, None).await;
    assert_eq!(response.status_code(), 200, "{}", response.text());
    for id in [&secret_id, &sub_id, &file_id, &other_id] {
        let node = json(
            request(
                &app,
                Method::GET,
                &format!("/api/files/{id}"),
                Some(&admin),
                None,
            )
            .await,
        )
        .await;
        assert_eq!(node["id"], id.as_str());
        assert_eq!(node["e2e_root"], secret_id.as_str());
    }
    let a = json(by_path(&app, &admin, "/Plain/a.txt").await).await;
    assert_eq!(a["e2e_root"], json!(null));
    let keys = json(folder_keys(&app, &admin, &secret_id).await).await;
    assert_eq!(keys["generation"], 1, "its keys stayed");

    // A file inside, then the folder: the file waits for its folder.
    assert_eq!(
        delete_node(&app, &admin, &other_id).await.status_code(),
        200
    );
    assert_eq!(
        delete_node(&app, &admin, &secret_id).await.status_code(),
        200
    );
    let trash = json(request(&app, Method::GET, "/api/trash", Some(&admin), None).await).await;
    let item = trash
        .as_array()
        .unwrap()
        .iter()
        .find(|i| i["id"] == other_id.as_str())
        .unwrap()
        .clone();
    assert_eq!(item["e2e_root"], secret_id.as_str());
    assert_eq!(item["original_parent_id"], secret_id.as_str());
    let response = post(
        &app,
        &format!("/api/trash/{other_id}/restore"),
        &admin,
        None,
    )
    .await;
    assert_eq!(response.status_code(), 409);
    let response = post(
        &app,
        &format!("/api/trash/{secret_id}/restore"),
        &admin,
        None,
    )
    .await;
    assert_eq!(response.status_code(), 200, "{}", response.text());
    let response = post(
        &app,
        &format!("/api/trash/{other_id}/restore"),
        &admin,
        None,
    )
    .await;
    assert_eq!(response.status_code(), 200, "{}", response.text());
    assert_eq!(json(response).await["e2e_root"], secret_id.as_str());
    assert_eq!(
        status_of(&app, &admin, &format!("/api/files/{file_id}")).await,
        200
    );

    // Purged: its keys go.
    assert_eq!(
        delete_node(&app, &admin, &secret_id).await.status_code(),
        200
    );
    let response = request(
        &app,
        Method::DELETE,
        &format!("/api/trash/{secret_id}"),
        Some(&admin),
        None,
    )
    .await;
    assert_eq!(response.status_code(), 204);
    let left: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM e2e_folders")
        .fetch_one(app.state.db.reader())
        .await
        .unwrap();
    assert_eq!(left, 0);
}

/// The user's drives, by name: each as `GET /api/drives` answers it.
async fn drives_of(app: &TestApp, session: &str) -> std::collections::HashMap<String, Value> {
    let response = request(app, Method::GET, "/api/drives", Some(session), None).await;
    assert_eq!(response.status_code(), 200, "{}", response.text());
    json(response)
        .await
        .as_array()
        .unwrap()
        .iter()
        .map(|d| (d["name"].as_str().unwrap().to_string(), d.clone()))
        .collect()
}

/// The children's names of a folder.
async fn names_in(app: &TestApp, session: &str, folder: &str) -> Vec<String> {
    let response = request(
        app,
        Method::GET,
        &format!("/api/files/{folder}/children"),
        Some(session),
        None,
    )
    .await;
    assert_eq!(response.status_code(), 200, "{}", response.text());
    json(response).await["items"]
        .as_array()
        .unwrap()
        .iter()
        .map(|n| n["name"].as_str().unwrap().to_string())
        .collect()
}

/// The superadmin makes a shared drive: its members see it with their My
/// drive and work in it as editors; who manages it adds and removes
/// members; everyone else, the superadmin included, gets a 404 for what is
/// in it; the change feed of each member carries the drive's id.
#[tokio::test]
async fn shared_drives_are_their_members_alone() {
    let app = spawn_app().await;
    let (admin, _share) = with_files(&app, &[("mine.txt", "mine")]).await;
    let (alice, _) = invited_user(&app, &admin, "alice@example.com").await;
    let (bob, _) = invited_user(&app, &admin, "bob@example.com").await;
    let bob_cursor = json(request(&app, Method::GET, "/api/changes", Some(&bob), None).await).await
        ["cursor"]
        .as_str()
        .unwrap()
        .to_string();

    let made = post(
        &app,
        "/api/superadmin/drives",
        &admin,
        Some(json!({
            "name": "Team",
            "members": [{ "email": "alice@example.com", "manages": true }],
        })),
    )
    .await;
    assert_eq!(made.status_code(), 201, "{}", made.text());
    let made = json(made).await;
    let drive = made["drive"]["id"].as_str().unwrap().to_string();
    assert_eq!(made["drive"]["kind"], "shared");
    assert_eq!(made["drive"]["members"], 1);
    assert_eq!(made["drive"]["managers"], json!(["alice@example.com"]));
    // A name is one drive's only.
    let again = post(
        &app,
        "/api/superadmin/drives",
        &admin,
        Some(json!({ "name": "team" })),
    )
    .await;
    assert_eq!(again.status_code(), 409);

    // Alice has My drive and Team; Bob only his own; the superadmin, who
    // named someone else to manage it, is not a member.
    let hers = drives_of(&app, &alice).await;
    assert_eq!(hers.len(), 2);
    assert_eq!(hers["My drive"]["kind"], "my");
    assert_eq!(hers["My drive"]["manages"], true);
    assert_eq!(hers["Team"]["manages"], true);
    let root = hers["Team"]["root_id"].as_str().unwrap().to_string();
    assert_eq!(drives_of(&app, &bob).await.len(), 1);
    assert_eq!(drives_of(&app, &admin).await.len(), 1);

    // Alice works in it; it is not hers alone, but the drive's.
    assert_eq!(
        upload(&app, &alice, &root, "plan.txt", "plan")
            .await
            .status_code(),
        201
    );
    let plan = json(
        request(
            &app,
            Method::GET,
            &format!("/api/files/{root}/children"),
            Some(&alice),
            None,
        )
        .await,
    )
    .await["items"][0]
        .clone();
    assert_eq!(plan["name"], "plan.txt");
    assert_eq!(plan["drive_id"], drive.as_str());
    let plan_id = plan["id"].as_str().unwrap().to_string();
    // Its trash is the drive's.
    assert_eq!(delete_node(&app, &alice, &plan_id).await.status_code(), 200);
    let trash = json(
        request(
            &app,
            Method::GET,
            &format!("/api/trash?drive={drive}"),
            Some(&alice),
            None,
        )
        .await,
    )
    .await;
    assert_eq!(trash[0]["name"], "plan.txt");
    let mine = json(request(&app, Method::GET, "/api/trash", Some(&alice), None).await).await;
    assert_eq!(mine, json!([]));
    let restored = post(&app, &format!("/api/trash/{plan_id}/restore"), &alice, None).await;
    assert_eq!(restored.status_code(), 200, "{}", restored.text());

    // Not a member: nothing of it, as if it did not exist.
    for who in [&bob, &admin] {
        assert_eq!(
            status_of(&app, who, &format!("/api/files/{root}")).await,
            404
        );
        assert_eq!(
            status_of(&app, who, &format!("/api/files/{plan_id}")).await,
            404
        );
        assert_eq!(
            status_of(&app, who, &format!("/api/drives/{drive}")).await,
            404
        );
        assert_eq!(
            status_of(&app, who, &format!("/api/trash?drive={drive}")).await,
            404
        );
        assert_eq!(
            upload(&app, who, &root, "x.txt", "x").await.status_code(),
            404
        );
    }
    assert_eq!(status_of(&app, &bob, "/api/drives/not-a-drive").await, 404);

    // Alice manages it: she adds Bob, who may then do what an editor does,
    // but not manage it.
    let added = post(
        &app,
        &format!("/api/drives/{drive}/access"),
        &alice,
        Some(json!({ "email": "bob@example.com", "role": "editor" })),
    )
    .await;
    assert_eq!(added.status_code(), 201, "{}", added.text());
    let bobs_grant = json(added).await["id"].as_str().unwrap().to_string();
    assert_eq!(names_in(&app, &bob, &root).await, ["plan.txt"]);
    assert_eq!(
        upload(&app, &bob, &root, "notes.txt", "n")
            .await
            .status_code(),
        201
    );
    assert_eq!(
        rename(&app, &bob, &plan_id, "Plan.txt").await.status_code(),
        200
    );
    let bobs = drives_of(&app, &bob).await;
    assert_eq!(bobs["Team"]["manages"], false);
    assert_eq!(bobs["Team"]["members"], 2);
    let refused = post(
        &app,
        &format!("/api/drives/{drive}/access"),
        &bob,
        Some(json!({ "email": "admin@example.com", "role": "editor" })),
    )
    .await;
    assert_eq!(refused.status_code(), 403);
    let renamed = request(
        &app,
        Method::PATCH,
        &format!("/api/drives/{drive}"),
        Some(&bob),
        Some(json!({ "name": "Mine now" })),
    )
    .await;
    assert_eq!(renamed.status_code(), 403);
    // Sharing from it is for who manages it.
    let shared = share_with(
        &app,
        &bob,
        &plan_id,
        json!({ "user_id": session_of(&app, Some(&admin)).await["user"]["id"] }),
        json!({}),
    )
    .await;
    assert_eq!(shared.status_code(), 403);
    // Nothing moves between drives: a copy is the way.
    let my_root = root_id(&app, &bob).await;
    assert_eq!(
        move_to(&app, &bob, &plan_id, &my_root).await.status_code(),
        400
    );

    // Bob's feed has the drive joined (its root), then what changed in it,
    // each with the drive's id.
    let page = json(
        request(
            &app,
            Method::GET,
            &format!("/api/changes?cursor={bob_cursor}"),
            Some(&bob),
            None,
        )
        .await,
    )
    .await;
    let changes = page["changes"].as_array().unwrap();
    assert!(
        changes
            .iter()
            .any(|c| c["node_id"] == root.as_str() && c["op"] == "create"),
        "{page}"
    );
    assert!(
        changes.iter().all(|c| c["drive_id"] == drive.as_str()),
        "{page}"
    );
    assert!(changes.iter().any(|c| c["name"] == "notes.txt"), "{page}");
    let cursor = page["cursor"].as_str().unwrap().to_string();

    // Alice renames it; the last who manages it cannot leave it so.
    let renamed = request(
        &app,
        Method::PATCH,
        &format!("/api/drives/{drive}"),
        Some(&alice),
        Some(json!({ "name": "Projects" })),
    )
    .await;
    assert_eq!(renamed.status_code(), 200, "{}", renamed.text());
    let hers = grant_drive_role_of(&app, &alice, &drive, "alice@example.com").await;
    let last = request(
        &app,
        Method::DELETE,
        &format!("/api/drives/{drive}/access/{hers}"),
        Some(&alice),
        None,
    )
    .await;
    assert_eq!(last.status_code(), 409);

    // Bob is taken out: the drive's root leaves his feed, and he reaches
    // nothing of it any more.
    let removed = request(
        &app,
        Method::DELETE,
        &format!("/api/drives/{drive}/access/{bobs_grant}"),
        Some(&alice),
        None,
    )
    .await;
    assert_eq!(removed.status_code(), 204);
    assert_eq!(
        status_of(&app, &bob, &format!("/api/files/{root}")).await,
        404
    );
    let page = json(
        request(
            &app,
            Method::GET,
            &format!("/api/changes?cursor={cursor}"),
            Some(&bob),
            None,
        )
        .await,
    )
    .await;
    assert_eq!(page["changes"][0]["node_id"], root.as_str(), "{page}");
    assert_eq!(page["changes"][0]["op"], "delete");
    assert_eq!(drives_of(&app, &bob).await.len(), 1);

    // The superadmin's list shows it (renamed), and deletes it.
    let list = json(
        request(
            &app,
            Method::GET,
            "/api/superadmin/drives",
            Some(&admin),
            None,
        )
        .await,
    )
    .await;
    assert_eq!(list[0]["drive"]["name"], "Projects");
    assert!(list[0]["folder"].as_str().unwrap().ends_with(&drive));
    let folder = std::path::PathBuf::from(list[0]["folder"].as_str().unwrap());
    assert!(folder.join("Plan.txt").is_file());
    let deleted = request(
        &app,
        Method::DELETE,
        &format!("/api/superadmin/drives/{drive}"),
        Some(&admin),
        None,
    )
    .await;
    assert_eq!(deleted.status_code(), 204);
    assert!(!folder.exists());
    assert_eq!(drives_of(&app, &alice).await.len(), 1);
    assert_eq!(
        status_of(&app, &alice, &format!("/api/files/{root}")).await,
        404
    );
}

/// Shared drives are the superadmin's to make, with a fresh second factor
/// and a session: an API key is refused, even a superadmin's.
#[tokio::test]
async fn shared_drives_are_made_by_the_superadmin_with_a_session() {
    let app = spawn_app().await;
    let admin = admin_session(&app).await;
    let (alice, _) = invited_user(&app, &admin, "alice@example.com").await;
    let body = json!({ "name": "Team" });
    assert_eq!(
        post(&app, "/api/superadmin/drives", &alice, Some(body.clone()))
            .await
            .status_code(),
        403
    );
    let key = create_key(&app, &admin, "write").await["token"]
        .as_str()
        .unwrap()
        .to_string();
    let response = with_key(
        &app,
        Method::POST,
        "/api/superadmin/drives",
        &key,
        Some(body.clone()),
    )
    .await;
    assert_eq!(response.status_code(), 403);
    stepped_up(&app, &admin, 20 * 60).await;
    let response = post(&app, "/api/superadmin/drives", &admin, Some(body.clone())).await;
    assert_eq!(response.status_code(), 403);
    assert_eq!(json(response).await["code"], "second_factor_required");
    stepped_up(&app, &admin, 0).await;
    let response = post(&app, "/api/superadmin/drives", &admin, Some(body)).await;
    assert_eq!(response.status_code(), 201, "{}", response.text());
    // Nobody named to manage it: the superadmin does, as a member.
    let made = json(response).await;
    assert_eq!(made["drive"]["managers"], json!(["admin@example.com"]));
    assert_eq!(drives_of(&app, &admin).await["Team"]["manages"], true);
}

/// Each drive has its own storage and limit: a shared drive's limit stops
/// writes into it, and My drive does not count what is in it.
#[tokio::test]
async fn storage_counts_per_drive() {
    let app = spawn_app().await;
    let (admin, _share) = with_files(&app, &[("mine.txt", "0123456789")]).await;
    let made = json(
        post(
            &app,
            "/api/superadmin/drives",
            &admin,
            Some(json!({ "name": "Small", "storage_limit": 12 })),
        )
        .await,
    )
    .await;
    let drive = made["drive"]["id"].as_str().unwrap().to_string();
    let root = made["drive"]["root_id"].as_str().unwrap().to_string();
    assert_eq!(made["drive"]["limit"], 12);
    assert_eq!(made["drive"]["used"], 0);
    assert_eq!(
        upload(&app, &admin, &root, "a.txt", "0123456789")
            .await
            .status_code(),
        201
    );
    assert_eq!(
        upload(&app, &admin, &root, "b.txt", "0123456789")
            .await
            .status_code(),
        507
    );
    let drives = drives_of(&app, &admin).await;
    assert_eq!(drives["Small"]["used"], 10);
    assert_eq!(drives["My drive"]["used"], 10);
    assert_eq!(drives["My drive"]["limit"], Value::Null);
    // The superadmin raises it.
    let changed = request(
        &app,
        Method::PATCH,
        &format!("/api/superadmin/drives/{drive}"),
        Some(&admin),
        Some(json!({ "storage_limit": null })),
    )
    .await;
    assert_eq!(changed.status_code(), 200, "{}", changed.text());
    assert_eq!(
        upload(&app, &admin, &root, "b.txt", "0123456789")
            .await
            .status_code(),
        201
    );
    // A user's limit is their My drive's.
    let me = session_of(&app, Some(&admin)).await["user"]["id"]
        .as_str()
        .unwrap()
        .to_string();
    assert_eq!(
        patch_user(&app, &admin, &me, json!({ "storage_limit": 15 }))
            .await
            .status_code(),
        200
    );
    assert_eq!(drives_of(&app, &admin).await["My drive"]["limit"], 15);
    let my_root = root_id(&app, &admin).await;
    assert_eq!(
        upload(&app, &admin, &my_root, "c.txt", "0123456789")
            .await
            .status_code(),
        507
    );
}

/// Organizer's own folder (`.organizer`, the trash and the versions) is in
/// the drive's folder, and hidden everywhere: the listings, the index,
/// search, the change feed, WebDAV; no name may take it at the root.
#[tokio::test]
async fn the_organizer_folder_is_hidden_everywhere() {
    let app = spawn_app().await;
    let (admin, share) =
        with_files(&app, &[("keep.txt", "keep"), ("old.txt", "trashed words")]).await;
    let old = id_at(&app, &admin, "old.txt").await;
    assert_eq!(delete_node(&app, &admin, &old).await.status_code(), 200);
    let keep = id_at(&app, &admin, "keep.txt").await;
    replace(&app, &admin, &keep, "kept again").await;
    // On disk, in the drive.
    assert!(share.0.join(".organizer/trash").join(&old).is_file());
    assert_eq!(
        std::fs::read_dir(share.0.join(".organizer/versions"))
            .unwrap()
            .count(),
        1
    );
    let cursor =
        json(request(&app, Method::GET, "/api/changes", Some(&admin), None).await).await["cursor"]
            .as_str()
            .unwrap()
            .to_string();
    // A scan finds nothing new there.
    let report = app
        .state
        .files
        .scan(user_id(&app, &admin).await)
        .await
        .unwrap();
    assert_eq!(report, Default::default());
    let root = root_id(&app, &admin).await;
    assert_eq!(names_in(&app, &admin, &root).await, ["keep.txt"]);
    assert_eq!(by_path(&app, &admin, ".organizer").await.status_code(), 404);
    let indexed: i64 = sqlx::query_scalar("SELECT count(*) FROM nodes WHERE name = '.organizer'")
        .fetch_one(app.state.db.reader())
        .await
        .unwrap();
    assert_eq!(indexed, 0);
    // No name takes it at the root; below, it is a name like any other.
    let made = post(
        &app,
        &format!("/api/files/{root}/folders"),
        &admin,
        Some(json!({ "name": ".Organizer" })),
    )
    .await;
    assert_eq!(made.status_code(), 400);
    // Something new in the tree reaches the feed; the trash does not.
    std::fs::write(share.0.join(".organizer/stray.txt"), "x").unwrap();
    std::fs::write(share.0.join("new.txt"), "x").unwrap();
    app.state
        .files
        .scan(user_id(&app, &admin).await)
        .await
        .unwrap();
    let page = json(
        request(
            &app,
            Method::GET,
            &format!("/api/changes?cursor={cursor}"),
            Some(&admin),
            None,
        )
        .await,
    )
    .await;
    let names: Vec<_> = page["changes"]
        .as_array()
        .unwrap()
        .iter()
        .map(|c| c["name"].clone())
        .collect();
    assert_eq!(names, [json!("new.txt")], "{page}");
    // Search: the trashed file's words are not found.
    let hits = found(&app, &admin, "new", |hits| {
        !hits.as_array().unwrap().is_empty()
    })
    .await;
    assert_eq!(hits[0]["name"], "new.txt");
    assert_eq!(search_for(&app, &admin, "trashed").await, json!([]));
    assert_eq!(search_for(&app, &admin, "stray").await, json!([]));
    // WebDAV: not listed, and nothing reaches into it.
    let body = json!({ "name": "rclone", "scope": "write", "kind": "app_password" });
    let password = json(post(&app, "/api/auth/api-keys", &admin, Some(body)).await).await["token"]
        .as_str()
        .unwrap()
        .to_string();
    let auth = basic(&password);
    let listing = app
        .server
        .method(Method::from_bytes(b"PROPFIND").unwrap(), "/dav/")
        .add_header("authorization", &auth)
        .add_header("depth", "1")
        .await;
    assert_eq!(listing.status_code(), 207);
    assert!(listing.text().contains("keep.txt"));
    assert!(!listing.text().contains(".organizer"), "{}", listing.text());
    for path in ["/dav/.organizer/", "/dav/.organizer/stray.txt"] {
        let response = app
            .server
            .get(path)
            .add_header("authorization", &auth)
            .await;
        assert_eq!(response.status_code(), 404, "{path}");
    }
    let put = app
        .server
        .put("/dav/.organizer/evil.txt")
        .add_header("authorization", &auth)
        .bytes("x".into())
        .await;
    assert_eq!(put.status_code(), 404);
}

// ---------------------------------------------------------------------------
// Roles, teams and settings levels (docs/M7B_DESIGN.md)
// ---------------------------------------------------------------------------

/// A shared drive made by the superadmin, managed by `manager`'s email:
/// its id and root.
async fn shared_drive(app: &TestApp, admin: &str, name: &str, manager: &str) -> (String, String) {
    let made = post(
        app,
        "/api/superadmin/drives",
        admin,
        Some(json!({ "name": name, "members": [{ "email": manager, "manages": true }] })),
    )
    .await;
    assert_eq!(made.status_code(), 201, "{}", made.text());
    let made = json(made).await;
    (
        made["drive"]["id"].as_str().unwrap().to_string(),
        made["drive"]["root_id"].as_str().unwrap().to_string(),
    )
}

/// Give a user a role on a drive: the grant's id.
async fn grant_drive(app: &TestApp, session: &str, drive: &str, user: &str, role: &str) -> String {
    let response = post(
        app,
        &format!("/api/drives/{drive}/access"),
        session,
        Some(json!({ "user_id": user, "role": role })),
    )
    .await;
    assert_eq!(response.status_code(), 201, "{}", response.text());
    json(response).await["id"].as_str().unwrap().to_string()
}

/// Give a user a role on a file or folder: the response.
async fn grant_item(app: &TestApp, session: &str, node: &str, user: &str, role: &str) -> Response {
    post(
        app,
        &format!("/api/files/{node}/access"),
        session,
        Some(json!({ "user_id": user, "role": role })),
    )
    .await
}

/// A folder's children by name, with each one's `my_role`.
async fn roles_in(app: &TestApp, session: &str, folder: &str) -> Vec<(String, String)> {
    let response = request(
        app,
        Method::GET,
        &format!("/api/files/{folder}/children"),
        Some(session),
        None,
    )
    .await;
    assert_eq!(response.status_code(), 200, "{}", response.text());
    json(response).await["items"]
        .as_array()
        .unwrap()
        .iter()
        .map(|n| {
            (
                n["name"].as_str().unwrap().to_string(),
                n["my_role"].as_str().unwrap().to_string(),
            )
        })
        .collect()
}

/// Make a folder: the response.
async fn new_folder(app: &TestApp, session: &str, parent: &str, name: &str) -> Response {
    post(
        app,
        &format!("/api/files/{parent}/folders"),
        session,
        Some(json!({ "name": name })),
    )
    .await
}

/// Each role on a shared drive allows what §1's table says and refuses the
/// rest; every entry carries `my_role`; a shared drive never loses its
/// last manager.
#[tokio::test]
async fn each_role_allows_its_operations_and_nothing_more() {
    let app = spawn_app().await;
    let admin = admin_session(&app).await;
    let (alice, _) = invited_user(&app, &admin, "alice@example.com").await;
    let (bob, bob_id) = invited_user(&app, &admin, "bob@example.com").await;
    let (carol, carol_id) = invited_user(&app, &admin, "carol@example.com").await;
    let (dave, dave_id) = invited_user(&app, &admin, "dave@example.com").await;
    let (drive, root) = shared_drive(&app, &admin, "Team", "alice@example.com").await;
    let alices = json(upload(&app, &alice, &root, "plan.txt", "plan").await).await;
    assert_eq!(alices["my_role"], "manager");
    let plan = alices["id"].as_str().unwrap().to_string();
    grant_drive(&app, &alice, &drive, &bob_id, "viewer").await;
    grant_drive(&app, &alice, &drive, &carol_id, "editor").await;
    grant_drive(&app, &alice, &drive, &dave_id, "upload_only").await;
    assert_eq!(drives_of(&app, &bob).await["Team"]["my_role"], "viewer");
    assert_eq!(
        drives_of(&app, &dave).await["Team"]["my_role"],
        "upload_only"
    );

    // Viewer: reads and downloads, nothing else.
    assert_eq!(
        roles_in(&app, &bob, &root).await,
        [("plan.txt".into(), "viewer".into())]
    );
    let response = request(
        &app,
        Method::GET,
        &format!("/api/files/{plan}/content"),
        Some(&bob),
        None,
    )
    .await;
    assert_eq!(response.text(), "plan");
    assert_eq!(
        upload(&app, &bob, &root, "b.txt", "b").await.status_code(),
        403
    );
    assert_eq!(new_folder(&app, &bob, &root, "B").await.status_code(), 403);
    assert_eq!(rename(&app, &bob, &plan, "x.txt").await.status_code(), 403);
    assert_eq!(replace_as(&app, &bob, &plan, "x").await.status_code(), 403);
    assert_eq!(delete_node(&app, &bob, &plan).await.status_code(), 403);
    assert_eq!(
        make_link(&app, &bob, &plan, json!({ "kind": "view" }))
            .await
            .status_code(),
        403
    );
    assert_eq!(
        grant_item(&app, &bob, &plan, &dave_id, "viewer")
            .await
            .status_code(),
        403
    );

    // Editor: writes, renames, deletes to the trash, makes links; not who
    // has access, not deleting for good.
    let notes = json(upload(&app, &carol, &root, "notes.txt", "n").await).await;
    assert_eq!(notes["my_role"], "editor");
    let notes = notes["id"].as_str().unwrap().to_string();
    assert_eq!(
        new_folder(&app, &carol, &root, "C").await.status_code(),
        201
    );
    assert_eq!(
        rename(&app, &carol, &notes, "Notes.txt")
            .await
            .status_code(),
        200
    );
    assert_eq!(
        replace_as(&app, &carol, &notes, "more").await.status_code(),
        200
    );
    assert_eq!(
        make_link(&app, &carol, &plan, json!({ "kind": "view" }))
            .await
            .status_code(),
        201
    );
    assert_eq!(
        grant_item(&app, &carol, &plan, &dave_id, "viewer")
            .await
            .status_code(),
        403
    );
    assert_eq!(delete_node(&app, &carol, &notes).await.status_code(), 200);
    let purge = request(
        &app,
        Method::DELETE,
        &format!("/api/trash/{notes}"),
        Some(&carol),
        None,
    )
    .await;
    assert_eq!(purge.status_code(), 403, "deleting for good is a manager's");
    let restored = post(&app, &format!("/api/trash/{notes}/restore"), &carol, None).await;
    assert_eq!(restored.status_code(), 200);
    assert_eq!(json(restored).await["my_role"], "editor");

    // Upload only: adds, and sees only what they added.
    assert_eq!(
        roles_in(&app, &dave, &root).await,
        Vec::<(String, String)>::new()
    );
    let mine = json(upload(&app, &dave, &root, "dave.txt", "d").await).await;
    assert_eq!(mine["my_role"], "upload_only");
    let mine = mine["id"].as_str().unwrap().to_string();
    assert_eq!(
        roles_in(&app, &dave, &root).await,
        [("dave.txt".into(), "upload_only".into())]
    );
    assert_eq!(
        status_of(&app, &dave, &format!("/api/files/{plan}")).await,
        404
    );
    assert_eq!(
        rename(&app, &dave, &mine, "d2.txt").await.status_code(),
        403
    );
    assert_eq!(delete_node(&app, &dave, &mine).await.status_code(), 403);
    // The others see it.
    assert!(
        names_in(&app, &bob, &root)
            .await
            .contains(&"dave.txt".to_string())
    );

    // Manager: who has access, the drive's settings, deleting for good.
    assert_eq!(
        grant_item(&app, &alice, &plan, &dave_id, "viewer")
            .await
            .status_code(),
        201
    );
    let access = json(
        request(
            &app,
            Method::GET,
            &format!("/api/files/{plan}/access"),
            Some(&alice),
            None,
        )
        .await,
    )
    .await;
    assert_eq!(access["my_role"], "manager");
    assert_eq!(access["may_manage"], true);
    assert_eq!(access["grants"][0]["role"], "viewer");
    assert!(access["owner"].is_null(), "a shared drive has no owner");
    assert!(
        access["inherited"]
            .as_array()
            .unwrap()
            .iter()
            .any(|g| g["role"] == "editor" && g["from_id"].is_null())
    );
    assert_eq!(delete_node(&app, &alice, &notes).await.status_code(), 200);
    let purge = request(
        &app,
        Method::DELETE,
        &format!("/api/trash/{notes}"),
        Some(&alice),
        None,
    )
    .await;
    assert_eq!(purge.status_code(), 204);
    // No owner role is given, and no more than the giver has.
    let response = post(
        &app,
        &format!("/api/drives/{drive}/access"),
        &alice,
        Some(json!({ "email": "admin@example.com", "role": "owner" })),
    )
    .await;
    assert_eq!(response.status_code(), 403);

    // The last manager stays one.
    let grants = json(
        request(
            &app,
            Method::GET,
            &format!("/api/drives/{drive}/access"),
            Some(&alice),
            None,
        )
        .await,
    )
    .await;
    let hers = grants["grants"]
        .as_array()
        .unwrap()
        .iter()
        .find(|g| g["user"]["email"] == "alice@example.com")
        .unwrap()["id"]
        .as_str()
        .unwrap()
        .to_string();
    let response = request(
        &app,
        Method::PATCH,
        &format!("/api/drives/{drive}/access/{hers}"),
        Some(&alice),
        Some(json!({ "role": "editor" })),
    )
    .await;
    assert_eq!(response.status_code(), 409);
    let response = request(
        &app,
        Method::DELETE,
        &format!("/api/drives/{drive}/access/{hers}"),
        Some(&alice),
        None,
    )
    .await;
    assert_eq!(response.status_code(), 409);
    // With a second manager she may go.
    let carols = grant_drive_role_of(&app, &alice, &drive, "carol@example.com").await;
    let response = request(
        &app,
        Method::PATCH,
        &format!("/api/drives/{drive}/access/{carols}"),
        Some(&alice),
        Some(json!({ "role": "manager" })),
    )
    .await;
    assert_eq!(response.status_code(), 200);
    let response = request(
        &app,
        Method::DELETE,
        &format!("/api/drives/{drive}/access/{hers}"),
        Some(&alice),
        None,
    )
    .await;
    assert_eq!(response.status_code(), 204);
    assert_eq!(
        status_of(&app, &alice, &format!("/api/files/{root}")).await,
        404
    );
}

/// The id of a user's own grant on a drive.
async fn grant_drive_role_of(app: &TestApp, session: &str, drive: &str, email: &str) -> String {
    let grants = json(
        request(
            app,
            Method::GET,
            &format!("/api/drives/{drive}/access"),
            Some(session),
            None,
        )
        .await,
    )
    .await;
    grants["grants"]
        .as_array()
        .unwrap()
        .iter()
        .find(|g| g["user"]["email"] == email)
        .unwrap()["id"]
        .as_str()
        .unwrap()
        .to_string()
}

/// A grant reaches everything below; a subfolder widens (a higher role
/// there); limited access hides a folder from everyone it does not grant,
/// in the listing, search, the trash, the versions and the change feed,
/// while the drive's managers keep it; a grant on it lets one in again.
#[tokio::test]
async fn inheritance_widening_and_limited_access() {
    let app = spawn_app().await;
    let admin = admin_session(&app).await;
    let (alice, _) = invited_user(&app, &admin, "alice@example.com").await;
    let (bob, bob_id) = invited_user(&app, &admin, "bob@example.com").await;
    let (drive, root) = shared_drive(&app, &admin, "Team", "alice@example.com").await;
    let docs = json(new_folder(&app, &alice, &root, "Docs").await).await["id"]
        .as_str()
        .unwrap()
        .to_string();
    let sub = json(new_folder(&app, &alice, &docs, "Sub").await).await["id"]
        .as_str()
        .unwrap()
        .to_string();
    let secret = json(new_folder(&app, &alice, &root, "Secret").await).await["id"]
        .as_str()
        .unwrap()
        .to_string();
    let hidden = json(upload(&app, &alice, &secret, "zebrafish.txt", "zebrafish plans").await)
        .await["id"]
        .as_str()
        .unwrap()
        .to_string();
    replace_as(&app, &alice, &hidden, "zebrafish plans, again").await;
    grant_drive(&app, &alice, &drive, &bob_id, "viewer").await;

    // Inherited: a viewer under Docs too; widened to editor on Sub.
    assert_eq!(
        roles_in(&app, &bob, &root).await,
        [
            ("Docs".into(), "viewer".into()),
            ("Secret".into(), "viewer".into())
        ]
    );
    assert_eq!(
        roles_in(&app, &bob, &docs).await,
        [("Sub".into(), "viewer".into())]
    );
    assert_eq!(
        grant_item(&app, &alice, &sub, &bob_id, "editor")
            .await
            .status_code(),
        201
    );
    assert_eq!(
        roles_in(&app, &bob, &docs).await,
        [("Sub".into(), "editor".into())]
    );
    assert_eq!(
        upload(&app, &bob, &sub, "s.txt", "s").await.status_code(),
        201
    );
    assert_eq!(
        upload(&app, &bob, &docs, "d.txt", "d").await.status_code(),
        403
    );

    // The change feed so far, then limited access on Secret.
    let cursor =
        json(request(&app, Method::GET, "/api/changes", Some(&bob), None).await).await["cursor"]
            .as_str()
            .unwrap()
            .to_string();
    found(&app, &bob, "zebrafish", |h| {
        h.as_array().unwrap().len() == 1
    })
    .await;
    let response = request(
        &app,
        Method::PATCH,
        &format!("/api/files/{secret}/access"),
        Some(&alice),
        Some(json!({ "limited": true })),
    )
    .await;
    assert_eq!(response.status_code(), 200, "{}", response.text());
    assert_eq!(json(response).await["limited"], true);
    let feed = json(
        request(
            &app,
            Method::GET,
            &format!("/api/changes?cursor={cursor}"),
            Some(&bob),
            None,
        )
        .await,
    )
    .await;
    let rows = feed["changes"].as_array().unwrap();
    assert!(
        rows.iter()
            .any(|c| c["node_id"] == json!(secret) && c["op"] == "delete"),
        "{feed}"
    );
    let cursor = feed["cursor"].as_str().unwrap().to_string();
    // Hidden from bob everywhere: the listing, by id, its file, search,
    // versions; alice (a manager of the drive) keeps it all.
    assert_eq!(
        roles_in(&app, &bob, &root).await,
        [("Docs".into(), "viewer".into())]
    );
    assert_eq!(
        status_of(&app, &bob, &format!("/api/files/{secret}")).await,
        404
    );
    assert_eq!(
        status_of(&app, &bob, &format!("/api/files/{hidden}/content")).await,
        404
    );
    assert_eq!(
        status_of(&app, &bob, &format!("/api/files/{hidden}/versions")).await,
        404
    );
    assert_eq!(
        search_for(&app, &bob, "zebrafish").await,
        json!([]),
        "not in bob's search"
    );
    assert_eq!(
        search_for(&app, &alice, "zebrafish")
            .await
            .as_array()
            .unwrap()
            .len(),
        1
    );
    assert_eq!(
        roles_in(&app, &alice, &root).await,
        [
            ("Docs".into(), "manager".into()),
            ("Secret".into(), "manager".into())
        ]
    );
    // A change inside it is not in bob's feed.
    upload(&app, &alice, &secret, "more.txt", "m").await;
    let feed = json(
        request(
            &app,
            Method::GET,
            &format!("/api/changes?cursor={cursor}"),
            Some(&bob),
            None,
        )
        .await,
    )
    .await;
    assert_eq!(feed["changes"], json!([]), "{feed}");
    // Its trash: what was deleted in it is not in bob's view of the
    // drive's trash.
    delete_node(&app, &alice, &hidden).await;
    let trash = |who: &str| {
        let (app, who, drive) = (&app, who.to_string(), drive.clone());
        async move {
            json(
                request(
                    app,
                    Method::GET,
                    &format!("/api/trash?drive={drive}"),
                    Some(&who),
                    None,
                )
                .await,
            )
            .await
            .as_array()
            .unwrap()
            .len()
        }
    };
    assert_eq!(trash(&alice).await, 1);
    assert_eq!(trash(&bob).await, 0);
    assert_eq!(
        post(&app, &format!("/api/trash/{hidden}/restore"), &alice, None)
            .await
            .status_code(),
        200
    );

    // A grant on it lets bob in again: it appears in his feed.
    assert_eq!(
        grant_item(&app, &alice, &secret, &bob_id, "viewer")
            .await
            .status_code(),
        201
    );
    assert_eq!(
        roles_in(&app, &bob, &root).await,
        [
            ("Docs".into(), "viewer".into()),
            ("Secret".into(), "viewer".into())
        ]
    );
    let feed = json(
        request(
            &app,
            Method::GET,
            &format!("/api/changes?cursor={cursor}"),
            Some(&bob),
            None,
        )
        .await,
    )
    .await;
    assert!(
        feed["changes"]
            .as_array()
            .unwrap()
            .iter()
            .any(|c| c["node_id"] == json!(secret) && c["op"] == "create"),
        "{feed}"
    );
    // A restored item keeps being limited: limited again, then restored.
    let limited_sub = json(new_folder(&app, &alice, &secret, "Inner").await).await["id"]
        .as_str()
        .unwrap()
        .to_string();
    request(
        &app,
        Method::PATCH,
        &format!("/api/files/{limited_sub}/access"),
        Some(&alice),
        Some(json!({ "limited": true })),
    )
    .await;
    assert_eq!(
        status_of(&app, &bob, &format!("/api/files/{limited_sub}")).await,
        404
    );
    delete_node(&app, &alice, &secret).await;
    post(&app, &format!("/api/trash/{secret}/restore"), &alice, None).await;
    let inner = json(
        request(
            &app,
            Method::GET,
            &format!("/api/files/{secret}/children"),
            Some(&alice),
            None,
        )
        .await,
    )
    .await["items"]
        .as_array()
        .unwrap()
        .iter()
        .find(|n| n["name"] == "Inner")
        .unwrap()["id"]
        .as_str()
        .unwrap()
        .to_string();
    let access = json(
        request(
            &app,
            Method::GET,
            &format!("/api/files/{inner}/access"),
            Some(&alice),
            None,
        )
        .await,
    )
    .await;
    assert_eq!(access["limited"], true, "limited after a restore");
}

/// Upload only on a folder of My drive, placed by its recipient: they see
/// the folder and what they added, nothing else, in the listing, search,
/// the change feed and WebDAV.
#[tokio::test]
async fn upload_only_sees_only_its_own_uploads() {
    let app = spawn_app().await;
    let (admin, _share) = with_files(&app, &[("Inbox/theirs.txt", "quokka secret")]).await;
    let (bob, bob_id) = invited_user(&app, &admin, "bob@example.com").await;
    let inbox = id_at(&app, &admin, "/Inbox").await;
    let theirs = id_at(&app, &admin, "/Inbox/theirs.txt").await;
    assert_eq!(
        grant_item(&app, &admin, &inbox, &bob_id, "upload_only")
            .await
            .status_code(),
        201
    );
    let shared = json(request(&app, Method::GET, "/api/shared", Some(&bob), None).await).await;
    assert_eq!(shared[0]["role"], "upload_only");
    assert_eq!(shared[0]["node"]["my_role"], "upload_only");
    assert_eq!(names_in(&app, &bob, &inbox).await, Vec::<String>::new());
    assert_eq!(
        status_of(&app, &bob, &format!("/api/files/{theirs}")).await,
        404
    );
    let mine = json(upload(&app, &bob, &inbox, "mine.txt", "m").await).await;
    assert_eq!(mine["my_role"], "upload_only");
    assert_eq!(names_in(&app, &bob, &inbox).await, ["mine.txt"]);
    assert_eq!(
        names_in(&app, &admin, &inbox).await,
        ["mine.txt", "theirs.txt"]
    );
    found(&app, &admin, "quokka", |h| h.as_array().unwrap().len() == 1).await;
    assert_eq!(search_for(&app, &bob, "quokka").await, json!([]));
    // Who has access is not shown to them.
    assert_eq!(
        status_of(&app, &bob, &format!("/api/files/{inbox}/access")).await,
        403
    );
}

/// Handing over: the new owner owns it, the previous owner manages it, it
/// stays where it is; only the owner hands over.
#[tokio::test]
async fn changing_the_owner_keeps_the_previous_one_as_manager() {
    let app = spawn_app().await;
    let (admin, share) = with_files(&app, &[("Project/plan.txt", "plan")]).await;
    let (bob, bob_id) = invited_user(&app, &admin, "bob@example.com").await;
    let admin_id = session_of(&app, Some(&admin)).await["user"]["id"]
        .as_str()
        .unwrap()
        .to_string();
    let project = id_at(&app, &admin, "/Project").await;
    let plan = id_at(&app, &admin, "/Project/plan.txt").await;
    // Bob, who has nothing there, cannot; the owner can.
    let response = request(
        &app,
        Method::PATCH,
        &format!("/api/files/{project}/owner"),
        Some(&bob),
        Some(json!({ "user_id": bob_id })),
    )
    .await;
    assert_eq!(response.status_code(), 404);
    let response = request(
        &app,
        Method::PATCH,
        &format!("/api/files/{project}/owner"),
        Some(&admin),
        Some(json!({ "email": "bob@example.com" })),
    )
    .await;
    assert_eq!(response.status_code(), 200, "{}", response.text());
    assert_eq!(json(response).await["my_role"], "manager");
    assert!(
        share.0.join("Project/plan.txt").exists(),
        "it stays where it is"
    );
    let entry = json(
        request(
            &app,
            Method::GET,
            &format!("/api/files/{plan}"),
            Some(&bob),
            None,
        )
        .await,
    )
    .await;
    assert_eq!(entry["my_role"], "owner");
    let access = json(
        request(
            &app,
            Method::GET,
            &format!("/api/files/{project}/access"),
            Some(&bob),
            None,
        )
        .await,
    )
    .await;
    assert_eq!(access["owner"]["email"], "bob@example.com");
    assert!(
        access["grants"]
            .as_array()
            .unwrap()
            .iter()
            .any(|g| g["user"]["email"] == "admin@example.com" && g["role"] == "manager")
    );
    // The previous owner, a manager now, does not hand over again.
    let response = request(
        &app,
        Method::PATCH,
        &format!("/api/files/{project}/owner"),
        Some(&admin),
        Some(json!({ "user_id": bob_id })),
    )
    .await;
    assert_eq!(response.status_code(), 403);
    // Bob hands it back.
    let response = request(
        &app,
        Method::PATCH,
        &format!("/api/files/{project}/owner"),
        Some(&bob),
        Some(json!({ "user_id": admin_id })),
    )
    .await;
    assert_eq!(response.status_code(), 200, "{}", response.text());
    let entry = json(
        request(
            &app,
            Method::GET,
            &format!("/api/files/{project}"),
            Some(&admin),
            None,
        )
        .await,
    )
    .await;
    assert_eq!(entry["my_role"], "owner");
    let entry = json(
        request(
            &app,
            Method::GET,
            &format!("/api/files/{project}"),
            Some(&bob),
            None,
        )
        .await,
    )
    .await;
    assert_eq!(entry["my_role"], "manager");
}

/// Teams: the superadmin makes one and names its managers; its managers
/// add members and make its drives (whose managers they are, whose members
/// are editors); the team's limit holds for all its drives together; its
/// settings bind its drives; the old groups endpoints still answer.
#[tokio::test]
async fn teams_and_their_drives() {
    let app = spawn_app().await;
    let admin = admin_session(&app).await;
    let (alice, alice_id) = invited_user(&app, &admin, "alice@example.com").await;
    let (bob, bob_id) = invited_user(&app, &admin, "bob@example.com").await;
    let made = post(
        &app,
        "/api/superadmin/teams",
        &admin,
        Some(json!({
            "name": "Research",
            "managers": ["alice@example.com"],
            "storage_limit": 10,
        })),
    )
    .await;
    assert_eq!(made.status_code(), 201, "{}", made.text());
    let team = json(made).await["id"].as_str().unwrap().to_string();
    // Alice manages it: she adds bob; bob, a member, does not add.
    let added = post(
        &app,
        &format!("/api/teams/{team}/members"),
        &alice,
        Some(json!({ "email": "bob@example.com" })),
    )
    .await;
    assert_eq!(added.status_code(), 200, "{}", added.text());
    let refused = post(
        &app,
        &format!("/api/teams/{team}/members"),
        &bob,
        Some(json!({ "email": "admin@example.com" })),
    )
    .await;
    assert_eq!(refused.status_code(), 403);
    let mine = json(request(&app, Method::GET, "/api/teams", Some(&bob), None).await).await;
    assert_eq!(mine[0]["name"], "Research");
    assert_eq!(mine[0]["my_role"], "member");
    assert_eq!(mine[0]["primary"], true);

    // Alice makes the team's drive: she manages it, bob edits.
    let drive = post(
        &app,
        &format!("/api/teams/{team}/drives"),
        &alice,
        Some(json!({ "name": "Lab" })),
    )
    .await;
    assert_eq!(drive.status_code(), 201, "{}", drive.text());
    let drive = json(drive).await;
    assert_eq!(drive["my_role"], "manager");
    assert_eq!(drive["team_name"], "Research");
    let root = drive["root_id"].as_str().unwrap().to_string();
    let drive_id = drive["id"].as_str().unwrap().to_string();
    assert_eq!(drives_of(&app, &bob).await["Lab"]["my_role"], "editor");
    // Bob cannot make one.
    let refused = post(
        &app,
        &format!("/api/teams/{team}/drives"),
        &bob,
        Some(json!({ "name": "Mine" })),
    )
    .await;
    assert_eq!(refused.status_code(), 403);
    // Nor anyone outside a team, by default.
    let refused = post(&app, "/api/drives", &bob, Some(json!({ "name": "Mine" }))).await;
    assert_eq!(refused.status_code(), 403);

    // The team's limit: 10 bytes for all its drives together.
    assert_eq!(
        upload(&app, &bob, &root, "a.txt", "123456")
            .await
            .status_code(),
        201
    );
    assert_eq!(
        upload(&app, &bob, &root, "b.txt", "123456")
            .await
            .status_code(),
        507
    );
    let team_view = json(
        request(
            &app,
            Method::GET,
            &format!("/api/teams/{team}"),
            Some(&alice),
            None,
        )
        .await,
    )
    .await;
    assert_eq!(team_view["used"], 6);
    assert_eq!(team_view["drives"][0]["name"], "Lab");

    // Its settings bind its drives: links off, whoever works there.
    let locked = request(
        &app,
        Method::PUT,
        &format!("/api/teams/{team}/settings/links.allowed"),
        Some(&alice),
        Some(json!({ "mode": "locked", "value": false })),
    )
    .await;
    assert_eq!(locked.status_code(), 200, "{}", locked.text());
    let a = names_in(&app, &bob, &root).await;
    assert_eq!(a, ["a.txt"]);
    let file = json(
        request(
            &app,
            Method::GET,
            &format!("/api/files/{root}/children"),
            Some(&bob),
            None,
        )
        .await,
    )
    .await["items"][0]["id"]
        .as_str()
        .unwrap()
        .to_string();
    assert_eq!(
        make_link(&app, &bob, &file, json!({ "kind": "view" }))
            .await
            .status_code(),
        403
    );
    // The drive's managers cannot loosen it below the team.
    let response = request(
        &app,
        Method::PUT,
        &format!("/api/drives/{drive_id}/settings/links.allowed"),
        Some(&alice),
        Some(json!({ "mode": "default", "value": true })),
    )
    .await;
    assert_eq!(response.status_code(), 403);
    // The superadmin locks what the team may not change.
    let response = request(
        &app,
        Method::PUT,
        "/api/superadmin/settings/retention.trash_days",
        Some(&admin),
        Some(json!({ "mode": "locked", "value": 14 })),
    )
    .await;
    assert_eq!(response.status_code(), 200, "{}", response.text());
    let response = request(
        &app,
        Method::PUT,
        &format!("/api/teams/{team}/settings/retention.trash_days"),
        Some(&alice),
        Some(json!({ "mode": "default", "value": 3 })),
    )
    .await;
    assert_eq!(response.status_code(), 403);
    // Who may make shared drives: the superadmin only, now.
    let response = request(
        &app,
        Method::PUT,
        "/api/superadmin/settings/drives.create",
        Some(&admin),
        Some(json!({ "mode": "locked", "value": "superadmin" })),
    )
    .await;
    assert_eq!(response.status_code(), 200);
    let refused = post(
        &app,
        &format!("/api/teams/{team}/drives"),
        &alice,
        Some(json!({ "name": "Lab 2" })),
    )
    .await;
    assert_eq!(refused.status_code(), 403);

    // A team drive keeps a manager: its only one cannot leave the team.
    let response = request(
        &app,
        Method::DELETE,
        &format!("/api/teams/{team}/members/{alice_id}"),
        Some(&alice),
        None,
    )
    .await;
    assert_eq!(response.status_code(), 409);
    // Bob leaves: the drive goes from his drives.
    let response = request(
        &app,
        Method::DELETE,
        &format!("/api/teams/{team}/members/{bob_id}"),
        Some(&bob),
        None,
    )
    .await;
    assert_eq!(response.status_code(), 204);
    assert!(!drives_of(&app, &bob).await.contains_key("Lab"));

    // The old groups endpoints answer, over the teams.
    let groups = json(
        request(
            &app,
            Method::GET,
            "/api/superadmin/groups",
            Some(&admin),
            None,
        )
        .await,
    )
    .await;
    assert_eq!(groups[0]["name"], "Research");
    let made = post(
        &app,
        "/api/superadmin/groups",
        &admin,
        Some(json!({ "name": "Friends", "members": [bob_id] })),
    )
    .await;
    assert_eq!(made.status_code(), 201);
    let teams = json(
        request(
            &app,
            Method::GET,
            "/api/superadmin/teams",
            Some(&admin),
            None,
        )
        .await,
    )
    .await;
    let friends = teams
        .as_array()
        .unwrap()
        .iter()
        .find(|t| t["name"] == "Friends")
        .unwrap()
        .clone();
    assert_eq!(friends["members"][0]["email"], "bob@example.com");
    assert_eq!(friends["members"][0]["role"], "member");
    let gone = request(
        &app,
        Method::DELETE,
        &format!("/api/superadmin/groups/{}", friends["id"].as_str().unwrap()),
        Some(&admin),
        None,
    )
    .await;
    assert_eq!(gone.status_code(), 204);
}

/// The settings a user sees: each with its value, where it comes from and
/// whether they may change it; a lock above refuses a change, a range
/// holds it; a team's two-factor requirement reaches its members.
#[tokio::test]
async fn settings_levels_through_the_api() {
    let app = spawn_app().await;
    let admin = admin_session(&app).await;
    let (bob, _) = invited_user(&app, &admin, "bob@example.com").await;
    let setting = |key: &'static str| {
        let (app, bob) = (&app, bob.clone());
        async move {
            let all =
                json(request(app, Method::GET, "/api/settings", Some(&bob), None).await).await;
            all.as_array()
                .unwrap()
                .iter()
                .find(|s| s["key"] == key)
                .unwrap()
                .clone()
        }
    };
    let pending = setting("drop_box.pending_mode").await;
    assert_eq!(pending["value"], "move_pending");
    assert_eq!(pending["source"]["level"], "builtin");
    assert_eq!(pending["may_change"], true);
    let response = request(
        &app,
        Method::PUT,
        "/api/settings/drop_box.pending_mode",
        Some(&bob),
        Some(json!({ "value": "stay" })),
    )
    .await;
    assert_eq!(response.status_code(), 200, "{}", response.text());
    assert_eq!(json(response).await["source"]["level"], "user");
    // The drop box's own endpoint reads the same.
    let dropbox = json(
        request(
            &app,
            Method::GET,
            "/api/drop-box/settings",
            Some(&bob),
            None,
        )
        .await,
    )
    .await;
    assert_eq!(dropbox["pending_mode"], "stay");

    // The superadmin locks it: bob's value no longer counts, nor can he
    // change it.
    let response = request(
        &app,
        Method::PUT,
        "/api/superadmin/settings/drop_box.pending_mode",
        Some(&admin),
        Some(json!({ "mode": "locked", "value": "timeout" })),
    )
    .await;
    assert_eq!(response.status_code(), 200, "{}", response.text());
    let pending = setting("drop_box.pending_mode").await;
    assert_eq!(pending["value"], "timeout");
    assert_eq!(pending["locked_by"]["level"], "superadmin");
    assert_eq!(pending["may_change"], false);
    let response = request(
        &app,
        Method::PUT,
        "/api/settings/drop_box.pending_mode",
        Some(&bob),
        Some(json!({ "value": "stay" })),
    )
    .await;
    assert_eq!(response.status_code(), 403);
    let response = request(
        &app,
        Method::PUT,
        "/api/drop-box/settings",
        Some(&bob),
        Some(json!({
            "pending_mode": "stay", "read_contents": "always", "folder_freedom": "balanced",
            "folders": "whole", "links": "snapshot",
        })),
    )
    .await;
    assert_eq!(response.status_code(), 403);

    // A range: at least 7 days in the trash.
    let response = request(
        &app,
        Method::PUT,
        "/api/superadmin/settings/retention.trash_days",
        Some(&admin),
        Some(json!({ "mode": "range", "min": 7 })),
    )
    .await;
    assert_eq!(response.status_code(), 200, "{}", response.text());
    let response = request(
        &app,
        Method::PUT,
        "/api/settings/retention.trash_days",
        Some(&bob),
        Some(json!({ "value": 3 })),
    )
    .await;
    assert_eq!(response.status_code(), 400);
    let response = request(
        &app,
        Method::PUT,
        "/api/settings/retention.trash_days",
        Some(&bob),
        Some(json!({ "value": 9 })),
    )
    .await;
    assert_eq!(response.status_code(), 200);
    let retention =
        json(request(&app, Method::GET, "/api/retention", Some(&bob), None).await).await;
    assert_eq!(retention["rules"]["trash_days"], 9);
    let levels = json(
        request(
            &app,
            Method::GET,
            "/api/superadmin/settings",
            Some(&admin),
            None,
        )
        .await,
    )
    .await;
    let trash = levels
        .as_array()
        .unwrap()
        .iter()
        .find(|s| s["key"] == "retention.trash_days")
        .unwrap()
        .clone();
    assert_eq!(trash["mode"], "range");
    assert_eq!(trash["min"], 7.0);

    // A team requires two-factor authentication: its members must set it
    // up.
    let made = post(
        &app,
        "/api/superadmin/teams",
        &admin,
        Some(json!({ "name": "Strict", "members": ["bob@example.com"] })),
    )
    .await;
    let team = json(made).await["id"].as_str().unwrap().to_string();
    assert_eq!(session_of(&app, Some(&bob)).await["totp_required"], false);
    sqlx::query(
        "INSERT INTO settings (level, level_id, key, value, mode)
         VALUES ('team', ?1, 'sign_in.two_factor_required', 'true', 'locked')",
    )
    .bind(uuid::Uuid::parse_str(&team).unwrap())
    .execute(app.state.db.writer())
    .await
    .unwrap();
    assert_eq!(session_of(&app, Some(&bob)).await["totp_required"], true);
    let response = password_login(&app, "bob@example.com").await;
    assert_eq!(response.status_code(), 200);
    assert_eq!(json(response).await["totp_setup_required"], true);
}

/// What the superadmin and the command line do to an account is in its
/// activity, which its user reads with a session.
#[tokio::test]
async fn the_account_activity_shows_what_was_done_to_it() {
    let app = spawn_app().await;
    let admin = admin_session(&app).await;
    let (bob, bob_id) = invited_user(&app, &admin, "bob@example.com").await;
    patch_user(&app, &admin, &bob_id, json!({ "storage_limit": 1000 })).await;
    let made = post(
        &app,
        "/api/superadmin/teams",
        &admin,
        Some(json!({ "name": "Ops", "members": ["bob@example.com"] })),
    )
    .await;
    assert_eq!(made.status_code(), 201);
    organizer::recovery::grant(&app.state.db, "bob@example.com")
        .await
        .unwrap();
    let activity =
        json(request(&app, Method::GET, "/api/me/activity", Some(&bob), None).await).await;
    let actions: Vec<(&str, &str)> = activity
        .as_array()
        .unwrap()
        .iter()
        .map(|e| {
            (
                e["action"].as_str().unwrap(),
                e["actor"]["kind"].as_str().unwrap(),
            )
        })
        .collect();
    assert_eq!(
        actions,
        [
            ("superadmin_granted", "command_line"),
            ("team_added", "superadmin"),
            ("storage_limit", "superadmin"),
            ("created", "superadmin"),
        ]
    );
    assert_eq!(activity[1]["detail"]["team"], "Ops");
    assert_eq!(activity[2]["detail"]["storage_limit"], 1000);
    assert_eq!(activity[2]["actor"]["email"], "admin@example.com");
    // Paged.
    let before = activity[1]["id"].as_str().unwrap();
    let older = json(
        request(
            &app,
            Method::GET,
            &format!("/api/me/activity?before={before}&limit=1"),
            Some(&bob),
            None,
        )
        .await,
    )
    .await;
    assert_eq!(older[0]["action"], "storage_limit");
    // Not with a token.
    let key = create_key(&app, &bob, "read").await;
    let response = with_key(
        &app,
        Method::GET,
        "/api/me/activity",
        key["token"].as_str().unwrap(),
        None,
    )
    .await;
    assert_eq!(response.status_code(), 403);
}

/// Limited access hides a folder from WebDAV too: a recipient walking
/// into the share they placed does not see it, nor reach into it.
#[tokio::test]
async fn webdav_hides_a_limited_folder() {
    let app = spawn_app().await;
    let (admin, _share) = with_files(
        &app,
        &[("Docs/a.txt", "alpha"), ("Docs/Private/p.txt", "private")],
    )
    .await;
    let (bob, bob_id) = invited_user(&app, &admin, "bob@example.com").await;
    let docs = id_at(&app, &admin, "/Docs").await;
    let private = id_at(&app, &admin, "/Docs/Private").await;
    assert_eq!(
        grant_item(&app, &admin, &docs, &bob_id, "editor")
            .await
            .status_code(),
        201
    );
    let bob_root = id_at(&app, &bob, "/").await;
    post(
        &app,
        &format!("/api/shared/{docs}/place"),
        &bob,
        Some(json!({ "parent_id": bob_root, "name": "Team" })),
    )
    .await;
    let response = request(
        &app,
        Method::PATCH,
        &format!("/api/files/{private}/access"),
        Some(&admin),
        Some(json!({ "limited": true })),
    )
    .await;
    assert_eq!(response.status_code(), 200);
    let body = json!({ "name": "dav", "scope": "write", "kind": "app_password" });
    let password = json(post(&app, "/api/auth/api-keys", &bob, Some(body)).await).await["token"]
        .as_str()
        .unwrap()
        .to_string();
    let auth = basic(&password);
    let dav = |method: &str, path: &str| {
        app.server
            .method(Method::from_bytes(method.as_bytes()).unwrap(), path)
            .add_header("authorization", &auth)
    };
    let response = dav("PROPFIND", "/dav/Team/").add_header("depth", "1").await;
    assert_eq!(response.status_code(), 207);
    assert!(response.text().contains("a.txt"), "{}", response.text());
    assert!(!response.text().contains("Private"), "{}", response.text());
    assert_eq!(
        dav("GET", "/dav/Team/Private/p.txt").await.status_code(),
        404
    );
    assert_eq!(
        dav("PROPFIND", "/dav/Team/Private/")
            .add_header("depth", "1")
            .await
            .status_code(),
        404
    );
    assert_eq!(names_in(&app, &bob, &docs).await, ["a.txt"]);
}

/// Every user lists the models they may choose for their work, by name
/// and id only, and chooses one of them.
#[tokio::test]
async fn users_list_the_ai_models_they_may_choose() {
    let app = spawn_app().await;
    let admin = admin_session(&app).await;
    let (bob, _) = invited_user(&app, &admin, "bob@example.com").await;
    let none = json(request(&app, Method::GET, "/api/ai/models", Some(&bob), None).await).await;
    assert_eq!(
        none,
        json!({ "proposals": [], "images": [], "scans": [], "audio": [] })
    );
    let server = model_server::spawn().await;
    let chat = add_model(&app, &admin, &server, "Writer", &["chat"]).await;
    let eyes = add_model(&app, &admin, &server, "Eyes", &["chat", "vision"]).await;
    let choices = json(request(&app, Method::GET, "/api/ai/models", Some(&bob), None).await).await;
    assert_eq!(
        choices["proposals"],
        json!([
            { "id": eyes["id"], "name": "Eyes" },
            { "id": chat["id"], "name": "Writer" },
        ])
    );
    assert_eq!(
        choices["images"],
        json!([{ "id": eyes["id"], "name": "Eyes" }])
    );
    assert_eq!(choices["scans"], choices["images"]);
    assert_eq!(choices["audio"], json!([]));
    // Nothing of where a model is.
    assert!(!choices.to_string().contains(&server.endpoint));
    // Bob chooses one of them; one that cannot do the work is refused.
    let chosen = request(
        &app,
        Method::PUT,
        "/api/settings/ai.model.images",
        Some(&bob),
        Some(json!({ "value": eyes["id"] })),
    )
    .await;
    assert_eq!(chosen.status_code(), 200, "{}", chosen.text());
    let refused = request(
        &app,
        Method::PUT,
        "/api/settings/ai.model.images",
        Some(&bob),
        Some(json!({ "value": chat["id"] })),
    )
    .await;
    assert_eq!(refused.status_code(), 400);
    // Not signed in: nothing.
    let anonymous = request(&app, Method::GET, "/api/ai/models", None, None).await;
    assert_eq!(anonymous.status_code(), 401);
}

/// Each trash item carries the user's role on its drive: what they may do
/// with it.
#[tokio::test]
async fn trash_items_carry_the_drive_role() {
    let app = spawn_app().await;
    let admin = admin_session(&app).await;
    let (alice, _) = invited_user(&app, &admin, "alice@example.com").await;
    let (bob, bob_id) = invited_user(&app, &admin, "bob@example.com").await;
    let (drive, root) = shared_drive(&app, &admin, "Team", "alice@example.com").await;
    grant_drive(&app, &alice, &drive, &bob_id, "editor").await;
    let made = upload(&app, &bob, &root, "a.txt", "a").await;
    assert_eq!(made.status_code(), 201);
    let file = json(made).await["id"].as_str().unwrap().to_string();
    let deleted = delete_node(&app, &bob, &file).await;
    assert_eq!(deleted.status_code(), 200);
    assert_eq!(json(deleted).await["my_role"], "editor");
    let trash = |session: String| {
        let (app, drive) = (&app, drive.clone());
        async move {
            json(
                request(
                    app,
                    Method::GET,
                    &format!("/api/trash?drive={drive}"),
                    Some(&session),
                    None,
                )
                .await,
            )
            .await
        }
    };
    assert_eq!(trash(bob.clone()).await[0]["my_role"], "editor");
    assert_eq!(trash(alice.clone()).await[0]["my_role"], "manager");
    // My drive's: its user owns it.
    let mine = root_id(&app, &bob).await;
    let made = upload(&app, &bob, &mine, "b.txt", "b").await;
    let file = json(made).await["id"].as_str().unwrap().to_string();
    assert_eq!(delete_node(&app, &bob, &file).await.status_code(), 200);
    let own = json(request(&app, Method::GET, "/api/trash", Some(&bob), None).await).await;
    assert_eq!(own[0]["my_role"], "owner");
}

/// What a team's managers do to a user (adding them, changing their team
/// role, taking them out) is in the user's activity, with the manager as
/// who did it; what one does oneself (leaving) is not.
#[tokio::test]
async fn team_managers_actions_are_in_the_account_activity() {
    let app = spawn_app().await;
    let admin = admin_session(&app).await;
    let (alice, _) = invited_user(&app, &admin, "alice@example.com").await;
    let (bob, bob_id) = invited_user(&app, &admin, "bob@example.com").await;
    let made = post(
        &app,
        "/api/superadmin/teams",
        &admin,
        Some(json!({ "name": "Lab", "managers": ["alice@example.com"] })),
    )
    .await;
    assert_eq!(made.status_code(), 201);
    let team = json(made).await["id"].as_str().unwrap().to_string();
    let added = post(
        &app,
        &format!("/api/teams/{team}/members"),
        &alice,
        Some(json!({ "email": "bob@example.com" })),
    )
    .await;
    assert_eq!(added.status_code(), 200, "{}", added.text());
    let promoted = request(
        &app,
        Method::PATCH,
        &format!("/api/teams/{team}/members/{bob_id}"),
        Some(&alice),
        Some(json!({ "role": "manager" })),
    )
    .await;
    assert_eq!(promoted.status_code(), 200, "{}", promoted.text());
    let removed = request(
        &app,
        Method::DELETE,
        &format!("/api/teams/{team}/members/{bob_id}"),
        Some(&alice),
        None,
    )
    .await;
    assert_eq!(removed.status_code(), 204);
    let activity = |session: String| {
        let app = &app;
        async move {
            json(request(app, Method::GET, "/api/me/activity", Some(&session), None).await).await
        }
    };
    let bobs = activity(bob.clone()).await;
    let seen: Vec<(&str, &str, &str)> = bobs
        .as_array()
        .unwrap()
        .iter()
        .take(3)
        .map(|e| {
            (
                e["action"].as_str().unwrap(),
                e["actor"]["kind"].as_str().unwrap(),
                e["actor"]["email"].as_str().unwrap(),
            )
        })
        .collect();
    assert_eq!(
        seen,
        [
            ("team_removed", "team_manager", "alice@example.com"),
            ("team_role", "team_manager", "alice@example.com"),
            ("team_added", "team_manager", "alice@example.com"),
        ]
    );
    assert_eq!(bobs[0]["detail"]["team"], "Lab");
    assert_eq!(bobs[1]["detail"]["role"], "manager");
    // Bob, added again, leaves on his own: nothing more for him.
    post(
        &app,
        &format!("/api/teams/{team}/members"),
        &alice,
        Some(json!({ "email": "bob@example.com" })),
    )
    .await;
    let left = request(
        &app,
        Method::DELETE,
        &format!("/api/teams/{team}/members/{bob_id}"),
        Some(&bob),
        None,
    )
    .await;
    assert_eq!(left.status_code(), 204);
    let now = activity(bob.clone()).await;
    assert_eq!(now[0]["action"], "team_added");
    // Alice's own acts are not in hers: only what the superadmin did.
    let hers = activity(alice.clone()).await;
    assert!(
        hers.as_array()
            .unwrap()
            .iter()
            .all(|e| e["actor"]["kind"] == "superadmin"),
        "{hers}"
    );
}

/// The superadmin changes who has access to any shared drive, a member of
/// it or not, through the drive's grants; each change of a user's is in
/// their account activity. A member who does not manage it leaves it by
/// giving up their own grant.
#[tokio::test]
async fn the_superadmin_gives_access_to_shared_drives() {
    let app = spawn_app().await;
    let admin = admin_session(&app).await;
    let (alice, _) = invited_user(&app, &admin, "alice@example.com").await;
    let (bob, bob_id) = invited_user(&app, &admin, "bob@example.com").await;
    let (drive, root) = shared_drive(&app, &admin, "Team", "alice@example.com").await;
    let path = format!("/api/superadmin/drives/{drive}/access");
    let access = json(request(&app, Method::GET, &path, Some(&admin), None).await).await;
    assert_eq!(access["my_role"], "manager");
    assert_eq!(access["may_manage"], true);
    assert_eq!(access["grants"][0]["user"]["email"], "alice@example.com");
    assert_eq!(access["grants"][0]["role"], "manager");
    // Not a member: still nothing of what is in it.
    assert_eq!(
        status_of(&app, &admin, &format!("/api/files/{root}")).await,
        404
    );
    // Bob gets editor, then viewer; never owner.
    let refused = post(
        &app,
        &path,
        &admin,
        Some(json!({ "user_id": bob_id, "role": "owner" })),
    )
    .await;
    assert_eq!(refused.status_code(), 403);
    let given = post(
        &app,
        &path,
        &admin,
        Some(json!({ "user_id": bob_id, "role": "editor" })),
    )
    .await;
    assert_eq!(given.status_code(), 201, "{}", given.text());
    let bobs = json(given).await["id"].as_str().unwrap().to_string();
    assert_eq!(drives_of(&app, &bob).await["Team"]["my_role"], "editor");
    let changed = request(
        &app,
        Method::PATCH,
        &format!("{path}/{bobs}"),
        Some(&admin),
        Some(json!({ "role": "viewer" })),
    )
    .await;
    assert_eq!(changed.status_code(), 200, "{}", changed.text());
    assert_eq!(drives_of(&app, &bob).await["Team"]["my_role"], "viewer");
    // The drive keeps a manager.
    let alices = grant_drive_role_of(&app, &alice, &drive, "alice@example.com").await;
    let kept = request(
        &app,
        Method::DELETE,
        &format!("{path}/{alices}"),
        Some(&admin),
        None,
    )
    .await;
    assert_eq!(kept.status_code(), 409);
    // Bob's activity: given editor, then viewer, by the superadmin.
    let activity =
        json(request(&app, Method::GET, "/api/me/activity", Some(&bob), None).await).await;
    assert_eq!(activity[0]["action"], "drive_access");
    assert_eq!(activity[0]["detail"]["role"], "viewer");
    assert_eq!(activity[0]["detail"]["drive"], "Team");
    assert_eq!(activity[0]["actor"]["kind"], "superadmin");
    assert_eq!(activity[1]["detail"]["role"], "editor");
    // Bob, a viewer, leaves on his own; he may not take anyone else's.
    let theirs = request(
        &app,
        Method::DELETE,
        &format!("/api/drives/{drive}/access/{alices}"),
        Some(&bob),
        None,
    )
    .await;
    assert_eq!(theirs.status_code(), 403);
    let left = request(
        &app,
        Method::DELETE,
        &format!("/api/drives/{drive}/access/{bobs}"),
        Some(&bob),
        None,
    )
    .await;
    assert_eq!(left.status_code(), 204);
    assert!(!drives_of(&app, &bob).await.contains_key("Team"));
    // The superadmin's endpoints are the superadmin's; the old member
    // endpoints are gone.
    assert_eq!(status_of(&app, &alice, &path).await, 403);
    let gone = post(
        &app,
        &format!("/api/superadmin/drives/{drive}/members"),
        &admin,
        Some(json!({ "email": "bob@example.com" })),
    )
    .await;
    assert_eq!(gone.status_code(), 404);
}

/// A shared drive's own settings (its links, its trash and versions) are
/// its managers' to set, within the levels above; its other members read
/// them.
#[tokio::test]
async fn a_shared_drives_managers_set_its_settings() {
    let app = spawn_app().await;
    let admin = admin_session(&app).await;
    let (alice, _) = invited_user(&app, &admin, "alice@example.com").await;
    let (bob, bob_id) = invited_user(&app, &admin, "bob@example.com").await;
    let (drive, _) = shared_drive(&app, &admin, "Team", "alice@example.com").await;
    grant_drive(&app, &alice, &drive, &bob_id, "editor").await;
    let path = format!("/api/drives/{drive}/settings");
    let listed = json(request(&app, Method::GET, &path, Some(&bob), None).await).await;
    let keys: Vec<&str> = listed
        .as_array()
        .unwrap()
        .iter()
        .map(|s| s["key"].as_str().unwrap())
        .collect();
    assert!(keys.contains(&"links.allowed"), "{keys:?}");
    assert!(keys.contains(&"retention.trash_days"), "{keys:?}");
    assert!(keys.contains(&"mirrors.allowed"), "{keys:?}");
    assert!(keys.contains(&"federation.allowed"), "{keys:?}");
    assert!(keys.iter().all(|k| k.starts_with("links.")
        || k.starts_with("retention.")
        || k.starts_with("mirrors.")
        || k.starts_with("federation.")));
    let set = |session: String, key: &'static str, body: Value| {
        let (app, path) = (&app, path.clone());
        async move {
            request(
                app,
                Method::PUT,
                &format!("{path}/{key}"),
                Some(&session),
                Some(body),
            )
            .await
        }
    };
    let refused = set(
        bob.clone(),
        "retention.trash_days",
        json!({ "mode": "default", "value": 3 }),
    )
    .await;
    assert_eq!(refused.status_code(), 403);
    let saved = set(
        alice.clone(),
        "retention.trash_days",
        json!({ "mode": "default", "value": 3 }),
    )
    .await;
    assert_eq!(saved.status_code(), 200, "{}", saved.text());
    assert_eq!(json(saved).await["value"], 3);
    // Within what the superadmin leaves: at least 7 days.
    let ranged = request(
        &app,
        Method::PUT,
        "/api/superadmin/settings/retention.trash_days",
        Some(&admin),
        Some(json!({ "mode": "range", "min": 7 })),
    )
    .await;
    assert_eq!(ranged.status_code(), 200, "{}", ranged.text());
    let below = set(
        alice.clone(),
        "retention.trash_days",
        json!({ "mode": "default", "value": 2 }),
    )
    .await;
    assert_eq!(below.status_code(), 400);
    let off = set(
        alice.clone(),
        "links.allowed",
        json!({ "mode": "locked", "value": false }),
    )
    .await;
    assert_eq!(off.status_code(), 200, "{}", off.text());
}

// ---------------------------------------------------------------------------
// Storage backends (docs/M7C_DESIGN.md). The tests have no S3: opendal's
// in-memory service stands in for an endpoint (like S3 it has no rename and
// keeps folders as markers; it has no copy either), so what the server
// writes there can be looked at.
// ---------------------------------------------------------------------------

/// The in-memory service standing in for an S3 endpoint.
fn stand_in(app: &TestApp, endpoint: &str) -> opendal::Operator {
    let op = opendal::Operator::new(opendal::services::Memory::default()).unwrap();
    app.state.files.backends().stand_in(endpoint, op.clone());
    op
}

/// A backend's body: an S3 connection at `endpoint`, with `extra` on top.
fn backend_body(name: &str, endpoint: &str, extra: Value) -> Value {
    let mut body = json!({
        "name": name,
        "endpoint": endpoint,
        "bucket": "bucket",
        "prefix": "organizer",
        "access_key": "AKIA-TEST",
        "secret": "the-secret-access-key",
    });
    body.as_object_mut()
        .unwrap()
        .extend(extra.as_object().unwrap().clone());
    body
}

/// A global backend added by the superadmin.
async fn global_backend(
    app: &TestApp,
    admin: &str,
    name: &str,
    endpoint: &str,
    extra: Value,
) -> Value {
    let made = post(
        app,
        "/api/superadmin/backends",
        admin,
        Some(backend_body(name, endpoint, extra)),
    )
    .await;
    assert_eq!(made.status_code(), 201, "{}", made.text());
    json(made).await
}

/// A shared drive made by the superadmin on a backend: its id and root.
async fn drive_on(app: &TestApp, admin: &str, name: &str, backend: &str) -> (String, String) {
    let made = post(
        app,
        "/api/superadmin/drives",
        admin,
        Some(json!({
            "name": name,
            "backend_id": backend,
            "members": [{ "email": "admin@example.com", "manages": true }],
        })),
    )
    .await;
    assert_eq!(made.status_code(), 201, "{}", made.text());
    let made = json(made).await;
    (
        made["drive"]["id"].as_str().unwrap().to_string(),
        made["drive"]["root_id"].as_str().unwrap().to_string(),
    )
}

/// The objects under a prefix of a stand-in (not the folders' markers),
/// sorted.
async fn objects(op: &opendal::Operator, prefix: &str) -> Vec<String> {
    let mut keys: Vec<String> = op
        .list_with(prefix)
        .recursive(true)
        .await
        .unwrap()
        .into_iter()
        .map(|e| e.path().to_string())
        .filter(|p| !p.ends_with('/'))
        .collect();
    keys.sort();
    keys
}

async fn download_text(app: &TestApp, session: &str, id: &str) -> Response {
    request(
        app,
        Method::GET,
        &format!("/api/files/{id}/content"),
        Some(session),
        None,
    )
    .await
}

async fn backends_of(app: &TestApp, session: &str, path: &str) -> Value {
    let response = request(app, Method::GET, path, Some(session), None).await;
    assert_eq!(response.status_code(), 200, "{}", response.text());
    json(response).await
}

/// The superadmin adds, tests, changes and removes a global backend; its
/// secret is kept encrypted and never sent back; a shared drive on it
/// keeps its tree as plain objects at `<prefix>/<drive id>/`, and every
/// file operation works there: uploads, downloads (ranges too), folders,
/// renames (each object copied, then deleted), the trash and its restore,
/// versions.
#[tokio::test]
async fn global_backends_hold_drives_as_plain_objects() {
    let app = spawn_app().await;
    let admin = admin_session(&app).await;
    let endpoint = "http://minio.internal:9000";
    let op = stand_in(&app, endpoint);

    // Tested before it is added: an endpoint that does not answer is not.
    let test = post(
        &app,
        "/api/superadmin/backends/test",
        &admin,
        Some(backend_body("Nowhere", "http://127.0.0.1:9", json!({}))),
    )
    .await;
    assert_eq!(test.status_code(), 200);
    let test = json(test).await;
    assert_eq!(test["ok"], json!(false));
    assert_eq!(test["error"], "The storage cannot be reached");
    let refused = post(
        &app,
        "/api/superadmin/backends",
        &admin,
        Some(backend_body("Nowhere", "http://127.0.0.1:9", json!({}))),
    )
    .await;
    assert_eq!(refused.status_code(), 400);
    // The superadmin's may be on the server's own network.
    let made = post(
        &app,
        "/api/superadmin/backends",
        &admin,
        Some(backend_body(
            "Cold storage",
            endpoint,
            json!({ "list_every_minutes": 30 }),
        )),
    )
    .await;
    assert_eq!(made.status_code(), 201, "{}", made.text());
    assert!(!made.text().contains("the-secret-access-key"));
    let backend = json(made).await;
    assert_eq!(backend["kind"], "s3");
    assert_eq!(backend["secret_set"], json!(true));
    assert_eq!(backend["private"], json!(false));
    assert_eq!(backend["status"], "ok");
    assert_eq!(backend["backed_up"], json!(false));
    assert_eq!(backend["list_every_minutes"], json!(30));
    assert_eq!(backend["make_drives"], "superadmin");
    let id = backend["id"].as_str().unwrap().to_string();
    // The test wrote nothing that stays.
    assert!(objects(&op, "").await.is_empty());
    // Stored under the server key: the database never holds the secret.
    let stored: Vec<u8> = sqlx::query_scalar("SELECT secret FROM backends WHERE id = ?1")
        .bind(uuid::Uuid::parse_str(&id).unwrap())
        .fetch_one(app.state.db.reader())
        .await
        .unwrap();
    assert!(!stored.windows(10).any(|w| w == b"the-secret"));
    let list = backends_of(&app, &admin, "/api/superadmin/backends").await;
    assert_eq!(list[0]["kind"], "disk");
    assert_eq!(list[0]["backed_up"], json!(true));
    assert_eq!(list[1]["name"], "Cold storage");
    assert!(!list.to_string().contains("the-secret-access-key"));
    let tested = post(
        &app,
        &format!("/api/superadmin/backends/{id}/test"),
        &admin,
        None,
    )
    .await;
    assert_eq!(json(tested).await["ok"], json!(true));
    // The same name twice is a conflict.
    let twice = post(
        &app,
        "/api/superadmin/backends",
        &admin,
        Some(backend_body("cold STORAGE", endpoint, json!({}))),
    )
    .await;
    assert_eq!(twice.status_code(), 409);

    // A drive on it, at its own prefix.
    let (drive, root) = drive_on(&app, &admin, "Archive", &id).await;
    let drives = drives_of(&app, &admin).await;
    assert_eq!(drives["Archive"]["backend"]["name"], "Cold storage");
    assert_eq!(drives["Archive"]["backend"]["kind"], "s3");
    assert_eq!(drives["Archive"]["backend"]["owner"], Value::Null);
    assert_eq!(drives["Archive"]["backend"]["backed_up"], json!(false));
    assert_eq!(drives["Archive"]["available"], json!(true));
    assert_eq!(drives["My drive"]["backend"]["kind"], "disk");
    assert_eq!(drives["My drive"]["backend"]["backed_up"], json!(true));
    let base = format!("organizer/{drive}/");
    let folder = post(
        &app,
        &format!("/api/files/{root}/folders"),
        &admin,
        Some(json!({ "name": "Reports" })),
    )
    .await;
    assert_eq!(folder.status_code(), 201, "{}", folder.text());
    let folder = json(folder).await;
    let folder_id = folder["id"].as_str().unwrap().to_string();
    let response = upload(&app, &admin, &folder_id, "q1.txt", "first quarter").await;
    assert_eq!(response.status_code(), 201, "{}", response.text());
    let q1 = json(response).await;
    let q1_id = q1["id"].as_str().unwrap().to_string();
    assert_eq!(objects(&op, &base).await, [format!("{base}Reports/q1.txt")]);
    let got = download_text(&app, &admin, &q1_id).await;
    assert_eq!(got.status_code(), 200);
    assert_eq!(got.text(), "first quarter");
    let part = app
        .server
        .get(&format!("/api/files/{q1_id}/content"))
        .add_header("cookie", &admin)
        .add_header("range", "bytes=6-12")
        .await;
    assert_eq!(part.status_code(), 206);
    assert_eq!(part.text(), "quarter");
    assert_eq!(part.headers()["content-range"], "bytes 6-12/13");

    // A replace keeps the old contents as a version, in the drive's
    // `.organizer/versions/`.
    let replaced = app
        .server
        .put(&format!("/api/files/{q1_id}/content"))
        .add_header("cookie", &admin)
        .add_header("if-match", format!("\"{}\"", q1["version"]))
        .bytes("Q1, revised".as_bytes().to_vec().into())
        .await;
    assert_eq!(replaced.status_code(), 200, "{}", replaced.text());
    let versions = backends_of(&app, &admin, &format!("/api/files/{q1_id}/versions")).await;
    let version = versions[0]["id"].as_str().unwrap().to_string();
    assert!(
        objects(&op, &base)
            .await
            .contains(&format!("{base}.organizer/versions/{version}"))
    );
    assert_eq!(
        download_text(&app, &admin, &q1_id).await.text(),
        "Q1, revised"
    );

    // A folder renamed: every object copied to the new place, the old
    // ones deleted; the ids stay.
    let renamed = request(
        &app,
        Method::PATCH,
        &format!("/api/files/{folder_id}"),
        Some(&admin),
        Some(json!({ "name": "Quarterly", "if_version": folder["version"] })),
    )
    .await;
    assert_eq!(renamed.status_code(), 200, "{}", renamed.text());
    let keys = objects(&op, &base).await;
    assert!(
        keys.contains(&format!("{base}Quarterly/q1.txt")),
        "{keys:?}"
    );
    assert!(!keys.iter().any(|k| k.contains("/Reports/")), "{keys:?}");
    assert_eq!(
        download_text(&app, &admin, &q1_id).await.text(),
        "Q1, revised"
    );
    let left: i64 = sqlx::query_scalar("SELECT count(*) FROM journal")
        .fetch_one(app.state.db.reader())
        .await
        .unwrap();
    assert_eq!(left, 0);

    // To the trash and back.
    let renamed = json(renamed).await;
    let deleted = request(
        &app,
        Method::DELETE,
        &format!("/api/files/{folder_id}?if_version={}", renamed["version"]),
        Some(&admin),
        None,
    )
    .await;
    assert_eq!(deleted.status_code(), 200, "{}", deleted.text());
    assert!(
        objects(&op, &base)
            .await
            .contains(&format!("{base}.organizer/trash/{folder_id}/q1.txt"))
    );
    let restored = post(
        &app,
        &format!("/api/trash/{folder_id}/restore"),
        &admin,
        None,
    )
    .await;
    assert_eq!(restored.status_code(), 200, "{}", restored.text());
    assert!(
        objects(&op, &base)
            .await
            .contains(&format!("{base}Quarterly/q1.txt"))
    );
    assert_eq!(names_in(&app, &admin, &folder_id).await, ["q1.txt"]);
    // A listing finds nothing changed.
    let drive_id = uuid::Uuid::parse_str(&drive).unwrap();
    assert_eq!(
        app.state.files.scan(drive_id).await.unwrap(),
        Default::default()
    );

    // Its bucket and prefix stay while it holds drives; the name changes.
    let changed = request(
        &app,
        Method::PATCH,
        &format!("/api/superadmin/backends/{id}"),
        Some(&admin),
        Some(json!({ "prefix": "elsewhere" })),
    )
    .await;
    assert_eq!(changed.status_code(), 409);
    let changed = request(
        &app,
        Method::PATCH,
        &format!("/api/superadmin/backends/{id}"),
        Some(&admin),
        Some(json!({ "name": "Archive storage", "limit": 1_000_000 })),
    )
    .await;
    assert_eq!(changed.status_code(), 200, "{}", changed.text());
    let changed = json(changed).await;
    assert_eq!(changed["name"], "Archive storage");
    assert_eq!(changed["limit"], json!(1_000_000));
    assert_eq!(changed["drives"], json!(1));
    // Not removed while it holds a drive; the drive's objects go with it.
    let refused = request(
        &app,
        Method::DELETE,
        &format!("/api/superadmin/backends/{id}"),
        Some(&admin),
        None,
    )
    .await;
    assert_eq!(refused.status_code(), 409);
    let gone = request(
        &app,
        Method::DELETE,
        &format!("/api/superadmin/drives/{drive}"),
        Some(&admin),
        None,
    )
    .await;
    assert_eq!(gone.status_code(), 204);
    assert!(objects(&op, &base).await.is_empty());
    let removed = request(
        &app,
        Method::DELETE,
        &format!("/api/superadmin/backends/{id}"),
        Some(&admin),
        None,
    )
    .await;
    assert_eq!(removed.status_code(), 204);
}

/// Who may make drives on a global backend: the superadmin always; the
/// others as its `make_drives` says (team managers for their team, chosen
/// teams, everyone who may make a drive); `/backends/choices` lists what
/// a user may pick. A backend's limit for new drives is each new drive's.
#[tokio::test]
async fn who_makes_drives_on_a_global_backend() {
    let app = spawn_app().await;
    let admin = admin_session(&app).await;
    let (alice, _) = invited_user(&app, &admin, "alice@example.com").await;
    stand_in(&app, "https://s3.example.com");
    let backend = global_backend(
        &app,
        &admin,
        "Shared S3",
        "https://s3.example.com",
        json!({}),
    )
    .await;
    let id = backend["id"].as_str().unwrap().to_string();
    let team = post(
        &app,
        "/api/superadmin/teams",
        &admin,
        Some(json!({ "name": "Research", "managers": ["alice@example.com"] })),
    )
    .await;
    assert_eq!(team.status_code(), 201, "{}", team.text());
    let team = json(team).await["id"].as_str().unwrap().to_string();
    let team_drive = async |name: &str| {
        post(
            &app,
            &format!("/api/teams/{team}/drives"),
            &alice,
            Some(json!({ "name": name, "backend_id": id })),
        )
        .await
    };
    let choices = |query: String| {
        let app = &app;
        let alice = &alice;
        async move {
            backends_of(app, alice, &format!("/api/backends/choices{query}"))
                .await
                .as_array()
                .unwrap()
                .iter()
                .map(|c| c["name"].as_str().unwrap().to_string())
                .collect::<Vec<_>>()
        }
    };
    let set = async |body: Value| {
        request(
            &app,
            Method::PATCH,
            &format!("/api/superadmin/backends/{id}"),
            Some(&admin),
            Some(body),
        )
        .await
    };
    // The superadmin only, by default.
    assert_eq!(team_drive("Lab").await.status_code(), 403);
    assert_eq!(
        choices(format!("?team_id={team}")).await,
        ["The server's disk"]
    );
    // The teams' managers: alice for her team's drive, not for her own.
    assert_eq!(
        set(json!({ "make_drives": "team_managers" }))
            .await
            .status_code(),
        200
    );
    assert_eq!(
        choices(format!("?team_id={team}")).await,
        ["The server's disk", "Shared S3"]
    );
    assert_eq!(choices(String::new()).await, ["The server's disk"]);
    let made = team_drive("Lab").await;
    assert_eq!(made.status_code(), 201, "{}", made.text());
    assert_eq!(json(made).await["backend"]["name"], "Shared S3");
    // Chosen teams: not hers, then hers.
    assert_eq!(
        set(json!({ "make_drives": "teams", "team_ids": [] }))
            .await
            .status_code(),
        200
    );
    assert_eq!(team_drive("Lab 2").await.status_code(), 403);
    assert_eq!(set(json!({ "team_ids": [team] })).await.status_code(), 200);
    assert_eq!(team_drive("Lab 2").await.status_code(), 201);
    // Everyone (who may make a shared drive at all: `drives.create`).
    assert_eq!(
        set(json!({ "make_drives": "everyone" }))
            .await
            .status_code(),
        200
    );
    let own = |name: &str| {
        post(
            &app,
            "/api/drives",
            &alice,
            Some(json!({ "name": name, "backend_id": id })),
        )
    };
    assert_eq!(own("Mine").await.status_code(), 403);
    let everyone = request(
        &app,
        Method::PUT,
        "/api/superadmin/settings/drives.create",
        Some(&admin),
        Some(json!({ "mode": "locked", "value": "everyone" })),
    )
    .await;
    assert_eq!(everyone.status_code(), 200);
    let made = own("Mine").await;
    assert_eq!(made.status_code(), 201, "{}", made.text());
    // No such storage.
    let none = post(
        &app,
        "/api/drives",
        &alice,
        Some(json!({ "name": "Lost", "backend_id": uuid::Uuid::now_v7() })),
    )
    .await;
    assert_eq!(none.status_code(), 400);
    assert_eq!(set(json!({ "drive_limit": 1000 })).await.status_code(), 200);
    let made = own("Limited").await;
    assert_eq!(json(made).await["limit"], json!(1000));
}

/// Users add their own S3 storage only when the setting allows it, at a
/// public https address (unless `BACKENDS_ALLOW_PRIVATE`), with a session;
/// a drive they make there is theirs to manage and its members see whose
/// storage it is; nobody else makes a drive there, the superadmin neither;
/// removing it takes the drive out of Organizer and leaves the objects in
/// the bucket.
#[tokio::test]
async fn users_own_backends_follow_the_setting_and_the_address_rules() {
    let app = spawn_app().await;
    let admin = admin_session(&app).await;
    let (alice, _) = invited_user(&app, &admin, "alice@example.com").await;
    let (bob, bob_id) = invited_user(&app, &admin, "bob@example.com").await;
    let endpoint = "https://8.8.8.8";
    let op = stand_in(&app, endpoint);
    // Off by default.
    let refused = post(
        &app,
        "/api/backends",
        &alice,
        Some(backend_body("Mine", endpoint, json!({}))),
    )
    .await;
    assert_eq!(refused.status_code(), 403);
    let on = request(
        &app,
        Method::PUT,
        "/api/superadmin/settings/storage.own_backends",
        Some(&admin),
        Some(json!({ "mode": "default", "value": true })),
    )
    .await;
    assert_eq!(on.status_code(), 200, "{}", on.text());
    // Not the server's own network, and https only.
    for private in [
        "https://127.0.0.1:9000",
        "https://10.1.2.3",
        "https://169.254.169.254",
        "https://[::1]:9000",
        "http://8.8.8.8",
    ] {
        stand_in(&app, private);
        let refused = post(
            &app,
            "/api/backends",
            &alice,
            Some(backend_body("Mine", private, json!({}))),
        )
        .await;
        assert_eq!(refused.status_code(), 400, "{private}");
    }
    // An API key does not manage storage.
    let key = create_key(&app, &alice, "write").await;
    let with_token = app
        .server
        .post("/api/backends")
        .add_header(
            "authorization",
            format!("Bearer {}", key["token"].as_str().unwrap()),
        )
        .json(&backend_body("Mine", endpoint, json!({})))
        .await;
    assert_eq!(with_token.status_code(), 403);
    let made = post(
        &app,
        "/api/backends",
        &alice,
        Some(backend_body("Mine", endpoint, json!({}))),
    )
    .await;
    assert_eq!(made.status_code(), 201, "{}", made.text());
    let backend = json(made).await;
    assert_eq!(backend["owner"]["email"], "alice@example.com");
    let id = backend["id"].as_str().unwrap().to_string();
    // Only hers: not in another user's list, not among the global ones.
    assert_eq!(backends_of(&app, &bob, "/api/backends").await, json!([]));
    let global = backends_of(&app, &admin, "/api/superadmin/backends").await;
    assert_eq!(global.as_array().unwrap().len(), 1);
    let theirs = request(
        &app,
        Method::PATCH,
        &format!("/api/backends/{id}"),
        Some(&bob),
        Some(json!({ "name": "Taken" })),
    )
    .await;
    assert_eq!(theirs.status_code(), 404);
    // Her drive there (`drives.create` need not be everyone's); bob, a
    // member, sees whose storage it is.
    let made = post(
        &app,
        "/api/drives",
        &alice,
        Some(json!({ "name": "Holidays", "backend_id": id })),
    )
    .await;
    assert_eq!(made.status_code(), 201, "{}", made.text());
    let drive = json(made).await;
    assert_eq!(drive["backend"]["owner"], "alice@example.com");
    assert_eq!(drive["my_role"], "manager");
    let drive_id = drive["id"].as_str().unwrap().to_string();
    grant_drive(&app, &alice, &drive_id, &bob_id, "editor").await;
    let seen = drives_of(&app, &bob).await;
    assert_eq!(seen["Holidays"]["backend"]["owner"], "alice@example.com");
    let root = seen["Holidays"]["root_id"].as_str().unwrap().to_string();
    assert_eq!(
        upload(&app, &bob, &root, "beach.txt", "sand")
            .await
            .status_code(),
        201
    );
    let not_bobs = post(
        &app,
        "/api/drives",
        &bob,
        Some(json!({ "name": "Bob's", "backend_id": id })),
    )
    .await;
    assert_eq!(not_bobs.status_code(), 403);
    let not_admins = post(
        &app,
        "/api/superadmin/drives",
        &admin,
        Some(json!({ "name": "Admin's", "backend_id": id })),
    )
    .await;
    assert_eq!(not_admins.status_code(), 403);
    // Removed: the drive goes from Organizer, the objects stay.
    let removed = request(
        &app,
        Method::DELETE,
        &format!("/api/backends/{id}"),
        Some(&alice),
        None,
    )
    .await;
    assert_eq!(removed.status_code(), 204);
    assert!(!drives_of(&app, &bob).await.contains_key("Holidays"));
    assert_eq!(
        objects(&op, "").await,
        [format!("organizer/{drive_id}/beach.txt")]
    );
    // With BACKENDS_ALLOW_PRIVATE, the server's own network is allowed.
    app.state.files.backends().set_allow_private(true);
    stand_in(&app, "http://10.1.2.3:9000");
    let made = post(
        &app,
        "/api/backends",
        &alice,
        Some(backend_body("LAN", "http://10.1.2.3:9000", json!({}))),
    )
    .await;
    assert_eq!(made.status_code(), 201, "{}", made.text());
}

/// Private credentials: only the sealed secret is stored; the session that
/// added (or unlocked) it holds the secret in memory, and without one the
/// drive is locked for its members "until" its owner signs in.
#[tokio::test]
async fn private_credentials_lock_the_drive_without_a_session() {
    let app = spawn_app().await;
    let admin = admin_session(&app).await;
    let (alice, _) = invited_user(&app, &admin, "alice@example.com").await;
    let on = request(
        &app,
        Method::PUT,
        "/api/superadmin/settings/storage.own_backends",
        Some(&admin),
        Some(json!({ "mode": "default", "value": true })),
    )
    .await;
    assert_eq!(on.status_code(), 200);
    let endpoint = "https://9.9.9.9";
    stand_in(&app, endpoint);
    let body = backend_body(
        "Vault",
        endpoint,
        json!({ "private": true, "sealed_secret": opaque(120, 1) }),
    );
    // Sealed to her encryption key: set up first.
    let refused = post(&app, "/api/backends", &alice, Some(body.clone())).await;
    assert_eq!(refused.status_code(), 400);
    assert_eq!(set_up_encryption(&app, &alice).await.status_code(), 201);
    let made = post(&app, "/api/backends", &alice, Some(body)).await;
    assert_eq!(made.status_code(), 201, "{}", made.text());
    let backend = json(made).await;
    assert_eq!(backend["private"], json!(true));
    assert_eq!(backend["status"], "ok");
    let id = backend["id"].as_str().unwrap().to_string();
    let (secret, sealed): (Option<Vec<u8>>, Option<String>) =
        sqlx::query_as("SELECT secret, sealed_secret FROM backends WHERE id = ?1")
            .bind(uuid::Uuid::parse_str(&id).unwrap())
            .fetch_one(app.state.db.reader())
            .await
            .unwrap();
    assert_eq!(secret, None);
    assert_eq!(sealed, Some(opaque(120, 1)));
    let made = post(
        &app,
        "/api/drives",
        &alice,
        Some(json!({ "name": "Diary", "backend_id": id })),
    )
    .await;
    assert_eq!(made.status_code(), 201, "{}", made.text());
    let drive = json(made).await;
    let root = drive["root_id"].as_str().unwrap().to_string();
    let file = json(upload(&app, &alice, &root, "day.txt", "sunny").await).await;
    let file_id = file["id"].as_str().unwrap().to_string();

    // Signed out: the secret goes with the session, the drive is locked.
    assert_eq!(
        post(&app, "/api/auth/logout", &alice, None)
            .await
            .status_code(),
        204
    );
    app.state.files.close_locked().await;
    let alice = cookie(&password_login(&app, "alice@example.com").await);
    let drives = drives_of(&app, &alice).await;
    assert_eq!(drives["Diary"]["available"], json!(false));
    assert_eq!(drives["Diary"]["unavailable"], "locked");
    assert_eq!(
        drives["Diary"]["unavailable_reason"],
        "Locked until alice@example.com signs in"
    );
    let locked = download_text(&app, &alice, &file_id).await;
    assert_eq!(locked.status_code(), 503);
    assert_eq!(json(locked).await["code"], "storage_unavailable");
    let mine = backends_of(&app, &alice, "/api/backends").await;
    assert_eq!(mine[0]["status"], "locked");
    // The new session's client opens the sealed secret it is given back.
    assert_eq!(mine[0]["sealed_secret"], json!(opaque(120, 1)));
    // Unlocked for this session (its device opened the sealed secret).
    let unlocked = request(
        &app,
        Method::PUT,
        &format!("/api/backends/{id}/unlock"),
        Some(&alice),
        Some(json!({ "secret": "the-secret-access-key" })),
    )
    .await;
    assert_eq!(unlocked.status_code(), 200, "{}", unlocked.text());
    assert_eq!(json(unlocked).await["status"], "ok");
    assert_eq!(download_text(&app, &alice, &file_id).await.text(), "sunny");
    // Locked again by hand.
    let locked = request(
        &app,
        Method::DELETE,
        &format!("/api/backends/{id}/unlock"),
        Some(&alice),
        None,
    )
    .await;
    assert_eq!(json(locked).await["status"], "locked");
    assert_eq!(
        download_text(&app, &alice, &file_id).await.status_code(),
        503
    );
    // A secret the server keeps has nothing to unlock.
    stand_in(&app, "https://1.1.1.1");
    let plain = post(
        &app,
        "/api/backends",
        &alice,
        Some(backend_body("Plain", "https://1.1.1.1", json!({}))),
    )
    .await;
    assert_eq!(plain.status_code(), 201, "{}", plain.text());
    let plain = json(plain).await;
    let nothing = request(
        &app,
        Method::PUT,
        &format!("/api/backends/{}/unlock", plain["id"].as_str().unwrap()),
        Some(&alice),
        Some(json!({ "secret": "x" })),
    )
    .await;
    assert_eq!(nothing.status_code(), 400);
}

/// The drive's limit and its backend's (for all its drives) both count:
/// a write over either is refused with 507.
#[tokio::test]
async fn a_backends_limit_counts_with_the_drives() {
    let app = spawn_app().await;
    let admin = admin_session(&app).await;
    stand_in(&app, "https://s3.example.com");
    let backend = global_backend(
        &app,
        &admin,
        "Small",
        "https://s3.example.com",
        json!({ "limit": 10, "drive_limit": 8 }),
    )
    .await;
    let id = backend["id"].as_str().unwrap().to_string();
    let (_, first) = drive_on(&app, &admin, "First", &id).await;
    let (_, second) = drive_on(&app, &admin, "Second", &id).await;
    // The drive's own limit: 8.
    let over = upload(&app, &admin, &first, "a.txt", "123456789").await;
    assert_eq!(over.status_code(), 507);
    let fits = upload(&app, &admin, &first, "a.txt", "123456").await;
    assert_eq!(fits.status_code(), 201);
    // The backend's: 10 for both drives together.
    let over = upload(&app, &admin, &second, "b.txt", "12345").await;
    assert_eq!(over.status_code(), 507);
    let fits = upload(&app, &admin, &second, "b.txt", "1234").await;
    assert_eq!(fits.status_code(), 201);
    let list = backends_of(&app, &admin, "/api/superadmin/backends").await;
    assert_eq!(list[1]["used"], json!(10));
}

/// A backend that stops answering marks its drives unavailable with the
/// reason, for their members and on the backend's row; nothing is deleted,
/// and once it answers again (a test) they are back.
#[tokio::test]
async fn a_failing_backend_makes_its_drives_unavailable() {
    let app = spawn_app().await;
    let admin = admin_session(&app).await;
    let endpoint = "http://127.0.0.1:9";
    let op = stand_in(&app, endpoint);
    let backend = global_backend(&app, &admin, "Flaky", endpoint, json!({})).await;
    let id = backend["id"].as_str().unwrap().to_string();
    let (drive, root) = drive_on(&app, &admin, "Team files", &id).await;
    let file = json(upload(&app, &admin, &root, "plan.txt", "plan").await).await;
    let file_id = file["id"].as_str().unwrap().to_string();

    // The endpoint is itself again: nothing listens there.
    app.state.files.backends().drop_stand_in(endpoint);
    app.state
        .files
        .forget_backend(uuid::Uuid::parse_str(&id).unwrap())
        .await;
    let failed = download_text(&app, &admin, &file_id).await;
    assert_eq!(failed.status_code(), 503);
    let failed = json(failed).await;
    assert_eq!(failed["code"], "storage_unavailable");
    assert_eq!(failed["error"], "The storage cannot be reached");
    let drives = drives_of(&app, &admin).await;
    assert_eq!(drives["Team files"]["available"], json!(false));
    assert_eq!(drives["Team files"]["unavailable"], "failing");
    assert_eq!(
        drives["Team files"]["unavailable_reason"],
        "The storage cannot be reached"
    );
    let list = backends_of(&app, &admin, "/api/superadmin/backends").await;
    assert_eq!(list[1]["status"], "failing");
    assert_eq!(list[1]["error"], "The storage cannot be reached");
    // Nothing deleted: the index keeps the file, even after a listing
    // that failed.
    let drive_id = uuid::Uuid::parse_str(&drive).unwrap();
    assert!(app.state.files.scan(drive_id).await.is_err());
    assert_eq!(names_in(&app, &admin, &root).await, ["plan.txt"]);
    // Back (the drive's tree took the real endpoint's operator: it is
    // opened anew with the stand-in).
    app.state.files.backends().stand_in(endpoint, op);
    app.state
        .files
        .forget_backend(uuid::Uuid::parse_str(&id).unwrap())
        .await;
    let tested = post(
        &app,
        &format!("/api/superadmin/backends/{id}/test"),
        &admin,
        None,
    )
    .await;
    assert_eq!(json(tested).await["ok"], json!(true));
    assert_eq!(
        drives_of(&app, &admin).await["Team files"]["available"],
        json!(true)
    );
    assert_eq!(download_text(&app, &admin, &file_id).await.text(), "plan");
}

/// The periodic listing of a drive's prefix finds what was changed in the
/// bucket outside Organizer: new objects (and the folders they imply),
/// changed ones, deleted ones; a file moved there keeps its id; and
/// Organizer's own folder is never one of its files.
#[tokio::test]
async fn the_listing_of_a_bucket_finds_outside_changes() {
    let app = spawn_app().await;
    let admin = admin_session(&app).await;
    let op = stand_in(&app, "https://s3.example.com");
    let backend = global_backend(&app, &admin, "S3", "https://s3.example.com", json!({})).await;
    let (drive, root) = drive_on(&app, &admin, "Synced", backend["id"].as_str().unwrap()).await;
    let drive_id = uuid::Uuid::parse_str(&drive).unwrap();
    let base = format!("organizer/{drive}/");
    let keep = json(upload(&app, &admin, &root, "keep.txt", "kept").await).await;
    let moved = json(upload(&app, &admin, &root, "moved.txt", "moved content").await).await;
    upload(&app, &admin, &root, "gone.txt", "gone").await;
    // The hashes are known (what Organizer writes is hashed as it goes).
    let hashed: i64 = sqlx::query_scalar(
        "SELECT count(*) FROM nodes WHERE drive_id = ?1 AND kind = 'file' AND hash IS NOT NULL",
    )
    .bind(drive_id)
    .fetch_one(app.state.db.reader())
    .await
    .unwrap();
    assert_eq!(hashed, 3);
    // Another tool writes to the bucket.
    let write = |key: String, content: &'static [u8]| {
        let op = op.clone();
        async move { op.write(&key, content.to_vec()).await.unwrap() }
    };
    write(format!("{base}Inbox/new.txt"), b"new").await;
    write(format!("{base}keep.txt"), b"changed!").await;
    op.delete(&format!("{base}gone.txt")).await.unwrap();
    write(format!("{base}Inbox/moved.txt"), b"moved content").await;
    op.delete(&format!("{base}moved.txt")).await.unwrap();
    write(format!("{base}.organizer/trash/stray"), b"x").await;

    let report = app.state.files.scan(drive_id).await.unwrap();
    assert_eq!(report.created, 2, "{report}");
    assert_eq!(report.updated, 1, "{report}");
    assert_eq!(report.moved, 1, "{report}");
    assert_eq!(report.deleted, 1, "{report}");
    let mut names = names_in(&app, &admin, &root).await;
    names.sort();
    assert_eq!(names, ["Inbox", "keep.txt"]);
    assert_eq!(
        download_text(&app, &admin, keep["id"].as_str().unwrap())
            .await
            .text(),
        "changed!"
    );
    let moved_now = backends_of(
        &app,
        &admin,
        &format!("/api/files/{}", moved["id"].as_str().unwrap()),
    )
    .await;
    assert_eq!(moved_now["name"], "moved.txt");
    assert_ne!(moved_now["parent_id"], json!(root));
    // Listed again: nothing more.
    assert_eq!(
        app.state.files.scan(drive_id).await.unwrap(),
        Default::default()
    );
}

/// A drive moves between backends: copied whole (trash and versions too),
/// read-only meanwhile, then switched over with the same ids; the old copy
/// goes. A move a stop interrupted part-way is finished when it is carried
/// on (at the next start).
#[tokio::test]
async fn drives_move_between_backends() {
    let app = spawn_app().await;
    let admin = admin_session(&app).await;
    let op = stand_in(&app, "https://s3.example.com");
    let backend = global_backend(&app, &admin, "S3", "https://s3.example.com", json!({})).await;
    let backend_id = backend["id"].as_str().unwrap().to_string();
    let (drive, root) = shared_drive(&app, &admin, "Projects", "admin@example.com").await;
    let drive_id = uuid::Uuid::parse_str(&drive).unwrap();
    let file = json(upload(&app, &admin, &root, "plan.txt", "the plan").await).await;
    let file_id = file["id"].as_str().unwrap().to_string();
    let trashed = json(upload(&app, &admin, &root, "old.txt", "old").await).await;
    let trashed_id = trashed["id"].as_str().unwrap().to_string();
    let deleted = request(
        &app,
        Method::DELETE,
        &format!("/api/files/{trashed_id}?if_version={}", trashed["version"]),
        Some(&admin),
        None,
    )
    .await;
    assert_eq!(deleted.status_code(), 200);
    let folder = app.state.files.local_base().join("drives").join(&drive);
    assert!(folder.join("plan.txt").is_file());

    // Its manager may not use that storage (the superadmin's only); the
    // superadmin moves it.
    let refused = post(
        &app,
        &format!("/api/drives/{drive}/move"),
        &admin,
        Some(json!({ "backend_id": backend_id })),
    )
    .await;
    assert_eq!(refused.status_code(), 403, "{}", refused.text());
    let moved = post(
        &app,
        &format!("/api/superadmin/drives/{drive}/move"),
        &admin,
        Some(json!({ "backend_id": backend_id })),
    )
    .await;
    assert_eq!(moved.status_code(), 202, "{}", moved.text());
    let mut done = false;
    for _ in 0..200 {
        let drives = drives_of(&app, &admin).await;
        if drives["Projects"]["moving_to"].is_null()
            && drives["Projects"]["backend"]["kind"] == "s3"
        {
            done = true;
            break;
        }
        tokio::time::sleep(std::time::Duration::from_millis(25)).await;
    }
    assert!(done, "the move did not finish");
    let base = format!("organizer/{drive}/");
    let keys = objects(&op, &base).await;
    assert!(keys.contains(&format!("{base}plan.txt")), "{keys:?}");
    assert!(
        keys.contains(&format!("{base}.organizer/trash/{trashed_id}")),
        "{keys:?}"
    );
    assert!(!folder.exists());
    assert_eq!(
        download_text(&app, &admin, &file_id).await.text(),
        "the plan"
    );
    assert_eq!(
        app.state.files.scan(drive_id).await.unwrap(),
        Default::default()
    );
    let restored = post(
        &app,
        &format!("/api/trash/{trashed_id}/restore"),
        &admin,
        None,
    )
    .await;
    assert_eq!(restored.status_code(), 200, "{}", restored.text());

    // A move back to the disk that a stop cut short: its row is there,
    // part of the drive copied to its folder. The drive is read-only.
    sqlx::query(
        "INSERT INTO drive_moves (drive_id, from_backend, to_backend) VALUES (?1, ?2, NULL)",
    )
    .bind(drive_id)
    .bind(uuid::Uuid::parse_str(&backend_id).unwrap())
    .execute(app.state.db.writer())
    .await
    .unwrap();
    app.state.files.forget(drive_id).await;
    std::fs::create_dir_all(&folder).unwrap();
    std::fs::write(folder.join("plan.txt"), "the plan").unwrap();
    let refused = upload(&app, &admin, &root, "new.txt", "new").await;
    assert_eq!(refused.status_code(), 409);
    assert!(refused.text().contains("read-only"));
    // A client knows it by its code, not by its words.
    assert_eq!(json(refused).await["code"], "read_only");
    let drives = drives_of(&app, &admin).await;
    assert_eq!(drives["Projects"]["moving_to"]["kind"], "disk");
    // Being moved is not being unavailable.
    assert_eq!(drives["Projects"]["available"], json!(true));
    assert_eq!(drives["Projects"]["unavailable"], Value::Null);
    // Reads go on.
    assert_eq!(
        download_text(&app, &admin, &file_id).await.text(),
        "the plan"
    );
    // Carried on (as at the next start): finished, the bucket's copy gone.
    app.state.files.move_once(drive_id).await.unwrap();
    let drives = drives_of(&app, &admin).await;
    assert_eq!(drives["Projects"]["backend"]["kind"], "disk");
    assert_eq!(drives["Projects"]["moving_to"], Value::Null);
    assert!(objects(&op, &base).await.is_empty());
    assert!(folder.join("old.txt").is_file());
    assert_eq!(
        download_text(&app, &admin, &file_id).await.text(),
        "the plan"
    );
    assert_eq!(
        upload(&app, &admin, &root, "new.txt", "new")
            .await
            .status_code(),
        201
    );
}

/// A folder move on S3 that a stop cut short (some objects copied, the
/// index not changed) is finished at the next start, and the folder goes
/// where it was going, keeping its id.
#[tokio::test]
async fn an_interrupted_folder_move_on_s3_is_finished_at_start() {
    let app = spawn_app().await;
    let admin = admin_session(&app).await;
    let op = stand_in(&app, "https://s3.example.com");
    let backend = global_backend(&app, &admin, "S3", "https://s3.example.com", json!({})).await;
    let (drive, root) = drive_on(&app, &admin, "Big", backend["id"].as_str().unwrap()).await;
    let drive_id = uuid::Uuid::parse_str(&drive).unwrap();
    let folder = json(
        post(
            &app,
            &format!("/api/files/{root}/folders"),
            &admin,
            Some(json!({ "name": "A" })),
        )
        .await,
    )
    .await;
    let folder_id = uuid::Uuid::parse_str(folder["id"].as_str().unwrap()).unwrap();
    for name in ["one.txt", "two.txt", "three.txt"] {
        upload(&app, &admin, &folder_id.to_string(), name, name).await;
    }
    let base = format!("organizer/{drive}/");
    // The intent, then one object copied and its original deleted.
    let root_id = uuid::Uuid::parse_str(&root).unwrap();
    organizer::files::journal::begin(
        &app.state.files,
        drive_id,
        "move",
        Some(folder_id),
        Some("A"),
        Some("B"),
        Some((root_id, "B")),
    )
    .await
    .unwrap();
    op.write(&format!("{base}B/one.txt"), b"one.txt".to_vec())
        .await
        .unwrap();
    op.delete(&format!("{base}A/one.txt")).await.unwrap();

    organizer::files::journal::recover(&app.state.files, None)
        .await
        .unwrap();
    assert_eq!(
        objects(&op, &base).await,
        [
            format!("{base}B/one.txt"),
            format!("{base}B/three.txt"),
            format!("{base}B/two.txt")
        ]
    );
    let node = app.state.files.node(drive_id, folder_id).await.unwrap();
    assert_eq!(node.name, "B");
    assert_eq!(
        app.state.files.scan(drive_id).await.unwrap(),
        Default::default()
    );
    let mut names = names_in(&app, &admin, &folder_id.to_string()).await;
    names.sort();
    assert_eq!(names, ["one.txt", "three.txt", "two.txt"]);
}

/// The server's backups cover the drives on its disk, not those in a
/// storage backend.
#[tokio::test]
async fn backups_leave_out_drives_in_backends() {
    let app = spawn_app().await;
    let admin = admin_session(&app).await;
    stand_in(&app, "https://s3.example.com");
    let backend = global_backend(&app, &admin, "S3", "https://s3.example.com", json!({})).await;
    let (on_disk, _) = shared_drive(&app, &admin, "On disk", "admin@example.com").await;
    let (in_s3, _) = drive_on(&app, &admin, "In S3", backend["id"].as_str().unwrap()).await;
    let staging = TempDir::new();
    let (_, manifest) =
        organizer::backup::run::sources(&app.state, &staging.0, staging.0.join("db.sqlite"))
            .await
            .unwrap();
    let drives: Vec<String> = manifest.drives.keys().map(|d| d.to_string()).collect();
    assert_eq!(drives, [on_disk]);
    assert!(!drives.contains(&in_s3));
}

// ---------------------------------------------------------------------------
// Mirrors (docs/M7D_DESIGN.md): read-only copies of drives in users' own
// storage, on the in-memory stand-in for S3. The loop that copies them is
// not running in the tests: they look when they want to
// (`Files::run_mirrors`).
// ---------------------------------------------------------------------------

/// Users' own storage allowed, and a backend of `session`'s on a stand-in
/// at `endpoint`: its id and the stand-in.
async fn own_backend(
    app: &TestApp,
    admin: &str,
    session: &str,
    endpoint: &str,
) -> (String, opendal::Operator) {
    let on = request(
        app,
        Method::PUT,
        "/api/superadmin/settings/storage.own_backends",
        Some(admin),
        Some(json!({ "mode": "default", "value": true })),
    )
    .await;
    assert_eq!(on.status_code(), 200, "{}", on.text());
    let op = stand_in(app, endpoint);
    let made = post(
        app,
        "/api/backends",
        session,
        Some(backend_body("Mine", endpoint, json!({}))),
    )
    .await;
    assert_eq!(made.status_code(), 201, "{}", made.text());
    (json(made).await["id"].as_str().unwrap().to_string(), op)
}

async fn make_mirror(
    app: &TestApp,
    session: &str,
    drive: &str,
    backend: &str,
    schedule: &str,
) -> Value {
    let made = post(
        app,
        "/api/mirrors",
        session,
        Some(json!({ "drive_id": drive, "backend_id": backend, "schedule": schedule })),
    )
    .await;
    assert_eq!(made.status_code(), 201, "{}", made.text());
    json(made).await
}

async fn mirror_view(app: &TestApp, session: &str, id: &str) -> Value {
    let response = request(
        app,
        Method::GET,
        &format!("/api/mirrors/{id}"),
        Some(session),
        None,
    )
    .await;
    assert_eq!(response.status_code(), 200, "{}", response.text());
    json(response).await
}

/// Everything under a folder as the user sees it, by path (a folder's
/// ending in `/`), sorted.
async fn paths_under(app: &TestApp, session: &str, folder: &str) -> Vec<String> {
    let mut out = Vec::new();
    let mut todo = vec![(folder.to_string(), String::new())];
    while let Some((id, at)) = todo.pop() {
        let response = request(
            app,
            Method::GET,
            &format!("/api/files/{id}/children"),
            Some(session),
            None,
        )
        .await;
        assert_eq!(response.status_code(), 200, "{}", response.text());
        for item in json(response).await["items"].as_array().unwrap() {
            let name = item["name"].as_str().unwrap();
            let path = if at.is_empty() {
                name.to_string()
            } else {
                format!("{at}/{name}")
            };
            if item["kind"] == "folder" {
                todo.push((item["id"].as_str().unwrap().to_string(), path.clone()));
                out.push(format!("{path}/"));
            } else {
                out.push(path);
            }
        }
    }
    out.sort();
    out
}

/// A child of a folder by name: the node.
async fn child(app: &TestApp, session: &str, folder: &str, name: &str) -> Value {
    let response = request(
        app,
        Method::GET,
        &format!("/api/files/{folder}/children"),
        Some(session),
        None,
    )
    .await;
    assert_eq!(response.status_code(), 200, "{}", response.text());
    json(response).await["items"]
        .as_array()
        .unwrap()
        .iter()
        .find(|n| n["name"] == name)
        .unwrap_or_else(|| panic!("no {name}"))
        .clone()
}

/// The user's mirrors among their drives.
async fn mirror_drives(app: &TestApp, session: &str) -> Vec<Value> {
    let response = request(app, Method::GET, "/api/drives", Some(session), None).await;
    assert_eq!(response.status_code(), 200, "{}", response.text());
    json(response)
        .await
        .as_array()
        .unwrap()
        .iter()
        .filter(|d| d["kind"] == "mirror")
        .cloned()
        .collect()
}

/// Set a mirror's columns as a stop or the passing time would leave them.
async fn mirror_sql(app: &TestApp, sql: &str, id: &str) {
    sqlx::query(sqlx::AssertSqlSafe(sql.to_string()))
        .bind(uuid::Uuid::parse_str(id).unwrap())
        .execute(app.state.db.writer())
        .await
        .unwrap();
}

/// My drive is mirrored to its user's own storage as plain objects at
/// `<prefix>/<mirror id>/`, and listed among their drives, read-only. A
/// continuous mirror copies a change at the next look; an hourly or daily
/// one when an hour or a day passed since its last copy; a manual one only
/// when asked.
#[tokio::test]
async fn mirrors_copy_on_their_schedule() {
    let app = spawn_app().await;
    let admin = admin_session(&app).await;
    let (alice, alice_id) = invited_user(&app, &admin, "alice@example.com").await;
    let (backend, op) = own_backend(&app, &admin, &alice, "https://8.8.4.4").await;
    let root = root_id(&app, &alice).await;
    assert_eq!(
        upload(&app, &alice, &root, "a.txt", "alpha")
            .await
            .status_code(),
        201
    );
    let docs = json(new_folder(&app, &alice, &root, "Docs").await).await["id"]
        .as_str()
        .unwrap()
        .to_string();
    upload(&app, &alice, &docs, "b.txt", "beta").await;

    let mirror = make_mirror(&app, &alice, &alice_id, &backend, "continuous").await;
    let id = mirror["id"].as_str().unwrap().to_string();
    assert_eq!(mirror["status"], "pending");
    assert_eq!(mirror["drive_id"], alice_id.as_str());
    assert_eq!(mirror["drive_name"], "My drive");
    assert_eq!(mirror["drive_kind"], "my");
    assert_eq!(mirror["schedule"], "continuous");
    assert_eq!(mirror["bucket"], "bucket");
    assert_eq!(mirror["location"], format!("s3://bucket/organizer/{id}/"));
    assert_eq!(mirror["restores_into_drive"], true);
    app.state.files.run_mirrors().await;
    let view = mirror_view(&app, &alice, &id).await;
    assert_eq!(view["status"], "up_to_date", "{view}");
    assert!(view["last_copy_at"].is_string());
    let base = format!("organizer/{id}/");
    assert_eq!(
        objects(&op, &base).await,
        [format!("{base}Docs/b.txt"), format!("{base}a.txt")]
    );
    let listed = json(request(&app, Method::GET, "/api/mirrors", Some(&alice), None).await).await;
    assert_eq!(listed.as_array().unwrap().len(), 1);

    // Among her drives, after My drive: read-only.
    let drives = mirror_drives(&app, &alice).await;
    assert_eq!(drives.len(), 1);
    let drive = &drives[0];
    assert_eq!(drive["id"], id.as_str());
    assert_eq!(drive["name"], "My drive");
    assert_eq!(drive["my_role"], "viewer");
    assert_eq!(drive["manages"], false);
    assert_eq!(drive["mirror_of"]["drive_id"], alice_id.as_str());
    assert_eq!(drive["mirror_of"]["status"], "up_to_date");
    assert_eq!(drive["backend"]["owner"], "alice@example.com");
    let mirror_root = drive["root_id"].as_str().unwrap().to_string();
    assert_eq!(mirror["root_id"], mirror_root.as_str());
    assert_eq!(
        paths_under(&app, &alice, &mirror_root).await,
        ["Docs/", "Docs/b.txt", "a.txt"]
    );
    let a_copy = child(&app, &alice, &mirror_root, "a.txt").await;
    assert_eq!(a_copy["my_role"], "viewer");
    let a = child(&app, &alice, &root, "a.txt").await;
    assert_eq!(a_copy["source_id"], a["id"]);
    assert_eq!(a["source_id"], json!(null));
    let a_copy = a_copy["id"].as_str().unwrap().to_string();
    assert_eq!(download_text(&app, &alice, &a_copy).await.text(), "alpha");
    assert_eq!(
        upload(&app, &alice, &mirror_root, "x.txt", "x")
            .await
            .status_code(),
        403
    );
    assert_eq!(delete_node(&app, &alice, &a_copy).await.status_code(), 403);
    assert_eq!(
        rename(&app, &alice, &a_copy, "b.txt").await.status_code(),
        403
    );
    // Continuous: a change is copied at the next look.
    upload(&app, &alice, &root, "c.txt", "gamma").await;
    assert_eq!(mirror_view(&app, &alice, &id).await["status"], "pending");
    app.state.files.run_mirrors().await;
    assert!(
        paths_under(&app, &alice, &mirror_root)
            .await
            .contains(&"c.txt".to_string())
    );
    assert_eq!(mirror_view(&app, &alice, &id).await["status"], "up_to_date");

    // Hourly: not within the hour after the last copy.
    let changed = request(
        &app,
        Method::PATCH,
        &format!("/api/mirrors/{id}"),
        Some(&alice),
        Some(json!({ "schedule": "hourly" })),
    )
    .await;
    assert_eq!(changed.status_code(), 200, "{}", changed.text());
    assert_eq!(json(changed).await["schedule"], "hourly");
    upload(&app, &alice, &root, "d.txt", "delta").await;
    app.state.files.run_mirrors().await;
    let copied = paths_under(&app, &alice, &mirror_root).await;
    assert!(!copied.contains(&"d.txt".to_string()));
    assert_eq!(mirror_view(&app, &alice, &id).await["status"], "pending");
    mirror_sql(
        &app,
        "UPDATE mirrors SET last_copy_at = strftime('%Y-%m-%dT%H:%M:%fZ', 'now', '-61 minutes')
         WHERE id = ?1",
        &id,
    )
    .await;
    app.state.files.run_mirrors().await;
    assert!(
        paths_under(&app, &alice, &mirror_root)
            .await
            .contains(&"d.txt".to_string())
    );

    // Daily: an hour is not enough, a day is.
    request(
        &app,
        Method::PATCH,
        &format!("/api/mirrors/{id}"),
        Some(&alice),
        Some(json!({ "schedule": "daily" })),
    )
    .await;
    upload(&app, &alice, &root, "e.txt", "epsilon").await;
    mirror_sql(
        &app,
        "UPDATE mirrors SET last_copy_at = strftime('%Y-%m-%dT%H:%M:%fZ', 'now', '-2 hours')
         WHERE id = ?1",
        &id,
    )
    .await;
    app.state.files.run_mirrors().await;
    assert!(
        !paths_under(&app, &alice, &mirror_root)
            .await
            .contains(&"e.txt".to_string())
    );
    mirror_sql(
        &app,
        "UPDATE mirrors SET last_copy_at = strftime('%Y-%m-%dT%H:%M:%fZ', 'now', '-25 hours')
         WHERE id = ?1",
        &id,
    )
    .await;
    app.state.files.run_mirrors().await;
    assert!(
        paths_under(&app, &alice, &mirror_root)
            .await
            .contains(&"e.txt".to_string())
    );

    // Manual: only when asked ("Mirror now").
    request(
        &app,
        Method::PATCH,
        &format!("/api/mirrors/{id}"),
        Some(&alice),
        Some(json!({ "schedule": "manual" })),
    )
    .await;
    upload(&app, &alice, &root, "f.txt", "phi").await;
    mirror_sql(
        &app,
        "UPDATE mirrors SET last_copy_at = strftime('%Y-%m-%dT%H:%M:%fZ', 'now', '-30 days')
         WHERE id = ?1",
        &id,
    )
    .await;
    app.state.files.run_mirrors().await;
    assert!(
        !paths_under(&app, &alice, &mirror_root)
            .await
            .contains(&"f.txt".to_string())
    );
    let asked = post(&app, &format!("/api/mirrors/{id}/run"), &alice, None).await;
    assert_eq!(asked.status_code(), 202, "{}", asked.text());
    assert_eq!(json(asked).await["status"], "pending");
    app.state.files.run_mirrors().await;
    assert_eq!(
        paths_under(&app, &alice, &mirror_root).await,
        [
            "Docs/",
            "Docs/b.txt",
            "a.txt",
            "c.txt",
            "d.txt",
            "e.txt",
            "f.txt"
        ]
    );
    assert_eq!(mirror_view(&app, &alice, &id).await["status"], "up_to_date");

    // Someone else's mirror is not found; removing one deletes its copy.
    let (bob, _) = invited_user(&app, &admin, "bob@example.com").await;
    assert_eq!(
        status_of(&app, &bob, &format!("/api/mirrors/{id}")).await,
        404
    );
    let removed = request(
        &app,
        Method::DELETE,
        &format!("/api/mirrors/{id}"),
        Some(&alice),
        None,
    )
    .await;
    assert_eq!(removed.status_code(), 204);
    assert!(objects(&op, &base).await.is_empty());
    assert!(mirror_drives(&app, &alice).await.is_empty());
    assert_eq!(
        paths_under(&app, &alice, &root).await,
        [
            "Docs/",
            "Docs/b.txt",
            "a.txt",
            "c.txt",
            "d.txt",
            "e.txt",
            "f.txt"
        ]
    );
}

/// A change in the drive goes to the mirror's versions, a delete to its
/// trash, a rename or a move is a move (the copy keeps its id); they are
/// kept by the mirroring user's retention, not the defaults.
#[tokio::test]
async fn deletes_and_changes_go_to_the_mirrors_trash_and_versions() {
    let app = spawn_app().await;
    let admin = admin_session(&app).await;
    let (alice, alice_id) = invited_user(&app, &admin, "alice@example.com").await;
    let (backend, op) = own_backend(&app, &admin, &alice, "https://8.8.4.4").await;
    let root = root_id(&app, &alice).await;
    let a = json(upload(&app, &alice, &root, "a.txt", "alpha").await).await["id"]
        .as_str()
        .unwrap()
        .to_string();
    let old = json(upload(&app, &alice, &root, "old.txt", "old").await).await["id"]
        .as_str()
        .unwrap()
        .to_string();
    let docs = json(new_folder(&app, &alice, &root, "Docs").await).await["id"]
        .as_str()
        .unwrap()
        .to_string();
    upload(&app, &alice, &docs, "b.txt", "beta").await;
    let mirror = make_mirror(&app, &alice, &alice_id, &backend, "continuous").await;
    let id = mirror["id"].as_str().unwrap().to_string();
    let mirror_root = mirror["root_id"].as_str().unwrap().to_string();
    app.state.files.run_mirrors().await;
    let docs_copy = child(&app, &alice, &mirror_root, "Docs").await["id"].clone();

    replace(&app, &alice, &a, "alpha 2").await;
    assert_eq!(
        rename(&app, &alice, &docs, "Papers").await.status_code(),
        200
    );
    assert_eq!(delete_node(&app, &alice, &old).await.status_code(), 200);
    app.state.files.run_mirrors().await;
    assert_eq!(
        paths_under(&app, &alice, &mirror_root).await,
        ["Papers/", "Papers/b.txt", "a.txt"]
    );
    assert_eq!(
        child(&app, &alice, &mirror_root, "Papers").await["id"],
        docs_copy
    );
    let base = format!("organizer/{id}/");
    let objects_now = objects(&op, &base).await;
    assert!(objects_now.contains(&format!("{base}Papers/b.txt")));
    assert!(!objects_now.contains(&format!("{base}Docs/b.txt")));
    // The change: the copy's earlier content is a version.
    let a_copy = child(&app, &alice, &mirror_root, "a.txt").await["id"]
        .as_str()
        .unwrap()
        .to_string();
    assert_eq!(download_text(&app, &alice, &a_copy).await.text(), "alpha 2");
    let versions = versions_of(&app, &alice, &a_copy).await;
    assert_eq!(versions.len(), 1);
    let version = versions[0]["id"].as_str().unwrap();
    let was = request(
        &app,
        Method::GET,
        &format!("/api/files/{a_copy}/versions/{version}/content"),
        Some(&alice),
        None,
    )
    .await;
    assert_eq!(was.text(), "alpha");
    // The delete: in the mirror's trash.
    let trash = json(
        request(
            &app,
            Method::GET,
            &format!("/api/trash?drive={id}"),
            Some(&alice),
            None,
        )
        .await,
    )
    .await;
    assert_eq!(trash.as_array().unwrap().len(), 1, "{trash}");
    assert_eq!(trash[0]["name"], "old.txt");
    assert_eq!(trash[0]["my_role"], "viewer");
    let item = trash[0]["id"].as_str().unwrap().to_string();
    // Hers to keep or empty by her rules, not to change by hand.
    assert_eq!(
        post(&app, &format!("/api/trash/{item}/restore"), &alice, None)
            .await
            .status_code(),
        403
    );

    // Her rules: the trash kept until emptied, no versions.
    for (key, value) in [
        ("retention.trash_days", 0),
        ("retention.versions_hours", 0),
        ("retention.versions_days", 0),
        ("retention.versions_months", 0),
    ] {
        let set = request(
            &app,
            Method::PUT,
            &format!("/api/settings/{key}"),
            Some(&alice),
            Some(json!({ "value": value })),
        )
        .await;
        assert_eq!(set.status_code(), 200, "{}", set.text());
    }
    mirror_sql(
        &app,
        "UPDATE trash SET deleted_at = '2000-01-01T00:00:00.000Z' WHERE drive_id = ?1",
        &id,
    )
    .await;
    organizer::retention::sweep(&app.state).await.unwrap();
    let left = json(
        request(
            &app,
            Method::GET,
            &format!("/api/trash?drive={id}"),
            Some(&alice),
            None,
        )
        .await,
    )
    .await;
    assert_eq!(left.as_array().unwrap().len(), 1, "kept until emptied");
    assert!(versions_of(&app, &alice, &a_copy).await.is_empty());
    // A week, then: what is older goes.
    let set = request(
        &app,
        Method::PUT,
        "/api/settings/retention.trash_days",
        Some(&alice),
        Some(json!({ "value": 7 })),
    )
    .await;
    assert_eq!(set.status_code(), 200);
    organizer::retention::sweep(&app.state).await.unwrap();
    let left = json(
        request(
            &app,
            Method::GET,
            &format!("/api/trash?drive={id}"),
            Some(&alice),
            None,
        )
        .await,
    )
    .await;
    assert!(left.as_array().unwrap().is_empty());
    assert!(
        !objects(&op, &base)
            .await
            .iter()
            .any(|k| k.contains(&format!(".organizer/trash/{item}")))
    );
}

/// What is copied is what the mirroring user sees: not a limited folder
/// they are not in, not others' uploads where they may only upload, not
/// the shares placed in their My drive; upload only may not mirror at
/// all. An encrypted folder is copied as the server holds it, each copy
/// knowing the id its name is sealed with.
#[tokio::test]
async fn mirrors_copy_only_what_the_user_sees() {
    let app = spawn_app().await;
    let admin = admin_session(&app).await;
    let (alice, alice_id) = invited_user(&app, &admin, "alice@example.com").await;
    let (bob, bob_id) = invited_user(&app, &admin, "bob@example.com").await;
    let (carol, carol_id) = invited_user(&app, &admin, "carol@example.com").await;
    let on = request(
        &app,
        Method::PUT,
        "/api/superadmin/settings/mirrors.shared_drives",
        Some(&admin),
        Some(json!({ "mode": "default", "value": true })),
    )
    .await;
    assert_eq!(on.status_code(), 200, "{}", on.text());
    let (drive, drive_root) = shared_drive(&app, &admin, "Team", "alice@example.com").await;
    grant_drive(&app, &alice, &drive, &bob_id, "viewer").await;
    grant_drive(&app, &alice, &drive, &carol_id, "upload_only").await;
    upload(&app, &alice, &drive_root, "open.txt", "for all").await;
    let secret = json(new_folder(&app, &alice, &drive_root, "Secret").await).await["id"]
        .as_str()
        .unwrap()
        .to_string();
    upload(&app, &alice, &secret, "plan.txt", "hidden").await;
    let limited = request(
        &app,
        Method::PATCH,
        &format!("/api/files/{secret}/access"),
        Some(&alice),
        Some(json!({ "limited": true })),
    )
    .await;
    assert_eq!(limited.status_code(), 200, "{}", limited.text());
    // A limited folder where bob may only upload: his own uploads only.
    let inbox = json(new_folder(&app, &alice, &drive_root, "Inbox").await).await["id"]
        .as_str()
        .unwrap()
        .to_string();
    upload(&app, &alice, &inbox, "theirs.txt", "not bob's").await;
    let limited = request(
        &app,
        Method::PATCH,
        &format!("/api/files/{inbox}/access"),
        Some(&alice),
        Some(json!({ "limited": true })),
    )
    .await;
    assert_eq!(limited.status_code(), 200);
    assert_eq!(
        grant_item(&app, &alice, &inbox, &bob_id, "upload_only")
            .await
            .status_code(),
        201
    );
    assert_eq!(
        upload(&app, &bob, &inbox, "bobs.txt", "his")
            .await
            .status_code(),
        201
    );

    // Upload only may not mirror.
    let (carol_backend, _) = own_backend(&app, &admin, &carol, "https://8.8.8.4").await;
    let refused = post(
        &app,
        "/api/mirrors",
        &carol,
        Some(json!({ "drive_id": drive, "backend_id": carol_backend })),
    )
    .await;
    assert_eq!(refused.status_code(), 403);
    assert_eq!(json(refused).await["code"], "access_lost");
    // Nor someone else's My drive (not hers: not found), nor to a global
    // storage.
    let refused = post(
        &app,
        "/api/mirrors",
        &carol,
        Some(json!({ "drive_id": alice_id, "backend_id": carol_backend })),
    )
    .await;
    assert_eq!(refused.status_code(), 404);
    stand_in(&app, "https://s3.example.com");
    let global = global_backend(&app, &admin, "S3", "https://s3.example.com", json!({})).await;
    let refused = post(
        &app,
        "/api/mirrors",
        &bob,
        Some(json!({ "drive_id": drive, "backend_id": global["id"] })),
    )
    .await;
    assert_eq!(refused.status_code(), 400);

    let (bob_backend, op) = own_backend(&app, &admin, &bob, "https://8.8.4.4").await;
    let mirror = make_mirror(&app, &bob, &drive, &bob_backend, "continuous").await;
    let id = mirror["id"].as_str().unwrap().to_string();
    assert_eq!(mirror["restores_into_drive"], false, "a viewer");
    app.state.files.run_mirrors().await;
    let mirror_root = mirror["root_id"].as_str().unwrap().to_string();
    assert_eq!(
        paths_under(&app, &bob, &mirror_root).await,
        ["Inbox/", "Inbox/bobs.txt", "open.txt"]
    );
    let base = format!("organizer/{id}/");
    assert_eq!(
        objects(&op, &base).await,
        [format!("{base}Inbox/bobs.txt"), format!("{base}open.txt")]
    );
    // The drive's managers see who mirrors it.
    let access = json(
        request(
            &app,
            Method::GET,
            &format!("/api/drives/{drive}/access"),
            Some(&alice),
            None,
        )
        .await,
    )
    .await;
    assert_eq!(access["mirrored_by"][0]["user"]["email"], "bob@example.com");

    // Bob's My drive: a share placed in it is not in its mirror; an
    // encrypted folder is, sealed.
    let alices_root = root_id(&app, &alice).await;
    let shared = json(new_folder(&app, &alice, &alices_root, "Shared").await).await["id"]
        .as_str()
        .unwrap()
        .to_string();
    upload(&app, &alice, &shared, "note.txt", "hello").await;
    assert_eq!(
        grant_item(&app, &alice, &shared, &bob_id, "editor")
            .await
            .status_code(),
        201
    );
    let bobs_root = root_id(&app, &bob).await;
    let placed = post(
        &app,
        &format!("/api/shared/{shared}/place"),
        &bob,
        Some(json!({ "parent_id": bobs_root })),
    )
    .await;
    assert_eq!(placed.status_code(), 201, "{}", placed.text());
    assert_eq!(names_in(&app, &bob, &bobs_root).await, ["Shared"]);
    assert_eq!(set_up_encryption(&app, &bob).await.status_code(), 201);
    let sealed = encrypted_folder(&app, &bob, &bobs_root, "Vault").await;
    let sealed_file = json(upload(&app, &bob, &sealed, &opaque(41, 1), "ciphertext").await).await;
    upload(&app, &bob, &bobs_root, "mine.txt", "bob's").await;
    let mine = make_mirror(&app, &bob, &bob_id, &bob_backend, "continuous").await;
    app.state.files.run_mirrors().await;
    let my_mirror_root = mine["root_id"].as_str().unwrap().to_string();
    assert_eq!(
        paths_under(&app, &bob, &my_mirror_root).await,
        [
            "Vault/".to_string(),
            format!("Vault/{}", opaque(41, 1)),
            "mine.txt".to_string()
        ]
    );
    let vault_copy = child(&app, &bob, &my_mirror_root, "Vault").await;
    assert_eq!(vault_copy["source_id"], sealed.as_str());
    assert_eq!(vault_copy["e2e_root"], vault_copy["id"]);
    let file_copy = child(
        &app,
        &bob,
        vault_copy["id"].as_str().unwrap(),
        &opaque(41, 1),
    )
    .await;
    assert_eq!(file_copy["source_id"], sealed_file["id"]);
    assert_eq!(file_copy["e2e_root"], vault_copy["id"]);
    assert_eq!(
        download_text(&app, &bob, file_copy["id"].as_str().unwrap())
            .await
            .text(),
        "ciphertext"
    );
}

/// Shared drives are mirrored only once the superadmin (or the drive's
/// team) allows it; a drive's managers may forbid it for their drive,
/// which stops the mirrors of it (they stay, stopped) until they allow it
/// again.
#[tokio::test]
async fn shared_drive_mirroring_follows_the_settings() {
    let app = spawn_app().await;
    let admin = admin_session(&app).await;
    let (alice, _) = invited_user(&app, &admin, "alice@example.com").await;
    let (bob, bob_id) = invited_user(&app, &admin, "bob@example.com").await;
    let (drive, drive_root) = shared_drive(&app, &admin, "Team", "alice@example.com").await;
    grant_drive(&app, &alice, &drive, &bob_id, "editor").await;
    upload(&app, &alice, &drive_root, "one.txt", "1").await;
    let (backend, _) = own_backend(&app, &admin, &bob, "https://8.8.4.4").await;
    // Off until the superadmin enables it.
    let refused = post(
        &app,
        "/api/mirrors",
        &bob,
        Some(json!({ "drive_id": drive, "backend_id": backend })),
    )
    .await;
    assert_eq!(refused.status_code(), 403);
    assert_eq!(json(refused).await["code"], "shared_drives_off");
    let on = request(
        &app,
        Method::PUT,
        "/api/superadmin/settings/mirrors.shared_drives",
        Some(&admin),
        Some(json!({ "mode": "default", "value": true })),
    )
    .await;
    assert_eq!(on.status_code(), 200, "{}", on.text());
    let mirror = make_mirror(&app, &bob, &drive, &backend, "continuous").await;
    let id = mirror["id"].as_str().unwrap().to_string();
    let mirror_root = mirror["root_id"].as_str().unwrap().to_string();
    assert_eq!(mirror["restores_into_drive"], true, "an editor");
    app.state.files.run_mirrors().await;
    assert_eq!(names_in(&app, &bob, &mirror_root).await, ["one.txt"]);
    let mirrored_by = |session: String| {
        let (app, drive) = (&app, drive.clone());
        async move {
            json(
                request(
                    app,
                    Method::GET,
                    &format!("/api/drives/{drive}/access"),
                    Some(&session),
                    None,
                )
                .await,
            )
            .await["mirrored_by"]
                .clone()
        }
    };
    let by = mirrored_by(bob.clone()).await;
    assert_eq!(by.as_array().unwrap().len(), 1);
    assert_eq!(by[0]["user"]["id"], bob_id.as_str());

    // The drive's managers forbid it: the mirror stops, and keeps what
    // it has.
    let settings = json(
        request(
            &app,
            Method::GET,
            &format!("/api/drives/{drive}/settings"),
            Some(&alice),
            None,
        )
        .await,
    )
    .await;
    assert!(
        settings
            .as_array()
            .unwrap()
            .iter()
            .any(|s| s["key"] == "mirrors.allowed")
    );
    let not_bob = request(
        &app,
        Method::PUT,
        &format!("/api/drives/{drive}/settings/mirrors.allowed"),
        Some(&bob),
        Some(json!({ "mode": "default", "value": false })),
    )
    .await;
    assert_eq!(not_bob.status_code(), 403, "an editor does not manage it");
    let forbid = request(
        &app,
        Method::PUT,
        &format!("/api/drives/{drive}/settings/mirrors.allowed"),
        Some(&alice),
        Some(json!({ "mode": "default", "value": false })),
    )
    .await;
    assert_eq!(forbid.status_code(), 200, "{}", forbid.text());
    let view = mirror_view(&app, &bob, &id).await;
    assert_eq!(view["status"], "stopped");
    assert_eq!(view["stopped"], "forbidden");
    assert!(view["stopped_reason"].is_string());
    upload(&app, &alice, &drive_root, "two.txt", "2").await;
    app.state.files.run_mirrors().await;
    assert_eq!(names_in(&app, &bob, &mirror_root).await, ["one.txt"]);
    let now = post(&app, &format!("/api/mirrors/{id}/run"), &bob, None).await;
    assert_eq!(now.status_code(), 409);
    assert_eq!(json(now).await["code"], "mirror_stopped");
    assert!(
        mirrored_by(alice.clone())
            .await
            .as_array()
            .unwrap()
            .is_empty()
    );
    let again = post(
        &app,
        "/api/mirrors",
        &bob,
        Some(json!({ "drive_id": drive, "backend_id": backend })),
    )
    .await;
    assert_eq!(json(again).await["code"], "mirroring_forbidden");

    // Allowed again: it goes on.
    let allow = request(
        &app,
        Method::DELETE,
        &format!("/api/drives/{drive}/settings/mirrors.allowed"),
        Some(&alice),
        None,
    )
    .await;
    assert_eq!(allow.status_code(), 200, "{}", allow.text());
    app.state.files.run_mirrors().await;
    assert_eq!(
        names_in(&app, &bob, &mirror_root).await,
        ["one.txt", "two.txt"]
    );
    // The superadmin turns shared drives' mirrors off: stopped again.
    let off = request(
        &app,
        Method::PUT,
        "/api/superadmin/settings/mirrors.shared_drives",
        Some(&admin),
        Some(json!({ "mode": "locked", "value": false })),
    )
    .await;
    assert_eq!(off.status_code(), 200);
    assert_eq!(
        mirror_view(&app, &bob, &id).await["stopped"],
        "shared_drives_off"
    );
}

/// A mirror on private credentials copies only while a session holds them
/// unlocked; it waits meanwhile, and catches up once they are.
#[tokio::test]
async fn a_mirror_on_private_credentials_waits_for_an_unlock() {
    let app = spawn_app().await;
    let admin = admin_session(&app).await;
    let (alice, alice_id) = invited_user(&app, &admin, "alice@example.com").await;
    let on = request(
        &app,
        Method::PUT,
        "/api/superadmin/settings/storage.own_backends",
        Some(&admin),
        Some(json!({ "mode": "default", "value": true })),
    )
    .await;
    assert_eq!(on.status_code(), 200);
    let endpoint = "https://9.9.9.9";
    let op = stand_in(&app, endpoint);
    assert_eq!(set_up_encryption(&app, &alice).await.status_code(), 201);
    let made = post(
        &app,
        "/api/backends",
        &alice,
        Some(backend_body(
            "Vault",
            endpoint,
            json!({ "private": true, "sealed_secret": opaque(120, 1) }),
        )),
    )
    .await;
    assert_eq!(made.status_code(), 201, "{}", made.text());
    let backend = json(made).await["id"].as_str().unwrap().to_string();
    let root = root_id(&app, &alice).await;
    upload(&app, &alice, &root, "a.txt", "alpha").await;
    let mirror = make_mirror(&app, &alice, &alice_id, &backend, "continuous").await;
    let id = mirror["id"].as_str().unwrap().to_string();
    app.state.files.run_mirrors().await;
    let base = format!("organizer/{id}/");
    assert_eq!(objects(&op, &base).await, [format!("{base}a.txt")]);

    // Signed out: locked; a change waits.
    assert_eq!(
        post(&app, "/api/auth/logout", &alice, None)
            .await
            .status_code(),
        204
    );
    app.state.files.close_locked().await;
    let alice = cookie(&password_login(&app, "alice@example.com").await);
    upload(&app, &alice, &root, "b.txt", "beta").await;
    let view = mirror_view(&app, &alice, &id).await;
    assert_eq!(view["status"], "waiting_for_unlock", "{view}");
    app.state.files.run_mirrors().await;
    assert_eq!(objects(&op, &base).await, [format!("{base}a.txt")]);
    let asked = post(&app, &format!("/api/mirrors/{id}/run"), &alice, None).await;
    assert_eq!(asked.status_code(), 202);
    assert_eq!(json(asked).await["status"], "waiting_for_unlock");
    app.state.files.run_mirrors().await;
    assert_eq!(objects(&op, &base).await, [format!("{base}a.txt")]);

    // Unlocked: it catches up.
    let unlocked = request(
        &app,
        Method::PUT,
        &format!("/api/backends/{backend}/unlock"),
        Some(&alice),
        Some(json!({ "secret": "the-secret-access-key" })),
    )
    .await;
    assert_eq!(unlocked.status_code(), 200, "{}", unlocked.text());
    app.state.files.run_mirrors().await;
    assert_eq!(
        objects(&op, &base).await,
        [format!("{base}a.txt"), format!("{base}b.txt")]
    );
    assert_eq!(mirror_view(&app, &alice, &id).await["status"], "up_to_date");
}

/// A mirror stays when its user loses the drive, and when the drive is
/// deleted: it stops, says why, and keeps what it has to browse.
#[tokio::test]
async fn a_mirror_stays_when_access_is_lost_or_the_drive_deleted() {
    let app = spawn_app().await;
    let admin = admin_session(&app).await;
    let (alice, _) = invited_user(&app, &admin, "alice@example.com").await;
    let (bob, bob_id) = invited_user(&app, &admin, "bob@example.com").await;
    let on = request(
        &app,
        Method::PUT,
        "/api/superadmin/settings/mirrors.shared_drives",
        Some(&admin),
        Some(json!({ "mode": "default", "value": true })),
    )
    .await;
    assert_eq!(on.status_code(), 200);
    let (drive, drive_root) = shared_drive(&app, &admin, "Team", "alice@example.com").await;
    let grant = grant_drive(&app, &alice, &drive, &bob_id, "viewer").await;
    upload(&app, &alice, &drive_root, "one.txt", "1").await;
    let (backend, op) = own_backend(&app, &admin, &bob, "https://8.8.4.4").await;
    let mirror = make_mirror(&app, &bob, &drive, &backend, "continuous").await;
    let id = mirror["id"].as_str().unwrap().to_string();
    let mirror_root = mirror["root_id"].as_str().unwrap().to_string();
    app.state.files.run_mirrors().await;
    assert_eq!(
        mirror_view(&app, &bob, &id).await["stopped_at"],
        json!(null)
    );

    // Access lost: stopped since the first look that saw it (the loop's).
    let removed = request(
        &app,
        Method::DELETE,
        &format!("/api/drives/{drive}/access/{grant}"),
        Some(&alice),
        None,
    )
    .await;
    assert_eq!(removed.status_code(), 204, "{}", removed.text());
    let before = organizer::auth::now_iso();
    app.state.files.run_mirrors().await;
    let view = mirror_view(&app, &bob, &id).await;
    assert_eq!(view["status"], "stopped");
    assert_eq!(view["stopped"], "access_lost");
    assert_eq!(view["restores_into_drive"], false);
    let lost_at = view["stopped_at"].as_str().expect("when").to_string();
    assert!(lost_at >= before, "{lost_at} {before}");
    upload(&app, &alice, &drive_root, "two.txt", "2").await;
    app.state.files.run_mirrors().await;
    assert_eq!(names_in(&app, &bob, &mirror_root).await, ["one.txt"]);
    let drives = mirror_drives(&app, &bob).await;
    assert_eq!(drives[0]["mirror_of"]["stopped_at"], lost_at.as_str());

    // Access given back: it goes on, no longer stopped.
    let grant = grant_drive(&app, &alice, &drive, &bob_id, "viewer").await;
    app.state.files.run_mirrors().await;
    let view = mirror_view(&app, &bob, &id).await;
    assert_ne!(view["status"], "stopped");
    assert_eq!(view["stopped_at"], json!(null));
    assert_eq!(
        names_in(&app, &bob, &mirror_root).await,
        ["one.txt", "two.txt"]
    );
    // Lost again: a stop of its own.
    tokio::time::sleep(std::time::Duration::from_millis(5)).await;
    let removed = request(
        &app,
        Method::DELETE,
        &format!("/api/drives/{drive}/access/{grant}"),
        Some(&alice),
        None,
    )
    .await;
    assert_eq!(removed.status_code(), 204, "{}", removed.text());
    let view = mirror_view(&app, &bob, &id).await;
    let lost_again = view["stopped_at"].as_str().expect("when").to_string();
    assert!(lost_again > lost_at);
    tokio::time::sleep(std::time::Duration::from_millis(5)).await;

    // The drive deleted: the mirror keeps what it had, stopped since then.
    let deleted = request(
        &app,
        Method::DELETE,
        &format!("/api/superadmin/drives/{drive}"),
        Some(&admin),
        None,
    )
    .await;
    assert_eq!(deleted.status_code(), 204);
    let view = mirror_view(&app, &bob, &id).await;
    assert_eq!(view["stopped"], "drive_deleted");
    assert_eq!(view["drive_id"], json!(null));
    assert_eq!(view["drive_name"], "Team");
    let deleted_at = view["stopped_at"].as_str().expect("when").to_string();
    assert!(deleted_at > lost_again, "the deletion's time");
    let drives = mirror_drives(&app, &bob).await;
    assert_eq!(drives[0]["mirror_of"]["status"], "stopped");
    assert_eq!(drives[0]["mirror_of"]["stopped"], "drive_deleted");
    assert_eq!(drives[0]["mirror_of"]["stopped_at"], deleted_at.as_str());
    assert_eq!(
        names_in(&app, &bob, &mirror_root).await,
        ["one.txt", "two.txt"]
    );
    let base = format!("organizer/{id}/");
    assert_eq!(
        objects(&op, &base).await,
        [format!("{base}one.txt"), format!("{base}two.txt")]
    );
}

/// Eject: in place, the mirror becomes a shared drive at the same place
/// (its id, its objects), managed by its user, and mirroring stops; a
/// copy is a new drive of the mirror's contents, and the mirror goes on.
#[tokio::test]
async fn a_mirror_is_ejected_in_place_or_as_a_copy() {
    let app = spawn_app().await;
    let admin = admin_session(&app).await;
    let (alice, alice_id) = invited_user(&app, &admin, "alice@example.com").await;
    let (backend, op) = own_backend(&app, &admin, &alice, "https://8.8.4.4").await;
    let root = root_id(&app, &alice).await;
    let docs = json(new_folder(&app, &alice, &root, "Docs").await).await["id"]
        .as_str()
        .unwrap()
        .to_string();
    upload(&app, &alice, &docs, "b.txt", "beta").await;
    let first = make_mirror(&app, &alice, &alice_id, &backend, "continuous").await;
    let second = make_mirror(&app, &alice, &alice_id, &backend, "continuous").await;
    app.state.files.run_mirrors().await;

    // A copy: a new drive, and the mirror goes on.
    let first_id = first["id"].as_str().unwrap().to_string();
    let copied = post(
        &app,
        &format!("/api/mirrors/{first_id}/eject"),
        &alice,
        Some(json!({ "mode": "copy", "name": "Archive" })),
    )
    .await;
    assert_eq!(copied.status_code(), 201, "{}", copied.text());
    let copy = json(copied).await;
    assert_eq!(copy["kind"], "shared");
    assert_eq!(copy["name"], "Archive");
    assert_eq!(copy["my_role"], "manager");
    assert_eq!(copy["backend"]["owner"], "alice@example.com");
    let copy_root = copy["root_id"].as_str().unwrap().to_string();
    assert_eq!(
        paths_under(&app, &alice, &copy_root).await,
        ["Docs/", "Docs/b.txt"]
    );
    let docs_in_copy = child(&app, &alice, &copy_root, "Docs").await;
    assert_eq!(docs_in_copy["source_id"], docs.as_str(), "the drive's ids");
    let copy_id = copy["id"].as_str().unwrap();
    let copy_base = format!("organizer/{copy_id}/");
    assert_eq!(
        objects(&op, &copy_base).await,
        [format!("{copy_base}Docs/b.txt")]
    );
    upload(&app, &alice, &copy_root, "new.txt", "writable").await;
    upload(&app, &alice, &root, "c.txt", "gamma").await;
    app.state.files.run_mirrors().await;
    let first_root = first["root_id"].as_str().unwrap().to_string();
    assert_eq!(
        paths_under(&app, &alice, &first_root).await,
        ["Docs/", "Docs/b.txt", "c.txt"]
    );

    // In place: the same drive, now a shared one of hers.
    let second_id = second["id"].as_str().unwrap().to_string();
    let taken = post(
        &app,
        &format!("/api/mirrors/{second_id}/eject"),
        &alice,
        Some(json!({ "mode": "in_place", "name": "Archive" })),
    )
    .await;
    assert_eq!(taken.status_code(), 409, "the name is taken");
    assert_eq!(json(taken).await["code"], "name_taken");
    let taken = post(
        &app,
        &format!("/api/mirrors/{second_id}/eject"),
        &alice,
        Some(json!({ "mode": "copy", "name": "Archive" })),
    )
    .await;
    assert_eq!(taken.status_code(), 409, "a copy's too");
    assert_eq!(json(taken).await["code"], "name_taken");
    let ejected = post(
        &app,
        &format!("/api/mirrors/{second_id}/eject"),
        &alice,
        Some(json!({ "mode": "in_place", "name": "Kept" })),
    )
    .await;
    assert_eq!(ejected.status_code(), 200, "{}", ejected.text());
    let ejected = json(ejected).await;
    assert_eq!(ejected["id"], second_id.as_str());
    assert_eq!(ejected["kind"], "shared");
    assert_eq!(ejected["my_role"], "manager");
    assert_eq!(ejected["mirror_of"], json!(null));
    assert_eq!(
        status_of(&app, &alice, &format!("/api/mirrors/{second_id}")).await,
        404
    );
    let second_root = second["root_id"].as_str().unwrap().to_string();
    assert_eq!(ejected["root_id"], second_root.as_str());
    let base = format!("organizer/{second_id}/");
    let before = objects(&op, &base).await;
    assert!(before.contains(&format!("{base}Docs/b.txt")));
    assert_eq!(
        upload(&app, &alice, &second_root, "mine.txt", "now")
            .await
            .status_code(),
        201
    );
    upload(&app, &alice, &root, "d.txt", "delta").await;
    app.state.files.run_mirrors().await;
    let now = paths_under(&app, &alice, &second_root).await;
    assert!(now.contains(&"mine.txt".to_string()));
    assert!(!now.contains(&"d.txt".to_string()), "no longer mirrored");
    let mirrors = json(request(&app, Method::GET, "/api/mirrors", Some(&alice), None).await).await;
    assert_eq!(mirrors.as_array().unwrap().len(), 1);
}

/// Restoring from a mirror's trash or versions copies back into the drive
/// mirrored, when the user may write there; else it is refused with a
/// code, and the file is downloaded instead.
#[tokio::test]
async fn restores_from_a_mirror_go_back_into_its_drive() {
    let app = spawn_app().await;
    let admin = admin_session(&app).await;
    let (alice, alice_id) = invited_user(&app, &admin, "alice@example.com").await;
    let (bob, bob_id) = invited_user(&app, &admin, "bob@example.com").await;
    let (backend, _) = own_backend(&app, &admin, &alice, "https://8.8.4.4").await;
    let root = root_id(&app, &alice).await;
    let docs = json(new_folder(&app, &alice, &root, "Docs").await).await["id"]
        .as_str()
        .unwrap()
        .to_string();
    let b = json(upload(&app, &alice, &docs, "b.txt", "beta").await).await["id"]
        .as_str()
        .unwrap()
        .to_string();
    let a = json(upload(&app, &alice, &root, "a.txt", "alpha").await).await["id"]
        .as_str()
        .unwrap()
        .to_string();
    let mirror = make_mirror(&app, &alice, &alice_id, &backend, "continuous").await;
    let id = mirror["id"].as_str().unwrap().to_string();
    let mirror_root = mirror["root_id"].as_str().unwrap().to_string();
    app.state.files.run_mirrors().await;

    // Deleted in the drive: back from the mirror's trash, where it was.
    assert_eq!(delete_node(&app, &alice, &b).await.status_code(), 200);
    // Emptied from the drive's own trash too.
    let purged = request(
        &app,
        Method::DELETE,
        &format!("/api/trash/{b}"),
        Some(&alice),
        None,
    )
    .await;
    assert_eq!(purged.status_code(), 204);
    app.state.files.run_mirrors().await;
    let trash = json(
        request(
            &app,
            Method::GET,
            &format!("/api/trash?drive={id}"),
            Some(&alice),
            None,
        )
        .await,
    )
    .await;
    let item = trash[0]["id"].as_str().unwrap().to_string();
    let downloaded = request(
        &app,
        Method::GET,
        &format!("/api/mirrors/{id}/trash/{item}/content"),
        Some(&alice),
        None,
    )
    .await;
    assert_eq!(downloaded.status_code(), 200);
    assert_eq!(downloaded.text(), "beta");
    let restored = post(
        &app,
        &format!("/api/mirrors/{id}/trash/{item}/restore"),
        &alice,
        None,
    )
    .await;
    assert_eq!(restored.status_code(), 200, "{}", restored.text());
    let restored = json(restored).await;
    assert_eq!(restored["drive_id"], alice_id.as_str());
    assert_eq!(restored["parent_id"], docs.as_str());
    assert_eq!(restored["name"], "b.txt");
    assert_eq!(
        download_text(&app, &alice, restored["id"].as_str().unwrap())
            .await
            .text(),
        "beta"
    );
    // A folder too, with what was in it.
    app.state.files.run_mirrors().await;
    assert_eq!(delete_node(&app, &alice, &docs).await.status_code(), 200);
    app.state.files.run_mirrors().await;
    let trash = json(
        request(
            &app,
            Method::GET,
            &format!("/api/trash?drive={id}"),
            Some(&alice),
            None,
        )
        .await,
    )
    .await;
    let folder = trash
        .as_array()
        .unwrap()
        .iter()
        .find(|t| t["name"] == "Docs")
        .unwrap()["id"]
        .as_str()
        .unwrap()
        .to_string();
    let taken = post(
        &app,
        &format!("/api/mirrors/{id}/trash/{folder}/restore"),
        &alice,
        None,
    )
    .await;
    assert_eq!(taken.status_code(), 200, "{}", taken.text());
    let docs_again = json(taken).await["id"].as_str().unwrap().to_string();
    assert_eq!(names_in(&app, &alice, &docs_again).await, ["b.txt"]);

    // A version: written over the file it copies, whose content is kept.
    replace(&app, &alice, &a, "alpha 2").await;
    app.state.files.run_mirrors().await;
    let a_copy = child(&app, &alice, &mirror_root, "a.txt").await["id"]
        .as_str()
        .unwrap()
        .to_string();
    let versions = versions_of(&app, &alice, &a_copy).await;
    let version = versions[0]["id"].as_str().unwrap().to_string();
    let back = post(
        &app,
        &format!("/api/mirrors/{id}/versions/{version}/restore"),
        &alice,
        None,
    )
    .await;
    assert_eq!(back.status_code(), 200, "{}", back.text());
    assert_eq!(json(back).await["id"], a.as_str());
    assert_eq!(download_text(&app, &alice, &a).await.text(), "alpha");
    assert_eq!(versions_of(&app, &alice, &a).await.len(), 2);

    // A viewer of a shared drive: refused, with a code; downloads instead.
    let on = request(
        &app,
        Method::PUT,
        "/api/superadmin/settings/mirrors.shared_drives",
        Some(&admin),
        Some(json!({ "mode": "default", "value": true })),
    )
    .await;
    assert_eq!(on.status_code(), 200);
    let (drive, drive_root) = shared_drive(&app, &admin, "Team", "alice@example.com").await;
    grant_drive(&app, &alice, &drive, &bob_id, "viewer").await;
    let gone = json(upload(&app, &alice, &drive_root, "gone.txt", "was").await).await["id"]
        .as_str()
        .unwrap()
        .to_string();
    let (bob_backend, _) = own_backend(&app, &admin, &bob, "https://8.8.8.4").await;
    let bobs = make_mirror(&app, &bob, &drive, &bob_backend, "continuous").await;
    let bobs_id = bobs["id"].as_str().unwrap().to_string();
    app.state.files.run_mirrors().await;
    assert_eq!(delete_node(&app, &alice, &gone).await.status_code(), 200);
    app.state.files.run_mirrors().await;
    let trash = json(
        request(
            &app,
            Method::GET,
            &format!("/api/trash?drive={bobs_id}"),
            Some(&bob),
            None,
        )
        .await,
    )
    .await;
    let item = trash[0]["id"].as_str().unwrap().to_string();
    let refused = post(
        &app,
        &format!("/api/mirrors/{bobs_id}/trash/{item}/restore"),
        &bob,
        None,
    )
    .await;
    assert_eq!(refused.status_code(), 403);
    assert_eq!(json(refused).await["code"], "drive_read_only");
    let downloaded = request(
        &app,
        Method::GET,
        &format!("/api/mirrors/{bobs_id}/trash/{item}/content"),
        Some(&bob),
        None,
    )
    .await;
    assert_eq!(downloaded.text(), "was");
    // Not alice's to restore from: not her mirror.
    assert_eq!(
        post(
            &app,
            &format!("/api/mirrors/{bobs_id}/trash/{item}/restore"),
            &alice,
            None,
        )
        .await
        .status_code(),
        404
    );
}

/// A copy a stop cut short is finished at the next start, whatever the
/// schedule: a file written to the mirror's storage but not indexed is
/// found by the scan and taken for the drive's file (not copied again, not
/// trashed), and the rest is copied.
#[tokio::test]
async fn a_copy_cut_short_by_a_stop_is_finished_at_start() {
    let app = spawn_app().await;
    let admin = admin_session(&app).await;
    let (alice, alice_id) = invited_user(&app, &admin, "alice@example.com").await;
    let (backend, op) = own_backend(&app, &admin, &alice, "https://8.8.4.4").await;
    let root = root_id(&app, &alice).await;
    upload(&app, &alice, &root, "a.txt", "alpha").await;
    let mirror = make_mirror(&app, &alice, &alice_id, &backend, "manual").await;
    let id = mirror["id"].as_str().unwrap().to_string();
    let mirror_id = uuid::Uuid::parse_str(&id).unwrap();
    post(&app, &format!("/api/mirrors/{id}/run"), &alice, None).await;
    app.state.files.run_mirrors().await;
    upload(&app, &alice, &root, "c.txt", "gamma").await;
    upload(&app, &alice, &root, "d.txt", "delta").await;

    // The stop: a copy under way, c.txt written to the bucket (its write
    // journaled) but not indexed.
    mirror_sql(
        &app,
        "UPDATE mirrors SET running_since = strftime('%Y-%m-%dT%H:%M:%fZ', 'now') WHERE id = ?1",
        &id,
    )
    .await;
    let base = format!("organizer/{id}/");
    op.write(&format!("{base}c.txt"), b"gamma".to_vec())
        .await
        .unwrap();
    organizer::files::journal::begin(
        &app.state.files,
        mirror_id,
        "write",
        None,
        Some("/nowhere/mirror-upload"),
        Some("c.txt"),
        None,
    )
    .await
    .unwrap();

    // The start: the journal settled, the tree scanned, the mirrors looked
    // at.
    organizer::files::journal::recover(&app.state.files, None)
        .await
        .unwrap();
    app.state.files.scan(mirror_id).await.unwrap();
    let mirror_root = mirror["root_id"].as_str().unwrap().to_string();
    let found = child(&app, &alice, &mirror_root, "c.txt").await;
    assert_eq!(found["source_id"], json!(null), "found by the scan");
    let found_id = found["id"].clone();
    assert_eq!(mirror_view(&app, &alice, &id).await["status"], "pending");
    app.state.files.run_mirrors().await;
    assert_eq!(
        paths_under(&app, &alice, &mirror_root).await,
        ["a.txt", "c.txt", "d.txt"]
    );
    let c = child(&app, &alice, &mirror_root, "c.txt").await;
    assert_eq!(c["id"], found_id, "taken over, not copied again");
    let c_source = child(&app, &alice, &root, "c.txt").await;
    assert_eq!(c["source_id"], c_source["id"]);
    let trash = json(
        request(
            &app,
            Method::GET,
            &format!("/api/trash?drive={id}"),
            Some(&alice),
            None,
        )
        .await,
    )
    .await;
    assert!(trash.as_array().unwrap().is_empty(), "{trash}");
    let c_id = c["id"].as_str().unwrap().to_string();
    assert!(versions_of(&app, &alice, &c_id).await.is_empty());
    let view = mirror_view(&app, &alice, &id).await;
    assert_eq!(view["status"], "up_to_date", "{view}");
    assert_eq!(
        objects(&op, &base).await,
        [
            format!("{base}a.txt"),
            format!("{base}c.txt"),
            format!("{base}d.txt")
        ]
    );
}

/// A wrapped folder key told apart from the others by `tag` (a base64url
/// character, put past the first byte, the format's version).
fn wrapping(tag: char) -> String {
    let mut wrapped = opaque(93, 1);
    wrapped.replace_range(10..11, &tag.to_string());
    wrapped
}

/// The generations and wrapped keys of a folder-keys answer.
fn generations(keys: &Value) -> Vec<(i64, String)> {
    keys["keys"]
        .as_array()
        .unwrap()
        .iter()
        .map(|k| {
            (
                k["generation"].as_i64().unwrap(),
                k["wrapped"].as_str().unwrap().to_string(),
            )
        })
        .collect()
}

/// A shared drive with an encrypted folder ("Vault", a file in it) whose
/// first key alice (its manager) wrapped for herself, bob and carol (its
/// viewers), and bob's mirror of it, copied once.
struct Sealed {
    admin: String,
    alice: String,
    bob: String,
    carol: String,
    /// alice's, bob's and carol's.
    ids: (String, String, String),
    drive: String,
    bob_grant: String,
    vault: String,
    file: String,
    mirror: String,
    vault_copy: String,
    file_copy: String,
}

async fn sealed_and_mirrored(app: &TestApp) -> Sealed {
    let admin = admin_session(app).await;
    let (alice, alice_id) = invited_user(app, &admin, "alice@example.com").await;
    let (bob, bob_id) = invited_user(app, &admin, "bob@example.com").await;
    let (carol, carol_id) = invited_user(app, &admin, "carol@example.com").await;
    let on = request(
        app,
        Method::PUT,
        "/api/superadmin/settings/mirrors.shared_drives",
        Some(&admin),
        Some(json!({ "mode": "default", "value": true })),
    )
    .await;
    assert_eq!(on.status_code(), 200);
    let (drive, drive_root) = shared_drive(app, &admin, "Team", "alice@example.com").await;
    let bob_grant = grant_drive(app, &alice, &drive, &bob_id, "viewer").await;
    grant_drive(app, &alice, &drive, &carol_id, "viewer").await;
    for session in [&alice, &bob, &carol] {
        assert_eq!(set_up_encryption(app, session).await.status_code(), 201);
    }
    let vault = encrypted_folder(app, &alice, &drive_root, "Vault").await;
    let first = json!([
        { "user_id": alice_id, "generation": 1, "wrapped": wrapping('a') },
        { "user_id": bob_id, "generation": 1, "wrapped": wrapping('b') },
        { "user_id": carol_id, "generation": 1, "wrapped": wrapping('c') },
    ]);
    let posted = post_keys(app, &alice, &vault, first).await;
    assert_eq!(posted.status_code(), 200, "{}", posted.text());
    let file = json(upload(app, &alice, &vault, &opaque(41, 1), "ciphertext").await).await["id"]
        .as_str()
        .unwrap()
        .to_string();
    let (backend, _op) = own_backend(app, &admin, &bob, "https://8.8.4.4").await;
    let mirror = make_mirror(app, &bob, &drive, &backend, "continuous").await;
    app.state.files.run_mirrors().await;
    let mirror_root = mirror["root_id"].as_str().unwrap().to_string();
    let vault_copy = child(app, &bob, &mirror_root, "Vault").await;
    assert_eq!(vault_copy["source_id"], vault.as_str());
    let vault_copy = vault_copy["id"].as_str().unwrap().to_string();
    let file_copy = child(app, &bob, &vault_copy, &opaque(41, 1)).await["id"]
        .as_str()
        .unwrap()
        .to_string();
    Sealed {
        admin,
        alice,
        bob,
        carol,
        ids: (alice_id, bob_id, carol_id),
        drive,
        bob_grant,
        vault,
        file,
        mirror: mirror["id"].as_str().unwrap().to_string(),
        vault_copy,
        file_copy,
    }
}

/// Bob's keys as his mirror kept them: of the drive's folder (the id they
/// are sealed with), his wrappings only, nothing to post.
async fn kept_for_bob(app: &TestApp, s: &Sealed, id: &str) -> Vec<(i64, String)> {
    let response = folder_keys(app, &s.bob, id).await;
    assert_eq!(response.status_code(), 200, "{}", response.text());
    let keys = json(response).await;
    assert_eq!(keys["folder_id"], s.vault.as_str());
    assert_eq!(keys["members"], json!([]));
    assert_eq!(keys["needs_new_key"], false);
    generations(&keys)
}

/// Every kept wrapping: whose, its generation and the wrapped key.
async fn kept_rows(app: &TestApp) -> Vec<(String, i64, String)> {
    sqlx::query_as(
        "SELECT u.email, k.generation, k.wrapped FROM e2e_copy_keys k
         JOIN users u ON u.id = k.user_id ORDER BY k.drive_id, k.generation",
    )
    .fetch_all(app.state.db.reader())
    .await
    .unwrap()
}

/// An item of a mirror's trash keeps the drive's ids its node had, which
/// its name and keys are sealed with: its own, its folder's and its
/// encrypted folder's. Elsewhere there are none.
#[tokio::test]
async fn a_mirrors_trash_keeps_the_drives_ids() {
    let app = spawn_app().await;
    let s = sealed_and_mirrored(&app).await;
    assert_eq!(
        delete_node(&app, &s.alice, &s.file).await.status_code(),
        200
    );
    app.state.files.run_mirrors().await;
    let trash = json(
        request(
            &app,
            Method::GET,
            &format!("/api/trash?drive={}", s.mirror),
            Some(&s.bob),
            None,
        )
        .await,
    )
    .await;
    assert_eq!(trash.as_array().unwrap().len(), 1, "{trash}");
    let item = &trash[0];
    assert_eq!(item["id"], s.file_copy.as_str());
    assert_eq!(item["original_parent_id"], s.vault_copy.as_str());
    assert_eq!(item["e2e_root"], s.vault_copy.as_str());
    assert_eq!(item["source_id"], s.file.as_str());
    assert_eq!(item["original_parent_source_id"], s.vault.as_str());
    assert_eq!(item["e2e_root_source_id"], s.vault.as_str());

    // The drive's own trash: no other ids.
    let theirs = json(
        request(
            &app,
            Method::GET,
            &format!("/api/trash?drive={}", s.drive),
            Some(&s.alice),
            None,
        )
        .await,
    )
    .await;
    assert_eq!(theirs[0]["id"], s.file.as_str());
    for key in [
        "source_id",
        "original_parent_source_id",
        "e2e_root_source_id",
    ] {
        assert_eq!(theirs[0][key], json!(null), "{key}");
    }
}

/// A mirror keeps its user's own wrapped keys of the encrypted folders it
/// copies, every generation (one made while it mirrors too), so the copy
/// still opens once the user lost access to the drive: by the copy's ids
/// or the drive's (`source_id`). Nobody else gets them.
#[tokio::test]
async fn a_mirror_keeps_its_users_keys_when_access_is_lost() {
    let app = spawn_app().await;
    let s = sealed_and_mirrored(&app).await;
    let (alice_id, bob_id, carol_id) = s.ids.clone();
    // While bob has access: the drive's keys by the drive's ids, the kept
    // ones by the copy's.
    let live = json(folder_keys(&app, &s.bob, &s.vault).await).await;
    assert_eq!(live["members"].as_array().unwrap().len(), 3);
    assert_eq!(generations(&live), [(1, wrapping('b'))]);
    assert_eq!(
        kept_for_bob(&app, &s, &s.vault_copy).await,
        [(1, wrapping('b'))]
    );
    // The copy's keys are kept, not posted.
    let refused = post_keys(
        &app,
        &s.bob,
        &s.vault_copy,
        json!([{ "user_id": bob_id, "generation": 1, "wrapped": wrapping('x') }]),
    )
    .await;
    assert_eq!(refused.status_code(), 404, "{}", refused.text());

    // A new generation while mirroring, kept without another copy.
    let second = json!([
        { "user_id": alice_id, "generation": 2, "wrapped": wrapping('d') },
        { "user_id": bob_id, "generation": 2, "wrapped": wrapping('e') },
        { "user_id": carol_id, "generation": 2, "wrapped": wrapping('f') },
    ]);
    let posted = post_keys(&app, &s.alice, &s.vault, second).await;
    assert_eq!(posted.status_code(), 200, "{}", posted.text());

    // Bob loses access: the drive drops his keys, the mirror stops.
    let removed = request(
        &app,
        Method::DELETE,
        &format!("/api/drives/{}/access/{}", s.drive, s.bob_grant),
        Some(&s.alice),
        None,
    )
    .await;
    assert_eq!(removed.status_code(), 204, "{}", removed.text());
    assert_eq!(
        mirror_view(&app, &s.bob, &s.mirror).await["stopped"],
        "access_lost"
    );
    let held: i64 = sqlx::query_scalar(
        "SELECT COUNT(*) FROM e2e_folder_keys
         WHERE user_id = (SELECT id FROM users WHERE email = 'bob@example.com')",
    )
    .fetch_one(app.state.db.reader())
    .await
    .unwrap();
    assert_eq!(held, 0);
    let both = [(1, wrapping('b')), (2, wrapping('e'))];
    for id in [&s.vault, &s.file, &s.vault_copy, &s.file_copy] {
        assert_eq!(kept_for_bob(&app, &s, id).await, both, "by {id}");
    }

    // Nobody else: alice and carol have their own keys of the drive's
    // folder, and the mirror is not theirs; the superadmin has neither.
    let carols = json(folder_keys(&app, &s.carol, &s.vault).await).await;
    assert_eq!(
        generations(&carols),
        [(1, wrapping('c')), (2, wrapping('f'))]
    );
    let alices = json(folder_keys(&app, &s.alice, &s.vault).await).await;
    assert_eq!(
        generations(&alices),
        [(1, wrapping('a')), (2, wrapping('d'))]
    );
    for session in [&s.carol, &s.alice, &s.admin] {
        assert_eq!(
            folder_keys(&app, session, &s.vault_copy)
                .await
                .status_code(),
            404
        );
    }
    assert!(
        !folder_keys(&app, &s.admin, &s.vault)
            .await
            .status_code()
            .is_success()
    );
    assert_eq!(
        kept_rows(&app).await,
        [
            ("bob@example.com".to_string(), 1, wrapping('b')),
            ("bob@example.com".to_string(), 2, wrapping('e')),
        ]
    );
}

/// A mirror's kept keys outlive the drive deleted; a drive ejected from
/// the mirror as a copy keeps them too, and they go with the last copy.
#[tokio::test]
async fn a_mirror_keeps_its_users_keys_when_the_drive_is_deleted() {
    let app = spawn_app().await;
    let s = sealed_and_mirrored(&app).await;
    let ejected = post(
        &app,
        &format!("/api/mirrors/{}/eject", s.mirror),
        &s.bob,
        Some(json!({ "mode": "copy", "name": "Archive" })),
    )
    .await;
    assert_eq!(ejected.status_code(), 201, "{}", ejected.text());
    let archive = json(ejected).await;
    let archive_root = archive["root_id"].as_str().unwrap().to_string();
    let vault_in_archive = child(&app, &s.bob, &archive_root, "Vault").await["id"]
        .as_str()
        .unwrap()
        .to_string();

    let deleted = request(
        &app,
        Method::DELETE,
        &format!("/api/superadmin/drives/{}", s.drive),
        Some(&s.admin),
        None,
    )
    .await;
    assert_eq!(deleted.status_code(), 204);
    assert_eq!(
        mirror_view(&app, &s.bob, &s.mirror).await["stopped"],
        "drive_deleted"
    );
    for id in [
        &s.vault,
        &s.file,
        &s.vault_copy,
        &s.file_copy,
        &vault_in_archive,
    ] {
        assert_eq!(
            kept_for_bob(&app, &s, id).await,
            [(1, wrapping('b'))],
            "by {id}"
        );
    }
    for session in [&s.alice, &s.carol, &s.admin] {
        for id in [&s.vault, &s.vault_copy] {
            assert!(
                !folder_keys(&app, session, id)
                    .await
                    .status_code()
                    .is_success()
            );
        }
    }

    // The mirror removed: the archive's copy still opens; removed too, the
    // keys go with it.
    let removed = request(
        &app,
        Method::DELETE,
        &format!("/api/mirrors/{}", s.mirror),
        Some(&s.bob),
        None,
    )
    .await;
    assert!(removed.status_code().is_success(), "{}", removed.text());
    assert_eq!(kept_for_bob(&app, &s, &s.vault).await, [(1, wrapping('b'))]);
    assert_eq!(kept_rows(&app).await.len(), 1);
    let archive_id = archive["id"].as_str().unwrap();
    let gone = request(
        &app,
        Method::DELETE,
        &format!("/api/superadmin/drives/{archive_id}"),
        Some(&s.admin),
        None,
    )
    .await;
    assert_eq!(gone.status_code(), 204, "{}", gone.text());
    assert_eq!(folder_keys(&app, &s.bob, &s.vault).await.status_code(), 404);
    assert!(kept_rows(&app).await.is_empty());
}
