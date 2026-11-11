//! The device login of a desktop app (ARCHITECTURE.md §4.4, RFC 8252): the
//! system browser opens the server's `/api/auth/device/authorize` with a
//! PKCE challenge and a loopback redirect, `http://127.0.0.1:<port>/callback`
//! (the server sends codes only there or to the mobile apps' scheme, see
//! `allowed_redirect` in `routes/device.rs`); the browser comes back to a
//! listener of ours with a one-time code, exchanged with the verifier for
//! the device's token.

use crate::Error;
use crate::api::types::DeviceToken;
use sha2::{Digest, Sha256};
use std::time::Duration;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpListener;

/// A PKCE verifier and its S256 challenge (RFC 7636).
pub struct Pkce {
    pub verifier: String,
    pub challenge: String,
}

impl Pkce {
    pub fn new() -> Self {
        let bytes: [u8; 32] = rand::random();
        let verifier = base64url(&bytes);
        let challenge = challenge_of(&verifier);
        Pkce {
            verifier,
            challenge,
        }
    }
}

impl Default for Pkce {
    fn default() -> Self {
        Self::new()
    }
}

/// base64url without padding. Written out here: a few lines, where a
/// crate would be one more dependency for one call.
pub fn base64url(bytes: &[u8]) -> String {
    const ALPHABET: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789-_";
    let mut out = String::new();
    for chunk in bytes.chunks(3) {
        let b = [
            chunk[0],
            *chunk.get(1).unwrap_or(&0),
            *chunk.get(2).unwrap_or(&0),
        ];
        let n = (b[0] as u32) << 16 | (b[1] as u32) << 8 | b[2] as u32;
        for i in 0..chunk.len() + 1 {
            out.push(ALPHABET[(n >> (18 - 6 * i) & 63) as usize] as char);
        }
    }
    out
}

pub fn challenge_of(verifier: &str) -> String {
    base64url(&Sha256::digest(verifier.as_bytes()))
}

/// A value for a URL's query, percent-encoded.
pub fn percent_encode(value: &str) -> String {
    let mut out = String::new();
    for byte in value.bytes() {
        if byte.is_ascii_alphanumeric() || b"-._~".contains(&byte) {
            out.push(byte as char);
        } else {
            out.push_str(&format!("%{byte:02X}"));
        }
    }
    out
}

fn percent_decode(value: &str) -> String {
    let bytes = value.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut i = 0;
    while i < bytes.len() {
        match bytes[i] {
            b'%' if i + 2 < bytes.len() => {
                let hex = |b: u8| (b as char).to_digit(16);
                match (hex(bytes[i + 1]), hex(bytes[i + 2])) {
                    (Some(high), Some(low)) => {
                        out.push((high * 16 + low) as u8);
                        i += 3;
                        continue;
                    }
                    _ => out.push(b'%'),
                }
            }
            b'+' => out.push(b' '),
            b => out.push(b),
        }
        i += 1;
    }
    String::from_utf8_lossy(&out).into_owned()
}

/// A server address as typed: `https://` when no scheme is given, without
/// a trailing slash.
pub fn normalize_server(address: &str) -> Result<String, Error> {
    let address = address.trim().trim_end_matches('/');
    if address.is_empty() {
        return Err(Error::Invalid("Enter the server's address".into()));
    }
    let address = if address.contains("://") {
        address.to_string()
    } else {
        format!("https://{address}")
    };
    if !(address.starts_with("https://") || address.starts_with("http://")) {
        return Err(Error::Invalid("The address must be http or https".into()));
    }
    Ok(address)
}

/// The listener the browser comes back to.
pub struct Loopback {
    listener: TcpListener,
    port: u16,
}

impl Loopback {
    /// A listener on a free port of 127.0.0.1.
    pub async fn bind() -> Result<Self, Error> {
        let listener = TcpListener::bind(("127.0.0.1", 0)).await?;
        let port = listener.local_addr()?.port();
        Ok(Loopback { listener, port })
    }

    pub fn redirect_uri(&self) -> String {
        format!("http://127.0.0.1:{}/callback", self.port)
    }

    /// The code the browser brings back, within `timeout`. Requests for
    /// other paths (a favicon) are answered 404 and waited past.
    pub async fn code(self, timeout: Duration) -> Result<String, Error> {
        tokio::time::timeout(timeout, async {
            loop {
                let (mut stream, _) = self.listener.accept().await?;
                let mut buffer = vec![0u8; 8192];
                let mut read = 0;
                // The request line and headers; the body (none) is not read.
                while read < buffer.len() {
                    let n = stream.read(&mut buffer[read..]).await?;
                    if n == 0 {
                        break;
                    }
                    read += n;
                    if buffer[..read].windows(4).any(|w| w == b"\r\n\r\n") {
                        break;
                    }
                }
                let request = String::from_utf8_lossy(&buffer[..read]);
                let target = request
                    .lines()
                    .next()
                    .and_then(|line| line.split(' ').nth(1))
                    .unwrap_or("");
                let Some(query) = target.strip_prefix("/callback?") else {
                    let _ = stream
                        .write_all(b"HTTP/1.1 404 Not Found\r\ncontent-length: 0\r\nconnection: close\r\n\r\n")
                        .await;
                    continue;
                };
                let code = query
                    .split('&')
                    .find_map(|pair| pair.strip_prefix("code="))
                    .map(percent_decode);
                let body = match code {
                    Some(_) => SIGNED_IN_PAGE,
                    None => FAILED_PAGE,
                };
                let response = format!(
                    "HTTP/1.1 200 OK\r\ncontent-type: text/html; charset=utf-8\r\ncontent-length: {}\r\nconnection: close\r\ncache-control: no-store\r\n\r\n{body}",
                    body.len()
                );
                let _ = stream.write_all(response.as_bytes()).await;
                let _ = stream.shutdown().await;
                return code.ok_or_else(|| Error::Invalid("The sign-in was not allowed".into()));
            }
        })
        .await
        .map_err(|_| Error::Invalid("The sign-in took too long: try again".into()))?
    }
}

const SIGNED_IN_PAGE: &str = "<!doctype html><meta charset=utf-8><title>Organizer</title><body style=\"font-family:system-ui;text-align:center;padding:4em\"><h1>Signed in</h1><p>You can close this tab and go back to Organizer.</p>";
const FAILED_PAGE: &str = "<!doctype html><meta charset=utf-8><title>Organizer</title><body style=\"font-family:system-ui;text-align:center;padding:4em\"><h1>Not signed in</h1><p>Go back to Organizer and try again.</p>";

/// Where the browser goes to sign the device in.
pub fn authorize_url(
    server: &str,
    redirect_uri: &str,
    challenge: &str,
    device_name: &str,
) -> String {
    format!(
        "{server}/api/auth/device/authorize?redirect_uri={}&code_challenge={}&device_name={}",
        percent_encode(redirect_uri),
        percent_encode(challenge),
        percent_encode(device_name)
    )
}

/// The device's token for its one-time code.
pub async fn exchange(server: &str, code: &str, verifier: &str) -> Result<String, Error> {
    let client = crate::remote::api_client(server, None)?;
    let created = client
        .device_token(&DeviceToken {
            code: code.to_string(),
            code_verifier: verifier.to_string(),
        })
        .await
        .map_err(Error::from_api)?;
    Ok(created.into_inner().token)
}

/// Who the token belongs to: their email.
pub async fn whoami(server: &str, token: &str) -> Result<String, Error> {
    let client = crate::remote::api_client(server, Some(token))?;
    let session = client.get_session().await.map_err(Error::from_api)?;
    session
        .into_inner()
        .user
        .map(|u| u.email)
        .ok_or(Error::Unauthorized)
}

/// Revoke the device's token on the server (`DELETE /api/auth/device`).
pub async fn sign_out(server: &str, token: &str) -> Result<(), Error> {
    let client = crate::remote::api_client(server, Some(token))?;
    client.sign_out_device().await.map_err(Error::from_api)?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn pkce_follows_rfc_7636() {
        assert_eq!(
            challenge_of("dBjftJeZ4CVP-mB92K27uhbUJU1p1r_wW1gFWFOEjXk"),
            "E9Melhoa2OwvFrEMTJguCHaoeK1t8URWbuGJSstw-cM"
        );
        let pkce = Pkce::new();
        assert_eq!(pkce.verifier.len(), 43);
        assert_eq!(pkce.challenge, challenge_of(&pkce.verifier));
    }

    #[test]
    fn server_addresses_are_normalized() {
        assert_eq!(
            normalize_server("files.example.com/").unwrap(),
            "https://files.example.com"
        );
        assert_eq!(
            normalize_server("http://127.0.0.1:8000").unwrap(),
            "http://127.0.0.1:8000"
        );
        assert!(normalize_server("ftp://x").is_err());
        assert!(normalize_server(" ").is_err());
    }

    #[tokio::test]
    async fn the_loopback_listener_takes_the_code() {
        let loopback = Loopback::bind().await.unwrap();
        let uri = loopback.redirect_uri();
        assert!(uri.starts_with("http://127.0.0.1:"));
        let port: u16 = uri
            .trim_start_matches("http://127.0.0.1:")
            .trim_end_matches("/callback")
            .parse()
            .unwrap();
        let waiting = tokio::spawn(loopback.code(Duration::from_secs(5)));
        let mut stream = tokio::net::TcpStream::connect(("127.0.0.1", port))
            .await
            .unwrap();
        stream
            .write_all(b"GET /favicon.ico HTTP/1.1\r\nhost: x\r\n\r\n")
            .await
            .unwrap();
        let mut stream = tokio::net::TcpStream::connect(("127.0.0.1", port))
            .await
            .unwrap();
        stream
            .write_all(b"GET /callback?code=ab%2Fc HTTP/1.1\r\nhost: x\r\n\r\n")
            .await
            .unwrap();
        assert_eq!(waiting.await.unwrap().unwrap(), "ab/c");
    }
}
