//! The files API (docs/M1_DESIGN.md §2.2): the tree by id or by path,
//! folders, renames and moves, deletes to the trash, downloads and small
//! uploads. Every change goes through `files::ops`, which journals it.
//!
//! Every handler asks `Files::access` first: a node of the user's own
//! tree, or one shared with them (docs/M6B_DESIGN.md §2), whose operation
//! then runs in its owner's tree, after the share's permission is checked.

use crate::error::{ErrorBody, HttpError};
use crate::extract::ApiJson;
use crate::files::ops::{TrashItem, WriteTarget};
use crate::files::roles::{self, Role};
use crate::files::shares::{Access, Need};
use crate::files::{Kind, NODE_COLUMNS, Node, drives, receive::receive};
use crate::routes::auth::AuthUser;
use crate::state::AppState;
use axum::Json;
use axum::body::Body;
use axum::extract::{Multipart, Path, Query, State};
use axum::http::{HeaderMap, HeaderValue, StatusCode, header};
use axum::response::{IntoResponse, Response};
use serde::{Deserialize, Serialize};
use utoipa::{IntoParams, ToSchema};
use utoipa_axum::router::OpenApiRouter;
use utoipa_axum::routes;
use uuid::Uuid;

/// The most a plain upload (multipart or PUT) may be. Larger files go
/// through the resumable uploads, which have no such limit.
pub const PLAIN_UPLOAD_MAX: u64 = 100_000_000;

pub fn router() -> OpenApiRouter<AppState> {
    OpenApiRouter::new()
        .routes(routes!(root))
        .routes(routes!(by_path))
        .routes(routes!(get_file, rename_or_move, delete_file))
        .routes(routes!(children))
        .routes(routes!(create_folder))
        .routes(routes!(upload_file))
        .routes(routes!(download, replace_content))
        // The plain uploads check their own limit while they stream.
        .layer(axum::extract::DefaultBodyLimit::disable())
}

pub fn trash_router() -> OpenApiRouter<AppState> {
    OpenApiRouter::new()
        .routes(routes!(list_trash, empty_trash))
        .routes(routes!(restore))
        .routes(routes!(purge))
}

/// A file or folder, with its path from the root.
#[derive(Serialize, ToSchema)]
pub struct Entry {
    #[serde(flatten)]
    pub node: Node,
    /// From the root, e.g. `/Documents/Taxes`; `/` for the root. Empty for
    /// something shared with the user that they have not placed in their
    /// files.
    pub path: String,
    /// What the user may do with it.
    pub permissions: Permissions,
}

/// What the user may do with a file or folder, as M6b's permissions (the
/// role, `my_role`, says it all; these stay for the clients that read
/// them).
#[derive(Serialize, ToSchema)]
pub struct Permissions {
    /// In the user's own files.
    pub own: bool,
    /// A share the user placed in their files: renaming, moving and
    /// deleting it change only their entry.
    pub mount: bool,
    pub create: bool,
    pub change: bool,
    pub delete: bool,
    pub share: bool,
}

impl Permissions {
    pub fn of(access: &Access) -> Self {
        Permissions {
            own: access.own,
            mount: access.mount.is_some(),
            create: access.allows(Need::Create),
            change: access.allows(Need::Change),
            delete: access.allows(Need::Delete),
            share: access.allows(Need::Share),
        }
    }
}

pub(crate) async fn entry(
    state: &AppState,
    user_id: Uuid,
    access: &Access,
) -> Result<Entry, HttpError> {
    let path = state
        .files
        .view_path(user_id, access.node.id)
        .await?
        .map(|parts| format!("/{}", parts.join("/")))
        .unwrap_or_default();
    Ok(Entry {
        node: access.view(),
        path,
        permissions: Permissions::of(access),
    })
}

/// A node an operation answered, as the user sees it now: with their role
/// on it (and, for their placed share, where they placed it).
pub(crate) async fn seen(state: &AppState, user: Uuid, node: Node) -> Result<Node, HttpError> {
    Ok(state.files.access(user, node.id).await?.view())
}

/// What the user may do with a node, refused unless it gives `need`.
pub(crate) async fn access(
    state: &AppState,
    user: &AuthUser,
    id: Uuid,
    need: Need,
) -> Result<Access, HttpError> {
    Ok(state.files.access_for(user.0.id, id, need).await?)
}

#[utoipa::path(
    get,
    path = "/root",
    tag = "Files",
    operation_id = "getRoot",
    summary = "The root folder",
    responses(
        (status = 200, description = "The root folder", body = Entry),
        (status = 401, description = "Not signed in", body = ErrorBody),
        (status = 503, description = "The files folder is not available", body = ErrorBody),
    ),
    security(("session" = []), ("api_key" = [])),
)]
async fn root(State(state): State<AppState>, user: AuthUser) -> Result<Json<Entry>, HttpError> {
    let node = state.files.root_node(user.0.id).await?;
    let access = state.files.access(user.0.id, node.id).await?;
    Ok(Json(entry(&state, user.0.id, &access).await?))
}

#[derive(Deserialize, IntoParams)]
#[into_params(parameter_in = Query)]
pub struct PathParams {
    /// From the root, e.g. `/Documents/Taxes`.
    pub path: String,
}

#[utoipa::path(
    get,
    path = "/by-path",
    tag = "Files",
    operation_id = "getFileByPath",
    summary = "A file or folder by its path",
    params(PathParams),
    responses(
        (status = 200, description = "The file or folder", body = Entry),
        (status = 401, description = "Not signed in", body = ErrorBody),
        (status = 404, description = "Not found", body = ErrorBody),
    ),
    security(("session" = []), ("api_key" = [])),
)]
async fn by_path(
    State(state): State<AppState>,
    user: AuthUser,
    Query(params): Query<PathParams>,
) -> Result<Json<Entry>, HttpError> {
    let mut node = state.files.root_node(user.0.id).await?;
    for name in params.path.split('/').filter(|n| !n.is_empty()) {
        // Names are looked up as stored, and in NFC (what a macOS client
        // may send decomposed); into the shares the user placed.
        node = state
            .files
            .child_named(user.0.id, node.id, name)
            .await?
            .ok_or_else(HttpError::not_found)?;
    }
    let access = access(&state, &user, node.id, Need::Read).await?;
    Ok(Json(entry(&state, user.0.id, &access).await?))
}

#[utoipa::path(
    get,
    path = "/{id}",
    tag = "Files",
    operation_id = "getFile",
    summary = "A file or folder",
    params(("id" = Uuid, Path, description = "The file or folder id")),
    responses(
        (status = 200, description = "The file or folder", body = Entry),
        (status = 401, description = "Not signed in", body = ErrorBody),
        (status = 404, description = "Not found", body = ErrorBody),
    ),
    security(("session" = []), ("api_key" = [])),
)]
async fn get_file(
    State(state): State<AppState>,
    user: AuthUser,
    Path(id): Path<Uuid>,
) -> Result<Json<Entry>, HttpError> {
    let access = access(&state, &user, id, Need::Read).await?;
    Ok(Json(entry(&state, user.0.id, &access).await?))
}

#[derive(Deserialize, IntoParams)]
#[into_params(parameter_in = Query)]
pub struct ChildrenParams {
    /// The `next` of the previous page.
    pub after: Option<String>,
    /// 1 to 1000; 200 by default.
    pub limit: Option<u32>,
}

/// A page of a folder's children: folders first, then by name. What the
/// user does not see (a limited folder, others' uploads in an upload-only
/// folder) is not in it.
#[derive(Serialize, ToSchema)]
pub struct Children {
    /// Each with `my_role`.
    pub items: Vec<Node>,
    /// What to send as `after` for the next page; null on the last.
    #[schema(required = true)]
    pub next: Option<String>,
    /// The items of the page that are shares the user placed here.
    pub mounts: Vec<Uuid>,
    /// The items of the page the user shared, with people or by a link.
    pub shared: Vec<Uuid>,
}

/// A folder's children with the shares the user placed in it (`?6`; nil
/// for none), as one set of rows; without what is `hidden` from the user
/// (`?8`, `roles::hidden`).
fn children_sql(hidden: &str) -> String {
    format!(
        "SELECT id, drive_id, parent_id, name, kind, size, mime, version, portable,
           created_at, updated_at, e2e_root, source_id FROM nodes WHERE parent_id = ?1 AND NOT {hidden}
         UNION ALL
         SELECT n.id, n.drive_id, m.parent_id, m.name, n.kind, n.size, n.mime, n.version,
           n.portable, n.created_at, n.updated_at, n.e2e_root, n.source_id
         FROM share_mounts m JOIN nodes n ON n.id = m.node_id
         WHERE m.parent_id = ?1 AND m.user_id = ?6"
    )
}

#[utoipa::path(
    get,
    path = "/{id}/children",
    tag = "Files",
    operation_id = "listChildren",
    summary = "What is in a folder",
    description = "Folders first, then files, each by name; paged.",
    params(("id" = Uuid, Path, description = "The folder id"), ChildrenParams),
    responses(
        (status = 200, description = "A page of the folder's children", body = Children),
        (status = 400, description = "Not a folder, or an invalid page", body = ErrorBody),
        (status = 401, description = "Not signed in", body = ErrorBody),
        (status = 404, description = "Not found", body = ErrorBody),
    ),
    security(("session" = []), ("api_key" = [])),
)]
async fn children(
    State(state): State<AppState>,
    user: AuthUser,
    Path(id): Path<Uuid>,
    Query(params): Query<ChildrenParams>,
) -> Result<Json<Children>, HttpError> {
    let folder = access(&state, &user, id, Need::Read).await?;
    if folder.node.kind != Kind::Folder {
        return Err(HttpError::bad_request("Not a folder"));
    }
    // Shares placed by the user are in the folders of their My drive only.
    let mounts_of = if folder.owner == drives::my(user.0.id) {
        user.0.id
    } else {
        Uuid::nil()
    };
    let hidden = crate::files::roles::hidden(
        "nodes",
        "?8",
        folder.keeper,
        folder.role == crate::files::roles::Role::UploadOnly,
    );
    let children = children_sql(&hidden);
    let limit = params.limit.unwrap_or(200).clamp(1, 1000) as i64;
    // Keyset paging on (folders first, name, id): stable while the folder
    // changes. The cursor is the last item's id.
    let after: Option<(i64, String, Uuid)> = match params.after {
        None => None,
        Some(after) => {
            let after: Uuid = after
                .parse()
                .map_err(|_| HttpError::bad_request("Invalid page"))?;
            let row: Option<(i64, String, Uuid)> = sqlx::query_as(sqlx::AssertSqlSafe(format!(
                "SELECT CASE kind WHEN 'folder' THEN 0 ELSE 1 END, name, id FROM ({children})
                 WHERE id = ?7"
            )))
            .bind(id)
            .bind(0)
            .bind("")
            .bind(Uuid::nil())
            .bind(0)
            .bind(mounts_of)
            .bind(after)
            .bind(user.0.id)
            .fetch_optional(state.db.reader())
            .await?;
            Some(row.ok_or_else(|| HttpError::bad_request("Invalid page"))?)
        }
    };
    let (k, name, last) = after.unwrap_or((-1, String::new(), Uuid::nil()));
    let mut items: Vec<Node> = sqlx::query_as(sqlx::AssertSqlSafe(format!(
        "SELECT {NODE_COLUMNS} FROM ({children})
         WHERE (CASE kind WHEN 'folder' THEN 0 ELSE 1 END, name, id) > (?2, ?3, ?4)
         ORDER BY CASE kind WHEN 'folder' THEN 0 ELSE 1 END, name, id
         LIMIT ?5"
    )))
    .bind(id)
    .bind(k)
    .bind(name)
    .bind(last)
    .bind(limit + 1)
    .bind(mounts_of)
    .bind(Uuid::nil())
    .bind(user.0.id)
    .fetch_all(state.db.reader())
    .await?;
    let next = if items.len() as i64 > limit {
        items.truncate(limit as usize);
        items.last().map(|n| n.id.to_string())
    } else {
        None
    };
    let mounts: Vec<Uuid> = sqlx::query_scalar(
        "SELECT node_id FROM share_mounts WHERE parent_id = ?1 AND user_id = ?2",
    )
    .bind(id)
    .bind(mounts_of)
    .fetch_all(state.db.reader())
    .await?;
    // Each child's role: the folder's, unless it has its own.
    let mut roled = Vec::with_capacity(items.len());
    for item in items {
        let role = if mounts.contains(&item.id) {
            state
                .files
                .access(user.0.id, item.id)
                .await
                .ok()
                .map(|a| a.role)
        } else {
            crate::files::roles::child_role(&state.db, user.0.id, folder.role, item.id).await?
        };
        if let Some(role) = role {
            roled.push(item.with_role(role));
        }
    }
    // What is shared from another server says from which.
    let mut hosts: std::collections::HashMap<Uuid, Option<String>> = Default::default();
    for item in &mut roled {
        let host = match hosts.get(&item.drive_id) {
            Some(host) => host.clone(),
            None => {
                let host = state.files.remote_host(item.drive_id).await?;
                hosts.insert(item.drive_id, host.clone());
                host
            }
        };
        item.remote = host;
    }
    let items = roled;
    let shared: Vec<Uuid> = if folder.own {
        sqlx::query_scalar(
            "SELECT id FROM nodes WHERE parent_id = ?1 AND (
               EXISTS (SELECT 1 FROM grants g WHERE g.node_id = nodes.id AND g.role != 'owner')
               OR EXISTS (SELECT 1 FROM links l WHERE l.node_id = nodes.id))",
        )
        .bind(id)
        .fetch_all(state.db.reader())
        .await?
    } else {
        Vec::new()
    };
    let in_page = |id: &Uuid| items.iter().any(|n| n.id == *id);
    let mounts = mounts.into_iter().filter(in_page).collect();
    let shared = shared.into_iter().filter(in_page).collect();
    Ok(Json(Children {
        items,
        next,
        mounts,
        shared,
    }))
}

#[derive(Deserialize, ToSchema)]
pub struct NewFolder {
    /// Trimmed and stored in NFC; refused when other systems would refuse
    /// it, or when the folder has a name that differs only in case. Inside
    /// an encrypted folder, the name sealed by the device (docs/E2E_SPEC.md).
    pub name: String,
    /// An end-to-end-encrypted folder (docs/E2E_SPEC.md): only in the
    /// user's own files, not inside another one, and only once they have
    /// set up encryption. Its own name stays plain. Its device then posts
    /// its first key (`POST /encryption/folders/{id}/keys`).
    #[serde(default)]
    pub encrypted: bool,
}

#[utoipa::path(
    post,
    path = "/{id}/folders",
    tag = "Files",
    operation_id = "createFolder",
    summary = "Make a folder",
    params(("id" = Uuid, Path, description = "The folder to make it in")),
    request_body(content = NewFolder, content_type = "application/json"),
    responses(
        (status = 201, description = "Made", body = Node),
        (status = 400, description = "An invalid name, or not a folder", body = ErrorBody),
        (status = 401, description = "Not signed in", body = ErrorBody),
        (status = 403, description = "A read-only API key", body = ErrorBody),
        (status = 404, description = "Not found", body = ErrorBody),
        (status = 409, description = "The name is taken", body = ErrorBody),
        (status = 415, description = "The body must be JSON (Content-Type: application/json)", body = ErrorBody),
    ),
    security(("session" = []), ("api_key" = [])),
)]
async fn create_folder(
    State(state): State<AppState>,
    user: AuthUser,
    Path(id): Path<Uuid>,
    ApiJson(body): ApiJson<NewFolder>,
) -> Result<Response, HttpError> {
    let parent = access(&state, &user, id, Need::Create).await?;
    let node = if body.encrypted {
        if !parent.own {
            return Err(HttpError::bad_request(
                "An encrypted folder goes in your own files, not in a shared folder",
            ));
        }
        let env = crate::settings::Env::of_state(&state);
        let allowed = crate::settings::resolve(
            &state.db,
            &env,
            "encryption.allowed",
            crate::settings::For::user(user.0.id),
        )
        .await?;
        if !allowed.bool() {
            return Err(HttpError::new(
                StatusCode::FORBIDDEN,
                "Encrypted folders are turned off for you",
            ));
        }
        crate::routes::encryption::keys_of(&state, user.0.id)
            .await?
            .ok_or_else(|| HttpError::bad_request("Set up encryption first"))?;
        state
            .files
            .create_encrypted_folder(parent.owner, id, &body.name)
            .await?
    } else {
        state
            .files
            .create_folder(parent.owner, id, &body.name)
            .await?
    };
    state.files.mark_created(node.id, user.0.id).await?;
    // In an encrypted folder shared from another server, its id there at
    // once: what goes in it is sealed with it (`organizer-e2e`).
    let node = crate::federation::e2e::with_source(&state.files, node).await;
    let node = seen(&state, user.0.id, node).await?;
    Ok((StatusCode::CREATED, Json(node)).into_response())
}

/// A rename, a move, or both. `if_version` must be the current version.
#[derive(Deserialize, ToSchema)]
pub struct MoveFile {
    pub name: Option<String>,
    /// The folder to move it to.
    pub parent_id: Option<Uuid>,
    pub if_version: i64,
}

#[utoipa::path(
    patch,
    path = "/{id}",
    tag = "Files",
    operation_id = "moveFile",
    summary = "Rename or move a file or folder",
    params(("id" = Uuid, Path, description = "The file or folder id")),
    request_body(content = MoveFile, content_type = "application/json"),
    responses(
        (status = 200, description = "Renamed or moved", body = Node),
        (status = 400, description = "An invalid name or target", body = ErrorBody),
        (status = 401, description = "Not signed in", body = ErrorBody),
        (status = 403, description = "A read-only API key", body = ErrorBody),
        (status = 404, description = "Not found", body = ErrorBody),
        (status = 409, description = "Changed since that version, or the name is taken", body = ErrorBody),
        (status = 415, description = "The body must be JSON (Content-Type: application/json)", body = ErrorBody),
    ),
    security(("session" = []), ("api_key" = [])),
)]
async fn rename_or_move(
    State(state): State<AppState>,
    user: AuthUser,
    Path(id): Path<Uuid>,
    ApiJson(body): ApiJson<MoveFile>,
) -> Result<Json<Node>, HttpError> {
    let moved = state
        .files
        .move_as(
            user.0.id,
            id,
            body.parent_id,
            body.name.as_deref(),
            body.if_version,
        )
        .await?;
    Ok(Json(seen(&state, user.0.id, moved).await?))
}

#[derive(Deserialize, IntoParams)]
#[into_params(parameter_in = Query)]
pub struct VersionParams {
    /// The current version: a delete of something that changed since is a
    /// conflict.
    pub if_version: i64,
}

#[utoipa::path(
    delete,
    path = "/{id}",
    tag = "Files",
    operation_id = "deleteFile",
    summary = "Delete a file or folder",
    description = "It goes to the trash, with what is in it, and can be restored.",
    params(("id" = Uuid, Path, description = "The file or folder id"), VersionParams),
    responses(
        (status = 200, description = "In the trash", body = TrashItem),
        (status = 400, description = "The root folder", body = ErrorBody),
        (status = 401, description = "Not signed in", body = ErrorBody),
        (status = 403, description = "A read-only API key", body = ErrorBody),
        (status = 404, description = "Not found", body = ErrorBody),
        (status = 409, description = "Changed since that version", body = ErrorBody),
    ),
    security(("session" = []), ("api_key" = [])),
)]
async fn delete_file(
    State(state): State<AppState>,
    user: AuthUser,
    Path(id): Path<Uuid>,
    Query(params): Query<VersionParams>,
) -> Result<Json<TrashItem>, HttpError> {
    let node = access(&state, &user, id, Need::Read).await?;
    match state
        .files
        .delete_as(user.0.id, id, params.if_version)
        .await?
    {
        Some(mut trashed) => {
            trashed.my_role = roles::drive_role(state.db.reader(), user.0.id, node.owner).await?;
            Ok(Json(trashed))
        }
        // The user's placed share: out of their files (the share stays in
        // "Shared with me"), nothing of the owner's is deleted or trashed.
        None => Ok(Json(TrashItem {
            id,
            name: node.view().name,
            kind: node.node.kind,
            original_path: state
                .files
                .view_path(user.0.id, id)
                .await
                .ok()
                .flatten()
                .unwrap_or_default()
                .join("/"),
            size: node.node.size,
            deleted_at: crate::auth::now_iso(),
            original_parent_id: node.view().parent_id,
            e2e_root: node.node.e2e_root,
            source_id: None,
            original_parent_source_id: None,
            e2e_root_source_id: None,
            // In no trash: the share stays its owner's.
            my_role: None,
        })),
    }
}

/// A new file, as a multipart form: its content in the part `file`, named
/// by the part's filename.
#[derive(ToSchema)]
#[allow(dead_code)] // Documents the multipart form; read by `Multipart`.
pub struct UploadForm {
    #[schema(format = Binary, content_media_type = "application/octet-stream")]
    file: String,
}

#[utoipa::path(
    post,
    path = "/{id}/files",
    tag = "Files",
    operation_id = "uploadFile",
    summary = "Upload a new file",
    description = "A multipart form with the file in the part `file`, named by its filename. At most 100 MB: larger files go through the resumable uploads. A name that is taken is a conflict: replacing a file is `PUT /files/{id}/content`.",
    params(("id" = Uuid, Path, description = "The folder to put it in")),
    request_body(content = UploadForm, content_type = "multipart/form-data"),
    responses(
        (status = 201, description = "Uploaded", body = Node),
        (status = 400, description = "No file part, an invalid name, or not a folder", body = ErrorBody),
        (status = 401, description = "Not signed in", body = ErrorBody),
        (status = 403, description = "A read-only API key", body = ErrorBody),
        (status = 404, description = "Not found", body = ErrorBody),
        (status = 409, description = "The name is taken", body = ErrorBody),
        (status = 413, description = "Over 100 MB", body = ErrorBody),
    ),
    security(("session" = []), ("api_key" = [])),
)]
async fn upload_file(
    State(state): State<AppState>,
    user: AuthUser,
    Path(id): Path<Uuid>,
    mut form: Multipart,
) -> Result<Response, HttpError> {
    let invalid = || HttpError::bad_request("Expected a multipart form with the file in `file`");
    while let Some(mut field) = form.next_field().await.map_err(|_| invalid())? {
        if field.name() != Some("file") {
            continue;
        }
        let name = field.file_name().map(String::from).ok_or_else(invalid)?;
        let temp = temp_upload(&state)?;
        let written = write_field(&mut field, &temp).await;
        if let Err(e) = written {
            let _ = std::fs::remove_file(&temp);
            return Err(e);
        }
        let parent = match access(&state, &user, id, Need::Create).await {
            Ok(parent) => parent,
            Err(e) => {
                let _ = std::fs::remove_file(&temp);
                return Err(e);
            }
        };
        // Counted against the owner's limit: it is their storage.
        let node = state
            .files
            .write_file(parent.owner, WriteTarget::New { parent: id, name }, &temp)
            .await?;
        state.files.mark_created(node.id, user.0.id).await?;
        let node = seen(&state, user.0.id, node).await?;
        return Ok((StatusCode::CREATED, Json(node)).into_response());
    }
    Err(invalid())
}

/// A new temporary file in the uploads folder.
pub(crate) fn temp_upload(state: &AppState) -> Result<std::path::PathBuf, HttpError> {
    let dir = state.files.uploads_dir();
    std::fs::create_dir_all(&dir).map_err(|e| HttpError::internal(e.to_string()))?;
    Ok(dir.join(format!("plain-{}", Uuid::now_v7())))
}

/// A multipart field into a file, a chunk at a time, at most the plain
/// upload limit.
pub(crate) async fn write_field(
    field: &mut axum::extract::multipart::Field<'_>,
    path: &std::path::Path,
) -> Result<(), HttpError> {
    use tokio::io::AsyncWriteExt;
    let mut file = tokio::fs::File::create(path)
        .await
        .map_err(|e| HttpError::internal(e.to_string()))?;
    let mut written = 0u64;
    while let Some(chunk) = field
        .chunk()
        .await
        .map_err(|_| HttpError::bad_request("The upload broke off"))?
    {
        written += chunk.len() as u64;
        if written > PLAIN_UPLOAD_MAX {
            return Err(crate::files::FileError::TooLarge(PLAIN_UPLOAD_MAX).into());
        }
        file.write_all(&chunk)
            .await
            .map_err(|e| HttpError::internal(e.to_string()))?;
    }
    file.sync_all()
        .await
        .map_err(|e| HttpError::internal(e.to_string()))?;
    Ok(())
}

#[derive(Deserialize, IntoParams)]
#[into_params(parameter_in = Query)]
pub struct DownloadParams {
    /// Shown in the browser (`Content-Disposition: inline`) rather than
    /// saved.
    #[serde(default)]
    pub inline: bool,
}

#[utoipa::path(
    get,
    path = "/{id}/content",
    tag = "Files",
    operation_id = "downloadFile",
    summary = "Download a file",
    description = "With `Range` for a part, and `ETag` (the version) with `If-None-Match`.",
    params(("id" = Uuid, Path, description = "The file id"), DownloadParams),
    responses(
        (status = 200, description = "The file", content_type = "application/octet-stream"),
        (status = 206, description = "The range asked for", content_type = "application/octet-stream"),
        (status = 304, description = "Not changed since that ETag"),
        (status = 400, description = "Not a file", body = ErrorBody),
        (status = 401, description = "Not signed in", body = ErrorBody),
        (status = 404, description = "Not found", body = ErrorBody),
        (status = 416, description = "A range outside the file"),
    ),
    security(("session" = []), ("api_key" = [])),
)]
async fn download(
    State(state): State<AppState>,
    user: AuthUser,
    Path(id): Path<Uuid>,
    Query(params): Query<DownloadParams>,
    headers: HeaderMap,
) -> Result<Response, HttpError> {
    let file = access(&state, &user, id, Need::Read).await?;
    let node = file.view();
    if node.kind != Kind::File {
        return Err(HttpError::bad_request("Not a file"));
    }
    let etag = format!("\"{}\"", node.version);
    if headers
        .get(header::IF_NONE_MATCH)
        .and_then(|v| v.to_str().ok())
        .is_some_and(|v| v.split(',').any(|t| t.trim() == etag || t.trim() == "*"))
    {
        return Ok((StatusCode::NOT_MODIFIED, [(header::ETAG, etag)]).into_response());
    }
    let storage = state.files.storage(file.owner).await?;
    let path = state.files.rel_path(id).await?;
    serve_file(
        &storage,
        &path,
        &node.name,
        node.mime.as_deref(),
        &etag,
        params.inline,
        &headers,
    )
    .await
}

/// The one range of bytes a `Range` header asks for, as `start..end`, in
/// a file of `size` bytes: None for the whole file (no header, one that
/// is not understood, several ranges, or an `If-Range` that is not the
/// current ETag); Err for a range outside the file (416).
fn wanted_range(headers: &HeaderMap, etag: &str, size: u64) -> Result<Option<(u64, u64)>, ()> {
    let Some(range) = headers.get(header::RANGE).and_then(|v| v.to_str().ok()) else {
        return Ok(None);
    };
    if let Some(if_range) = headers.get(header::IF_RANGE).and_then(|v| v.to_str().ok())
        && if_range.trim() != etag
    {
        return Ok(None);
    }
    let Some(spec) = range.trim().strip_prefix("bytes=") else {
        return Ok(None);
    };
    if spec.contains(',') {
        return Ok(None);
    }
    let Some((first, last)) = spec.trim().split_once('-') else {
        return Ok(None);
    };
    let (start, end) = match (first.trim(), last.trim()) {
        ("", "") => return Ok(None),
        // The last n bytes.
        ("", n) => match n.parse::<u64>() {
            Ok(0) => return Err(()),
            Ok(n) => (size.saturating_sub(n), size),
            Err(_) => return Ok(None),
        },
        (a, b) => {
            let Ok(a) = a.parse::<u64>() else {
                return Ok(None);
            };
            let b = match b {
                "" => size,
                b => match b.parse::<u64>() {
                    Ok(b) if b >= a => (b + 1).min(size),
                    _ => return Ok(None),
                },
            };
            (a, b)
        }
    };
    if start >= size {
        return Err(());
    }
    Ok(Some((start, end)))
}

/// A file's bytes as a download, streamed from its drive's storage (the
/// server's disk or a backend): one range when asked for, its type and
/// name, and the headers that keep it from running as a page.
pub(crate) async fn serve_file(
    storage: &crate::storage::Storage,
    path: &str,
    name: &str,
    mime: Option<&str>,
    etag: &str,
    inline: bool,
    headers: &HeaderMap,
) -> Result<Response, HttpError> {
    let (reader, size) = match storage.reader(path).await {
        Ok(found) => found,
        // In the index but not in storage: the next scan settles it.
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
            return Err(HttpError::not_found());
        }
        Err(e) => return Err(crate::files::FileError::from(e).into()),
    };
    let (status, start, end) = match wanted_range(headers, etag, size) {
        Ok(Some((start, end))) => (StatusCode::PARTIAL_CONTENT, start, end),
        Ok(None) => (StatusCode::OK, 0, size),
        Err(()) => {
            return Ok((
                StatusCode::RANGE_NOT_SATISFIABLE,
                [(header::CONTENT_RANGE, format!("bytes */{size}"))],
            )
                .into_response());
        }
    };
    let stream = reader
        .into_bytes_stream(start..end)
        .await
        .map_err(|e| crate::files::FileError::from(std::io::Error::other(e)))?;
    let mut response = Response::new(Body::from_stream(stream));
    *response.status_mut() = status;
    let headers = response.headers_mut();
    headers.insert(header::CONTENT_LENGTH, HeaderValue::from(end - start));
    headers.insert(header::ACCEPT_RANGES, HeaderValue::from_static("bytes"));
    if status == StatusCode::PARTIAL_CONTENT {
        headers.insert(
            header::CONTENT_RANGE,
            HeaderValue::from_str(&format!("bytes {start}-{}/{size}", end - 1)).expect("ascii"),
        );
    }
    if mime.is_none() {
        headers.insert(
            header::CONTENT_TYPE,
            HeaderValue::from_static("application/octet-stream"),
        );
    }
    headers.insert(header::ETAG, HeaderValue::from_str(etag).expect("ascii"));
    headers.insert(
        header::CACHE_CONTROL,
        HeaderValue::from_static("private, no-cache"),
    );
    if let Some(mime) = mime.and_then(|m| HeaderValue::from_str(m).ok()) {
        headers.insert(header::CONTENT_TYPE, mime);
    }
    headers.insert(
        header::CONTENT_DISPOSITION,
        content_disposition(name, inline),
    );
    // A file served from this origin must not run as a page of it.
    headers.insert(
        header::X_CONTENT_TYPE_OPTIONS,
        HeaderValue::from_static("nosniff"),
    );
    headers.insert(
        header::CONTENT_SECURITY_POLICY,
        HeaderValue::from_static("sandbox"),
    );
    Ok(response)
}

/// `attachment` (or `inline`) with the name, ASCII and RFC 5987 UTF-8.
pub(crate) fn content_disposition(name: &str, inline: bool) -> HeaderValue {
    let ascii: String = name
        .chars()
        .map(|c| {
            if c.is_ascii_graphic() && c != '"' && c != '\\' || c == ' ' {
                c
            } else {
                '_'
            }
        })
        .collect();
    let mut encoded = String::new();
    for byte in name.bytes() {
        if byte.is_ascii_alphanumeric() || b"!#$&+-.^_`|~".contains(&byte) {
            encoded.push(byte as char);
        } else {
            encoded.push_str(&format!("%{byte:02X}"));
        }
    }
    let kind = if inline { "inline" } else { "attachment" };
    HeaderValue::from_str(&format!(
        "{kind}; filename=\"{ascii}\"; filename*=UTF-8''{encoded}"
    ))
    .expect("an ascii header")
}

#[utoipa::path(
    put,
    path = "/{id}/content",
    tag = "Files",
    operation_id = "replaceFile",
    summary = "Replace a file's content",
    description = "The body is the new content, at most 100 MB (larger files go through the resumable uploads). `If-Match` with the current ETag (the version) is required: a file that changed since is a conflict.",
    params(("id" = Uuid, Path, description = "The file id")),
    request_body(content = Vec<u8>, content_type = "application/octet-stream"),
    responses(
        (status = 200, description = "Replaced", body = Node),
        (status = 400, description = "Not a file", body = ErrorBody),
        (status = 401, description = "Not signed in", body = ErrorBody),
        (status = 403, description = "A read-only API key", body = ErrorBody),
        (status = 404, description = "Not found", body = ErrorBody),
        (status = 409, description = "Changed since that version", body = ErrorBody),
        (status = 413, description = "Over 100 MB", body = ErrorBody),
        (status = 428, description = "No If-Match", body = ErrorBody),
    ),
    security(("session" = []), ("api_key" = [])),
)]
async fn replace_content(
    State(state): State<AppState>,
    user: AuthUser,
    Path(id): Path<Uuid>,
    headers: HeaderMap,
    body: Body,
) -> Result<Json<Node>, HttpError> {
    let if_version: i64 = headers
        .get(header::IF_MATCH)
        .and_then(|v| v.to_str().ok())
        .and_then(|v| {
            v.trim()
                .trim_start_matches("W/")
                .trim_matches('"')
                .parse()
                .ok()
        })
        .ok_or_else(|| {
            HttpError::new(
                StatusCode::PRECONDITION_REQUIRED,
                "If-Match with the file's current ETag is required",
            )
        })?;
    let file = access(&state, &user, id, Need::Change).await?;
    let temp = temp_upload(&state)?;
    if let Err(e) = receive(body, &temp, false, PLAIN_UPLOAD_MAX).await {
        let _ = std::fs::remove_file(&temp);
        return Err(e.into());
    }
    let node = state
        .files
        .write_file(
            file.owner,
            WriteTarget::Replace {
                id,
                if_version,
                source: Some(user.source()),
            },
            &temp,
        )
        .await?;
    Ok(Json(seen(&state, user.0.id, node).await?))
}

#[utoipa::path(
    get,
    path = "",
    tag = "Files",
    operation_id = "listTrash",
    summary = "What is in the trash",
    description = "A drive's trash (My drive's by default), the most recently deleted first.",
    params(TrashParams),
    responses(
        (status = 200, description = "The trash", body = Vec<TrashItem>),
        (status = 401, description = "Not signed in", body = ErrorBody),
        (status = 404, description = "Not a drive of the user's", body = ErrorBody),
    ),
    security(("session" = []), ("api_key" = [])),
)]
async fn list_trash(
    State(state): State<AppState>,
    user: AuthUser,
    Query(params): Query<TrashParams>,
) -> Result<Json<Vec<TrashItem>>, HttpError> {
    let (drive, role) = trash_drive(&state, &user, params.drive.as_deref()).await?;
    Ok(Json(
        state.files.trash_items_for(drive, user.0.id, role).await?,
    ))
}

/// Which drive's trash.
#[derive(Deserialize, IntoParams)]
#[into_params(parameter_in = Query)]
pub struct TrashParams {
    /// The drive's id; My drive's when left out.
    pub drive: Option<String>,
}

/// A drive of the user's whose trash is asked for (My drive by default),
/// with their role on it; another is not found.
async fn trash_drive(
    state: &AppState,
    user: &AuthUser,
    drive: Option<&str>,
) -> Result<(Uuid, Role), HttpError> {
    let drive = match drive {
        None => drives::my(user.0.id),
        Some(drive) => drive.parse().map_err(|_| HttpError::not_found())?,
    };
    let role = roles::drive_role(state.db.reader(), user.0.id, drive)
        .await?
        .ok_or_else(HttpError::not_found)?;
    Ok((drive, role))
}

/// The drive of a trash item the user sees, and their role on the drive.
async fn drive_of_trashed(
    state: &AppState,
    user: &AuthUser,
    id: Uuid,
) -> Result<(Uuid, Role), HttpError> {
    let drive: Uuid = sqlx::query_scalar("SELECT drive_id FROM trash WHERE id = ?1")
        .bind(id)
        .fetch_optional(state.db.reader())
        .await?
        .ok_or_else(HttpError::not_found)?;
    let role = roles::drive_role(state.db.reader(), user.0.id, drive)
        .await?
        .ok_or_else(HttpError::not_found)?;
    if !state
        .files
        .trash_items_for(drive, user.0.id, role)
        .await?
        .iter()
        .any(|item| item.id == id)
    {
        return Err(HttpError::not_found());
    }
    Ok((drive, role))
}

/// Deleting for good is the owner's (a shared drive's managers'): refused
/// for the drive's other users.
fn may_purge(role: Role) -> Result<(), HttpError> {
    if role >= Role::Manager {
        return Ok(());
    }
    Err(HttpError::new(
        StatusCode::FORBIDDEN,
        "Only the owner (in a shared drive, a manager) deletes for good",
    ))
}

/// What emptying the trash did.
#[derive(Serialize, ToSchema)]
pub struct Emptied {
    /// How many items were deleted for good.
    pub deleted: usize,
}

#[utoipa::path(
    delete,
    path = "",
    tag = "Files",
    operation_id = "emptyTrash",
    summary = "Empty the trash",
    description = "Everything in a drive's trash (My drive's by default) is deleted for good, with the versions that went with it.",
    params(TrashParams),
    responses(
        (status = 200, description = "Emptied", body = Emptied),
        (status = 401, description = "Not signed in", body = ErrorBody),
        (status = 403, description = "A read-only API key", body = ErrorBody),
    ),
    security(("session" = []), ("api_key" = [])),
)]
async fn empty_trash(
    State(state): State<AppState>,
    user: AuthUser,
    Query(params): Query<TrashParams>,
) -> Result<Json<Emptied>, HttpError> {
    let (drive, role) = trash_drive(&state, &user, params.drive.as_deref()).await?;
    may_purge(role)?;
    Ok(Json(Emptied {
        deleted: state.files.empty_trash(drive).await?,
    }))
}

#[utoipa::path(
    post,
    path = "/{id}/restore",
    tag = "Files",
    operation_id = "restoreFromTrash",
    summary = "Restore from the trash",
    description = "Back where it was, with its id; into the root when that folder is gone.",
    params(("id" = Uuid, Path, description = "The trash item id")),
    responses(
        (status = 200, description = "Restored", body = Node),
        (status = 401, description = "Not signed in", body = ErrorBody),
        (status = 403, description = "A read-only API key", body = ErrorBody),
        (status = 404, description = "Not found", body = ErrorBody),
        (status = 409, description = "The name is taken there", body = ErrorBody),
    ),
    security(("session" = []), ("api_key" = [])),
)]
async fn restore(
    State(state): State<AppState>,
    user: AuthUser,
    Path(id): Path<Uuid>,
) -> Result<Json<Node>, HttpError> {
    let (drive, role) = drive_of_trashed(&state, &user, id).await?;
    if role < Role::Editor {
        return Err(HttpError::new(
            StatusCode::FORBIDDEN,
            "Restoring takes an editor or above",
        ));
    }
    let node = state.files.restore(drive, id).await?;
    Ok(Json(seen(&state, user.0.id, node).await?))
}

#[utoipa::path(
    delete,
    path = "/{id}",
    tag = "Files",
    operation_id = "purgeFromTrash",
    summary = "Delete from the trash for good",
    params(("id" = Uuid, Path, description = "The trash item id")),
    responses(
        (status = 204, description = "Deleted for good"),
        (status = 401, description = "Not signed in", body = ErrorBody),
        (status = 403, description = "A read-only API key", body = ErrorBody),
        (status = 404, description = "Not found", body = ErrorBody),
    ),
    security(("session" = []), ("api_key" = [])),
)]
async fn purge(
    State(state): State<AppState>,
    user: AuthUser,
    Path(id): Path<Uuid>,
) -> Result<StatusCode, HttpError> {
    let (drive, role) = drive_of_trashed(&state, &user, id).await?;
    may_purge(role)?;
    state.files.purge(drive, id).await?;
    Ok(StatusCode::NO_CONTENT)
}
