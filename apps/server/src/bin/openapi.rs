//! Writes the OpenAPI spec to a file (default `dist/openapi.json`,
//! or the path given as the first argument), for the frontend's client
//! generator. Built with `--features spec`; see `mise run gen:spec`.

use anyhow::Context;

fn main() -> anyhow::Result<()> {
    let path = std::env::args()
        .nth(1)
        .unwrap_or_else(|| "dist/openapi.json".into());
    if let Some(parent) = std::path::Path::new(&path).parent() {
        std::fs::create_dir_all(parent).with_context(|| format!("cannot create {parent:?}"))?;
    }
    std::fs::write(&path, organizer::openapi::spec_json())
        .with_context(|| format!("cannot write {path}"))?;
    println!("wrote {path}");
    Ok(())
}
