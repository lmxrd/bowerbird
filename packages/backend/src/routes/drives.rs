//! Drives (docs/M7A_DESIGN.md §2): a user's drives (My drive and the
//! shared drives they are a member of), what a drive's managers do to it
//! (rename), and the superadmin's shared drives (make, rename, storage
//! limit, team, delete). Who has access to a drive is its grants
//! (docs/M7B_DESIGN.md §1): `/drives/{id}/access` for its managers,
//! `/superadmin/drives/{id}/access` for the superadmin (`routes/access.rs`).
//! The rules are in `files/drives.rs` and `files/grants.rs`.
//!
//! A drive's id is a string in the API, a UUID for this server's drives,
//! so that a remote one (`drive@host`, M7 step 5) fits without a change.

use crate::activity::{self, Actor};
use crate::error::{ErrorBody, HttpError};
use crate::extract::ApiJson;
use crate::files::drives::{self, DriveKind, DriveRow};
use crate::files::grants::Grantee;
use crate::files::roles::{self, Role};
use crate::routes::auth::{AuthUser, Superadmin};
use crate::state::AppState;
use axum::Json;
use axum::extract::{Path, State};
use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use serde::{Deserialize, Serialize};
use utoipa::ToSchema;
use utoipa_axum::router::OpenApiRouter;
use utoipa_axum::routes;
use uuid::Uuid;

/// Nested under /drives.
pub fn router() -> OpenApiRouter<AppState> {
    OpenApiRouter::new()
        .routes(routes!(list_drives, create_drive))
        .routes(routes!(get_drive, rename_drive))
}

/// Nested under /superadmin.
pub fn superadmin_router() -> OpenApiRouter<AppState> {
    OpenApiRouter::new()
        .routes(routes!(list_shared_drives, create_shared_drive))
        .routes(routes!(update_shared_drive, delete_shared_drive))
}

/// A drive, as its users see it.
#[derive(Serialize, ToSchema)]
pub struct Drive {
    /// A string: a UUID for this server's drives.
    pub id: String,
    pub kind: DriveKind,
    /// "My drive" for a user's own; a shared drive's name; a mirror's, the name of the drive it copies.
    pub name: String,
    /// The drive's root folder (a node: every `/files/{id}` endpoint takes
    /// it); null while the drive's folder is not available.
    #[schema(required = true)]
    pub root_id: Option<Uuid>,
    /// Bytes the drive takes: its files, trash and versions (and, for My
    /// drive, the drop box's items not yet placed).
    pub used: i64,
    /// Bytes; null: no limit.
    #[schema(required = true)]
    pub limit: Option<i64>,
    /// The user manages it (`my_role` manager or owner): who has access,
    /// its name and settings. Always true for My drive.
    pub manages: bool,
    /// The user's role on the drive itself: `owner` for My drive; a shared
    /// drive's from its grants (theirs, their teams') and its team's
    /// managers. Null in the superadmin's view of a drive they are not a
    /// member of. Always present.
    #[schema(required = true)]
    pub my_role: Option<Role>,
    /// How many members a shared drive has; 1 for My drive.
    pub members: i64,
    /// Who manage a shared drive, by email (empty for My drive).
    pub managers: Vec<String>,
    /// The team it belongs to (its managers manage it); null for none.
    /// Always present.
    #[schema(required = true)]
    pub team_id: Option<Uuid>,
    #[schema(required = true)]
    pub team_name: Option<String>,
    /// Where it is stored (docs/M7C_DESIGN.md §1): the server's disk, a
    /// global backend, or someone's own storage (`owner`: warn its members
    /// that the drive goes if they remove it). `backed_up`: whether the
    /// server's backups cover it.
    pub backend: crate::routes::backends::BackendRef,
    /// Its storage can be used now: false when its backend fails or its
    /// private credentials are locked (`unavailable` says which,
    /// `unavailable_reason` why in words, e.g. "Locked until
    /// alice@example.com signs in"); sync waits meanwhile.
    pub available: bool,
    /// Why it cannot be used now, for a client to act on: `locked` (back
    /// once its owner signs in) or `failing`; null when available.
    #[schema(required = true)]
    pub unavailable: Option<crate::backends::Unavailable>,
    #[schema(required = true)]
    pub unavailable_reason: Option<String>,
    /// Being moved to other storage: where; null otherwise. The drive is
    /// read-only meanwhile.
    #[schema(required = true)]
    pub moving_to: Option<crate::routes::backends::BackendRef>,
    /// A mirror (`kind` `mirror`, docs/M7D_DESIGN.md): the drive it copies
    /// and how it stands (`my_role` is `viewer`: it is read-only; Settings
    /// → Mirrors is `/mirrors/{id}`, the same id). Null for other drives.
    #[schema(required = true)]
    pub mirror_of: Option<crate::routes::mirrors::MirrorOf>,
    /// A drive shared from another server (`kind` `remote`,
    /// docs/M7E_DESIGN.md §6, between Organizer servers): whose it is and
    /// where; `my_role` is the role it was given there. Null for this
    /// server's drives. Always present.
    #[schema(required = true)]
    pub remote: Option<RemoteDrive>,
}

/// Where a drive or an item shared from another server comes from.
#[derive(Serialize, ToSchema)]
pub struct RemoteDrive {
    /// The server it is on, `host[:port]`.
    pub server: String,
    /// Its owner's address there, `user`, and name.
    pub owner: String,
    pub owner_name: String,
    /// Who shared it, `user`.
    pub shared_by: String,
    /// `pending` until the user accepts it (`POST /shared/{root_id}/accept`;
    /// `DELETE /shared/{root_id}` declines it).
    pub state: crate::federation::incoming::ReceivedState,
    /// Why the last look at it failed (its server unreachable, the token
    /// refused); null when it answered. Always present.
    #[schema(required = true)]
    pub error: Option<String>,
}

/// Where a remote drive comes from.
pub(crate) async fn remote_drive(
    state: &AppState,
    drive: Uuid,
) -> Result<Option<RemoteDrive>, HttpError> {
    Ok(
        crate::federation::incoming::received_of_drive(state.db.reader(), drive)
            .await?
            .map(|r| RemoteDrive {
                server: r.host,
                owner: r.owner,
                owner_name: r.owner_name,
                shared_by: r.sender,
                state: r.state,
                error: r.error,
            }),
    )
}

/// A member of a shared drive: a user with a role on the drive itself
/// (directly or through a team), for its count and its managers.
struct DriveMember {
    email: String,
    /// Manages it (manager or above).
    manages: bool,
}

/// A shared drive, as the superadmin sees it.
#[derive(Serialize, ToSchema)]
pub struct SharedDrive {
    pub drive: Drive,
    /// Its folder on the server.
    pub folder: String,
    /// Whether that is the default one in the data dir.
    pub default_folder: bool,
    pub created_at: String,
}

/// A drive's id from the path: a UUID for this server's drives; anything
/// else is not found.
fn drive_id(id: &str) -> Result<Uuid, HttpError> {
    id.parse().map_err(|_| HttpError::not_found())
}

async fn members_of(state: &AppState, id: Uuid) -> Result<Vec<DriveMember>, HttpError> {
    let users = {
        let mut conn = state.db.reader().acquire().await?;
        drives::users_of(&mut conn, id).await?
    };
    let mut out = Vec::with_capacity(users.len());
    for (user, role) in users {
        let email: String = sqlx::query_scalar("SELECT email FROM users WHERE id = ?1")
            .bind(user)
            .fetch_one(state.db.reader())
            .await?;
        out.push(DriveMember {
            email,
            manages: role >= Role::Manager,
        });
    }
    out.sort_by(|a, b| b.manages.cmp(&a.manages).then(a.email.cmp(&b.email)));
    Ok(out)
}

/// A drive as `user` sees it (`role` is theirs on it; None: the
/// superadmin's view of a drive they are not a member of).
async fn drive_view(
    state: &AppState,
    row: &DriveRow,
    role: Option<Role>,
) -> Result<Drive, HttpError> {
    let root_id: Option<Uuid> =
        match sqlx::query_scalar("SELECT id FROM nodes WHERE drive_id = ?1 AND parent_id IS NULL")
            .bind(row.id)
            .fetch_optional(state.db.reader())
            .await?
        {
            Some(id) => Some(id),
            // Not indexed yet: now.
            None => state.files.root_node(row.id).await.ok().map(|n| n.id),
        };
    let usage = state.files.usage(row.id).await?;
    let (members, managers) = match row.kind {
        DriveKind::My | DriveKind::Mirror | DriveKind::Remote => (1, Vec::new()),
        DriveKind::Shared => {
            let members = members_of(state, row.id).await?;
            let managers = members
                .iter()
                .filter(|m| m.manages)
                .map(|m| m.email.clone())
                .collect();
            (members.len() as i64, managers)
        }
    };
    let team: Option<(Uuid, String)> = sqlx::query_as(
        "SELECT t.id, t.name FROM drives d JOIN teams t ON t.id = d.team_id WHERE d.id = ?1",
    )
    .bind(row.id)
    .fetch_optional(state.db.reader())
    .await?;
    let (unavailable, unavailable_reason) = state.files.unavailable(row.id).await?.unzip();
    let moving: Option<Option<Uuid>> =
        sqlx::query_scalar("SELECT to_backend FROM drive_moves WHERE drive_id = ?1")
            .bind(row.id)
            .fetch_optional(state.db.reader())
            .await?;
    let moving_to = match moving {
        Some(to) => Some(crate::routes::backends::backend_ref(state, to).await?),
        None => None,
    };
    let mirror_of = match row.kind {
        DriveKind::Mirror => crate::routes::mirrors::mirror_of(state, row.id).await?,
        _ => None,
    };
    let remote = match row.kind {
        DriveKind::Remote => remote_drive(state, row.id).await?,
        _ => None,
    };
    Ok(Drive {
        mirror_of,
        remote,
        backend: crate::routes::backends::backend_ref(state, row.backend_id).await?,
        available: unavailable.is_none(),
        unavailable,
        unavailable_reason,
        moving_to,
        id: row.id.to_string(),
        kind: row.kind,
        name: drives::display_name(row),
        root_id,
        used: usage.used,
        limit: usage.limit,
        manages: role.is_some_and(|r| r >= Role::Manager),
        my_role: role,
        members,
        managers,
        team_id: team.as_ref().map(|(id, _)| *id),
        team_name: team.map(|(_, name)| name),
    })
}

/// A drive of the user's, and their role on it; another is not found.
async fn mine(state: &AppState, user: Uuid, id: &str) -> Result<(DriveRow, Role), HttpError> {
    let id = drive_id(id)?;
    let role = roles::drive_role(state.db.reader(), user, id)
        .await?
        .ok_or_else(HttpError::not_found)?;
    let row = DriveRow::get(state.db.reader(), id)
        .await?
        .ok_or_else(HttpError::not_found)?;
    Ok((row, role))
}

/// A drive as a user sees it (their role, if any).
pub(crate) async fn view_for(
    state: &AppState,
    drive: Uuid,
    user: Uuid,
) -> Result<Drive, HttpError> {
    let row = DriveRow::get(state.db.reader(), drive)
        .await?
        .ok_or_else(HttpError::not_found)?;
    let role = roles::drive_role(state.db.reader(), user, drive).await?;
    drive_view(state, &row, role).await
}

/// A shared drive the user manages; refused (403) for a member who does
/// not, not found for anyone else.
async fn managed(state: &AppState, user: Uuid, id: &str) -> Result<DriveRow, HttpError> {
    let (row, role) = mine(state, user, id).await?;
    if row.kind != DriveKind::Shared {
        return Err(HttpError::bad_request("My drive has no members"));
    }
    if role < Role::Manager {
        return Err(HttpError::new(
            StatusCode::FORBIDDEN,
            "Only who manages this drive can do that",
        ));
    }
    Ok(row)
}

/// A user by email (any case).
async fn user_by_email(state: &AppState, email: &str) -> Result<Uuid, HttpError> {
    sqlx::query_scalar("SELECT id FROM users WHERE email = ?1 COLLATE NOCASE")
        .bind(email.trim())
        .fetch_optional(state.db.reader())
        .await?
        .ok_or_else(|| HttpError::bad_request("No user has that email"))
}

#[utoipa::path(
    get,
    path = "",
    tag = "Drives",
    operation_id = "listDrives",
    summary = "My drives",
    description = "My drive first, then the shared drives the user is a member of, by name, then the user's mirrors (read-only, `kind` `mirror`, with `mirror_of`), by name.",
    responses(
        (status = 200, description = "The user's drives", body = Vec<Drive>),
        (status = 401, description = "Not signed in", body = ErrorBody),
    ),
    security(("session" = []), ("api_key" = [])),
)]
async fn list_drives(
    State(state): State<AppState>,
    user: AuthUser,
) -> Result<Json<Vec<Drive>>, HttpError> {
    let mut out = Vec::new();
    for id in drives::drives_of(state.db.reader(), user.0.id).await? {
        let (row, role) = mine(&state, user.0.id, &id.to_string()).await?;
        out.push(drive_view(&state, &row, Some(role)).await?);
    }
    Ok(Json(out))
}

#[utoipa::path(
    get,
    path = "/{id}",
    tag = "Drives",
    operation_id = "getDrive",
    summary = "A drive",
    params(("id" = String, Path, description = "The drive's id")),
    responses(
        (status = 200, description = "The drive", body = Drive),
        (status = 401, description = "Not signed in", body = ErrorBody),
        (status = 404, description = "Not a drive of the user's", body = ErrorBody),
    ),
    security(("session" = []), ("api_key" = [])),
)]
async fn get_drive(
    State(state): State<AppState>,
    user: AuthUser,
    Path(id): Path<String>,
) -> Result<Json<Drive>, HttpError> {
    let (row, role) = mine(&state, user.0.id, &id).await?;
    Ok(Json(drive_view(&state, &row, Some(role)).await?))
}

/// A shared drive's new name.
#[derive(Deserialize, ToSchema)]
pub struct RenameDrive {
    /// 1 to 100 characters, unique among the shared drives (any case).
    pub name: String,
}

#[utoipa::path(
    patch,
    path = "/{id}",
    tag = "Drives",
    operation_id = "renameDrive",
    summary = "Rename a shared drive",
    description = "By who manages it.",
    params(("id" = String, Path, description = "The drive's id")),
    request_body(content = RenameDrive, content_type = "application/json"),
    responses(
        (status = 200, description = "Renamed", body = Drive),
        (status = 400, description = "An invalid name, or My drive", body = ErrorBody),
        (status = 401, description = "Not signed in", body = ErrorBody),
        (status = 403, description = "A member who does not manage it, or a read-only key", body = ErrorBody),
        (status = 404, description = "Not a drive of the user's", body = ErrorBody),
        (status = 409, description = "A shared drive has that name", body = ErrorBody),
        (status = 415, description = "The body must be JSON (Content-Type: application/json)", body = ErrorBody),
    ),
    security(("session" = []), ("api_key" = [])),
)]
async fn rename_drive(
    State(state): State<AppState>,
    user: AuthUser,
    Path(id): Path<String>,
    ApiJson(body): ApiJson<RenameDrive>,
) -> Result<Json<Drive>, HttpError> {
    let row = managed(&state, user.0.id, &id).await?;
    state.files.rename_drive(row.id, &body.name).await?;
    notify_members(&state, row.id).await?;
    get_drive(State(state), user, Path(id)).await
}

/// Give a user a grant on a drive (manager, else editor), or change theirs;
/// with the account activity when the superadmin did it.
async fn set_member(
    state: &AppState,
    drive: Uuid,
    member: Uuid,
    manages: bool,
    actor: Option<Actor>,
) -> Result<(), HttpError> {
    if state.files.set_member(drive, member, manages).await? {
        let role = if manages { Role::Manager } else { Role::Editor };
        log_drive_access(state, drive, member, Some(role), actor).await?;
    }
    Ok(())
}

/// Note a change of a user's access to a drive in their account activity,
/// when the superadmin made it (`None`: someone else, nothing noted).
pub(crate) async fn log_drive_access(
    state: &AppState,
    drive: Uuid,
    member: Uuid,
    role: Option<Role>,
    actor: Option<Actor>,
) -> Result<(), HttpError> {
    let Some(actor) = actor else {
        return Ok(());
    };
    let name: String = sqlx::query_scalar("SELECT name FROM drives WHERE id = ?1")
        .bind(drive)
        .fetch_one(state.db.reader())
        .await?;
    activity::log(
        state.db.writer(),
        member,
        actor,
        activity::Action::DriveAccess,
        activity::Detail {
            drive: Some(name),
            role: role.map(|r| r.as_str().to_string()),
            ..Default::default()
        },
    )
    .await?;
    Ok(())
}

/// Tell the open apps of a drive's members that it changed (its name).
async fn notify_members(state: &AppState, drive: Uuid) -> Result<(), HttpError> {
    let members = {
        let mut conn = state.db.reader().acquire().await?;
        drives::users_of(&mut conn, drive).await?
    };
    for (member, _) in members {
        state.files.announce(member, crate::files::Topic::Changes);
    }
    Ok(())
}

/// Make a shared drive (its folder, a new one in the data dir or one
/// given, or its prefix in a storage backend; and its limit) and index its
/// root: its members' apps open it. A drive whose backend does not answer
/// is not made.
pub(crate) async fn make_shared(
    state: &AppState,
    name: &str,
    folder: Option<&std::path::Path>,
    backend: Option<Uuid>,
    limit: Option<i64>,
    by: Uuid,
) -> Result<Uuid, HttpError> {
    let id = state
        .files
        .create_shared_drive_in(name, folder, backend, by)
        .await?;
    if limit.is_some() {
        state.files.set_drive_limit(id, limit).await?;
    }
    if let Err(e) = state.files.root_node(id).await {
        // Not left half made: nothing is in it yet.
        if let Err(e) = state.files.forget_drive(id).await {
            tracing::warn!("cannot take back the drive {id}: {e}");
        }
        return Err(e.into());
    }
    Ok(id)
}

/// A new shared drive made by a user.
#[derive(Deserialize, ToSchema)]
pub struct NewDrive {
    /// 1 to 100 characters, unique among the shared drives (any case).
    pub name: String,
    /// Where it is stored (`GET /backends/choices`): left out, or the
    /// server's disk's id, the server's disk.
    pub backend_id: Option<Uuid>,
}

#[utoipa::path(
    post,
    path = "",
    tag = "Drives",
    operation_id = "createDrive",
    summary = "Make a shared drive",
    description = "When the superadmin's setting `drives.create` is `everyone` (a team's managers make their team's drives with `POST /teams/{id}/drives`), or on the user's own storage (`storage.own_backends`). Its folder is in the data dir, or its prefix in the chosen backend (`backend_id`, from `GET /backends/choices`); the user manages it.",
    request_body(content = NewDrive, content_type = "application/json"),
    responses(
        (status = 201, description = "Made", body = Drive),
        (status = 400, description = "An invalid name, or no such storage", body = ErrorBody),
        (status = 401, description = "Not signed in", body = ErrorBody),
        (status = 403, description = "The setting does not allow it, a storage the user may not use, or a read-only key", body = ErrorBody),
        (status = 409, description = "A shared drive has that name", body = ErrorBody),
        (status = 503, description = "The storage backend does not answer or is locked (`code` `storage_unavailable`)", body = ErrorBody),
        (status = 415, description = "The body must be JSON (Content-Type: application/json)", body = ErrorBody),
    ),
    security(("session" = []), ("api_key" = [])),
)]
async fn create_drive(
    State(state): State<AppState>,
    user: AuthUser,
    ApiJson(body): ApiJson<NewDrive>,
) -> Result<Response, HttpError> {
    let backend = crate::routes::backends::chosen(body.backend_id);
    crate::routes::backends::check_maker(
        &state,
        backend,
        user.0.id,
        crate::routes::backends::Maker::User,
    )
    .await?;
    // A drive on the user's own storage is theirs to make (the
    // `storage.own_backends` setting); any other needs `drives.create`.
    let own: Option<i64> = match backend {
        Some(backend) => {
            sqlx::query_scalar("SELECT 1 FROM backends WHERE id = ?1 AND owner_id = ?2")
                .bind(backend)
                .bind(user.0.id)
                .fetch_optional(state.db.reader())
                .await?
        }
        None => None,
    };
    let env = crate::settings::Env::of_state(&state);
    let who = crate::settings::resolve(
        &state.db,
        &env,
        "drives.create",
        crate::settings::For::default(),
    )
    .await?;
    if own.is_none() && who.str() != Some("everyone") {
        return Err(HttpError::new(
            StatusCode::FORBIDDEN,
            "Shared drives are made by the superadmin and the teams' managers",
        ));
    }
    let id = make_shared(&state, &body.name, None, backend, None, user.0.id).await?;
    state
        .files
        .grant(id, None, Grantee::User(user.0.id), Role::Manager, user.0.id)
        .await?;
    Ok((
        StatusCode::CREATED,
        Json(view_for(&state, id, user.0.id).await?),
    )
        .into_response())
}

/// A shared drive as the superadmin sees it.
pub(crate) async fn shared_view(
    state: &AppState,
    id: Uuid,
    me: Uuid,
) -> Result<SharedDrive, HttpError> {
    let row = DriveRow::get(state.db.reader(), id)
        .await?
        .filter(|d| d.kind == DriveKind::Shared)
        .ok_or_else(HttpError::not_found)?;
    let role = roles::drive_role(state.db.reader(), me, id).await?;
    let backend = match row.backend_id {
        Some(backend) => crate::backends::BackendRow::get(state.db.reader(), backend).await?,
        None => None,
    };
    let (folder, default_folder) = match &backend {
        // In a bucket: its prefix there.
        Some(backend) => (
            format!(
                "s3://{}/{}",
                backend.bucket,
                crate::storage::drive_base(&backend.prefix, id)
            ),
            false,
        ),
        None => {
            let (folder, default_folder) = state.files.drive_root(id).await?;
            (folder.to_string_lossy().into_owned(), default_folder)
        }
    };
    Ok(SharedDrive {
        drive: drive_view(state, &row, role).await?,
        folder,
        default_folder,
        created_at: row.created_at,
    })
}

#[utoipa::path(
    get,
    path = "/drives",
    tag = "Superadmin",
    operation_id = "listSharedDrives",
    summary = "The shared drives",
    description = "Every shared drive, by name, with its folder (who has access: `/superadmin/drives/{id}/access`). The superadmin sees the drives, not what is in them: only members do.",
    responses(
        (status = 200, description = "The shared drives", body = Vec<SharedDrive>),
        (status = 401, description = "Not signed in", body = ErrorBody),
        (status = 403, description = "Superadmin only, with a session (never a token) whose second factor was confirmed within 15 minutes: else `code` is `step_up_required` (confirm it, `/auth/step-up/…`) or `second_factor_required` (add one first)", body = ErrorBody),
    ),
    security(("session" = [])),
)]
async fn list_shared_drives(
    State(state): State<AppState>,
    me: Superadmin,
) -> Result<Json<Vec<SharedDrive>>, HttpError> {
    let ids: Vec<Uuid> = sqlx::query_scalar(
        "SELECT id FROM drives WHERE kind = 'shared' ORDER BY name COLLATE NOCASE, id",
    )
    .fetch_all(state.db.reader())
    .await?;
    let mut out = Vec::with_capacity(ids.len());
    for id in ids {
        out.push(shared_view(&state, id, me.0.id).await?);
    }
    Ok(Json(out))
}

/// Someone to add to a new shared drive.
#[derive(Deserialize, ToSchema)]
pub struct NewMember {
    /// The user's email.
    pub email: String,
    /// They manage it (a manager grant; else editor).
    #[serde(default)]
    pub manages: bool,
}

/// A new shared drive.
#[derive(Deserialize, ToSchema)]
pub struct NewSharedDrive {
    /// 1 to 100 characters, unique among the shared drives (any case).
    pub name: String,
    /// An existing folder on the server to make it of (absolute, apart from
    /// the data dir and every other drive); left out: a new folder in the
    /// data dir (`<DATA_DIR>/drives/<id>`).
    pub folder: Option<String>,
    /// Its members. When none of them manages it (and it is no team's),
    /// the superadmin does (and is a member).
    #[serde(default)]
    pub members: Vec<NewMember>,
    /// The team it belongs to: the team's managers manage it, its members
    /// are editors.
    pub team_id: Option<Uuid>,
    /// Bytes; left out or null: no limit (on a backend with a limit for new
    /// drives: that one).
    pub storage_limit: Option<i64>,
    /// Where it is stored: the server's disk (left out, or its id), or a
    /// global backend (not with `folder`). The superadmin makes no drive on
    /// a user's own storage.
    pub backend_id: Option<Uuid>,
}

#[utoipa::path(
    post,
    path = "/drives",
    tag = "Superadmin",
    operation_id = "createSharedDrive",
    summary = "Make a shared drive",
    request_body(content = NewSharedDrive, content_type = "application/json"),
    responses(
        (status = 201, description = "Made", body = SharedDrive),
        (status = 503, description = "Its storage backend does not answer (`code` `storage_unavailable`)", body = ErrorBody),
        (status = 400, description = "An invalid name or folder, no such user, or no such storage", body = ErrorBody),
        (status = 401, description = "Not signed in", body = ErrorBody),
        (status = 403, description = "Superadmin only, with a session (never a token) whose second factor was confirmed within 15 minutes: else `code` is `step_up_required` (confirm it, `/auth/step-up/…`) or `second_factor_required` (add one first)", body = ErrorBody),
        (status = 409, description = "A shared drive has that name", body = ErrorBody),
        (status = 415, description = "The body must be JSON (Content-Type: application/json)", body = ErrorBody),
    ),
    security(("session" = [])),
)]
async fn create_shared_drive(
    State(state): State<AppState>,
    me: Superadmin,
    ApiJson(body): ApiJson<NewSharedDrive>,
) -> Result<Response, HttpError> {
    // Every member must exist before anything is made.
    let mut members = Vec::new();
    for member in &body.members {
        members.push((user_by_email(&state, &member.email).await?, member.manages));
    }
    let team_managed = match body.team_id {
        Some(team) => {
            let managers: i64 = sqlx::query_scalar(
                "SELECT count(*) FROM team_members WHERE team_id = ?1 AND role = 'manager'",
            )
            .bind(team)
            .fetch_optional(state.db.reader())
            .await?
            .unwrap_or(0);
            let exists: Option<i64> = sqlx::query_scalar("SELECT 1 FROM teams WHERE id = ?1")
                .bind(team)
                .fetch_optional(state.db.reader())
                .await?;
            exists.ok_or_else(|| HttpError::bad_request("No such team"))?;
            managers > 0
        }
        None => false,
    };
    if !team_managed && !members.iter().any(|(_, manages)| *manages) {
        members.retain(|(id, _)| *id != me.0.id);
        members.push((me.0.id, true));
    }
    let folder = body
        .folder
        .as_deref()
        .map(str::trim)
        .filter(|f| !f.is_empty())
        .map(std::path::PathBuf::from);
    let backend = crate::routes::backends::chosen(body.backend_id);
    crate::routes::backends::check_maker(
        &state,
        backend,
        me.0.id,
        crate::routes::backends::Maker::Superadmin,
    )
    .await?;
    let id = make_shared(
        &state,
        &body.name,
        folder.as_deref(),
        backend,
        body.storage_limit,
        me.0.id,
    )
    .await?;
    // The managers first: a shared drive never is without one.
    members.sort_by_key(|(_, manages)| !*manages);
    if let Some(team) = body.team_id {
        sqlx::query("UPDATE drives SET team_id = ?1 WHERE id = ?2")
            .bind(team)
            .bind(id)
            .execute(state.db.writer())
            .await?;
        state
            .files
            .grant(id, None, Grantee::Team(team), Role::Editor, me.0.id)
            .await?;
    }
    for (member, manages) in members {
        set_member(
            &state,
            id,
            member,
            manages,
            Some(Actor::Superadmin(me.0.id)),
        )
        .await?;
    }
    Ok((
        StatusCode::CREATED,
        Json(shared_view(&state, id, me.0.id).await?),
    )
        .into_response())
}

/// What to change of a shared drive; a field left out stays as it is.
#[derive(Deserialize, ToSchema)]
pub struct UpdateSharedDrive {
    pub name: Option<String>,
    /// Bytes; null for no limit.
    #[serde(default, deserialize_with = "crate::util::present")]
    #[schema(value_type = Option<i64>, nullable)]
    pub storage_limit: Option<Option<i64>>,
    /// The team it belongs to; null for none. A shared drive keeps a
    /// manager (409).
    #[serde(default, deserialize_with = "crate::util::present")]
    #[schema(value_type = Option<Uuid>, nullable)]
    pub team_id: Option<Option<Uuid>>,
}

#[utoipa::path(
    patch,
    path = "/drives/{id}",
    tag = "Superadmin",
    operation_id = "updateSharedDrive",
    summary = "Rename a shared drive, or set its storage limit or team",
    params(("id" = String, Path, description = "The drive's id")),
    request_body(content = UpdateSharedDrive, content_type = "application/json"),
    responses(
        (status = 200, description = "Changed", body = SharedDrive),
        (status = 400, description = "An invalid name or limit", body = ErrorBody),
        (status = 401, description = "Not signed in", body = ErrorBody),
        (status = 403, description = "Superadmin only, with a session (never a token) whose second factor was confirmed within 15 minutes: else `code` is `step_up_required` (confirm it, `/auth/step-up/…`) or `second_factor_required` (add one first)", body = ErrorBody),
        (status = 404, description = "No such shared drive", body = ErrorBody),
        (status = 409, description = "A shared drive has that name", body = ErrorBody),
        (status = 415, description = "The body must be JSON (Content-Type: application/json)", body = ErrorBody),
    ),
    security(("session" = [])),
)]
async fn update_shared_drive(
    State(state): State<AppState>,
    me: Superadmin,
    Path(id): Path<String>,
    ApiJson(body): ApiJson<UpdateSharedDrive>,
) -> Result<Json<SharedDrive>, HttpError> {
    let id = drive_id(&id)?;
    shared_view(&state, id, me.0.id).await?;
    if let Some(name) = &body.name {
        state.files.rename_drive(id, name).await?;
        notify_members(&state, id).await?;
    }
    if let Some(limit) = body.storage_limit {
        state.files.set_drive_limit(id, limit).await?;
    }
    if let Some(team) = body.team_id {
        state.files.set_drive_team(id, team).await?;
    }
    Ok(Json(shared_view(&state, id, me.0.id).await?))
}

#[utoipa::path(
    delete,
    path = "/drives/{id}",
    tag = "Superadmin",
    operation_id = "deleteSharedDrive",
    summary = "Delete a shared drive",
    description = "With everything in it, its trash and versions, for good. A folder in the data dir goes; an existing folder it was made of keeps its files (only Organizer's own folder in it goes).",
    params(("id" = String, Path, description = "The drive's id")),
    responses(
        (status = 204, description = "Deleted"),
        (status = 401, description = "Not signed in", body = ErrorBody),
        (status = 403, description = "Superadmin only, with a session (never a token) whose second factor was confirmed within 15 minutes: else `code` is `step_up_required` (confirm it, `/auth/step-up/…`) or `second_factor_required` (add one first)", body = ErrorBody),
        (status = 404, description = "No such shared drive", body = ErrorBody),
    ),
    security(("session" = [])),
)]
async fn delete_shared_drive(
    State(state): State<AppState>,
    _: Superadmin,
    Path(id): Path<String>,
) -> Result<StatusCode, HttpError> {
    state.files.delete_drive(drive_id(&id)?).await?;
    Ok(StatusCode::NO_CONTENT)
}
