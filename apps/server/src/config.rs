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

/// An `s3://endpoint/bucket/prefix` URL (`s3+http://` for a plain-http
/// endpoint, e.g. a local MinIO): the endpoint, bucket and prefix.
pub fn parse_s3_url(url: &str) -> Result<(String, String, String), String> {
    let (scheme, rest) = url.split_once("://").ok_or("no scheme")?;
    let endpoint_scheme = match scheme {
        "s3" => "https",
        "s3+http" => "http",
        other => return Err(format!("not an s3 URL: {other}://")),
    };
    let (host, path) = rest
        .split_once('/')
        .ok_or_else(|| "no bucket: s3://endpoint/bucket/prefix".to_string())?;
    if host.is_empty() {
        return Err("no endpoint host: s3://endpoint/bucket/prefix".into());
    }
    let (bucket, prefix) = match path.split_once('/') {
        Some((bucket, prefix)) => (bucket.to_string(), prefix.trim_end_matches('/').to_string()),
        None => (path.to_string(), String::new()),
    };
    if bucket.is_empty() {
        return Err("no bucket: s3://endpoint/bucket/prefix".into());
    }
    Ok((format!("{endpoint_scheme}://{host}"), bucket, prefix))
}

/// `STORAGE_URL`: where the file content is — a local path (by default a
/// `files` folder in the data dir), `file://…`, or an `s3://endpoint/bucket/prefix`
/// URL, whose host is the endpoint and whose credentials come from
/// `STORAGE_ACCESS_KEY` / `STORAGE_SECRET_KEY` (`STORAGE_REGION` for the
/// region). The data dir itself always keeps the database, the embeddings,
/// the key files and the scratch (uploads, the drop box, the backup work dir).
pub enum StorageUrl {
    Local(PathBuf),
    S3 {
        endpoint: String,
        region: String,
        bucket: String,
        prefix: String,
    },
}

pub fn storage_url(data_dir: &Path) -> Result<StorageUrl, String> {
    match var("STORAGE_URL") {
        None => Ok(StorageUrl::Local(data_dir.join("files"))),
        Some(url) if url.starts_with("s3://") || url.starts_with("s3+http://") => {
            let (endpoint, bucket, prefix) =
                parse_s3_url(&url).map_err(|e| format!("STORAGE_URL: {e}"))?;
            Ok(StorageUrl::S3 {
                endpoint,
                region: var("STORAGE_REGION").unwrap_or_else(|| "us-east-1".into()),
                bucket,
                prefix,
            })
        }
        Some(url) if url.starts_with("file://") => Ok(StorageUrl::Local(PathBuf::from(
            url.trim_start_matches("file://"),
        ))),
        Some(other) if other.contains("://") => Err(format!(
            "STORAGE_URL must be a folder path, file://…, or s3://endpoint/bucket/prefix, not `{other}`"
        )),
        Some(path) => Ok(StorageUrl::Local(PathBuf::from(path))),
    }
}

/// The credentials of an `s3://` STORAGE_URL.
pub fn storage_access_key() -> Result<String, String> {
    var("STORAGE_ACCESS_KEY")
        .ok_or_else(|| "STORAGE_ACCESS_KEY is needed with an s3:// STORAGE_URL".into())
}

pub fn storage_secret_key() -> Result<String, String> {
    var("STORAGE_SECRET_KEY")
        .ok_or_else(|| "STORAGE_SECRET_KEY is needed with an s3:// STORAGE_URL".into())
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
