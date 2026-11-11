//! The embedded frontend: the built files of `packages/frontend/dist`, served with
//! the cache policy they need, and the SPA fallback to index.html.

use crate::state::AppState;
use axum::Router;
use axum::http::{HeaderMap, StatusCode, Uri, header};
use axum::middleware::{self, Next};
use axum::response::{IntoResponse, Response};
use rust_embed::RustEmbed;

/// The built frontend, embedded in the binary (and read from disk in
/// debug builds, so a dev server needs no rebuild).
#[derive(RustEmbed)]
#[folder = "../frontend/dist"]
// The spec build (the `openapi` binary) happens before the frontend is
// built, because the frontend is generated from the spec: it must compile
// without `frontend/dist`. No other build may, or it would embed nothing.
#[cfg_attr(feature = "spec", allow_missing = true)]
struct Dist;

/// Serve the frontend for every path the router does not handle itself,
/// and make everything without its own cache policy answer no-cache.
pub fn mount(router: Router<AppState>) -> Router<AppState> {
    router
        .fallback(static_handler)
        .layer(middleware::from_fn(no_cache))
}

/// Everything without its own cache policy answers with no-cache, like
/// the API responses and the shell; the hashed files under /assets are
/// immutable.
async fn no_cache(request: axum::extract::Request, next: Next) -> Response {
    let mut response = next.run(request).await;
    if response.headers().get(header::CACHE_CONTROL).is_none() {
        response
            .headers_mut()
            .insert(header::CACHE_CONTROL, "no-cache".parse().unwrap());
    }
    response
}

/// The embedded CSR frontend: static files, falling back to index.html
/// for client routes.
async fn static_handler(uri: Uri, _headers: HeaderMap) -> Response {
    let path = uri.path().trim_start_matches('/');
    let (file, cache) = if path.is_empty() {
        ("index.html", "no-cache")
    } else {
        match Dist::get(path) {
            Some(_) if path.starts_with("assets/") => (path, "public, max-age=31536000, immutable"),
            Some(_) => (path, "no-cache"),
            None => ("index.html", "no-cache"),
        }
    };
    match Dist::get(file) {
        Some(content) => {
            let mime = mime_guess::from_path(file).first_or_octet_stream();
            (
                StatusCode::OK,
                [
                    (header::CONTENT_TYPE, mime.as_ref()),
                    (header::CACHE_CONTROL, cache),
                ],
                content.data,
            )
                .into_response()
        }
        None => (
            StatusCode::NOT_FOUND,
            "frontend not built; run: mise run build:frontend",
        )
            .into_response(),
    }
}
