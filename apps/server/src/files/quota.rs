//! A drive's storage limit (docs/M6A_DESIGN.md §2, docs/M7A_DESIGN.md §1),
//! with its team's and its storage backend's (docs/M7C_DESIGN.md §4):
//! what it takes, and whether a write through Organizer still fits. The
//! files in its tree, its trash, its versions and (in My drive) its user's
//! drop box items not yet placed all count. Changes made directly in a files folder are not stopped, only
//! counted (the next scan sees them).

use super::{FileError, Files};
use serde::Serialize;
use utoipa::ToSchema;
use uuid::Uuid;

/// What a user takes, and their limit.
#[derive(Clone, Copy, Debug, Serialize, ToSchema)]
pub struct Usage {
    /// Bytes: files, trash, versions and the drop box's waiting items.
    pub used: i64,
    /// Bytes; null: no limit. Always present.
    #[schema(required = true)]
    pub limit: Option<i64>,
}

impl Files {
    /// The bytes a drive takes, and its limit. My drive counts its user's
    /// drop box items not yet placed too.
    pub async fn usage(&self, drive_id: Uuid) -> Result<Usage, FileError> {
        let row: Option<(i64, Option<i64>)> = sqlx::query_as(
            "SELECT
               (SELECT coalesce(sum(size), 0) FROM nodes WHERE drive_id = ?1 AND kind = 'file')
               + (SELECT coalesce(sum(size), 0) FROM trash WHERE drive_id = ?1)
               + (SELECT coalesce(sum(size), 0) FROM versions WHERE drive_id = ?1)
               + (SELECT coalesce(sum(size), 0) FROM inbox_items
                  WHERE user_id = drives.user_id AND node_id IS NULL),
               storage_limit
             FROM drives WHERE id = ?1",
        )
        .bind(drive_id)
        .fetch_optional(self.db.reader())
        .await?;
        let (used, limit) = row.ok_or(FileError::NotFound)?;
        Ok(Usage { used, limit })
    }

    /// Whether `adding` more bytes fit under the user's limit: a write that
    /// would go past it is refused (507). A replaced file counts whole: its
    /// old contents stay, as a version.
    pub async fn check_room(&self, drive_id: Uuid, adding: u64) -> Result<(), FileError> {
        // A share from another server counts against nothing here: the
        // server that holds it counts what is written (docs/M7E_DESIGN.md).
        let remote: Option<i64> =
            sqlx::query_scalar("SELECT 1 FROM drives WHERE id = ?1 AND kind = 'remote'")
                .bind(drive_id)
                .fetch_optional(self.db.reader())
                .await?;
        if remote.is_some() {
            return Ok(());
        }
        let usage = self.usage(drive_id).await?;
        if let Some(limit) = usage.limit
            && usage.used.saturating_add(adding as i64) > limit
        {
            return Err(FileError::Full);
        }
        // A team's drive: the team's limit is for all its drives together
        // (docs/M7B_DESIGN.md §3).
        let team: Option<Uuid> = sqlx::query_scalar("SELECT team_id FROM drives WHERE id = ?1")
            .bind(drive_id)
            .fetch_optional(self.db.reader())
            .await?
            .flatten();
        if let Some(team) = team {
            let usage = self.team_usage(team).await?;
            if let Some(limit) = usage.limit
                && usage.used.saturating_add(adding as i64) > limit
            {
                return Err(FileError::Full);
            }
        }
        // A drive in a storage backend: the backend's limit is for all its
        // drives together (docs/M7C_DESIGN.md §4).
        let backend: Option<Uuid> =
            sqlx::query_scalar("SELECT backend_id FROM drives WHERE id = ?1")
                .bind(drive_id)
                .fetch_optional(self.db.reader())
                .await?
                .flatten();
        if let Some(backend) = backend {
            let usage = self.backend_usage(backend).await?;
            if let Some(limit) = usage.limit
                && usage.used.saturating_add(adding as i64) > limit
            {
                return Err(FileError::Full);
            }
        }
        Ok(())
    }

    /// What a backend's drives take together, and the backend's limit.
    pub async fn backend_usage(&self, backend: Uuid) -> Result<Usage, FileError> {
        let (limit,): (Option<i64>,) =
            sqlx::query_as("SELECT storage_limit FROM backends WHERE id = ?1")
                .bind(backend)
                .fetch_optional(self.db.reader())
                .await?
                .ok_or(FileError::NotFound)?;
        Ok(Usage {
            used: self.drives_usage("backend_id = ?1", Some(backend)).await?,
            limit,
        })
    }

    /// What the drives on the server's disk take together.
    pub async fn disk_usage(&self) -> Result<i64, FileError> {
        self.drives_usage("backend_id IS NULL AND kind != 'remote'", None)
            .await
    }

    /// What the drives that `filter` (a fixed fragment) picks take.
    async fn drives_usage(&self, filter: &str, value: Option<Uuid>) -> Result<i64, FileError> {
        let mut query = sqlx::query_scalar(sqlx::AssertSqlSafe(format!(
            "SELECT id FROM drives WHERE {filter}"
        )));
        if let Some(value) = value {
            query = query.bind(value);
        }
        let drives: Vec<Uuid> = query.fetch_all(self.db.reader()).await?;
        let mut used = 0i64;
        for drive in drives {
            used = used.saturating_add(self.usage(drive).await?.used);
        }
        Ok(used)
    }

    /// What a team's drives take together, and the team's limit.
    pub async fn team_usage(&self, team: Uuid) -> Result<Usage, FileError> {
        let (limit,): (Option<i64>,) =
            sqlx::query_as("SELECT storage_limit FROM teams WHERE id = ?1")
                .bind(team)
                .fetch_optional(self.db.reader())
                .await?
                .ok_or(FileError::NotFound)?;
        Ok(Usage {
            used: self.drives_usage("team_id = ?1", Some(team)).await?,
            limit,
        })
    }
}

/// The size of a file, or of everything in a folder.
pub fn size_on_disk(path: &std::path::Path) -> u64 {
    let Ok(metadata) = std::fs::symlink_metadata(path) else {
        return 0;
    };
    if metadata.is_file() {
        return metadata.len();
    }
    if !metadata.is_dir() {
        return 0;
    }
    std::fs::read_dir(path)
        .map(|entries| {
            entries
                .flatten()
                .map(|entry| size_on_disk(&entry.path()))
                .sum()
        })
        .unwrap_or(0)
}
