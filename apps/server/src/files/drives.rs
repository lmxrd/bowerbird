//! Drives (docs/M7A_DESIGN.md §1): a drive is a tree with its own root,
//! trash, versions and storage limit. Each user has their **My drive**,
//! the tree they always had (its id is the user's own, one per user, so a
//! tree's rows kept their keys); **shared drives** are trees of their own,
//! under `<DATA_DIR>/drives/<id>` or a folder the superadmin picked, seen
//! by their members only.
//!
//! Who has a shared drive and with what role are its grants
//! (docs/M7B_DESIGN.md §1, `roles.rs`): grants on the drive to users and
//! teams, and the managers of the team it belongs to. A shared drive always
//! keeps a manager. Everything that asks what a user may do in a drive
//! goes through `roles`.

use super::roles::Role;
use super::{FileError, Files, ORGANIZER_DIR};
use serde::{Deserialize, Serialize};
use std::path::Path;
use utoipa::ToSchema;
use uuid::Uuid;

/// A user's My drive. Its id is the user's: the trees before drives were
/// keyed by their user, and stay so.
pub fn my(user_id: Uuid) -> Uuid {
    user_id
}

/// The kind of a drive.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize, ToSchema, sqlx::Type)]
#[serde(rename_all = "lowercase")]
#[sqlx(type_name = "TEXT", rename_all = "lowercase")]
pub enum DriveKind {
    /// A user's own drive.
    My,
    /// A drive of its members, outside every user's tree.
    Shared,
    /// A mirror (docs/M7D_DESIGN.md): a read-only copy of another drive in
    /// its user's own storage, which they view.
    Mirror,
    /// Someone on another server's file, folder or drive shared with a
    /// user here (docs/M7E_DESIGN.md §5): its files are on that server,
    /// read and written through ours (`remote` says which).
    Remote,
}

/// A row of `drives`.
#[derive(Clone, Debug, sqlx::FromRow)]
pub struct DriveRow {
    pub id: Uuid,
    pub kind: DriveKind,
    /// My drive's user.
    pub user_id: Option<Uuid>,
    pub name: String,
    /// A shared drive's chosen folder; None: the default in the data dir.
    pub root: Option<String>,
    pub storage_limit: Option<i64>,
    pub created_by: Option<Uuid>,
    pub created_at: String,
    /// The storage backend it is in; None: the server's disk.
    pub backend_id: Option<Uuid>,
}

impl DriveRow {
    pub async fn get<'e>(
        db: impl sqlx::SqliteExecutor<'e>,
        id: Uuid,
    ) -> sqlx::Result<Option<DriveRow>> {
        sqlx::query_as(
            "SELECT id, kind, user_id, name, root, storage_limit, created_by, created_at, backend_id
             FROM drives WHERE id = ?1",
        )
        .bind(id)
        .fetch_optional(db)
        .await
    }
}

/// The ids of the drives a user has: My drive first, then the shared
/// drives they are a member of (themselves or through a team), by name,
/// then their mirrors, by name.
pub async fn drives_of<'e>(
    db: impl sqlx::SqliteExecutor<'e>,
    user: Uuid,
) -> sqlx::Result<Vec<Uuid>> {
    sqlx::query_scalar(
        "SELECT id FROM drives WHERE user_id = ?1
         UNION ALL
         SELECT * FROM (SELECT d.id FROM drives d
                        WHERE d.kind = 'shared'
                          AND d.id IN (SELECT drive_id FROM drive_users WHERE user_id = ?1)
                        ORDER BY d.name COLLATE NOCASE, d.id)
         UNION ALL
         SELECT * FROM (SELECT d.id FROM drives d
                        WHERE d.kind = 'mirror'
                          AND d.id IN (SELECT drive_id FROM drive_users WHERE user_id = ?1)
                        ORDER BY d.name COLLATE NOCASE, d.id)
         UNION ALL
         SELECT * FROM (SELECT d.id FROM drives d
                        WHERE d.kind = 'remote'
                          AND d.id IN (SELECT drive_id FROM drive_users WHERE user_id = ?1)
                        ORDER BY d.name COLLATE NOCASE, d.id)",
    )
    .bind(user)
    .fetch_all(db)
    .await
}

/// The users of a drive (distinct).
pub(crate) async fn users_of(
    conn: &mut sqlx::SqliteConnection,
    drive: Uuid,
) -> sqlx::Result<Vec<(Uuid, Role)>> {
    let rows: Vec<(Uuid, Role)> =
        sqlx::query_as("SELECT user_id, role FROM drive_users WHERE drive_id = ?1")
            .bind(drive)
            .fetch_all(conn)
            .await?;
    let mut out: Vec<(Uuid, Role)> = Vec::new();
    for (user, role) in rows {
        match out.iter_mut().find(|(u, _)| *u == user) {
            Some(found) => found.1 = found.1.max(role),
            None => out.push((user, role)),
        }
    }
    Ok(out)
}

/// A drive's name as the apps show it.
pub fn display_name(row: &DriveRow) -> String {
    match row.kind {
        DriveKind::My => "My drive".into(),
        DriveKind::Shared | DriveKind::Mirror | DriveKind::Remote => row.name.clone(),
    }
}

/// A shared drive's name: 1 to 100 characters, portable (it is the name
/// of the drive's folder on the desktop).
pub fn clean_name(name: &str) -> Result<String, FileError> {
    let name = name.trim();
    if name.is_empty() || name.chars().count() > 100 {
        return Err(FileError::Invalid(
            "A drive's name is 1 to 100 characters".into(),
        ));
    }
    super::names::new_name(name).map_err(FileError::Invalid)
}

impl Files {
    /// Make a shared drive: its folder (the default in the data dir, or an
    /// existing folder, apart from every other drive and the data dir), its
    /// row, and its members. Nothing is indexed yet: that is the caller's
    /// `open_and_scan`.
    pub async fn create_shared_drive(
        &self,
        name: &str,
        root: Option<&Path>,
        created_by: Uuid,
    ) -> Result<Uuid, FileError> {
        self.create_shared_drive_in(name, root, None, created_by)
            .await
    }

    /// [`Files::create_shared_drive`] in a storage backend (`backend`;
    /// None: the server's disk): at its own prefix there, with the
    /// backend's limit for a new drive.
    pub async fn create_shared_drive_in(
        &self,
        name: &str,
        root: Option<&Path>,
        backend: Option<Uuid>,
        created_by: Uuid,
    ) -> Result<Uuid, FileError> {
        let name = clean_name(name)?;
        let id = Uuid::now_v7();
        let root = match root {
            Some(_) if backend.is_some() => {
                return Err(FileError::Invalid(
                    "A drive in a storage backend has no folder on the server".into(),
                ));
            }
            Some(root) => {
                let root = self.check_root(root)?;
                self.check_apart(id, &root).await?;
                Some(root.to_string_lossy().into_owned())
            }
            None => None,
        };
        let made = sqlx::query(
            "INSERT INTO drives (id, kind, name, root, created_by, backend_id, storage_limit)
             VALUES (?1, 'shared', ?2, ?3, ?4, ?5,
                     (SELECT drive_limit FROM backends WHERE id = ?5))",
        )
        .bind(id)
        .bind(&name)
        .bind(&root)
        .bind(created_by)
        .bind(backend)
        .execute(self.db.writer())
        .await;
        match made {
            Ok(_) => Ok(id),
            Err(sqlx::Error::Database(e)) if e.is_unique_violation() => Err(FileError::Conflict(
                "A shared drive has that name already".into(),
            )),
            Err(e) => Err(e.into()),
        }
    }

    /// Rename a shared drive.
    pub async fn rename_drive(&self, id: Uuid, name: &str) -> Result<(), FileError> {
        let name = clean_name(name)?;
        let renamed = sqlx::query("UPDATE drives SET name = ?1 WHERE id = ?2 AND kind = 'shared'")
            .bind(&name)
            .bind(id)
            .execute(self.db.writer())
            .await;
        match renamed {
            Ok(done) if done.rows_affected() == 0 => Err(FileError::NotFound),
            Ok(_) => Ok(()),
            Err(sqlx::Error::Database(e)) if e.is_unique_violation() => Err(FileError::Conflict(
                "A shared drive has that name already".into(),
            )),
            Err(e) => Err(e.into()),
        }
    }

    /// Set a drive's storage limit (None: none).
    pub async fn set_drive_limit(&self, id: Uuid, limit: Option<i64>) -> Result<(), FileError> {
        if limit.is_some_and(|l| l < 0) {
            return Err(FileError::Invalid(
                "A storage limit is 0 bytes or more".into(),
            ));
        }
        let done = sqlx::query("UPDATE drives SET storage_limit = ?1 WHERE id = ?2")
            .bind(limit)
            .bind(id)
            .execute(self.db.writer())
            .await?;
        if done.rows_affected() == 0 {
            return Err(FileError::NotFound);
        }
        Ok(())
    }

    /// Delete a shared drive: its rows (the tree, its trash, versions,
    /// shares and links go with it), the shares placed from it, and its
    /// folder in the data dir; a folder someone chose keeps its files and
    /// loses only Organizer's own folder. Each member's feed gets the root
    /// as deleted. In a storage backend, its objects there go too (when the
    /// backend answers; else they are left, and said so in the log).
    pub async fn delete_drive(&self, drive: Uuid) -> Result<(), FileError> {
        self.remove_drive(drive, true).await
    }

    /// Take a drive out of Organizer but leave its objects in its backend:
    /// a user removed their own storage, whose bucket is theirs.
    pub async fn forget_drive(&self, drive: Uuid) -> Result<(), FileError> {
        self.remove_drive(drive, false).await
    }

    async fn remove_drive(&self, drive: Uuid, delete_objects: bool) -> Result<(), FileError> {
        let row = DriveRow::get(self.db.reader(), drive)
            .await?
            .filter(|d| d.kind != DriveKind::My)
            .ok_or(FileError::NotFound)?;
        let tree = self.trees.lock().unwrap().remove(&drive);
        let _guard = match &tree {
            Some(tree) => Some(tree.read_lock().await),
            None => None,
        };
        let backend = match row.backend_id {
            Some(backend) => crate::backends::BackendRow::get(self.db.reader(), backend).await?,
            None => None,
        };
        let mut tx = self.db.writer().begin().await?;
        super::shares::end_mounts_on(&mut tx, drive).await?;
        let root: Option<Uuid> =
            sqlx::query_scalar("SELECT id FROM nodes WHERE drive_id = ?1 AND parent_id IS NULL")
                .bind(drive)
                .fetch_optional(&mut *tx)
                .await?;
        let members: Vec<Uuid> = users_of(&mut tx, drive)
            .await?
            .into_iter()
            .map(|(user, _)| user)
            .collect();
        if let Some(root) = root {
            for member in &members {
                root_row(&mut tx, *member, drive, root, "delete", None, None).await?;
            }
        }
        // The search index's text rows are not foreign keys: they go first.
        sqlx::query(
            "DELETE FROM search WHERE rowid IN (SELECT rowid FROM search_docs WHERE drive_id = ?1)",
        )
        .bind(drive)
        .execute(&mut *tx)
        .await?;
        sqlx::query("DELETE FROM drives WHERE id = ?1")
            .bind(drive)
            .execute(&mut *tx)
            .await?;
        tx.commit().await?;
        match &backend {
            Some(backend) if delete_objects => {
                let removed = match self.backends.storage(backend, drive).await {
                    Ok(storage) => storage.remove_drive().await.map_err(FileError::from),
                    Err(e) => Err(e),
                };
                if let Err(e) = removed {
                    tracing::warn!(
                        "the objects of the deleted drive {drive} are left in {}: {e}",
                        backend.location()
                    );
                }
            }
            Some(_) => {}
            None => {
                let path = match &row.root {
                    Some(root) => Path::new(root).join(ORGANIZER_DIR),
                    None => self.shared_default_root(drive),
                };
                if let Err(e) = std::fs::remove_dir_all(&path)
                    && e.kind() != std::io::ErrorKind::NotFound
                {
                    tracing::warn!("cannot remove {}: {e}", path.display());
                }
            }
        }
        for member in members {
            self.announce(member, super::Topic::Changes);
        }
        self.settle_mounts().await
    }

    /// Once, at the first start with drives: each user's trash and versions
    /// move from `<DATA_DIR>/users/<id>/` into their My drive's
    /// `.organizer/`. A drive whose folder is not there now (an unmounted
    /// share) is tried again at the next start.
    pub async fn move_into_drives(&self) {
        let users: Vec<Uuid> = match sqlx::query_scalar("SELECT id FROM users")
            .fetch_all(self.db.reader())
            .await
        {
            Ok(users) => users,
            Err(e) => {
                tracing::warn!("cannot list the users: {e}");
                return;
            }
        };
        for user in users {
            let old = self.user_dir(user);
            if !old.join("trash").exists() && !old.join("versions").exists() {
                continue;
            }
            let Ok((root, default)) = self.drive_root(my(user)).await else {
                continue;
            };
            if default && let Err(e) = std::fs::create_dir_all(&root) {
                tracing::warn!("cannot make {}: {e}", root.display());
                continue;
            }
            if !root.is_dir() {
                tracing::warn!(
                    "the trash and versions of user {user} wait for {} to be there",
                    root.display()
                );
                continue;
            }
            move_organizer_dir(&old, &root.join(ORGANIZER_DIR));
            tracing::info!(
                "moved the trash and versions of user {user} into {}",
                root.join(ORGANIZER_DIR).display()
            );
        }
    }
}

/// Move the `trash` and `versions` of one Organizer folder into another,
/// entry by entry (an entry already there stays where it is), and remove
/// what is left empty.
pub fn move_organizer_dir(from: &Path, to: &Path) {
    for part in ["trash", "versions"] {
        let (from, to) = (from.join(part), to.join(part));
        let Ok(entries) = std::fs::read_dir(&from) else {
            continue;
        };
        if let Err(e) = std::fs::create_dir_all(&to) {
            tracing::warn!("cannot make {}: {e}", to.display());
            continue;
        }
        for entry in entries.flatten() {
            let target = to.join(entry.file_name());
            if target.exists() {
                continue;
            }
            if let Err(e) = super::move_path(&entry.path(), &target) {
                tracing::warn!("cannot move {}: {e}", entry.path().display());
            }
        }
        let _ = std::fs::remove_dir(&from);
    }
    let _ = std::fs::remove_dir(from);
}

/// A row of a user's change feed about a drive's root: they got the drive
/// (`create`, with its name) or lost it (`delete`).
pub(crate) async fn root_row(
    tx: &mut sqlx::SqliteConnection,
    user: Uuid,
    drive: Uuid,
    root: Uuid,
    op: &str,
    version: Option<i64>,
    name: Option<&str>,
) -> sqlx::Result<()> {
    super::feed_row(
        tx,
        user,
        drive,
        root,
        op,
        version.map(|_| super::Kind::Folder),
        None,
        name,
        version,
    )
    .await
}
