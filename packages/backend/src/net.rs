//! The client's network address, for the sign-in limits.

use axum::http::HeaderMap;
use std::net::IpAddr;

/// The client's address: the connection's, or with `TRUST_PROXY` the first
/// one of `X-Forwarded-For` (the proxy's view of the client). Never stored
/// or logged; the limits keep it in memory only.
pub fn client_ip(headers: &HeaderMap, addr: Option<IpAddr>, trust_proxy: bool) -> String {
    if trust_proxy
        && let Some(forwarded) = headers.get("x-forwarded-for").and_then(|v| v.to_str().ok())
        && let Some(first) = forwarded.split(',').next()
    {
        let first = first.trim();
        if !first.is_empty() {
            return first.to_string();
        }
    }
    addr.map(|a| a.to_string()).unwrap_or_default()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn forwarded_headers_count_only_behind_a_trusted_proxy() {
        let mut headers = HeaderMap::new();
        headers.insert("x-forwarded-for", "203.0.113.7, 10.0.0.1".parse().unwrap());
        let addr = Some("10.0.0.1".parse().unwrap());
        assert_eq!(client_ip(&headers, addr, true), "203.0.113.7");
        assert_eq!(client_ip(&headers, addr, false), "10.0.0.1");
        assert_eq!(client_ip(&HeaderMap::new(), addr, true), "10.0.0.1");
    }
}
