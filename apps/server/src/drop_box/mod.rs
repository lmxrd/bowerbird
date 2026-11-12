//! The drop box (docs/M2_DESIGN.md §1): items arrive (an upload, a link),
//! are staged in `<DATA_DIR>/users/<id>/inbox/<item id>/`, and are analysed
//! by an `analyse` job, which checks the sensitive-file rules, reads the
//! metadata and the text, and goes on to the proposal.
//!
//! The HTTP side is `routes/drop_box.rs`; the settings and rules live
//! there too.

pub mod analyse;
pub mod extract;
pub mod guard;
pub mod link;
pub mod place;
pub mod propose;
pub mod retrieve;
pub mod review;
pub mod snapshot;

use crate::auth::now_iso;
use crate::files::{FileError, Topic, names};
use crate::routes::drop_box::{Links, ReadContents, settings_of};
use crate::state::AppState;
use serde::Serialize;
use std::path::PathBuf;
use utoipa::ToSchema;
use uuid::Uuid;

/// The longest note a user may add to an item.
pub const MAX_NOTE: usize = 2_000;
/// The longest link.
pub const MAX_URL: usize = 2_048;

/// What was dropped.
#[derive(Clone, Copy, Debug, PartialEq, Serialize, ToSchema, sqlx::Type)]
#[serde(rename_all = "snake_case")]
#[sqlx(rename_all = "snake_case")]
pub enum ItemKind {
    File,
    Folder,
    Link,
}

/// Where an item is in its life.
#[derive(Clone, Copy, Debug, PartialEq, Serialize, serde::Deserialize, ToSchema, sqlx::Type)]
#[serde(rename_all = "snake_case")]
#[sqlx(rename_all = "snake_case")]
pub enum Status {
    /// Waiting for its analysis.
    Queued,
    Analysing,
    /// No AI connection: analysed once there is one.
    Waiting,
    /// A place is proposed, to confirm.
    Proposed,
    Confirmed,
    /// Placed elsewhere than proposed.
    Changed,
    /// The proposal was refused: to place by hand.
    Rejected,
    AutoAccepted,
    /// The analysis failed three times (`error` says why); it can be
    /// retried.
    Failed,
    /// To place by hand: a sensitive-file rule says so, or there is no
    /// proposal.
    NeedsManual,
    Undone,
}

/// An item of the drop box.
#[derive(Clone, Debug, Serialize, ToSchema, sqlx::FromRow)]
pub struct InboxItem {
    pub id: Uuid,
    pub kind: ItemKind,
    /// The file's name; for a link, its address until the page gives a
    /// title.
    pub name: String,
    pub url: Option<String>,
    /// What the user said about it when dropping it.
    pub note: Option<String>,
    pub size: Option<i64>,
    pub mime_type: Option<String>,
    /// Whether its contents may be read and sent.
    pub read_contents: bool,
    /// For a link: what it becomes.
    pub link_mode: Option<Links>,
    pub status: Status,
    /// The sensitive-file rule that matched: `name_only` (its contents were
    /// not read) or `by_hand` (nothing was sent).
    pub guard: Option<crate::routes::drop_box::RuleAction>,
    /// The length of the text read from it, in characters; absent when
    /// none was.
    pub text_length: Option<i64>,
    /// Why the analysis failed.
    pub error: Option<String>,
    /// Where it is in the tree, once placed.
    pub node_id: Option<Uuid>,
    /// Its path inside a dropped folder that was split into its files.
    pub source_path: Option<String>,
    pub created_at: String,
    pub updated_at: String,
}

/// The columns of an [`InboxItem`].
pub const ITEM_COLUMNS: &str = "id, kind, name, url, note, size, mime_type, read_contents, \
    link_mode, status, guard, length(text) AS text_length, error, node_id, source_path, \
    created_at, updated_at";

/// Where an item's file is staged.
pub fn staging_dir(state: &AppState, user_id: Uuid, item_id: Uuid) -> PathBuf {
    state
        .files
        .user_dir(user_id)
        .join("inbox")
        .join(item_id.to_string())
}

/// An item of a user, or `NotFound`.
pub async fn item(state: &AppState, user_id: Uuid, id: Uuid) -> Result<InboxItem, FileError> {
    sqlx::query_as(sqlx::AssertSqlSafe(format!(
        "SELECT {ITEM_COLUMNS} FROM inbox_items WHERE id = ?1 AND user_id = ?2"
    )))
    .bind(id)
    .bind(user_id)
    .fetch_optional(state.db.reader())
    .await?
    .ok_or(FileError::NotFound)
}

/// Whether an item's contents are read: the user's setting, and their
/// choice for this item where the setting leaves it to them.
async fn reads_contents(
    state: &AppState,
    user_id: Uuid,
    asked: Option<bool>,
) -> Result<bool, sqlx::Error> {
    Ok(match settings_of(state, user_id).await?.read_contents {
        ReadContents::Always => true,
        ReadContents::DefaultOn => asked.unwrap_or(true),
        ReadContents::PerItem => asked.unwrap_or(false),
    })
}

/// A note as stored: trimmed, `None` when empty, refused when too long.
pub fn clean_note(note: Option<String>) -> Result<Option<String>, FileError> {
    let note = note.map(|n| n.trim().to_string()).filter(|n| !n.is_empty());
    if note.as_ref().is_some_and(|n| n.chars().count() > MAX_NOTE) {
        return Err(FileError::Invalid(format!(
            "A note is at most {MAX_NOTE} characters"
        )));
    }
    Ok(note)
}

/// A file received into `temp` becomes an item: it is moved into its
/// staging folder, recorded, and its analysis queued. `temp` is gone
/// afterwards either way.
pub async fn add_file(
    state: &AppState,
    user_id: Uuid,
    name: &str,
    temp: &std::path::Path,
    note: Option<String>,
    read_contents: Option<bool>,
) -> Result<InboxItem, FileError> {
    add_file_from(
        state,
        user_id,
        name,
        temp,
        Origin {
            note,
            read_contents,
            ..Origin::default()
        },
    )
    .await
}

/// How a file came into the drop box.
#[derive(Default)]
pub struct Origin {
    pub note: Option<String>,
    pub read_contents: Option<bool>,
    /// The WebDAV drop box's folder it came in by (`""`, `review`, `auto`).
    pub dav_folder: Option<&'static str>,
    /// How long its analysis waits (a WebDAV client may write it again).
    pub delay: Option<std::time::Duration>,
    /// Its path inside a dropped folder that was split into its files.
    pub source_path: Option<String>,
}

/// [`add_file`], from somewhere in particular.
pub async fn add_file_from(
    state: &AppState,
    user_id: Uuid,
    name: &str,
    temp: &std::path::Path,
    origin: Origin,
) -> Result<InboxItem, FileError> {
    let result = stage_file(state, user_id, name, temp, origin).await;
    if result.is_err() {
        let _ = std::fs::remove_file(temp);
    }
    result
}

async fn stage_file(
    state: &AppState,
    user_id: Uuid,
    name: &str,
    temp: &std::path::Path,
    origin: Origin,
) -> Result<InboxItem, FileError> {
    let Origin {
        note,
        read_contents,
        dav_folder,
        delay,
        source_path,
    } = origin;
    let name = names::new_name(name).map_err(FileError::Invalid)?;
    let note = clean_note(note)?;
    let read_contents = reads_contents(state, user_id, read_contents).await?;
    let size = std::fs::metadata(temp)?.len() as i64;
    state.files.check_room(user_id, size as u64).await?;
    let id = Uuid::now_v7();
    let dir = staging_dir(state, user_id, id);
    std::fs::create_dir_all(&dir)?;
    if let Err(e) = crate::files::move_path(temp, &dir.join(&name)) {
        let _ = std::fs::remove_dir_all(&dir);
        return Err(e.into());
    }
    let now = now_iso();
    let inserted = sqlx::query(
        "INSERT INTO inbox_items (id, user_id, kind, name, note, size, mime_type, read_contents,
           status, created_at, updated_at, dav_folder, source_path)
         VALUES (?1, ?2, 'file', ?3, ?4, ?5, ?6, ?7, 'queued', ?8, ?8, ?9, ?10)",
    )
    .bind(id)
    .bind(user_id)
    .bind(&name)
    .bind(&note)
    .bind(size)
    .bind(mime_type(&name))
    .bind(read_contents)
    .bind(&now)
    .bind(dav_folder)
    .bind(&source_path)
    .execute(state.db.writer())
    .await;
    if let Err(e) = inserted {
        let _ = std::fs::remove_dir_all(&dir);
        return Err(e.into());
    }
    queue_after(state, user_id, id, delay).await?;
    item(state, user_id, id).await
}

/// A dropped folder kept whole becomes one item: its files, received into
/// temporary files, are staged under `<staging>/<name>/` at their paths
/// (each part checked as a name). The temporary files are gone afterwards
/// either way.
pub async fn add_folder(
    state: &AppState,
    user_id: Uuid,
    name: &str,
    files: Vec<(Vec<String>, PathBuf)>,
    note: Option<String>,
    read_contents: Option<bool>,
) -> Result<InboxItem, FileError> {
    let result = stage_folder(state, user_id, name, &files, note, read_contents).await;
    for (_, temp) in &files {
        let _ = std::fs::remove_file(temp);
    }
    result
}

async fn stage_folder(
    state: &AppState,
    user_id: Uuid,
    name: &str,
    files: &[(Vec<String>, PathBuf)],
    note: Option<String>,
    read_contents: Option<bool>,
) -> Result<InboxItem, FileError> {
    let name = names::new_name(name).map_err(FileError::Invalid)?;
    let note = clean_note(note)?;
    let read_contents = reads_contents(state, user_id, read_contents).await?;
    let total: u64 = files
        .iter()
        .map(|(_, temp)| std::fs::metadata(temp).map(|m| m.len()).unwrap_or(0))
        .sum();
    state.files.check_room(user_id, total).await?;
    let id = Uuid::now_v7();
    let dir = staging_dir(state, user_id, id);
    let root = dir.join(&name);
    let staged: Result<i64, FileError> = (|| {
        std::fs::create_dir_all(&root)?;
        let mut size = 0i64;
        for (parts, temp) in files {
            let mut path = root.clone();
            for part in parts {
                path = path.join(names::new_name(part).map_err(FileError::Invalid)?);
            }
            if let Some(parent) = path.parent() {
                std::fs::create_dir_all(parent)?;
            }
            if path.exists() {
                return Err(FileError::Conflict(format!(
                    "{} is in the folder twice",
                    parts.join("/")
                )));
            }
            size += std::fs::metadata(temp)?.len() as i64;
            crate::files::move_path(temp, &path)?;
        }
        Ok(size)
    })();
    let size = match staged {
        Ok(size) => size,
        Err(e) => {
            let _ = std::fs::remove_dir_all(&dir);
            return Err(e);
        }
    };
    let now = now_iso();
    let inserted = sqlx::query(
        "INSERT INTO inbox_items (id, user_id, kind, name, note, size, read_contents,
           status, created_at, updated_at)
         VALUES (?1, ?2, 'folder', ?3, ?4, ?5, ?6, 'queued', ?7, ?7)",
    )
    .bind(id)
    .bind(user_id)
    .bind(&name)
    .bind(&note)
    .bind(size)
    .bind(read_contents)
    .bind(&now)
    .execute(state.db.writer())
    .await;
    if let Err(e) = inserted {
        let _ = std::fs::remove_dir_all(&dir);
        return Err(e.into());
    }
    queue(state, user_id, id).await?;
    item(state, user_id, id).await
}

/// A folder made in the WebDAV drop box: an empty folder item, filled by
/// the files written into it, analysed `delay` after the last one.
pub async fn add_empty_folder(
    state: &AppState,
    user_id: Uuid,
    name: &str,
    dav_folder: &'static str,
    delay: std::time::Duration,
) -> Result<InboxItem, FileError> {
    let name = names::new_name(name).map_err(FileError::Invalid)?;
    let read_contents = reads_contents(state, user_id, None).await?;
    let id = Uuid::now_v7();
    std::fs::create_dir_all(staging_dir(state, user_id, id).join(&name))?;
    let now = now_iso();
    sqlx::query(
        "INSERT INTO inbox_items (id, user_id, kind, name, size, read_contents, status,
           created_at, updated_at, dav_folder)
         VALUES (?1, ?2, 'folder', ?3, 0, ?4, 'queued', ?5, ?5, ?6)",
    )
    .bind(id)
    .bind(user_id)
    .bind(&name)
    .bind(read_contents)
    .bind(&now)
    .bind(dav_folder)
    .execute(state.db.writer())
    .await?;
    queue_after(state, user_id, id, Some(delay)).await?;
    item(state, user_id, id).await
}

/// The files of a staged folder, by their paths inside it, sorted.
pub fn folder_files(root: &std::path::Path) -> Vec<String> {
    fn walk(dir: &std::path::Path, prefix: &str, out: &mut Vec<String>) {
        let Ok(entries) = std::fs::read_dir(dir) else {
            return;
        };
        for entry in entries.flatten() {
            let name = entry.file_name().to_string_lossy().into_owned();
            let path = if prefix.is_empty() {
                name
            } else {
                format!("{prefix}/{name}")
            };
            match entry.file_type() {
                Ok(t) if t.is_dir() => walk(&entry.path(), &path, out),
                Ok(_) => out.push(path),
                Err(_) => {}
            }
        }
    }
    let mut out = Vec::new();
    walk(root, "", &mut out);
    out.sort();
    out
}

/// A link becomes an item, and its analysis is queued.
pub async fn add_link(
    state: &AppState,
    user_id: Uuid,
    url: &str,
    note: Option<String>,
    mode: Option<Links>,
    read_contents: Option<bool>,
) -> Result<InboxItem, FileError> {
    let url = url.trim();
    let parsed = reqwest::Url::parse(url)
        .ok()
        .filter(|u| matches!(u.scheme(), "http" | "https") && u.host_str().is_some())
        .ok_or_else(|| FileError::Invalid("The link must be an http:// or https:// URL".into()))?;
    if url.len() > MAX_URL {
        return Err(FileError::Invalid(format!(
            "A link is at most {MAX_URL} characters"
        )));
    }
    let note = clean_note(note)?;
    let settings = settings_of(state, user_id).await?;
    let mode = mode.unwrap_or(settings.links);
    let read_contents = reads_contents(state, user_id, read_contents).await?;
    let id = Uuid::now_v7();
    let now = now_iso();
    // Its address, without the scheme, until the page gives a title.
    let name = format!(
        "{}{}",
        parsed.host_str().unwrap_or_default(),
        parsed.path().trim_end_matches('/')
    );
    sqlx::query(
        "INSERT INTO inbox_items (id, user_id, kind, name, url, note, read_contents, link_mode,
           status, created_at, updated_at)
         VALUES (?1, ?2, 'link', ?3, ?4, ?5, ?6, ?7, 'queued', ?8, ?8)",
    )
    .bind(id)
    .bind(user_id)
    .bind(&name)
    .bind(parsed.as_str())
    .bind(&note)
    .bind(read_contents)
    .bind(mode)
    .bind(&now)
    .execute(state.db.writer())
    .await?;
    queue(state, user_id, id).await?;
    item(state, user_id, id).await
}

/// Queue an item's analysis, and tell the user's apps.
pub async fn queue(state: &AppState, user_id: Uuid, item_id: Uuid) -> Result<(), sqlx::Error> {
    queue_after(state, user_id, item_id, None).await
}

/// [`queue`], to start after a while.
pub async fn queue_after(
    state: &AppState,
    user_id: Uuid,
    item_id: Uuid,
    delay: Option<std::time::Duration>,
) -> Result<(), sqlx::Error> {
    let payload = analyse::Payload { item: item_id };
    // One analysis waiting per item: a second queuing (a WebDAV client
    // writing the file again) moves the waiting one's start.
    let moved = sqlx::query(
        "UPDATE jobs SET run_after = ?3
         WHERE kind = ?1 AND payload = ?2 AND status = 'queued'",
    )
    .bind(analyse::KIND)
    .bind(serde_json::to_string(&payload).expect("a payload that serializes"))
    .bind(crate::auth::iso(
        std::time::SystemTime::now() + delay.unwrap_or_default(),
    ))
    .execute(state.db.writer())
    .await?;
    if moved.rows_affected() == 0 {
        state.jobs.add(analyse::KIND, &payload, delay).await?;
    }
    state.files.announce(user_id, Topic::DropBox);
    Ok(())
}

/// Set an item's status (and its error), and tell the user's apps.
pub async fn set_status(
    state: &AppState,
    user_id: Uuid,
    item_id: Uuid,
    status: Status,
    error: Option<&str>,
) -> Result<(), sqlx::Error> {
    sqlx::query("UPDATE inbox_items SET status = ?2, error = ?3, updated_at = ?4 WHERE id = ?1")
        .bind(item_id)
        .bind(status)
        .bind(error)
        .bind(now_iso())
        .execute(state.db.writer())
        .await?;
    state.files.announce(user_id, Topic::DropBox);
    Ok(())
}

/// Queue the analysis of every item that waited for an AI connection: one
/// was just set up.
pub async fn resume_waiting(state: &AppState) -> Result<(), sqlx::Error> {
    let waiting: Vec<(Uuid, Uuid)> =
        sqlx::query_as("SELECT id, user_id FROM inbox_items WHERE status = 'waiting'")
            .fetch_all(state.db.reader())
            .await?;
    for (id, user_id) in waiting {
        sqlx::query(
            "UPDATE inbox_items SET status = 'queued' WHERE id = ?1 AND status = 'waiting'",
        )
        .bind(id)
        .execute(state.db.writer())
        .await?;
        queue(state, user_id, id).await?;
    }
    Ok(())
}

/// The handlers of the drop box's jobs, for [`crate::jobs::Jobs::new`].
pub fn handlers() -> Vec<std::sync::Arc<dyn crate::jobs::Handler>> {
    vec![
        std::sync::Arc::new(analyse::Analyse),
        std::sync::Arc::new(place::Accept),
    ]
}

/// The type of a file, from its name.
pub fn mime_type(name: &str) -> String {
    mime_guess::from_path(name)
        .first_or_octet_stream()
        .essence_str()
        .to_string()
}
