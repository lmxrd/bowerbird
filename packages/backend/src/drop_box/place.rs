//! Putting an item in the tree, and taking it back (docs/M2_DESIGN.md §1,
//! steps 3 and 4).
//!
//! A placement makes the folders it needs (or uses those of the same name
//! that exist by now), moves the staged file in under a free name, and is
//! recorded in `placements`. An undo moves the file back to the drop box
//! and removes the folders made for it if nothing else is in them.

use super::{InboxItem, ItemKind, Status, set_status, staging_dir};
use crate::auth::now_iso;
use crate::files::ops::WriteTarget;
use crate::files::shares::Need;
use crate::files::{FileError, Topic, path_of};
use crate::jobs::{BoxFuture, Handler, Job};
use crate::state::AppState;
use serde::{Deserialize, Serialize};
use uuid::Uuid;

/// Where an item goes: a folder (`None`: the top of the tree), new folders
/// under it, and its name.
#[derive(Debug, Clone)]
pub struct Target {
    pub folder_id: Option<Uuid>,
    pub new_folders: Vec<String>,
    pub name: String,
}

/// An item's placement in the tree.
#[derive(Debug, Clone, Serialize, utoipa::ToSchema, sqlx::FromRow)]
pub struct Placement {
    pub id: Uuid,
    /// The file in the tree.
    pub node_id: Uuid,
    pub to_parent: Uuid,
    pub to_name: String,
    pub applied_at: String,
}

/// The placement of an item that is not undone.
pub async fn current(state: &AppState, item_id: Uuid) -> Result<Option<Placement>, sqlx::Error> {
    sqlx::query_as(
        "SELECT id, node_id, to_parent, to_name, applied_at FROM placements
         WHERE item_id = ?1 AND undone_at IS NULL ORDER BY applied_at DESC LIMIT 1",
    )
    .bind(item_id)
    .fetch_optional(state.db.reader())
    .await
}

/// A name in a folder that is free: the name, else `name (2).ext`, ...
async fn free_name(state: &AppState, parent: Uuid, name: &str) -> Result<String, FileError> {
    let path = std::path::Path::new(name);
    let (stem, ext) = match (path.file_stem(), path.extension()) {
        (Some(stem), Some(ext)) => (
            stem.to_string_lossy().into_owned(),
            format!(".{}", ext.to_string_lossy()),
        ),
        _ => (name.to_string(), String::new()),
    };
    for n in 1..100 {
        let candidate = if n == 1 {
            name.to_string()
        } else {
            format!("{stem} ({n}){ext}")
        };
        let folded = crate::files::names::fold(&candidate);
        // The shares placed in the folder take their names too.
        let taken: Vec<(String,)> = sqlx::query_as(
            "SELECT name FROM nodes WHERE parent_id = ?1
             UNION ALL SELECT name FROM share_mounts WHERE parent_id = ?1",
        )
        .bind(parent)
        .fetch_all(state.db.reader())
        .await?;
        if !taken
            .iter()
            .any(|(t,)| crate::files::names::fold(t) == folded)
        {
            return Ok(candidate);
        }
    }
    Err(FileError::Conflict(format!("{name} is taken")))
}

/// Whose tree an item goes to, and the folder: the user's own (the top of
/// it by default), or a share they placed in their files and may add to
/// (docs/M6B_DESIGN.md §5), whose owner's tree and limit it then goes to.
/// A share only in "Shared with me" is not a place for the drop box.
async fn tree_of(
    state: &AppState,
    user_id: Uuid,
    folder: Option<Uuid>,
) -> Result<(Uuid, Uuid), FileError> {
    let Some(id) = folder else {
        return Ok((user_id, state.files.root_node(user_id).await?.id));
    };
    let access = state.files.access_for(user_id, id, Need::Create).await?;
    // The drop box never reads or proposes encrypted folders, and cannot
    // write into one: the names and contents there are sealed by devices.
    crate::files::e2e::refuse_encrypted(&access.node, "the drop box")?;
    if !access.own && state.files.mount_over(user_id, id).await?.is_none() {
        return Err(FileError::Forbidden(
            "The drop box puts things only in shares you added to your files".into(),
        ));
    }
    Ok((access.owner, id))
}

/// Put a staged file item in the tree. Answers the placement.
pub async fn place(
    state: &AppState,
    item: &InboxItem,
    user_id: Uuid,
    target: &Target,
) -> Result<Placement, FileError> {
    let files = &state.files;
    let (owner, mut parent) = tree_of(state, user_id, target.folder_id).await?;
    // The folders it needs, made or found.
    let mut created = Vec::new();
    for name in &target.new_folders {
        parent = match files.child_folder(owner, parent, name).await? {
            Some(existing) => existing.id,
            None => {
                let folder = files.create_folder(owner, parent, name).await?;
                created.push(folder.id);
                folder.id
            }
        };
    }
    let staged = staging_dir(state, user_id, item.id).join(&item.name);
    // Into someone else's tree: their storage, so their limit.
    if owner != user_id {
        let size = crate::files::quota::size_on_disk(&staging_dir(state, user_id, item.id));
        if let Err(e) = files.check_room(owner, size).await {
            remove_if_empty(state, &created).await;
            return Err(e);
        }
    }
    let name = free_name(state, parent, &target.name).await?;
    // A folder goes in whole.
    let node = if item.kind == ItemKind::Folder {
        files.import_dir(owner, parent, &name, &staged).await
    } else {
        files
            .place_file(owner, WriteTarget::New { parent, name }, &staged)
            .await
    };
    let node = match node {
        Ok(node) => node,
        Err(e) => {
            remove_if_empty(state, &created).await;
            return Err(e);
        }
    };
    // Who added it: the user whose drop box it came from.
    for id in created.iter().chain([&node.id]) {
        files.mark_created(*id, user_id).await?;
    }
    // Its companions beside it, under its name.
    let companions: (String,) = sqlx::query_as("SELECT companions FROM inbox_items WHERE id = ?1")
        .bind(item.id)
        .fetch_one(state.db.reader())
        .await?;
    let companions: Vec<String> = serde_json::from_str(&companions.0).unwrap_or_default();
    let mut companion_nodes = Vec::new();
    for companion in companions {
        let path = staging_dir(state, user_id, item.id).join(&companion);
        if !path.exists() {
            continue;
        }
        let name = free_name(state, parent, &companion_name(&node.name, &companion)).await?;
        match files
            .place_file(owner, WriteTarget::New { parent, name }, &path)
            .await
        {
            Ok(placed) => companion_nodes.push(placed.id),
            Err(e) => tracing::info!("cannot place {companion} beside {}: {e}", node.name),
        }
    }
    let _ = std::fs::remove_dir(staging_dir(state, user_id, item.id));
    let placement = Placement {
        id: Uuid::now_v7(),
        node_id: node.id,
        to_parent: parent,
        to_name: node.name.clone(),
        applied_at: now_iso(),
    };
    sqlx::query(
        "INSERT INTO placements (id, item_id, node_id, to_parent, to_name, created_folders,
           applied_at, companion_nodes)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8)",
    )
    .bind(placement.id)
    .bind(item.id)
    .bind(node.id)
    .bind(parent)
    .bind(&placement.to_name)
    .bind(serde_json::to_string(&created).expect("ids serialize"))
    .bind(&placement.applied_at)
    .bind(serde_json::to_string(&companion_nodes).expect("ids serialize"))
    .execute(state.db.writer())
    .await?;
    sqlx::query("UPDATE inbox_items SET node_id = ?2, updated_at = ?3 WHERE id = ?1")
        .bind(item.id)
        .bind(node.id)
        .bind(now_iso())
        .execute(state.db.writer())
        .await?;
    Ok(placement)
}

/// Move a placed item elsewhere in the tree (the user changed the place).
pub async fn move_placed(
    state: &AppState,
    user_id: Uuid,
    placement: &Placement,
    target: &Target,
) -> Result<Placement, FileError> {
    let files = &state.files;
    let (owner, mut parent) = tree_of(state, user_id, target.folder_id).await?;
    // A placed file moves within its tree only: from the user's own files
    // to a share (or back) it is taken back and placed anew.
    let now = files.owner_of(placement.node_id).await?;
    if now != owner {
        return Err(FileError::Invalid(
            "Undo it first to put it in another person's shared folder".into(),
        ));
    }
    let mut created = Vec::new();
    for name in &target.new_folders {
        parent = match files.child_folder(owner, parent, name).await? {
            Some(existing) => existing.id,
            None => {
                let folder = files.create_folder(owner, parent, name).await?;
                created.push(folder.id);
                folder.id
            }
        };
    }
    let node = files.node(owner, placement.node_id).await?;
    let name = if node.parent_id == Some(parent) && node.name == target.name {
        target.name.clone()
    } else {
        free_name(state, parent, &target.name).await?
    };
    let moved = files
        .move_node(owner, node.id, Some(parent), Some(&name), node.version)
        .await;
    let moved = match moved {
        Ok(moved) => moved,
        Err(e) => {
            remove_if_empty(state, &created).await;
            return Err(e);
        }
    };
    // Its companions follow it.
    for companion in companion_nodes(state, placement.id).await? {
        let Ok(node) = files.node(owner, companion).await else {
            continue;
        };
        let wanted = companion_name(&moved.name, &node.name);
        if node.parent_id == Some(parent) && node.name == wanted {
            continue;
        }
        let name = free_name(state, parent, &wanted).await?;
        if let Err(e) = files
            .move_node(owner, node.id, Some(parent), Some(&name), node.version)
            .await
        {
            tracing::info!("cannot move {} with {}: {e}", node.name, moved.name);
        }
    }
    // The folders made for the first place go if the move emptied them;
    // the new ones are this placement's.
    let earlier: (String,) = sqlx::query_as("SELECT created_folders FROM placements WHERE id = ?1")
        .bind(placement.id)
        .fetch_one(state.db.reader())
        .await?;
    let earlier: Vec<Uuid> = serde_json::from_str(&earlier.0).unwrap_or_default();
    remove_if_empty(state, &earlier).await;
    sqlx::query(
        "UPDATE placements SET to_parent = ?2, to_name = ?3, created_folders = ?4 WHERE id = ?1",
    )
    .bind(placement.id)
    .bind(parent)
    .bind(&moved.name)
    .bind(serde_json::to_string(&created).expect("ids serialize"))
    .execute(state.db.writer())
    .await?;
    Ok(Placement {
        to_parent: parent,
        to_name: moved.name,
        ..placement.clone()
    })
}

/// Take a placed item back into the drop box, and remove the folders made
/// for it if they are empty. The file is taken from wherever it is now.
pub async fn take_back(
    state: &AppState,
    user_id: Uuid,
    item: &InboxItem,
    placement: &Placement,
) -> Result<(), FileError> {
    let dir = staging_dir(state, user_id, item.id);
    std::fs::create_dir_all(&dir)?;
    // From whichever tree it is in (a share's owner's).
    let owner = state.files.owner_of(placement.node_id).await?;
    state
        .files
        .take_out(owner, placement.node_id, &dir.join(&item.name), None)
        .await?;
    // Its companions too, under their staged names; one that is gone stays
    // gone.
    for companion in companion_nodes(state, placement.id).await? {
        let Ok(node) = state.files.node(owner, companion).await else {
            continue;
        };
        let staged = dir.join(companion_name(&item.name, &node.name));
        if let Err(e) = state.files.take_out(owner, companion, &staged, None).await {
            tracing::info!("cannot take {} back: {e}", node.name);
        }
    }
    let created: (String,) = sqlx::query_as("SELECT created_folders FROM placements WHERE id = ?1")
        .bind(placement.id)
        .fetch_one(state.db.reader())
        .await?;
    let created: Vec<Uuid> = serde_json::from_str(&created.0).unwrap_or_default();
    remove_if_empty(state, &created).await;
    sqlx::query("UPDATE placements SET undone_at = ?2 WHERE id = ?1")
        .bind(placement.id)
        .bind(now_iso())
        .execute(state.db.writer())
        .await?;
    sqlx::query("UPDATE inbox_items SET node_id = NULL, updated_at = ?2 WHERE id = ?1")
        .bind(item.id)
        .bind(now_iso())
        .execute(state.db.writer())
        .await?;
    Ok(())
}

/// Remove folders (the deepest first) that have nothing in them; one that
/// is gone or holds something is left.
async fn remove_if_empty(state: &AppState, folders: &[Uuid]) {
    for id in folders.iter().rev() {
        // In whichever tree it was made (a share's owner's).
        let Ok(user_id) = state.files.owner_of(*id).await else {
            continue;
        };
        let Ok(node) = state.files.node(user_id, *id).await else {
            continue;
        };
        if !state.files.is_empty_folder(*id).await.unwrap_or(false) {
            continue;
        }
        let path = match path_of(state.db.reader(), *id).await {
            Ok(path) => path,
            Err(_) => continue,
        };
        // Through the trash, as any delete: the folder is empty, so
        // nothing is lost, and the index stays in step.
        match state.files.trash(user_id, *id, node.version).await {
            Ok(_) => {
                let _ = state.files.purge(user_id, *id).await;
            }
            Err(e) => tracing::info!("cannot remove the empty folder {}: {e}", path.join("/")),
        }
    }
}

/// The path of a node from the user's root, for display (through the
/// share they placed it in).
pub async fn display_path(state: &AppState, user_id: Uuid, node_id: Uuid) -> Option<String> {
    state
        .files
        .view_path(user_id, node_id)
        .await
        .ok()
        .flatten()
        .map(|p| p.join("/"))
}

/// The `accept` job: a pending item is accepted when its time is up
/// (pending mode `timeout`).
pub const ACCEPT: &str = "accept";

#[derive(Serialize, Deserialize)]
pub struct AcceptPayload {
    pub item: Uuid,
}

pub struct Accept;

impl Handler for Accept {
    fn kind(&self) -> &'static str {
        ACCEPT
    }

    fn run<'a>(&'a self, state: &'a AppState, job: Job) -> BoxFuture<'a, Result<(), String>> {
        Box::pin(async move {
            let AcceptPayload { item } = job.payload()?;
            let row: Option<(Uuid, Status, Option<Uuid>)> =
                sqlx::query_as("SELECT user_id, status, node_id FROM inbox_items WHERE id = ?1")
                    .bind(item)
                    .fetch_optional(state.db.reader())
                    .await
                    .map_err(|e| e.to_string())?;
            // Only an item still pending and still placed.
            if let Some((user, Status::Proposed, Some(_))) = row {
                set_status(state, user, item, Status::AutoAccepted, None)
                    .await
                    .map_err(|e| e.to_string())?;
            }
            Ok(())
        })
    }
}

/// Tell a user's apps that their drop box changed.
pub fn announce(state: &AppState, user_id: Uuid) {
    state.files.announce(user_id, Topic::DropBox);
}

/// A companion's name beside an item: the item's name with the
/// companion's extension (`Bread.html` and `x.md` make `Bread.md`).
pub fn companion_name(item: &str, companion: &str) -> String {
    let stem = std::path::Path::new(item)
        .file_stem()
        .map(|s| s.to_string_lossy().into_owned())
        .unwrap_or_else(|| item.to_string());
    match std::path::Path::new(companion).extension() {
        Some(ext) => format!("{stem}.{}", ext.to_string_lossy()),
        None => stem,
    }
}

async fn companion_nodes(state: &AppState, placement: Uuid) -> Result<Vec<Uuid>, sqlx::Error> {
    let (nodes,): (String,) =
        sqlx::query_as("SELECT companion_nodes FROM placements WHERE id = ?1")
            .bind(placement)
            .fetch_one(state.db.reader())
            .await?;
    Ok(serde_json::from_str(&nodes).unwrap_or_default())
}

#[cfg(test)]
mod tests {
    #[test]
    fn companions_take_the_item_s_name() {
        assert_eq!(
            super::companion_name("Bread.html", "Sourdough.md"),
            "Bread.md"
        );
        assert_eq!(
            super::companion_name("Bread (2).html", "x.md"),
            "Bread (2).md"
        );
        assert_eq!(super::companion_name("README", "x.md"), "README.md");
    }
}
