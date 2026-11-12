use crate::error::ErrorBody;
use crate::state::AppState;
use axum::Json;
use axum::http::StatusCode;
use utoipa_axum::router::OpenApiRouter;
use utoipa_axum::routes;

pub mod access;
pub mod ai;
pub mod api_keys;
pub mod auth;
pub mod backends;
pub mod backup;
pub mod changes;
pub mod device;
pub mod drives;
pub mod drop_box;
pub mod encryption;
pub mod federation;
pub mod files;
pub mod groups;
pub mod links;
pub mod me;
pub mod mirrors;
pub mod notifications;
pub mod ocm;
pub mod oidc;
pub mod passkeys;
pub mod search;
pub mod settings;
pub mod shares;
pub mod step_up;
pub mod storage;
pub mod teams;
pub mod totp;
pub mod uploads;
pub mod users;
pub mod versions;

/// Every /api route, with the JSON 404 for unknown paths. The routes are
/// registered together with their `#[utoipa::path]` annotations, so the
/// OpenAPI document is made of exactly what is routed: the paths in the
/// annotations are relative to their module's nest, and the nests are the
/// namespaces under /api.
pub fn router() -> OpenApiRouter<AppState> {
    OpenApiRouter::new()
        .routes(routes!(health))
        .nest("/superadmin", superadmin_router())
        .nest(
            "/auth",
            auth::router()
                .merge(oidc::router())
                .merge(totp::router())
                .merge(step_up::router())
                .merge(api_keys::router())
                .merge(device::router())
                .merge(passkeys::router())
                .merge(users::router()),
        )
        .nest("/ai", ai::router())
        .nest("/backends", backends::router())
        .nest("/backups", backup::router())
        .nest(
            "/drives",
            drives::router()
                .merge(access::drives_router())
                .merge(settings::drives_router())
                .merge(backends::drives_router()),
        )
        .nest("/drop-box", drop_box::router())
        .nest("/encryption", encryption::router())
        .nest(
            "/files",
            files::router()
                .merge(versions::router())
                .merge(shares::files_router())
                .merge(access::files_router())
                .merge(links::files_router()),
        )
        .merge(notifications::router())
        .nest("/placements", drop_box::placements_router())
        .nest("/links", links::router())
        .nest("/s", links::public_router())
        .nest(
            "/shared",
            shares::router().merge(federation::shared_router()),
        )
        .nest("/contacts", federation::contacts_router())
        .nest("/invites/federated", federation::invites_router())
        .nest("/federation", federation::router())
        .nest("/settings", settings::router())
        .nest("/teams", teams::router())
        .nest("/me", me::router())
        .nest("/mirrors", mirrors::router())
        .nest("/retention", versions::retention_router())
        .nest("/trash", files::trash_router())
        .nest("/uploads", uploads::router())
        .merge(changes::router())
        .merge(search::router())
        .fallback(not_found)
}

/// The superadmin's endpoints (settings of the whole instance), under
/// /api/superadmin, and under the old /api/admin too (`crate::app`).
pub fn superadmin_router() -> OpenApiRouter<AppState> {
    oidc::superadmin_router()
        .merge(storage::superadmin_router())
        .merge(backends::superadmin_router())
        .merge(ai::superadmin_router())
        .merge(versions::superadmin_router())
        .merge(backup::superadmin_router())
        .merge(users::superadmin_router())
        .merge(groups::superadmin_router())
        .merge(drives::superadmin_router())
        .merge(access::superadmin_router())
        .merge(teams::superadmin_router())
        .merge(settings::superadmin_router())
        .merge(federation::superadmin_router())
}

async fn not_found() -> (StatusCode, Json<ErrorBody>) {
    (StatusCode::NOT_FOUND, Json(ErrorBody::new("Not found")))
}

/// Liveness: 200 OK, for the Docker healthcheck and load balancers.
#[utoipa::path(
    get,
    path = "/health",
    tag = "System",
    operation_id = "getHealth",
    summary = "Liveness",
    description = "200 OK, for the Docker healthcheck and load balancers.",
    responses(
        (status = 200, description = "The server is up", body = String, content_type = "text/plain"),
    ),
)]
async fn health() -> &'static str {
    "OK"
}
