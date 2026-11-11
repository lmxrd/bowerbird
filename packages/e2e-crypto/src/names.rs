//! Names inside an encrypted folder: AES-256-GCM under a key derived from
//! the folder key, with the parent folder's id as associated data, in
//! base64url. docs/E2E_SPEC.md §6.

use crate::{Error, VERSION, b64, check_version, hkdf, open, seal, unb64};
use unicode_normalization::UnicodeNormalization;
use uuid::Uuid;

/// The HKDF info of the name key.
pub const NAME_INFO: &[u8] = b"organizer-e2e/1/name";
/// The most bytes (UTF-8, NFC) a name may have: encrypted and encoded, it
/// must still fit the 255 bytes most filesystems allow.
pub const MAX_NAME_BYTES: usize = 158;

/// The key names are sealed with, from the folder key.
pub fn name_key(folder_key: &[u8; 32]) -> [u8; 32] {
    hkdf(folder_key, &[], NAME_INFO)
}

/// A name sealed for the folder `parent`, with the nonce given: version,
/// generation (4 bytes, big-endian), nonce, ciphertext and tag, in
/// unpadded base64url.
pub fn encrypt_with(
    folder_key: &[u8; 32],
    generation: u32,
    parent: Uuid,
    name: &str,
    nonce: &[u8; 12],
) -> Result<String, Error> {
    let name: String = name.nfc().collect();
    if name.is_empty() {
        return Err(Error::Malformed("an empty name"));
    }
    if name.len() > MAX_NAME_BYTES {
        return Err(Error::Malformed("a name of at most 158 bytes"));
    }
    let mut out = vec![VERSION];
    out.extend_from_slice(&generation.to_be_bytes());
    out.extend_from_slice(nonce);
    out.extend(seal(
        &name_key(folder_key),
        nonce,
        name.as_bytes(),
        parent.as_bytes(),
    ));
    Ok(b64(&out))
}

/// A name sealed for the folder `parent`, with a new nonce.
pub fn encrypt(
    folder_key: &[u8; 32],
    generation: u32,
    parent: Uuid,
    name: &str,
) -> Result<String, Error> {
    encrypt_with(folder_key, generation, parent, name, &crate::random())
}

/// The generation of the folder key a name was sealed with.
pub fn generation(encrypted: &str) -> Result<u32, Error> {
    let bytes = unb64(encrypted)?;
    let rest = check_version(&bytes)?;
    if rest.len() < 4 + 12 + 16 {
        return Err(Error::Malformed("too short for a name"));
    }
    Ok(u32::from_be_bytes(rest[..4].try_into().expect("4 bytes")))
}

/// The name out of [`encrypt`], with the key of its generation.
pub fn decrypt(folder_key: &[u8; 32], parent: Uuid, encrypted: &str) -> Result<String, Error> {
    let bytes = unb64(encrypted)?;
    let rest = check_version(&bytes)?;
    if rest.len() < 4 + 12 + 16 {
        return Err(Error::Malformed("too short for a name"));
    }
    let nonce: [u8; 12] = rest[4..16].try_into().expect("12 bytes");
    let plain = open(
        &name_key(folder_key),
        &nonce,
        &rest[16..],
        parent.as_bytes(),
    )?;
    String::from_utf8(plain).map_err(|_| Error::Malformed("not UTF-8"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn names_open_in_their_folder_only() {
        let key = crate::random_32();
        let parent = Uuid::new_v4();
        let sealed = encrypt(&key, 2, parent, "Taxes 2026.pdf").unwrap();
        assert_eq!(generation(&sealed).unwrap(), 2);
        assert_eq!(decrypt(&key, parent, &sealed).unwrap(), "Taxes 2026.pdf");
        assert_eq!(decrypt(&key, Uuid::new_v4(), &sealed), Err(Error::Decrypt));
        // Two seals of a name differ.
        assert_ne!(sealed, encrypt(&key, 2, parent, "Taxes 2026.pdf").unwrap());
    }

    #[test]
    fn the_longest_name_still_fits_a_filesystem() {
        let key = crate::random_32();
        let name = "é".repeat(MAX_NAME_BYTES / 2);
        let sealed = encrypt(&key, 1, Uuid::nil(), &name).unwrap();
        assert!(sealed.len() <= 255, "{}", sealed.len());
        assert!(encrypt(&key, 1, Uuid::nil(), &format!("{name}a")).is_err());
    }
}
