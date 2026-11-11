//! A folder key wrapped for a member: an ephemeral X25519 agreement with
//! their public key, HKDF-SHA256, AES-256-GCM with the folder's id and the
//! key's generation as associated data. docs/E2E_SPEC.md §4.

use crate::keys::{public_key, x25519};
use crate::{Error, VERSION, check_version, hkdf, open, seal};
use uuid::Uuid;

/// The HKDF info of a wrapping.
pub const WRAP_INFO: &[u8] = b"organizer-e2e/1/folder-key";
/// The length of a wrapped folder key: version, ephemeral public key,
/// nonce, the key and its tag.
pub const WRAPPED_LEN: usize = 1 + 32 + 12 + 32 + 16;

/// The associated data: the folder's id (16 bytes) and the generation
/// (4 bytes, big-endian).
pub fn wrap_ad(folder: Uuid, generation: u32) -> [u8; 20] {
    let mut ad = [0u8; 20];
    ad[..16].copy_from_slice(folder.as_bytes());
    ad[16..].copy_from_slice(&generation.to_be_bytes());
    ad
}

/// The folder key wrapped for `recipient`, with the ephemeral private key
/// and the nonce given.
pub fn wrap_with(
    folder_key: &[u8; 32],
    folder: Uuid,
    generation: u32,
    recipient: &[u8; 32],
    ephemeral: &[u8; 32],
    nonce: &[u8; 12],
) -> Result<Vec<u8>, Error> {
    let ephemeral_public = public_key(ephemeral);
    let shared = x25519(ephemeral, recipient)?;
    let key = wrapping_key(&shared, &ephemeral_public, recipient);
    let mut out = Vec::with_capacity(WRAPPED_LEN);
    out.push(VERSION);
    out.extend_from_slice(&ephemeral_public);
    out.extend_from_slice(nonce);
    out.extend(seal(&key, nonce, folder_key, &wrap_ad(folder, generation)));
    Ok(out)
}

/// The folder key wrapped for `recipient`, with new random values.
pub fn wrap(
    folder_key: &[u8; 32],
    folder: Uuid,
    generation: u32,
    recipient: &[u8; 32],
) -> Result<Vec<u8>, Error> {
    wrap_with(
        folder_key,
        folder,
        generation,
        recipient,
        &crate::random_32(),
        &crate::random(),
    )
}

/// The folder key out of a wrapping for the holder of `private`.
pub fn unwrap(
    wrapped: &[u8],
    folder: Uuid,
    generation: u32,
    private: &[u8; 32],
) -> Result<[u8; 32], Error> {
    let rest = check_version(wrapped)?;
    if wrapped.len() != WRAPPED_LEN {
        return Err(Error::Malformed("a wrapped folder key is 93 bytes"));
    }
    let ephemeral_public: [u8; 32] = rest[..32].try_into().expect("32 bytes");
    let nonce: [u8; 12] = rest[32..44].try_into().expect("12 bytes");
    let shared = x25519(private, &ephemeral_public)?;
    let key = wrapping_key(&shared, &ephemeral_public, &public_key(private));
    let folder_key = open(&key, &nonce, &rest[44..], &wrap_ad(folder, generation))?;
    Ok(folder_key.try_into().expect("32 bytes"))
}

/// HKDF of the shared secret, salted with both public keys (the
/// ephemeral one first).
fn wrapping_key(shared: &[u8; 32], ephemeral_public: &[u8; 32], recipient: &[u8; 32]) -> [u8; 32] {
    let mut salt = [0u8; 64];
    salt[..32].copy_from_slice(ephemeral_public);
    salt[32..].copy_from_slice(recipient);
    hkdf(shared, &salt, WRAP_INFO)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::keys::Identity;

    #[test]
    fn a_wrapped_key_opens_for_its_member_and_folder_only() {
        let member = Identity::generate();
        let key = crate::random_32();
        let folder = Uuid::new_v4();
        let wrapped = wrap(&key, folder, 3, &member.public).unwrap();
        assert_eq!(wrapped.len(), WRAPPED_LEN);
        assert_eq!(unwrap(&wrapped, folder, 3, &member.private).unwrap(), key);
        assert_eq!(
            unwrap(&wrapped, Uuid::new_v4(), 3, &member.private),
            Err(Error::Decrypt)
        );
        assert_eq!(
            unwrap(&wrapped, folder, 2, &member.private),
            Err(Error::Decrypt)
        );
        let other = Identity::generate();
        assert_eq!(
            unwrap(&wrapped, folder, 3, &other.private),
            Err(Error::Decrypt)
        );
    }
}
