//! What Organizer changes in a tree itself: new folders, renames and moves,
//! deletes (to the trash), restores and purges. Each one holds the tree's
//! lock and is journaled: the intent is written, then the storage is
//! changed, then the index, the change feed and the end of the intent are
//! committed together (see `journal.rs` for a crash in between).
//!
//! The bytes go through the drive's storage (`storage.rs`), the server's
//! disk or a backend. A move, a delete to the trash and a restore run as a
//! task of their own: on S3 they copy every object, which a dropped
//! request must not cut short.

use super::{
    Change, FileError, Files, Kind, NODE_COLUMNS, Node, Tree, journal, names, path_of,
    record_change,
};
use crate::storage::join;
use serde::Serialize;
use std::sync::Arc;
use utoipa::ToSchema;
use uuid::Uuid;

/// Where a written file goes.
pub enum WriteTarget {
    /// A new file in a folder.
    New { parent: Uuid, name: String },
    /// Over an existing file, at its current version; what writes it is
    /// recorded with the version kept of the contents it replaces.
    Replace {
        id: Uuid,
        if_version: i64,
        source: Option<super::versions::Source>,
    },
    /// A mirror's copy of a file (docs/M7D_DESIGN.md): a new file whose
    /// name is kept as the drive mirrored has it, and the node it copies.
    Copy {
        parent: Uuid,
        name: String,
        source: Uuid,
    },
}

/// What kind of folder [`Files::make_folder`] makes.
#[derive(Clone, Copy)]
enum NewFolder {
    Plain,
    /// The top of an end-to-end-encrypted folder.
    Encrypted,
    /// A mirror's copy of a folder (`source`), its name as it is there;
    /// `sealed`: the top of an encrypted folder (its keys stay the
    /// drive mirrored's).
    Copy {
        source: Uuid,
        sealed: bool,
    },
}

/// A deleted file or folder, in the trash.
#[derive(Clone, Debug, Serialize, sqlx::FromRow, ToSchema)]
pub struct TrashItem {
    /// The id it had in the tree, which a restore gives back.
    pub id: Uuid,
    pub name: String,
    pub kind: Kind,
    /// Where it was, relative to the root, e.g. `Documents/Taxes`.
    pub original_path: String,
    /// In bytes; 0 for a folder.
    pub size: i64,
    pub deleted_at: String,
    /// The folder it was in (its id), null at the top.
    #[schema(required = true)]
    pub original_parent_id: Option<Uuid>,
    /// The encrypted folder it was in, as `Node.e2e_root`: its name is
    /// sealed with `original_parent_id` (docs/E2E_SPEC.md), but for the
    /// encrypted folder itself.
    #[schema(required = true)]
    pub e2e_root: Option<Uuid>,
    /// In a mirror (or a drive ejected from one): the node of the drive
    /// mirrored it copied, as `Node.source_id`; null elsewhere.
    #[schema(required = true)]
    pub source_id: Option<Uuid>,
    /// In a mirror: the `source_id` of the folder it was in, the id its
    /// name is sealed with inside an encrypted folder (in place of
    /// `original_parent_id`).
    #[schema(required = true)]
    pub original_parent_source_id: Option<Uuid>,
    /// In a mirror: the `source_id` of its encrypted folder, the id the
    /// keys are asked by (in place of `e2e_root`).
    #[schema(required = true)]
    pub e2e_root_source_id: Option<Uuid>,
    /// What the signed-in user may do with it: their role on its drive
    /// (restoring takes an editor, deleting for good the owner or a shared
    /// drive's manager). Null when they have no role on the drive (an item
    /// shared with them that they deleted: it is in its drive's trash).
    #[sqlx(skip)]
    #[schema(required = true)]
    pub my_role: Option<super::roles::Role>,
}

/// The columns of `trash` a [`TrashItem`] is read from.
const TRASH_COLUMNS: &str = "id, name, kind, original_path, size, deleted_at, original_parent_id, \
     e2e_root, source_id, parent_source_id AS original_parent_source_id, e2e_root_source_id";

impl Files {
    /// A node of a user's tree.
    pub async fn node(&self, drive_id: Uuid, id: Uuid) -> Result<Node, FileError> {
        sqlx::query_as(sqlx::AssertSqlSafe(format!(
            "SELECT {NODE_COLUMNS} FROM nodes WHERE id = ?1 AND drive_id = ?2"
        )))
        .bind(id)
        .bind(drive_id)
        .fetch_optional(self.db.reader())
        .await?
        .ok_or(FileError::NotFound)
    }

    /// The user's root folder.
    pub async fn root_node(self: &Arc<Self>, drive_id: Uuid) -> Result<Node, FileError> {
        let root: Option<Node> = sqlx::query_as(sqlx::AssertSqlSafe(format!(
            "SELECT {NODE_COLUMNS} FROM nodes WHERE drive_id = ?1 AND parent_id IS NULL"
        )))
        .bind(drive_id)
        .fetch_optional(self.db.reader())
        .await?;
        match root {
            Some(root) => Ok(root),
            // Not indexed yet: index it now.
            None => {
                self.scan(drive_id).await?;
                Box::pin(self.root_node(drive_id)).await
            }
        }
    }

    /// The node at a path relative to the root (`Documents/Taxes`); names
    /// are matched as stored, and in NFC.
    pub async fn resolve(
        self: &Arc<Self>,
        drive_id: Uuid,
        path: &std::path::Path,
    ) -> Result<Node, FileError> {
        let mut node = self.root_node(drive_id).await?;
        for part in path.components() {
            let std::path::Component::Normal(name) = part else {
                continue;
            };
            let name = name.to_string_lossy();
            node = sqlx::query_as(sqlx::AssertSqlSafe(format!(
                "SELECT {NODE_COLUMNS} FROM nodes WHERE parent_id = ?1 AND name IN (?2, ?3)"
            )))
            .bind(node.id)
            .bind(name.as_ref())
            .bind(names::nfc(&name))
            .fetch_optional(self.db.reader())
            .await?
            .ok_or(FileError::NotFound)?;
        }
        Ok(node)
    }

    /// Run an operation as a task of its own, so that it ends even when the
    /// request that asked for it goes away.
    async fn detached<T: Send + 'static>(
        work: impl std::future::Future<Output = Result<T, FileError>> + Send + 'static,
    ) -> Result<T, FileError> {
        tokio::spawn(work)
            .await
            .map_err(|e| FileError::Internal(anyhow::anyhow!("an operation stopped: {e}")))?
    }

    /// A move in storage failed. On the disk nothing moved. On S3 some
    /// objects may have: they go back, and the intent with them; if that
    /// fails too, the intent stays and the next start finishes the move
    /// (the node then goes where it was going).
    async fn undo_copies(&self, tree: &Tree, intent: journal::Intent, from: &str, to: &str) {
        if !tree.storage.renames()
            && let Err(e) = tree.storage.move_objects(to, from).await
        {
            tracing::warn!(
                "a move in drive {} is finished at the next start: {e}",
                tree.drive_id
            );
            return;
        }
        journal::abandon(self, intent).await;
    }

    /// A new folder in a folder.
    pub async fn create_folder(
        self: &Arc<Self>,
        drive_id: Uuid,
        parent_id: Uuid,
        name: &str,
    ) -> Result<Node, FileError> {
        let name = names::new_name(name).map_err(FileError::Invalid)?;
        self.make_folder(drive_id, parent_id, name, NewFolder::Plain)
            .await
    }

    /// A new end-to-end-encrypted folder (docs/E2E_SPEC.md): the top of
    /// one, never inside another. It has no key yet: the owner's device
    /// posts its first (`routes/encryption.rs`).
    pub async fn create_encrypted_folder(
        self: &Arc<Self>,
        drive_id: Uuid,
        parent_id: Uuid,
        name: &str,
    ) -> Result<Node, FileError> {
        let name = names::new_name(name).map_err(FileError::Invalid)?;
        self.make_folder(drive_id, parent_id, name, NewFolder::Encrypted)
            .await
    }

    /// A mirror's copy of a folder of the drive it mirrors (`source`): the
    /// name as it is there; `sealed`, the top of an encrypted folder.
    pub(crate) async fn copy_folder(
        self: &Arc<Self>,
        drive_id: Uuid,
        parent_id: Uuid,
        name: &str,
        source: Uuid,
        sealed: bool,
    ) -> Result<Node, FileError> {
        self.make_folder(
            drive_id,
            parent_id,
            name.to_string(),
            NewFolder::Copy { source, sealed },
        )
        .await
    }

    async fn make_folder(
        self: &Arc<Self>,
        drive_id: Uuid,
        parent_id: Uuid,
        name: String,
        kind: NewFolder,
    ) -> Result<Node, FileError> {
        let (encrypted, source) = match kind {
            NewFolder::Plain => (false, None),
            NewFolder::Encrypted => (true, None),
            NewFolder::Copy { source, sealed } => (sealed, Some(source)),
        };
        let tree = self.tree(drive_id).await?;
        let _guard = tree.write_lock().await?;
        let parent = self.folder(drive_id, parent_id).await?;
        if encrypted && parent.e2e_root.is_some() {
            return Err(FileError::Invalid(
                "An encrypted folder cannot be made inside another: make a plain folder in it"
                    .into(),
            ));
        }
        match source {
            None => self.check_free(parent.id, &name, None).await?,
            Some(_) => self.check_free_exactly(parent.id, &name, None).await?,
        }
        let path = join(&self.rel_path(parent.id).await?, &name);

        let intent = journal::begin(
            self,
            drive_id,
            "create_folder",
            None,
            None,
            Some(&path),
            None,
        )
        .await?;
        if let Err(e) = tree.storage.create_dir(&path).await {
            journal::abandon(self, intent).await;
            return Err(match e.kind() {
                std::io::ErrorKind::AlreadyExists => {
                    FileError::Conflict(format!("{name} already exists"))
                }
                _ => e.into(),
            });
        }
        // A bucket's folder has no time of its own.
        let mtime = tree.storage.file_meta(&path).await.map_or(0, |(_, m)| m);
        let id = Uuid::now_v7();
        let mut tx = self.db.writer().begin().await?;
        sqlx::query(
            "INSERT INTO nodes (id, drive_id, parent_id, name, kind, mtime_ns, e2e_root, portable,
               source_id)
             VALUES (?1, ?2, ?3, ?4, 'folder', ?5, ?6, ?7, ?8)",
        )
        .bind(id)
        .bind(drive_id)
        .bind(parent.id)
        .bind(&name)
        .bind(mtime)
        .bind(encrypted.then_some(id))
        .bind(names::is_portable(&name))
        .bind(source)
        .execute(&mut *tx)
        .await?;
        // A copy's keys stay those of the folder it copies.
        if encrypted && source.is_none() {
            sqlx::query("INSERT INTO e2e_folders (root_id) VALUES (?1)")
                .bind(id)
                .execute(&mut *tx)
                .await?;
        }
        record_change(
            &mut tx,
            drive_id,
            Change {
                node_id: id,
                op: "create",
                kind: Some(Kind::Folder),
                parent_id: Some(parent.id),
                name: Some(&name),
                version: Some(1),
                from_parent: None,
                from_path: None,
            },
        )
        .await?;
        journal::end(&mut tx, intent).await?;
        tx.commit().await?;
        self.notify(drive_id);
        self.node(drive_id, id).await
    }

    /// Rename a node, move it to another folder, or both. `if_version` must
    /// be its current version.
    pub async fn move_node(
        self: &Arc<Self>,
        drive_id: Uuid,
        id: Uuid,
        new_parent: Option<Uuid>,
        new_name: Option<&str>,
        if_version: i64,
    ) -> Result<Node, FileError> {
        let new_name = new_name
            .map(names::new_name)
            .transpose()
            .map_err(FileError::Invalid)?;
        let files = self.clone();
        Self::detached(async move {
            files
                .move_now(drive_id, id, new_parent, new_name, if_version, false)
                .await
        })
        .await
    }

    /// A mirror's move of a copy to where its source is now: the name as
    /// it is there.
    pub(crate) async fn move_copy(
        self: &Arc<Self>,
        drive_id: Uuid,
        id: Uuid,
        new_parent: Uuid,
        new_name: &str,
        if_version: i64,
    ) -> Result<Node, FileError> {
        let files = self.clone();
        let new_name = new_name.to_string();
        Self::detached(async move {
            files
                .move_now(
                    drive_id,
                    id,
                    Some(new_parent),
                    Some(new_name),
                    if_version,
                    true,
                )
                .await
        })
        .await
    }

    async fn move_now(
        self: &Arc<Self>,
        drive_id: Uuid,
        id: Uuid,
        new_parent: Option<Uuid>,
        new_name: Option<String>,
        if_version: i64,
        // A mirror's copy: only the same name clashes (`check_free_exactly`).
        exact: bool,
    ) -> Result<Node, FileError> {
        let tree = self.tree(drive_id).await?;
        let _guard = tree.write_lock().await?;
        let node = self.node(drive_id, id).await?;
        let Some(old_parent) = node.parent_id else {
            return Err(FileError::Invalid("The root folder cannot be moved".into()));
        };
        check_version(&node, if_version)?;
        let parent = match new_parent {
            Some(parent) => self.folder(drive_id, parent).await?,
            None => self.node(drive_id, old_parent).await?,
        };
        let name = new_name.unwrap_or_else(|| node.name.clone());
        if parent.id == old_parent && name == node.name {
            return Ok(node);
        }
        // Not into itself, or anything under it.
        if node.kind == Kind::Folder && self.is_within(parent.id, node.id).await? {
            return Err(FileError::Invalid(
                "A folder cannot be moved into itself".into(),
            ));
        }
        super::e2e::check_move(&node, &parent)?;
        if exact {
            self.check_free_exactly(parent.id, &name, Some(node.id))
                .await?;
        } else {
            self.check_free(parent.id, &name, Some(node.id)).await?;
        }
        let from = self.rel_path(node.id).await?;
        let to = join(&self.rel_path(parent.id).await?, &name);
        // A rename that only changes the case is the same file on a
        // case-insensitive disk: it exists already, and is allowed.
        if tree.storage.exists(&to).await?
            && !(parent.id == old_parent && names::fold(&name) == names::fold(&node.name))
        {
            return Err(FileError::Conflict(format!("{name} already exists")));
        }

        let intent = journal::begin(
            self,
            drive_id,
            "move",
            Some(id),
            Some(&from),
            Some(&to),
            Some((parent.id, &name)),
        )
        .await?;
        if let Err(e) = tree.storage.rename(&from, &to).await {
            self.undo_copies(&tree, intent, &from, &to).await;
            return Err(e.into());
        }
        let mut tx = self.db.writer().begin().await?;
        let version: i64 = sqlx::query_scalar(
            "UPDATE nodes SET parent_id = ?1, name = ?2, portable = ?5, version = version + 1,
               mime = CASE kind WHEN 'file' THEN ?3 ELSE NULL END,
               updated_at = strftime('%Y-%m-%dT%H:%M:%fZ', 'now')
             WHERE id = ?4 RETURNING version",
        )
        .bind(parent.id)
        .bind(&name)
        // An encrypted name says nothing of the type.
        .bind(if node.e2e_root.is_some() {
            None
        } else {
            super::scan::mime_of(&name)
        })
        .bind(id)
        .bind(names::is_portable(&name))
        .fetch_one(&mut *tx)
        .await?;
        record_change(
            &mut tx,
            drive_id,
            Change {
                node_id: id,
                op: "move",
                kind: Some(node.kind),
                parent_id: Some(parent.id),
                name: Some(&name),
                version: Some(version),
                from_parent: Some(old_parent),
                from_path: Some(&from),
            },
        )
        .await?;
        journal::end(&mut tx, intent).await?;
        tx.commit().await?;
        self.notify(drive_id);
        self.node(drive_id, id).await
    }

    /// Move a node out of the tree into the trash. Nothing is deleted for
    /// good until the trash is emptied.
    pub async fn trash(
        self: &Arc<Self>,
        drive_id: Uuid,
        id: Uuid,
        if_version: i64,
    ) -> Result<TrashItem, FileError> {
        let files = self.clone();
        Self::detached(async move { files.trash_now(drive_id, id, if_version).await }).await
    }

    async fn trash_now(
        self: &Arc<Self>,
        drive_id: Uuid,
        id: Uuid,
        if_version: i64,
    ) -> Result<TrashItem, FileError> {
        let tree = self.tree(drive_id).await?;
        let _guard = tree.write_lock().await?;
        let node = self.node(drive_id, id).await?;
        if node.parent_id.is_none() {
            return Err(FileError::Invalid(
                "The root folder cannot be deleted".into(),
            ));
        }
        check_version(&node, if_version)?;
        let parts = path_of(self.db.reader(), id).await?;
        let from = parts.join("/");
        let to = Tree::trash_path(id);
        tree.prepare()?;
        // A share from another server: deleted there, whose own trash
        // keeps it (we keep nothing of another server's files).
        if tree.storage.is_remote() {
            tree.storage.remove_all(&from).await?;
            let mut tx = self.db.writer().begin().await?;
            record_change(
                &mut tx,
                drive_id,
                Change {
                    node_id: id,
                    op: "delete",
                    kind: None,
                    parent_id: None,
                    name: None,
                    version: None,
                    from_parent: None,
                    from_path: None,
                },
            )
            .await?;
            sqlx::query("DELETE FROM nodes WHERE id = ?1")
                .bind(id)
                .execute(&mut *tx)
                .await?;
            tx.commit().await?;
            self.notify(drive_id);
            return Ok(TrashItem {
                id,
                name: node.name,
                kind: node.kind,
                original_path: from,
                size: node.size,
                deleted_at: crate::auth::now_iso(),
                original_parent_id: node.parent_id,
                e2e_root: node.e2e_root,
                source_id: None,
                original_parent_source_id: None,
                e2e_root_source_id: None,
                my_role: None,
            });
        }

        // Its shares with other servers end with it: their servers are told.
        let ended = crate::federation::outgoing::shares_under(self.db.reader(), id).await?;
        let intent = journal::begin(
            self,
            drive_id,
            "trash",
            Some(id),
            Some(&from),
            Some(&to),
            None,
        )
        .await?;
        if let Err(e) = tree.storage.rename(&from, &to).await {
            self.undo_copies(&tree, intent, &from, &to).await;
            return Err(e.into());
        }
        let mut tx = self.db.writer().begin().await?;
        sqlx::query(
            "INSERT INTO trash (id, drive_id, name, kind, original_parent_id, original_path, size,
               e2e_root, source_id, parent_source_id, e2e_root_source_id)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9,
               (SELECT source_id FROM nodes WHERE id = ?5),
               (SELECT source_id FROM nodes WHERE id = ?8))",
        )
        .bind(id)
        .bind(drive_id)
        .bind(&node.name)
        .bind(node.kind.as_str())
        .bind(node.parent_id)
        .bind(parts.join("/"))
        .bind(node.size)
        .bind(node.e2e_root)
        .bind(node.source_id)
        .execute(&mut *tx)
        .await?;
        super::e2e::keep_for_trash(&mut tx, id).await?;
        keep_marks(&mut tx, id).await?;
        Self::trash_versions(&mut tx, id).await?;
        record_change(
            &mut tx,
            drive_id,
            Change {
                node_id: id,
                op: "delete",
                kind: None,
                parent_id: None,
                name: None,
                version: None,
                from_parent: None,
                from_path: None,
            },
        )
        .await?;
        sqlx::query("DELETE FROM nodes WHERE id = ?1")
            .bind(id)
            .execute(&mut *tx)
            .await?;
        journal::end(&mut tx, intent).await?;
        tx.commit().await?;
        self.notify(drive_id);
        crate::federation::outgoing::tell_ended(self, ended);
        self.trashed(drive_id, id).await
    }

    /// Move a folder from outside the tree (a folder dropped into the drop
    /// box) into a folder of it, under `name`, whole; then index what came
    /// in. A stop half way is settled by the rescan at start: the folder is
    /// then simply new on disk.
    pub async fn import_dir(
        self: &Arc<Self>,
        drive_id: Uuid,
        parent_id: Uuid,
        name: &str,
        from: &std::path::Path,
    ) -> Result<Node, FileError> {
        let name = names::new_name(name).map_err(FileError::Invalid)?;
        let tree = self.tree(drive_id).await?;
        {
            let _guard = tree.write_lock().await?;
            let parent = self.folder(drive_id, parent_id).await?;
            self.check_free(parent.id, &name, None).await?;
            let to = join(&self.rel_path(parent.id).await?, &name);
            if tree.storage.exists(&to).await? {
                return Err(FileError::Conflict(format!("{name} already exists")));
            }
            tree.storage.put_dir(from, &to).await?;
        }
        self.scan(drive_id).await?;
        let folded = names::fold(&name);
        let children: Vec<Node> = sqlx::query_as(sqlx::AssertSqlSafe(format!(
            "SELECT {NODE_COLUMNS} FROM nodes WHERE drive_id = ?1 AND parent_id = ?2"
        )))
        .bind(drive_id)
        .bind(parent_id)
        .fetch_all(self.db.reader())
        .await?;
        children
            .into_iter()
            .find(|c| names::fold(&c.name) == folded)
            .ok_or(FileError::NotFound)
    }

    /// Move a file or a folder out of the tree to `to` (outside any tree):
    /// the drop box taking an item back. `if_version`, when given, must be its current
    /// version. A stop half way is settled by the rescan at start: the file
    /// is then only gone from the tree.
    pub async fn take_out(
        self: &Arc<Self>,
        drive_id: Uuid,
        id: Uuid,
        to: &std::path::Path,
        if_version: Option<i64>,
    ) -> Result<(), FileError> {
        let tree = self.tree(drive_id).await?;
        let _guard = tree.write_lock().await?;
        let node = self.node(drive_id, id).await?;
        if node.parent_id.is_none() {
            return Err(FileError::Invalid("The root folder cannot be moved".into()));
        }
        if let Some(version) = if_version {
            check_version(&node, version)?;
        }
        let from = self.rel_path(id).await?;
        if to.exists() {
            return Err(FileError::Conflict(format!("{} exists", to.display())));
        }
        let intent = journal::begin(
            self,
            drive_id,
            "take_out",
            Some(id),
            Some(&from),
            Some(&to.to_string_lossy()),
            None,
        )
        .await?;
        if let Err(e) = tree.storage.take_out(&from, to).await {
            journal::abandon(self, intent).await;
            return Err(e.into());
        }
        let mut tx = self.db.writer().begin().await?;
        record_change(
            &mut tx,
            drive_id,
            Change {
                node_id: id,
                op: "delete",
                kind: None,
                parent_id: None,
                name: None,
                version: None,
                from_parent: None,
                from_path: None,
            },
        )
        .await?;
        sqlx::query("DELETE FROM nodes WHERE id = ?1")
            .bind(id)
            .execute(&mut *tx)
            .await?;
        journal::end(&mut tx, intent).await?;
        tx.commit().await?;
        self.notify(drive_id);
        Ok(())
    }

    /// The child folder of a folder by name, ignoring case and Unicode form.
    pub async fn child_folder(
        &self,
        drive_id: Uuid,
        parent_id: Uuid,
        name: &str,
    ) -> Result<Option<Node>, FileError> {
        let wanted = names::fold(name);
        let children: Vec<Node> = sqlx::query_as(sqlx::AssertSqlSafe(format!(
            "SELECT {NODE_COLUMNS} FROM nodes
             WHERE drive_id = ?1 AND parent_id = ?2 AND kind = 'folder'"
        )))
        .bind(drive_id)
        .bind(parent_id)
        .fetch_all(self.db.reader())
        .await?;
        Ok(children
            .into_iter()
            .find(|c| names::fold(&c.name) == wanted))
    }

    /// Whether a folder has nothing in it, in the index.
    pub async fn is_empty_folder(&self, id: Uuid) -> Result<bool, FileError> {
        let (count,): (i64,) = sqlx::query_as("SELECT COUNT(*) FROM nodes WHERE parent_id = ?1")
            .bind(id)
            .fetch_one(self.db.reader())
            .await?;
        Ok(count == 0)
    }

    /// What is in a user's trash, the most recently deleted first.
    pub async fn trash_items(&self, drive_id: Uuid) -> Result<Vec<TrashItem>, FileError> {
        Ok(sqlx::query_as(sqlx::AssertSqlSafe(format!(
            "SELECT {TRASH_COLUMNS} FROM trash WHERE drive_id = ?1
             ORDER BY deleted_at DESC, id DESC"
        )))
        .bind(drive_id)
        .fetch_all(self.db.reader())
        .await?)
    }

    async fn trashed(&self, drive_id: Uuid, id: Uuid) -> Result<TrashItem, FileError> {
        sqlx::query_as(sqlx::AssertSqlSafe(format!(
            "SELECT {TRASH_COLUMNS} FROM trash WHERE id = ?1 AND drive_id = ?2"
        )))
        .bind(id)
        .bind(drive_id)
        .fetch_optional(self.db.reader())
        .await?
        .ok_or(FileError::NotFound)
    }

    /// Put a trashed node back where it was, with its id; into the root
    /// when that folder is gone. A name taken there in the meantime is a
    /// conflict.
    pub async fn restore(self: &Arc<Self>, drive_id: Uuid, id: Uuid) -> Result<Node, FileError> {
        let files = self.clone();
        Self::detached(async move { files.restore_now(drive_id, id).await }).await
    }

    async fn restore_now(self: &Arc<Self>, drive_id: Uuid, id: Uuid) -> Result<Node, FileError> {
        let tree = self.tree(drive_id).await?;
        let _guard = tree.write_lock().await?;
        let item = self.trashed(drive_id, id).await?;
        let (original_parent, e2e_root): (Option<Uuid>, Option<Uuid>) =
            sqlx::query_as("SELECT original_parent_id, e2e_root FROM trash WHERE id = ?1")
                .bind(id)
                .fetch_one(self.db.reader())
                .await?;
        // Inside an encrypted folder, its name is sealed with its parent's
        // id: only there can it be read again.
        let inside_encrypted = e2e_root.is_some_and(|root| root != id);
        let parent = match original_parent {
            Some(parent) => match self.folder(drive_id, parent).await {
                Ok(parent) => parent,
                Err(FileError::NotFound) if inside_encrypted => {
                    return Err(FileError::Conflict(
                        "The encrypted folder it was in is gone: restore that first".into(),
                    ));
                }
                Err(FileError::NotFound) => self.root_node_locked(drive_id).await?,
                Err(e) => return Err(e),
            },
            None => self.root_node_locked(drive_id).await?,
        };
        self.check_free(parent.id, &item.name, None).await?;
        let from = Tree::trash_path(id);
        let to = join(&self.rel_path(parent.id).await?, &item.name);
        if tree.storage.exists(&to).await? {
            return Err(FileError::Conflict(format!("{} already exists", item.name)));
        }

        let intent = journal::begin(
            self,
            drive_id,
            "restore",
            Some(id),
            Some(&from),
            Some(&to),
            None,
        )
        .await?;
        if let Err(e) = tree.storage.rename(&from, &to).await {
            self.undo_copies(&tree, intent, &from, &to).await;
            return Err(e.into());
        }
        let (size, mtime) = match item.kind {
            Kind::File => tree.storage.file_meta(&to).await?,
            // A bucket's folder has no time of its own.
            Kind::Folder => (0, tree.storage.file_meta(&to).await.map_or(0, |(_, m)| m)),
        };
        // What was inside an encrypted folder, with its place in storage
        // looked at before the transaction (no slow work inside one).
        let kept = {
            let mut conn = self.db.reader().acquire().await?;
            super::e2e::kept_under(&mut conn, id).await?
        };
        let kept = super::e2e::with_storage_meta(&tree.storage, &to, kept).await?;
        let mut tx = self.db.writer().begin().await?;
        let marks: Vec<(String, bool, Option<Uuid>)> = sqlx::query_as(
            "SELECT rel_path, limited, created_by FROM trash_marks WHERE trash_id = ?1
             ORDER BY length(rel_path)",
        )
        .bind(id)
        .fetch_all(&mut *tx)
        .await?;
        let top = marks.iter().find(|(path, _, _)| path.is_empty());
        sqlx::query(
            "INSERT INTO nodes (id, drive_id, parent_id, name, kind, size, mtime_ns, mime, portable,
               e2e_root, limited, created_by, source_id)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13)",
        )
        .bind(id)
        .bind(drive_id)
        .bind(parent.id)
        .bind(&item.name)
        .bind(item.kind.as_str())
        .bind(size)
        .bind(mtime)
        .bind(match (item.kind, e2e_root) {
            (Kind::File, None) => super::scan::mime_of(&item.name),
            _ => None,
        })
        .bind(names::is_portable(&item.name))
        .bind(e2e_root)
        .bind(top.is_some_and(|(_, limited, _)| *limited))
        .bind(top.and_then(|(_, _, by)| *by))
        // In a drive ejected from a mirror, the drive's id it keeps: its
        // name and keys may be sealed with it.
        .bind(item.source_id)
        .execute(&mut *tx)
        .await?;
        sqlx::query("DELETE FROM trash WHERE id = ?1")
            .bind(id)
            .execute(&mut *tx)
            .await?;
        record_change(
            &mut tx,
            drive_id,
            Change {
                node_id: id,
                op: "create",
                kind: Some(item.kind),
                parent_id: Some(parent.id),
                name: Some(&item.name),
                version: Some(1),
                from_parent: None,
                from_path: None,
            },
        )
        .await?;
        super::e2e::restore_kept(&mut tx, drive_id, id, kept).await?;
        journal::end(&mut tx, intent).await?;
        tx.commit().await?;
        self.notify(drive_id);
        // What is inside a restored folder is indexed like anything new.
        if item.kind == Kind::Folder {
            super::scan::scan(&self.db, drive_id, &tree.storage).await?;
            // What was limited, and who added what, as it was.
            for (path, limited, by) in marks.iter().filter(|(path, _, _)| !path.is_empty()) {
                let mut at = id;
                let mut found = true;
                for name in path.split('/') {
                    let child: Option<Uuid> = sqlx::query_scalar(
                        "SELECT id FROM nodes WHERE parent_id = ?1 AND name = ?2",
                    )
                    .bind(at)
                    .bind(name)
                    .fetch_optional(self.db.reader())
                    .await?;
                    match child {
                        Some(child) => at = child,
                        None => {
                            found = false;
                            break;
                        }
                    }
                }
                if found {
                    sqlx::query("UPDATE nodes SET limited = ?1, created_by = ?2 WHERE id = ?3")
                        .bind(limited)
                        .bind(by)
                        .bind(at)
                        .execute(self.db.writer())
                        .await?;
                }
            }
        }
        self.restore_versions(drive_id, id).await?;
        self.node(drive_id, id).await
    }

    /// Delete a trashed node for good.
    pub async fn purge(self: &Arc<Self>, drive_id: Uuid, id: Uuid) -> Result<(), FileError> {
        let tree = self.tree(drive_id).await?;
        let _guard = tree.write_lock().await?;
        self.trashed(drive_id, id).await?;
        match tree.storage.remove_all(&Tree::trash_path(id)).await {
            Ok(()) => {}
            // Gone already (a purge that crashed after it): the row goes.
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
            Err(e) => return Err(e.into()),
        }
        sqlx::query("DELETE FROM trash WHERE id = ?1")
            .bind(id)
            .execute(self.db.writer())
            .await?;
        self.purge_versions(drive_id, id).await?;
        super::e2e::forget_gone(&self.db).await?;
        Ok(())
    }

    /// What of a drive's trash a user sees, each with their role on the
    /// drive (`my_role`): everything for its owner and managers; for the others, not what was limited or in a folder hidden
    /// from them, and for upload only, only what they added.
    pub async fn trash_items_for(
        &self,
        drive_id: Uuid,
        user: Uuid,
        role: super::roles::Role,
    ) -> Result<Vec<TrashItem>, FileError> {
        let mut items = self.trash_items(drive_id).await?;
        for item in &mut items {
            item.my_role = Some(role);
        }
        if role >= super::roles::Role::Manager {
            return Ok(items);
        }
        let mut conn = self.db.reader().acquire().await?;
        let mut out = Vec::with_capacity(items.len());
        for item in items {
            let mark: Option<(bool, Option<Uuid>)> = sqlx::query_as(
                "SELECT limited, created_by FROM trash_marks WHERE trash_id = ?1 AND rel_path = ''",
            )
            .bind(item.id)
            .fetch_optional(&mut *conn)
            .await?;
            let (limited, by) = mark.unwrap_or((false, None));
            if limited || (role == super::roles::Role::UploadOnly && by != Some(user)) {
                continue;
            }
            if let Some(parent) = item.original_parent_id {
                let there: Option<i64> = sqlx::query_scalar("SELECT 1 FROM nodes WHERE id = ?1")
                    .bind(parent)
                    .fetch_optional(&mut *conn)
                    .await?;
                if there.is_some() && !super::roles::sees(&mut conn, user, parent).await? {
                    continue;
                }
            }
            out.push(item);
        }
        Ok(out)
    }

    /// Note who added a node through Organizer (upload only sees theirs).
    pub async fn mark_created(&self, id: Uuid, user: Uuid) -> Result<(), FileError> {
        sqlx::query("UPDATE nodes SET created_by = ?2 WHERE id = ?1")
            .bind(id)
            .bind(user)
            .execute(self.db.writer())
            .await?;
        Ok(())
    }

    /// Delete everything in a user's trash for good. Answers how many items
    /// went.
    pub async fn empty_trash(self: &Arc<Self>, drive_id: Uuid) -> Result<usize, FileError> {
        let items = self.trash_items(drive_id).await?;
        for item in &items {
            match self.purge(drive_id, item.id).await {
                Ok(()) | Err(FileError::NotFound) => {}
                Err(e) => return Err(e),
            }
        }
        Ok(items.len())
    }

    /// Put a file received into `temp` (in the uploads folder) into the
    /// tree: as a new file in a folder, or over an existing one at its
    /// current version. The upload is moved in whole, so nobody sees half a
    /// file; `temp` is gone afterwards either way.
    pub async fn write_file(
        self: &Arc<Self>,
        drive_id: Uuid,
        target: WriteTarget,
        temp: &std::path::Path,
    ) -> Result<Node, FileError> {
        // New bytes through Organizer: within the user's storage limit.
        let size = std::fs::metadata(temp).map(|m| m.len()).unwrap_or(0);
        let result = match self.check_room(drive_id, size).await {
            Ok(()) => self.place_file(drive_id, target, temp).await,
            Err(e) => Err(e),
        };
        if result.is_err() {
            let _ = std::fs::remove_file(temp);
        }
        result
    }

    /// [`Files::write_file`], but `temp` stays where it is when the file
    /// cannot be placed: a large resumable upload is not thrown away for a
    /// name taken in the meantime.
    pub async fn place_file(
        self: &Arc<Self>,
        drive_id: Uuid,
        target: WriteTarget,
        temp: &std::path::Path,
    ) -> Result<Node, FileError> {
        // Hashed before the lock: a large file takes a while.
        let hash = super::scan::hash_file(temp)
            .await
            .ok_or_else(|| anyhow::anyhow!("cannot read the upload {}", temp.display()))?;
        let size = std::fs::metadata(temp)?.len() as i64;
        let tree = self.tree(drive_id).await?;
        let _guard = tree.write_lock().await?;
        let source = match &target {
            WriteTarget::Replace { source, .. } => *source,
            WriteTarget::New { .. } | WriteTarget::Copy { .. } => None,
        };
        let copy_of = match &target {
            WriteTarget::Copy { source, .. } => Some(*source),
            _ => None,
        };
        let mut encrypted_parent = false;
        let (existing, parent_id, name, path) = match target {
            WriteTarget::New { parent, name } | WriteTarget::Copy { parent, name, .. } => {
                let name = match copy_of {
                    None => names::new_name(&name).map_err(FileError::Invalid)?,
                    Some(_) => name,
                };
                let parent = self.folder(drive_id, parent).await?;
                encrypted_parent = parent.e2e_root.is_some();
                match copy_of {
                    None => self.check_free(parent.id, &name, None).await?,
                    Some(_) => self.check_free_exactly(parent.id, &name, None).await?,
                }
                let path = join(&self.rel_path(parent.id).await?, &name);
                if tree.storage.exists(&path).await? {
                    return Err(FileError::Conflict(format!("{name} already exists")));
                }
                (None, parent.id, name, path)
            }
            WriteTarget::Replace {
                id,
                if_version,
                source: _,
            } => {
                let node = self.node(drive_id, id).await?;
                if node.kind != Kind::File {
                    return Err(FileError::Invalid("Not a file".into()));
                }
                check_version(&node, if_version)?;
                let path = self.rel_path(id).await?;
                let parent = node.parent_id.ok_or(FileError::NotFound)?;
                (Some(node), parent, String::new(), path)
            }
        };

        // The contents replaced are kept first; a stop before the write's
        // row leaves bytes without a row, which the sweep removes.
        // Not on another server's WebDAV: it keeps its own versions.
        let kept = match &existing {
            Some(node) if !tree.storage.is_remote() => {
                self.keep_version(&tree, node.id, &path, &hash).await?
            }
            _ => None,
        };
        let intent = journal::begin(
            self,
            drive_id,
            "write",
            existing.as_ref().map(|n| n.id),
            Some(&temp.to_string_lossy()),
            Some(&path),
            None,
        )
        .await?;
        if let Err(e) = tree.storage.put_file(temp, &path).await {
            journal::abandon(self, intent).await;
            if let Some(kept) = &kept {
                let _ = tree.storage.remove_all(&kept.path).await;
            }
            return Err(e.into());
        }
        let (_, mtime) = tree.storage.file_meta(&path).await?;
        let mut tx = self.db.writer().begin().await?;
        let (id, op, version, name) = match &existing {
            Some(node) => {
                let version: i64 = sqlx::query_scalar(
                    "UPDATE nodes SET size = ?1, mtime_ns = ?2, hash = ?3, version = version + 1,
                       updated_at = strftime('%Y-%m-%dT%H:%M:%fZ', 'now')
                     WHERE id = ?4 RETURNING version",
                )
                .bind(size)
                .bind(mtime)
                .bind(&hash)
                .bind(node.id)
                .fetch_one(&mut *tx)
                .await?;
                if let Some(kept) = &kept {
                    Self::record_version(&mut tx, drive_id, node.id, kept, source).await?;
                }
                (node.id, "update", version, node.name.clone())
            }
            None => {
                let id = Uuid::now_v7();
                sqlx::query(
                    "INSERT INTO nodes (id, drive_id, parent_id, name, kind, size, mtime_ns, hash, mime,
                       portable, source_id)
                     VALUES (?1, ?2, ?3, ?4, 'file', ?5, ?6, ?7, ?8, ?9, ?10)",
                )
                .bind(id)
                .bind(drive_id)
                .bind(parent_id)
                .bind(&name)
                .bind(size)
                .bind(mtime)
                .bind(&hash)
                .bind(if encrypted_parent {
                    None
                } else {
                    super::scan::mime_of(&name)
                })
                .bind(names::is_portable(&name))
                .bind(copy_of)
                .execute(&mut *tx)
                .await?;
                (id, "create", 1, name)
            }
        };
        record_change(
            &mut tx,
            drive_id,
            Change {
                node_id: id,
                op,
                kind: Some(Kind::File),
                parent_id: Some(parent_id),
                name: Some(&name),
                version: Some(version),
                from_parent: None,
                from_path: None,
            },
        )
        .await?;
        journal::end(&mut tx, intent).await?;
        tx.commit().await?;
        self.notify(drive_id);
        self.node(drive_id, id).await
    }

    /// The root node, with the tree's lock already held.
    async fn root_node_locked(&self, drive_id: Uuid) -> Result<Node, FileError> {
        sqlx::query_as(sqlx::AssertSqlSafe(format!(
            "SELECT {NODE_COLUMNS} FROM nodes WHERE drive_id = ?1 AND parent_id IS NULL"
        )))
        .bind(drive_id)
        .fetch_optional(self.db.reader())
        .await?
        .ok_or(FileError::NotFound)
    }

    /// A folder of a user's tree; a file is not one.
    pub(crate) async fn folder(&self, drive_id: Uuid, id: Uuid) -> Result<Node, FileError> {
        let node = self.node(drive_id, id).await?;
        if node.kind != Kind::Folder {
            return Err(FileError::Invalid("Not a folder".into()));
        }
        Ok(node)
    }

    /// Whether a folder has no child whose name differs from `name` only in
    /// case (or not at all), but `except`.
    pub(crate) async fn check_free(
        &self,
        parent: Uuid,
        name: &str,
        except: Option<Uuid>,
    ) -> Result<(), FileError> {
        // Organizer's own folder is at every drive's root, never listed.
        if names::fold(name) == names::fold(super::ORGANIZER_DIR) {
            let at_root: Option<i64> =
                sqlx::query_scalar("SELECT 1 FROM nodes WHERE id = ?1 AND parent_id IS NULL")
                    .bind(parent)
                    .fetch_optional(self.db.reader())
                    .await?;
            if at_root.is_some() {
                return Err(FileError::Invalid(format!(
                    "{} is Organizer's own folder: choose another name",
                    super::ORGANIZER_DIR
                )));
            }
        }
        // The shares the user placed there are in the folder too.
        let siblings: Vec<(Uuid, String)> = sqlx::query_as(
            "SELECT id, name FROM nodes WHERE parent_id = ?1
             UNION ALL SELECT node_id, name FROM share_mounts WHERE parent_id = ?1",
        )
        .bind(parent)
        .fetch_all(self.db.reader())
        .await?;
        let key = names::fold(name);
        match siblings
            .iter()
            .find(|(id, other)| Some(*id) != except && names::fold(other) == key)
        {
            Some((_, other)) => Err(FileError::Conflict(format!("{other} already exists"))),
            None => Ok(()),
        }
    }

    /// [`Files::check_free`] for a mirror's copy: its names are the drive
    /// mirrored's, which may differ only in case (a bucket tells them
    /// apart), so only the same name clashes.
    pub(crate) async fn check_free_exactly(
        &self,
        parent: Uuid,
        name: &str,
        except: Option<Uuid>,
    ) -> Result<(), FileError> {
        let taken: Option<Uuid> =
            sqlx::query_scalar("SELECT id FROM nodes WHERE parent_id = ?1 AND name = ?2")
                .bind(parent)
                .bind(name)
                .fetch_optional(self.db.reader())
                .await?;
        match taken {
            Some(id) if Some(id) != except => {
                Err(FileError::Conflict(format!("{name} already exists")))
            }
            _ => Ok(()),
        }
    }

    /// Whether `id` is `ancestor` or under it.
    pub(crate) async fn is_within(&self, id: Uuid, ancestor: Uuid) -> Result<bool, FileError> {
        let found: Option<i64> = sqlx::query_scalar(
            "WITH RECURSIVE up (id, parent_id) AS (
               SELECT id, parent_id FROM nodes WHERE id = ?1
               UNION ALL
               SELECT n.id, n.parent_id FROM nodes n JOIN up ON n.id = up.parent_id
             )
             SELECT 1 FROM up WHERE id = ?2",
        )
        .bind(id)
        .bind(ancestor)
        .fetch_optional(self.db.reader())
        .await?;
        Ok(found.is_some())
    }
}

pub(crate) fn check_version(node: &Node, if_version: i64) -> Result<(), FileError> {
    if node.version != if_version {
        return Err(FileError::Conflict(format!(
            "{} has changed (version {}, not {if_version}): reload it",
            node.name, node.version
        )));
    }
    Ok(())
}

/// When a node goes to the trash (its `trash` row written, its node rows
/// not yet deleted): what was limited under it and who added what, by the
/// path under it, for the restore.
async fn keep_marks(tx: &mut sqlx::SqliteConnection, trash_id: Uuid) -> sqlx::Result<()> {
    sqlx::query(
        "WITH RECURSIVE down (id, rel_path, limited, created_by) AS (
           SELECT id, '', limited, created_by FROM nodes WHERE id = ?1
           UNION ALL
           SELECT n.id, CASE down.rel_path WHEN '' THEN n.name
                          ELSE down.rel_path || '/' || n.name END,
                  n.limited, n.created_by
           FROM nodes n JOIN down ON n.parent_id = down.id
         )
         INSERT INTO trash_marks (trash_id, rel_path, limited, created_by)
         SELECT ?1, rel_path, limited, created_by FROM down
         WHERE rel_path = '' OR limited = 1 OR created_by IS NOT NULL",
    )
    .bind(trash_id)
    .execute(tx)
    .await?;
    Ok(())
}
