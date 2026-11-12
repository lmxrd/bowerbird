//! A link dropped into the drop box, made a file (docs/M2_DESIGN.md §3.1):
//! a bookmark (`<title>.url`) with the page's text read for the proposal,
//! or the file the link points to.
//!
//! The page is fetched by the server, so a user could ask it for addresses
//! only the server reaches. Every address a request goes to, redirects
//! included, must be a public one: not the machine itself, not the local
//! network, not link-local (where clouds keep their metadata service).
//! `LINKS_ALLOW_PRIVATE` lifts that, for a server whose users may file the
//! pages of their own network.

use super::extract::{self, NoText};
use std::net::{IpAddr, SocketAddr};
use std::sync::Arc;
use std::time::Duration;

/// The most of a page that is read.
pub const MAX_PAGE_BYTES: usize = 5_000_000;
/// The largest file a `download` link may bring.
pub const MAX_DOWNLOAD_BYTES: usize = 100_000_000;
const TIMEOUT: Duration = Duration::from_secs(30);
const MAX_REDIRECTS: usize = 5;

/// Whether an address is a public one.
pub fn is_public(ip: IpAddr) -> bool {
    match ip {
        IpAddr::V4(ip) => {
            let [a, b, c, _] = ip.octets();
            !(ip.is_private()
                || ip.is_loopback()
                || ip.is_link_local()
                || ip.is_broadcast()
                || ip.is_documentation()
                || ip.is_unspecified()
                || ip.is_multicast()
                || a == 0
                // Carrier-grade NAT, 100.64.0.0/10.
                || (a == 100 && (64..128).contains(&b))
                // IETF protocol assignments, 192.0.0.0/24.
                || (a == 192 && b == 0 && c == 0)
                // Benchmarking, 198.18.0.0/15.
                || (a == 198 && (18..20).contains(&b))
                // Reserved, 240.0.0.0/4.
                || a >= 240)
        }
        IpAddr::V6(ip) => {
            if let Some(v4) = ip.to_ipv4_mapped() {
                return is_public(IpAddr::V4(v4));
            }
            let first = ip.segments()[0];
            !(ip.is_loopback()
                || ip.is_unspecified()
                || ip.is_multicast()
                // Unique local, fc00::/7.
                || (first & 0xfe00) == 0xfc00
                // Link-local, fe80::/10.
                || (first & 0xffc0) == 0xfe80
                // Documentation, 2001:db8::/32.
                || (first == 0x2001 && ip.segments()[1] == 0x0db8)
                // NAT64, 64:ff9b::/96: what it maps to is not known here.
                || (first == 0x64 && ip.segments()[1] == 0xff9b))
        }
    }
}

/// Resolves names to their public addresses only.
pub(crate) struct PublicOnly;

impl reqwest::dns::Resolve for PublicOnly {
    fn resolve(&self, name: reqwest::dns::Name) -> reqwest::dns::Resolving {
        let host = name.as_str().to_string();
        Box::pin(async move {
            let addrs: Vec<SocketAddr> = tokio::net::lookup_host((host.as_str(), 0))
                .await?
                .filter(|a| is_public(a.ip()))
                .collect();
            if addrs.is_empty() {
                return Err(format!("{host} has no public address").into());
            }
            Ok(Box::new(addrs.into_iter()) as reqwest::dns::Addrs)
        })
    }
}

/// Whether a URL may be fetched: http or https, and a host that is not an
/// address of the private kind (names are checked when resolved).
pub(crate) fn allowed(url: &reqwest::Url, allow_private: bool) -> bool {
    if !matches!(url.scheme(), "http" | "https") {
        return false;
    }
    let Some(host) = url.host_str() else {
        return false;
    };
    // An IPv6 address comes in brackets.
    match host
        .trim_start_matches('[')
        .trim_end_matches(']')
        .parse::<IpAddr>()
    {
        Ok(ip) => allow_private || is_public(ip),
        Err(_) => true,
    }
}

fn client(allow_private: bool) -> reqwest::Client {
    let mut builder = reqwest::Client::builder()
        .timeout(TIMEOUT)
        .user_agent(concat!("Organizer/", env!("CARGO_PKG_VERSION")))
        .redirect(reqwest::redirect::Policy::custom(move |attempt| {
            if attempt.previous().len() >= MAX_REDIRECTS {
                attempt.error("too many redirects")
            } else if !allowed(attempt.url(), allow_private) {
                attempt.error("a redirect to an address that is not allowed")
            } else {
                attempt.follow()
            }
        }));
    if !allow_private {
        builder = builder.dns_resolver(Arc::new(PublicOnly));
    }
    builder.build().expect("an HTTP client")
}

/// What a link gave.
#[derive(Debug)]
pub struct Fetched {
    /// Where it ended, after redirects.
    pub url: String,
    /// The type it said, without parameters.
    pub mime_type: Option<String>,
    /// The file name it gave (`Content-Disposition`), if any.
    pub filename: Option<String>,
    pub bytes: Vec<u8>,
}

/// GET a link, at most `max` bytes (more is an error).
pub async fn fetch(url: &str, max: usize, allow_private: bool) -> Result<Fetched, String> {
    let parsed = reqwest::Url::parse(url).map_err(|e| e.to_string())?;
    if !allowed(&parsed, allow_private) {
        return Err("the link's address is not allowed".into());
    }
    let mut response = client(allow_private)
        .get(parsed)
        .send()
        .await
        .map_err(|e| format!("the page cannot be fetched: {e}"))?;
    if !response.status().is_success() {
        return Err(format!("the page answered {}", response.status()));
    }
    let header = |name: reqwest::header::HeaderName| {
        response
            .headers()
            .get(name)
            .and_then(|v| v.to_str().ok())
            .map(String::from)
    };
    let mime_type = header(reqwest::header::CONTENT_TYPE).map(|t| {
        t.split(';')
            .next()
            .unwrap_or_default()
            .trim()
            .to_lowercase()
    });
    let filename = header(reqwest::header::CONTENT_DISPOSITION).and_then(|d| disposition_name(&d));
    let final_url = response.url().to_string();
    let mut bytes = Vec::new();
    while let Some(chunk) = response
        .chunk()
        .await
        .map_err(|e| format!("the page broke off: {e}"))?
    {
        if bytes.len() + chunk.len() > max {
            return Err(format!("it is larger than {} MB", max / 1_000_000));
        }
        bytes.extend_from_slice(&chunk);
    }
    Ok(Fetched {
        url: final_url,
        mime_type,
        filename,
        bytes,
    })
}

/// The file name of a `Content-Disposition` header: `filename="..."` (the
/// RFC 5987 `filename*` form is left to the URL's name).
fn disposition_name(header: &str) -> Option<String> {
    header.split(';').find_map(|part| {
        let (key, value) = part.trim().split_once('=')?;
        (key.trim().eq_ignore_ascii_case("filename"))
            .then(|| value.trim().trim_matches('"').to_string())
            .filter(|v| !v.is_empty())
    })
}

/// A title or URL part as a file name: no separators or control
/// characters, spaces squeezed, at most 120 characters; `None` when nothing
/// usable is left.
pub fn file_name(raw: &str) -> Option<String> {
    let cleaned: String = raw
        .chars()
        .map(|c| match c {
            '/' | '\\' | ':' | '*' | '?' | '"' | '<' | '>' | '|' => ' ',
            c if c.is_control() => ' ',
            c => c,
        })
        .collect();
    let cleaned = cleaned.split_whitespace().collect::<Vec<_>>().join(" ");
    let cleaned = extract::truncate(cleaned.trim_matches('.'), 120);
    crate::files::names::new_name(cleaned.trim()).ok()
}

/// A bookmark file's content (Windows' and most desktops' `.url`).
pub fn bookmark(url: &str) -> String {
    format!("[InternetShortcut]\r\nURL={url}\r\n")
}

/// The name a page's bookmark gets: its title, else its address.
pub fn bookmark_name(title: Option<&str>, url: &reqwest::Url) -> String {
    let base = title
        .and_then(file_name)
        .or_else(|| {
            file_name(&format!(
                "{}{}",
                url.host_str().unwrap_or_default(),
                url.path().trim_end_matches('/').replace('/', " ")
            ))
        })
        .unwrap_or_else(|| "Link".into());
    format!("{base}.url")
}

/// The name a downloaded file gets: the one it gave, else the last part of
/// its address, with an extension for its type when it has none.
pub fn download_name(fetched: &Fetched) -> String {
    let url = reqwest::Url::parse(&fetched.url).ok();
    let from_url = url.as_ref().and_then(|u| {
        u.path_segments()
            .and_then(|mut s| s.next_back().map(String::from))
            .map(|s| percent_decode(&s))
    });
    let name = fetched
        .filename
        .as_deref()
        .and_then(file_name)
        .or_else(|| from_url.as_deref().and_then(file_name))
        .unwrap_or_else(|| "Download".into());
    let has_extension = std::path::Path::new(&name).extension().is_some();
    match (&fetched.mime_type, has_extension) {
        (Some(mime), false) => {
            match mime_guess::get_mime_extensions_str(mime).and_then(|e| e.first()) {
                Some(ext) => format!("{name}.{ext}"),
                None => name,
            }
        }
        _ => name,
    }
}

fn percent_decode(s: &str) -> String {
    let hex = |b: u8| (b as char).to_digit(16).map(|d| d as u8);
    let bytes = s.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut i = 0;
    while i < bytes.len() {
        match (
            bytes[i],
            bytes.get(i + 1).and_then(|b| hex(*b)),
            bytes.get(i + 2).and_then(|b| hex(*b)),
        ) {
            (b'%', Some(high), Some(low)) => {
                out.push(high * 16 + low);
                i += 3;
            }
            (byte, _, _) => {
                out.push(byte);
                i += 1;
            }
        }
    }
    String::from_utf8_lossy(&out).into_owned()
}

/// A page's title and text, when it is a page with text.
pub async fn page(fetched: &Fetched) -> Option<extract::Document> {
    let mime = fetched.mime_type.as_deref().unwrap_or("text/html");
    if !(mime == "text/html" || mime == "application/xhtml+xml" || mime.starts_with("text/")) {
        return None;
    }
    match extract::document(fetched.bytes.clone(), "page.html", mime, extract::MAX_CHARS).await {
        Ok(document) => Some(document),
        Err(NoText::Failed(reason)) => {
            tracing::debug!("cannot read the page {}: {reason}", fetched.url);
            None
        }
        Err(_) => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn only_public_addresses_are_fetched() {
        for ip in [
            "127.0.0.1",
            "10.1.2.3",
            "192.168.1.1",
            "172.16.0.1",
            "169.254.169.254",
            "100.64.0.1",
            "0.0.0.0",
            "::1",
            "fe80::1",
            "fd00::1",
            "::ffff:127.0.0.1",
        ] {
            assert!(!is_public(ip.parse().unwrap()), "{ip}");
        }
        for ip in ["93.184.216.34", "2606:2800:220:1:248:1893:25c8:1946"] {
            assert!(is_public(ip.parse().unwrap()), "{ip}");
        }
        let url = |u: &str| reqwest::Url::parse(u).unwrap();
        assert!(!allowed(&url("http://127.0.0.1:8000/"), false));
        assert!(allowed(&url("http://127.0.0.1:8000/"), true));
        assert!(!allowed(&url("http://[::1]/"), false));
        assert!(allowed(&url("https://example.com/"), false));
        assert!(!allowed(&url("file:///etc/passwd"), true));
    }

    #[test]
    fn names_come_from_titles_headers_and_addresses() {
        let url = reqwest::Url::parse("https://example.com/recipes/bread/").unwrap();
        assert_eq!(
            bookmark_name(Some("Bread: a recipe / 2026"), &url),
            "Bread a recipe 2026.url"
        );
        assert_eq!(bookmark_name(None, &url), "example.com recipes bread.url");
        assert_eq!(
            disposition_name("attachment; filename=\"report 2025.pdf\""),
            Some("report 2025.pdf".into())
        );
        let fetched = |url: &str, mime: Option<&str>, filename: Option<&str>| Fetched {
            url: url.into(),
            mime_type: mime.map(String::from),
            filename: filename.map(String::from),
            bytes: vec![],
        };
        assert_eq!(
            download_name(&fetched(
                "https://x.org/files/Annual%20report.pdf",
                None,
                None
            )),
            "Annual report.pdf"
        );
        assert_eq!(
            download_name(&fetched(
                "https://x.org/get?id=1",
                Some("application/pdf"),
                None
            )),
            "get.pdf"
        );
        assert_eq!(
            download_name(&fetched(
                "https://x.org/",
                Some("application/pdf"),
                Some("a.pdf")
            )),
            "a.pdf"
        );
        assert_eq!(
            bookmark("https://x.org/"),
            "[InternetShortcut]\r\nURL=https://x.org/\r\n"
        );
    }
}
