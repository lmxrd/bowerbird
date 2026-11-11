//! The reconciler: walk a drive's tree in its storage (the disk's folder,
//! or a listing of its prefix in a backend) and bring the index in line
//! with it. The storage wins. What appeared is added, what changed (size or
//! modification time) is updated, what vanished is removed; a file that
//! vanished in one place and appeared in another with the same size and
//! content is a move made outside Organizer, and keeps its id.
//!
//! Called with the tree's lock held (see `Files::scan`). Hashing the files
//! that have no hash yet is slow on a large tree, so it is a pass of its
//! own, after the scan and without the lock (`hash_pending`).

use super::{Change, FileError, Kind, names, record_change};
use crate::db::Db;
use crate::storage::{Entry, Storage};
use std::collections::{HashMap, HashSet};
use std::fmt;
use std::io::Read;
use std::path::Path;
use uuid::Uuid;

/// What a scan changed in the index.
#[derive(Debug, Default, PartialEq, Eq)]
pub struct Report {
    pub created: usize,
    pub updated: usize,
    pub moved: usize,
    pub deleted: usize,
}

impl fmt::Display for Report {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            f,
            "{} created, {} updated, {} moved, {} deleted",
            self.created, self.updated, self.moved, self.deleted
        )
    }
}

/// A node of the index, as the scan needs it.
#[derive(Debug, sqlx::FromRow)]
struct IndexNode {
    id: Uuid,
    parent_id: Option<Uuid>,
    name: String,
    kind: Kind,
    size: i64,
    mtime_ns: i64,
    hash: Option<Vec<u8>>,
}

/// Bring the index of a drive's tree in line with its storage: the disk's
/// folder, or the drive's prefix in a backend (a listing: the changes made
/// outside Organizer, docs/M7C_DESIGN.md §4).
pub async fn scan(db: &Db, drive_id: Uuid, storage: &Storage) -> Result<Report, FileError> {
    let disk = match storage.walk().await {
        Ok(entries) => entries,
        // The disk's folder is not there (an unmounted share).
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
            return Err(FileError::Unavailable(e.to_string()));
        }
        Err(e) => return Err(e.into()),
    };

    let root_id = ensure_root(db, drive_id).await?;
    let index: Vec<IndexNode> = sqlx::query_as(
        "SELECT id, parent_id, name, kind, size, mtime_ns, hash FROM nodes
         WHERE drive_id = ?1 AND parent_id IS NOT NULL",
    )
    .bind(drive_id)
    .fetch_all(db.reader())
    .await?;

    // An empty root while the index has files is most likely a share that
    // is not mounted (or a bucket's prefix that changed): removing
    // everything from the index would lose every id, so nothing is done
    // until it is back.
    if disk.is_empty() && !index.is_empty() {
        return Err(FileError::Unavailable(format!(
            "the storage of drive {drive_id} is empty while the index is not: is it mounted?"
        )));
    }

    // The index by path.
    let by_id: HashMap<Uuid, &IndexNode> = index.iter().map(|n| (n.id, n)).collect();
    let mut paths: HashMap<Uuid, String> = HashMap::new();
    for node in &index {
        index_path(node.id, &by_id, root_id, &mut paths);
    }
    let by_path: HashMap<&str, &IndexNode> = index
        .iter()
        .filter_map(|n| paths.get(&n.id).map(|p| (p.as_str(), n)))
        .collect();
    let on_disk: HashMap<&str, &Entry> = disk.iter().map(|e| (e.path.as_str(), e)).collect();

    // What appeared, what vanished, what changed. A path whose kind changed
    // (a file where a folder was) has vanished and appeared both.
    let appeared: Vec<&Entry> = disk
        .iter()
        .filter(|e| !matches!(by_path.get(e.path.as_str()), Some(n) if n.kind == e.kind))
        .collect();
    let vanished: Vec<&IndexNode> = index
        .iter()
        .filter(|n| {
            let path = paths.get(&n.id).map(String::as_str).unwrap_or("");
            !matches!(on_disk.get(path), Some(e) if e.kind == n.kind)
        })
        .collect();
    let changed: Vec<(&IndexNode, &Entry)> = disk
        .iter()
        .filter_map(|e| match by_path.get(e.path.as_str()) {
            Some(n)
                if n.kind == Kind::File
                    && e.kind == Kind::File
                    && (n.size != e.size || n.mtime_ns != e.mtime_ns) =>
            {
                Some((*n, e))
            }
            _ => None,
        })
        .collect();

    // Moves made outside Organizer: a vanished file that has a hash, and an
    // appeared file of the same size and content. Only those candidates are
    // hashed now.
    let mut by_size: HashMap<i64, Vec<&IndexNode>> = HashMap::new();
    for node in &vanished {
        if node.kind == Kind::File && node.hash.is_some() {
            by_size.entry(node.size).or_default().push(node);
        }
    }
    let mut moves: Vec<(&IndexNode, &Entry)> = Vec::new();
    let mut hashes: HashMap<String, Vec<u8>> = HashMap::new();
    let mut matched: HashSet<Uuid> = HashSet::new();
    for entry in appeared.iter().filter(|e| e.kind == Kind::File) {
        let Some(candidates) = by_size.get(&entry.size) else {
            continue;
        };
        let Some(hash) = storage.hash(&entry.path).await else {
            continue;
        };
        if let Some(node) = candidates
            .iter()
            .find(|n| !matched.contains(&n.id) && n.hash.as_deref() == Some(hash.as_slice()))
        {
            matched.insert(node.id);
            moves.push((node, entry));
        }
        hashes.insert(entry.path.clone(), hash);
    }
    let moved_to: HashSet<&str> = moves.iter().map(|(_, e)| e.path.as_str()).collect();

    // A changed file is hashed again now: its old hash is wrong.
    let mut changed_hashes: HashMap<Uuid, Option<Vec<u8>>> = HashMap::new();
    for (node, entry) in &changed {
        changed_hashes.insert(node.id, storage.hash(&entry.path).await);
    }

    let mut report = Report::default();
    let mut tx = db.writer().begin().await?;
    let mut ids: HashMap<String, Uuid> = paths.iter().map(|(id, p)| (p.clone(), *id)).collect();
    ids.insert(String::new(), root_id);

    // 0. A path whose kind changed: its old node goes first, so the new one
    //    can take its name.
    for node in vanished.iter().filter(|n| {
        paths
            .get(&n.id)
            .is_some_and(|p| on_disk.contains_key(p.as_str()))
    }) {
        delete_node(&mut tx, drive_id, node).await?;
        ids.remove(paths[&node.id].as_str());
        report.deleted += 1;
    }

    // 1. New folders, parents first (the walk lists them in that order).
    for entry in appeared.iter().filter(|e| e.kind == Kind::Folder) {
        let parent = ids[entry.parent.as_str()];
        let id = insert_node(&mut tx, drive_id, parent, entry, None).await?;
        ids.insert(entry.path.clone(), id);
        report.created += 1;
    }

    // 2. Files moved outside Organizer keep their id.
    for (node, entry) in &moves {
        let parent = ids[entry.parent.as_str()];
        let version: i64 = sqlx::query_scalar(
            "UPDATE nodes SET parent_id = ?1, name = ?2, mtime_ns = ?3, portable = ?4,
               mime = ?5, version = version + 1,
               updated_at = strftime('%Y-%m-%dT%H:%M:%fZ', 'now')
             WHERE id = ?6 RETURNING version",
        )
        .bind(parent)
        .bind(&entry.name)
        .bind(entry.mtime_ns)
        .bind(names::is_portable(&entry.name))
        .bind(mime_of(&entry.name))
        .bind(node.id)
        .fetch_one(&mut *tx)
        .await?;
        record_change(
            &mut tx,
            drive_id,
            Change {
                node_id: node.id,
                op: "move",
                kind: Some(Kind::File),
                parent_id: Some(parent),
                name: Some(&entry.name),
                version: Some(version),
                from_parent: node.parent_id,
                from_path: paths.get(&node.id).map(String::as_str),
            },
        )
        .await?;
        ids.insert(entry.path.clone(), node.id);
        report.moved += 1;
    }

    // 3. New files.
    for entry in appeared
        .iter()
        .filter(|e| e.kind == Kind::File && !moved_to.contains(e.path.as_str()))
    {
        let parent = ids[entry.parent.as_str()];
        let id = insert_node(&mut tx, drive_id, parent, entry, hashes.get(&entry.path)).await?;
        ids.insert(entry.path.clone(), id);
        report.created += 1;
    }

    // 4. Files that changed.
    for (node, entry) in &changed {
        let version: i64 = sqlx::query_scalar(
            "UPDATE nodes SET size = ?1, mtime_ns = ?2, hash = ?3, version = version + 1,
               updated_at = strftime('%Y-%m-%dT%H:%M:%fZ', 'now')
             WHERE id = ?4 RETURNING version",
        )
        .bind(entry.size)
        .bind(entry.mtime_ns)
        .bind(changed_hashes.get(&node.id).cloned().flatten())
        .bind(node.id)
        .fetch_one(&mut *tx)
        .await?;
        record_change(
            &mut tx,
            drive_id,
            Change {
                node_id: node.id,
                op: "update",
                kind: Some(Kind::File),
                parent_id: node.parent_id,
                name: Some(&node.name),
                version: Some(version),
                from_parent: None,
                from_path: None,
            },
        )
        .await?;
        report.updated += 1;
    }

    // 5. What vanished and did not move. Only the topmost of a vanished
    //    subtree is deleted (its rows go with it) and listed in the feed:
    //    a client removes the folder with what is in it. Files that moved
    //    out of a vanished folder were given their new parent above.
    let gone: HashSet<Uuid> = vanished
        .iter()
        .filter(|n| !matched.contains(&n.id))
        .map(|n| n.id)
        .collect();
    for node in vanished.iter().filter(|n| {
        gone.contains(&n.id)
            && !n.parent_id.is_some_and(|p| gone.contains(&p))
            && !paths
                .get(&n.id)
                .is_some_and(|p| on_disk.contains_key(p.as_str()))
    }) {
        delete_node(&mut tx, drive_id, node).await?;
        report.deleted += 1;
    }
    tx.commit().await?;
    Ok(report)
}

/// After a drive moved to other storage: each file of the index takes the
/// time of its copy there (same path, same size), so the next scan sees
/// nothing changed.
pub async fn adopt_times(db: &Db, drive_id: Uuid, walked: &[Entry]) -> Result<(), FileError> {
    let root_id = ensure_root(db, drive_id).await?;
    let index: Vec<IndexNode> = sqlx::query_as(
        "SELECT id, parent_id, name, kind, size, mtime_ns, hash FROM nodes
         WHERE drive_id = ?1 AND parent_id IS NOT NULL",
    )
    .bind(drive_id)
    .fetch_all(db.reader())
    .await?;
    let by_id: HashMap<Uuid, &IndexNode> = index.iter().map(|n| (n.id, n)).collect();
    let mut paths: HashMap<Uuid, String> = HashMap::new();
    for node in &index {
        index_path(node.id, &by_id, root_id, &mut paths);
    }
    let by_path: HashMap<&str, &IndexNode> = index
        .iter()
        .filter_map(|n| paths.get(&n.id).map(|p| (p.as_str(), n)))
        .collect();
    let mut tx = db.writer().begin().await?;
    for entry in walked {
        if let Some(node) = by_path.get(entry.path.as_str())
            && node.kind == entry.kind
            && node.size == entry.size
        {
            sqlx::query("UPDATE nodes SET mtime_ns = ?1 WHERE id = ?2")
                .bind(entry.mtime_ns)
                .bind(node.id)
                .execute(&mut *tx)
                .await?;
        }
    }
    tx.commit().await?;
    Ok(())
}

/// Hash the files that have no hash yet, one at a time. A file that
/// changed in the meantime keeps no hash: the next scan sees it.
/// In a bucket that is a download of each file changed outside Organizer
/// (what Organizer writes is hashed as it is written).
pub async fn hash_pending(db: &Db, drive_id: Uuid, storage: &Storage) -> Result<(), FileError> {
    loop {
        let pending: Vec<(Uuid, i64, i64)> = sqlx::query_as(
            "SELECT id, size, mtime_ns FROM nodes
             WHERE drive_id = ?1 AND kind = 'file' AND hash IS NULL LIMIT 100",
        )
        .bind(drive_id)
        .fetch_all(db.reader())
        .await?;
        if pending.is_empty() {
            return Ok(());
        }
        let mut hashed = 0;
        for (id, size, mtime) in pending {
            let Ok(parts) = super::path_of(db.reader(), id).await else {
                continue;
            };
            let Some(hash) = storage.hash(&parts.join("/")).await else {
                continue;
            };
            let done = sqlx::query(
                "UPDATE nodes SET hash = ?1 WHERE id = ?2 AND size = ?3 AND mtime_ns = ?4",
            )
            .bind(hash)
            .bind(id)
            .bind(size)
            .bind(mtime)
            .execute(db.writer())
            .await?
            .rows_affected();
            hashed += done;
        }
        // Nothing could be hashed (all changed or unreadable): stop until
        // the next scan rather than loop over the same rows.
        if hashed == 0 {
            return Ok(());
        }
    }
}

/// The user's root folder node, made on the first scan.
async fn ensure_root(db: &Db, drive_id: Uuid) -> sqlx::Result<Uuid> {
    let existing: Option<Uuid> =
        sqlx::query_scalar("SELECT id FROM nodes WHERE drive_id = ?1 AND parent_id IS NULL")
            .bind(drive_id)
            .fetch_optional(db.reader())
            .await?;
    if let Some(id) = existing {
        return Ok(id);
    }
    let id = Uuid::now_v7();
    sqlx::query(
        "INSERT INTO nodes (id, drive_id, parent_id, name, kind) VALUES (?1, ?2, NULL, '', 'folder')
         ON CONFLICT DO NOTHING",
    )
    .bind(id)
    .bind(drive_id)
    .execute(db.writer())
    .await?;
    sqlx::query_scalar("SELECT id FROM nodes WHERE drive_id = ?1 AND parent_id IS NULL")
        .bind(drive_id)
        .fetch_one(db.writer())
        .await
}

async fn insert_node(
    tx: &mut sqlx::SqliteConnection,
    drive_id: Uuid,
    parent: Uuid,
    entry: &Entry,
    hash: Option<&Vec<u8>>,
) -> sqlx::Result<Uuid> {
    let id = Uuid::now_v7();
    sqlx::query(
        "INSERT INTO nodes (id, drive_id, parent_id, name, kind, size, mtime_ns, hash, mime, portable)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10)",
    )
    .bind(id)
    .bind(drive_id)
    .bind(parent)
    .bind(&entry.name)
    .bind(entry.kind.as_str())
    .bind(entry.size)
    .bind(entry.mtime_ns)
    .bind(hash)
    .bind(match entry.kind {
        Kind::File => mime_of(&entry.name),
        Kind::Folder => None,
    })
    .bind(names::is_portable(&entry.name))
    .execute(&mut *tx)
    .await?;
    record_change(
        tx,
        drive_id,
        Change {
            node_id: id,
            op: "create",
            kind: Some(entry.kind),
            parent_id: Some(parent),
            name: Some(&entry.name),
            version: Some(1),
            from_parent: None,
            from_path: None,
        },
    )
    .await?;
    Ok(id)
}

async fn delete_node(
    tx: &mut sqlx::SqliteConnection,
    drive_id: Uuid,
    node: &IndexNode,
) -> sqlx::Result<()> {
    record_change(
        &mut *tx,
        drive_id,
        Change {
            node_id: node.id,
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
        .bind(node.id)
        .execute(&mut *tx)
        .await?;
    Ok(())
}

/// The path of an indexed node, from the root (memoized in `paths`).
fn index_path(
    id: Uuid,
    by_id: &HashMap<Uuid, &IndexNode>,
    root_id: Uuid,
    paths: &mut HashMap<Uuid, String>,
) -> Option<String> {
    if let Some(path) = paths.get(&id) {
        return Some(path.clone());
    }
    let node = by_id.get(&id)?;
    let parent = node.parent_id?;
    let path = if parent == root_id {
        node.name.clone()
    } else {
        format!(
            "{}/{}",
            index_path(parent, by_id, root_id, paths)?,
            node.name
        )
    };
    paths.insert(id, path.clone());
    Some(path)
}

/// The type of a file, guessed from its name.
pub fn mime_of(name: &str) -> Option<String> {
    mime_guess::from_path(name).first().map(|m| m.to_string())
}

/// The BLAKE3 hash of a file, or None when it cannot be read.
pub async fn hash_file(path: &Path) -> Option<Vec<u8>> {
    let path = path.to_path_buf();
    tokio::task::spawn_blocking(move || {
        let mut file = std::fs::File::open(&path).ok()?;
        let mut hasher = blake3::Hasher::new();
        let mut buffer = vec![0u8; 256 * 1024];
        loop {
            let read = file.read(&mut buffer).ok()?;
            if read == 0 {
                break;
            }
            hasher.update(&buffer[..read]);
        }
        Some(hasher.finalize().as_bytes().to_vec())
    })
    .await
    .ok()
    .flatten()
}
