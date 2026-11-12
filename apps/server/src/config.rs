//! Settings read from the environment (the same helpers as Potoo's and
//! Woodmouse's). Everything user-facing lives in the database instead; the
//! environment holds only what is needed before it opens.

use std::path::{Path, PathBuf};

/// The value with surrounding whitespace removed; `None` while it is empty.
pub fn non_empty(value: Option<String>) -> Option<String> {
    value
        .map(|v| v.trim().to_string())
        .filter(|v| !v.is_empty())
}

/// An environment variable; `None` while it is unset or blank.
pub fn var(name: &str) -> Option<String> {
    non_empty(std::env::var(name).ok())
}

/// The port the server listens on (`PORT`, default 8000).
pub fn port() -> Result<u16, String> {
    match var("PORT") {
        None => Ok(8000),
        Some(port) => port
            .parse()
            .map_err(|_| format!("PORT must be a port number, not `{port}`")),
    }
}

/// Where Organizer is reached from (`PUBLIC_URL`), without a trailing slash.
pub fn public_url() -> Option<String> {
    var("PUBLIC_URL").map(|u| u.trim_end_matches('/').to_string())
}

/// Where Organizer keeps its own files: the database (unless `DATABASE_URL`
/// puts it elsewhere), the drop box, uploads, versions and caches
/// (`DATA_DIR`, default `data`).
pub fn data_dir() -> PathBuf {
    var("DATA_DIR").unwrap_or_else(|| "data".into()).into()
}

/// `FILES_DIR`: the files root of the first superadmin (the one made at setup),
/// which then cannot be changed in the settings; every other user has their
/// own. Unset: the setup page and the superadmin settings decide, by default a
/// folder in the data dir.
pub fn files_dir() -> Option<PathBuf> {
    var("FILES_DIR").map(PathBuf::from)
}

/// The database's file, from `DATABASE_URL`, by default `organizer.db` in
/// the data dir. A URL rather than a path, so that another database could
/// take its place later; for now it must be SQLite (`sqlite:path`,
/// `sqlite://path`, with sqlx's options such as `?mode=rwc` allowed). It
/// must be on a local disk: SQLite's locking is not safe on NFS or SMB.
pub fn database_path(data_dir: &Path) -> Result<String, String> {
    match var("DATABASE_URL") {
        Some(url) => parse_database_url(&url),
        None => Ok(data_dir.join("organizer.db").to_string_lossy().into_owned()),
    }
}

/// The file of a SQLite `DATABASE_URL` already read.
pub fn parse_database_url(url: &str) -> Result<String, String> {
    use std::str::FromStr;
    if !url.starts_with("sqlite:") {
        return Err(format!(
            "DATABASE_URL must be a SQLite URL (sqlite:path), not `{url}`"
        ));
    }
    let options = sqlx::sqlite::SqliteConnectOptions::from_str(url)
        .map_err(|e| format!("DATABASE_URL is not a valid SQLite URL: {e}"))?;
    Ok(options.get_filename().to_string_lossy().into_owned())
}

/// Where the embeddings are kept, from `VECTOR_DATABASE_URL`; by default
/// `turso:<DATA_DIR>/vectors.db`. The scheme names the engine; Turso is the
/// only one so far (`turso:path`).
pub fn vector_database_url(data_dir: &Path) -> String {
    var("VECTOR_DATABASE_URL")
        .unwrap_or_else(|| format!("turso:{}", data_dir.join("vectors.db").to_string_lossy()))
}

/// A yes/no setting: `1`/`0`, `true`/`false`, `yes`/`no` or `on`/`off` (any
/// case); `None` while unset or blank. Anything else is an error, so a typo
/// stops the server instead of silently meaning "off".
pub fn flag(name: &str) -> Result<Option<bool>, String> {
    parse_flag(name, &std::env::var(name).unwrap_or_default())
}

/// [`flag`] for a value already read.
pub fn parse_flag(name: &str, value: &str) -> Result<Option<bool>, String> {
    let value = value.trim();
    match value.to_lowercase().as_str() {
        "" => Ok(None),
        "1" | "true" | "yes" | "on" => Ok(Some(true)),
        "0" | "false" | "no" | "off" => Ok(Some(false)),
        _ => Err(format!(
            "{name} must be true or false (1/0, yes/no, on/off), not `{value}`"
        )),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_database_url_names_a_sqlite_file() {
        assert_eq!(
            parse_database_url("sqlite:data/organizer.db").unwrap(),
            "data/organizer.db"
        );
        assert_eq!(
            parse_database_url("sqlite:///data/organizer.db").unwrap(),
            "/data/organizer.db"
        );
        assert_eq!(
            parse_database_url("sqlite:/data/organizer.db?mode=rwc").unwrap(),
            "/data/organizer.db"
        );
        assert!(parse_database_url("postgres://db/organizer").is_err());
        assert!(parse_database_url("data/organizer.db").is_err());
    }

    #[test]
    fn flags_take_the_usual_spellings_and_refuse_the_rest() {
        for yes in ["1", "true", "TRUE", "yes", "on", " on "] {
            assert_eq!(parse_flag("X", yes), Ok(Some(true)), "{yes}");
        }
        for no in ["0", "false", "no", "off"] {
            assert_eq!(parse_flag("X", no), Ok(Some(false)), "{no}");
        }
        assert_eq!(parse_flag("X", ""), Ok(None));
        assert!(parse_flag("X", "maybe").unwrap_err().contains("X must be"));
    }
}
