pub mod activity;
pub mod ai;
pub mod auth;
pub mod backends;
pub mod backup;
pub mod config;
pub mod csrf;
pub mod dav;
pub mod dav_inbox;
pub mod db;
pub mod drop_box;
pub mod embed;
pub mod error;
pub mod extract;
pub mod federation;
pub mod files;
pub mod frontend;
pub mod jobs;
pub mod limits;
pub mod net;
pub mod oidc;
pub mod openapi;
pub mod passkeys;
pub mod push;
pub mod reading;
pub mod recovery;
pub mod retention;
pub mod routes;
pub mod search;
pub mod server_settings;
pub mod session;
pub mod settings;
pub mod state;
pub mod storage;
pub mod totp;
pub mod users;
pub mod util;
pub mod vectors;

use axum::Router;
use state::AppState;

/// Where the spec is served with `SWAGGER_UI` set, and Swagger UI itself
/// (the same paths as Woodmouse's).
pub const SPEC_PATH: &str = "/api-docs/openapi.json";
pub const SWAGGER_UI_PATH: &str = "/swagger-ui";

/// Every route of the API with its OpenAPI document, under /api. The app
/// serves the routes; the `openapi` binary writes the document
/// (`openapi::spec`).
pub fn api() -> utoipa_axum::router::OpenApiRouter<AppState> {
    utoipa_axum::router::OpenApiRouter::with_openapi(openapi::base()).nest("/api", routes::router())
}

/// The whole app: the API routes, the spec and Swagger UI when asked for
/// (`SWAGGER_UI`), then the embedded frontend.
/// The handlers of every kind of background job.
pub fn handlers() -> Vec<std::sync::Arc<dyn jobs::Handler>> {
    let mut handlers = drop_box::handlers();
    handlers.extend(backup::handlers());
    handlers.push(std::sync::Arc::new(reading::Read));
    handlers
}

pub fn app(state: AppState) -> Router {
    let (mut router, spec) = api().split_for_parts();
    if state.swagger_ui {
        router =
            router.merge(utoipa_swagger_ui::SwaggerUi::new(SWAGGER_UI_PATH).url(SPEC_PATH, spec));
    }
    // Sessions (tower-sessions, our store) and who is signed in on them
    // (axum-login). A session is only loaded when a handler asks for it.
    let sessions = tower_sessions::SessionManagerLayer::new(session::Store::new(state.db.clone()))
        .with_name(session::cookie_name(state.secure_cookies))
        .with_path("/")
        .with_http_only(true)
        .with_same_site(tower_sessions::cookie::SameSite::Strict)
        .with_secure(state.secure_cookies);
    let auth =
        axum_login::AuthManagerLayerBuilder::new(session::Backend::new(state.db.clone()), sessions)
            .build();
    // The superadmin's endpoints under their old name, /api/admin, the same
    // handlers and rules, for clients not updated yet: outside the spec,
    // which lists /api/superadmin only (docs/M7A_DESIGN.md §5). Removed
    // after one release.
    let (old_admin, _) = routes::superadmin_router().split_for_parts();
    // WebDAV: its methods are not an API the spec can describe.
    let router = router
        .nest("/api/admin", old_admin)
        // OCM for other servers (docs/M7E_DESIGN.md): outside the spec, as
        // WebDAV, since its API is OCM's.
        .merge(routes::ocm::router())
        .route(dav::PREFIX, axum::routing::any(dav::handle))
        .route(
            &format!("{}/", dav::PREFIX),
            axum::routing::any(dav::handle),
        )
        .route(
            &format!("{}/{{*path}}", dav::PREFIX),
            axum::routing::any(dav::handle),
        )
        // The WebDAV drop box, a drive of its own.
        .route(dav_inbox::PREFIX, axum::routing::any(dav_inbox::handle))
        .route(
            &format!("{}/", dav_inbox::PREFIX),
            axum::routing::any(dav_inbox::handle),
        )
        .route(
            &format!("{}/{{*path}}", dav_inbox::PREFIX),
            axum::routing::any(dav_inbox::handle),
        );
    frontend::mount(router)
        .layer(auth)
        // Outside the sessions: a forged request is refused before any
        // session is loaded or saved.
        .layer(axum::middleware::from_fn_with_state(
            state.clone(),
            csrf::check,
        ))
        .layer(tower_http::trace::TraceLayer::new_for_http())
        .with_state(state)
}

/// Install ring as the process TLS provider. reqwest is built with
/// `rustls-no-provider` (aws-lc is painful on musl, which the image is built
/// for), so the provider must be installed before the first client is
/// built. Safe to call more than once.
pub fn install_tls_provider() {
    use std::sync::Once;
    static INSTALL: Once = Once::new();
    INSTALL.call_once(|| {
        rustls::crypto::ring::default_provider()
            .install_default()
            .ok();
    });
}
