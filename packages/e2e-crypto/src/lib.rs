//! The reference implementation of Organizer's end-to-end-encrypted
//! folders (docs/E2E_SPEC.md, docs/M6E_DESIGN.md): the keys, the wrapping
//! of the private key and of the folder keys, the names and the contents.
//! The web, iOS and Android apps implement the same format with their
//! platform's primitives and check themselves against `vectors.json`,
//! which this crate writes (`cargo run -p organizer-e2e-crypto --example
//! vectors`) and checks (`tests/vectors.rs`).
//!
//! Every function here is pure but for the ones named `random_*` or
//! `generate`, which take their randomness from the operating system's
//! generator; the others take the random values as arguments, so that the
//! vectors are reproducible.

pub mod contents;
pub mod folder;
pub mod keys;
pub mod names;
pub mod secrets;
pub mod tool;
pub mod vectors;

use aes_gcm::aead::{Aead, KeyInit, Payload};
use aes_gcm::{Aes256Gcm, Nonce};
use hkdf::Hkdf;
use sha2::Sha256;

/// The version of the format, the first byte of everything this writes.
/// A reader refuses any other.
pub const VERSION: u8 = 1;

/// Why something cannot be read.
#[derive(Debug, PartialEq, Eq, thiserror::Error)]
pub enum Error {
    /// A version of the format this does not know.
    #[error("format version {0} is not supported")]
    Version(u8),
    /// Too short, or not the encoding expected.
    #[error("malformed: {0}")]
    Malformed(&'static str),
    /// The authentication failed: a wrong key, passphrase or recovery key,
    /// or bytes that were changed, cut, reordered or moved.
    #[error("cannot decrypt: wrong key, or the data was changed")]
    Decrypt,
    /// No key of this generation is known.
    #[error("no key for generation {0}")]
    NoKey(u32),
}

/// 32 random bytes from the operating system's generator.
pub fn random_32() -> [u8; 32] {
    random()
}

/// `N` random bytes from the operating system's generator.
pub fn random<const N: usize>() -> [u8; N] {
    use rand::Rng;
    let mut bytes = [0u8; N];
    rand::rng().fill_bytes(&mut bytes);
    bytes
}

/// HKDF-SHA256 to 32 bytes.
pub(crate) fn hkdf(ikm: &[u8], salt: &[u8], info: &[u8]) -> [u8; 32] {
    let mut okm = [0u8; 32];
    Hkdf::<Sha256>::new(Some(salt), ikm)
        .expand(info, &mut okm)
        .expect("32 bytes is a valid length");
    okm
}

/// AES-256-GCM seal: the ciphertext with its 16-byte tag.
pub(crate) fn seal(key: &[u8; 32], nonce: &[u8; 12], plaintext: &[u8], aad: &[u8]) -> Vec<u8> {
    Aes256Gcm::new(&(*key).into())
        .encrypt(
            &Nonce::from(*nonce),
            Payload {
                msg: plaintext,
                aad,
            },
        )
        .expect("within AES-GCM's limits")
}

/// AES-256-GCM open.
pub(crate) fn open(
    key: &[u8; 32],
    nonce: &[u8; 12],
    ciphertext: &[u8],
    aad: &[u8],
) -> Result<Vec<u8>, Error> {
    Aes256Gcm::new(&(*key).into())
        .decrypt(
            &Nonce::from(*nonce),
            Payload {
                msg: ciphertext,
                aad,
            },
        )
        .map_err(|_| Error::Decrypt)
}

/// Unpadded base64url, the encoding of every binary value in the API.
pub fn b64(bytes: &[u8]) -> String {
    use base64::Engine;
    base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(bytes)
}

/// The bytes of unpadded base64url.
pub fn unb64(text: &str) -> Result<Vec<u8>, Error> {
    use base64::Engine;
    base64::engine::general_purpose::URL_SAFE_NO_PAD
        .decode(text)
        .map_err(|_| Error::Malformed("not base64url"))
}

/// The first byte, checked to be [`VERSION`].
pub(crate) fn check_version(bytes: &[u8]) -> Result<&[u8], Error> {
    match bytes.split_first() {
        None => Err(Error::Malformed("empty")),
        Some((&VERSION, rest)) => Ok(rest),
        Some((&other, _)) => Err(Error::Version(other)),
    }
}
