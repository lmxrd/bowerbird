//! The server-wide settings: one JSON document per area in the single
//! `server_settings` table — `oidc`, `ai`, `backup`, `backup_status`,
//! `signup`, `federation`. The table is key-value on purpose: the shapes,
//! defaults and validation live in the areas' structs, in code, not in
//! column types. Secrets (the OIDC client secret, the backup keys) are
//! fields of those structs; the API views never serialize them.

use serde::{Serialize, de::DeserializeOwned};
use sqlx::{Executor, Sqlite};

/// One area's settings, from its row. No row yet, or one that does not
/// parse: the built-in defaults. Pass `db.reader()`, or a transaction to
/// read inside it.
pub async fn load<'e, E, T>(db: E, key: &str) -> sqlx::Result<T>
where
    E: Executor<'e, Database = Sqlite>,
    T: DeserializeOwned + Default,
{
    let row: Option<String> =
        sqlx::query_scalar("SELECT value FROM server_settings WHERE key = ?1")
            .bind(key)
            .fetch_one(db)
            .await?;
    Ok(row
        .and_then(|value| serde_json::from_str(&value).ok())
        .unwrap_or_default())
}

/// Store one area's settings, making or replacing its row. Pass
/// `db.writer()`, or a transaction to write inside it.
pub async fn save<'e, E, T>(db: E, key: &str, settings: &T) -> sqlx::Result<()>
where
    E: Executor<'e, Database = Sqlite>,
    T: Serialize,
{
    let value = serde_json::to_string(settings).map_err(|e| sqlx::Error::Encode(Box::new(e)))?;
    sqlx::query(
        "INSERT INTO server_settings (key, value) VALUES (?1, ?2)
         ON CONFLICT (key) DO UPDATE SET value = excluded.value",
    )
    .bind(key)
    .bind(value)
    .execute(db)
    .await?;
    Ok(())
}
