//! The test vectors every client checks itself against (`vectors.json`
//! at the crate's root, described in docs/E2E_SPEC.md §9). Made from fixed
//! inputs, so that writing them again gives the same file: the crate's
//! test `vectors_are_up_to_date` checks that it does.

use crate::{contents, folder, keys, names, secrets};
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use uuid::Uuid;

/// A fixed "random" value: SHA-256 of a label, cut to `N` bytes.
fn fixed<const N: usize>(label: &str) -> [u8; N] {
    let digest = Sha256::digest(label.as_bytes());
    digest[..N].try_into().expect("N <= 32")
}

/// The contents of a vector of `len` bytes: byte `i` is `i % 251`.
pub fn pattern(len: usize) -> Vec<u8> {
    (0..len).map(|i| (i % 251) as u8).collect()
}

/// The PBKDF2 iterations of the vectors: the fewest a reader accepts, so
/// that the tests stay quick.
pub const VECTOR_ITERATIONS: u32 = keys::MIN_ITERATIONS;

/// Every vector, as written to `vectors.json`.
pub fn generate() -> Value {
    let hex = |b: &[u8]| hex::encode(b);

    let identities: Vec<Value> = ["alice", "bob"]
        .iter()
        .map(|who| {
            let id = keys::Identity::from_private(fixed(&format!("identity {who}")));
            json!({ "private": hex(&id.private), "public": hex(&id.public) })
        })
        .collect();

    let passphrases: Vec<Value> = [
        ("correct horse battery staple", "salt 1"),
        // Decomposed: the NFC form is what is derived from.
        ("Cre\u{300}me bru\u{302}le\u{301}e \u{1F510}", "salt 2"),
    ]
    .iter()
    .map(|(passphrase, salt_label)| {
        let salt: [u8; 16] = fixed(salt_label);
        let private: [u8; 32] = fixed(&format!("private of {salt_label}"));
        let nonce: [u8; 12] = fixed(&format!("nonce of {salt_label}"));
        let key = keys::passphrase_key(passphrase, &salt, VECTOR_ITERATIONS);
        json!({
            "passphrase": passphrase,
            "salt": hex(&salt),
            "iterations": VECTOR_ITERATIONS,
            "key": hex(&key),
            "private": hex(&private),
            "nonce": hex(&nonce),
            "wrapped": hex(&keys::wrap_private(&key, &private, &nonce)),
        })
    })
    .collect();

    let recovery: Vec<Value> = ["recovery 1", "recovery 2"]
        .iter()
        .map(|label| {
            let key: [u8; 32] = fixed(label);
            let private: [u8; 32] = fixed(&format!("private of {label}"));
            let nonce: [u8; 12] = fixed(&format!("nonce of {label}"));
            let wrapping = keys::recovery_wrapping_key(&key);
            json!({
                "key": hex(&key),
                "text": keys::encode_recovery(&key),
                "wrapping_key": hex(&wrapping),
                "private": hex(&private),
                "nonce": hex(&nonce),
                "wrapped": hex(&keys::wrap_private(&wrapping, &private, &nonce)),
            })
        })
        .collect();

    let folder_keys: Vec<Value> = [(1u32, "bob"), (7, "alice")]
        .iter()
        .map(|(generation, who)| {
            let recipient = keys::Identity::from_private(fixed(&format!("identity {who}")));
            let folder_key: [u8; 32] = fixed(&format!("folder key {generation}"));
            let folder_id = Uuid::from_bytes(fixed(&format!("folder {generation}")));
            let ephemeral: [u8; 32] = fixed(&format!("ephemeral {generation}"));
            let nonce: [u8; 12] = fixed(&format!("wrap nonce {generation}"));
            let wrapped = folder::wrap_with(
                &folder_key,
                folder_id,
                *generation,
                &recipient.public,
                &ephemeral,
                &nonce,
            )
            .expect("a valid public key");
            json!({
                "folder_key": hex(&folder_key),
                "folder_id": folder_id.to_string(),
                "generation": generation,
                "recipient_private": hex(&recipient.private),
                "recipient_public": hex(&recipient.public),
                "ephemeral_private": hex(&ephemeral),
                "ephemeral_public": hex(&keys::public_key(&ephemeral)),
                "nonce": hex(&nonce),
                "wrapped": hex(&wrapped),
                "wrapped_base64url": crate::b64(&wrapped),
            })
        })
        .collect();

    let names: Vec<Value> = [
        (1u32, "Taxes 2026.pdf"),
        (2, "Photos"),
        // Decomposed: sealed as NFC ("Café ☕.txt").
        (2, "Cafe\u{301} \u{2615}.txt"),
    ]
    .iter()
    .enumerate()
    .map(|(i, (generation, name))| {
        let folder_key: [u8; 32] = fixed(&format!("folder key {generation}"));
        let parent = Uuid::from_bytes(fixed(&format!("parent {i}")));
        let nonce: [u8; 12] = fixed(&format!("name nonce {i}"));
        json!({
            "folder_key": hex(&folder_key),
            "generation": generation,
            "parent_id": parent.to_string(),
            "name": name,
            "decrypted": names::decrypt(
                &folder_key,
                parent,
                &names::encrypt_with(&folder_key, *generation, parent, name, &nonce).expect("short"),
            ).expect("opens"),
            "name_key": hex(&names::name_key(&folder_key)),
            "nonce": hex(&nonce),
            "encrypted": names::encrypt_with(&folder_key, *generation, parent, name, &nonce)
                .expect("short"),
        })
    })
    .collect();

    let contents: Vec<Value> = [0usize, 13, contents::SEGMENT, 2 * contents::SEGMENT + 1000]
        .iter()
        .enumerate()
        .map(|(i, len)| {
            let generation = 1 + i as u32;
            let folder_key: [u8; 32] = fixed(&format!("folder key {generation}"));
            let file_nonce: [u8; 32] = fixed(&format!("file nonce {i}"));
            let plain = pattern(*len);
            let sealed = contents::encrypt_with(&folder_key, generation, &file_nonce, &plain);
            let mut vector = json!({
                "folder_key": hex(&folder_key),
                "generation": generation,
                "file_nonce": hex(&file_nonce),
                "file_key": hex(&contents::file_key(&folder_key, &file_nonce)),
                "plain_length": len,
                "plain_sha256": hex(&Sha256::digest(&plain)),
                "encrypted_length": sealed.len(),
                "encrypted_sha256": hex(&Sha256::digest(&sealed)),
            });
            // The small ones whole; the large ones by their hash.
            if sealed.len() <= 256 {
                vector["encrypted"] = json!(hex(&sealed));
            }
            vector
        })
        .collect();

    let backend_secrets: Vec<Value> = [
        ("bob", "wJalrXUtnFEMI/K7MDENG/bPxRfiCYEXAMPLEKEY"),
        ("alice", "\u{e9}t\u{e9} \u{1F511}"),
    ]
    .iter()
    .enumerate()
    .map(|(i, (who, secret))| {
        let owner = keys::Identity::from_private(fixed(&format!("identity {who}")));
        let owner_id = Uuid::from_bytes(fixed(&format!("owner {i}")));
        let ephemeral: [u8; 32] = fixed(&format!("secret ephemeral {i}"));
        let nonce: [u8; 12] = fixed(&format!("secret nonce {i}"));
        let sealed = secrets::seal_with(secret, owner_id, &owner.public, &ephemeral, &nonce)
            .expect("a valid public key");
        json!({
            "secret": secret,
            "owner_id": owner_id.to_string(),
            "recipient_private": hex(&owner.private),
            "recipient_public": hex(&owner.public),
            "ephemeral_private": hex(&ephemeral),
            "nonce": hex(&nonce),
            "sealed": hex(&sealed),
            "sealed_base64url": crate::b64(&sealed),
        })
    })
    .collect();

    json!({
        "about": "Organizer's end-to-end encryption test vectors (docs/E2E_SPEC.md §9). Bytes are hex; contents are bytes i % 251; written by `cargo run -p organizer-e2e-crypto --example vectors`.",
        "format_version": crate::VERSION,
        "identities": identities,
        "passphrases": passphrases,
        "recovery_keys": recovery,
        "folder_keys": folder_keys,
        "names": names,
        "contents": contents,
        "backend_secrets": backend_secrets,
    })
}
