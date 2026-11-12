//! What the user does with an item (docs/M2_DESIGN.md §1, step 4): confirm
//! the proposal or choose another place (a correction, an example for the
//! next proposals), reject it (back to the drop box), place it by hand,
//! analyse it again, or undo a placement.
//!
//! One review at a time, and the analysis's own placing waits for it too
//! ([`LOCK`]): two clicks, or a click during an analysis, never place an
//! item twice.

use super::place::{self, Placement, Target};
use super::{ITEM_COLUMNS, InboxItem, Status, set_status};
use crate::auth::now_iso;
use crate::files::FileError;
use crate::state::AppState;
use serde::{Deserialize, Serialize};
use utoipa::ToSchema;
use uuid::Uuid;

/// Held while an item is placed, moved or taken back.
pub static LOCK: tokio::sync::Mutex<()> = tokio::sync::Mutex::const_new(());

/// A place chosen by the user.
#[derive(Debug, Clone, Deserialize, ToSchema)]
pub struct Choice {
    /// The folder; absent (or null) for the top of the tree.
    pub folder_id: Option<Uuid>,
    /// New folders to make under it, each under the one before.
    #[serde(default, deserialize_with = "crate::util::null_as_default")]
    pub new_folders: Vec<String>,
    /// A new name; absent keeps the item's.
    pub name: Option<String>,
}

/// The model's proposal, as shown.
#[derive(Debug, Clone, Serialize, ToSchema)]
pub struct ProposalView {
    /// The existing folder; absent for the top of the tree.
    pub folder_id: Option<Uuid>,
    pub folder_path: String,
    pub new_folders: Vec<String>,
    /// A new name; absent keeps the item's.
    pub name: Option<String>,
    /// Where it puts the item: folders and name.
    pub path: String,
    pub reason: String,
    /// From 0 to 1.
    pub confidence: f64,
    pub model: String,
    pub created_at: String,
}

/// An item's current placement, as shown.
#[derive(Debug, Clone, Serialize, ToSchema)]
pub struct PlacementView {
    /// For `POST /placements/{id}/undo`.
    pub id: Uuid,
    pub node_id: Uuid,
    /// Where the file is now, from the root.
    pub path: Option<String>,
    pub applied_at: String,
}

/// An item with its proposal and placement.
#[derive(Debug, Clone, Serialize, ToSchema)]
pub struct ItemView {
    #[serde(flatten)]
    pub item: InboxItem,
    pub proposal: Option<ProposalView>,
    pub placement: Option<PlacementView>,
}

#[derive(sqlx::FromRow)]
struct ProposalRow {
    folder_id: Option<Uuid>,
    folder_path: String,
    new_folders: String,
    name: Option<String>,
    reason: String,
    confidence: f64,
    model: String,
    created_at: String,
}

impl ProposalRow {
    fn view(self, item_name: &str) -> ProposalView {
        let new_folders: Vec<String> = serde_json::from_str(&self.new_folders).unwrap_or_default();
        let mut parts: Vec<&str> = Vec::new();
        if !self.folder_path.is_empty() {
            parts.push(&self.folder_path);
        }
        parts.extend(new_folders.iter().map(String::as_str));
        parts.push(self.name.as_deref().unwrap_or(item_name));
        ProposalView {
            path: parts.join("/"),
            folder_id: self.folder_id,
            folder_path: self.folder_path,
            new_folders,
            name: self.name,
            reason: self.reason,
            confidence: self.confidence,
            model: self.model,
            created_at: self.created_at,
        }
    }

    fn target(&self, item_name: &str) -> Target {
        Target {
            folder_id: self.folder_id,
            new_folders: serde_json::from_str(&self.new_folders).unwrap_or_default(),
            name: self.name.clone().unwrap_or_else(|| item_name.to_string()),
        }
    }
}

async fn proposal(state: &AppState, item_id: Uuid) -> Result<Option<ProposalRow>, sqlx::Error> {
    sqlx::query_as(
        "SELECT folder_id, folder_path, new_folders, name, reason, confidence, model, created_at
         FROM proposals WHERE item_id = ?1",
    )
    .bind(item_id)
    .fetch_optional(state.db.reader())
    .await
}

/// An item with its proposal and placement.
pub async fn view(state: &AppState, item: InboxItem) -> Result<ItemView, sqlx::Error> {
    let proposal = proposal(state, item.id)
        .await?
        .map(|row| row.view(&item.name));
    let placement = match place::current(state, item.id).await? {
        Some(p) => Some(PlacementView {
            path: {
                let user_id: Uuid =
                    sqlx::query_scalar("SELECT user_id FROM inbox_items WHERE id = ?1")
                        .bind(item.id)
                        .fetch_one(state.db.reader())
                        .await?;
                place::display_path(state, user_id, p.node_id).await
            },
            id: p.id,
            node_id: p.node_id,
            applied_at: p.applied_at,
        }),
        None => None,
    };
    Ok(ItemView {
        item,
        proposal,
        placement,
    })
}

/// A user's item, with its proposal and placement.
pub async fn item_view(state: &AppState, user_id: Uuid, id: Uuid) -> Result<ItemView, FileError> {
    let item = super::item(state, user_id, id).await?;
    Ok(view(state, item).await?)
}

/// A choice as a target: the name checked, the new folders too.
fn target(choice: &Choice, item_name: &str) -> Result<Target, FileError> {
    let name = match choice.name.as_deref().map(str::trim) {
        None | Some("") => item_name.to_string(),
        Some(name) => crate::files::names::new_name(name).map_err(FileError::Invalid)?,
    };
    let new_folders = choice
        .new_folders
        .iter()
        .map(|f| crate::files::names::new_name(f).map_err(FileError::Invalid))
        .collect::<Result<Vec<_>, _>>()?;
    if choice.folder_id.is_none() && new_folders.is_empty() {
        return Err(FileError::Invalid(
            "Choose a folder: an item cannot go at the top".into(),
        ));
    }
    Ok(Target {
        folder_id: choice.folder_id,
        new_folders,
        name,
    })
}

/// A target's path from the root, for a correction.
async fn target_path(state: &AppState, target: &Target) -> String {
    let mut parts = match target.folder_id {
        Some(id) => crate::files::path_of(state.db.reader(), id)
            .await
            .unwrap_or_default(),
        None => Vec::new(),
    };
    parts.extend(target.new_folders.iter().cloned());
    parts.push(target.name.clone());
    parts.join("/")
}

/// Confirm an item's proposal (`choice` absent), or put it somewhere else
/// (`choice`): a pending item, or one placed by the proposal. Another
/// place than proposed is kept as a correction.
pub async fn confirm(
    state: &AppState,
    user_id: Uuid,
    id: Uuid,
    choice: Option<Choice>,
) -> Result<ItemView, FileError> {
    let _lock = LOCK.lock().await;
    let item = super::item(state, user_id, id).await?;
    if !matches!(item.status, Status::Proposed | Status::AutoAccepted) {
        return Err(FileError::Conflict(
            "Only a proposed item can be confirmed".into(),
        ));
    }
    let proposed = proposal(state, id)
        .await?
        .ok_or_else(|| FileError::Conflict("The item has no proposal".into()))?;
    let proposed_target = proposed.target(&item.name);
    let target = match &choice {
        Some(choice) => target(choice, &item.name)?,
        None => proposed_target.clone(),
    };
    let proposed_path = proposed.view(&item.name).path;
    let chosen_path = target_path(state, &target).await;
    let changed = choice.is_some() && !same_path(&chosen_path, &proposed_path);
    match place::current(state, id).await? {
        Some(placement) if changed => {
            place::move_placed(state, user_id, &placement, &target).await?;
        }
        Some(_) => {}
        None => {
            place::place(state, &item, user_id, &target).await?;
        }
    }
    if changed {
        record_correction(state, user_id, &item, &proposed_path, &chosen_path).await?;
    }
    let status = if changed {
        Status::Changed
    } else {
        Status::Confirmed
    };
    set_status(state, user_id, id, status, None).await?;
    item_view(state, user_id, id).await
}

fn same_path(a: &str, b: &str) -> bool {
    let fold = |p: &str| {
        p.split('/')
            .map(crate::files::names::fold)
            .collect::<Vec<_>>()
    };
    fold(a) == fold(b)
}

async fn record_correction(
    state: &AppState,
    user_id: Uuid,
    item: &InboxItem,
    proposed: &str,
    chosen: &str,
) -> Result<(), sqlx::Error> {
    let summary = match &item.note {
        Some(note) => format!(
            "{} (\"{}\")",
            item.name,
            super::extract::truncate(note, 120)
        ),
        None => item.name.clone(),
    };
    sqlx::query(
        "INSERT INTO corrections (id, user_id, summary, proposed, chosen, created_at)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
    )
    .bind(Uuid::now_v7())
    .bind(user_id)
    .bind(summary)
    .bind(proposed)
    .bind(chosen)
    .bind(now_iso())
    .execute(state.db.writer())
    .await?;
    Ok(())
}

/// Place an item by hand: one without a usable proposal, rejected, waiting,
/// failed or undone. With a proposal that is not followed, a correction.
pub async fn place_by_hand(
    state: &AppState,
    user_id: Uuid,
    id: Uuid,
    choice: Choice,
) -> Result<ItemView, FileError> {
    let _lock = LOCK.lock().await;
    let item = super::item(state, user_id, id).await?;
    if !matches!(
        item.status,
        Status::NeedsManual | Status::Rejected | Status::Waiting | Status::Failed | Status::Undone
    ) {
        return Err(FileError::Conflict(
            "This item is being analysed or is placed: confirm or change its proposal instead"
                .into(),
        ));
    }
    // A link waiting for the AI has no file yet.
    super::analyse::ensure_staged(state, user_id, &item)
        .await
        .map_err(FileError::Internal)?;
    let item = super::item(state, user_id, id).await?;
    let target = target(&choice, &item.name)?;
    place::place(state, &item, user_id, &target).await?;
    let chosen = target_path(state, &target).await;
    let proposed = proposal(state, id).await?.map(|p| p.view(&item.name).path);
    let status = match proposed {
        Some(proposed) if !same_path(&chosen, &proposed) => {
            record_correction(state, user_id, &item, &proposed, &chosen).await?;
            Status::Changed
        }
        _ => Status::Confirmed,
    };
    set_status(state, user_id, id, status, None).await?;
    item_view(state, user_id, id).await
}

/// Reject a proposal: the item goes back to the drop box (taken out of the
/// tree if it was placed), to place by hand.
pub async fn reject(state: &AppState, user_id: Uuid, id: Uuid) -> Result<ItemView, FileError> {
    let _lock = LOCK.lock().await;
    let item = super::item(state, user_id, id).await?;
    if !matches!(item.status, Status::Proposed | Status::AutoAccepted) {
        return Err(FileError::Conflict(
            "Only a proposed item can be rejected".into(),
        ));
    }
    if let Some(placement) = place::current(state, id).await? {
        place::take_back(state, user_id, &item, &placement).await?;
    }
    set_status(state, user_id, id, Status::Rejected, None).await?;
    item_view(state, user_id, id).await
}

/// Analyse an item again: one that is not placed and not being analysed.
pub async fn retry(state: &AppState, user_id: Uuid, id: Uuid) -> Result<ItemView, FileError> {
    let _lock = LOCK.lock().await;
    let item = super::item(state, user_id, id).await?;
    if !matches!(
        item.status,
        Status::NeedsManual | Status::Rejected | Status::Waiting | Status::Failed | Status::Undone
    ) {
        return Err(FileError::Conflict(
            "This item is being analysed or is placed".into(),
        ));
    }
    set_status(state, user_id, id, Status::Queued, None).await?;
    super::queue(state, user_id, id).await?;
    item_view(state, user_id, id).await
}

/// Undo a placement: the file goes back to the drop box, wherever it is in
/// the tree now, and the folders made for it go if they are empty.
pub async fn undo(
    state: &AppState,
    user_id: Uuid,
    placement_id: Uuid,
) -> Result<ItemView, FileError> {
    let _lock = LOCK.lock().await;
    let row: Option<(Uuid,)> = sqlx::query_as(
        "SELECT p.item_id FROM placements p JOIN inbox_items i ON i.id = p.item_id
         WHERE p.id = ?1 AND i.user_id = ?2 AND p.undone_at IS NULL",
    )
    .bind(placement_id)
    .bind(user_id)
    .fetch_optional(state.db.reader())
    .await?;
    let (item_id,) = row.ok_or(FileError::NotFound)?;
    let item: InboxItem = sqlx::query_as(sqlx::AssertSqlSafe(format!(
        "SELECT {ITEM_COLUMNS} FROM inbox_items WHERE id = ?1"
    )))
    .bind(item_id)
    .fetch_one(state.db.reader())
    .await?;
    let placement: Placement = place::current(state, item_id)
        .await?
        .ok_or(FileError::NotFound)?;
    place::take_back(state, user_id, &item, &placement).await?;
    set_status(state, user_id, item_id, Status::Undone, None).await?;
    item_view(state, user_id, item_id).await
}
