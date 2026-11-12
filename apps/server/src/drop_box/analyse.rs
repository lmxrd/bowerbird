//! The `analyse` job of an item: the sensitive-file rules, the metadata,
//! the text when reading it is allowed, then the proposal (docs/M2_DESIGN.md
//! §1, step 2).

use super::extract::{self, NoText};
use super::guard::{self, Subject};
use super::link;
use super::place::{self, Target};
use super::propose::{self, Example, ItemFacts};
use super::retrieve;
use super::snapshot;
use super::{ITEM_COLUMNS, InboxItem, ItemKind, Status, set_status, staging_dir};
use crate::ai::AiError;
use crate::auth::now_iso;
use crate::jobs::{BoxFuture, Handler, Job};
use crate::routes::drop_box::{Links, PendingMode, settings_of};
use crate::routes::drop_box::{RuleAction, rules_of};
use crate::state::AppState;
use serde::{Deserialize, Serialize};
use uuid::Uuid;

pub const KIND: &str = "analyse";
/// How many items are analysed at once (their calls to the model are capped
/// by `AI_CONCURRENCY` on top).
const PARALLEL: usize = 4;
/// How many of the user's corrections the model is shown.
const EXAMPLES: i64 = 5;

#[derive(Serialize, Deserialize)]
pub struct Payload {
    pub item: Uuid,
}

pub struct Analyse;

impl Handler for Analyse {
    fn kind(&self) -> &'static str {
        KIND
    }

    fn parallel(&self) -> usize {
        PARALLEL
    }

    fn run<'a>(&'a self, state: &'a AppState, job: Job) -> BoxFuture<'a, Result<(), String>> {
        Box::pin(async move {
            let Payload { item } = job.payload()?;
            analyse(state, item).await.map_err(|e| e.to_string())
        })
    }

    fn give_up<'a>(&'a self, state: &'a AppState, job: Job, error: String) -> BoxFuture<'a, ()> {
        Box::pin(async move {
            let Ok(Payload { item }) = job.payload() else {
                return;
            };
            let user: Option<(Uuid,)> =
                sqlx::query_as("SELECT user_id FROM inbox_items WHERE id = ?1")
                    .bind(item)
                    .fetch_optional(state.db.reader())
                    .await
                    .ok()
                    .flatten();
            if let Some((user,)) = user
                && let Err(e) = set_status(state, user, item, Status::Failed, Some(&error)).await
            {
                tracing::warn!("cannot mark drop box item {item} failed: {e}");
            }
        })
    }
}

/// The row the analysis starts from.
#[derive(sqlx::FromRow)]
struct Row {
    user_id: Uuid,
    kind: ItemKind,
    name: String,
    note: Option<String>,
    mime_type: Option<String>,
    read_contents: bool,
    status: Status,
    url: Option<String>,
    link_mode: Option<Links>,
    source_path: Option<String>,
}

/// Analyse an item: an error is a failure of this attempt (the job is
/// tried again); anything that decides the item ends in its status.
pub async fn analyse(state: &AppState, id: Uuid) -> anyhow::Result<()> {
    let row: Option<Row> = sqlx::query_as(
        "SELECT user_id, kind, name, note, mime_type, read_contents, status, url, link_mode,
           source_path
         FROM inbox_items WHERE id = ?1",
    )
    .bind(id)
    .fetch_optional(state.db.reader())
    .await?;
    // Gone, or placed or decided in the meantime: nothing to do.
    let Some(row) = row else { return Ok(()) };
    if !matches!(
        row.status,
        Status::Queued | Status::Analysing | Status::Failed
    ) {
        return Ok(());
    }
    let user = row.user_id;
    set_status(state, user, id, Status::Analysing, None).await?;

    // The rules on what is known before anything is read.
    let rules = rules_of(state, user).await?;
    let mut subject = Subject {
        name: &row.name,
        note: row.note.as_deref(),
        path: row.source_path.as_deref(),
        ..Subject::default()
    };
    let mut guard = guard::check(&rules, &subject);
    // A folder: the rules on each of its files too, the strictest wins.
    let folder_root = staging_dir(state, user, id).join(&row.name);
    let folder_files = if row.kind == ItemKind::Folder {
        super::folder_files(&folder_root)
    } else {
        Vec::new()
    };
    for file in &folder_files {
        let path = format!("{}/{file}", row.name);
        let name = file.rsplit('/').next().unwrap_or(file);
        let found = guard::check(
            &rules,
            &Subject {
                name,
                path: Some(&path),
                ..Subject::default()
            },
        );
        if found == Some(RuleAction::ByHand) {
            return by_hand(state, user, id).await;
        }
        guard = guard.or(found);
    }
    if guard == Some(RuleAction::ByHand) {
        return by_hand(state, user, id).await;
    }

    // Nothing is sent without an AI connection: the item waits for one.
    if !state.ai.enabled() {
        set_guard(state, id, guard, None).await?;
        return Ok(set_status(state, user, id, Status::Waiting, None).await?);
    }

    // A link becomes a file: its bookmark (with the page's text), or the
    // file it points to.
    let mut name = row.name.clone();
    let mut mime = row.mime_type.clone();
    let mut text = None;
    let mut read_file = row.kind == ItemKind::File;
    if row.kind == ItemKind::Link {
        let url = row.url.as_deref().unwrap_or_default();
        let mode = row.link_mode.unwrap_or(Links::Bookmark);
        let staged = stage_link(state, user, id, url, mode).await?;
        name = staged.name;
        mime = Some(staged.mime_type);
        read_file = mode == Links::Download;
        text = staged.page_text;
        // The rules again, on its name now.
        subject.name = &name;
        guard = guard::check(&rules, &subject);
        if guard == Some(RuleAction::ByHand) {
            return by_hand(state, user, id).await;
        }
    }
    if !row.read_contents || guard.is_some() {
        text = None;
    }
    // A folder's text: its file list, and the start of a few of its files.
    if row.kind == ItemKind::Folder {
        text = Some(
            folder_text(
                &folder_root,
                &folder_files,
                row.read_contents && guard.is_none(),
            )
            .await,
        );
    }

    // The text, read here, when the user allows it and no rule forbids it.
    if read_file && row.read_contents && guard.is_none() {
        let path = staging_dir(state, user, id).join(&name);
        let mime = mime.as_deref().unwrap_or("application/octet-stream");
        match extract::text_of_file(&path, &name, mime, extract::MAX_CHARS).await {
            Ok(read) => text = Some(read),
            Err(NoText::Failed(reason)) => {
                tracing::info!("cannot read the text of drop box item {id}: {reason}")
            }
            Err(NoText::TooLarge | NoText::Empty) => {}
        }
        // A photo, a scan or a recording: read by its model, when one is
        // chosen (once per content).
        if let Some(work) = crate::reading::work_for(mime, text.is_some())
            && state.ai.on(work)
            && let Some(hash) = crate::files::scan::hash_file(&path).await
            && let Ok((staged, rel)) = crate::storage::Storage::local_file(&path)
            && let Some(read) =
                crate::reading::read(state, user, work, (&staged, &rel), &name, mime, &hash).await?
            && !read.is_empty()
        {
            text = Some(extract::truncate(
                &crate::reading::marked(work, &read),
                extract::MAX_CHARS,
            ));
        }
    }
    // The keywords in the text, now that it is known (here, before anything
    // is sent).
    if let Some(read) = &text {
        subject.text = Some(read);
        guard = guard::check(&rules, &subject);
        match guard {
            Some(RuleAction::ByHand) => return by_hand(state, user, id).await,
            Some(RuleAction::NameOnly) => text = None,
            None => {}
        }
    }
    set_guard(state, id, guard, text.as_deref()).await?;

    propose_and_place(state, id).await
}

/// Ask the model where the item goes, check the answer, store it, and act
/// on it by the user's pending mode. A model that cannot be reached fails
/// this attempt (the job is tried again); one whose answers do not fit the
/// tree, twice, leaves the item to the user.
pub async fn propose_and_place(state: &AppState, id: Uuid) -> anyhow::Result<()> {
    let item: InboxItem = sqlx::query_as(sqlx::AssertSqlSafe(format!(
        "SELECT {ITEM_COLUMNS} FROM inbox_items WHERE id = ?1"
    )))
    .bind(id)
    .fetch_one(state.db.reader())
    .await?;
    let (user, text): (Uuid, Option<String>) =
        sqlx::query_as("SELECT user_id, text FROM inbox_items WHERE id = ?1")
            .bind(id)
            .fetch_one(state.db.reader())
            .await?;
    let settings = settings_of(state, user).await?;
    let allowed_top: Option<Vec<String>> = settings
        .allowed_top_folders
        .as_deref()
        .and_then(|json| serde_json::from_str(json).ok());

    let words = retrieve::item_words(&item.name, item.note.as_deref(), text.as_deref());
    let mut folders = retrieve::folders_of(&state.db, user).await?;
    // The shares the user placed and may add to are places too.
    folders.extend(retrieve::shared_folders(&state.files, user).await?);
    // With embeddings, the folders nearest to the item join the lexical
    // ones.
    let query = format!(
        "{}\n{}\n{}",
        item.name,
        item.note.as_deref().unwrap_or_default(),
        super::extract::truncate(text.as_deref().unwrap_or_default(), 2_000)
    );
    let semantic = crate::embed::nearest_folders(state, user, &query, 15).await;
    let context = retrieve::context_with(folders, &words, allowed_top.as_deref(), &semantic);
    let examples = examples(state, user).await?;
    let facts = ItemFacts {
        name: &item.name,
        mime_type: item.mime_type.as_deref(),
        size: item.size,
        dropped_at: &item.created_at,
        note: item.note.as_deref(),
        url: item.url.as_deref(),
        text: text.as_deref(),
    };
    let system = propose::system_prompt(settings.folder_freedom, allowed_top.as_deref());
    let mut prompt = propose::user_prompt(&facts, &context, &examples);

    let chosen = crate::settings::ai_model(state, user, crate::ai::Work::Proposals).await?;
    let mut proposal = None;
    let mut problem = String::new();
    for _ in 0..2 {
        let answer = match state
            .ai
            .chat(chosen.as_deref(), &system, &prompt, true)
            .await
        {
            Ok(answer) => answer,
            // Turned off in the meantime: wait for it again.
            Err(AiError::Off) => {
                return Ok(set_status(state, user, id, Status::Waiting, None).await?);
            }
            Err(e) => anyhow::bail!("the model failed: {e}"),
        };
        match propose::parse(
            &answer,
            &item.name,
            &context,
            settings.folder_freedom,
            allowed_top.as_deref(),
        ) {
            Ok(p) => {
                proposal = Some(p);
                break;
            }
            Err(e) => {
                // Once more, told what was wrong.
                prompt.push_str(&format!(
                    "\nYour previous answer could not be used: {e}. Answer again with JSON only.\n"
                ));
                problem = e;
            }
        }
    }
    let Some(proposal) = proposal else {
        let error = format!("The model gave no usable proposal: {problem}");
        return Ok(set_status(state, user, id, Status::NeedsManual, Some(&error)).await?);
    };
    sqlx::query(
        "INSERT INTO proposals (item_id, folder_id, folder_path, new_folders, name, reason,
           confidence, model, created_at)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9)
         ON CONFLICT (item_id) DO UPDATE SET folder_id = ?2, folder_path = ?3, new_folders = ?4,
           name = ?5, reason = ?6, confidence = ?7, model = ?8, created_at = ?9",
    )
    .bind(id)
    .bind(proposal.folder_id)
    .bind(&proposal.folder_path)
    .bind(serde_json::to_string(&proposal.new_folders).expect("names serialize"))
    .bind(&proposal.name)
    .bind(&proposal.reason)
    .bind(proposal.confidence)
    .bind(
        state
            .ai
            .model_chosen(
                crate::ai::Work::Proposals,
                crate::settings::ai_model(state, user, crate::ai::Work::Proposals)
                    .await?
                    .as_deref(),
            )
            .map(|m| m.model)
            .unwrap_or_default(),
    )
    .bind(now_iso())
    .execute(state.db.writer())
    .await?;

    // By the pending mode: placed now or not, accepted now or later; under
    // the review lock, and only if nobody decided the item meanwhile.
    let _lock = super::review::LOCK.lock().await;
    let now: Option<(Status,)> = sqlx::query_as("SELECT status FROM inbox_items WHERE id = ?1")
        .bind(id)
        .fetch_optional(state.db.reader())
        .await?;
    if !matches!(now, Some((Status::Analysing,))) {
        return Ok(());
    }
    // The WebDAV drop box's folders override the pending mode.
    let dav_folder: Option<String> =
        sqlx::query_scalar("SELECT dav_folder FROM inbox_items WHERE id = ?1")
            .bind(id)
            .fetch_one(state.db.reader())
            .await?;
    let (place_now, status) = match (dav_folder.as_deref(), settings.pending_mode) {
        (Some("review"), _) => (false, Status::Proposed),
        (Some("auto"), _) => (true, Status::AutoAccepted),
        (_, mode) => match mode {
            PendingMode::MovePending | PendingMode::Timeout => (true, Status::Proposed),
            PendingMode::Stay => (false, Status::Proposed),
            PendingMode::Confidence => {
                let threshold = settings.confidence_threshold.unwrap_or(0.8);
                if proposal.confidence >= threshold {
                    (true, Status::AutoAccepted)
                } else {
                    (false, Status::Proposed)
                }
            }
        },
    };
    if place_now {
        let target = Target {
            folder_id: proposal.folder_id,
            new_folders: proposal.new_folders.clone(),
            name: proposal.name.clone().unwrap_or_else(|| item.name.clone()),
        };
        if let Err(e) = place::place(state, &item, user, &target).await {
            // The proposal stands; placing it is left to the user.
            let error = format!("The proposal could not be placed: {e}");
            return Ok(set_status(state, user, id, Status::Proposed, Some(&error)).await?);
        }
        if status == Status::Proposed && settings.pending_mode == PendingMode::Timeout {
            let hours = settings.accept_after_hours.unwrap_or(24).max(1) as u64;
            state
                .jobs
                .add(
                    place::ACCEPT,
                    &place::AcceptPayload { item: id },
                    Some(std::time::Duration::from_secs(hours * 3600)),
                )
                .await?;
        }
    }
    Ok(set_status(state, user, id, status, None).await?)
}

/// The user's latest corrections, as examples for the model.
async fn examples(state: &AppState, user: Uuid) -> Result<Vec<Example>, sqlx::Error> {
    let rows: Vec<(String, String, String)> = sqlx::query_as(
        "SELECT summary, proposed, chosen FROM corrections WHERE user_id = ?1
         ORDER BY created_at DESC LIMIT ?2",
    )
    .bind(user)
    .bind(EXAMPLES)
    .fetch_all(state.db.reader())
    .await?;
    Ok(rows
        .into_iter()
        .map(|(summary, proposed, chosen)| Example {
            summary,
            proposed,
            chosen,
        })
        .collect())
}

/// A link made a file in the item's staging folder.
struct StagedLink {
    name: String,
    mime_type: String,
    /// A bookmarked page's text.
    page_text: Option<String>,
}

/// Fetch a link and stage the file it becomes; the item takes its name,
/// type and size. A page that cannot be fetched still makes a bookmark (by
/// its address); a download that fails fails this attempt.
/// A link item's file, made now if analysis has not made it yet (a link
/// placed by hand while it waits for the AI, or one the rules keep from
/// it): what the user's link setting says, as analysis would.
pub async fn ensure_staged(
    state: &AppState,
    user_id: Uuid,
    item: &super::InboxItem,
) -> anyhow::Result<()> {
    if item.kind != ItemKind::Link {
        return Ok(());
    }
    let dir = super::staging_dir(state, user_id, item.id);
    if dir.join(&item.name).is_file() {
        return Ok(());
    }
    let url = item.url.as_deref().unwrap_or_default();
    stage_link(
        state,
        user_id,
        item.id,
        url,
        item.link_mode.unwrap_or(Links::Bookmark),
    )
    .await?;
    Ok(())
}

async fn stage_link(
    state: &AppState,
    user: Uuid,
    id: Uuid,
    url: &str,
    mode: Links,
) -> anyhow::Result<StagedLink> {
    let allow_private = state.links_allow_private;
    let (name, bytes, page_text) = match mode {
        Links::Download => {
            let fetched = link::fetch(url, link::MAX_DOWNLOAD_BYTES, allow_private)
                .await
                .map_err(|e| anyhow::anyhow!("the link cannot be downloaded: {e}"))?;
            (link::download_name(&fetched), fetched.bytes, None)
        }
        // The page saved: self-contained HTML, and its Markdown beside it.
        // A page that cannot be fetched is a bookmark.
        Links::Snapshot => match link::fetch(url, link::MAX_PAGE_BYTES, allow_private).await {
            Ok(fetched) => {
                let page = link::page(&fetched).await;
                let title = page.as_ref().and_then(|p| p.title.clone());
                let snapshot = snapshot::snapshot(&fetched, title.as_deref(), allow_private).await;
                let parsed = reqwest::Url::parse(url)?;
                let bookmark = link::bookmark_name(title.as_deref(), &parsed);
                let stem = bookmark.trim_end_matches(".url");
                let markdown_name = format!("{stem}.md");
                let dir = staging_dir(state, user, id);
                let _ = std::fs::remove_dir_all(&dir);
                std::fs::create_dir_all(&dir)?;
                std::fs::write(dir.join(&markdown_name), snapshot.markdown.as_bytes())?;
                sqlx::query("UPDATE inbox_items SET companions = ?2 WHERE id = ?1")
                    .bind(id)
                    .bind(serde_json::to_string(&[&markdown_name]).expect("names serialize"))
                    .execute(state.db.writer())
                    .await?;
                return finish_link(
                    state,
                    id,
                    format!("{stem}.html"),
                    snapshot.html.into_bytes(),
                    page.map(|p| p.text),
                    false,
                )
                .await;
            }
            Err(e) => {
                tracing::info!("drop box item {id}: {e}");
                let parsed = reqwest::Url::parse(url)?;
                (
                    link::bookmark_name(None, &parsed),
                    link::bookmark(url).into_bytes(),
                    None,
                )
            }
        },
        Links::Bookmark => {
            let (title, text) = match link::fetch(url, link::MAX_PAGE_BYTES, allow_private).await {
                Ok(fetched) => match link::page(&fetched).await {
                    Some(page) => (page.title, Some(page.text)),
                    None => (None, None),
                },
                Err(e) => {
                    tracing::info!("drop box item {id}: {e}");
                    (None, None)
                }
            };
            let parsed = reqwest::Url::parse(url)?;
            (
                link::bookmark_name(title.as_deref(), &parsed),
                link::bookmark(url).into_bytes(),
                text,
            )
        }
    };
    let dir = staging_dir(state, user, id);
    // A file of an earlier attempt goes.
    let _ = std::fs::remove_dir_all(&dir);
    finish_link(state, id, name, bytes, page_text, true).await
}

/// Stage the file a link became (beside any companions already staged,
/// unless `fresh` asks for an empty folder), and give the item its name,
/// type and size.
async fn finish_link(
    state: &AppState,
    id: Uuid,
    name: String,
    bytes: Vec<u8>,
    page_text: Option<String>,
    fresh: bool,
) -> anyhow::Result<StagedLink> {
    let user: Uuid = sqlx::query_scalar("SELECT user_id FROM inbox_items WHERE id = ?1")
        .bind(id)
        .fetch_one(state.db.reader())
        .await?;
    let dir = staging_dir(state, user, id);
    if fresh {
        sqlx::query("UPDATE inbox_items SET companions = '[]' WHERE id = ?1")
            .bind(id)
            .execute(state.db.writer())
            .await?;
    }
    std::fs::create_dir_all(&dir)?;
    std::fs::write(dir.join(&name), &bytes)?;
    let mime_type = super::mime_type(&name);
    sqlx::query(
        "UPDATE inbox_items SET name = ?2, size = ?3, mime_type = ?4, updated_at = ?5 WHERE id = ?1",
    )
    .bind(id)
    .bind(&name)
    .bind(bytes.len() as i64)
    .bind(&mime_type)
    .bind(now_iso())
    .execute(state.db.writer())
    .await?;
    Ok(StagedLink {
        name,
        mime_type,
        page_text,
    })
}

/// What the model reads of a folder: its files (at most 100), and when
/// reading is allowed the start of the first three that have text.
async fn folder_text(root: &std::path::Path, files: &[String], read: bool) -> String {
    let mut out = String::from("Files in it:\n");
    for file in files.iter().take(100) {
        out.push_str(&format!("- {file}\n"));
    }
    if files.len() > 100 {
        out.push_str(&format!("- … and {} more\n", files.len() - 100));
    }
    if read {
        let mut read_count = 0;
        for file in files {
            if read_count == 3 {
                break;
            }
            let mime = super::mime_type(file);
            if !crate::search::readable(&mime) {
                continue;
            }
            if let Ok(text) = extract::text_of_file(&root.join(file), file, &mime, 2_000).await {
                out.push_str(&format!("\n--- {file} ---\n{text}\n"));
                read_count += 1;
            }
        }
    }
    out
}

/// A rule says: nothing is sent, the user places it.
async fn by_hand(state: &AppState, user: Uuid, id: Uuid) -> anyhow::Result<()> {
    set_guard(state, id, Some(RuleAction::ByHand), None).await?;
    Ok(set_status(state, user, id, Status::NeedsManual, None).await?)
}

async fn set_guard(
    state: &AppState,
    id: Uuid,
    guard: Option<RuleAction>,
    text: Option<&str>,
) -> Result<(), sqlx::Error> {
    sqlx::query("UPDATE inbox_items SET guard = ?2, text = ?3 WHERE id = ?1")
        .bind(id)
        .bind(guard)
        .bind(text)
        .execute(state.db.writer())
        .await?;
    Ok(())
}
