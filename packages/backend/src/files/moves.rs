//! Moving a drive to another storage backend (docs/M7C_DESIGN.md §2): a job
//! that copies everything of the drive (its tree, trash and versions) to
//! the new backend, then switches over. The drive is read-only meanwhile
//! (`Tree::write_lock` refuses with [`super::READ_ONLY`]); reads go on from
//! where it was. The `drive_moves` row is the job: one a stop left is
//! carried on at the next start, the copy skipping what is there already.
//! A failed try is noted on the row and tried again a few minutes later.
//!
//! After the switch the old copy goes: the drive's folder in the data dir,
//! or its prefix in a bucket. A folder on the server the superadmin chose
//! for the drive keeps its files (only Organizer's own folder goes): it was
//! there before the drive.

use super::drives::{DriveKind, DriveRow};
use super::{FileError, Files, ORGANIZER_DIR};
use crate::storage::Storage;
use std::sync::Arc;
use std::sync::atomic::Ordering;
use std::time::Duration;
use uuid::Uuid;

/// How long a failed try waits before the next.
const RETRY: Duration = Duration::from_secs(5 * 60);

impl Files {
    /// Start moving a shared drive to another backend (None: the server's
    /// disk). Both must answer now; the copy runs in the background.
    pub async fn move_drive(
        self: &Arc<Self>,
        drive: Uuid,
        to: Option<Uuid>,
    ) -> Result<(), FileError> {
        let row = DriveRow::get(self.db.reader(), drive)
            .await?
            .filter(|d| d.kind == DriveKind::Shared)
            .ok_or(FileError::NotFound)?;
        if row.backend_id == to {
            return Err(FileError::Invalid(
                "The drive is stored there already".into(),
            ));
        }
        let tree = self.tree(drive).await?;
        let target = self.storage_in(to, drive).await?;
        target.check().await?;
        {
            let _guard = tree.write_lock().await?;
            let started = sqlx::query(
                "INSERT INTO drive_moves (drive_id, from_backend, to_backend) VALUES (?1, ?2, ?3)",
            )
            .bind(drive)
            .bind(row.backend_id)
            .bind(to)
            .execute(self.db.writer())
            .await;
            match started {
                Ok(_) => {}
                Err(sqlx::Error::Database(e)) if e.is_unique_violation() => {
                    return Err(FileError::ReadOnly);
                }
                Err(e) => return Err(e.into()),
            }
            tree.moving.store(true, Ordering::SeqCst);
        }
        self.notify_drive(drive).await;
        let files = self.clone();
        tokio::spawn(async move { files.run_move(drive).await });
        Ok(())
    }

    /// At start: the moves a stop interrupted carry on.
    pub(crate) async fn resume_moves(self: &Arc<Self>) -> anyhow::Result<()> {
        let drives: Vec<Uuid> = sqlx::query_scalar("SELECT drive_id FROM drive_moves")
            .fetch_all(self.db.reader())
            .await?;
        for drive in drives {
            let files = self.clone();
            tokio::spawn(async move { files.run_move(drive).await });
        }
        Ok(())
    }

    /// Carry a move on until it is done (or its row is gone: the drive
    /// was deleted).
    async fn run_move(self: Arc<Self>, drive: Uuid) {
        loop {
            match self.move_once(drive).await {
                Ok(()) => return,
                Err(e) => {
                    tracing::warn!(
                        "moving drive {drive} to other storage failed, tried again later: {e}"
                    );
                    let reason = match &e {
                        FileError::Down(reason) => reason.clone(),
                        _ => "The copy failed; it is tried again in a few minutes".into(),
                    };
                    let noted =
                        sqlx::query("UPDATE drive_moves SET error = ?2 WHERE drive_id = ?1")
                            .bind(drive)
                            .bind(reason)
                            .execute(self.db.writer())
                            .await;
                    if let Err(e) = noted {
                        tracing::warn!("cannot note why a move failed: {e}");
                    }
                }
            }
            tokio::time::sleep(RETRY).await;
        }
    }

    /// One try: copy, switch over, then remove the old copy.
    pub async fn move_once(self: &Arc<Self>, drive: Uuid) -> Result<(), FileError> {
        let job: Option<(Option<Uuid>, Option<Uuid>)> =
            sqlx::query_as("SELECT from_backend, to_backend FROM drive_moves WHERE drive_id = ?1")
                .bind(drive)
                .fetch_optional(self.db.reader())
                .await?;
        let Some((from, to)) = job else {
            return Ok(());
        };
        let row = DriveRow::get(self.db.reader(), drive)
            .await?
            .ok_or(FileError::NotFound)?;
        if row.backend_id != to {
            let tree = self.tree(drive).await?;
            let target = self.storage_in(to, drive).await?;
            // The copy runs without the lock: the drive is read-only, so
            // only its own trash and versions could change, which the copy
            // under the lock below takes too.
            tree.storage.copy_all_to(&target).await?;
            {
                let _guard = tree.read_lock().await;
                tree.storage.copy_all_to(&target).await?;
                let walked = target.walk().await?;
                sqlx::query("UPDATE drives SET backend_id = ?2, root = NULL WHERE id = ?1")
                    .bind(drive)
                    .bind(to)
                    .execute(self.db.writer())
                    .await?;
                // The same files, with the times of their new storage: the
                // next listing sees nothing changed.
                super::scan::adopt_times(&self.db, drive, &walked).await?;
            }
            self.forget(drive).await;
            tracing::info!("drive {drive} is in its new storage now");
        }
        // Switched over: the old copy goes.
        let removed = match from {
            Some(_) => self.storage_in(from, drive).await?.remove_drive().await,
            None => match &row.root {
                // A folder the superadmin chose was there before the drive.
                Some(root) => {
                    match std::fs::remove_dir_all(std::path::Path::new(root).join(ORGANIZER_DIR)) {
                        Err(e) if e.kind() != std::io::ErrorKind::NotFound => Err(e),
                        _ => Ok(()),
                    }
                }
                None => {
                    Storage::disk(&self.shared_default_root(drive))?
                        .remove_drive()
                        .await
                }
            },
        };
        if let Err(e) = removed {
            tracing::warn!("the old copy of drive {drive} is left where it was: {e}");
        }
        sqlx::query("DELETE FROM drive_moves WHERE drive_id = ?1")
            .bind(drive)
            .execute(self.db.writer())
            .await?;
        self.notify_drive(drive).await;
        let files = self.clone();
        tokio::spawn(async move { files.open_and_scan(drive).await });
        Ok(())
    }

    /// A drive's storage in a backend (None: its default folder on the
    /// server's disk), whether or not it is there now.
    pub(crate) async fn storage_in(
        &self,
        backend: Option<Uuid>,
        drive: Uuid,
    ) -> Result<Storage, FileError> {
        match backend {
            None => {
                let root = self.shared_default_root(drive);
                std::fs::create_dir_all(&root)?;
                Ok(Storage::disk(&root)?)
            }
            Some(backend) => {
                let row = crate::backends::BackendRow::get(self.db.reader(), backend)
                    .await?
                    .ok_or(FileError::NotFound)?;
                self.backends.storage(&row, drive).await
            }
        }
    }

    /// Tell a drive's members' apps that it changed (where it is, whether
    /// it is read-only).
    pub(crate) async fn notify_drive(&self, drive: Uuid) {
        let members = match self.db.reader().acquire().await {
            Ok(mut conn) => super::drives::users_of(&mut conn, drive).await,
            Err(e) => Err(e),
        };
        match members {
            Ok(members) => {
                for (member, _) in members {
                    self.announce(member, super::Topic::Changes);
                }
            }
            Err(e) => tracing::warn!("cannot list the users of drive {drive}: {e}"),
        }
    }
}
