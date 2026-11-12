//! The journal of what Organizer is changing in a drive's storage: an
//! intent row is written before the storage is touched and deleted, with
//! the index's change, in the transaction that follows it. A row left
//! behind means the process stopped in between; at the next start,
//! `recover` settles it.
//!
//! The storage is the truth, so settling is mostly a rescan, which every
//! tree gets at start anyway. What a rescan cannot know is the trash: a file
//! that left the tree for the trash is, to a scan, only gone. (A
//! `take_out`, a file moved back to the drop box, is only gone: the rescan
//! settles it.) So a `trash` that reached the storage gets its trash row,
//! and a `restore` that did loses its one. A move on storage without
//! rename (S3: each object copied, then deleted) is finished first, and a
//! moved node goes where it was going, keeping its id.
//!
//! Paths in the tree are relative to the drive's root (`Documents/a.txt`);
//! a path outside it (an upload's temporary file) is absolute.

use super::{Change, Files, Kind, record_change};
use std::path::Path;
use std::sync::Arc;
use uuid::Uuid;

/// An intent in progress (its row id).
#[derive(Debug, Clone, Copy)]
pub struct Intent(i64);

/// Where a node goes: its new parent and name (a move).
pub type Target<'a> = Option<(Uuid, &'a str)>;

/// Write the intent, before the storage is touched.
pub async fn begin(
    files: &Files,
    drive_id: Uuid,
    op: &str,
    node_id: Option<Uuid>,
    from: Option<&str>,
    to: Option<&str>,
    target: Target<'_>,
) -> sqlx::Result<Intent> {
    let id: i64 = sqlx::query_scalar(
        "INSERT INTO journal (drive_id, op, node_id, from_path, to_path, target_parent, target_name)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7) RETURNING id",
    )
    .bind(drive_id)
    .bind(op)
    .bind(node_id)
    .bind(from)
    .bind(to)
    .bind(target.map(|(parent, _)| parent))
    .bind(target.map(|(_, name)| name))
    .fetch_one(files.db().writer())
    .await?;
    Ok(Intent(id))
}

/// The operation is done: the intent goes, in the transaction of the
/// index's change.
pub async fn end(tx: &mut sqlx::SqliteConnection, intent: Intent) -> sqlx::Result<()> {
    sqlx::query("DELETE FROM journal WHERE id = ?1")
        .bind(intent.0)
        .execute(tx)
        .await?;
    Ok(())
}

/// The storage operation failed, so nothing changed: the intent goes.
/// A failure to delete it is only logged; recovery would settle it.
pub async fn abandon(files: &Files, intent: Intent) {
    if let Err(e) = sqlx::query("DELETE FROM journal WHERE id = ?1")
        .bind(intent.0)
        .execute(files.db().writer())
        .await
    {
        tracing::warn!("cannot remove journal entry {}: {e}", intent.0);
    }
}

#[derive(sqlx::FromRow)]
struct Entry {
    id: i64,
    drive_id: Uuid,
    op: String,
    node_id: Option<Uuid>,
    from_path: Option<String>,
    to_path: Option<String>,
    target_parent: Option<Uuid>,
    target_name: Option<String>,
}

/// Settle what the journal holds after a stop in the middle of an
/// operation: at start, before any tree is scanned, and for the drives of
/// a backend whose credentials were just unlocked (`backend`). An entry of
/// a drive whose storage cannot be reached now stays for the next time.
pub async fn recover(files: &Arc<Files>, backend: Option<Uuid>) -> anyhow::Result<()> {
    let entries: Vec<Entry> = sqlx::query_as(
        "SELECT j.id, j.drive_id, j.op, j.node_id, j.from_path, j.to_path, j.target_parent,
                j.target_name
         FROM journal j JOIN drives d ON d.id = j.drive_id
         WHERE ?1 IS NULL OR d.backend_id = ?1 ORDER BY j.id",
    )
    .bind(backend)
    .fetch_all(files.db().reader())
    .await?;
    for entry in entries {
        let tree = match files.tree(entry.drive_id).await {
            Ok(tree) => tree,
            Err(e) => {
                tracing::warn!(
                    "the journal of drive {} waits for its storage: {e}",
                    entry.drive_id
                );
                continue;
            }
        };
        let _guard = tree.read_lock().await;
        let storage = &tree.storage;
        let from = entry.from_path.as_deref().unwrap_or("");
        let to = entry.to_path.as_deref().unwrap_or("");
        // A move that copies object by object, stopped part-way: finished.
        if !storage.renames()
            && matches!(entry.op.as_str(), "move" | "trash" | "restore")
            && !from.is_empty()
            && storage.exists(from).await?
        {
            storage.move_objects(from, to).await?;
            tracing::info!("finished a move the stop interrupted: {from} to {to}");
        }
        let exists = async |path: &str| -> std::io::Result<bool> {
            if path.is_empty() {
                Ok(false)
            } else if Path::new(path).is_absolute() {
                Ok(Path::new(path).exists())
            } else {
                storage.exists(path).await
            }
        };
        let moved = entry.from_path.is_some()
            && entry.to_path.is_some()
            && !exists(from).await?
            && exists(to).await?;
        // Read before the transaction: no read on the reader pool while a
        // writer transaction is open (in the tests they are one connection).
        let original_path = original_path(files, entry.drive_id, from).await;
        let (kind, size) = match (entry.op.as_str(), moved) {
            ("trash", true) if Path::new(to).is_absolute() => match std::fs::metadata(to) {
                Ok(m) if m.is_file() => (Kind::File, m.len() as i64),
                _ => (Kind::Folder, 0),
            },
            ("trash", true) => match storage.kind_of(to).await? {
                Some(Kind::File) => (Kind::File, storage.file_meta(to).await?.0),
                _ => (Kind::Folder, 0),
            },
            _ => (Kind::File, 0),
        };
        let mut tx = files.db().writer().begin().await?;
        match (entry.op.as_str(), entry.node_id, moved) {
            // In the trash in storage, but not in the trash table: put it there.
            ("trash", Some(id), true) => {
                let name = original_path
                    .rsplit('/')
                    .next()
                    .unwrap_or_default()
                    .to_string();
                sqlx::query(
                    "INSERT INTO trash (id, drive_id, name, kind, original_parent_id, original_path, size)
                     VALUES (?1, ?2, ?3, ?4, NULL, ?5, ?6) ON CONFLICT (id) DO NOTHING",
                )
                .bind(id)
                .bind(entry.drive_id)
                .bind(name)
                .bind(kind.as_str())
                .bind(&original_path)
                .bind(size)
                .execute(&mut *tx)
                .await?;
                tracing::info!("recovered a delete to the trash: {original_path}");
            }
            // Back in the tree: it is no longer in the trash.
            ("restore", Some(id), true) => {
                sqlx::query("DELETE FROM trash WHERE id = ?1")
                    .bind(id)
                    .execute(&mut *tx)
                    .await?;
                tracing::info!("recovered a restore from the trash");
            }
            // Moved in storage: the node goes there too, with its id, when
            // it and its new folder are still in the index.
            ("move", Some(id), true) => {
                if let (Some(parent), Some(name)) = (entry.target_parent, &entry.target_name) {
                    settle_move(&mut tx, entry.drive_id, id, parent, name).await?;
                }
            }
            // An upload that never reached the tree: its temporary file
            // goes. One that did is found by the rescan.
            ("write", _, false) if Path::new(from).is_absolute() => {
                let _ = std::fs::remove_file(from);
            }
            // Anything else: the rescan at start settles the index.
            _ => {}
        }
        sqlx::query("DELETE FROM journal WHERE id = ?1")
            .bind(entry.id)
            .execute(&mut *tx)
            .await?;
        tx.commit().await?;
    }
    Ok(())
}

/// A node moved in storage before the index knew: it goes to its new
/// parent under its new name, when both are still there.
async fn settle_move(
    tx: &mut sqlx::SqliteConnection,
    drive_id: Uuid,
    id: Uuid,
    parent: Uuid,
    name: &str,
) -> sqlx::Result<()> {
    let from_parent: Option<Option<Uuid>> =
        sqlx::query_scalar("SELECT parent_id FROM nodes WHERE id = ?1 AND drive_id = ?2")
            .bind(id)
            .bind(drive_id)
            .fetch_optional(&mut *tx)
            .await?;
    let parent_there: Option<i64> =
        sqlx::query_scalar("SELECT 1 FROM nodes WHERE id = ?1 AND drive_id = ?2")
            .bind(parent)
            .bind(drive_id)
            .fetch_optional(&mut *tx)
            .await?;
    let (Some(from_parent), Some(_)) = (from_parent, parent_there) else {
        return Ok(());
    };
    let (version, kind): (i64, Kind) = sqlx::query_as(
        "UPDATE nodes SET parent_id = ?1, name = ?2, version = version + 1,
           updated_at = strftime('%Y-%m-%dT%H:%M:%fZ', 'now')
         WHERE id = ?3 RETURNING version, kind",
    )
    .bind(parent)
    .bind(name)
    .bind(id)
    .fetch_one(&mut *tx)
    .await?;
    record_change(
        tx,
        drive_id,
        Change {
            node_id: id,
            op: "move",
            kind: Some(kind),
            parent_id: Some(parent),
            name: Some(name),
            version: Some(version),
            from_parent,
            from_path: None,
        },
    )
    .await?;
    tracing::info!("recovered a move: node {id}");
    Ok(())
}

/// A path relative to the drive's root, for the trash's "where it was"; a
/// row written before drives had storage holds an absolute one.
async fn original_path(files: &Files, drive_id: Uuid, path: &str) -> String {
    if !Path::new(path).is_absolute() {
        return path.to_string();
    }
    let Ok((root, _)) = files.drive_root(drive_id).await else {
        return path.to_string();
    };
    Path::new(path)
        .strip_prefix(&root)
        .unwrap_or(Path::new(path))
        .to_string_lossy()
        .into_owned()
}
