//! Resumable uploads: draft-ietf-httpbis-resumable-upload at interop
//! version 6, the one iOS URLSession speaks (docs/M1_DESIGN.md §2.4).
//!
//! An upload is created with a POST that may carry the first bytes (or all
//! of them, with `Upload-Complete: ?1`), and continued with PATCHes at the
//! offset a HEAD reports. What was received is kept if a request breaks
//! off: the client asks for the offset and goes on from there. Once
//! complete, the file is moved into the tree whole (`Files::place_file`),
//! or, for an upload to the drop box, becomes an item as a multipart drop
//! would (`drop_box::add_file`).
//!
//! No 104 (Upload Resumption Supported) is sent: hyper, under axum, cannot
//! send 1xx responses. The draft makes it optional; our clients know the
//! server supports resumption. URLSession's automatic resumption waits for
//! a 104, so the iOS app runs the protocol itself until hyper can send one.

use crate::drop_box::{self, InboxItem};
use crate::error::{ErrorBody, HttpError};
use crate::files::ops::WriteTarget;
use crate::files::receive::receive;
use crate::files::shares::Need;
use crate::files::{FileError, Files, Node};
use crate::routes::auth::AuthUser;
use crate::state::AppState;
use axum::Json;
use axum::body::Body;
use axum::extract::{Path, Query, State};
use axum::http::{HeaderMap, HeaderName, HeaderValue, StatusCode, header};
use axum::response::{IntoResponse, Response};
use serde::{Deserialize, Serialize};
use std::sync::Arc;
use std::time::{Duration, SystemTime};
use utoipa::{IntoParams, ToSchema};
use utoipa_axum::router::OpenApiRouter;
use utoipa_axum::routes;
use uuid::Uuid;

/// The draft's interop version this server speaks.
pub const INTEROP_VERSION: i64 = 6;
/// How long an upload waits for its next bytes.
const LIFETIME: Duration = Duration::from_secs(24 * 3600);

static UPLOAD_OFFSET: HeaderName = HeaderName::from_static("upload-offset");
static UPLOAD_COMPLETE: HeaderName = HeaderName::from_static("upload-complete");
static UPLOAD_LIMIT: HeaderName = HeaderName::from_static("upload-limit");
static UPLOAD_LENGTH: HeaderName = HeaderName::from_static("upload-length");
static INTEROP: HeaderName = HeaderName::from_static("upload-draft-interop-version");

pub fn router() -> OpenApiRouter<AppState> {
    OpenApiRouter::new()
        .routes(routes!(create))
        .routes(routes!(offset, append, cancel))
        // An upload has no size limit but the disk.
        .layer(axum::extract::DefaultBodyLimit::disable())
}

#[derive(sqlx::FromRow)]
struct Upload {
    id: Uuid,
    parent_id: Option<Uuid>,
    name: Option<String>,
    replaces: Option<Uuid>,
    if_version: Option<i64>,
    source: Option<crate::files::versions::Source>,
    length: Option<i64>,
    offset: i64,
    complete: bool,
    drop_box: bool,
    note: Option<String>,
    read_contents: Option<bool>,
}

impl Upload {
    fn target(&self) -> WriteTarget {
        match (self.replaces, self.if_version) {
            (Some(id), Some(if_version)) => WriteTarget::Replace {
                id,
                if_version,
                source: self.source,
            },
            _ => WriteTarget::New {
                parent: self.parent_id.unwrap_or_default(),
                name: self.name.clone().unwrap_or_default(),
            },
        }
    }
}

const COLUMNS: &str = r#"id, parent_id, name, replaces, if_version, source, length, "offset", complete = 1 AS complete, drop_box = 1 AS drop_box, note, read_contents"#;

/// What a complete upload made: the file in the tree, or the drop-box item.
#[derive(Serialize, ToSchema)]
#[serde(untagged)]
pub enum Uploaded {
    /// An upload into a folder, or one that replaced a file.
    File(Node),
    /// An upload to the drop box (`drop_box=true`).
    DropBoxItem(InboxItem),
}

fn part_path(files: &Files, id: Uuid) -> std::path::PathBuf {
    files.uploads_dir().join(format!("{id}.part"))
}

/// A structured-field boolean (`?1`, `?0`).
fn boolean(headers: &HeaderMap, name: &HeaderName) -> Option<bool> {
    match headers.get(name)?.to_str().ok()?.trim() {
        "?1" => Some(true),
        "?0" => Some(false),
        _ => None,
    }
}

/// A structured-field integer.
fn integer(headers: &HeaderMap, name: &HeaderName) -> Option<i64> {
    headers
        .get(name)?
        .to_str()
        .ok()?
        .trim()
        .parse()
        .ok()
        .filter(|n: &i64| *n >= 0)
}

/// The headers of every answer about an active upload.
fn upload_headers(offset: i64, complete: bool) -> [(HeaderName, HeaderValue); 4] {
    [
        (UPLOAD_OFFSET.clone(), HeaderValue::from(offset)),
        (
            UPLOAD_COMPLETE.clone(),
            HeaderValue::from_static(if complete { "?1" } else { "?0" }),
        ),
        (
            UPLOAD_LIMIT.clone(),
            HeaderValue::from_str(&format!("expires={}", LIFETIME.as_secs())).expect("ascii"),
        ),
        (INTEROP.clone(), HeaderValue::from(INTEROP_VERSION)),
    ]
}

fn expires_at() -> String {
    crate::auth::iso(SystemTime::now() + LIFETIME)
}

/// The upload's lock: one request at a time per upload.
fn lock(files: &Files, id: Uuid) -> Arc<tokio::sync::Mutex<()>> {
    files
        .upload_locks
        .lock()
        .unwrap()
        .entry(id)
        .or_default()
        .clone()
}

/// The final size, from `Upload-Length` (later drafts) or, on a request
/// that completes the upload, the offset plus `Content-Length`.
fn length(headers: &HeaderMap, offset: i64, complete: bool) -> Option<i64> {
    integer(headers, &UPLOAD_LENGTH).or_else(|| {
        complete
            .then(|| integer(headers, &header::CONTENT_LENGTH))
            .flatten()
            .map(|n| offset + n)
    })
}

#[derive(Deserialize, IntoParams)]
#[into_params(parameter_in = Query)]
pub struct CreateParams {
    /// A new file: the folder to put it in, with `name`.
    pub parent_id: Option<Uuid>,
    /// A new file: its name.
    pub name: Option<String>,
    /// Or the file it replaces, with `if_version`.
    pub replaces: Option<Uuid>,
    /// The version of `replaces` it replaces: a file that changed since is
    /// a conflict when the upload completes.
    pub if_version: Option<i64>,
    /// Or `true` for the drop box, with `name`: the file becomes a drop-box
    /// item when complete, as one dropped with `POST /drop-box/files`.
    pub drop_box: Option<bool>,
    /// The drop box only: what to tell the model about it, at most 2,000
    /// characters.
    pub note: Option<String>,
    /// The drop box only: whether its contents may be read, where the
    /// user's settings leave it to each item (`per_item`, `default_on`).
    pub read_contents: Option<bool>,
}

/// Where an upload goes, once the request is checked.
struct Target {
    /// Whose tree it goes to (a share's owner), whose limit it counts on.
    owner: Uuid,
    parent: Option<Uuid>,
    name: Option<String>,
    drop_box: bool,
    note: Option<String>,
}

#[utoipa::path(
    post,
    path = "",
    tag = "Uploads",
    operation_id = "createUpload",
    summary = "Start a resumable upload",
    description = "draft-ietf-httpbis-resumable-upload, interop version 6. The target is a new file (`parent_id` and `name`), a file to replace (`replaces` and `if_version`), or the drop box (`drop_box=true` and `name`, optionally `note` and `read_contents`): the file then becomes a drop-box item, as with `POST /drop-box/files` but without its size limit. The body is the first bytes, possibly all or none. `Upload-Complete: ?1` with the whole file answers 201 with what was made (the file, or the drop-box item); `?0` answers 201 with `Location` (the upload), `Upload-Offset` and `Upload-Complete: ?0`: continue with PATCH. A request that breaks off keeps what arrived; ask HEAD for the offset. No 104 is sent.",
    params(CreateParams,
        ("Upload-Complete" = String, Header, description = "`?1` if this request carries the whole file, else `?0`"),
    ),
    request_body(content = Vec<u8>, content_type = "application/octet-stream"),
    responses(
        (status = 201, description = "Complete (with the file, or the drop-box item), or created and waiting for more (`Location`)", body = Uploaded),
        (status = 400, description = "No target or more than one, an invalid name, a note too long, or no `Upload-Complete`", body = ErrorBody),
        (status = 401, description = "Not signed in", body = ErrorBody),
        (status = 403, description = "A read-only API key", body = ErrorBody),
        (status = 404, description = "The folder or the file is not found", body = ErrorBody),
        (status = 409, description = "The name is taken, or the file changed since that version", body = ErrorBody),
    ),
    security(("session" = []), ("api_key" = [])),
)]
async fn create(
    State(state): State<AppState>,
    user: AuthUser,
    Query(params): Query<CreateParams>,
    headers: HeaderMap,
    body: Body,
) -> Result<Response, HttpError> {
    let user_id = user.0.id;
    let complete = boolean(&headers, &UPLOAD_COMPLETE)
        .ok_or_else(|| HttpError::bad_request("Upload-Complete (?0 or ?1) is required"))?;
    if headers.contains_key(&UPLOAD_OFFSET) {
        return Err(HttpError::bad_request("No Upload-Offset on creation"));
    }
    let target = check_target(&state, user_id, &params).await?;
    // A length given up front is checked against the storage limit now,
    // before any byte is sent for nothing.
    if let Some(length) = length(&headers, 0, complete) {
        state
            .files
            .check_room(target.owner, length.max(0) as u64)
            .await?;
    }

    let id = Uuid::now_v7();
    std::fs::create_dir_all(state.files.uploads_dir())
        .map_err(|e| HttpError::internal(e.to_string()))?;
    sqlx::query(
        r#"INSERT INTO uploads (id, user_id, parent_id, name, replaces, if_version, length,
             expires_at, source, drop_box, note, read_contents)
           VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12)"#,
    )
    .bind(id)
    .bind(user_id)
    .bind(target.parent)
    .bind(target.name)
    .bind(params.replaces)
    .bind(params.if_version)
    .bind(length(&headers, 0, complete))
    .bind(expires_at())
    .bind(user.source())
    .bind(target.drop_box)
    .bind(target.note)
    .bind(params.read_contents)
    .execute(state.db.writer())
    .await?;
    let guard = lock(&state.files, id);
    let _held = guard.lock().await;
    let upload = load(&state, user_id, id).await?;
    receive_into(&state, &upload, body, complete, false).await
}

/// The target of a new upload, checked now so that nothing is uploaded for
/// a name that is taken or invalid; a file's is checked again when it is
/// placed.
async fn check_target(
    state: &AppState,
    user_id: Uuid,
    params: &CreateParams,
) -> Result<Target, HttpError> {
    let drop_box = params.drop_box.unwrap_or(false);
    if !drop_box && (params.note.is_some() || params.read_contents.is_some()) {
        return Err(HttpError::bad_request(
            "note and read_contents are for the drop box (drop_box=true) only",
        ));
    }
    match params {
        // The drop box: a name, and the per-item options of a multipart drop.
        CreateParams {
            name: Some(name),
            parent_id: None,
            replaces: None,
            if_version: None,
            ..
        } if drop_box => {
            let name = crate::files::names::new_name(name).map_err(HttpError::bad_request)?;
            // A client's own file (`.DS_Store`, `._*`) makes no item in a
            // multipart drop: here it would make none either.
            if crate::dav_inbox::ignored(&name) {
                return Err(HttpError::bad_request(
                    "A client's own file (.DS_Store, ._*) is not taken",
                ));
            }
            Ok(Target {
                owner: user_id,
                parent: None,
                name: Some(name),
                drop_box: true,
                note: drop_box::clean_note(params.note.clone())?,
            })
        }
        _ if drop_box => Err(HttpError::bad_request(
            "drop_box=true takes a name, and no parent_id, replaces or if_version",
        )),
        CreateParams {
            replaces: Some(id),
            if_version: Some(_),
            ..
        } => {
            let file = state.files.access_for(user_id, *id, Need::Change).await?;
            if file.node.kind != crate::files::Kind::File {
                return Err(HttpError::bad_request("Not a file"));
            }
            Ok(Target {
                owner: file.owner,
                parent: None,
                name: None,
                drop_box: false,
                note: None,
            })
        }
        CreateParams {
            parent_id: Some(parent),
            name: Some(name),
            replaces: None,
            if_version: None,
            ..
        } => {
            let name = crate::files::names::new_name(name).map_err(HttpError::bad_request)?;
            let folder = state
                .files
                .access_for(user_id, *parent, Need::Create)
                .await?;
            if folder.node.kind != crate::files::Kind::Folder {
                return Err(HttpError::bad_request("Not a folder"));
            }
            state.files.check_free(*parent, &name, None).await?;
            Ok(Target {
                owner: folder.owner,
                parent: Some(*parent),
                name: Some(name),
                drop_box: false,
                note: None,
            })
        }
        _ => Err(HttpError::bad_request(
            "Expected parent_id and name, replaces and if_version, or drop_box=true and name",
        )),
    }
}

#[utoipa::path(
    head,
    path = "/{id}",
    tag = "Uploads",
    operation_id = "getUploadOffset",
    summary = "Where an upload is",
    description = "Answers 204 with `Upload-Offset` (the bytes received) and `Upload-Complete`: continue with a PATCH at that offset.",
    params(("id" = Uuid, Path, description = "The upload id")),
    responses(
        (status = 204, description = "The offset, in `Upload-Offset`"),
        (status = 400, description = "Upload-Offset or Upload-Complete sent"),
        (status = 401, description = "Not signed in"),
        (status = 404, description = "No such upload in progress"),
    ),
    security(("session" = []), ("api_key" = [])),
)]
async fn offset(
    State(state): State<AppState>,
    user: AuthUser,
    Path(id): Path<Uuid>,
    headers: HeaderMap,
) -> Result<Response, HttpError> {
    if headers.contains_key(&UPLOAD_OFFSET) || headers.contains_key(&UPLOAD_COMPLETE) {
        return Err(HttpError::bad_request(
            "No Upload-Offset or Upload-Complete here",
        ));
    }
    let upload = load(&state, user.0.id, id).await?;
    Ok((
        StatusCode::NO_CONTENT,
        upload_headers(upload.offset, upload.complete),
        [(header::CACHE_CONTROL, HeaderValue::from_static("no-store"))],
    )
        .into_response())
}

#[utoipa::path(
    patch,
    path = "/{id}",
    tag = "Uploads",
    operation_id = "appendToUpload",
    summary = "Continue an upload",
    description = "`Content-Type: application/partial-upload`, `Upload-Offset` (the offset HEAD reported) and `Upload-Complete` (`?1` if these are the last bytes). Answers 204 with the new offset, or 201 with the file when complete. Another offset is a 409 with the right one in `Upload-Offset`.",
    params(("id" = Uuid, Path, description = "The upload id"),
        ("Upload-Offset" = i64, Header, description = "Where these bytes start"),
        ("Upload-Complete" = String, Header, description = "`?1` if these are the last bytes, else `?0`"),
    ),
    request_body(content = Vec<u8>, content_type = "application/partial-upload"),
    responses(
        (status = 201, description = "Complete: the file, or the drop-box item", body = Uploaded),
        (status = 204, description = "Received; waiting for more"),
        (status = 400, description = "Missing headers, or the upload is complete already", body = ErrorBody),
        (status = 401, description = "Not signed in", body = ErrorBody),
        (status = 404, description = "No such upload in progress", body = ErrorBody),
        (status = 409, description = "Another offset; the right one is in `Upload-Offset`", body = ErrorBody),
        (status = 415, description = "Not application/partial-upload, or encoded", body = ErrorBody),
    ),
    security(("session" = []), ("api_key" = [])),
)]
async fn append(
    State(state): State<AppState>,
    user: AuthUser,
    Path(id): Path<Uuid>,
    headers: HeaderMap,
    body: Body,
) -> Result<Response, HttpError> {
    let content_type = headers
        .get(header::CONTENT_TYPE)
        .and_then(|v| v.to_str().ok())
        .unwrap_or("");
    if !content_type.starts_with("application/partial-upload")
        || headers.contains_key(header::CONTENT_ENCODING)
    {
        return Err(HttpError::new(
            StatusCode::UNSUPPORTED_MEDIA_TYPE,
            "Expected Content-Type: application/partial-upload, without Content-Encoding",
        ));
    }
    let (Some(at), Some(complete)) = (
        integer(&headers, &UPLOAD_OFFSET),
        boolean(&headers, &UPLOAD_COMPLETE),
    ) else {
        return Err(HttpError::bad_request(
            "Upload-Offset and Upload-Complete are required",
        ));
    };
    let guard = lock(&state.files, id);
    let _held = guard.lock().await;
    let mut upload = load(&state, user.0.id, id).await?;
    if upload.complete {
        return Ok((
            StatusCode::BAD_REQUEST,
            upload_headers(upload.offset, true),
            Json(ErrorBody::new("The upload is complete already")),
        )
            .into_response());
    }
    if at != upload.offset {
        return Ok((
            StatusCode::CONFLICT,
            upload_headers(upload.offset, false),
            Json(ErrorBody::new(format!(
                "The upload is at offset {}, not {at}",
                upload.offset
            ))),
        )
            .into_response());
    }
    if let Some(length) = length(&headers, at, complete) {
        if upload.length.is_some_and(|l| l != length) {
            return Err(HttpError::bad_request(
                "The length differs from the one given before",
            ));
        }
        state
            .files
            .check_room(user.0.id, length.max(0) as u64)
            .await?;
        upload.length = Some(length);
        sqlx::query("UPDATE uploads SET length = ?1 WHERE id = ?2")
            .bind(length)
            .bind(id)
            .execute(state.db.writer())
            .await?;
    }
    receive_into(&state, &upload, body, complete, true).await
}

#[utoipa::path(
    delete,
    path = "/{id}",
    tag = "Uploads",
    operation_id = "cancelUpload",
    summary = "Cancel an upload",
    params(("id" = Uuid, Path, description = "The upload id")),
    responses(
        (status = 204, description = "Cancelled; what was received is gone"),
        (status = 400, description = "Upload-Offset or Upload-Complete sent", body = ErrorBody),
        (status = 401, description = "Not signed in", body = ErrorBody),
        (status = 404, description = "No such upload in progress", body = ErrorBody),
    ),
    security(("session" = []), ("api_key" = [])),
)]
async fn cancel(
    State(state): State<AppState>,
    user: AuthUser,
    Path(id): Path<Uuid>,
    headers: HeaderMap,
) -> Result<StatusCode, HttpError> {
    if headers.contains_key(&UPLOAD_OFFSET) || headers.contains_key(&UPLOAD_COMPLETE) {
        return Err(HttpError::bad_request(
            "No Upload-Offset or Upload-Complete here",
        ));
    }
    let guard = lock(&state.files, id);
    let _held = guard.lock().await;
    load(&state, user.0.id, id).await?;
    forget(&state.files, id).await?;
    Ok(StatusCode::NO_CONTENT)
}

async fn load(state: &AppState, user_id: Uuid, id: Uuid) -> Result<Upload, HttpError> {
    let mut upload: Upload = sqlx::query_as(sqlx::AssertSqlSafe(format!(
        "SELECT {COLUMNS} FROM uploads WHERE id = ?1 AND user_id = ?2"
    )))
    .bind(id)
    .bind(user_id)
    .fetch_optional(state.db.reader())
    .await?
    .ok_or_else(HttpError::not_found)?;
    // The bytes on disk are the offset: when a client goes away mid-request,
    // hyper drops the handler, which then never writes the row's offset, but
    // what arrived until then was written. (Under the upload's lock, as in
    // PATCH, they are final; a HEAD may see them grow, and the PATCH that
    // follows is then told the right offset.)
    if let Ok(metadata) = std::fs::metadata(part_path(&state.files, id)) {
        upload.offset = metadata.len() as i64;
    }
    Ok(upload)
}

/// Receive a request's bytes at the end of the upload, then answer: the
/// file when complete, the offset otherwise. A request that breaks off
/// keeps what arrived.
async fn receive_into(
    state: &AppState,
    upload: &Upload,
    body: Body,
    complete: bool,
    append: bool,
) -> Result<Response, HttpError> {
    let path = part_path(&state.files, upload.id);
    let received = receive(body, &path, append, u64::MAX).await;
    // What is on disk is what was received, whatever the outcome.
    let offset = std::fs::metadata(&path)
        .map(|m| m.len() as i64)
        .unwrap_or(0);
    sqlx::query(r#"UPDATE uploads SET "offset" = ?1, expires_at = ?2 WHERE id = ?3"#)
        .bind(offset)
        .bind(expires_at())
        .bind(upload.id)
        .execute(state.db.writer())
        .await?;
    received?;

    if !complete {
        let mut response = (
            if append {
                StatusCode::NO_CONTENT
            } else {
                StatusCode::CREATED
            },
            upload_headers(offset, false),
        )
            .into_response();
        if !append {
            response.headers_mut().insert(
                header::LOCATION,
                HeaderValue::from_str(&format!("/api/uploads/{}", upload.id)).expect("ascii"),
            );
        }
        return Ok(response);
    }
    if upload.length.is_some_and(|l| l != offset) {
        return Err(HttpError::bad_request(format!(
            "Received {offset} bytes, not the {} announced",
            upload.length.unwrap_or_default()
        )));
    }
    let user_id: Uuid = sqlx::query_scalar("SELECT user_id FROM uploads WHERE id = ?1")
        .bind(upload.id)
        .fetch_one(state.db.reader())
        .await?;
    if upload.drop_box {
        // As a multipart drop: the bytes move into the item's staging
        // folder, or are removed when it cannot be made, so the upload is
        // done with either way.
        let made = drop_box::add_file(
            state,
            user_id,
            upload.name.as_deref().unwrap_or_default(),
            &path,
            upload.note.clone(),
            upload.read_contents,
        )
        .await;
        forget(&state.files, upload.id).await?;
        return Ok((
            StatusCode::CREATED,
            upload_headers(offset, true),
            Json(Uploaded::DropBoxItem(made?)),
        )
            .into_response());
    }
    // Into whose tree: the user's, or a share's owner's, if the share
    // still lets them.
    let target = upload.target();
    let owner = match &target {
        WriteTarget::New { parent, .. } | WriteTarget::Copy { parent, .. } => {
            state.files.access_for(user_id, *parent, Need::Create).await
        }
        WriteTarget::Replace { id, .. } => state.files.access_for(user_id, *id, Need::Change).await,
    };
    let owner = match owner {
        Ok(access) => access.owner,
        Err(e) => {
            forget(&state.files, upload.id).await?;
            return Err(e.into());
        }
    };
    // Over the storage limit: the bytes cannot go anywhere, so the upload
    // is done with.
    if let Err(e) = state.files.check_room(owner, offset as u64).await {
        forget(&state.files, upload.id).await?;
        return Err(e.into());
    }
    let new = matches!(target, WriteTarget::New { .. });
    match state.files.place_file(owner, target, &path).await {
        Ok(node) => {
            forget(&state.files, upload.id).await?;
            if new {
                state.files.mark_created(node.id, user_id).await?;
            }
            let node = crate::routes::files::seen(state, user_id, node).await?;
            Ok((
                StatusCode::CREATED,
                upload_headers(offset, true),
                Json(Uploaded::File(node)),
            )
                .into_response())
        }
        // Received in full but not placed (a name taken since, a file
        // changed since): kept until it expires, appending refused.
        Err(e) => {
            sqlx::query("UPDATE uploads SET complete = 1 WHERE id = ?1")
                .bind(upload.id)
                .execute(state.db.writer())
                .await?;
            Err(e.into())
        }
    }
}

/// An upload is done with: its bytes and its row go.
async fn forget(files: &Files, id: Uuid) -> Result<(), FileError> {
    let _ = std::fs::remove_file(part_path(files, id));
    sqlx::query("DELETE FROM uploads WHERE id = ?1")
        .bind(id)
        .execute(files.db().writer())
        .await?;
    files.upload_locks.lock().unwrap().remove(&id);
    Ok(())
}

/// Remove the uploads that waited longer than their lifetime.
pub async fn expire(files: &Files) -> Result<(), FileError> {
    let expired: Vec<Uuid> = sqlx::query_scalar("SELECT id FROM uploads WHERE expires_at < ?1")
        .bind(crate::auth::now_iso())
        .fetch_all(files.db().reader())
        .await?;
    for id in expired {
        forget(files, id).await?;
    }
    Ok(())
}
