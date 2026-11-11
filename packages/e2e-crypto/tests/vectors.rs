//! The committed vectors are the ones this implementation writes, and
//! read back through the public functions the way a client reads them.

use organizer_e2e_crypto::{Error, contents, folder, keys, names, secrets, vectors};
use serde_json::Value;
use uuid::Uuid;

fn committed() -> Value {
    let path = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("vectors.json");
    serde_json::from_str(&std::fs::read_to_string(path).unwrap()).unwrap()
}

fn bytes(v: &Value) -> Vec<u8> {
    hex::decode(v.as_str().unwrap()).unwrap()
}

fn array<const N: usize>(v: &Value) -> [u8; N] {
    bytes(v).try_into().unwrap()
}

#[test]
fn vectors_are_up_to_date() {
    assert_eq!(
        committed(),
        vectors::generate(),
        "run `cargo run -p organizer-e2e-crypto --example vectors`"
    );
}

#[test]
fn vectors_read_back() {
    let v = committed();
    for id in v["identities"].as_array().unwrap() {
        assert_eq!(
            keys::public_key(&array(&id["private"])).to_vec(),
            bytes(&id["public"])
        );
    }
    for p in v["passphrases"].as_array().unwrap() {
        let private = keys::unwrap_with_passphrase(
            &bytes(&p["wrapped"]),
            p["passphrase"].as_str().unwrap(),
            &bytes(&p["salt"]),
            p["iterations"].as_u64().unwrap() as u32,
        )
        .unwrap();
        assert_eq!(private.to_vec(), bytes(&p["private"]));
    }
    for r in v["recovery_keys"].as_array().unwrap() {
        let key = keys::decode_recovery(r["text"].as_str().unwrap()).unwrap();
        assert_eq!(key.to_vec(), bytes(&r["key"]));
        let private = keys::unwrap_with_recovery(&bytes(&r["wrapped"]), &key).unwrap();
        assert_eq!(private.to_vec(), bytes(&r["private"]));
    }
    for f in v["folder_keys"].as_array().unwrap() {
        let id: Uuid = f["folder_id"].as_str().unwrap().parse().unwrap();
        let generation = f["generation"].as_u64().unwrap() as u32;
        let key = folder::unwrap(
            &bytes(&f["wrapped"]),
            id,
            generation,
            &array(&f["recipient_private"]),
        )
        .unwrap();
        assert_eq!(key.to_vec(), bytes(&f["folder_key"]));
        assert_eq!(
            folder::unwrap(
                &bytes(&f["wrapped"]),
                id,
                generation + 1,
                &array(&f["recipient_private"])
            ),
            Err(Error::Decrypt)
        );
    }
    for s in v["backend_secrets"].as_array().unwrap() {
        let owner: Uuid = s["owner_id"].as_str().unwrap().parse().unwrap();
        let private = array(&s["recipient_private"]);
        assert_eq!(
            secrets::open_secret(&bytes(&s["sealed"]), owner, &private).unwrap(),
            s["secret"].as_str().unwrap()
        );
        assert_eq!(
            secrets::open_secret(&bytes(&s["sealed"]), Uuid::nil(), &private),
            Err(Error::Decrypt)
        );
    }
    for n in v["names"].as_array().unwrap() {
        let key = array(&n["folder_key"]);
        let parent: Uuid = n["parent_id"].as_str().unwrap().parse().unwrap();
        let encrypted = n["encrypted"].as_str().unwrap();
        assert_eq!(
            names::generation(encrypted).unwrap() as u64,
            n["generation"].as_u64().unwrap()
        );
        assert_eq!(
            names::decrypt(&key, parent, encrypted).unwrap(),
            n["decrypted"].as_str().unwrap()
        );
        assert_eq!(
            names::decrypt(&key, Uuid::nil(), encrypted),
            Err(Error::Decrypt)
        );
    }
    for c in v["contents"].as_array().unwrap() {
        let key = array(&c["folder_key"]);
        let generation = c["generation"].as_u64().unwrap() as u32;
        let len = c["plain_length"].as_u64().unwrap() as usize;
        let sealed = contents::encrypt_with(
            &key,
            generation,
            &array(&c["file_nonce"]),
            &vectors::pattern(len),
        );
        if let Some(whole) = c.get("encrypted") {
            assert_eq!(sealed, bytes(whole));
        }
        assert_eq!(sealed.len() as u64, c["encrypted_length"].as_u64().unwrap());
        assert_eq!(
            contents::decrypt(|g| (g == generation).then_some(key), &sealed).unwrap(),
            vectors::pattern(len)
        );
    }
}
