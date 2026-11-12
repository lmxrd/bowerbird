//! Reading what has no text of its own (docs/M6C_DESIGN.md §3 to §5): a
//! photo or an image by a vision model (its text and a one-line
//! description), a scanned PDF's pages by a vision model, a recording by a
//! speech model (its first hour). Each only once a model is chosen for it
//! (`ai::Work`), and only where text is read now: the drop box's analysis,
//! before the proposal, and the search index, as `read` jobs.
//!
//! The user's "read contents" setting and the sensitive-file rules decide
//! as they do for text: a file a rule holds by its name or path is never
//! read or sent. A keyword rule can only look at what the model gave back.
//!
//! What a model read is kept by the content's hash (`model_reads`), so a
//! file is read once however it is moved, renamed or dropped again.

use crate::ai::{AiError, Work};
use crate::auth::now_iso;
use crate::drop_box::extract::{self, NoText};
use crate::drop_box::guard::{self, Subject};
use crate::files::Topic;
use crate::jobs::{BoxFuture, Handler, Job};
use crate::routes::drop_box::{ReadContents, rules_of, settings_of};
use crate::state::AppState;
use genai::chat::Binary;
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::sync::{Arc, LazyLock, Mutex};
use uuid::Uuid;

pub const KIND: &str = "read";
/// The largest image sent as it is; a larger one is placed by its name (no
/// image library is added to make it smaller).
pub const MAX_IMAGE_BYTES: u64 = 20_000_000;
/// The largest recording sent for its transcription.
pub const MAX_AUDIO_BYTES: u64 = 100_000_000;
/// The most pages of a scanned PDF read.
pub const MAX_PAGES: usize = 20;
/// How many files are read at once (their calls to the models are capped
/// by `AI_CONCURRENCY` on top).
const PARALLEL: usize = 4;

/// What a vision model is asked.
const IMAGE_PROMPT: &str = "Give the text in this image exactly as it is written, then on a last line `Description:` and one line saying what the image shows. If there is no text, give the description line only.";

/// The work that reads a file of this type: images (JPEG, PNG, WebP, GIF:
/// what vision models take), audio, and a PDF without text of its own.
pub fn work_for(mime: &str, has_text: bool) -> Option<Work> {
    match mime {
        "image/jpeg" | "image/png" | "image/webp" | "image/gif" => Some(Work::Images),
        "application/pdf" if !has_text => Some(Work::Scans),
        m if m.starts_with("audio/") => Some(Work::Audio),
        _ => None,
    }
}

/// What a model read of this content before.
pub async fn cached(
    state: &AppState,
    user: Uuid,
    hash: &[u8],
    work: Work,
) -> Result<Option<String>, sqlx::Error> {
    sqlx::query_scalar(
        "SELECT text FROM model_reads WHERE user_id = ?1 AND hash = ?2 AND work = ?3",
    )
    .bind(user)
    .bind(hash)
    .bind(work.as_str())
    .fetch_optional(state.db.reader())
    .await
}

/// Why a file was not read.
#[derive(Debug, thiserror::Error)]
pub enum ReadError {
    /// The model failed: the attempt is tried again.
    #[error("{0}")]
    Model(#[from] AiError),
    #[error("{0}")]
    Io(#[from] std::io::Error),
    #[error("{0}")]
    Db(#[from] sqlx::Error),
}

/// A file read by the model of a work, or what was read of the same
/// content before. `None`: no model does it, or the file is too large,
/// or there is nothing in it to read; it is then placed and found by name.
pub async fn read(
    state: &AppState,
    user: Uuid,
    work: Work,
    (storage, path): (&crate::storage::Storage, &str),
    name: &str,
    mime: &str,
    hash: &[u8],
) -> Result<Option<String>, ReadError> {
    if let Some(text) = cached(state, user, hash, work).await? {
        return Ok(Some(text));
    }
    // One read of a content at a time: the copies of a file wait for it,
    // and find it read.
    let key = (user, hash.to_vec(), work);
    let lock = READING
        .lock()
        .unwrap()
        .entry(key.clone())
        .or_default()
        .clone();
    let read = {
        let _reading = lock.lock().await;
        match cached(state, user, hash, work).await? {
            Some(text) => Ok(Some(text)),
            None => read_now(state, user, work, (storage, path), name, mime, hash).await,
        }
    };
    let mut reading = READING.lock().unwrap();
    if Arc::strong_count(&lock) == 2 {
        reading.remove(&key);
    }
    read
}

type ReadKey = (Uuid, Vec<u8>, Work);

/// The reads under way, by user, content and work.
static READING: LazyLock<Mutex<HashMap<ReadKey, Arc<tokio::sync::Mutex<()>>>>> =
    LazyLock::new(Mutex::default);

async fn read_now(
    state: &AppState,
    user: Uuid,
    work: Work,
    (storage, path): (&crate::storage::Storage, &str),
    name: &str,
    mime: &str,
    hash: &[u8],
) -> Result<Option<String>, ReadError> {
    // The user's choice of model for the work, if they (or their team)
    // made one.
    let chosen = crate::settings::ai_model(state, user, work).await?;
    let Some(model) = state.ai.model_chosen(work, chosen.as_deref()) else {
        return Ok(None);
    };
    let size = storage.file_meta(path).await?.0 as u64;
    let text = match work {
        Work::Images => {
            if size > MAX_IMAGE_BYTES {
                return Ok(None);
            }
            let bytes = storage.read(path).await?;
            look(state, work, chosen.as_deref(), bytes, mime, name).await?
        }
        Work::Scans => {
            if size > extract::MAX_READ_BYTES {
                return Ok(None);
            }
            let bytes = storage.read(path).await?;
            let pages = match extract::page_images(bytes, mime, MAX_PAGES).await {
                Ok(pages) => pages,
                Err(NoText::Failed(reason)) => {
                    tracing::info!("cannot find the pages of {name}: {reason}");
                    return Ok(None);
                }
                Err(_) => return Ok(None),
            };
            let mut text = String::new();
            for (n, page) in pages.into_iter().enumerate() {
                if page.bytes.len() as u64 > MAX_IMAGE_BYTES {
                    continue;
                }
                let read = look(
                    state,
                    work,
                    chosen.as_deref(),
                    page.bytes,
                    &page.mime_type,
                    name,
                )
                .await?;
                if !read.trim().is_empty() {
                    text.push_str(&format!("Page {}:\n{}\n\n", n + 1, read.trim()));
                }
            }
            text
        }
        Work::Audio => {
            if size > MAX_AUDIO_BYTES {
                return Ok(None);
            }
            let bytes = storage.read(path).await?;
            state
                .ai
                .transcribe(chosen.as_deref(), bytes, name, mime)
                .await?
        }
        Work::Proposals | Work::Embeddings => return Ok(None),
    };
    let text = extract::truncate(&extract::squeeze(&text), crate::search::MAX_CHARS);
    sqlx::query(
        "INSERT INTO model_reads (user_id, hash, work, text, model, read_at)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6)
         ON CONFLICT (user_id, hash, work) DO UPDATE SET text = ?4, model = ?5, read_at = ?6",
    )
    .bind(user)
    .bind(hash)
    .bind(work.as_str())
    .bind(&text)
    .bind(&model.name)
    .bind(now_iso())
    .execute(state.db.writer())
    .await?;
    Ok(Some(text))
}

async fn look(
    state: &AppState,
    work: Work,
    chosen: Option<&str>,
    bytes: Vec<u8>,
    mime: &str,
    name: &str,
) -> Result<String, AiError> {
    let image = Binary::from_base64(
        mime,
        crate::util::base64_encode(&bytes),
        Some(name.to_string()),
    );
    state.ai.look(work, chosen, IMAGE_PROMPT, image).await
}

/// The text the drop box's model is shown: marked as read by a model.
pub fn marked(work: Work, text: &str) -> String {
    let what = match work {
        Work::Images => "Read from the image by a vision model",
        Work::Scans => "Read from the scanned pages by a vision model",
        _ => "Transcribed from the recording by a speech model",
    };
    format!("[{what}]\n{text}")
}

#[derive(Serialize, Deserialize)]
pub struct Payload {
    pub user: Uuid,
    pub node: Uuid,
    /// The content it was queued for.
    pub hash: String,
    pub work: Work,
}

/// The `read` job: a file of a user's tree read for search, then its text
/// put in the index (and so embedded).
pub struct Read;

impl Handler for Read {
    fn kind(&self) -> &'static str {
        KIND
    }

    fn parallel(&self) -> usize {
        PARALLEL
    }

    fn run<'a>(&'a self, state: &'a AppState, job: Job) -> BoxFuture<'a, Result<(), String>> {
        Box::pin(async move {
            let payload: Payload = job.payload()?;
            read_node(state, payload).await.map_err(|e| e.to_string())
        })
    }
}

#[derive(sqlx::FromRow)]
struct NodeFile {
    name: String,
    path: String,
    mime: Option<String>,
    hash: Option<Vec<u8>>,
}

async fn read_node(state: &AppState, payload: Payload) -> anyhow::Result<()> {
    let Payload {
        user,
        node,
        hash,
        work,
    } = payload;
    let hash = hex::decode(&hash)?;
    let file: Option<NodeFile> = sqlx::query_as(
        "SELECT d.name, d.path, n.mime, n.hash FROM search_docs d JOIN nodes n ON n.id = d.node_id
         WHERE d.node_id = ?1 AND d.drive_id = ?2",
    )
    .bind(node)
    .bind(user)
    .fetch_optional(state.db.reader())
    .await?;
    // Gone, or changed since: the index reads it again by itself.
    let Some(file) = file else { return Ok(()) };
    if file.hash.as_deref() != Some(hash.as_slice()) {
        return Ok(());
    }
    // The settings and rules as they are now.
    if !allowed(state, user, &file.name, &file.path).await? {
        return Ok(());
    }
    let storage = state.files.storage(user).await?;
    let path = state.files.rel_path(node).await?;
    let mime = file.mime.as_deref().unwrap_or("application/octet-stream");
    let Some(text) = read(
        state,
        user,
        work,
        (&storage, &path),
        &file.name,
        mime,
        &hash,
    )
    .await?
    else {
        return Ok(());
    };
    set_text(state, user, node, &hash, &text).await?;
    // The embeddings follow the index.
    state.files.announce(user, Topic::Reindex);
    Ok(())
}

/// The user reads contents for search, and no rule holds the file by its
/// name or path.
pub async fn allowed(
    state: &AppState,
    user: Uuid,
    name: &str,
    folder: &str,
) -> Result<bool, sqlx::Error> {
    let reading = match settings_of(state, user).await?.read_contents {
        ReadContents::Always | ReadContents::DefaultOn => true,
        ReadContents::PerItem => false,
    };
    if !reading {
        return Ok(false);
    }
    let rules = rules_of(state, user).await?;
    let full_path = if folder.is_empty() {
        name.to_string()
    } else {
        format!("{folder}/{name}")
    };
    Ok(guard::check(
        &rules,
        &Subject {
            name,
            path: Some(&full_path),
            ..Subject::default()
        },
    )
    .is_none())
}

/// A file's text in the index, if it still has this content; a keyword
/// rule that matches what the model read keeps it out.
pub async fn set_text(
    state: &AppState,
    user: Uuid,
    node: Uuid,
    hash: &[u8],
    text: &str,
) -> Result<(), sqlx::Error> {
    let rules = rules_of(state, user).await?;
    let text = if guard::check(
        &rules,
        &Subject {
            name: "",
            text: Some(text),
            ..Subject::default()
        },
    )
    .is_some()
    {
        ""
    } else {
        text
    };
    let mut tx = state.db.writer().begin().await?;
    let rowid: Option<i64> = sqlx::query_scalar(
        "SELECT rowid FROM search_docs WHERE node_id = ?1 AND drive_id = ?2 AND text_hash = ?3",
    )
    .bind(node)
    .bind(user)
    .bind(hash)
    .fetch_optional(&mut *tx)
    .await?;
    if let Some(rowid) = rowid {
        sqlx::query("UPDATE search SET text = ?2 WHERE rowid = ?1")
            .bind(rowid)
            .bind(text)
            .execute(&mut *tx)
            .await?;
    }
    tx.commit().await
}

/// Queue a file of the index to be read by a model.
pub async fn queue(
    state: &AppState,
    user: Uuid,
    node: Uuid,
    hash: &[u8],
    work: Work,
) -> Result<(), sqlx::Error> {
    state
        .jobs
        .add(
            KIND,
            &Payload {
                user,
                node,
                hash: hex::encode(hash),
                work,
            },
            None,
        )
        .await?;
    Ok(())
}

/// "Read existing files again": the photos, PDFs and recordings of every
/// user's index that no model read are looked at again by the index (which
/// queues those a model is now chosen for). Answers how many.
pub async fn read_again(state: &AppState) -> Result<i64, sqlx::Error> {
    let users: Vec<Uuid> = sqlx::query_scalar("SELECT id FROM users")
        .fetch_all(state.db.reader())
        .await?;
    let mut total = 0;
    for user in users {
        let done = sqlx::query(
            "UPDATE search_docs SET text_hash = NULL
             WHERE drive_id = ?1 AND kind = 'file' AND text_hash IS NOT NULL
               AND node_id IN (SELECT id FROM nodes WHERE drive_id = ?1
                 AND (mime LIKE 'image/%' OR mime LIKE 'audio/%' OR mime = 'application/pdf'))
               AND NOT EXISTS (SELECT 1 FROM model_reads r
                 WHERE r.user_id = ?1 AND r.hash = search_docs.text_hash)",
        )
        .bind(user)
        .execute(state.db.writer())
        .await?
        .rows_affected() as i64;
        if done > 0 {
            state.files.announce(user, Topic::Reindex);
        }
        total += done;
    }
    Ok(total)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_work_follows_the_type() {
        assert_eq!(work_for("image/jpeg", false), Some(Work::Images));
        assert_eq!(work_for("image/heic", false), None);
        assert_eq!(work_for("audio/mpeg", false), Some(Work::Audio));
        assert_eq!(work_for("application/pdf", false), Some(Work::Scans));
        assert_eq!(work_for("application/pdf", true), None);
        assert_eq!(work_for("text/plain", false), None);
    }
}
