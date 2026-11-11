//! A storage backend's secret sealed to its owner's identity key (private
//! credentials, docs/M7C_DESIGN.md §3): the folder keys' wrapping
//! (an ephemeral X25519 agreement, HKDF-SHA256, AES-256-GCM) with its own
//! `info`, the owner's id as associated data and the secret's UTF-8 bytes
//! as plaintext. docs/E2E_SPEC.md §11.

use crate::keys::{public_key, x25519};
use crate::{Error, VERSION, check_version, hkdf, open, seal};
use uuid::Uuid;

/// The HKDF info of a sealed secret.
pub const SECRET_INFO: &[u8] = b"organizer-e2e/1/backend-secret";
/// Version, ephemeral public key, nonce and tag around the secret.
pub const OVERHEAD: usize = 1 + 32 + 12 + 16;

/// The secret sealed to `recipient` (the owner's public key), with the
/// ephemeral private key and the nonce given.
pub fn seal_with(
    secret: &str,
    owner: Uuid,
    recipient: &[u8; 32],
    ephemeral: &[u8; 32],
    nonce: &[u8; 12],
) -> Result<Vec<u8>, Error> {
    let ephemeral_public = public_key(ephemeral);
    let shared = x25519(ephemeral, recipient)?;
    let key = sealing_key(&shared, &ephemeral_public, recipient);
    let mut out = Vec::with_capacity(OVERHEAD + secret.len());
    out.push(VERSION);
    out.extend_from_slice(&ephemeral_public);
    out.extend_from_slice(nonce);
    out.extend(seal(&key, nonce, secret.as_bytes(), owner.as_bytes()));
    Ok(out)
}

/// The secret sealed to `recipient`, with new random values.
pub fn seal_secret(secret: &str, owner: Uuid, recipient: &[u8; 32]) -> Result<Vec<u8>, Error> {
    seal_with(
        secret,
        owner,
        recipient,
        &crate::random_32(),
        &crate::random(),
    )
}

/// The secret out of its sealing, for the owner holding `private`.
pub fn open_secret(sealed: &[u8], owner: Uuid, private: &[u8; 32]) -> Result<String, Error> {
    let rest = check_version(sealed)?;
    if sealed.len() < OVERHEAD {
        return Err(Error::Malformed("a sealed secret is at least 61 bytes"));
    }
    let ephemeral_public: [u8; 32] = rest[..32].try_into().expect("32 bytes");
    let nonce: [u8; 12] = rest[32..44].try_into().expect("12 bytes");
    let shared = x25519(private, &ephemeral_public)?;
    let key = sealing_key(&shared, &ephemeral_public, &public_key(private));
    let plain = open(&key, &nonce, &rest[44..], owner.as_bytes())?;
    String::from_utf8(plain).map_err(|_| Error::Malformed("a secret is UTF-8"))
}

/// HKDF of the shared secret, salted with both public keys (the
/// ephemeral one first), as a folder key's wrapping.
fn sealing_key(shared: &[u8; 32], ephemeral_public: &[u8; 32], recipient: &[u8; 32]) -> [u8; 32] {
    let mut salt = [0u8; 64];
    salt[..32].copy_from_slice(ephemeral_public);
    salt[32..].copy_from_slice(recipient);
    hkdf(shared, &salt, SECRET_INFO)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::keys::Identity;

    #[test]
    fn a_sealed_secret_opens_for_its_owner_only() {
        let owner = Identity::generate();
        let id = Uuid::new_v4();
        let sealed = seal_secret("s3cret/κey", id, &owner.public).unwrap();
        assert_eq!(sealed.len(), OVERHEAD + "s3cret/κey".len());
        assert_eq!(
            open_secret(&sealed, id, &owner.private).unwrap(),
            "s3cret/κey"
        );
        assert_eq!(
            open_secret(&sealed, Uuid::new_v4(), &owner.private),
            Err(Error::Decrypt)
        );
        let other = Identity::generate();
        assert_eq!(
            open_secret(&sealed, id, &other.private),
            Err(Error::Decrypt)
        );
    }
}
