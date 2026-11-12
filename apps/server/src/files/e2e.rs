//! End-to-end-encrypted folders (docs/M6E_DESIGN.md, docs/E2E_SPEC.md):
//! what the server keeps of them and the rules it can check. It never has
//! a key: the names and contents under an encrypted folder are opaque
//! bytes to it, and its keys are wrapped for each member by their devices.
//!
//! A node's `e2e_root` is the top encrypted folder it is in (its own id
//! for that folder); set on insert from the parent (a trigger, so the
//! reconciler and every operation agree). Nothing moves across the edge
//! of an encrypted folder: names inside are sealed with their parent's
//! id, so a move inside one goes with a new name from the client, and a
//! trashed encrypted node keeps its ids for its restore (`keep_for_trash`,
//! `restore_kept`).
//!
//! Its **members** are its owner and everyone a grant gives access to it
//! (on it, a folder above it, or its drive), who see it. A member who loses access loses their
//! wrapped keys and the folder `needs_new_key` (`settle`): the next device
//! of a member left makes a new generation, which new writes use.
//!
//! A **mirror** copies an encrypted folder as it is (sealed with the
//! drive's ids) and keeps its user's own wrapped keys of it, every
//! generation they had (`keep_for_mirrors`, `kept_keys`): the copy still
//! opens once the drive is deleted or the user lost access.

use super::{Change, FileError, Files, Kind, Node, record_change};
use uuid::Uuid;

/// Refused unless a move stays on its side of an encrypted folder's edge:
/// an encrypted folder's top goes anywhere but into another one; what is
/// in one stays in it; nothing comes in from outside.
pub(crate) fn check_move(node: &Node, parent: &Node) -> Result<(), FileError> {
    let ok = match node.e2e_root {
        Some(root) if root == node.id => parent.e2e_root.is_none(),
        Some(root) => parent.e2e_root == Some(root),
        None => parent.e2e_root.is_none(),
    };
    if ok {
        return Ok(());
    }
    Err(FileError::Invalid(match node.e2e_root {
        Some(root) if root == node.id => {
            "An encrypted folder cannot go inside another one".into()
        }
        Some(_) => "What is in an encrypted folder stays in it: download it to take it out".into(),
        None => "Only what is uploaded into an encrypted folder is encrypted: upload it there instead of moving it".into(),
    }))
}

/// Refused for a node in an encrypted folder: what the server would need
/// to read it for (`what`, fit to show after "Not available for encrypted
/// folders: ").
pub fn refuse_encrypted(node: &Node, what: &str) -> Result<(), FileError> {
    if node.e2e_root.is_some() {
        return Err(FileError::Forbidden(format!(
            "Not available for encrypted folders: {what}"
        )));
    }
    Ok(())
}

/// The members of an encrypted folder: the users of its drive first (My
/// drive's user, or a shared drive's), then everyone a grant gives access
/// to it; only those who see it (limited access hides it from some).
pub async fn members(db: &crate::db::Db, root: Uuid) -> Result<Vec<Uuid>, FileError> {
    let drive: Uuid = sqlx::query_scalar("SELECT drive_id FROM nodes WHERE id = ?1")
        .bind(root)
        .fetch_optional(db.reader())
        .await?
        .ok_or(FileError::NotFound)?;
    let mut conn = db.reader().acquire().await?;
    let mut candidates: Vec<Uuid> = sqlx::query_scalar(
        "SELECT user_id FROM drives WHERE id = ?1 AND user_id IS NOT NULL
         UNION ALL
         SELECT * FROM (SELECT DISTINCT user_id FROM drive_users WHERE drive_id = ?1
                        ORDER BY user_id)",
    )
    .bind(drive)
    .fetch_all(&mut *conn)
    .await?;
    let granted: Vec<Uuid> = sqlx::query_scalar(
        "WITH RECURSIVE up (id, parent_id) AS (
           SELECT id, parent_id FROM nodes WHERE id = ?1
           UNION ALL
           SELECT n.id, n.parent_id FROM nodes n JOIN up ON n.id = up.parent_id
         )
         SELECT g.user_id FROM grants g JOIN up ON g.node_id = up.id
         WHERE g.user_id IS NOT NULL
         UNION
         SELECT m.user_id FROM grants g JOIN up ON g.node_id = up.id
           JOIN team_members m ON m.team_id = g.team_id
         ORDER BY 1",
    )
    .bind(root)
    .fetch_all(&mut *conn)
    .await?;
    candidates.extend(granted);
    let mut members = Vec::new();
    for user in candidates {
        if !members.contains(&user) && super::roles::sees(&mut conn, user, root).await? {
            members.push(user);
        }
    }
    Ok(members)
}

impl Files {
    /// Drop the wrapped keys of whoever is no longer a member of an
    /// encrypted folder, and mark it for a new key: after any change of
    /// shares, groups or their members (`settle_mounts`).
    pub async fn settle_e2e(&self) -> Result<(), FileError> {
        let roots: Vec<Uuid> = sqlx::query_scalar(
            "SELECT root_id FROM e2e_folders WHERE root_id IN (SELECT id FROM nodes)",
        )
        .fetch_all(self.db.reader())
        .await?;
        for root in roots {
            // Members on other servers (`organizer-e2e`): a share that no
            // longer reaches the folder takes its keys with it.
            let remote: Vec<Uuid> = crate::federation::e2e::remote_members(self, root)
                .await?
                .into_iter()
                .map(|m| m.share_id)
                .collect();
            let remote_holders: Vec<Uuid> = sqlx::query_scalar(
                "SELECT DISTINCT share_id FROM e2e_remote_keys WHERE root_id = ?1",
            )
            .bind(root)
            .fetch_all(self.db.reader())
            .await?;
            for share in remote_holders.into_iter().filter(|s| !remote.contains(s)) {
                crate::federation::e2e::remote_gone_now(self, share).await?;
            }
            let members = members(&self.db, root).await?;
            let holders: Vec<Uuid> = sqlx::query_scalar(
                "SELECT DISTINCT user_id FROM e2e_folder_keys WHERE root_id = ?1",
            )
            .bind(root)
            .fetch_all(self.db.reader())
            .await?;
            let gone: Vec<Uuid> = holders
                .into_iter()
                .filter(|h| !members.contains(h))
                .collect();
            if gone.is_empty() {
                continue;
            }
            let mut tx = self.db.writer().begin().await?;
            for user in &gone {
                sqlx::query("DELETE FROM e2e_folder_keys WHERE root_id = ?1 AND user_id = ?2")
                    .bind(root)
                    .bind(user)
                    .execute(&mut *tx)
                    .await?;
            }
            sqlx::query("UPDATE e2e_folders SET needs_new_key = 1 WHERE root_id = ?1")
                .bind(root)
                .execute(&mut *tx)
                .await?;
            tx.commit().await?;
        }
        Ok(())
    }
}

/// Forget the keys of encrypted folders that are gone for good: neither
/// in a tree nor in a trash (a mirror's copies too). After a purge and the
/// hourly sweep.
pub async fn forget_gone(db: &crate::db::Db) -> sqlx::Result<()> {
    sqlx::query(
        "DELETE FROM e2e_folders WHERE root_id NOT IN (SELECT id FROM nodes)
           AND root_id NOT IN (SELECT id FROM trash)
           AND root_id NOT IN (SELECT id FROM e2e_trash_nodes)",
    )
    .execute(db.writer())
    .await?;
    sqlx::query(
        "DELETE FROM e2e_copy_keys WHERE folder_id NOT IN (SELECT id FROM nodes)
           AND folder_id NOT IN (SELECT id FROM trash)
           AND folder_id NOT IN (SELECT id FROM e2e_trash_nodes)",
    )
    .execute(db.writer())
    .await?;
    Ok(())
}

/// Keep, in the mirrors, their users' wrapped keys of the encrypted
/// folders they copied (`e2e_copy_keys`), so that a copy still opens once
/// the drive is deleted or its user lost access (and with it their rows of
/// `e2e_folder_keys`). Only the mirror's own user's wrappings, never
/// anyone else's; a generation kept is never changed. `mirror`: one
/// mirror's copies (after each of its copies); `root`: one folder of a
/// drive, in every mirror of it (after keys are posted for it).
pub async fn keep_for_mirrors(
    db: &crate::db::Db,
    mirror: Option<Uuid>,
    root: Option<Uuid>,
) -> sqlx::Result<()> {
    sqlx::query(
        "INSERT INTO e2e_copy_keys (folder_id, drive_id, source_id, generation, user_id, wrapped)
         SELECT c.id, c.drive_id, k.root_id, k.generation, k.user_id, k.wrapped
         FROM mirrors m
           JOIN nodes c ON c.drive_id = m.id AND c.source_id IS NOT NULL AND c.e2e_root = c.id
           JOIN e2e_folder_keys k ON k.root_id = c.source_id AND k.user_id = m.user_id
         WHERE (?1 IS NULL OR m.id = ?1) AND (?2 IS NULL OR k.root_id = ?2)
         ON CONFLICT DO NOTHING",
    )
    .bind(mirror)
    .bind(root)
    .execute(db.writer())
    .await?;
    Ok(())
}

/// The wrapped keys a mirror (or a drive ejected from one) kept for `user`
/// of the encrypted folder that `id` is in, or is a copy of, in one of
/// the drives they have: `id` a node of the copy, or of the drive's
/// folder it copies (gone or not). Answers the drive's folder (the id the
/// keys are sealed with) and the keys, every generation kept; None when
/// nothing is kept.
pub async fn kept_keys(
    db: &crate::db::Db,
    user: Uuid,
    id: Uuid,
) -> sqlx::Result<Option<(Uuid, Vec<(i64, String)>)>> {
    let mut conn = db.reader().acquire().await?;
    let source: Option<Uuid> = sqlx::query_scalar(
        "WITH mine (drive_id) AS (SELECT DISTINCT drive_id FROM drive_users WHERE user_id = ?2),
              tops (id) AS (
                SELECT ?1
                UNION
                SELECT n.e2e_root FROM nodes n
                WHERE n.drive_id IN mine AND (n.id = ?1 OR n.source_id = ?1)
                  AND n.e2e_root IS NOT NULL
              )
         SELECT k.source_id FROM e2e_copy_keys k
         WHERE k.user_id = ?2 AND k.drive_id IN mine
           AND (k.folder_id IN tops OR k.source_id IN tops)
         LIMIT 1",
    )
    .bind(id)
    .bind(user)
    .fetch_optional(&mut *conn)
    .await?;
    let Some(source) = source else {
        return Ok(None);
    };
    let keys: Vec<(i64, String)> = sqlx::query_as(
        "SELECT generation, min(wrapped) FROM e2e_copy_keys
         WHERE user_id = ?1 AND source_id = ?2
           AND drive_id IN (SELECT drive_id FROM drive_users WHERE user_id = ?1)
         GROUP BY generation ORDER BY generation",
    )
    .bind(user)
    .bind(source)
    .fetch_all(&mut *conn)
    .await?;
    Ok(Some((source, keys)))
}

/// When a node at or above encrypted ones goes to the trash (its `trash`
/// row written, its node rows not yet deleted): every node under it, with
/// its id, path below it and encrypted folder, for the restore.
pub(crate) async fn keep_for_trash(
    tx: &mut sqlx::SqliteConnection,
    trash_id: Uuid,
) -> sqlx::Result<()> {
    const DOWN: &str = "WITH RECURSIVE down (id, rel_path, kind, e2e_root) AS (
           SELECT id, '', kind, e2e_root FROM nodes WHERE id = ?1
           UNION ALL
           SELECT n.id, CASE down.rel_path WHEN '' THEN n.name
                          ELSE down.rel_path || '/' || n.name END,
                  n.kind, n.e2e_root
           FROM nodes n JOIN down ON n.parent_id = down.id
         )";
    let any: Option<i64> = sqlx::query_scalar(sqlx::AssertSqlSafe(format!(
        "{DOWN} SELECT 1 FROM down WHERE e2e_root IS NOT NULL LIMIT 1"
    )))
    .bind(trash_id)
    .fetch_optional(&mut *tx)
    .await?;
    if any.is_none() {
        return Ok(());
    }
    sqlx::query(sqlx::AssertSqlSafe(format!(
        "{DOWN} INSERT INTO e2e_trash_nodes (trash_id, id, rel_path, kind, e2e_root)
         SELECT ?1, id, rel_path, kind, e2e_root FROM down WHERE rel_path != ''"
    )))
    .bind(trash_id)
    .execute(&mut *tx)
    .await?;
    Ok(())
}

/// A node kept under a trashed one.
#[derive(sqlx::FromRow)]
pub(crate) struct Kept {
    id: Uuid,
    rel_path: String,
    kind: Kind,
    e2e_root: Option<Uuid>,
    /// Its size and time in storage once restored; None: not there.
    #[sqlx(skip)]
    found: Option<(i64, i64)>,
}

/// The kept nodes with what the storage has of each under `at` (where the
/// restored node is now), looked at before the restore's transaction.
pub(crate) async fn with_storage_meta(
    storage: &crate::storage::Storage,
    at: &str,
    mut kept: Vec<Kept>,
) -> Result<Vec<Kept>, FileError> {
    for node in &mut kept {
        let path = crate::storage::join(at, &node.rel_path);
        node.found = match storage.kind_of(&path).await? {
            Some(Kind::File) => Some(storage.file_meta(&path).await?),
            Some(Kind::Folder) => Some((0, storage.file_meta(&path).await.map_or(0, |(_, m)| m))),
            None => None,
        };
    }
    Ok(kept)
}

/// The nodes kept under a trashed one (read before its trash row goes).
pub(crate) async fn kept_under(
    tx: &mut sqlx::SqliteConnection,
    top: Uuid,
) -> sqlx::Result<Vec<Kept>> {
    sqlx::query_as("SELECT id, rel_path, kind, e2e_root FROM e2e_trash_nodes WHERE trash_id = ?1")
        .bind(top)
        .fetch_all(&mut *tx)
        .await
}

/// After a restore put the trashed node back (at `top`): the nodes kept
/// under it go back with their ids, parents first, each with its change,
/// before the reconciler looks (it then finds them indexed). What is no
/// longer in storage ([`with_storage_meta`]) is skipped; the reconciler
/// settles the rest.
pub(crate) async fn restore_kept(
    tx: &mut sqlx::SqliteConnection,
    user_id: Uuid,
    top: Uuid,
    mut kept: Vec<Kept>,
) -> Result<(), FileError> {
    kept.sort_by_key(|k| k.rel_path.matches('/').count());
    let mut ids: std::collections::HashMap<String, Uuid> = Default::default();
    ids.insert(String::new(), top);
    for node in kept {
        let (parent_path, name) = match node.rel_path.rsplit_once('/') {
            Some((parent, name)) => (parent.to_string(), name.to_string()),
            None => (String::new(), node.rel_path.clone()),
        };
        let Some(parent) = ids.get(&parent_path).copied() else {
            continue;
        };
        let Some((size, mtime)) = node.found else {
            continue;
        };
        let size = if node.kind == Kind::File { size } else { 0 };
        sqlx::query(
            "INSERT INTO nodes (id, drive_id, parent_id, name, kind, size, mtime_ns, mime, portable, e2e_root)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10)",
        )
        .bind(node.id)
        .bind(user_id)
        .bind(parent)
        .bind(&name)
        .bind(node.kind.as_str())
        .bind(size)
        .bind(mtime)
        .bind(match (node.kind, node.e2e_root) {
            (Kind::File, None) => super::scan::mime_of(&name),
            _ => None,
        })
        .bind(super::names::is_portable(&name))
        .bind(node.e2e_root)
        .execute(&mut *tx)
        .await?;
        record_change(
            &mut *tx,
            user_id,
            Change {
                node_id: node.id,
                op: "create",
                kind: Some(node.kind),
                parent_id: Some(parent),
                name: Some(&name),
                version: Some(1),
                from_parent: None,
                from_path: None,
            },
        )
        .await?;
        ids.insert(node.rel_path, node.id);
    }
    Ok(())
}
