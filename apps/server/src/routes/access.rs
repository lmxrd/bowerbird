//! Who has access (docs/M7B_DESIGN.md §1, §6): the grants on a file or
//! folder and its limited access (`/files/{id}/access`), handing it over
//! to a new owner (`/files/{id}/owner`), and the grants on a shared drive
//! (`/drives/{id}/access`, and the superadmin's
//! `/superadmin/drives/{id}/access`). Managers decide; a grant gives at
//! most what its giver has, and never `owner`. The rules are in `files/roles.rs` and
//! `files/grants.rs`.

use crate::activity::Actor;
use crate::error::{ErrorBody, HttpError};
use crate::extract::ApiJson;
use crate::files::drives::DriveKind;
use crate::files::grants::{Grantee, grant_row, grants_of};
use crate::files::roles::{GrantRow, Role};
use crate::files::shares::Need;
use crate::routes::auth::{AuthUser, Superadmin};
use crate::routes::drives::log_drive_access;
use crate::routes::files::{Entry, access, entry};
use crate::routes::shares::{GroupRef, Person, check_gives, group_ref, tell_shared};
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

/// Nested under /files.
pub fn files_router() -> OpenApiRouter<AppState> {
    OpenApiRouter::new()
        .routes(routes!(get_access, add_grant, set_limited))
        .routes(routes!(change_grant, remove_grant))
        .routes(routes!(change_owner))
}

/// Nested under /drives.
pub fn drives_router() -> OpenApiRouter<AppState> {
    OpenApiRouter::new()
        .routes(routes!(get_drive_access, add_drive_grant))
        .routes(routes!(change_drive_grant, remove_drive_grant))
}

/// Nested under /superadmin.
pub fn superadmin_router() -> OpenApiRouter<AppState> {
    OpenApiRouter::new()
        .routes(routes!(get_shared_drive_access, add_shared_drive_grant))
        .routes(routes!(
            change_shared_drive_grant,
            remove_shared_drive_grant
        ))
}

/// A grant: a role given to a user or a team.
#[derive(Serialize, ToSchema)]
pub struct Grant {
    pub id: Uuid,
    /// The user it is to; null for a team's. Always present.
    #[schema(required = true)]
    pub user: Option<Person>,
    /// The team it is to; null for a user's. Always present.
    #[schema(required = true)]
    pub team: Option<GroupRef>,
    pub role: Role,
    /// Who gave it. Always present.
    #[schema(required = true)]
    pub created_by: Option<Person>,
    pub created_at: String,
    /// Someone on another server it is shared with (docs/M7E_DESIGN.md
    /// §4): `user` and `team` are null then; its id is changed and removed
    /// as any grant's. Null for a grant here. Always present.
    #[schema(required = true)]
    pub remote: Option<RemoteParty>,
}

/// Someone on another server a grant is to.
#[derive(Serialize, ToSchema)]
pub struct RemoteParty {
    /// Their OCM address, `user@host`.
    pub address: String,
    /// Their name, as their server gave it (may be empty).
    pub name: String,
    /// Their server, `host[:port]`.
    pub server: String,
    /// Whether they accepted it yet.
    pub state: crate::federation::outgoing::ShareState,
}

/// A share with someone on another server, as a grant.
pub(crate) async fn remote_grant_view(
    state: &AppState,
    share: crate::federation::outgoing::OcmShare,
) -> Result<Grant, HttpError> {
    Ok(Grant {
        id: share.id,
        user: None,
        team: None,
        role: share.role,
        created_by: person(state, share.created_by).await?,
        created_at: share.created_at,
        remote: Some(RemoteParty {
            address: share.address,
            name: share.recipient_name,
            server: share.host,
            state: share.state,
        }),
    })
}

/// A grant on a folder above an item, or on its drive: it reaches the
/// item.
#[derive(Serialize, ToSchema)]
pub struct InheritedGrant {
    #[serde(flatten)]
    pub grant: Grant,
    /// The folder it is on; null: the drive itself. Always present.
    #[schema(required = true)]
    pub from_id: Option<Uuid>,
    /// That folder's name, or the drive's.
    pub from_name: String,
}

/// Who has access to a file or folder.
#[derive(Serialize, ToSchema)]
pub struct ItemAccess {
    pub node_id: Uuid,
    /// The signed-in user's role on it.
    pub my_role: Role,
    /// They may change who has access (manager or owner).
    pub may_manage: bool,
    /// Its owner; null in a shared drive (which has no owner). Always
    /// present.
    #[schema(required = true)]
    pub owner: Option<Person>,
    /// Limited access: only the grants on it or below count (`inherited`
    /// is then only the drive's managers).
    pub limited: bool,
    /// The grants on it, the oldest first (not the owner's).
    pub grants: Vec<Grant>,
    /// The grants from the folders above it (up to a limited one) and its
    /// drive, nearest first.
    pub inherited: Vec<InheritedGrant>,
    /// Who keep access whatever is limited: a shared drive's managers (and
    /// its team's), My drive's user when they are not the owner.
    pub keepers: Vec<Person>,
}

async fn person(state: &AppState, id: Option<Uuid>) -> Result<Option<Person>, HttpError> {
    let Some(id) = id else { return Ok(None) };
    Ok(sqlx::query_as("SELECT id, email FROM users WHERE id = ?1")
        .bind(id)
        .fetch_optional(state.db.reader())
        .await?)
}

async fn grant_view(state: &AppState, row: GrantRow) -> Result<Grant, HttpError> {
    Ok(Grant {
        id: row.id,
        user: person(state, row.user_id).await?,
        team: match row.team_id {
            Some(team) => group_ref(state, team).await?,
            None => None,
        },
        role: row.role,
        created_by: person(state, row.created_by).await?,
        created_at: row.created_at,
        remote: None,
    })
}

/// The shares with other servers of an item or a whole drive (one the
/// recipient declined too, `state` says so: it is removed as any grant).
async fn remote_shares(
    state: &AppState,
    drive: Uuid,
    node: Option<Uuid>,
) -> Result<Vec<crate::federation::outgoing::OcmShare>, HttpError> {
    Ok(crate::federation::outgoing::shares_of(state.db.reader(), drive, node).await?)
}

/// A share with another server of an item or a whole drive, by id.
async fn remote_share(
    state: &AppState,
    drive: Uuid,
    node: Option<Uuid>,
    id: Uuid,
) -> Result<Option<crate::federation::outgoing::OcmShare>, HttpError> {
    Ok(
        crate::federation::outgoing::share_row(state.db.reader(), id)
            .await?
            .filter(|s| s.drive_id == drive && s.node_id == node),
    )
}

/// The address of a new grant to someone on another server.
fn remote_address(
    body: &NewGrant,
) -> Result<Option<crate::federation::address::Address>, HttpError> {
    let Some(address) = body.address.as_deref() else {
        return Ok(None);
    };
    if body.user_id.is_some() || body.email.is_some() || body.team_id.is_some() {
        return Err(HttpError::bad_request(
            "Give access to one user, one team or one address on another server",
        ));
    }
    crate::federation::address::Address::parse(address)
        .map(Some)
        .ok_or_else(|| {
            HttpError::bad_request("That is not an address on another server (user@host)")
        })
}

/// The keepers of a drive: its managers by grant (users, teams' members),
/// its team's managers, and My drive's user.
async fn keepers(state: &AppState, drive: Uuid) -> Result<Vec<Person>, HttpError> {
    Ok(sqlx::query_as(
        "SELECT DISTINCT u.id, u.email FROM drive_users d JOIN users u ON u.id = d.user_id
         WHERE d.drive_id = ?1 AND d.role IN ('manager', 'owner') ORDER BY u.email",
    )
    .bind(drive)
    .fetch_all(state.db.reader())
    .await?)
}

async fn item_access(state: &AppState, user: &AuthUser, id: Uuid) -> Result<ItemAccess, HttpError> {
    let node = access(state, user, id, Need::Read).await?;
    if node.role == Role::UploadOnly {
        return Err(HttpError::new(
            StatusCode::FORBIDDEN,
            "Upload only does not show who has access",
        ));
    }
    let drive_name: String = sqlx::query_scalar("SELECT name FROM drives WHERE id = ?1")
        .bind(node.owner)
        .fetch_one(state.db.reader())
        .await?;
    let limited: bool = sqlx::query_scalar("SELECT limited FROM nodes WHERE id = ?1")
        .bind(id)
        .fetch_one(state.db.reader())
        .await?;
    let grants = grants_of(state.db.reader(), node.owner, Some(id)).await?;
    let mut mine = Vec::new();
    for g in grants.into_iter().filter(|g| g.role != Role::Owner) {
        mine.push(grant_view(state, g).await?);
    }
    for share in remote_shares(state, node.owner, Some(id)).await? {
        mine.push(remote_grant_view(state, share).await?);
    }
    let mut inherited = Vec::new();
    if !limited {
        let mut conn = state.db.reader().acquire().await?;
        let chain = crate::files::roles::chain(&mut conn, id).await?;
        drop(conn);
        let mut cut = false;
        for up in chain.iter().skip(1) {
            let name: String = sqlx::query_scalar("SELECT name FROM nodes WHERE id = ?1")
                .bind(up.id)
                .fetch_one(state.db.reader())
                .await?;
            for g in grants_of(state.db.reader(), node.owner, Some(up.id)).await? {
                if g.role == Role::Owner {
                    continue;
                }
                inherited.push(InheritedGrant {
                    grant: grant_view(state, g).await?,
                    from_id: Some(up.id),
                    from_name: name.clone(),
                });
            }
            for share in remote_shares(state, node.owner, Some(up.id)).await? {
                inherited.push(InheritedGrant {
                    grant: remote_grant_view(state, share).await?,
                    from_id: Some(up.id),
                    from_name: name.clone(),
                });
            }
            if up.limited {
                cut = true;
                break;
            }
        }
        if !cut {
            let mut drive_grants = Vec::new();
            for g in grants_of(state.db.reader(), node.owner, None).await? {
                drive_grants.push(grant_view(state, g).await?);
            }
            for share in remote_shares(state, node.owner, None).await? {
                drive_grants.push(remote_grant_view(state, share).await?);
            }
            for grant in drive_grants {
                inherited.push(InheritedGrant {
                    grant,
                    from_id: None,
                    from_name: if drive_name.is_empty() {
                        "My drive".into()
                    } else {
                        drive_name.clone()
                    },
                });
            }
        }
    }
    let owner = state.files.owner_user(id).await?;
    let mut keep = keepers(state, node.owner).await?;
    keep.retain(|p| Some(p.id) != owner);
    Ok(ItemAccess {
        node_id: id,
        my_role: node.role,
        may_manage: node.allows(Need::Share),
        owner: person(state, owner).await?,
        limited,
        grants: mine,
        inherited,
        keepers: keep,
    })
}

#[utoipa::path(
    get,
    path = "/{id}/access",
    tag = "Sharing",
    operation_id = "getAccess",
    summary = "Who has access to a file or folder",
    description = "Its owner, the grants on it, the grants that reach it from the folders above and its drive (unless it is limited), and who keep access whatever is limited. For anyone with access but upload only.",
    params(("id" = Uuid, Path, description = "The file or folder id")),
    responses(
        (status = 200, description = "Who has access", body = ItemAccess),
        (status = 401, description = "Not signed in", body = ErrorBody),
        (status = 403, description = "Upload only", body = ErrorBody),
        (status = 404, description = "Not found", body = ErrorBody),
    ),
    security(("session" = []), ("api_key" = [])),
)]
async fn get_access(
    State(state): State<AppState>,
    user: AuthUser,
    Path(id): Path<Uuid>,
) -> Result<Json<ItemAccess>, HttpError> {
    Ok(Json(item_access(&state, &user, id).await?))
}

/// A new grant: to a user (by id or email) or a team, with a role.
#[derive(Deserialize, ToSchema)]
pub struct NewGrant {
    /// The user; or `email`, or `team_id`.
    pub user_id: Option<Uuid>,
    /// The user's email.
    pub email: Option<String>,
    /// The team.
    pub team_id: Option<Uuid>,
    /// Someone on another server, by their address `user@host`
    /// (docs/M7E_DESIGN.md §4): a contact, anyone on a server the
    /// superadmin listed, or any address where `federation.share_with_any`
    /// allows it. `manager`, `editor` or `viewer` (a whole drive takes
    /// `upload_only` too, between Organizer servers). Their server is asked
    /// at once; a refusal is a 403 or 400 with `code` `federation_off`,
    /// `server_refused`, `contact_required`, `federation_forbidden` (the
    /// drive is not shared with other servers), `e2e_unsupported` (an
    /// encrypted folder, to a server without `organizer-e2e`),
    /// `drives_unsupported`, or a 502 with `remote_unreachable` or
    /// `remote_refused`.
    #[serde(default)]
    pub address: Option<String>,
    /// `manager`, `editor`, `viewer` or `upload_only` (never `owner`: hand
    /// it over instead), at most the giver's own.
    pub role: Role,
}

/// Who a new grant is to.
async fn grantee(state: &AppState, body: &NewGrant) -> Result<Grantee, HttpError> {
    match (body.user_id, body.email.as_deref(), body.team_id) {
        (Some(user), None, None) => Ok(Grantee::User(user)),
        (None, Some(email), None) => {
            let user: Uuid = sqlx::query_scalar(
                "SELECT id FROM users WHERE email = ?1 COLLATE NOCASE AND disabled_at IS NULL",
            )
            .bind(email.trim())
            .fetch_optional(state.db.reader())
            .await?
            .ok_or_else(|| HttpError::bad_request("No user has that email"))?;
            Ok(Grantee::User(user))
        }
        (None, None, Some(team)) => Ok(Grantee::Team(team)),
        _ => Err(HttpError::bad_request(
            "Give access to one user (user_id or email) or one team (team_id)",
        )),
    }
}

#[utoipa::path(
    post,
    path = "/{id}/access",
    tag = "Sharing",
    operation_id = "addGrant",
    summary = "Give a user or a team a role on a file or folder",
    description = "By a manager (or the owner), giving at most their own role and never `owner`. It reaches everything under it; on a limited folder it is how someone gets in. Those it reaches see it in \"Shared with me\" (but for a drive's members) and get a notice.",
    params(("id" = Uuid, Path, description = "The file or folder id")),
    request_body(content = NewGrant, content_type = "application/json"),
    responses(
        (status = 201, description = "Given", body = Grant),
        (status = 400, description = "Not one user or team, yourself or its owner, the drive's root, inside an encrypted folder, or `owner`", body = ErrorBody),
        (status = 401, description = "Not signed in", body = ErrorBody),
        (status = 403, description = "Not a manager, more than you have, or a read-only key", body = ErrorBody),
        (status = 404, description = "Not found (the file, the user or the team)", body = ErrorBody),
        (status = 409, description = "They have a grant here already: change it", body = ErrorBody),
        (status = 415, description = "The body must be JSON (Content-Type: application/json)", body = ErrorBody),
    ),
    security(("session" = []), ("api_key" = [])),
)]
async fn add_grant(
    State(state): State<AppState>,
    user: AuthUser,
    Path(id): Path<Uuid>,
    ApiJson(body): ApiJson<NewGrant>,
) -> Result<Response, HttpError> {
    let node = access(&state, &user, id, Need::Share).await?;
    check_gives(node.role, body.role)?;
    // Another server's file is shared by its server only (OCM §8.3: a
    // receiving server never reshares itself).
    if node.node.remote.is_some() {
        return Err(HttpError::bad_request(
            "This is on another server: only its owner's server can share it",
        ));
    }
    if let Some(address) = remote_address(&body)? {
        let share = crate::federation::outgoing::share_out(
            &state.files,
            user.0.id,
            node.owner,
            Some(id),
            &address,
            body.role,
        )
        .await?;
        return Ok((
            StatusCode::CREATED,
            Json(remote_grant_view(&state, share).await?),
        )
            .into_response());
    }
    let to = grantee(&state, &body).await?;
    if let Grantee::User(to) = to
        && (to == user.0.id || state.files.owner_user(id).await? == Some(to))
    {
        return Err(HttpError::bad_request(
            "They have it already: give access to someone else",
        ));
    }
    let row = state
        .files
        .grant(node.owner, Some(id), to, body.role, user.0.id)
        .await?;
    tell_shared(&state, user.0.id, to).await?;
    Ok((StatusCode::CREATED, Json(grant_view(&state, row).await?)).into_response())
}

/// Limited access on a folder.
#[derive(Deserialize, ToSchema)]
pub struct AccessChange {
    /// On: from this folder down only the grants on it or below count;
    /// everyone else (but the owner and the drive's managers) does not see
    /// it at all.
    pub limited: bool,
}

#[utoipa::path(
    patch,
    path = "/{id}/access",
    tag = "Sharing",
    operation_id = "setLimitedAccess",
    summary = "Turn limited access on a folder on or off",
    description = "By a manager. Those who no longer see it lose it at once (their change feed says it went; what they placed of it leaves their files); those who see it again get it back.",
    params(("id" = Uuid, Path, description = "The folder id")),
    request_body(content = AccessChange, content_type = "application/json"),
    responses(
        (status = 200, description = "Who has access now", body = ItemAccess),
        (status = 400, description = "A file, or a drive's root", body = ErrorBody),
        (status = 401, description = "Not signed in", body = ErrorBody),
        (status = 403, description = "Not a manager, or a read-only key", body = ErrorBody),
        (status = 404, description = "Not found", body = ErrorBody),
        (status = 415, description = "The body must be JSON (Content-Type: application/json)", body = ErrorBody),
    ),
    security(("session" = []), ("api_key" = [])),
)]
async fn set_limited(
    State(state): State<AppState>,
    user: AuthUser,
    Path(id): Path<Uuid>,
    ApiJson(body): ApiJson<AccessChange>,
) -> Result<Json<ItemAccess>, HttpError> {
    access(&state, &user, id, Need::Share).await?;
    state.files.set_limited(id, body.limited).await?;
    Ok(Json(item_access(&state, &user, id).await?))
}

/// A grant's new role.
#[derive(Deserialize, ToSchema)]
pub struct GrantChange {
    /// At most the changer's own, never `owner`.
    pub role: Role,
}

/// A grant on an item, which the user manages.
async fn item_grant(
    state: &AppState,
    user: &AuthUser,
    node: Uuid,
    id: Uuid,
) -> Result<(GrantRow, Role), HttpError> {
    let access = access(state, user, node, Need::Share).await?;
    let row = grant_row(state.db.reader(), id)
        .await?
        .filter(|g| g.node_id == Some(node) && g.role != Role::Owner)
        .ok_or_else(HttpError::not_found)?;
    Ok((row, access.role))
}

#[utoipa::path(
    patch,
    path = "/{id}/access/{grant_id}",
    tag = "Sharing",
    operation_id = "changeGrant",
    summary = "Change the role of a grant on a file or folder",
    params(
        ("id" = Uuid, Path, description = "The file or folder id"),
        ("grant_id" = Uuid, Path, description = "The grant"),
    ),
    request_body(content = GrantChange, content_type = "application/json"),
    responses(
        (status = 200, description = "Changed", body = Grant),
        (status = 400, description = "`owner`", body = ErrorBody),
        (status = 401, description = "Not signed in", body = ErrorBody),
        (status = 403, description = "Not a manager, more than you have, or a read-only key", body = ErrorBody),
        (status = 404, description = "Not found", body = ErrorBody),
        (status = 415, description = "The body must be JSON (Content-Type: application/json)", body = ErrorBody),
    ),
    security(("session" = []), ("api_key" = [])),
)]
async fn change_grant(
    State(state): State<AppState>,
    user: AuthUser,
    Path((node, id)): Path<(Uuid, Uuid)>,
    ApiJson(body): ApiJson<GrantChange>,
) -> Result<Json<Grant>, HttpError> {
    let item = access(&state, &user, node, Need::Share).await?;
    if let Some(share) = remote_share(&state, item.owner, Some(node), id).await? {
        check_gives(item.role, body.role)?;
        let share =
            crate::federation::outgoing::change_role(&state.files, &share, body.role).await?;
        return Ok(Json(remote_grant_view(&state, share).await?));
    }
    let (_, have) = item_grant(&state, &user, node, id).await?;
    check_gives(have, body.role)?;
    let row = state.files.change_grant(id, body.role).await?;
    Ok(Json(grant_view(&state, row).await?))
}

#[utoipa::path(
    delete,
    path = "/{id}/access/{grant_id}",
    tag = "Sharing",
    operation_id = "removeGrant",
    summary = "Take a grant on a file or folder away",
    description = "Those who lose the item through it lose it at once; what they placed of it leaves their files.",
    params(
        ("id" = Uuid, Path, description = "The file or folder id"),
        ("grant_id" = Uuid, Path, description = "The grant"),
    ),
    responses(
        (status = 204, description = "Taken away"),
        (status = 401, description = "Not signed in", body = ErrorBody),
        (status = 403, description = "Not a manager, or a read-only key", body = ErrorBody),
        (status = 404, description = "Not found", body = ErrorBody),
    ),
    security(("session" = []), ("api_key" = [])),
)]
async fn remove_grant(
    State(state): State<AppState>,
    user: AuthUser,
    Path((node, id)): Path<(Uuid, Uuid)>,
) -> Result<StatusCode, HttpError> {
    let item = access(&state, &user, node, Need::Share).await?;
    if let Some(share) = crate::federation::outgoing::share_row(state.db.reader(), id)
        .await?
        .filter(|s| s.drive_id == item.owner && s.node_id == Some(node))
    {
        crate::federation::outgoing::unshare(&state.files, &share).await?;
        return Ok(StatusCode::NO_CONTENT);
    }
    item_grant(&state, &user, node, id).await?;
    state.files.revoke(id).await?;
    Ok(StatusCode::NO_CONTENT)
}

/// The new owner.
#[derive(Deserialize, ToSchema)]
pub struct NewOwner {
    /// The user; or `email`.
    pub user_id: Option<Uuid>,
    pub email: Option<String>,
}

#[utoipa::path(
    patch,
    path = "/{id}/owner",
    tag = "Sharing",
    operation_id = "changeOwner",
    summary = "Hand a file or folder over to another user",
    description = "By its owner, in My drive only (a shared drive has no owner). They become its owner and the previous owner its manager; it stays where it is (and counts in the drive it is in).",
    params(("id" = Uuid, Path, description = "The file or folder id")),
    request_body(content = NewOwner, content_type = "application/json"),
    responses(
        (status = 200, description = "Handed over: the item as the previous owner sees it now", body = Entry),
        (status = 400, description = "Not one user, themselves, My drive's root, or a shared drive", body = ErrorBody),
        (status = 401, description = "Not signed in", body = ErrorBody),
        (status = 403, description = "Not the owner, or a read-only key", body = ErrorBody),
        (status = 404, description = "Not found (the file or the user)", body = ErrorBody),
        (status = 415, description = "The body must be JSON (Content-Type: application/json)", body = ErrorBody),
    ),
    security(("session" = []), ("api_key" = [])),
)]
async fn change_owner(
    State(state): State<AppState>,
    user: AuthUser,
    Path(id): Path<Uuid>,
    ApiJson(body): ApiJson<NewOwner>,
) -> Result<Json<Entry>, HttpError> {
    let node = access(&state, &user, id, Need::Read).await?;
    if !node.my_drive {
        return Err(HttpError::bad_request(
            "A shared drive has no owner: its managers decide",
        ));
    }
    node.require(Need::Own)?;
    let to = grantee(
        &state,
        &NewGrant {
            user_id: body.user_id,
            email: body.email,
            team_id: None,
            address: None,
            role: Role::Owner,
        },
    )
    .await?;
    let Grantee::User(to) = to else {
        return Err(HttpError::bad_request("Hand it over to a user"));
    };
    state.files.change_owner(id, to, user.0.id).await?;
    tell_shared(&state, user.0.id, Grantee::User(to)).await?;
    let now = access(&state, &user, id, Need::Read).await?;
    Ok(Json(entry(&state, user.0.id, &now).await?))
}

/// Who has access to a shared drive.
#[derive(Serialize, ToSchema)]
pub struct DriveAccess {
    pub drive_id: String,
    pub my_role: Role,
    pub may_manage: bool,
    /// The grants on the drive, the oldest first.
    pub grants: Vec<Grant>,
    /// The team it belongs to; null for none. Always present.
    #[schema(required = true)]
    pub team: Option<GroupRef>,
    /// The team's managers: they manage the drive.
    pub team_managers: Vec<Person>,
    /// Who mirror it, each to their own storage (docs/M7D_DESIGN.md §3):
    /// "Mirrored by alice@example.com (her own storage)". Mirrors that
    /// stopped are not listed.
    pub mirrored_by: Vec<crate::routes::mirrors::MirroredBy>,
}

/// A shared drive of the user's, and their role on it.
async fn shared_drive(state: &AppState, user: Uuid, id: &str) -> Result<(Uuid, Role), HttpError> {
    let id: Uuid = id.parse().map_err(|_| HttpError::not_found())?;
    let role = crate::files::roles::drive_role(state.db.reader(), user, id)
        .await?
        .ok_or_else(HttpError::not_found)?;
    let row = crate::files::drives::DriveRow::get(state.db.reader(), id)
        .await?
        .ok_or_else(HttpError::not_found)?;
    if row.kind != DriveKind::Shared {
        return Err(HttpError::bad_request(
            "My drive's access is given on its folders and files",
        ));
    }
    Ok((id, role))
}

fn manages(role: Role) -> Result<(), HttpError> {
    if role >= Role::Manager {
        return Ok(());
    }
    Err(HttpError::new(
        StatusCode::FORBIDDEN,
        "Only who manages this drive can do that",
    ))
}

async fn drive_access(state: &AppState, drive: Uuid, role: Role) -> Result<DriveAccess, HttpError> {
    let mut grants = Vec::new();
    for g in grants_of(state.db.reader(), drive, None).await? {
        grants.push(grant_view(state, g).await?);
    }
    for share in remote_shares(state, drive, None).await? {
        grants.push(remote_grant_view(state, share).await?);
    }
    let team: Option<Uuid> = sqlx::query_scalar("SELECT team_id FROM drives WHERE id = ?1")
        .bind(drive)
        .fetch_one(state.db.reader())
        .await?;
    let team_managers: Vec<Person> = sqlx::query_as(
        "SELECT u.id, u.email FROM team_members m JOIN users u ON u.id = m.user_id
         WHERE m.team_id = ?1 AND m.role = 'manager' ORDER BY u.email",
    )
    .bind(team)
    .fetch_all(state.db.reader())
    .await?;
    Ok(DriveAccess {
        drive_id: drive.to_string(),
        my_role: role,
        may_manage: role >= Role::Manager,
        grants,
        team: match team {
            Some(team) => group_ref(state, team).await?,
            None => None,
        },
        team_managers,
        mirrored_by: crate::routes::mirrors::mirrored_by(state, drive).await?,
    })
}

#[utoipa::path(
    get,
    path = "/{id}/access",
    tag = "Drives",
    operation_id = "getDriveAccess",
    summary = "Who has access to a shared drive",
    description = "Its grants (users and teams, with their roles), its team and the team's managers. For its members.",
    params(("id" = String, Path, description = "The drive's id")),
    responses(
        (status = 200, description = "Who has access", body = DriveAccess),
        (status = 400, description = "My drive", body = ErrorBody),
        (status = 401, description = "Not signed in", body = ErrorBody),
        (status = 404, description = "Not a drive of the user's", body = ErrorBody),
    ),
    security(("session" = []), ("api_key" = [])),
)]
async fn get_drive_access(
    State(state): State<AppState>,
    user: AuthUser,
    Path(id): Path<String>,
) -> Result<Json<DriveAccess>, HttpError> {
    let (drive, role) = shared_drive(&state, user.0.id, &id).await?;
    Ok(Json(drive_access(&state, drive, role).await?))
}

#[utoipa::path(
    post,
    path = "/{id}/access",
    tag = "Drives",
    operation_id = "addDriveGrant",
    summary = "Give a user or a team a role on a shared drive",
    description = "By its managers; at most manager. They get the drive at once (its root appears in their change feed).",
    params(("id" = String, Path, description = "The drive's id")),
    request_body(content = NewGrant, content_type = "application/json"),
    responses(
        (status = 201, description = "Given", body = Grant),
        (status = 400, description = "Not one user or team, My drive, or `owner`", body = ErrorBody),
        (status = 401, description = "Not signed in", body = ErrorBody),
        (status = 403, description = "Not a manager of it, or a read-only key", body = ErrorBody),
        (status = 404, description = "Not a drive of the user's, or no such user or team", body = ErrorBody),
        (status = 409, description = "They have a grant on it already: change it", body = ErrorBody),
        (status = 415, description = "The body must be JSON (Content-Type: application/json)", body = ErrorBody),
    ),
    security(("session" = []), ("api_key" = [])),
)]
async fn add_drive_grant(
    State(state): State<AppState>,
    user: AuthUser,
    Path(id): Path<String>,
    ApiJson(body): ApiJson<NewGrant>,
) -> Result<Response, HttpError> {
    let (drive, role) = shared_drive(&state, user.0.id, &id).await?;
    manages(role)?;
    check_gives(role, body.role)?;
    // A whole drive for someone on another Organizer server
    // (`organizer-drives`).
    if let Some(address) = remote_address(&body)? {
        let share = crate::federation::outgoing::share_out(
            &state.files,
            user.0.id,
            drive,
            None,
            &address,
            body.role,
        )
        .await?;
        return Ok((
            StatusCode::CREATED,
            Json(remote_grant_view(&state, share).await?),
        )
            .into_response());
    }
    let to = grantee(&state, &body).await?;
    let row = state
        .files
        .grant(drive, None, to, body.role, user.0.id)
        .await?;
    Ok((StatusCode::CREATED, Json(grant_view(&state, row).await?)).into_response())
}

/// A grant on a drive, which the user manages.
async fn drive_grant(
    state: &AppState,
    user: &AuthUser,
    id: &str,
    grant: Uuid,
) -> Result<Role, HttpError> {
    let (drive, role) = shared_drive(state, user.0.id, id).await?;
    manages(role)?;
    grant_on_drive(state, drive, grant).await?;
    Ok(role)
}

#[utoipa::path(
    patch,
    path = "/{id}/access/{grant_id}",
    tag = "Drives",
    operation_id = "changeDriveGrant",
    summary = "Change the role of a grant on a shared drive",
    description = "A shared drive keeps a manager (409).",
    params(
        ("id" = String, Path, description = "The drive's id"),
        ("grant_id" = Uuid, Path, description = "The grant"),
    ),
    request_body(content = GrantChange, content_type = "application/json"),
    responses(
        (status = 200, description = "Changed", body = Grant),
        (status = 400, description = "`owner`, or My drive", body = ErrorBody),
        (status = 401, description = "Not signed in", body = ErrorBody),
        (status = 403, description = "Not a manager of it, or a read-only key", body = ErrorBody),
        (status = 404, description = "Not found", body = ErrorBody),
        (status = 409, description = "It would keep no manager", body = ErrorBody),
        (status = 415, description = "The body must be JSON (Content-Type: application/json)", body = ErrorBody),
    ),
    security(("session" = []), ("api_key" = [])),
)]
async fn change_drive_grant(
    State(state): State<AppState>,
    user: AuthUser,
    Path((id, grant)): Path<(String, Uuid)>,
    ApiJson(body): ApiJson<GrantChange>,
) -> Result<Json<Grant>, HttpError> {
    let (drive, have) = shared_drive(&state, user.0.id, &id).await?;
    if let Some(share) = remote_share(&state, drive, None, grant).await? {
        manages(have)?;
        check_gives(have, body.role)?;
        let share =
            crate::federation::outgoing::change_role(&state.files, &share, body.role).await?;
        return Ok(Json(remote_grant_view(&state, share).await?));
    }
    let role = drive_grant(&state, &user, &id, grant).await?;
    check_gives(role, body.role)?;
    let row = state.files.change_grant(grant, body.role).await?;
    Ok(Json(grant_view(&state, row).await?))
}

#[utoipa::path(
    delete,
    path = "/{id}/access/{grant_id}",
    tag = "Drives",
    operation_id = "removeDriveGrant",
    summary = "Take a grant on a shared drive away",
    description = "By its managers, or by the user it is to (leaving). Those who lose the drive through it lose it at once. A shared drive keeps a manager (409).",
    params(
        ("id" = String, Path, description = "The drive's id"),
        ("grant_id" = Uuid, Path, description = "The grant"),
    ),
    responses(
        (status = 204, description = "Taken away"),
        (status = 400, description = "My drive", body = ErrorBody),
        (status = 401, description = "Not signed in", body = ErrorBody),
        (status = 403, description = "Not a manager of it (nor its grant's user), or a read-only key", body = ErrorBody),
        (status = 404, description = "Not found", body = ErrorBody),
        (status = 409, description = "It would keep no manager", body = ErrorBody),
    ),
    security(("session" = []), ("api_key" = [])),
)]
async fn remove_drive_grant(
    State(state): State<AppState>,
    user: AuthUser,
    Path((id, grant)): Path<(String, Uuid)>,
) -> Result<StatusCode, HttpError> {
    let (drive, role) = shared_drive(&state, user.0.id, &id).await?;
    if let Some(share) = remote_share(&state, drive, None, grant).await? {
        manages(role)?;
        crate::federation::outgoing::unshare(&state.files, &share).await?;
        return Ok(StatusCode::NO_CONTENT);
    }
    let row = grant_on_drive(&state, drive, grant).await?;
    // One's own grant is one's to give up: leaving the drive.
    if row.user_id != Some(user.0.id) {
        manages(role)?;
    }
    state.files.revoke(grant).await?;
    Ok(StatusCode::NO_CONTENT)
}

/// Any shared drive, for the superadmin (who need not be a member).
async fn any_shared_drive(state: &AppState, id: &str) -> Result<Uuid, HttpError> {
    let id: Uuid = id.parse().map_err(|_| HttpError::not_found())?;
    crate::files::drives::DriveRow::get(state.db.reader(), id)
        .await?
        .filter(|d| d.kind == DriveKind::Shared)
        .ok_or_else(HttpError::not_found)?;
    Ok(id)
}

/// A grant on a shared drive itself (not on an item in it).
async fn grant_on_drive(state: &AppState, drive: Uuid, grant: Uuid) -> Result<GrantRow, HttpError> {
    grant_row(state.db.reader(), grant)
        .await?
        .filter(|g| g.drive_id == drive && g.node_id.is_none())
        .ok_or_else(HttpError::not_found)
}

/// Note a change of a user's grant on a drive in their account activity
/// (a team's grant reaches its members through the team: not noted).
async fn log_grant(
    state: &AppState,
    drive: Uuid,
    user: Option<Uuid>,
    role: Option<Role>,
    by: Uuid,
) -> Result<(), HttpError> {
    match user {
        Some(user) => log_drive_access(state, drive, user, role, Some(Actor::Superadmin(by))).await,
        None => Ok(()),
    }
}

const SUPERADMIN_ONLY: &str = "Superadmin only, with a session (never a token) whose second factor was confirmed within 15 minutes: else `code` is `step_up_required` (confirm it, `/auth/step-up/…`) or `second_factor_required` (add one first)";

#[utoipa::path(
    get,
    path = "/drives/{id}/access",
    tag = "Superadmin",
    operation_id = "getSharedDriveAccess",
    summary = "Who has access to a shared drive, as the superadmin",
    description = "As `/drives/{id}/access`, for any shared drive, the superadmin a member of it or not; `my_role` is `manager` (what the superadmin may do to its access) and says nothing of what they may do in it.",
    params(("id" = String, Path, description = "The drive's id")),
    responses(
        (status = 200, description = "Who has access", body = DriveAccess),
        (status = 401, description = "Not signed in", body = ErrorBody),
        (status = 403, description = SUPERADMIN_ONLY, body = ErrorBody),
        (status = 404, description = "No such shared drive", body = ErrorBody),
    ),
    security(("session" = [])),
)]
async fn get_shared_drive_access(
    State(state): State<AppState>,
    _: Superadmin,
    Path(id): Path<String>,
) -> Result<Json<DriveAccess>, HttpError> {
    let drive = any_shared_drive(&state, &id).await?;
    Ok(Json(drive_access(&state, drive, Role::Manager).await?))
}

#[utoipa::path(
    post,
    path = "/drives/{id}/access",
    tag = "Superadmin",
    operation_id = "addSharedDriveGrant",
    summary = "Give a user or a team a role on a shared drive, as the superadmin",
    description = "At most manager. They get the drive at once; a user's grant is noted in their account activity.",
    params(("id" = String, Path, description = "The drive's id")),
    request_body(content = NewGrant, content_type = "application/json"),
    responses(
        (status = 201, description = "Given", body = Grant),
        (status = 400, description = "Not one user or team", body = ErrorBody),
        (status = 401, description = "Not signed in", body = ErrorBody),
        (status = 403, description = "Superadmin only, with a session (never a token) whose second factor was confirmed within 15 minutes (else `code` is `step_up_required` or `second_factor_required`); or more than manager", body = ErrorBody),
        (status = 404, description = "No such shared drive, user or team", body = ErrorBody),
        (status = 409, description = "They have a grant on it already: change it", body = ErrorBody),
        (status = 415, description = "The body must be JSON (Content-Type: application/json)", body = ErrorBody),
    ),
    security(("session" = [])),
)]
async fn add_shared_drive_grant(
    State(state): State<AppState>,
    me: Superadmin,
    Path(id): Path<String>,
    ApiJson(body): ApiJson<NewGrant>,
) -> Result<Response, HttpError> {
    let drive = any_shared_drive(&state, &id).await?;
    check_gives(Role::Manager, body.role)?;
    let to = grantee(&state, &body).await?;
    let row = state
        .files
        .grant(drive, None, to, body.role, me.0.id)
        .await?;
    log_grant(&state, drive, row.user_id, Some(row.role), me.0.id).await?;
    Ok((StatusCode::CREATED, Json(grant_view(&state, row).await?)).into_response())
}

#[utoipa::path(
    patch,
    path = "/drives/{id}/access/{grant_id}",
    tag = "Superadmin",
    operation_id = "changeSharedDriveGrant",
    summary = "Change the role of a grant on a shared drive, as the superadmin",
    description = "At most manager; a shared drive keeps a manager (409). A user's change is noted in their account activity.",
    params(
        ("id" = String, Path, description = "The drive's id"),
        ("grant_id" = Uuid, Path, description = "The grant"),
    ),
    request_body(content = GrantChange, content_type = "application/json"),
    responses(
        (status = 200, description = "Changed", body = Grant),
        (status = 401, description = "Not signed in", body = ErrorBody),
        (status = 403, description = "Superadmin only, with a session (never a token) whose second factor was confirmed within 15 minutes (else `code` is `step_up_required` or `second_factor_required`); or more than manager", body = ErrorBody),
        (status = 404, description = "Not found", body = ErrorBody),
        (status = 409, description = "It would keep no manager", body = ErrorBody),
        (status = 415, description = "The body must be JSON (Content-Type: application/json)", body = ErrorBody),
    ),
    security(("session" = [])),
)]
async fn change_shared_drive_grant(
    State(state): State<AppState>,
    me: Superadmin,
    Path((id, grant)): Path<(String, Uuid)>,
    ApiJson(body): ApiJson<GrantChange>,
) -> Result<Json<Grant>, HttpError> {
    let drive = any_shared_drive(&state, &id).await?;
    let was = grant_on_drive(&state, drive, grant).await?;
    check_gives(Role::Manager, body.role)?;
    let row = state.files.change_grant(grant, body.role).await?;
    if was.role != row.role {
        log_grant(&state, drive, row.user_id, Some(row.role), me.0.id).await?;
    }
    Ok(Json(grant_view(&state, row).await?))
}

#[utoipa::path(
    delete,
    path = "/drives/{id}/access/{grant_id}",
    tag = "Superadmin",
    operation_id = "removeSharedDriveGrant",
    summary = "Take a grant on a shared drive away, as the superadmin",
    description = "Those who lose the drive through it lose it at once; a shared drive keeps a manager (409). A user's is noted in their account activity.",
    params(
        ("id" = String, Path, description = "The drive's id"),
        ("grant_id" = Uuid, Path, description = "The grant"),
    ),
    responses(
        (status = 204, description = "Taken away"),
        (status = 401, description = "Not signed in", body = ErrorBody),
        (status = 403, description = SUPERADMIN_ONLY, body = ErrorBody),
        (status = 404, description = "Not found", body = ErrorBody),
        (status = 409, description = "It would keep no manager", body = ErrorBody),
    ),
    security(("session" = [])),
)]
async fn remove_shared_drive_grant(
    State(state): State<AppState>,
    me: Superadmin,
    Path((id, grant)): Path<(String, Uuid)>,
) -> Result<StatusCode, HttpError> {
    let drive = any_shared_drive(&state, &id).await?;
    let was = grant_on_drive(&state, drive, grant).await?;
    state.files.revoke(grant).await?;
    log_grant(&state, drive, was.user_id, None, me.0.id).await?;
    Ok(StatusCode::NO_CONTENT)
}
