//! The engine's state: a small SQLite database per account
//! (`migrations/`). Queries are run-time `sqlx::query`, as in the backend.

use crate::Error;
use crate::local::Kind;
use crate::remote::{Node, Role};
use sqlx::sqlite::{SqliteConnectOptions, SqliteJournalMode, SqlitePoolOptions};
use sqlx::{FromRow, SqlitePool};
use std::path::{Path, PathBuf};
use std::str::FromStr;
use uuid::Uuid;

#[derive(Clone)]
pub struct State {
    pool: SqlitePool,
}

/// A synced folder.
#[derive(Clone, Debug, FromRow, serde::Serialize)]
pub struct Folder {
    pub id: i64,
    pub remote_id: String,
    pub local_path: String,
    pub mode: String,
    /// The drive it is in (docs/M7A_DESIGN.md); None until the engine has
    /// asked the server.
    pub drive_id: Option<String>,
}

impl Folder {
    pub fn remote_id(&self) -> Uuid {
        self.remote_id.parse().unwrap_or_default()
    }

    pub fn local(&self) -> PathBuf {
        PathBuf::from(&self.local_path)
    }

    pub fn mode(&self) -> Mode {
        Mode::from_str(&self.mode).unwrap_or(Mode::Sync)
    }
}

/// How a folder is kept (docs/M5B_DESIGN.md §1).
#[derive(Clone, Copy, Debug, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Mode {
    /// Full sync: every file here and on the server.
    Sync,
    /// Its files show here, but are downloaded only when opened.
    OnDemand,
}

impl Mode {
    pub fn as_str(self) -> &'static str {
        match self {
            Mode::Sync => "sync",
            Mode::OnDemand => "on_demand",
        }
    }
}

impl FromStr for Mode {
    type Err = ();
    fn from_str(s: &str) -> Result<Self, ()> {
        match s {
            "sync" => Ok(Mode::Sync),
            "on_demand" => Ok(Mode::OnDemand),
            _ => Err(()),
        }
    }
}

/// A node of the server's tree, as last told.
#[derive(Clone, Debug, FromRow)]
pub struct RemoteRow {
    pub node_id: String,
    pub parent_id: Option<String>,
    pub name: String,
    pub kind: String,
    pub version: i64,
    pub size: Option<i64>,
    pub modified: Option<String>,
    /// What the user may do with it (`Role::as_str`); None until listed.
    pub role: Option<String>,
    /// The drive it is in; None in a mirror of before roles, until listed.
    pub drive_id: Option<String>,
}

impl RemoteRow {
    pub fn role(&self) -> Option<Role> {
        self.role.as_deref().and_then(Role::parse)
    }
}

/// What both sides agreed on about one node.
#[derive(Clone, Debug, FromRow, PartialEq)]
pub struct Entry {
    pub folder: i64,
    pub node_id: String,
    pub path: String,
    pub parent_id: String,
    pub name: String,
    pub kind: String,
    pub version: i64,
    pub hash: Option<String>,
    pub size: Option<i64>,
    pub mtime_ns: Option<i64>,
    pub inode: Option<i64>,
}

impl Entry {
    pub fn kind(&self) -> Kind {
        Kind::from_str(&self.kind).unwrap_or(Kind::File)
    }
}

/// A resumable upload under way.
#[derive(Clone, Debug, FromRow)]
pub struct Upload {
    pub url: String,
    pub size: i64,
    pub mtime_ns: i64,
}

impl State {
    /// The database at this path, made if needed.
    pub async fn open(path: &Path) -> Result<Self, Error> {
        if let Some(dir) = path.parent() {
            std::fs::create_dir_all(dir)?;
        }
        let options = SqliteConnectOptions::new()
            .filename(path)
            .create_if_missing(true)
            .journal_mode(SqliteJournalMode::Wal)
            .foreign_keys(true)
            .busy_timeout(std::time::Duration::from_secs(10));
        Self::with(options).await
    }

    /// An in-memory database, for the tests.
    pub async fn memory() -> Result<Self, Error> {
        Self::with(SqliteConnectOptions::from_str("sqlite::memory:")?.foreign_keys(true)).await
    }

    async fn with(options: SqliteConnectOptions) -> Result<Self, Error> {
        // One connection: the engine is the one writer, and an in-memory
        // database is one per connection.
        let pool = SqlitePoolOptions::new()
            .max_connections(1)
            .connect_with(options)
            .await?;
        sqlx::migrate!("./migrations").run(&pool).await?;
        Ok(State { pool })
    }

    pub async fn close(&self) {
        self.pool.close().await;
    }

    pub async fn meta(&self, key: &str) -> Result<Option<String>, Error> {
        Ok(sqlx::query_scalar("SELECT value FROM meta WHERE key = ?1")
            .bind(key)
            .fetch_optional(&self.pool)
            .await?)
    }

    pub async fn set_meta(&self, key: &str, value: Option<&str>) -> Result<(), Error> {
        match value {
            Some(value) => {
                sqlx::query(
                    "INSERT INTO meta (key, value) VALUES (?1, ?2)
                     ON CONFLICT (key) DO UPDATE SET value = excluded.value",
                )
                .bind(key)
                .bind(value)
                .execute(&self.pool)
                .await?
            }
            None => {
                sqlx::query("DELETE FROM meta WHERE key = ?1")
                    .bind(key)
                    .execute(&self.pool)
                    .await?
            }
        };
        Ok(())
    }

    // --- Folders ---------------------------------------------------------

    pub async fn folders(&self) -> Result<Vec<Folder>, Error> {
        Ok(sqlx::query_as(
            "SELECT id, remote_id, local_path, mode, drive_id FROM folders ORDER BY id",
        )
        .fetch_all(&self.pool)
        .await?)
    }

    pub async fn add_folder(
        &self,
        remote_id: Uuid,
        local: &Path,
        mode: Mode,
        drive_id: Option<&str>,
    ) -> Result<Folder, Error> {
        let id: i64 = sqlx::query_scalar(
            "INSERT INTO folders (remote_id, local_path, mode, drive_id) VALUES (?1, ?2, ?3, ?4)
             RETURNING id",
        )
        .bind(remote_id.to_string())
        .bind(local.to_string_lossy())
        .bind(mode.as_str())
        .bind(drive_id)
        .fetch_one(&self.pool)
        .await
        .map_err(|e| match e {
            sqlx::Error::Database(d) if d.is_unique_violation() => {
                Error::Invalid("That folder, or that local place, is synced already".into())
            }
            e => e.into(),
        })?;
        Ok(Folder {
            id,
            remote_id: remote_id.to_string(),
            local_path: local.to_string_lossy().into_owned(),
            mode: mode.as_str().into(),
            drive_id: drive_id.map(str::to_string),
        })
    }

    /// The drive a folder is in, as the server says.
    pub async fn set_folder_drive(&self, id: i64, drive_id: &str) -> Result<(), Error> {
        sqlx::query("UPDATE folders SET drive_id = ?1 WHERE id = ?2")
            .bind(drive_id)
            .bind(id)
            .execute(&self.pool)
            .await?;
        Ok(())
    }

    pub async fn remove_folder(&self, id: i64) -> Result<(), Error> {
        sqlx::query("DELETE FROM folders WHERE id = ?1")
            .bind(id)
            .execute(&self.pool)
            .await?;
        sqlx::query("DELETE FROM uploads WHERE folder = ?1")
            .bind(id)
            .execute(&self.pool)
            .await?;
        Ok(())
    }

    pub async fn move_folder(&self, id: i64, local: &Path) -> Result<(), Error> {
        sqlx::query("UPDATE folders SET local_path = ?1 WHERE id = ?2")
            .bind(local.to_string_lossy())
            .bind(id)
            .execute(&self.pool)
            .await?;
        Ok(())
    }

    // --- The server's tree -----------------------------------------------

    pub async fn remote_rows(&self) -> Result<Vec<RemoteRow>, Error> {
        Ok(sqlx::query_as(
            "SELECT node_id, parent_id, name, kind, version, size, modified, role, drive_id FROM remote",
        )
        .fetch_all(&self.pool)
        .await?)
    }

    pub async fn remote(&self, id: Uuid) -> Result<Option<RemoteRow>, Error> {
        Ok(sqlx::query_as(
            "SELECT node_id, parent_id, name, kind, version, size, modified, role, drive_id FROM remote WHERE node_id = ?1",
        )
        .bind(id.to_string())
        .fetch_optional(&self.pool)
        .await?)
    }

    /// What is in a folder, as last told.
    pub async fn remote_children(&self, parent: Uuid) -> Result<Vec<RemoteRow>, Error> {
        Ok(sqlx::query_as(
            "SELECT node_id, parent_id, name, kind, version, size, modified, role, drive_id FROM remote
             WHERE parent_id = ?1",
        )
        .bind(parent.to_string())
        .fetch_all(&self.pool)
        .await?)
    }

    pub async fn put_remote(&self, node: &Node) -> Result<(), Error> {
        sqlx::query(
            "INSERT INTO remote (node_id, parent_id, name, kind, version, size, modified, role,
               drive_id)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9)
             ON CONFLICT (node_id) DO UPDATE SET parent_id = excluded.parent_id,
               name = excluded.name, kind = excluded.kind, version = excluded.version,
               size = excluded.size, modified = excluded.modified, role = excluded.role,
               drive_id = coalesce(excluded.drive_id, remote.drive_id)",
        )
        .bind(node.id.to_string())
        .bind(node.parent_id.map(|p| p.to_string()))
        .bind(&node.name)
        .bind(node.kind.as_str())
        .bind(node.version)
        .bind(node.size)
        .bind(&node.modified)
        .bind(node.role.map(Role::as_str))
        .bind(&node.drive_id)
        .execute(&self.pool)
        .await?;
        Ok(())
    }

    /// A node and everything under it leave the mirror.
    pub async fn remove_remote(&self, id: Uuid) -> Result<(), Error> {
        sqlx::query(
            "WITH RECURSIVE gone(id) AS (
               SELECT ?1 UNION SELECT remote.node_id FROM remote JOIN gone ON remote.parent_id = gone.id
             )
             DELETE FROM remote WHERE node_id IN (SELECT id FROM gone)",
        )
        .bind(id.to_string())
        .execute(&self.pool)
        .await?;
        Ok(())
    }

    pub async fn clear_remote(&self) -> Result<(), Error> {
        sqlx::query("DELETE FROM remote")
            .execute(&self.pool)
            .await?;
        Ok(())
    }

    // --- What both sides agreed on ---------------------------------------

    pub async fn entries(&self, folder: i64) -> Result<Vec<Entry>, Error> {
        Ok(sqlx::query_as(
            "SELECT folder, node_id, path, parent_id, name, kind, version, hash, size, mtime_ns, inode
             FROM entries WHERE folder = ?1",
        )
        .bind(folder)
        .fetch_all(&self.pool)
        .await?)
    }

    pub async fn put_entry(&self, entry: &Entry) -> Result<(), Error> {
        // A path is one node's: another that had it is gone by now.
        sqlx::query("DELETE FROM entries WHERE folder = ?1 AND path = ?2 AND node_id <> ?3")
            .bind(entry.folder)
            .bind(&entry.path)
            .bind(&entry.node_id)
            .execute(&self.pool)
            .await?;
        sqlx::query(
            "INSERT INTO entries (folder, node_id, path, parent_id, name, kind, version, hash, size, mtime_ns, inode)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11)
             ON CONFLICT (folder, node_id) DO UPDATE SET path = excluded.path,
               parent_id = excluded.parent_id, name = excluded.name, kind = excluded.kind,
               version = excluded.version, hash = excluded.hash, size = excluded.size,
               mtime_ns = excluded.mtime_ns, inode = excluded.inode",
        )
        .bind(entry.folder)
        .bind(&entry.node_id)
        .bind(&entry.path)
        .bind(&entry.parent_id)
        .bind(&entry.name)
        .bind(&entry.kind)
        .bind(entry.version)
        .bind(&entry.hash)
        .bind(entry.size)
        .bind(entry.mtime_ns)
        .bind(entry.inode)
        .execute(&self.pool)
        .await?;
        Ok(())
    }

    pub async fn remove_entry(&self, folder: i64, node_id: &str) -> Result<(), Error> {
        sqlx::query("DELETE FROM entries WHERE folder = ?1 AND node_id = ?2")
            .bind(folder)
            .bind(node_id)
            .execute(&self.pool)
            .await?;
        Ok(())
    }

    /// The entries under a moved folder follow it: `from/...` becomes `to/...`.
    pub async fn move_entries(&self, folder: i64, from: &str, to: &str) -> Result<(), Error> {
        sqlx::query(
            "UPDATE entries SET path = ?3 || substr(path, length(?2) + 1)
             WHERE folder = ?1 AND substr(path, 1, length(?2) + 1) = ?2 || '/'",
        )
        .bind(folder)
        .bind(from)
        .bind(to)
        .execute(&self.pool)
        .await?;
        Ok(())
    }

    /// The entries under a folder (not the folder's own) are dropped.
    pub async fn remove_entries_under(&self, folder: i64, path: &str) -> Result<(), Error> {
        sqlx::query(
            "DELETE FROM entries WHERE folder = ?1 AND substr(path, 1, length(?2) + 1) = ?2 || '/'",
        )
        .bind(folder)
        .bind(path)
        .execute(&self.pool)
        .await?;
        Ok(())
    }

    // --- Resumable uploads -----------------------------------------------

    pub async fn upload(&self, folder: i64, path: &str) -> Result<Option<Upload>, Error> {
        Ok(sqlx::query_as(
            "SELECT url, size, mtime_ns FROM uploads WHERE folder = ?1 AND path = ?2",
        )
        .bind(folder)
        .bind(path)
        .fetch_optional(&self.pool)
        .await?)
    }

    pub async fn put_upload(&self, folder: i64, path: &str, upload: &Upload) -> Result<(), Error> {
        sqlx::query(
            "INSERT INTO uploads (folder, path, url, size, mtime_ns) VALUES (?1, ?2, ?3, ?4, ?5)
             ON CONFLICT (folder, path) DO UPDATE SET url = excluded.url, size = excluded.size,
               mtime_ns = excluded.mtime_ns",
        )
        .bind(folder)
        .bind(path)
        .bind(&upload.url)
        .bind(upload.size)
        .bind(upload.mtime_ns)
        .execute(&self.pool)
        .await?;
        Ok(())
    }

    pub async fn remove_upload(&self, folder: i64, path: &str) -> Result<(), Error> {
        sqlx::query("DELETE FROM uploads WHERE folder = ?1 AND path = ?2")
            .bind(folder)
            .bind(path)
            .execute(&self.pool)
            .await?;
        Ok(())
    }
}
