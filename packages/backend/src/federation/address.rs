//! OCM addresses (`identifier@host[:port]`, draft-ietf-ocm-open-cloud-mesh
//! §2) and invite strings (§4.4.6).

/// An OCM address: an opaque identifier at a server. The identifier may
/// hold `@` itself (an email): the host is after the last one.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Address {
    pub identifier: String,
    /// `host` or `host:port`, lowercase.
    pub host: String,
}

impl Address {
    /// An address as typed or received; None when it is not one (no `@`,
    /// an empty part, a path, a scheme, spaces).
    pub fn parse(input: &str) -> Option<Address> {
        let input = input.trim();
        let (identifier, host) = input.rsplit_once('@')?;
        let host = host_of(host)?;
        if identifier.is_empty() || identifier.chars().any(char::is_control) {
            return None;
        }
        Some(Address {
            identifier: identifier.to_string(),
            host,
        })
    }

    pub fn new(identifier: &str, host: &str) -> Address {
        Address {
            identifier: identifier.to_string(),
            host: host.to_lowercase(),
        }
    }
}

impl std::fmt::Display for Address {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}@{}", self.identifier, self.host)
    }
}

/// A server's host as OCM names it (`host` or `host:port`, an IPv6
/// literal in brackets), lowercase; None when it is not one.
pub fn host_of(input: &str) -> Option<String> {
    let host = input.trim().to_lowercase();
    if host.is_empty()
        || host.len() > 300
        || host.contains(['/', '?', '#', '@', ' ', '\\'])
        || host.chars().any(char::is_control)
    {
        return None;
    }
    // A URL is a way to say "this server's host": checked by parsing one.
    let url = reqwest::Url::parse(&format!("https://{host}/")).ok()?;
    let name = url.host_str()?;
    Some(match url.port() {
        Some(port) => format!("{name}:{port}"),
        None => name.to_string(),
    })
}

/// An invite string: the token and the inviting server's host joined by
/// `@`, base64url without padding (§4.4.6).
pub fn invite_string(token: &str, host: &str) -> String {
    crate::util::base64url_encode(format!("{token}@{host}").as_bytes())
}

/// The token and the host of an invite string (with or without padding);
/// a link of the form `…?token=…&providerDomain=…` is taken too.
pub fn parse_invite(input: &str) -> Option<(String, String)> {
    let input = input.trim();
    if let Ok(url) = reqwest::Url::parse(input)
        && matches!(url.scheme(), "http" | "https")
    {
        let mut token = None;
        let mut provider = None;
        for (key, value) in url.query_pairs() {
            match key.as_ref() {
                "token" => token = Some(value.trim().to_string()),
                "providerDomain" => provider = Some(value.trim().to_string()),
                _ => {}
            }
        }
        let provider = match provider {
            Some(provider) => host_of(&provider)?,
            None => {
                let name = url.host_str()?;
                match url.port() {
                    Some(port) => format!("{name}:{port}"),
                    None => name.to_string(),
                }
            }
        };
        return token.filter(|t| !t.is_empty()).map(|t| (t, provider));
    }
    let decoded = crate::util::base64url_decode(input.trim_end_matches('='))?;
    let decoded = String::from_utf8(decoded).ok()?;
    let (token, host) = decoded.rsplit_once('@')?;
    if token.is_empty() {
        return None;
    }
    Some((token.to_string(), host_of(host)?))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn addresses_split_at_the_last_at_sign() {
        let a = Address::parse("nomen.nescio@example.org@Cloud.Example.org").unwrap();
        assert_eq!(a.identifier, "nomen.nescio@example.org");
        assert_eq!(a.host, "cloud.example.org");
        let a = Address::parse("alice@127.0.0.1:8080").unwrap();
        assert_eq!(a.host, "127.0.0.1:8080");
        assert_eq!(a.to_string(), "alice@127.0.0.1:8080");
        let a = Address::parse("bob@cloud.example.org:443").unwrap();
        assert_eq!(a.host, "cloud.example.org", "the default port is left out");
        for bad in [
            "",
            "alice",
            "@host",
            "alice@",
            "alice@host/path",
            "alice@https://host",
            "a b@host x",
        ] {
            assert!(Address::parse(bad).is_none(), "{bad}");
        }
    }

    #[test]
    fn invite_strings_are_the_specifications() {
        // The example of §4.4.6.
        let made = invite_string("a55a966e-15c1-4cb9-a39d-4e4c54399baf", "cloud.example.org");
        assert_eq!(
            made,
            "YTU1YTk2NmUtMTVjMS00Y2I5LWEzOWQtNGU0YzU0Mzk5YmFmQGNsb3VkLmV4YW1wbGUub3Jn"
        );
        let (token, host) = parse_invite(&made).unwrap();
        assert_eq!(token, "a55a966e-15c1-4cb9-a39d-4e4c54399baf");
        assert_eq!(host, "cloud.example.org");
        // A token with @ in it, and padding.
        let odd = crate::util::base64url_encode(b"t@k@en@host:8443");
        let (token, host) = parse_invite(&format!("{odd}==")).unwrap();
        assert_eq!((token.as_str(), host.as_str()), ("t@k@en", "host:8443"));
        let (token, host) =
            parse_invite("https://a.example/settings/contacts?token=abc&providerDomain=b.example")
                .unwrap();
        assert_eq!((token.as_str(), host.as_str()), ("abc", "b.example"));
        assert!(parse_invite("not an invite").is_none());
    }
}
