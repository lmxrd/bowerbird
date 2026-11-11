//! Cross-site request forgery: a request that changes something must come
//! from Organizer's own pages. The session cookie is `SameSite=Strict`, but
//! a sibling subdomain counts as the same *site* (`blog.example.com` next
//! to `files.example.com`), and a form there can send a multipart upload
//! without a CORS preflight. So the browser's Fetch Metadata
//! decides (OWASP's "Fetch Metadata" defence, the rule of Go's
//! `http.CrossOriginProtection`):
//!
//! - GET, HEAD and OPTIONS change nothing and always pass;
//! - `Sec-Fetch-Site: same-origin` (or `none`, typed by the user) passes,
//!   anything else (`same-site`, `cross-site`) is refused;
//! - without it (an older browser), an `Origin` must be `PUBLIC_URL`'s, or,
//!   without `PUBLIC_URL`, the request's `Host`;
//! - without either, it is not a browser, so not a forgery: it passes.
//!
//! A request with `Authorization: Bearer` (an API key, a device) cannot be
//! forged by a page. Basic authentication (WebDAV's app passwords) gets no
//! such pass: a browser resends remembered Basic credentials by itself.
//! WebDAV clients are not browsers and send neither header, so they pass
//! by the last rule. No crate does this (`axum_csrf` needs tokens in the
//! frontend), hence our own.

use crate::error::HttpError;
use crate::state::AppState;
use axum::extract::{Request, State};
use axum::http::{HeaderMap, StatusCode, header};
use axum::middleware::Next;
use axum::response::{IntoResponse, Response};

pub async fn check(State(state): State<AppState>, request: Request, next: Next) -> Response {
    if allowed(&request, state.public_url.as_deref()) {
        next.run(request).await
    } else {
        HttpError::new(StatusCode::FORBIDDEN, "Cross-origin request refused").into_response()
    }
}

fn allowed(request: &Request, public_url: Option<&str>) -> bool {
    let headers = request.headers();
    if request.method().is_safe()
        || headers
            .get(header::AUTHORIZATION)
            .and_then(|v| v.to_str().ok())
            .is_some_and(|v| v.len() > 7 && v[..7].eq_ignore_ascii_case("bearer "))
    {
        return true;
    }
    if let Some(site) = headers.get("sec-fetch-site") {
        return matches!(site.to_str(), Ok("same-origin" | "none"));
    }
    match headers.get(header::ORIGIN).and_then(|v| v.to_str().ok()) {
        Some(origin) => own_origin(origin, headers, public_url),
        None => true,
    }
}

/// Whether an `Origin` (`scheme://host[:port]`) is Organizer's own: the one of
/// `PUBLIC_URL` (scheme included), or without it the host the request was
/// sent to. Forwarded headers do not count: behind a proxy that rewrites
/// `Host`, set `PUBLIC_URL`.
fn own_origin(origin: &str, headers: &HeaderMap, public_url: Option<&str>) -> bool {
    if let Some(public_url) = public_url {
        return origin.eq_ignore_ascii_case(public_url);
    }
    let Some((_, origin_host)) = origin.split_once("://") else {
        return false; // "null", or not a URL
    };
    headers
        .get(header::HOST)
        .and_then(|v| v.to_str().ok())
        .is_some_and(|host| host.eq_ignore_ascii_case(origin_host))
}

#[cfg(test)]
mod tests {
    use super::*;
    use axum::body::Body;

    fn request(method: &str, path: &str, headers: &[(&str, &str)]) -> Request {
        let mut builder = Request::builder()
            .method(method)
            .uri(path)
            .header("host", "files.example.com");
        for (name, value) in headers {
            builder = builder.header(*name, *value);
        }
        builder.body(Body::empty()).unwrap()
    }

    #[test]
    fn fetch_metadata_decides_for_a_browser() {
        let post =
            |headers: &[(&str, &str)]| allowed(&request("POST", "/api/files", headers), None);
        assert!(post(&[("sec-fetch-site", "same-origin")]));
        assert!(post(&[("sec-fetch-site", "none")]));
        assert!(!post(&[("sec-fetch-site", "same-site")]));
        assert!(!post(&[("sec-fetch-site", "cross-site")]));
        // Fetch Metadata wins over a matching Origin.
        assert!(!post(&[
            ("sec-fetch-site", "same-site"),
            ("origin", "https://files.example.com")
        ]));
    }

    #[test]
    fn without_fetch_metadata_the_origin_must_match() {
        let post = |headers: &[(&str, &str)], public_url| {
            allowed(&request("DELETE", "/api/files/1", headers), public_url)
        };
        assert!(post(&[("origin", "https://files.example.com")], None));
        assert!(post(&[("origin", "https://FILES.example.com")], None));
        assert!(!post(&[("origin", "https://blog.example.com")], None));
        assert!(!post(&[("origin", "null")], None));
        // Not a browser: nothing to forge.
        assert!(post(&[], None));
        // With PUBLIC_URL, its origin, scheme included, whatever the Host
        // a proxy sends; forwarded headers do not count.
        let public = Some("https://files.example.org");
        assert!(post(&[("origin", "https://files.example.org")], public));
        assert!(!post(&[("origin", "http://files.example.org")], public));
        assert!(!post(&[("origin", "https://files.example.com")], public));
        let forwarded = [
            ("origin", "https://elsewhere.example.org"),
            ("x-forwarded-host", "elsewhere.example.org"),
        ];
        assert!(!post(&forwarded, None));
    }

    #[test]
    fn reads_and_api_keys_pass_but_basic_authentication_does_not() {
        let cross = [("sec-fetch-site", "cross-site")];
        assert!(allowed(&request("GET", "/api/files/root", &cross), None));
        let key = [
            ("sec-fetch-site", "cross-site"),
            ("authorization", "Bearer org_abc"),
        ];
        assert!(allowed(&request("POST", "/api/files", &key), None));
        // A browser resends remembered Basic credentials by itself.
        let basic = [
            ("sec-fetch-site", "cross-site"),
            ("authorization", "Basic dXNlcjpvcmdfYWJj"),
        ];
        assert!(!allowed(&request("PUT", "/dav/a.txt", &basic), None));
        // A WebDAV client is not a browser: neither header, so it passes.
        let client = [("authorization", "Basic dXNlcjpvcmdfYWJj")];
        assert!(allowed(&request("PUT", "/dav/a.txt", &client), None));
    }
}
