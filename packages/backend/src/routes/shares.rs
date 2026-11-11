//! Sharing with people and teams: what is shared with the user (`/shared`:
//! place, take out, decline) and who can be shared with. M6b's shares of a
//! file or folder (`/files/{id}/shares`) are grants now
//! (docs/M7B_DESIGN.md §2, `/files/{id}/access`): these endpoints answer
//! for one release, a share's permissions read and written as the role
//! they stand for. The rules are in `files/roles.rs` and `files/shares.rs`.

use crate::error::{ErrorBody, HttpError};
use crate::extract::ApiJson;
use crate::files::Node;
use crate::files::grants::{Grantee, grant_row, grants_of};
use crate::files::roles::{GrantRow, Role};
use crate::files::shares::{Need, Perms};
use crate::routes::auth::AuthUser;
use crate::routes::files::{Entry, access, entry};
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
        .routes(routes!(list_shares, add_share))
        .routes(routes!(change_share, remove_share))
}

/// Nested under /shared.
pub fn router() -> OpenApiRouter<AppState> {
    OpenApiRouter::new()
        .routes(routes!(shared_with_me))
        .routes(routes!(mark_seen))
        .routes(routes!(recipients))
        .routes(routes!(place, unplace))
        .routes(routes!(decline))
}

/// A user, as sharing shows them.
#[derive(Serialize, ToSchema, sqlx::FromRow)]
pub struct Person {
    pub id: Uuid,
    pub email: String,
}

/// A team, as sharing shows it (M6b's groups are teams).
#[derive(Serialize, ToSchema)]
pub struct GroupRef {
    pub id: Uuid,
    pub name: String,
    /// How many members it has.
    pub members: i64,
}

/// A share of a file or folder: a grant, as M6b showed it.
#[derive(Serialize, ToSchema)]
pub struct Share {
    /// The grant's id.
    pub id: Uuid,
    /// The user it is shared with; null for a group's share. Always
    /// present.
    #[schema(required = true)]
    pub user: Option<Person>,
    /// The team it is shared with; null for a user's share. Always
    /// present.
    #[schema(required = true)]
    pub group: Option<GroupRef>,
    /// Read is always given; these besides (what the role allows).
    pub permissions: Perms,
    /// The role it gives.
    pub role: Role,
    /// Who shared it: the owner, or someone with re-share.
    #[schema(required = true)]
    pub created_by: Option<Person>,
    pub created_at: String,
    /// Whether the user may change or remove it: a manager of the item.
    pub editable: bool,
}

async fn person(state: &AppState, id: Uuid) -> Result<Option<Person>, HttpError> {
    Ok(sqlx::query_as("SELECT id, email FROM users WHERE id = ?1")
        .bind(id)
        .fetch_optional(state.db.reader())
        .await?)
}

pub(crate) async fn group_ref(state: &AppState, id: Uuid) -> Result<Option<GroupRef>, HttpError> {
    let row: Option<(Uuid, String, i64)> = sqlx::query_as(
        "SELECT id, name, (SELECT COUNT(*) FROM team_members m WHERE m.team_id = teams.id)
         FROM teams WHERE id = ?1",
    )
    .bind(id)
    .fetch_optional(state.db.reader())
    .await?;
    Ok(row.map(|(id, name, members)| GroupRef { id, name, members }))
}

async fn share(state: &AppState, row: GrantRow) -> Result<Share, HttpError> {
    Ok(Share {
        user: match row.user_id {
            Some(id) => person(state, id).await?,
            None => None,
        },
        group: match row.team_id {
            Some(id) => group_ref(state, id).await?,
            None => None,
        },
        permissions: row.role.perms(),
        role: row.role,
        created_by: match row.created_by {
            Some(id) => person(state, id).await?,
            None => None,
        },
        // Only a manager reads them (`Need::Share`), and manages them all.
        editable: true,
        created_at: row.created_at,
        id: row.id,
    })
}

#[utoipa::path(
    get,
    path = "/{id}/shares",
    tag = "Sharing",
    operation_id = "listShares",
    summary = "Who a file or folder is shared with",
    description = "Deprecated, answered for one release: `GET /files/{id}/access`. The grants on this file or folder itself (not those of a folder above it), as shares. For its managers.",
    params(("id" = Uuid, Path, description = "The file or folder id")),
    responses(
        (status = 200, description = "Its shares, the oldest first", body = Vec<Share>),
        (status = 401, description = "Not signed in", body = ErrorBody),
        (status = 403, description = "Not the owner, and no re-share", body = ErrorBody),
        (status = 404, description = "Not found", body = ErrorBody),
    ),
    security(("session" = []), ("api_key" = [])),
)]
async fn list_shares(
    State(state): State<AppState>,
    user: AuthUser,
    Path(id): Path<Uuid>,
) -> Result<Json<Vec<Share>>, HttpError> {
    let node = access(&state, &user, id, Need::Share).await?;
    let rows = grants_of(state.db.reader(), node.owner, Some(id)).await?;
    let mut out = Vec::with_capacity(rows.len());
    for row in rows.into_iter().filter(|g| g.role != Role::Owner) {
        out.push(share(&state, row).await?);
    }
    Ok(Json(out))
}

/// A new share: with a user or a group, and what it gives besides reading.
#[derive(Deserialize, ToSchema)]
pub struct NewShare {
    /// The user to share with; or `group_id`.
    pub user_id: Option<Uuid>,
    /// The group to share with; or `user_id`.
    pub group_id: Option<Uuid>,
    #[serde(default)]
    pub permissions: Perms,
}

#[utoipa::path(
    post,
    path = "/{id}/shares",
    tag = "Sharing",
    operation_id = "addShare",
    summary = "Share a file or folder",
    description = "Deprecated, answered for one release: `POST /files/{id}/access`. With a user or a team, as the role the permissions stand for (read only: viewer; create, change or delete: editor; re-share: manager). For a manager, who gives at most what they have.",
    params(("id" = Uuid, Path, description = "The file or folder id")),
    request_body(content = NewShare, content_type = "application/json"),
    responses(
        (status = 201, description = "Shared", body = Share),
        (status = 400, description = "Neither a user nor a group, or both; the owner or yourself", body = ErrorBody),
        (status = 401, description = "Not signed in", body = ErrorBody),
        (status = 403, description = "No re-share, or more than you have", body = ErrorBody),
        (status = 404, description = "Not found (the file, the user or the group)", body = ErrorBody),
        (status = 409, description = "Already shared with them", body = ErrorBody),
        (status = 415, description = "The body must be JSON (Content-Type: application/json)", body = ErrorBody),
    ),
    security(("session" = []), ("api_key" = [])),
)]
async fn add_share(
    State(state): State<AppState>,
    user: AuthUser,
    Path(id): Path<Uuid>,
    ApiJson(body): ApiJson<NewShare>,
) -> Result<Response, HttpError> {
    let node = access(&state, &user, id, Need::Share).await?;
    let role = Role::of_perms(body.permissions);
    check_gives(node.role, role)?;
    let to = match (body.user_id, body.group_id) {
        (Some(to), None) => {
            if to == user.0.id
                || crate::files::roles::drive_role(state.db.reader(), to, node.owner)
                    .await?
                    .is_some()
            {
                return Err(HttpError::bad_request(
                    "It is theirs already: share with someone else",
                ));
            }
            Grantee::User(to)
        }
        (None, Some(group)) => Grantee::Team(group),
        _ => {
            return Err(HttpError::bad_request(
                "Share with a user (user_id) or a group (group_id)",
            ));
        }
    };
    let row = state
        .files
        .grant(node.owner, Some(id), to, role, user.0.id)
        .await?;
    tell_shared(&state, user.0.id, to).await?;
    Ok((StatusCode::CREATED, Json(share(&state, row).await?)).into_response())
}

/// The open apps of whoever a grant reaches fetch "Shared with me" again
/// (the sidebar's badge), and each gets a push notice as their settings
/// allow (not the one who gave it, when they are in the team).
pub(crate) async fn tell_shared(state: &AppState, by: Uuid, to: Grantee) -> Result<(), HttpError> {
    let told: Vec<Uuid> = match to {
        Grantee::User(user) => vec![user],
        Grantee::Team(team) => {
            sqlx::query_scalar("SELECT user_id FROM team_members WHERE team_id = ?1")
                .bind(team)
                .fetch_all(state.db.reader())
                .await?
        }
    };
    for someone in told {
        state.files.announce(someone, crate::files::Topic::Changes);
        if someone != by {
            crate::push::notify(state, someone, crate::push::Kind::Shared);
        }
    }
    Ok(())
}

/// A grant gives no more than its giver has, and never more than manager.
pub(crate) fn check_gives(have: Role, gives: Role) -> Result<(), HttpError> {
    if gives <= have.min(Role::Manager) {
        Ok(())
    } else {
        Err(HttpError::new(
            StatusCode::FORBIDDEN,
            "You cannot give more than you have",
        ))
    }
}

/// A grant on a node, which the user manages.
async fn editable(
    state: &AppState,
    user: &AuthUser,
    node: Uuid,
    id: Uuid,
) -> Result<(GrantRow, Role), HttpError> {
    let access = access(state, user, node, Need::Share).await?;
    let row = grant_row(state.db.reader(), id)
        .await?
        .filter(|g| g.node_id == Some(node))
        .ok_or_else(HttpError::not_found)?;
    Ok((row, access.role))
}

/// What a share gives besides reading.
#[derive(Deserialize, ToSchema)]
pub struct ShareChange {
    pub permissions: Perms,
}

#[utoipa::path(
    patch,
    path = "/{id}/shares/{share_id}",
    tag = "Sharing",
    operation_id = "changeShare",
    summary = "Change what a share gives",
    description = "Deprecated, answered for one release: `PATCH /files/{id}/access/{grant_id}`.",
    params(
        ("id" = Uuid, Path, description = "The file or folder id"),
        ("share_id" = Uuid, Path, description = "The share id"),
    ),
    request_body(content = ShareChange, content_type = "application/json"),
    responses(
        (status = 200, description = "Changed", body = Share),
        (status = 401, description = "Not signed in", body = ErrorBody),
        (status = 403, description = "Not the owner or who shared it, or more than they have", body = ErrorBody),
        (status = 404, description = "Not found", body = ErrorBody),
        (status = 415, description = "The body must be JSON (Content-Type: application/json)", body = ErrorBody),
    ),
    security(("session" = []), ("api_key" = [])),
)]
async fn change_share(
    State(state): State<AppState>,
    user: AuthUser,
    Path((node, id)): Path<(Uuid, Uuid)>,
    ApiJson(body): ApiJson<ShareChange>,
) -> Result<Json<Share>, HttpError> {
    let (_, have) = editable(&state, &user, node, id).await?;
    let role = Role::of_perms(body.permissions);
    check_gives(have, role)?;
    let row = state.files.change_grant(id, role).await?;
    Ok(Json(share(&state, row).await?))
}

#[utoipa::path(
    delete,
    path = "/{id}/shares/{share_id}",
    tag = "Sharing",
    operation_id = "removeShare",
    summary = "Stop sharing",
    description = "Deprecated, answered for one release: `DELETE /files/{id}/access/{grant_id}`. Where its recipients placed it, it goes from their files; the shares they made from it give nothing more.",
    params(
        ("id" = Uuid, Path, description = "The file or folder id"),
        ("share_id" = Uuid, Path, description = "The share id"),
    ),
    responses(
        (status = 204, description = "Removed"),
        (status = 401, description = "Not signed in", body = ErrorBody),
        (status = 403, description = "Not the owner or who shared it", body = ErrorBody),
        (status = 404, description = "Not found", body = ErrorBody),
    ),
    security(("session" = []), ("api_key" = [])),
)]
async fn remove_share(
    State(state): State<AppState>,
    user: AuthUser,
    Path((node, id)): Path<(Uuid, Uuid)>,
) -> Result<StatusCode, HttpError> {
    editable(&state, &user, node, id).await?;
    state.files.revoke(id).await?;
    Ok(StatusCode::NO_CONTENT)
}

/// A share with the user: its node, owner, maker, group's name and when.
type WithMe = (Uuid, Uuid, Option<Uuid>, Option<String>, String);

/// Something shared with the user.
#[derive(Serialize, ToSchema)]
pub struct SharedItem {
    /// The file or folder, as its owner named it (with `my_role`).
    pub node: Node,
    /// Whose it is.
    pub owner: Person,
    /// Who shared it with the user (the owner, or someone who re-shared
    /// it); several shares of it: the newest's.
    pub shared_by: Person,
    /// Through a team: its name; null when shared with the user. Always
    /// present.
    #[schema(required = true)]
    pub group: Option<String>,
    /// What the user may do with it besides reading (what `role` allows).
    pub permissions: Perms,
    /// Their role on it.
    pub role: Role,
    pub shared_at: String,
    /// Shared since the user last looked at "Shared with me".
    pub new: bool,
    /// Where the user placed it in their files: its path (e.g.
    /// `/Projects/Plans`); null while it is only here. Always present.
    #[schema(required = true)]
    pub placed: Option<String>,
    /// Shared by someone on another server (docs/M7E_DESIGN.md §5): from
    /// where and whom ("from alice@cloud.example"), and whether the user
    /// accepted it (`pending` until `POST /shared/{id}/accept`, or placing
    /// it; `DELETE /shared/{id}` declines it). `owner` and `shared_by` then
    /// carry the nil id and their addresses. Null for a share here.
    /// Always present.
    #[schema(required = true)]
    pub remote: Option<crate::routes::drives::RemoteDrive>,
}

#[utoipa::path(
    get,
    path = "",
    tag = "Sharing",
    operation_id = "listSharedWithMe",
    summary = "What is shared with me",
    description = "Each file or folder a grant gives the user or a team of theirs, in a drive that is not theirs, not declined, the newest first; where they placed it, if they did.",
    responses(
        (status = 200, description = "What is shared with the user", body = Vec<SharedItem>),
        (status = 401, description = "Not signed in", body = ErrorBody),
    ),
    security(("session" = []), ("api_key" = [])),
)]
async fn shared_with_me(
    State(state): State<AppState>,
    user: AuthUser,
) -> Result<Json<Vec<SharedItem>>, HttpError> {
    let me = user.0.id;
    // The newest share of each node first.
    let rows: Vec<WithMe> = sqlx::query_as(
        "SELECT s.node_id, s.drive_id, s.created_by, g.name, s.created_at
         FROM grants s LEFT JOIN teams g ON g.id = s.team_id
         WHERE s.node_id IS NOT NULL
           AND (s.user_id = ?1
                OR s.team_id IN (SELECT team_id FROM team_members WHERE user_id = ?1))
           AND s.drive_id NOT IN (SELECT drive_id FROM drive_users WHERE user_id = ?1)
           AND NOT EXISTS (SELECT 1 FROM grant_declines d
                           WHERE d.grant_id = s.id AND d.user_id = ?1)
         ORDER BY s.created_at DESC, s.id DESC",
    )
    .bind(me)
    .fetch_all(state.db.reader())
    .await?;
    let seen: Option<String> = sqlx::query_scalar("SELECT shared_seen_at FROM users WHERE id = ?1")
        .bind(me)
        .fetch_one(state.db.reader())
        .await?;
    let mut done = Vec::new();
    let mut out = Vec::new();
    for (node_id, _drive, created_by, group, created_at) in rows {
        if done.contains(&node_id) {
            continue;
        }
        done.push(node_id);
        // Through a re-share whose maker lost theirs, or hidden: nothing.
        let Ok(access) = state.files.access(me, node_id).await else {
            continue;
        };
        let mut node = access.node.clone();
        // From another server: its top stands for what was shared.
        if let Some(received) = crate::routes::federation::received_for(&state, me, node_id).await?
        {
            node.name = received.name.clone();
            let placed = match state.files.mount_of(me, node_id).await? {
                Some(_) => state
                    .files
                    .view_path(me, node_id)
                    .await?
                    .map(|p| format!("/{}", p.join("/"))),
                None => None,
            };
            out.push(SharedItem {
                new: seen
                    .as_deref()
                    .is_none_or(|seen| created_at.as_str() > seen),
                node,
                owner: Person {
                    id: Uuid::nil(),
                    email: received.owner.clone(),
                },
                shared_by: Person {
                    id: Uuid::nil(),
                    email: received.sender.clone(),
                },
                group,
                permissions: access.perms(),
                role: access.role,
                shared_at: created_at,
                placed,
                remote: crate::routes::drives::remote_drive(&state, received.drive_id).await?,
            });
            continue;
        }
        // Its owner; for a shared drive, who shared it.
        let owner_id = state.files.owner_user(node_id).await?.or(created_by);
        let owner = match owner_id {
            Some(id) => person(&state, id).await?,
            None => None,
        }
        .ok_or_else(HttpError::not_found)?;
        let shared_by = match created_by {
            Some(id) => person(&state, id).await?,
            None => None,
        }
        .unwrap_or(Person {
            id: owner.id,
            email: owner.email.clone(),
        });
        let placed = match state.files.mount_of(me, node_id).await? {
            Some(_) => state
                .files
                .view_path(me, node_id)
                .await?
                .map(|p| format!("/{}", p.join("/"))),
            None => None,
        };
        out.push(SharedItem {
            new: seen
                .as_deref()
                .is_none_or(|seen| created_at.as_str() > seen),
            node,
            owner,
            shared_by,
            group,
            permissions: access.perms(),
            role: access.role,
            shared_at: created_at,
            placed,
            remote: None,
        });
    }
    Ok(Json(out))
}

#[utoipa::path(
    post,
    path = "/seen",
    tag = "Sharing",
    operation_id = "markSharedSeen",
    summary = "I have seen what is shared with me",
    description = "What was shared until now is no longer new (the sidebar's badge).",
    responses(
        (status = 204, description = "Seen"),
        (status = 401, description = "Not signed in", body = ErrorBody),
    ),
    security(("session" = []), ("api_key" = [])),
)]
async fn mark_seen(State(state): State<AppState>, user: AuthUser) -> Result<StatusCode, HttpError> {
    sqlx::query("UPDATE users SET shared_seen_at = ?1 WHERE id = ?2")
        .bind(crate::auth::now_iso())
        .bind(user.0.id)
        .execute(state.db.writer())
        .await?;
    Ok(StatusCode::NO_CONTENT)
}

/// Who can be shared with.
#[derive(Serialize, ToSchema)]
pub struct Recipients {
    /// Every other user who is not disabled.
    pub users: Vec<Person>,
    /// The teams (M6b's groups).
    pub groups: Vec<GroupRef>,
}

#[utoipa::path(
    get,
    path = "/recipients",
    tag = "Sharing",
    operation_id = "listRecipients",
    summary = "Who can be shared with",
    description = "The other users (not disabled) and the teams, by email and by name.",
    responses(
        (status = 200, description = "The users and the groups", body = Recipients),
        (status = 401, description = "Not signed in", body = ErrorBody),
    ),
    security(("session" = []), ("api_key" = [])),
)]
async fn recipients(
    State(state): State<AppState>,
    user: AuthUser,
) -> Result<Json<Recipients>, HttpError> {
    let users: Vec<Person> = sqlx::query_as(
        "SELECT id, email FROM users WHERE id != ?1 AND disabled_at IS NULL ORDER BY email",
    )
    .bind(user.0.id)
    .fetch_all(state.db.reader())
    .await?;
    let groups: Vec<(Uuid, String, i64)> = sqlx::query_as(
        "SELECT id, name, (SELECT COUNT(*) FROM team_members m WHERE m.team_id = teams.id)
         FROM teams ORDER BY name",
    )
    .fetch_all(state.db.reader())
    .await?;
    Ok(Json(Recipients {
        users,
        groups: groups
            .into_iter()
            .map(|(id, name, members)| GroupRef { id, name, members })
            .collect(),
    }))
}

/// Where to place a share in the user's files.
#[derive(Deserialize, ToSchema)]
pub struct PlaceShare {
    /// A folder of the user's own files (not inside another share).
    pub parent_id: Uuid,
    /// Its name there; its own name by default.
    pub name: Option<String>,
}

#[utoipa::path(
    post,
    path = "/{id}/place",
    tag = "Sharing",
    operation_id = "placeShared",
    summary = "Add something shared with me to my files",
    description = "As an entry in one of the user's folders that stands for the owner's: listed, synced and in WebDAV like their own, while storage, versions and trash stay the owner's.",
    params(("id" = Uuid, Path, description = "The shared file or folder id")),
    request_body(content = PlaceShare, content_type = "application/json"),
    responses(
        (status = 201, description = "Placed", body = Entry),
        (status = 400, description = "Your own, or an invalid name", body = ErrorBody),
        (status = 401, description = "Not signed in", body = ErrorBody),
        (status = 403, description = "A read-only API key", body = ErrorBody),
        (status = 404, description = "Not shared with you, or the folder is not yours", body = ErrorBody),
        (status = 409, description = "Placed already (it, or a folder above or in it), or the name is taken", body = ErrorBody),
        (status = 415, description = "The body must be JSON (Content-Type: application/json)", body = ErrorBody),
    ),
    security(("session" = []), ("api_key" = [])),
)]
async fn place(
    State(state): State<AppState>,
    user: AuthUser,
    Path(id): Path<Uuid>,
    ApiJson(body): ApiJson<PlaceShare>,
) -> Result<Response, HttpError> {
    // From another server: placing it accepts it, under its name.
    let mut name = body.name.clone();
    if let Some(received) = crate::routes::federation::received_for(&state, user.0.id, id).await? {
        crate::federation::incoming::accept(&state.files, &received).await?;
        name = name.or(Some(received.name));
    }
    state
        .files
        .place_share(user.0.id, id, body.parent_id, name.as_deref())
        .await?;
    let placed = access(&state, &user, id, Need::Read).await?;
    Ok((
        StatusCode::CREATED,
        Json(entry(&state, user.0.id, &placed).await?),
    )
        .into_response())
}

#[utoipa::path(
    delete,
    path = "/{id}/place",
    tag = "Sharing",
    operation_id = "unplaceShared",
    summary = "Remove something shared with me from my files",
    description = "It stays in \"Shared with me\"; nothing of the owner's is deleted.",
    params(("id" = Uuid, Path, description = "The shared file or folder id")),
    responses(
        (status = 204, description = "Removed from the user's files"),
        (status = 401, description = "Not signed in", body = ErrorBody),
        (status = 403, description = "A read-only API key", body = ErrorBody),
        (status = 404, description = "Not placed", body = ErrorBody),
    ),
    security(("session" = []), ("api_key" = [])),
)]
async fn unplace(
    State(state): State<AppState>,
    user: AuthUser,
    Path(id): Path<Uuid>,
) -> Result<StatusCode, HttpError> {
    state.files.unplace_share(user.0.id, id).await?;
    Ok(StatusCode::NO_CONTENT)
}

#[utoipa::path(
    delete,
    path = "/{id}",
    tag = "Sharing",
    operation_id = "declineShared",
    summary = "Decline something shared with me",
    description = "Gone for the user (for a team's grant, for them alone), and from their files if they placed it.",
    params(("id" = Uuid, Path, description = "The shared file or folder id")),
    responses(
        (status = 204, description = "Declined"),
        (status = 401, description = "Not signed in", body = ErrorBody),
        (status = 403, description = "A read-only API key", body = ErrorBody),
        (status = 404, description = "Not shared with you", body = ErrorBody),
    ),
    security(("session" = []), ("api_key" = [])),
)]
async fn decline(
    State(state): State<AppState>,
    user: AuthUser,
    Path(id): Path<Uuid>,
) -> Result<StatusCode, HttpError> {
    let me = user.0.id;
    // From another server: its server is told, and it goes from here.
    if let Some(received) = crate::routes::federation::received_for(&state, me, id).await? {
        crate::federation::incoming::decline(&state.files, &received).await?;
        return Ok(StatusCode::NO_CONTENT);
    }
    let declined = sqlx::query(
        "INSERT OR IGNORE INTO grant_declines (grant_id, user_id)
         SELECT s.id, ?2 FROM grants s
         WHERE s.node_id = ?1 AND s.role != 'owner'
           AND (s.user_id = ?2
                OR s.team_id IN (SELECT team_id FROM team_members WHERE user_id = ?2))",
    )
    .bind(id)
    .bind(me)
    .execute(state.db.writer())
    .await?
    .rows_affected();
    if declined == 0 {
        return Err(HttpError::not_found());
    }
    state.files.settle_mounts().await?;
    Ok(StatusCode::NO_CONTENT)
}
