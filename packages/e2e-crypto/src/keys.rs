//! A user's identity key pair (X25519) and its private key wrapped twice:
//! under the passphrase (PBKDF2-HMAC-SHA256) and under the recovery key
//! (HKDF-SHA256). docs/E2E_SPEC.md §2 and §3.

use crate::{Error, VERSION, check_version, hkdf, open, seal};
use curve25519_dalek::montgomery::MontgomeryPoint;
use unicode_normalization::UnicodeNormalization;

/// The PBKDF2 iterations of a new or changed passphrase. A reader takes
/// the count stored with the key, so that it can be raised later.
pub const ITERATIONS: u32 = 600_000;
/// The fewest iterations a reader accepts: stored parameters are the
/// server's to change, and a low count would make a guess cheap.
pub const MIN_ITERATIONS: u32 = 100_000;
/// The associated data of both wrappings of the private key.
pub const PRIVATE_KEY_AD: &[u8] = b"organizer-e2e/1/private-key";
/// The HKDF info of the recovery key's wrapping key.
pub const RECOVERY_INFO: &[u8] = b"organizer-e2e/1/recovery";

/// A user's X25519 key pair.
#[derive(Clone)]
pub struct Identity {
    pub private: [u8; 32],
    pub public: [u8; 32],
}

impl Identity {
    /// The pair of a private key.
    pub fn from_private(private: [u8; 32]) -> Self {
        Identity {
            private,
            public: public_key(&private),
        }
    }

    /// A new pair.
    pub fn generate() -> Self {
        Self::from_private(crate::random_32())
    }
}

/// X25519's public key of a private key (RFC 7748: the clamped private
/// key times the base point).
pub fn public_key(private: &[u8; 32]) -> [u8; 32] {
    MontgomeryPoint::mul_base_clamped(*private).to_bytes()
}

/// X25519 (RFC 7748). An all-zero result (a public key of small order) is
/// refused.
pub fn x25519(private: &[u8; 32], public: &[u8; 32]) -> Result<[u8; 32], Error> {
    let shared = MontgomeryPoint(*public).mul_clamped(*private).to_bytes();
    if shared == [0u8; 32] {
        return Err(Error::Malformed("a public key of small order"));
    }
    Ok(shared)
}

/// The passphrase's bytes: UTF-8 of its NFC form, so that the same
/// passphrase typed on any system is the same key.
pub fn passphrase_bytes(passphrase: &str) -> Vec<u8> {
    passphrase.nfc().collect::<String>().into_bytes()
}

/// PBKDF2-HMAC-SHA256 of the passphrase, 32 bytes.
pub fn passphrase_key(passphrase: &str, salt: &[u8], iterations: u32) -> [u8; 32] {
    let mut key = [0u8; 32];
    pbkdf2::pbkdf2_hmac::<sha2::Sha256>(&passphrase_bytes(passphrase), salt, iterations, &mut key);
    key
}

/// The wrapping key of the recovery key.
pub fn recovery_wrapping_key(recovery: &[u8; 32]) -> [u8; 32] {
    hkdf(recovery, &[], RECOVERY_INFO)
}

/// The private key sealed under a wrapping key: version, nonce, ciphertext
/// and tag (1 + 12 + 32 + 16 bytes).
pub fn wrap_private(wrapping_key: &[u8; 32], private: &[u8; 32], nonce: &[u8; 12]) -> Vec<u8> {
    let mut out = vec![VERSION];
    out.extend_from_slice(nonce);
    out.extend(seal(wrapping_key, nonce, private, PRIVATE_KEY_AD));
    out
}

/// The private key out of [`wrap_private`]'s bytes.
pub fn unwrap_private(wrapping_key: &[u8; 32], wrapped: &[u8]) -> Result<[u8; 32], Error> {
    let rest = check_version(wrapped)?;
    if rest.len() != 12 + 32 + 16 {
        return Err(Error::Malformed("a wrapped private key is 61 bytes"));
    }
    let nonce: [u8; 12] = rest[..12].try_into().expect("12 bytes");
    let private = open(wrapping_key, &nonce, &rest[12..], PRIVATE_KEY_AD)?;
    Ok(private.try_into().expect("32 bytes"))
}

/// The private key wrapped under the passphrase, with a new nonce.
pub fn wrap_with_passphrase(
    private: &[u8; 32],
    passphrase: &str,
    salt: &[u8],
    iterations: u32,
) -> Vec<u8> {
    wrap_private(
        &passphrase_key(passphrase, salt, iterations),
        private,
        &crate::random(),
    )
}

/// The private key out of its passphrase wrapping.
pub fn unwrap_with_passphrase(
    wrapped: &[u8],
    passphrase: &str,
    salt: &[u8],
    iterations: u32,
) -> Result<[u8; 32], Error> {
    if iterations < MIN_ITERATIONS {
        return Err(Error::Malformed("too few PBKDF2 iterations"));
    }
    unwrap_private(&passphrase_key(passphrase, salt, iterations), wrapped)
}

/// The private key out of its recovery key wrapping.
pub fn unwrap_with_recovery(wrapped: &[u8], recovery: &[u8; 32]) -> Result<[u8; 32], Error> {
    unwrap_private(&recovery_wrapping_key(recovery), wrapped)
}

/// The recovery key's alphabet: RFC 4648 base32.
const BASE32: &[u8; 32] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZ234567";

/// The recovery key as shown: 52 base32 characters (RFC 4648, no padding;
/// the last one carries one bit and four zero bits) in 13 groups of 4,
/// joined by `-`.
pub fn encode_recovery(recovery: &[u8; 32]) -> String {
    let text = base32::encode(base32::Alphabet::Rfc4648 { padding: false }, recovery);
    debug_assert_eq!(text.len(), 52);
    text.as_bytes()
        .chunks(4)
        .map(|c| std::str::from_utf8(c).expect("ascii"))
        .collect::<Vec<_>>()
        .join("-")
}

/// The recovery key as typed: dashes and white space are ignored, and
/// lowercase is read as uppercase. Anything else is refused, including
/// unused bits that are not zero (a typo).
pub fn decode_recovery(text: &str) -> Result<[u8; 32], Error> {
    let clean: String = text
        .chars()
        .filter(|c| *c != '-' && !c.is_whitespace())
        .map(|c| c.to_ascii_uppercase())
        .collect();
    if clean.len() != 52 || !clean.bytes().all(|b| BASE32.contains(&b)) {
        return Err(Error::Malformed("a recovery key is 52 base32 characters"));
    }
    let bytes = base32::decode(base32::Alphabet::Rfc4648 { padding: false }, &clean)
        .ok_or(Error::Malformed("not base32"))?;
    let key: [u8; 32] = bytes
        .try_into()
        .map_err(|_| Error::Malformed("a recovery key is 32 bytes"))?;
    // The four bits after the key must be zero: encoding again gives the
    // same text.
    if encode_recovery(&key).replace('-', "") != clean {
        return Err(Error::Malformed("not a recovery key"));
    }
    Ok(key)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn x25519_matches_rfc_7748() {
        // RFC 7748 §6.1.
        let alice: [u8; 32] =
            hex("77076d0a7318a57d3c16c17251b26645df4c2f87ebc0992ab177fba51db92c2a");
        let bob: [u8; 32] = hex("5dab087e624a8a4b79e17f8b83800ee66f3bb1292618b6fd1c2f8b27ff88e0eb");
        assert_eq!(
            public_key(&alice),
            hex::<32>("8520f0098930a754748b7ddcb43ef75a0dbf3a0d26381af4eba4a98eaa9b4e6a")
        );
        assert_eq!(
            x25519(&alice, &public_key(&bob)).unwrap(),
            hex::<32>("4a5d9d5ba4ce2de1728e3bf480350f25e07e21c947d19e3376f09b3c1e161742")
        );
        assert!(x25519(&alice, &[0; 32]).is_err());
    }

    #[test]
    fn recovery_keys_read_back_and_refuse_typos() {
        let key = crate::random_32();
        let text = encode_recovery(&key);
        assert_eq!(text.len(), 52 + 12);
        assert_eq!(decode_recovery(&text).unwrap(), key);
        assert_eq!(
            decode_recovery(&text.to_lowercase().replace('-', " ")).unwrap(),
            key
        );
        let mut short = text.clone();
        short.pop();
        assert!(decode_recovery(&short).is_err());
        // The last character carries one bit and four zero bits: A or Q.
        assert!(text.ends_with('A') || text.ends_with('Q'));
        let typo = format!("{}B", &text[..text.len() - 1]);
        assert!(decode_recovery(&typo).is_err());
        let typo = format!("1{}", &text[1..]);
        assert!(decode_recovery(&typo).is_err());
    }

    #[test]
    fn the_private_key_unwraps_with_the_passphrase_or_the_recovery_key() {
        let id = Identity::generate();
        let salt = [7u8; 16];
        let wrapped = wrap_with_passphrase(&id.private, "e\u{301}té", &salt, MIN_ITERATIONS);
        // The same passphrase in another Unicode form.
        assert_eq!(
            unwrap_with_passphrase(&wrapped, "été", &salt, MIN_ITERATIONS).unwrap(),
            id.private
        );
        assert_eq!(
            unwrap_with_passphrase(&wrapped, "ete", &salt, MIN_ITERATIONS),
            Err(Error::Decrypt)
        );
        assert!(unwrap_with_passphrase(&wrapped, "été", &salt, 1000).is_err());
        let recovery = crate::random_32();
        let wrapped = wrap_private(
            &recovery_wrapping_key(&recovery),
            &id.private,
            &crate::random(),
        );
        assert_eq!(
            unwrap_with_recovery(&wrapped, &recovery).unwrap(),
            id.private
        );
        assert_eq!(
            unwrap_with_recovery(&wrapped, &crate::random_32()),
            Err(Error::Decrypt)
        );
    }

    fn hex<const N: usize>(text: &str) -> [u8; N] {
        let bytes: Vec<u8> = (0..text.len())
            .step_by(2)
            .map(|i| u8::from_str_radix(&text[i..i + 2], 16).unwrap())
            .collect();
        bytes.try_into().unwrap()
    }
}
