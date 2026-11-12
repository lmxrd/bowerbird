//! The earlier contents of files (docs/M3_DESIGN.md §2): kept when a write
//! through Organizer replaces a file ([`Files::place_file`]), listed,
//! downloaded, restored and deleted here, and thinned by the user's rules
//! (`retention.rs`). They go to the trash with their file, or with a folder
//! it was in, and come back with a restore.
//!
//! A version's bytes are in `<drive root>/.organizer/versions/<version id>`
//! of the drive's storage: on the server's disk a hard link to the
//! replaced file where it can be, elsewhere a copy (`Storage::keep_copy`).

use super::ops::WriteTarget;
use super::{FileError, Files, Kind, Tree, VERSIONS_DIR};
use serde::{Deserialize, Serialize};
use std::path::Path;
use std::sync::Arc;
use utoipa::ToSchema;
use uuid::Uuid;

/// What wrote a file's contents.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize, ToSchema, sqlx::Type)]
#[serde(rename_all = "lowercase")]
#[sqlx(type_name = "TEXT", rename_all = "lowercase")]
pub enum Source {
    /// The web app (a session).
    Web,
    /// An API key: an app, the sync client.
    Api,
    /// A WebDAV client.
    Webdav,
}

/// An earlier content of a file.
#[derive(Clone, Debug, Serialize, ToSchema, sqlx::FromRow)]
pub struct Version {
    pub id: Uuid,
    /// The file's version these contents were.
    pub version: i64,
    pub size: i64,
    /// What wrote the contents that replaced these; absent when unknown.
    #[schema(required = true)]
    pub source: Option<Source>,
    /// When they were replaced.
    pub created_at: String,
}

/// A version to keep, made before a write replaces a file.
pub(crate) struct Kept {
    pub id: Uuid,
    /// From the drive's root.
    pub path: String,
    pub version: i64,
    pub size: i64,
    pub hash: Vec<u8>,
}

impl Files {
    /// Keep the file at `path` (node `id`, at `version`) before it is
    /// replaced by contents hashed `new_hash`; `None` when versions are off
    /// for the user or the contents do not change. Called with the tree's
    /// lock held.
    pub(crate) async fn keep_version(
        self: &Arc<Self>,
        tree: &Tree,
        id: Uuid,
        path: &str,
        new_hash: &[u8],
    ) -> Result<Option<Kept>, FileError> {
        let drive_id = tree.drive_id;
        let rules = crate::retention::rules(self, drive_id).await?;
        if !rules.versions() {
            return Ok(None);
        }
        let (version, size, hash): (i64, i64, Option<Vec<u8>>) =
            sqlx::query_as("SELECT version, size, hash FROM nodes WHERE id = ?1")
                .bind(id)
                .fetch_one(self.db.reader())
                .await?;
        // Not hashed yet (a file the reconciler has not got to): hashed now.
        let hash = match hash {
            Some(hash) => hash,
            None => tree
                .storage
                .hash(path)
                .await
                .ok_or_else(|| anyhow::anyhow!("cannot read {path} of drive {drive_id}"))?,
        };
        if hash == new_hash {
            return Ok(None);
        }
        tree.prepare()?;
        let kept = Uuid::now_v7();
        let to = Tree::version_path(kept);
        tree.storage.keep_copy(path, &to).await?;
        Ok(Some(Kept {
            id: kept,
            path: to,
            version,
            size,
            hash,
        }))
    }

    /// Record a version kept by [`Files::keep_version`], in the write's
    /// transaction.
    pub(crate) async fn record_version(
        tx: &mut sqlx::SqliteConnection,
        drive_id: Uuid,
        node_id: Uuid,
        kept: &Kept,
        source: Option<Source>,
    ) -> sqlx::Result<()> {
        sqlx::query(
            "INSERT INTO versions (id, drive_id, node_id, version, size, hash, source)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)",
        )
        .bind(kept.id)
        .bind(drive_id)
        .bind(node_id)
        .bind(kept.version)
        .bind(kept.size)
        .bind(&kept.hash)
        .bind(source)
        .execute(tx)
        .await?;
        Ok(())
    }

    /// The versions of a file of the tree, the newest first.
    pub async fn versions(&self, drive_id: Uuid, id: Uuid) -> Result<Vec<Version>, FileError> {
        let node = self.node(drive_id, id).await?;
        if node.kind != Kind::File {
            return Err(FileError::Invalid("A folder has no versions".into()));
        }
        Ok(sqlx::query_as(
            "SELECT id, version, size, source, created_at FROM versions
             WHERE node_id = ?1 AND drive_id = ?2 AND trash_id IS NULL
             ORDER BY created_at DESC, version DESC",
        )
        .bind(id)
        .bind(drive_id)
        .fetch_all(self.db.reader())
        .await?)
    }

    /// A version of a file of the tree, and where its bytes are.
    pub async fn version(
        self: &Arc<Self>,
        drive_id: Uuid,
        id: Uuid,
        version_id: Uuid,
    ) -> Result<(Version, String), FileError> {
        self.node(drive_id, id).await?;
        let version: Version = sqlx::query_as(
            "SELECT id, version, size, source, created_at FROM versions
             WHERE id = ?1 AND node_id = ?2 AND drive_id = ?3 AND trash_id IS NULL",
        )
        .bind(version_id)
        .bind(id)
        .bind(drive_id)
        .fetch_optional(self.db.reader())
        .await?
        .ok_or(FileError::NotFound)?;
        Ok((version, Tree::version_path(version_id)))
    }

    /// Make a version the file's contents again, at the file's current
    /// version. It is a write like any other: the contents it replaces are
    /// kept as a version in turn.
    pub async fn restore_version(
        self: &Arc<Self>,
        drive_id: Uuid,
        id: Uuid,
        version_id: Uuid,
        if_version: i64,
        source: Option<Source>,
    ) -> Result<super::Node, FileError> {
        let (_, path) = self.version(drive_id, id, version_id).await?;
        std::fs::create_dir_all(self.uploads_dir())?;
        let temp = self
            .uploads_dir()
            .join(format!("version-{}", Uuid::now_v7()));
        self.storage(drive_id).await?.copy_out(&path, &temp).await?;
        self.write_file(
            drive_id,
            WriteTarget::Replace {
                id,
                if_version,
                source,
            },
            &temp,
        )
        .await
    }

    /// Delete a version for good.
    pub async fn delete_version(
        self: &Arc<Self>,
        drive_id: Uuid,
        id: Uuid,
        version_id: Uuid,
    ) -> Result<(), FileError> {
        self.version(drive_id, id, version_id).await?;
        self.remove_versions(drive_id, &[version_id]).await
    }

    /// Remove versions, their rows and their bytes.
    async fn remove_versions(
        self: &Arc<Self>,
        drive_id: Uuid,
        ids: &[Uuid],
    ) -> Result<(), FileError> {
        if ids.is_empty() {
            return Ok(());
        }
        let storage = self.storage(drive_id).await?;
        for id in ids {
            sqlx::query("DELETE FROM versions WHERE id = ?1 AND drive_id = ?2")
                .bind(id)
                .bind(drive_id)
                .execute(self.db.writer())
                .await?;
            match storage.remove_all(&Tree::version_path(*id)).await {
                Ok(()) => {}
                Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
                Err(e) => return Err(e.into()),
            }
        }
        Ok(())
    }

    /// The versions of `id` (a file) or of every file in it (a folder) go
    /// to the trash with it: called in the trashing's transaction, before
    /// the nodes are deleted.
    pub(crate) async fn trash_versions(
        tx: &mut sqlx::SqliteConnection,
        id: Uuid,
    ) -> sqlx::Result<()> {
        sqlx::query(
            "WITH RECURSIVE sub(id, path) AS (
               SELECT id, '' FROM nodes WHERE id = ?1
               UNION ALL
               SELECT n.id, CASE WHEN sub.path = '' THEN n.name ELSE sub.path || '/' || n.name END
               FROM nodes n JOIN sub ON n.parent_id = sub.id
             )
             UPDATE versions SET trash_id = ?1,
               trash_path = (SELECT path FROM sub WHERE sub.id = versions.node_id)
             WHERE node_id IN (SELECT id FROM sub)",
        )
        .bind(id)
        .execute(tx)
        .await?;
        Ok(())
    }

    /// The versions that went to the trash with `id` come back with it: a
    /// file keeps its id; what is in a folder is found again by its path
    /// (a restored folder's contents are indexed anew). Those not found go.
    pub(crate) async fn restore_versions(
        self: &Arc<Self>,
        drive_id: Uuid,
        id: Uuid,
    ) -> Result<(), FileError> {
        let rows: Vec<(Uuid, String)> = sqlx::query_as(
            "SELECT id, coalesce(trash_path, '') FROM versions WHERE trash_id = ?1 AND drive_id = ?2",
        )
        .bind(id)
        .bind(drive_id)
        .fetch_all(self.db.reader())
        .await?;
        if rows.is_empty() {
            return Ok(());
        }
        let base = super::path_of(self.db.reader(), id).await?.join("/");
        let mut lost = Vec::new();
        for (version, path) in rows {
            let full = if path.is_empty() {
                base.clone()
            } else {
                format!("{base}/{path}")
            };
            match self.resolve(drive_id, Path::new(&full)).await {
                Ok(node) if node.kind == Kind::File => {
                    sqlx::query(
                        "UPDATE versions SET node_id = ?1, trash_id = NULL, trash_path = NULL
                         WHERE id = ?2",
                    )
                    .bind(node.id)
                    .bind(version)
                    .execute(self.db.writer())
                    .await?;
                }
                Ok(_) | Err(FileError::NotFound) => lost.push(version),
                Err(e) => return Err(e),
            }
        }
        self.remove_versions(drive_id, &lost).await
    }

    /// The versions that went to the trash with `id` go for good with it.
    pub(crate) async fn purge_versions(
        self: &Arc<Self>,
        drive_id: Uuid,
        id: Uuid,
    ) -> Result<(), FileError> {
        let ids: Vec<Uuid> =
            sqlx::query_scalar("SELECT id FROM versions WHERE trash_id = ?1 AND drive_id = ?2")
                .bind(id)
                .bind(drive_id)
                .fetch_all(self.db.reader())
                .await?;
        self.remove_versions(drive_id, &ids).await
    }

    /// Apply a user's version rules: thin each file's versions by age, and
    /// remove the versions of files that are gone (deleted on disk, say),
    /// and bytes no row refers to (a write that stopped half way).
    pub async fn thin_versions(
        self: &Arc<Self>,
        drive_id: Uuid,
        rules: &crate::retention::Rules,
        now: jiff::Timestamp,
    ) -> Result<(), FileError> {
        // The writes keep their versions with the tree's lock held: none is
        // half made while this looks.
        let tree = self.tree(drive_id).await?;
        // A drive being moved is left as it is until it is done.
        let _guard = tree.write_lock().await?;
        let rows: Vec<(Uuid, Uuid, String, bool)> = sqlx::query_as(
            "SELECT v.id, v.node_id, v.created_at,
               v.trash_id IS NULL AND NOT EXISTS (SELECT 1 FROM nodes n WHERE n.id = v.node_id)
             FROM versions v WHERE v.drive_id = ?1",
        )
        .bind(drive_id)
        .fetch_all(self.db.reader())
        .await?;
        let mut by_node: std::collections::HashMap<Uuid, Vec<(Uuid, jiff::Timestamp)>> =
            Default::default();
        let mut gone = Vec::new();
        let mut known = std::collections::HashSet::new();
        for (id, node, created_at, orphan) in rows {
            known.insert(id);
            if orphan {
                gone.push(id);
                continue;
            }
            match created_at.parse() {
                Ok(at) => by_node.entry(node).or_default().push((id, at)),
                Err(_) => gone.push(id),
            }
        }
        for versions in by_node.values() {
            gone.extend(crate::retention::thin(rules, versions, now));
        }
        self.remove_versions(drive_id, &gone).await?;

        // Bytes without a row: a write that stopped after keeping them.
        for name in tree.storage.names_in(VERSIONS_DIR).await? {
            let known = name
                .parse::<Uuid>()
                .ok()
                .is_some_and(|id| known.contains(&id));
            if !known {
                let _ = tree
                    .storage
                    .remove_all(&format!("{VERSIONS_DIR}/{name}"))
                    .await;
            }
        }
        Ok(())
    }

    /// The bytes the versions and the trash of a user take.
    pub async fn kept_sizes(&self, drive_id: Uuid) -> Result<(i64, i64), FileError> {
        let versions: i64 =
            sqlx::query_scalar("SELECT coalesce(sum(size), 0) FROM versions WHERE drive_id = ?1")
                .bind(drive_id)
                .fetch_one(self.db.reader())
                .await?;
        let trash: i64 =
            sqlx::query_scalar("SELECT coalesce(sum(size), 0) FROM trash WHERE drive_id = ?1")
                .bind(drive_id)
                .fetch_one(self.db.reader())
                .await?;
        Ok((versions, trash))
    }
}
