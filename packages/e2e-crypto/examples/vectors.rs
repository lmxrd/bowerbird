//! Writes the test vectors to `vectors.json` at the crate's root (what
//! every client's tests read): `cargo run -p organizer-e2e-crypto --example
//! vectors`.

fn main() {
    let path = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("vectors.json");
    let text = serde_json::to_string_pretty(&organizer_e2e_crypto::vectors::generate())
        .expect("serialisable");
    std::fs::write(&path, format!("{text}\n")).expect("writable");
    println!("{}", path.display());
}
