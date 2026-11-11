//! HTTP Message Signatures between OCM servers (RFC 9421, as §6 of
//! draft-ietf-ocm-open-cloud-mesh-07 profiles it): the components
//! `@method`, `@target-uri`, `content-digest` and `content-length` at
//! least, `created` and `keyid`, `tag="ocm"` (one signature with it, and
//! only one), the algorithm from the signer's JWK (Ed25519 here), and the
//! body bound by a `Content-Digest` (RFC 9530, sha-256).
//!
//! httpsig builds and checks the signature base and the signature; what
//! OCM adds (the tag, the mandatory components, freshness, the digest, the
//! key from the JWK Set) is checked here.

use axum::http::HeaderMap;
use httpsig::prelude::message_component::{
    DerivedComponentName, HttpMessageComponent, HttpMessageComponentId, HttpMessageComponentName,
};
use httpsig::prelude::{
    AlgorithmName, HttpSignatureBase, HttpSignatureHeaders, HttpSignatureParams, PublicKey,
    SecretKey,
};
use sha2::Digest;
use std::time::{SystemTime, UNIX_EPOCH};

/// The components every OCM signature covers (§6.2).
pub const COVERED: [&str; 4] = ["@method", "@target-uri", "content-digest", "content-length"];

/// How old a signature's `created` may be.
pub const FRESHNESS_SECS: u64 = 300;
/// How far in the future a `created` may be (clocks differ a little).
pub const SKEW_SECS: u64 = 60;

/// The `Content-Digest` of a body: its SHA-256.
pub fn content_digest(body: &[u8]) -> String {
    format!(
        "sha-256=:{}:",
        crate::util::base64_encode(&sha2::Sha256::digest(body))
    )
}

fn now() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0)
}

/// The headers that sign a request: `Content-Digest`, `Signature-Input`
/// and `Signature` (the client sets `Content-Length` from the body).
pub fn sign(
    seed: &[u8; 32],
    keyid: &str,
    method: &str,
    target_uri: &str,
    body: &[u8],
) -> Result<Vec<(&'static str, String)>, String> {
    sign_at(seed, keyid, method, target_uri, body, now())
}

/// [`sign`] with a given `created` (the tests sign stale requests).
pub fn sign_at(
    seed: &[u8; 32],
    keyid: &str,
    method: &str,
    target_uri: &str,
    body: &[u8],
    created: u64,
) -> Result<Vec<(&'static str, String)>, String> {
    let digest = content_digest(body);
    let values = [
        method.to_ascii_uppercase(),
        target_uri.to_string(),
        digest.clone(),
        body.len().to_string(),
    ];
    let ids = COVERED
        .iter()
        .map(|c| HttpMessageComponentId::try_from(*c))
        .collect::<Result<Vec<_>, _>>()
        .map_err(|e| e.to_string())?;
    let lines = ids
        .iter()
        .zip(values)
        .map(|(id, value)| HttpMessageComponent::try_from((id, &[value][..])))
        .collect::<Result<Vec<_>, _>>()
        .map_err(|e| e.to_string())?;
    let mut params = HttpSignatureParams::try_new(&ids).map_err(|e| e.to_string())?;
    params
        .set_created(created)
        .set_keyid(keyid)
        .set_alg(&AlgorithmName::Ed25519)
        .set_tag("ocm");
    let base = HttpSignatureBase::try_new(&lines, &params).map_err(|e| e.to_string())?;
    let key = SecretKey::from_bytes(&AlgorithmName::Ed25519, seed).map_err(|e| e.to_string())?;
    let headers = base
        .build_signature_headers(&key, Some("sig1"))
        .map_err(|e| e.to_string())?;
    Ok(vec![
        ("content-digest", digest),
        ("signature-input", headers.signature_input_header_value()),
        ("signature", headers.signature_header_value()),
    ])
}

/// A request's OCM signature, found by its `tag="ocm"` (§6.4): None when
/// it carries none (unsigned); an error when it carries more than one, or
/// the headers cannot be read.
pub fn ocm_signature(headers: &HeaderMap) -> Result<Option<HttpSignatureHeaders>, String> {
    let all = |name: &str| {
        headers
            .get_all(name)
            .iter()
            .filter_map(|v| v.to_str().ok())
            .collect::<Vec<_>>()
            .join(", ")
    };
    let (input, signature) = (all("signature-input"), all("signature"));
    if input.is_empty() && signature.is_empty() {
        return Ok(None);
    }
    let parsed = HttpSignatureHeaders::try_parse(&signature, &input)
        .map_err(|e| format!("the signature headers cannot be read: {e}"))?;
    let mut ocm: Vec<HttpSignatureHeaders> = parsed
        .into_values()
        .filter(|s| s.signature_params().tag.as_deref() == Some("ocm"))
        .collect();
    match ocm.len() {
        0 => Ok(None),
        1 => Ok(ocm.pop()),
        _ => Err("more than one signature carries tag=\"ocm\"".into()),
    }
}

/// What a received request is, for checking its signature.
pub struct Received<'a> {
    pub method: &'a str,
    /// The request's full URI as the sender addressed it (scheme,
    /// authority, path and query).
    pub target_uri: &'a str,
    pub headers: &'a HeaderMap,
    pub body: &'a [u8],
}

/// What a JWK says of a key.
#[derive(Clone, Debug, serde::Deserialize, serde::Serialize, PartialEq, Eq)]
pub struct Jwk {
    pub kty: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub crv: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub alg: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub kid: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub x: Option<String>,
    #[serde(default, rename = "use", skip_serializing_if = "Option::is_none")]
    pub use_: Option<String>,
}

impl Jwk {
    /// The Ed25519 public key it holds, when its `alg` is an acceptable
    /// asymmetric one we can verify (§6.3): `Ed25519`, or `EdDSA` on the
    /// Ed25519 curve.
    pub fn ed25519(&self) -> Option<[u8; 32]> {
        let ed = match self.alg.as_deref() {
            Some("Ed25519") => true,
            Some("EdDSA") => self.crv.as_deref() == Some("Ed25519"),
            _ => false,
        };
        if !ed || self.kty != "OKP" {
            return None;
        }
        let x = crate::util::base64url_decode(self.x.as_deref()?)?;
        x.try_into().ok()
    }
}

/// The parts of a signature to check before its key is fetched: the
/// mandatory components and parameters, freshness, and the body against
/// its digest and length. Gives the `keyid`.
pub fn check_shape(signature: &HttpSignatureHeaders, request: &Received) -> Result<String, String> {
    check_shape_at(signature, request, now())
}

fn check_shape_at(
    signature: &HttpSignatureHeaders,
    request: &Received,
    now: u64,
) -> Result<String, String> {
    let params = signature.signature_params();
    for needed in COVERED {
        let id = HttpMessageComponentId::try_from(needed).map_err(|e| e.to_string())?;
        if !params.covered_components.iter().any(|c| c.name == id.name) {
            return Err(format!("the signature does not cover {needed}"));
        }
    }
    let keyid = params.keyid.clone().ok_or("the signature has no keyid")?;
    let created = params.created.ok_or("the signature has no created")?;
    if created > now + SKEW_SECS {
        return Err("the signature was made in the future".into());
    }
    if created + FRESHNESS_SECS < now {
        return Err("the signature is too old".into());
    }
    if params.expires.is_some_and(|e| e < now) {
        return Err("the signature has expired".into());
    }
    if let Some(alg) = &params.alg
        && alg != "ed25519"
    {
        return Err(format!("the signature's alg {alg} is not its key's"));
    }
    let length = request
        .headers
        .get("content-length")
        .and_then(|v| v.to_str().ok())
        .and_then(|v| v.trim().parse::<usize>().ok());
    if length != Some(request.body.len()) {
        return Err("Content-Length does not match the body".into());
    }
    check_digest(request.headers, request.body)?;
    Ok(keyid)
}

/// The body against its `Content-Digest`: every algorithm we know must
/// match, and at least one must be there (§6.4).
pub fn check_digest(headers: &HeaderMap, body: &[u8]) -> Result<(), String> {
    let value = headers
        .get_all("content-digest")
        .iter()
        .filter_map(|v| v.to_str().ok())
        .collect::<Vec<_>>()
        .join(",");
    let mut known = 0;
    for member in value.split(',') {
        let Some((alg, digest)) = member.trim().split_once('=') else {
            continue;
        };
        let digest = digest.trim();
        let Some(digest) = digest
            .strip_prefix(':')
            .and_then(|d| d.strip_suffix(':'))
            .and_then(crate::util::base64_decode)
        else {
            return Err("Content-Digest cannot be read".into());
        };
        let ours = match alg.trim().to_ascii_lowercase().as_str() {
            "sha-256" => sha2::Sha256::digest(body).to_vec(),
            "sha-512" => sha2::Sha512::digest(body).to_vec(),
            _ => continue,
        };
        known += 1;
        if ours != digest {
            return Err("Content-Digest does not match the body".into());
        }
    }
    if known == 0 {
        return Err("no Content-Digest of a known algorithm".into());
    }
    Ok(())
}

/// Check the signature itself with the signer's key: the signature base
/// made again from the request, as its `Signature-Input` lists it.
pub fn verify(
    signature: &HttpSignatureHeaders,
    request: &Received,
    key: &[u8; 32],
) -> Result<(), String> {
    let params = signature.signature_params();
    let mut lines = Vec::new();
    for id in &params.covered_components {
        let values: Vec<String> = match &id.name {
            HttpMessageComponentName::Derived(derived) => {
                vec![derived_value(derived, request)?]
            }
            HttpMessageComponentName::HttpField(name) => {
                let values: Vec<String> = request
                    .headers
                    .get_all(name.as_str())
                    .iter()
                    .filter_map(|v| v.to_str().ok())
                    .map(|v| v.trim().to_string())
                    .collect();
                if values.is_empty() {
                    return Err(format!("the signed header {name} is missing"));
                }
                values
            }
        };
        lines.push(HttpMessageComponent::try_from((id, &values[..])).map_err(|e| e.to_string())?);
    }
    let base = HttpSignatureBase::try_new(&lines, params).map_err(|e| e.to_string())?;
    let key = PublicKey::from_bytes(&AlgorithmName::Ed25519, key).map_err(|e| e.to_string())?;
    base.verify_signature_headers(&key, signature)
        .map_err(|_| "the signature does not verify".to_string())
}

fn derived_value(name: &DerivedComponentName, request: &Received) -> Result<String, String> {
    let url = reqwest::Url::parse(request.target_uri).map_err(|e| e.to_string())?;
    let authority = match url.port() {
        Some(port) => format!("{}:{port}", url.host_str().unwrap_or_default()),
        None => url.host_str().unwrap_or_default().to_string(),
    };
    Ok(match name {
        DerivedComponentName::Method => request.method.to_string(),
        DerivedComponentName::TargetUri => request.target_uri.to_string(),
        DerivedComponentName::Authority => authority,
        DerivedComponentName::Scheme => url.scheme().to_string(),
        DerivedComponentName::Path => url.path().to_string(),
        DerivedComponentName::Query => format!("?{}", url.query().unwrap_or_default()),
        DerivedComponentName::RequestTarget => match url.query() {
            Some(q) => format!("{}?{q}", url.path()),
            None => url.path().to_string(),
        },
        other => return Err(format!("the component {other} is not taken")),
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use axum::http::HeaderValue;

    fn signed(seed: &[u8; 32], body: &[u8], created: u64) -> HeaderMap {
        let mut headers = HeaderMap::new();
        for (name, value) in sign_at(
            seed,
            "a.example#k1",
            "POST",
            "https://b.example/ocm/shares",
            body,
            created,
        )
        .unwrap()
        {
            headers.insert(name, HeaderValue::from_str(&value).unwrap());
        }
        headers.insert(
            "content-length",
            HeaderValue::from_str(&body.len().to_string()).unwrap(),
        );
        headers
    }

    fn public(seed: &[u8; 32]) -> [u8; 32] {
        ed25519_dalek::SigningKey::from_bytes(seed)
            .verifying_key()
            .to_bytes()
    }

    #[test]
    fn a_signature_verifies_and_binds_the_body() {
        let seed = [7u8; 32];
        let body = br#"{"shareWith":"bob@b.example"}"#;
        let headers = signed(&seed, body, now());
        let request = Received {
            method: "POST",
            target_uri: "https://b.example/ocm/shares",
            headers: &headers,
            body,
        };
        let signature = ocm_signature(&headers).unwrap().unwrap();
        let input = headers["signature-input"].to_str().unwrap();
        assert!(input.contains(r#"tag="ocm""#) && input.contains(r#"keyid="a.example#k1""#));
        assert!(input.contains(r#"alg="ed25519""#));
        assert_eq!(check_shape(&signature, &request).unwrap(), "a.example#k1");
        verify(&signature, &request, &public(&seed)).unwrap();
        // Another key.
        assert!(verify(&signature, &request, &public(&[8u8; 32])).is_err());
        // Another URI.
        let elsewhere = Received {
            target_uri: "https://c.example/ocm/shares",
            ..request
        };
        assert!(verify(&signature, &elsewhere, &public(&seed)).is_err());
        // Another body.
        let other = Received {
            body: br#"{"shareWith":"eve@b.example"}"#,
            ..request
        };
        assert!(check_shape(&signature, &other).is_err());
    }

    #[test]
    fn stale_or_future_signatures_are_refused() {
        let seed = [7u8; 32];
        let body = b"x";
        for created in [now() - FRESHNESS_SECS - 10, now() + SKEW_SECS + 10] {
            let headers = signed(&seed, body, created);
            let request = Received {
                method: "POST",
                target_uri: "https://b.example/ocm/shares",
                headers: &headers,
                body,
            };
            let signature = ocm_signature(&headers).unwrap().unwrap();
            assert!(check_shape(&signature, &request).is_err());
        }
    }

    #[test]
    fn only_one_ocm_signature_counts() {
        let headers = signed(&[7u8; 32], b"x", now());
        let mut twice = headers.clone();
        let input = headers["signature-input"].to_str().unwrap();
        let signature = headers["signature"].to_str().unwrap();
        twice.insert(
            "signature-input",
            HeaderValue::from_str(&format!("{input}, {}", input.replacen("sig1", "sig2", 1)))
                .unwrap(),
        );
        twice.insert(
            "signature",
            HeaderValue::from_str(&format!(
                "{signature}, {}",
                signature.replacen("sig1", "sig2", 1)
            ))
            .unwrap(),
        );
        assert!(ocm_signature(&twice).is_err());
        assert!(ocm_signature(&HeaderMap::new()).unwrap().is_none());
    }

    #[test]
    fn digests_must_all_match() {
        let body = b"hello";
        let mut headers = HeaderMap::new();
        headers.insert(
            "content-digest",
            HeaderValue::from_str(&content_digest(body)).unwrap(),
        );
        check_digest(&headers, body).unwrap();
        assert!(check_digest(&headers, b"other").is_err());
        let sha512 = crate::util::base64_encode(&sha2::Sha512::digest(b"other"));
        headers.insert(
            "content-digest",
            HeaderValue::from_str(&format!("{}, sha-512=:{sha512}:", content_digest(body)))
                .unwrap(),
        );
        assert!(check_digest(&headers, body).is_err(), "one mismatch fails");
        headers.insert("content-digest", HeaderValue::from_static("md5=:AAAA:"));
        assert!(check_digest(&headers, body).is_err(), "none known");
    }

    #[test]
    fn the_appendix_key_is_read_from_its_jwk() {
        let jwk: Jwk = serde_json::from_str(
            r#"{"kty":"OKP","crv":"Ed25519","alg":"Ed25519","kid":"sender.example.org#key1",
                "x":"AzeStPAdQ-vbi6bJVxcQjBPF2I7fDZdfDZ_XZ3J4Azs"}"#,
        )
        .unwrap();
        assert!(jwk.ed25519().is_some());
        let hs256 = Jwk {
            alg: Some("HS256".into()),
            ..jwk.clone()
        };
        assert!(hs256.ed25519().is_none(), "a symmetric alg is refused");
        let none = Jwk { alg: None, ..jwk };
        assert!(none.ed25519().is_none(), "a JWK must carry its alg");
    }
}
