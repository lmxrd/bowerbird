//! Mirrors (docs/M7D_DESIGN.md §6): a user's read-only copies of drives in
//! their own storage. Listing, making, changing the schedule, removing
//! (with the copy), "Mirror now", ejecting (in place, or a copy), and
//! restoring from a mirror's trash or versions into the drive mirrored. A
//! mirror is browsed as the drive it is (`GET /drives`: `kind` `mirror`,
//! `mirror_of`), through the files API, read-only. The rules are in
//! `files/mirrors.rs`.

use crate::error::{ErrorBody, HttpError};
use crate::extract::ApiJson;
use crate::files::drives::{DriveKind, DriveRow};
use crate::files::mirrors::{self, MirrorRow, MirrorStatus, Progress, Schedule, Stopped};
use crate::files::roles::{self, Role};
use crate::files::{FileError, Node};
use crate::routes::auth::AuthUser;
use crate::routes::backends::BackendRef;
use crate::routes::drives::Drive;
use crate::routes::files::serve_file;
use crate::state::AppState;
use axum::Json;
use axum::extract::{Path, Query, State};
use axum::http::{HeaderMap, StatusCode};
use axum::response::{IntoResponse, Response};
use serde::{Deserialize, Serialize};
use utoipa::{IntoParams, ToSchema};
use utoipa_axum::router::OpenApiRouter;
use utoipa_axum::routes;
use uuid::Uuid;

/// Nested under /mirrors.
pub fn router() -> OpenApiRouter<AppState> {
    OpenApiRouter::new()
        .routes(routes!(list_mirrors, create_mirror))
        .routes(routes!(get_mirror, update_mirror, delete_mirror))
        .routes(routes!(run_mirror))
        .routes(routes!(eject_mirror))
        .routes(routes!(restore_from_mirror_trash))
        .routes(routes!(download_from_mirror_trash))
        .routes(routes!(restore_mirror_version))
}

/// The `code` of a refusal to restore into a drive the user may not write
/// to: the client downloads instead.
pub const DRIVE_READ_ONLY: &str = "drive_read_only";
/// The `code` of "Mirror now" on a stopped mirror.
pub const MIRROR_STOPPED: &str = "mirror_stopped";
/// The `code` of a removal or an eject while the mirror copies.
pub const COPY_RUNNING: &str = "copy_running";
/// The `code` of an eject whose name a shared drive has.
pub const NAME_TAKEN: &str = "name_taken";

/// A removal's or an eject's conflict with its `code`: the copy under way
/// (`copy_running`), else the name taken (`name_taken`), the one other
/// conflict an eject has.
fn conflict_code(e: FileError) -> HttpError {
    match e {
        FileError::Conflict(message) => {
            let code = if message == mirrors::COPY_UNDER_WAY {
                COPY_RUNNING
            } else {
                NAME_TAKEN
            };
            HttpError::Status {
                status: StatusCode::CONFLICT,
                message,
                code: Some(code),
            }
        }
        e => e.into(),
    }
}

/// A mirror, as its user sees it.
#[derive(Serialize, ToSchema)]
pub struct Mirror {
    /// Its id, which is also its drive's (a string, as every drive's id):
    /// the drive `kind` `mirror` in `GET /drives`, browsed from `root_id`.
    pub id: String,
    /// Its root folder (every `/files/{id}` endpoint takes it, read-only).
    /// Always present.
    #[schema(required = true)]
    pub root_id: Option<Uuid>,
    /// The drive it copies; null once that drive was deleted.
    #[schema(required = true)]
    pub drive_id: Option<String>,
    /// That drive's name as last copied ("My drive" for one's own).
    pub drive_name: String,
    /// That drive's kind (`my` or `shared`); null once it was deleted.
    #[schema(required = true)]
    pub drive_kind: Option<DriveKind>,
    /// The user's own storage it is in.
    pub backend: BackendRef,
    /// The bucket its copy is in, for the confirmation before removing it
    /// (which deletes the copy).
    pub bucket: String,
    /// Where its files are: `s3://<bucket>/<prefix>/<id>/`.
    pub location: String,
    pub schedule: Schedule,
    pub status: MirrorStatus,
    /// Why it stopped (with `status` `stopped`), for a client to act on;
    /// null otherwise. `stopped_reason` says it in words.
    #[schema(required = true)]
    pub stopped: Option<Stopped>,
    #[schema(required = true)]
    pub stopped_reason: Option<String>,
    /// When it stopped (with `stopped`): the drive's deletion, or the
    /// first look that found it stopped (within a minute); null while it
    /// goes on.
    #[schema(required = true)]
    pub stopped_at: Option<String>,
    /// Why the last copy failed (with `status` `failing`), fit to show.
    #[schema(required = true)]
    pub error: Option<String>,
    /// How far the copy is (with `status` `copying`).
    #[schema(required = true)]
    pub progress: Option<Progress>,
    /// When the last complete copy ended; null before the first.
    #[schema(required = true)]
    pub last_copy_at: Option<String>,
    /// Restoring from its trash or versions copies the file back into the
    /// drive (the user may write there); false: the client downloads it
    /// instead.
    pub restores_into_drive: bool,
    /// Bytes it takes: its files, trash and versions.
    pub used: i64,
    pub created_at: String,
}

/// What a drive that is a mirror copies (`Drive.mirror_of`).
#[derive(Serialize, ToSchema)]
pub struct MirrorOf {
    /// The drive it copies; null once that drive was deleted.
    #[schema(required = true)]
    pub drive_id: Option<String>,
    /// That drive's name as last copied.
    pub drive_name: String,
    pub schedule: Schedule,
    pub status: MirrorStatus,
    /// Why it stopped (with `status` `stopped`); null otherwise.
    #[schema(required = true)]
    pub stopped: Option<Stopped>,
    /// As `Mirror.stopped_at`.
    #[schema(required = true)]
    pub stopped_at: Option<String>,
    /// As `Mirror.restores_into_drive`.
    pub restores_into_drive: bool,
}

/// Whether the user may write to the drive a mirror copies.
async fn restores_into_drive(state: &AppState, row: &MirrorRow) -> Result<bool, HttpError> {
    let Some(source) = row.source_id else {
        return Ok(false);
    };
    let role = roles::drive_role(state.db.reader(), row.user_id, source).await?;
    Ok(role.is_some_and(|r| r >= Role::Editor))
}

/// What a mirror drive copies, for `Drive.mirror_of`; None for another
/// drive.
pub(crate) async fn mirror_of(
    state: &AppState,
    drive: Uuid,
) -> Result<Option<MirrorOf>, HttpError> {
    let Some(row) = MirrorRow::get(state.db.reader(), drive).await? else {
        return Ok(None);
    };
    let now = state.files.mirror_status(&row).await?;
    let name: String = sqlx::query_scalar("SELECT name FROM drives WHERE id = ?1")
        .bind(drive)
        .fetch_one(state.db.reader())
        .await?;
    Ok(Some(MirrorOf {
        drive_id: row.source_id.map(|s| s.to_string()),
        drive_name: name,
        schedule: row.schedule,
        status: now.status,
        stopped: now.stopped,
        stopped_at: now.stopped_at,
        restores_into_drive: restores_into_drive(state, &row).await?,
    }))
}

/// Who mirrors a drive (`DriveAccess.mirrored_by`): the users whose mirror
/// of it copies (not one that stopped), each to their own storage.
#[derive(Serialize, ToSchema)]
pub struct MirroredBy {
    pub user: crate::routes::shares::Person,
    /// Since when.
    pub since: String,
}

pub(crate) async fn mirrored_by(
    state: &AppState,
    drive: Uuid,
) -> Result<Vec<MirroredBy>, HttpError> {
    let rows: Vec<(Uuid, String, String)> = sqlx::query_as(
        "SELECT m.user_id, u.email, m.created_at FROM mirrors m JOIN users u ON u.id = m.user_id
         WHERE m.source_id = ?1 ORDER BY u.email, m.created_at",
    )
    .bind(drive)
    .fetch_all(state.db.reader())
    .await?;
    let mut out = Vec::new();
    for (user, email, since) in rows {
        if state
            .files
            .mirror_refusal(user, Some(drive))
            .await?
            .is_some()
        {
            continue;
        }
        out.push(MirroredBy {
            user: crate::routes::shares::Person { id: user, email },
            since,
        });
    }
    Ok(out)
}

async fn view(state: &AppState, row: &MirrorRow) -> Result<Mirror, HttpError> {
    let drive = DriveRow::get(state.db.reader(), row.id)
        .await?
        .ok_or_else(HttpError::not_found)?;
    let backend = match drive.backend_id {
        Some(id) => crate::backends::BackendRow::get(state.db.reader(), id).await?,
        None => None,
    }
    .ok_or_else(|| HttpError::internal("a mirror without its storage"))?;
    let root_id: Option<Uuid> =
        sqlx::query_scalar("SELECT id FROM nodes WHERE drive_id = ?1 AND parent_id IS NULL")
            .bind(row.id)
            .fetch_optional(state.db.reader())
            .await?;
    let source_kind = match row.source_id {
        Some(source) => DriveRow::get(state.db.reader(), source)
            .await?
            .map(|d| d.kind),
        None => None,
    };
    let now = state.files.mirror_status(row).await?;
    Ok(Mirror {
        id: row.id.to_string(),
        root_id,
        drive_id: row.source_id.map(|s| s.to_string()),
        drive_name: drive.name.clone(),
        drive_kind: source_kind,
        backend: crate::routes::backends::backend_ref(state, Some(backend.id)).await?,
        location: format!(
            "s3://{}/{}",
            backend.bucket,
            crate::storage::drive_base(&backend.prefix, row.id)
        ),
        bucket: backend.bucket,
        schedule: row.schedule,
        status: now.status,
        stopped: now.stopped,
        stopped_reason: now.stopped.map(|s| s.reason().to_string()),
        stopped_at: now.stopped_at,
        error: row.error.clone(),
        progress: now.progress,
        last_copy_at: row.last_copy_at.clone(),
        restores_into_drive: restores_into_drive(state, row).await?,
        used: state.files.usage(row.id).await?.used,
        created_at: row.created_at.clone(),
    })
}

/// A mirror of the user's; another is not found.
async fn mine(state: &AppState, user: Uuid, id: &str) -> Result<MirrorRow, HttpError> {
    let id: Uuid = id.parse().map_err(|_| HttpError::not_found())?;
    MirrorRow::get(state.db.reader(), id)
        .await?
        .filter(|m| m.user_id == user)
        .ok_or_else(HttpError::not_found)
}

/// A refusal of a mirror the user may not have, with its code.
fn refused(stopped: Stopped) -> HttpError {
    HttpError::Status {
        status: StatusCode::FORBIDDEN,
        message: stopped.reason().into(),
        code: Some(stopped.code()),
    }
}

#[utoipa::path(
    get,
    path = "",
    tag = "Mirrors",
    operation_id = "listMirrors",
    summary = "My mirrors",
    description = "Every mirror of the user's, the oldest first, with its drive, storage, schedule, last copy and status.",
    responses(
        (status = 200, description = "The user's mirrors", body = Vec<Mirror>),
        (status = 401, description = "Not signed in", body = ErrorBody),
    ),
    security(("session" = []), ("api_key" = [])),
)]
async fn list_mirrors(
    State(state): State<AppState>,
    user: AuthUser,
) -> Result<Json<Vec<Mirror>>, HttpError> {
    let mut out = Vec::new();
    for row in MirrorRow::of_user(state.db.reader(), user.0.id).await? {
        out.push(view(&state, &row).await?);
    }
    Ok(Json(out))
}

/// A new mirror.
#[derive(Deserialize, ToSchema)]
pub struct NewMirror {
    /// The drive to mirror: My drive (its user), or a shared drive the
    /// user has as a viewer or above, when mirroring shared drives is on
    /// and its managers do not forbid it.
    pub drive_id: String,
    /// One of the user's own storage backends (`GET /backends`).
    pub backend_id: Uuid,
    /// Left out: `continuous`.
    #[serde(default)]
    pub schedule: Option<Schedule>,
}

#[utoipa::path(
    post,
    path = "",
    tag = "Mirrors",
    operation_id = "createMirror",
    summary = "Mirror a drive",
    description = "A read-only copy of a drive in one of the user's own storage backends, at `<prefix>/<mirror id>/`, of what they see in it (not the shares placed in My drive). It appears among the user's drives (`kind` `mirror`); the first copy starts at once, but for the `manual` schedule (`POST /mirrors/{id}/run`). A refusal carries a `code`: `access_lost` (not the user's drive, or upload only), `shared_drives_off`, `mirroring_forbidden`.",
    request_body(content = NewMirror, content_type = "application/json"),
    responses(
        (status = 201, description = "Made", body = Mirror),
        (status = 400, description = "Not the user's own storage, or a mirror (which is not mirrored)", body = ErrorBody),
        (status = 401, description = "Not signed in", body = ErrorBody),
        (status = 403, description = "The user may not mirror that drive (`code`), their own storage is not allowed here, or a read-only key", body = ErrorBody),
        (status = 404, description = "Not a drive of the user's", body = ErrorBody),
        (status = 415, description = "The body must be JSON (Content-Type: application/json)", body = ErrorBody),
        (status = 503, description = "The storage does not answer or is locked (`code` `storage_unavailable`)", body = ErrorBody),
    ),
    security(("session" = []), ("api_key" = [])),
)]
async fn create_mirror(
    State(state): State<AppState>,
    user: AuthUser,
    ApiJson(body): ApiJson<NewMirror>,
) -> Result<Response, HttpError> {
    let me = user.0.id;
    let source: Uuid = body.drive_id.parse().map_err(|_| HttpError::not_found())?;
    let drive = DriveRow::get(state.db.reader(), source)
        .await?
        .ok_or_else(HttpError::not_found)?;
    if roles::drive_role(state.db.reader(), me, source)
        .await?
        .is_none()
    {
        return Err(HttpError::not_found());
    }
    if drive.kind == DriveKind::Mirror {
        return Err(HttpError::bad_request("A mirror is not mirrored"));
    }
    if let Some(stopped) = state.files.mirror_refusal(me, Some(source)).await? {
        return Err(refused(stopped));
    }
    let backend = crate::backends::BackendRow::get(state.db.reader(), body.backend_id)
        .await?
        .filter(|b| b.owner_id == Some(me))
        .ok_or_else(|| HttpError::bad_request("A mirror goes to your own storage"))?;
    crate::routes::backends::require_own_allowed(&state, me).await?;
    let id = state
        .files
        .create_mirror(me, source, backend.id, body.schedule.unwrap_or_default())
        .await?;
    let row = MirrorRow::get(state.db.reader(), id)
        .await?
        .ok_or_else(HttpError::not_found)?;
    Ok((StatusCode::CREATED, Json(view(&state, &row).await?)).into_response())
}

#[utoipa::path(
    get,
    path = "/{id}",
    tag = "Mirrors",
    operation_id = "getMirror",
    summary = "A mirror",
    params(("id" = String, Path, description = "The mirror's id")),
    responses(
        (status = 200, description = "The mirror", body = Mirror),
        (status = 401, description = "Not signed in", body = ErrorBody),
        (status = 404, description = "Not a mirror of the user's", body = ErrorBody),
    ),
    security(("session" = []), ("api_key" = [])),
)]
async fn get_mirror(
    State(state): State<AppState>,
    user: AuthUser,
    Path(id): Path<String>,
) -> Result<Json<Mirror>, HttpError> {
    let row = mine(&state, user.0.id, &id).await?;
    Ok(Json(view(&state, &row).await?))
}

/// What to change of a mirror.
#[derive(Deserialize, ToSchema)]
pub struct UpdateMirror {
    pub schedule: Schedule,
}

#[utoipa::path(
    patch,
    path = "/{id}",
    tag = "Mirrors",
    operation_id = "updateMirror",
    summary = "Change when a mirror copies",
    params(("id" = String, Path, description = "The mirror's id")),
    request_body(content = UpdateMirror, content_type = "application/json"),
    responses(
        (status = 200, description = "Changed", body = Mirror),
        (status = 401, description = "Not signed in", body = ErrorBody),
        (status = 403, description = "A read-only key", body = ErrorBody),
        (status = 404, description = "Not a mirror of the user's", body = ErrorBody),
        (status = 415, description = "The body must be JSON (Content-Type: application/json)", body = ErrorBody),
    ),
    security(("session" = []), ("api_key" = [])),
)]
async fn update_mirror(
    State(state): State<AppState>,
    user: AuthUser,
    Path(id): Path<String>,
    ApiJson(body): ApiJson<UpdateMirror>,
) -> Result<Json<Mirror>, HttpError> {
    let row = mine(&state, user.0.id, &id).await?;
    sqlx::query("UPDATE mirrors SET schedule = ?2 WHERE id = ?1")
        .bind(row.id)
        .bind(body.schedule)
        .execute(state.db.writer())
        .await?;
    state.files.wake_mirrors();
    let row = mine(&state, user.0.id, &id).await?;
    Ok(Json(view(&state, &row).await?))
}

#[utoipa::path(
    delete,
    path = "/{id}",
    tag = "Mirrors",
    operation_id = "deleteMirror",
    summary = "Remove a mirror and its copy",
    description = "Its files, trash and versions are deleted from the bucket (`Mirror.bucket`: the client confirms it by name first), when the storage answers. The drive it copied is not touched.",
    params(("id" = String, Path, description = "The mirror's id")),
    responses(
        (status = 204, description = "Removed"),
        (status = 401, description = "Not signed in", body = ErrorBody),
        (status = 403, description = "A read-only key", body = ErrorBody),
        (status = 404, description = "Not a mirror of the user's", body = ErrorBody),
        (status = 409, description = "A copy is under way (`code` `copy_running`): try again once it is done", body = ErrorBody),
    ),
    security(("session" = []), ("api_key" = [])),
)]
async fn delete_mirror(
    State(state): State<AppState>,
    user: AuthUser,
    Path(id): Path<String>,
) -> Result<StatusCode, HttpError> {
    let row = mine(&state, user.0.id, &id).await?;
    state
        .files
        .delete_mirror(row.id)
        .await
        .map_err(conflict_code)?;
    Ok(StatusCode::NO_CONTENT)
}

#[utoipa::path(
    post,
    path = "/{id}/run",
    tag = "Mirrors",
    operation_id = "runMirror",
    summary = "Mirror now",
    description = "A copy now, whatever the schedule (on locked private credentials: as soon as they are unlocked). Answers at once; `status` says how it goes.",
    params(("id" = String, Path, description = "The mirror's id")),
    responses(
        (status = 202, description = "Asked for", body = Mirror),
        (status = 401, description = "Not signed in", body = ErrorBody),
        (status = 403, description = "A read-only key", body = ErrorBody),
        (status = 404, description = "Not a mirror of the user's", body = ErrorBody),
        (status = 409, description = "It stopped (`code` `mirror_stopped`; `Mirror.stopped` says why)", body = ErrorBody),
    ),
    security(("session" = []), ("api_key" = [])),
)]
async fn run_mirror(
    State(state): State<AppState>,
    user: AuthUser,
    Path(id): Path<String>,
) -> Result<Response, HttpError> {
    let row = mine(&state, user.0.id, &id).await?;
    if let Some(stopped) = state
        .files
        .mirror_refusal(row.user_id, row.source_id)
        .await?
    {
        return Err(HttpError::Status {
            status: StatusCode::CONFLICT,
            message: stopped.reason().into(),
            code: Some(MIRROR_STOPPED),
        });
    }
    state.files.request_mirror(row.id).await?;
    let row = mine(&state, user.0.id, &id).await?;
    Ok((StatusCode::ACCEPTED, Json(view(&state, &row).await?)).into_response())
}

/// How a mirror is ejected.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Deserialize, ToSchema)]
#[serde(rename_all = "snake_case")]
pub enum EjectMode {
    /// The mirror becomes a shared drive where it is (the same storage, the
    /// same place), the user its manager; mirroring stops and nothing is
    /// copied.
    InPlace,
    /// A new shared drive on the same storage with the mirror's current
    /// contents (not its trash or versions), the user its manager; the
    /// mirror goes on.
    Copy,
}

/// Ejecting a mirror.
#[derive(Deserialize, ToSchema)]
pub struct EjectMirror {
    pub mode: EjectMode,
    /// The drive's name: 1 to 100 characters, unique among the shared
    /// drives; left out, the mirror's (its drive's name).
    #[serde(default)]
    pub name: Option<String>,
}

#[utoipa::path(
    post,
    path = "/{id}/eject",
    tag = "Mirrors",
    operation_id = "ejectMirror",
    summary = "Make a drive of a mirror",
    description = "`in_place`: the mirror becomes a shared drive at the same place (its id stays, now a drive's), and is no longer a mirror. `copy`: a new shared drive with what the mirror holds now (copied on the storage; answered once done), and the mirror goes on. Either is on the user's own storage, as their shared drive.",
    params(("id" = String, Path, description = "The mirror's id")),
    request_body(content = EjectMirror, content_type = "application/json"),
    responses(
        (status = 200, description = "The mirror is the drive now", body = Drive),
        (status = 201, description = "The new drive", body = Drive),
        (status = 400, description = "An invalid name", body = ErrorBody),
        (status = 401, description = "Not signed in", body = ErrorBody),
        (status = 403, description = "Their own storage is not allowed here, or a read-only key", body = ErrorBody),
        (status = 404, description = "Not a mirror of the user's", body = ErrorBody),
        (status = 409, description = "A shared drive has that name (`code` `name_taken`), or a copy is under way (`code` `copy_running`)", body = ErrorBody),
        (status = 415, description = "The body must be JSON (Content-Type: application/json)", body = ErrorBody),
        (status = 503, description = "The storage does not answer or is locked (`code` `storage_unavailable`)", body = ErrorBody),
    ),
    security(("session" = []), ("api_key" = [])),
)]
async fn eject_mirror(
    State(state): State<AppState>,
    user: AuthUser,
    Path(id): Path<String>,
    ApiJson(body): ApiJson<EjectMirror>,
) -> Result<Response, HttpError> {
    let me = user.0.id;
    let row = mine(&state, me, &id).await?;
    crate::routes::backends::require_own_allowed(&state, me).await?;
    let name = match body.name {
        Some(name) => name,
        None => {
            sqlx::query_scalar("SELECT name FROM drives WHERE id = ?1")
                .bind(row.id)
                .fetch_one(state.db.reader())
                .await?
        }
    };
    match body.mode {
        EjectMode::InPlace => {
            state
                .files
                .eject_in_place(row.id, &name)
                .await
                .map_err(conflict_code)?;
            let drive = crate::routes::drives::view_for(&state, row.id, me).await?;
            Ok((StatusCode::OK, Json(drive)).into_response())
        }
        EjectMode::Copy => {
            let files = state.files.clone();
            // Not cut short by a dropped request.
            let made = tokio::spawn(async move { files.eject_copy(row.id, &name, me).await })
                .await
                .map_err(HttpError::internal)?
                .map_err(conflict_code)?;
            let drive = crate::routes::drives::view_for(&state, made, me).await?;
            Ok((StatusCode::CREATED, Json(drive)).into_response())
        }
    }
}

/// A restore into the drive mirrored, with its refusals' codes.
fn into_drive(e: FileError) -> HttpError {
    match e {
        FileError::Forbidden(message) => HttpError::Status {
            status: StatusCode::FORBIDDEN,
            message,
            code: Some(DRIVE_READ_ONLY),
        },
        e => e.into(),
    }
}

/// The mirror a restore is from, when its drive is there and the user may
/// write to it.
async fn restorable(state: &AppState, user: Uuid, id: &str) -> Result<MirrorRow, HttpError> {
    let row = mine(state, user, id).await?;
    let Some(source) = row.source_id else {
        return Err(refused(Stopped::DriveDeleted));
    };
    match roles::drive_role(state.db.reader(), user, source).await? {
        None => Err(refused(Stopped::AccessLost)),
        Some(role) if role < Role::Editor => Err(HttpError::Status {
            status: StatusCode::FORBIDDEN,
            message: "You may not add to that drive: download it instead".into(),
            code: Some(DRIVE_READ_ONLY),
        }),
        Some(_) => Ok(row),
    }
}

#[utoipa::path(
    post,
    path = "/{id}/trash/{item_id}/restore",
    tag = "Mirrors",
    operation_id = "restoreFromMirrorTrash",
    summary = "Restore from a mirror's trash into its drive",
    description = "The item (a file, or a folder with what was in it) is copied back into the drive mirrored, where it was (its folder there now, else the folder at its path, else the root), as a new item. Refused with a `code` when it cannot: `drive_read_only` (the user may not add there: download it instead, `GET /mirrors/{id}/trash/{item_id}/content`), `drive_deleted`, `access_lost`.",
    params(
        ("id" = String, Path, description = "The mirror's id"),
        ("item_id" = Uuid, Path, description = "The item of the mirror's trash"),
    ),
    responses(
        (status = 200, description = "The new item in the drive mirrored", body = Node),
        (status = 401, description = "Not signed in", body = ErrorBody),
        (status = 403, description = "The user may not add there (`code`), or a read-only key", body = ErrorBody),
        (status = 404, description = "Not a mirror of the user's, or not in its trash", body = ErrorBody),
        (status = 409, description = "The name is taken there", body = ErrorBody),
        (status = 503, description = "A storage does not answer or is locked (`code` `storage_unavailable`)", body = ErrorBody),
        (status = 507, description = "Not enough room in the drive", body = ErrorBody),
    ),
    security(("session" = []), ("api_key" = [])),
)]
async fn restore_from_mirror_trash(
    State(state): State<AppState>,
    user: AuthUser,
    Path((id, item_id)): Path<(String, Uuid)>,
) -> Result<Json<Node>, HttpError> {
    let row = restorable(&state, user.0.id, &id).await?;
    let files = state.files.clone();
    let me = user.0.id;
    let node = tokio::spawn(async move { files.restore_into_source(&row, item_id, me).await })
        .await
        .map_err(HttpError::internal)?
        .map_err(into_drive)?;
    Ok(Json(
        crate::routes::files::seen(&state, user.0.id, node).await?,
    ))
}

#[derive(Deserialize, IntoParams)]
#[into_params(parameter_in = Query)]
pub struct TrashDownloadParams {
    /// Shown in the browser rather than saved.
    #[serde(default)]
    pub inline: bool,
}

#[utoipa::path(
    get,
    path = "/{id}/trash/{item_id}/content",
    tag = "Mirrors",
    operation_id = "downloadFromMirrorTrash",
    summary = "Download a file from a mirror's trash",
    description = "What a restore does when the user may not write to the drive mirrored. One range is honoured.",
    params(
        ("id" = String, Path, description = "The mirror's id"),
        ("item_id" = Uuid, Path, description = "The file in the mirror's trash"),
        TrashDownloadParams,
    ),
    responses(
        (status = 200, description = "The contents", content_type = "application/octet-stream"),
        (status = 206, description = "The range asked for", content_type = "application/octet-stream"),
        (status = 400, description = "A folder", body = ErrorBody),
        (status = 401, description = "Not signed in", body = ErrorBody),
        (status = 404, description = "Not a mirror of the user's, or not in its trash", body = ErrorBody),
        (status = 503, description = "The storage does not answer or is locked (`code` `storage_unavailable`)", body = ErrorBody),
    ),
    security(("session" = []), ("api_key" = [])),
)]
async fn download_from_mirror_trash(
    State(state): State<AppState>,
    user: AuthUser,
    Path((id, item_id)): Path<(String, Uuid)>,
    Query(params): Query<TrashDownloadParams>,
    headers: HeaderMap,
) -> Result<Response, HttpError> {
    let row = mine(&state, user.0.id, &id).await?;
    let (name, kind): (String, crate::files::Kind) =
        sqlx::query_as("SELECT name, kind FROM trash WHERE id = ?1 AND drive_id = ?2")
            .bind(item_id)
            .bind(row.id)
            .fetch_optional(state.db.reader())
            .await?
            .ok_or_else(HttpError::not_found)?;
    if kind != crate::files::Kind::File {
        return Err(HttpError::bad_request("A folder: restore it instead"));
    }
    let storage = state.files.storage(row.id).await?;
    let mime = crate::files::scan::mime_of(&name);
    // A trashed file never changes: its id is its tag.
    serve_file(
        &storage,
        &crate::files::Tree::trash_path(item_id),
        &name,
        mime.as_deref(),
        &format!("\"t-{item_id}\""),
        params.inline,
        &headers,
    )
    .await
}

#[utoipa::path(
    post,
    path = "/{id}/versions/{version_id}/restore",
    tag = "Mirrors",
    operation_id = "restoreMirrorVersion",
    summary = "Restore a mirror's version into its drive",
    description = "An earlier content of a mirror's file (`GET /files/{id}/versions` of the mirror's file lists them, `…/content` downloads one) is written over the file it copies in the drive mirrored (whose content is kept there as a version), or, when that file is gone, as a new file where it was. Refused with a `code` when it cannot: `drive_read_only` (download it instead), `drive_deleted`, `access_lost`.",
    params(
        ("id" = String, Path, description = "The mirror's id"),
        ("version_id" = Uuid, Path, description = "The version of a file of the mirror"),
    ),
    responses(
        (status = 200, description = "The file in the drive mirrored, with its new version", body = Node),
        (status = 401, description = "Not signed in", body = ErrorBody),
        (status = 403, description = "The user may not write there (`code`), or a read-only key", body = ErrorBody),
        (status = 404, description = "Not a mirror of the user's, or not one of its versions", body = ErrorBody),
        (status = 409, description = "Its file is in the mirror's trash, or the name is taken", body = ErrorBody),
        (status = 503, description = "A storage does not answer or is locked (`code` `storage_unavailable`)", body = ErrorBody),
        (status = 507, description = "Not enough room in the drive", body = ErrorBody),
    ),
    security(("session" = []), ("api_key" = [])),
)]
async fn restore_mirror_version(
    State(state): State<AppState>,
    user: AuthUser,
    Path((id, version_id)): Path<(String, Uuid)>,
) -> Result<Json<Node>, HttpError> {
    let row = restorable(&state, user.0.id, &id).await?;
    let files = state.files.clone();
    let me = user.0.id;
    let by = Some(user.source());
    let node = tokio::spawn(async move {
        files
            .restore_version_into_source(&row, version_id, me, by)
            .await
    })
    .await
    .map_err(HttpError::internal)?
    .map_err(into_drive)?;
    Ok(Json(
        crate::routes::files::seen(&state, user.0.id, node).await?,
    ))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn code(e: HttpError) -> Option<&'static str> {
        match e {
            HttpError::Status { code, .. } => code,
            _ => None,
        }
    }

    #[test]
    fn ejects_and_removals_say_why_they_conflict() {
        let running = conflict_code(FileError::Conflict(mirrors::COPY_UNDER_WAY.into()));
        assert_eq!(running.status(), StatusCode::CONFLICT);
        assert_eq!(code(running), Some(COPY_RUNNING));
        let taken = conflict_code(FileError::Conflict(
            "A shared drive has that name already".into(),
        ));
        assert_eq!(code(taken), Some(NAME_TAKEN));
        assert_eq!(code(conflict_code(FileError::NotFound)), None);
    }
}
