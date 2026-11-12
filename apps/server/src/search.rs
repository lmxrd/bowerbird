//! Full-text search over each user's tree: names, folder paths, and the text
//! read from files (docs/M2_DESIGN.md §6, `GET /search`), in SQLite's FTS5.
//!
//! The index follows the tree: after the tree changes (the `changes` live
//! event, a second of quiet first), [`refresh`] compares `search_docs` with
//! the nodes and updates the rows that differ. Then the text of new or
//! changed files is read, a few at a time, when the user's setting allows
//! reading contents (`always` or `default_on`) and no sensitive-file rule
//! says otherwise. Nothing of this leaves the server: it is not sent to the
//! model (embeddings, later, will say so in the settings' description).

use crate::drop_box::extract::{self, NoText};
use crate::drop_box::guard::{self, Subject};
use crate::files::Topic;
use crate::routes::drop_box::{ReadContents, rules_of, settings_of};
use crate::state::AppState;
use std::collections::{HashMap, HashSet};
use std::time::Duration;
use uuid::Uuid;

/// The most text indexed of a file, in characters.
pub const MAX_CHARS: usize = 100_000;
/// How many files' text is read in one round, before the tree is looked at
/// again.
const READ_PER_ROUND: i64 = 50;
/// The pause between two files read in the background.
const READ_PAUSE: Duration = Duration::from_millis(20);
/// How long the tree must be quiet before the index follows it.
const QUIET: Duration = Duration::from_secs(1);

/// Keep every drive's index in step with its tree, until the process ends.
/// The events name a user (their drives) or a drive.
pub async fn run(state: AppState) {
    let mut events = state.files.subscribe();
    // Every drive at start: the trees may have changed while the server was
    // down.
    let mut dirty: HashSet<Uuid> = all_drives(&state).await;
    loop {
        if dirty.is_empty() {
            match events.recv().await {
                Ok((user, Topic::Changes | Topic::Reindex)) => {
                    dirty.insert(user);
                }
                Ok(_) => continue,
                Err(tokio::sync::broadcast::error::RecvError::Lagged(_)) => {
                    dirty = all_drives(&state).await;
                }
                Err(tokio::sync::broadcast::error::RecvError::Closed) => return,
            }
        }
        // Until the tree is quiet.
        let quiet = tokio::time::sleep(QUIET);
        tokio::pin!(quiet);
        loop {
            tokio::select! {
                event = events.recv() => match event {
                    Ok((user, Topic::Changes | Topic::Reindex)) => { dirty.insert(user); }
                    Ok(_) => {}
                    Err(tokio::sync::broadcast::error::RecvError::Lagged(_)) => {
                        dirty = all_drives(&state).await;
                    }
                    Err(tokio::sync::broadcast::error::RecvError::Closed) => return,
                },
                _ = &mut quiet => break,
            }
        }
        let mut drives = HashSet::new();
        for id in std::mem::take(&mut dirty) {
            drives.extend(drives_named(&state, id).await);
        }
        for drive in drives {
            match refresh(&state, drive, READ_PAUSE).await {
                // Files left to read: another round.
                Ok(true) => {
                    dirty.insert(drive);
                }
                Ok(false) => {}
                Err(e) => tracing::warn!("cannot update the search index of drive {drive}: {e}"),
            }
            // Then the embeddings, from what the index holds now, of My
            // drives (the embeddings are their users'). An endpoint that
            // fails is tried again at the next change.
            let Some(user) = user_of(&state, drive).await else {
                continue;
            };
            match crate::embed::sync(&state, user).await {
                Ok(true) => {
                    dirty.insert(drive);
                }
                Ok(false) => {}
                Err(e) => tracing::warn!("cannot update the embeddings of {user}: {e}"),
            }
        }
    }
}

/// Every drive but the mirrors: a mirror is a copy of a drive the user
/// searches already (docs/M7D_DESIGN.md), and is not searched.
async fn all_drives(state: &AppState) -> HashSet<Uuid> {
    let remote = crate::federation::incoming::read_remote(&state.files).await;
    sqlx::query_scalar("SELECT id FROM drives WHERE kind != 'mirror' AND (?1 OR kind != 'remote')")
        .bind(remote)
        .fetch_all(state.db.reader())
        .await
        .unwrap_or_default()
        .into_iter()
        .collect()
}

/// The drives an event's id stands for: a drive, or a user's drives (not
/// mirrors).
async fn drives_named(state: &AppState, id: Uuid) -> Vec<Uuid> {
    let remote = crate::federation::incoming::read_remote(&state.files).await;
    sqlx::query_scalar(
        "SELECT id FROM drives WHERE kind != 'mirror' AND (?2 OR kind != 'remote')
           AND id IN (SELECT ?1 UNION SELECT drive_id FROM drive_users WHERE user_id = ?1)",
    )
    .bind(id)
    .bind(remote)
    .fetch_all(state.db.reader())
    .await
    .unwrap_or_default()
}

/// My drive's user; None for a shared drive.
async fn user_of(state: &AppState, drive: Uuid) -> Option<Uuid> {
    sqlx::query_scalar("SELECT user_id FROM drives WHERE id = ?1")
        .bind(drive)
        .fetch_optional(state.db.reader())
        .await
        .ok()
        .flatten()
        .flatten()
}

/// The ids of the drives a user searches: theirs, and the shared drives
/// they are a member of, as `?2` in a query.
const USER_DRIVES: &str = "(SELECT drive_id FROM drive_users WHERE user_id = ?2)";

#[derive(sqlx::FromRow)]
struct NodeRow {
    id: Uuid,
    parent_id: Option<Uuid>,
    name: String,
    kind: String,
}

#[derive(sqlx::FromRow)]
struct DocRow {
    rowid: i64,
    node_id: Uuid,
    kind: String,
    name: String,
    path: String,
}

/// Bring a drive's index up to date with its tree, then read the text of
/// a round of files. Answers whether files are left to read.
pub async fn refresh(state: &AppState, drive: Uuid, pause: Duration) -> anyhow::Result<bool> {
    sync_tree(state, drive).await?;
    read_texts(state, drive, pause).await
}

/// The rows of the index made to match the nodes: new ones added, renamed
/// or moved ones changed (their text kept), gone ones removed.
async fn sync_tree(state: &AppState, user: Uuid) -> Result<(), sqlx::Error> {
    // Read before the transaction (one connection in the tests).
    let nodes: Vec<NodeRow> =
        // Encrypted folders are not indexed at all, not even their names.
        sqlx::query_as(
            "SELECT id, parent_id, name, kind FROM nodes WHERE drive_id = ?1 AND e2e_root IS NULL",
        )
            .bind(user)
            .fetch_all(state.db.reader())
            .await?;
    let docs: Vec<DocRow> = sqlx::query_as(
        "SELECT rowid, node_id, kind, name, path FROM search_docs WHERE drive_id = ?1",
    )
    .bind(user)
    .fetch_all(state.db.reader())
    .await?;
    let by_id: HashMap<Uuid, &NodeRow> = nodes.iter().map(|n| (n.id, n)).collect();
    let mut paths: HashMap<Uuid, String> = HashMap::new();
    let mut docs: HashMap<Uuid, DocRow> = docs.into_iter().map(|d| (d.node_id, d)).collect();

    let mut tx = state.db.writer().begin().await?;
    for node in &nodes {
        // The root is not a result.
        let Some(parent) = node.parent_id else {
            continue;
        };
        let path = folder_path(parent, &by_id, &mut paths);
        match docs.remove(&node.id) {
            None => {
                let rowid: i64 = sqlx::query_scalar(
                    "INSERT INTO search_docs (node_id, drive_id, kind, name, path)
                     VALUES (?1, ?2, ?3, ?4, ?5) RETURNING rowid",
                )
                .bind(node.id)
                .bind(user)
                .bind(&node.kind)
                .bind(&node.name)
                .bind(&path)
                .fetch_one(&mut *tx)
                .await?;
                sqlx::query("INSERT INTO search (rowid, name, path, text) VALUES (?1, ?2, ?3, '')")
                    .bind(rowid)
                    .bind(&node.name)
                    .bind(&path)
                    .execute(&mut *tx)
                    .await?;
            }
            Some(doc) if doc.name != node.name || doc.path != path || doc.kind != node.kind => {
                sqlx::query(
                    "UPDATE search_docs SET kind = ?2, name = ?3, path = ?4 WHERE rowid = ?1",
                )
                .bind(doc.rowid)
                .bind(&node.kind)
                .bind(&node.name)
                .bind(&path)
                .execute(&mut *tx)
                .await?;
                sqlx::query("UPDATE search SET name = ?2, path = ?3 WHERE rowid = ?1")
                    .bind(doc.rowid)
                    .bind(&node.name)
                    .bind(&path)
                    .execute(&mut *tx)
                    .await?;
            }
            Some(_) => {}
        }
    }
    // What is left is gone from the tree.
    for doc in docs.values() {
        sqlx::query("DELETE FROM search WHERE rowid = ?1")
            .bind(doc.rowid)
            .execute(&mut *tx)
            .await?;
        sqlx::query("DELETE FROM search_docs WHERE rowid = ?1")
            .bind(doc.rowid)
            .execute(&mut *tx)
            .await?;
    }
    tx.commit().await
}

/// The path of a folder from the root ('' for the root), remembered.
fn folder_path(
    id: Uuid,
    nodes: &HashMap<Uuid, &NodeRow>,
    paths: &mut HashMap<Uuid, String>,
) -> String {
    if let Some(path) = paths.get(&id) {
        return path.clone();
    }
    let mut parts = Vec::new();
    let mut at = id;
    while let Some(node) = nodes.get(&at) {
        let Some(parent) = node.parent_id else {
            break;
        };
        parts.push(node.name.as_str());
        if parts.len() > 256 {
            break;
        }
        at = parent;
    }
    parts.reverse();
    let path = parts.join("/");
    paths.insert(id, path.clone());
    path
}

/// Whether xberg can find text in a file of this type.
pub fn readable(mime: &str) -> bool {
    mime.starts_with("text/")
        || matches!(
            mime,
            "application/pdf"
                | "application/rtf"
                | "application/msword"
                | "application/vnd.ms-excel"
                | "application/vnd.ms-powerpoint"
                | "application/xhtml+xml"
                | "application/json"
                | "application/xml"
        )
        || mime.starts_with("application/vnd.openxmlformats-officedocument.")
        || mime.starts_with("application/vnd.oasis.opendocument.")
}

#[derive(sqlx::FromRow)]
struct Unread {
    rowid: i64,
    node_id: Uuid,
    name: String,
    path: String,
    hash: Vec<u8>,
    size: i64,
    mime: Option<String>,
}

/// Read the text of the files whose content was not looked at yet, a
/// round of them. Answers whether more are left.
async fn read_texts(state: &AppState, drive: Uuid, pause: Duration) -> anyhow::Result<bool> {
    // My drive's user's settings and rules; a shared drive's are the
    // defaults (settings per drive come with M7b's levels).
    let user = user_of(state, drive).await;
    let reading = match settings_of(state, user.unwrap_or(drive))
        .await?
        .read_contents
    {
        ReadContents::Always | ReadContents::DefaultOn => true,
        ReadContents::PerItem => false,
    };
    let unread: Vec<Unread> = sqlx::query_as(
        "SELECT d.rowid, d.node_id, d.name, d.path, n.hash, n.size, n.mime
         FROM search_docs d JOIN nodes n ON n.id = d.node_id
         WHERE d.drive_id = ?1 AND d.kind = 'file' AND n.hash IS NOT NULL
           AND (d.text_hash IS NULL OR d.text_hash != n.hash)
         LIMIT ?2",
    )
    .bind(drive)
    .bind(READ_PER_ROUND + 1)
    .fetch_all(state.db.reader())
    .await?;
    if unread.is_empty() {
        return Ok(false);
    }
    let more = unread.len() as i64 > READ_PER_ROUND;
    let rules = match user {
        Some(user) => rules_of(state, user).await?,
        None => Vec::new(),
    };
    let storage = state.files.storage(drive).await?;
    for file in unread.into_iter().take(READ_PER_ROUND as usize) {
        let mime = file.mime.as_deref().unwrap_or("application/octet-stream");
        let full_path = if file.path.is_empty() {
            file.name.clone()
        } else {
            format!("{}/{}", file.path, file.name)
        };
        // A file a rule keeps from being read is not read for search either.
        let guarded = guard::check(
            &rules,
            &Subject {
                name: &file.name,
                path: Some(&full_path),
                ..Subject::default()
            },
        )
        .is_some();
        let mut text =
            if reading && !guarded && readable(mime) && file.size as u64 <= extract::MAX_READ_BYTES
            {
                let path = state.files.rel_path(file.node_id).await?;
                match extract::text_of(&storage, &path, &file.name, mime, MAX_CHARS).await {
                    Ok(text) => text,
                    Err(NoText::Failed(reason)) => {
                        tracing::debug!("cannot read the text of {full_path}: {reason}");
                        String::new()
                    }
                    Err(NoText::TooLarge | NoText::Empty) => String::new(),
                }
            } else {
                String::new()
            };
        // A photo, a scan or a recording: what its model read of this
        // content before, else a `read` job, when a model is chosen for it.
        // In My drives: what a model read is kept per user.
        if reading
            && let Some(user) = user
            && !guarded
            && let Some(work) = crate::reading::work_for(mime, !text.is_empty())
            && state.ai.on(work)
        {
            match crate::reading::cached(state, user, &file.hash, work).await? {
                Some(read) => text = read,
                None => crate::reading::queue(state, user, file.node_id, &file.hash, work).await?,
            }
        }
        let mut tx = state.db.writer().begin().await?;
        sqlx::query("UPDATE search SET text = ?2 WHERE rowid = ?1")
            .bind(file.rowid)
            .bind(&text)
            .execute(&mut *tx)
            .await?;
        sqlx::query("UPDATE search_docs SET text_hash = ?2 WHERE rowid = ?1")
            .bind(file.rowid)
            .bind(&file.hash)
            .execute(&mut *tx)
            .await?;
        tx.commit().await?;
        if !pause.is_zero() {
            tokio::time::sleep(pause).await;
        }
    }
    Ok(more)
}

/// A search's words as an FTS5 query: each word a prefix, all of them
/// needed. Quotes are doubled; anything else in the words is taken as it
/// is (inside quotes, FTS5's operators mean nothing).
pub fn fts_query(q: &str) -> Option<String> {
    let terms: Vec<String> = q
        .split_whitespace()
        .take(20)
        .map(|word| format!("\"{}\"*", word.replace('"', "\"\"")))
        .collect();
    (!terms.is_empty()).then(|| terms.join(" AND "))
}

/// A match.
#[derive(Debug, serde::Serialize, utoipa::ToSchema, sqlx::FromRow)]
pub struct Hit {
    pub node_id: Uuid,
    /// The drive it is in (a string, a UUID for this server's drives).
    #[schema(value_type = String)]
    pub drive_id: Uuid,
    /// `file` or `folder`.
    pub kind: String,
    pub name: String,
    /// The folder it is in, from the root ('' at the top).
    pub path: String,
    /// A piece of its text around the words found, the words between
    /// U+0001 and U+0002; empty when they were found in the name or path
    /// only.
    pub snippet: String,
    /// Found by meaning (embeddings) rather than by its words.
    #[sqlx(default)]
    pub semantic: bool,
    /// What the user may do with it (docs/M7B_DESIGN.md §1).
    #[sqlx(skip)]
    #[schema(required = true)]
    pub my_role: Option<crate::files::roles::Role>,
}

/// The best matches of a user's search: the name counts most, then the
/// path, then the text. What is hidden from them (a limited folder, others'
/// uploads where they may only upload) is left out.
pub async fn search(
    state: &AppState,
    user: Uuid,
    q: &str,
    limit: i64,
) -> Result<Vec<Hit>, sqlx::Error> {
    let Some(query) = fts_query(q) else {
        return Ok(Vec::new());
    };
    // More than asked: some may be hidden from the user.
    let found: Vec<Hit> = sqlx::query_as(sqlx::AssertSqlSafe(format!(
        "SELECT d.node_id, d.drive_id, d.kind, d.name, d.path,
           snippet(search, 2, char(1), char(2), '…', 16) AS snippet
         FROM search JOIN search_docs d ON d.rowid = search.rowid
         WHERE search MATCH ?1 AND d.drive_id IN {USER_DRIVES}
         ORDER BY bm25(search, 10.0, 3.0, 1.0)
         LIMIT ?3"
    )))
    .bind(query)
    .bind(user)
    .bind(limit * 4)
    .fetch_all(state.db.reader())
    .await?;
    let mut hits = Vec::new();
    let mut conn = state.db.reader().acquire().await?;
    for mut hit in found {
        if hits.len() as i64 == limit {
            break;
        }
        if let Some(seen) = crate::files::roles::resolve(&mut conn, user, hit.node_id).await? {
            hit.my_role = Some(seen.role);
            hits.push(hit);
        }
    }
    Ok(hits)
}

/// The files whose text is nearest in meaning to a search, besides those
/// already found: with embeddings only. Their snippet is the start of the
/// piece of text that matched.
pub async fn semantic(
    state: &AppState,
    user: Uuid,
    q: &str,
    limit: usize,
    found: &HashSet<Uuid>,
) -> anyhow::Result<Vec<Hit>> {
    if !state.ai.on(crate::ai::Work::Embeddings) || q.trim().is_empty() {
        return Ok(Vec::new());
    }
    let query = state
        .ai
        .embed(vec![q.to_string()])
        .await?
        .pop()
        .unwrap_or_default();
    let near = state
        .vectors
        .nearest(user, crate::vectors::VectorKind::Chunk, &query, limit * 3)
        .await?;
    let mut seen = found.clone();
    let mut hits = Vec::new();
    for n in near {
        if hits.len() == limit || !seen.insert(n.node_id) {
            continue;
        }
        let row: Option<(String, String, String, String)> = sqlx::query_as(
            "SELECT d.kind, d.name, d.path, s.text FROM search_docs d
             JOIN search s ON s.rowid = d.rowid WHERE d.node_id = ?1 AND d.drive_id = ?2",
        )
        .bind(n.node_id)
        .bind(user)
        .fetch_optional(state.db.reader())
        .await?;
        let Some((kind, name, path, text)) = row else {
            continue;
        };
        let piece: usize = n
            .key
            .rsplit(':')
            .next()
            .and_then(|p| p.parse().ok())
            .unwrap_or(0);
        let snippet = crate::embed::chunks(&text)
            .into_iter()
            .nth(piece)
            .map(|c| extract::truncate(&c, 200))
            .unwrap_or_default();
        let mut conn = state.db.reader().acquire().await?;
        let Some(seen) = crate::files::roles::resolve(&mut conn, user, n.node_id).await? else {
            continue;
        };
        hits.push(Hit {
            node_id: n.node_id,
            drive_id: crate::files::drives::my(user),
            kind,
            name,
            path,
            snippet,
            semantic: true,
            my_role: Some(seen.role),
        });
    }
    Ok(hits)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn words_become_quoted_prefixes() {
        assert_eq!(
            fts_query("power bill").as_deref(),
            Some("\"power\"* AND \"bill\"*")
        );
        assert_eq!(
            fts_query("say \"hi\" OR NEAR(").as_deref(),
            Some("\"say\"* AND \"\"\"hi\"\"\"* AND \"OR\"* AND \"NEAR(\"*")
        );
        assert_eq!(fts_query("   "), None);
    }

    #[test]
    fn readable_types() {
        assert!(readable("application/pdf"));
        assert!(readable("text/plain"));
        assert!(readable(
            "application/vnd.openxmlformats-officedocument.wordprocessingml.document"
        ));
        assert!(!readable("image/jpeg"));
        assert!(!readable("application/zip"));
    }
}
