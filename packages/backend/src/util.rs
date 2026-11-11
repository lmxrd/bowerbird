use rand::Rng;
use sha2::{Digest, Sha256};

/// The hex-encoded SHA-256 of a string.
pub fn sha256_hex(input: &str) -> String {
    let mut hasher = Sha256::new();
    hasher.update(input.as_bytes());
    hex::encode(hasher.finalize())
}

/// `n` random bytes, hex encoded.
pub fn random_hex(n: usize) -> String {
    let mut bytes = vec![0u8; n];
    rand::rng().fill_bytes(&mut bytes);
    hex::encode(bytes)
}

/// Standard base64 with padding (for `data:` URIs). The decoder for Basic
/// authentication is its pair in `dav.rs`.
pub fn base64_encode(bytes: &[u8]) -> String {
    const ALPHABET: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
    let mut out = String::with_capacity(bytes.len().div_ceil(3) * 4);
    for chunk in bytes.chunks(3) {
        let n = (chunk[0] as u32) << 16
            | (*chunk.get(1).unwrap_or(&0) as u32) << 8
            | *chunk.get(2).unwrap_or(&0) as u32;
        for i in 0..4 {
            if i <= chunk.len() {
                out.push(ALPHABET[(n >> (18 - 6 * i) & 63) as usize] as char);
            } else {
                out.push('=');
            }
        }
    }
    out
}

/// A field where `null` means the same as leaving it out: its default.
/// Generated clients send `null` for an unset optional field.
pub fn null_as_default<'de, D, T>(deserializer: D) -> Result<T, D::Error>
where
    D: serde::Deserializer<'de>,
    T: serde::Deserialize<'de> + Default,
{
    use serde::Deserialize;
    Ok(Option::<T>::deserialize(deserializer)?.unwrap_or_default())
}

/// A field where `null` is a value of its own, apart from leaving it out:
/// `Some(None)` is "set to none", `None` (with `#[serde(default)]`) is "left
/// as it is".
pub fn present<'de, D, T>(deserializer: D) -> Result<Option<Option<T>>, D::Error>
where
    D: serde::Deserializer<'de>,
    T: serde::Deserialize<'de>,
{
    use serde::Deserialize;
    Option::<T>::deserialize(deserializer).map(Some)
}

#[cfg(test)]
mod tests {
    #[test]
    fn base64_is_standard() {
        assert_eq!(super::base64_encode(b""), "");
        assert_eq!(super::base64_encode(b"a"), "YQ==");
        assert_eq!(super::base64_encode(b"ab"), "YWI=");
        assert_eq!(super::base64_encode(b"user:org_abc"), "dXNlcjpvcmdfYWJj");
    }
}

/// Unpadded base64url (RFC 4648 §5), as the end-to-end encryption's keys
/// are sent; None for anything else. Written here rather than adding a
/// crate for a dozen lines, as `dav.rs` does for Basic authentication.
pub fn base64url_decode(input: &str) -> Option<Vec<u8>> {
    fn value(c: u8) -> Option<u32> {
        Some(match c {
            b'A'..=b'Z' => c - b'A',
            b'a'..=b'z' => c - b'a' + 26,
            b'0'..=b'9' => c - b'0' + 52,
            b'-' => 62,
            b'_' => 63,
            _ => return None,
        } as u32)
    }
    let input = input.as_bytes();
    if input.len() % 4 == 1 {
        return None;
    }
    let mut out = Vec::with_capacity(input.len() * 3 / 4);
    for chunk in input.chunks(4) {
        let mut n = 0u32;
        for (i, c) in chunk.iter().enumerate() {
            n |= value(*c)? << (18 - 6 * i);
        }
        let bytes = n.to_be_bytes();
        out.extend_from_slice(&bytes[1..chunk.len()]);
    }
    Some(out)
}

/// Unpadded base64url (RFC 4648 §5): JWKs and OCM's invite strings.
pub fn base64url_encode(bytes: &[u8]) -> String {
    base64_encode(bytes)
        .trim_end_matches('=')
        .replace('+', "-")
        .replace('/', "_")
}

/// Standard base64, with or without its padding; None for anything else.
pub fn base64_decode(input: &str) -> Option<Vec<u8>> {
    let input = input.trim().trim_end_matches('=');
    if input.contains(['-', '_']) {
        return None;
    }
    base64url_decode(&input.replace('+', "-").replace('/', "_"))
}

/// A value for a URL's query or an `application/x-www-form-urlencoded`
/// body: every byte but the unreserved ones percent-encoded.
pub fn form_encode(value: &str) -> String {
    let mut out = String::with_capacity(value.len());
    for byte in value.bytes() {
        if byte.is_ascii_alphanumeric() || b"-._~".contains(&byte) {
            out.push(byte as char);
        } else {
            out.push_str(&format!("%{byte:02X}"));
        }
    }
    out
}

#[cfg(test)]
mod encoding_tests {
    #[test]
    fn base64url_and_base64_round_trip() {
        let bytes = [0u8, 1, 2, 250, 251, 252, 253, 254, 255];
        let url = super::base64url_encode(&bytes);
        assert!(!url.contains(['+', '/', '=']));
        assert_eq!(super::base64url_decode(&url).unwrap(), bytes);
        let standard = super::base64_encode(&bytes);
        assert_eq!(super::base64_decode(&standard).unwrap(), bytes);
        assert_eq!(super::base64_decode("YQ").unwrap(), b"a");
        assert!(super::base64_decode("YQ-_").is_none());
        assert_eq!(super::form_encode("a b&c=é"), "a%20b%26c%3D%C3%A9");
    }
}

#[cfg(test)]
mod base64url_tests {
    #[test]
    fn base64url_is_decoded() {
        assert_eq!(
            super::base64url_decode("AQID_-8").unwrap(),
            [1, 2, 3, 255, 239]
        );
        assert_eq!(super::base64url_decode("").unwrap(), Vec::<u8>::new());
        assert!(super::base64url_decode("AQID+").is_none());
        assert!(super::base64url_decode("AQIDa").is_none());
    }
}
