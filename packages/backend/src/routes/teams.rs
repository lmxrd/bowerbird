//! Teams (docs/M7B_DESIGN.md §3): a name, members, its own managers, a
//! storage limit for all its drives together, and setting defaults. The
//! superadmin makes and removes teams and names their first managers
//! (`/superadmin/teams`); a team's managers add and remove members and
//! managers, make the team's shared drives and set the team's settings
//! within what the superadmin left open (`/teams/{id}/…`). The rules of a
//! member's drives are in `files/grants.rs`.

use crate::activity::{self, Actor};
use crate::error::{ErrorBody, HttpError};
use crate::extract::ApiJson;
use crate::files::grants::{Grantee, TeamRole};
use crate::files::roles::Role;
use crate::routes::auth::{AuthUser, Superadmin};
use crate::settings::{self, For, Level, LevelSetting, LevelValue};
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

/// Nested under /teams.
pub fn router() -> OpenApiRouter<AppState> {
    OpenApiRouter::new()
        .routes(routes!(list_my_teams))
        .routes(routes!(get_team))
        .routes(routes!(add_team_member))
        .routes(routes!(update_team_member, remove_team_member))
        .routes(routes!(create_team_drive))
        .routes(routes!(list_team_settings))
        .routes(routes!(set_team_setting, unset_team_setting))
}

/// Nested under /superadmin.
pub fn superadmin_router() -> OpenApiRouter<AppState> {
    OpenApiRouter::new()
        .routes(routes!(list_teams, create_team))
        .routes(routes!(update_team, delete_team))
        .routes(routes!(
            set_team_member_as_superadmin,
            remove_team_member_as_superadmin
        ))
}

/// A member of a team.
#[derive(Serialize, ToSchema, sqlx::FromRow)]
pub struct TeamMember {
    pub user_id: Uuid,
    pub email: String,
    pub role: TeamRole,
    pub joined_at: String,
}

/// A drive of a team.
#[derive(Serialize, ToSchema, sqlx::FromRow)]
pub struct TeamDrive {
    /// A string, as every drive's id.
    pub id: String,
    pub name: String,
}

/// A team.
#[derive(Serialize, ToSchema)]
pub struct Team {
    pub id: Uuid,
    pub name: String,
    /// Bytes, for all its drives together; null: no limit. Always present.
    #[schema(required = true)]
    pub storage_limit: Option<i64>,
    /// Bytes its drives take.
    pub used: i64,
    /// The signed-in user's role in it; null when they are not a member
    /// (the superadmin's view). Always present.
    #[schema(required = true)]
    pub my_role: Option<TeamRole>,
    /// It is the signed-in user's primary team: their defaults come from
    /// it (§3).
    pub primary: bool,
    /// Managers first, then by email.
    pub members: Vec<TeamMember>,
    /// Its shared drives, by name.
    pub drives: Vec<TeamDrive>,
    pub created_at: String,
}

/// A team as `me` sees it; not found when there is no such team.
pub(crate) async fn team_view(state: &AppState, id: Uuid, me: Uuid) -> Result<Team, HttpError> {
    let (name, storage_limit, created_at): (String, Option<i64>, String) =
        sqlx::query_as("SELECT name, storage_limit, created_at FROM teams WHERE id = ?1")
            .bind(id)
            .fetch_optional(state.db.reader())
            .await?
            .ok_or_else(HttpError::not_found)?;
    let members: Vec<TeamMember> = sqlx::query_as(
        "SELECT u.id AS user_id, u.email, m.role, m.joined_at FROM team_members m
         JOIN users u ON u.id = m.user_id WHERE m.team_id = ?1
         ORDER BY m.role = 'manager' DESC, u.email",
    )
    .bind(id)
    .fetch_all(state.db.reader())
    .await?;
    let drives: Vec<(Uuid, String)> = sqlx::query_as(
        "SELECT id, name FROM drives WHERE team_id = ?1 ORDER BY name COLLATE NOCASE",
    )
    .bind(id)
    .fetch_all(state.db.reader())
    .await?;
    let used = state.files.team_usage(id).await?.used;
    let primary = primary_team(state, me).await? == Some(id);
    Ok(Team {
        id,
        name,
        storage_limit,
        used,
        my_role: members.iter().find(|m| m.user_id == me).map(|m| m.role),
        primary,
        members,
        drives: drives
            .into_iter()
            .map(|(id, name)| TeamDrive {
                id: id.to_string(),
                name,
            })
            .collect(),
        created_at,
    })
}

/// A user's primary team: the one they chose, else the first they joined.
pub async fn primary_team(state: &AppState, user: Uuid) -> Result<Option<Uuid>, HttpError> {
    Ok(sqlx::query_scalar(
        "SELECT m.team_id FROM team_members m WHERE m.user_id = ?1
         ORDER BY m.team_id = (SELECT primary_team_id FROM users WHERE id = ?1) DESC, m.joined_at
         LIMIT 1",
    )
    .bind(user)
    .fetch_optional(state.db.reader())
    .await?)
}

/// The user's role in a team; not found for a team they are not in.
async fn role_in(state: &AppState, team: Uuid, user: Uuid) -> Result<TeamRole, HttpError> {
    sqlx::query_scalar("SELECT role FROM team_members WHERE team_id = ?1 AND user_id = ?2")
        .bind(team)
        .bind(user)
        .fetch_optional(state.db.reader())
        .await?
        .ok_or_else(HttpError::not_found)
}

/// Refused (403) unless the user manages the team; not found when they
/// are not in it.
async fn managed(state: &AppState, team: Uuid, user: Uuid) -> Result<(), HttpError> {
    if role_in(state, team, user).await? == TeamRole::Manager {
        return Ok(());
    }
    Err(HttpError::new(
        StatusCode::FORBIDDEN,
        "Only the team's managers can do that",
    ))
}

/// Add, change or take out a team's member, with the account activity
/// when the superadmin or a team manager did it (one's own leaving or
/// change of role is not noted: they did it themselves).
pub(crate) async fn set_member(
    state: &AppState,
    team: Uuid,
    user: Uuid,
    role: Option<TeamRole>,
    actor: Option<Actor>,
) -> Result<(), HttpError> {
    let was: Option<TeamRole> =
        sqlx::query_scalar("SELECT role FROM team_members WHERE team_id = ?1 AND user_id = ?2")
            .bind(team)
            .bind(user)
            .fetch_optional(state.db.reader())
            .await?;
    if was == role {
        return Ok(());
    }
    state.files.set_team_member(team, user, role).await?;
    let actor = actor.filter(|a| *a != Actor::TeamManager(user));
    if let Some(actor) = actor {
        let name: String = sqlx::query_scalar("SELECT name FROM teams WHERE id = ?1")
            .bind(team)
            .fetch_one(state.db.reader())
            .await?;
        let action = match (was, role) {
            (None, Some(_)) => activity::Action::TeamAdded,
            (Some(_), None) => activity::Action::TeamRemoved,
            _ => activity::Action::TeamRole,
        };
        activity::log(
            state.db.writer(),
            user,
            actor,
            action,
            activity::Detail {
                team: Some(name),
                role: role.map(|r| team_role_name(r).into()),
                ..Default::default()
            },
        )
        .await?;
    }
    Ok(())
}

fn team_role_name(role: TeamRole) -> &'static str {
    match role {
        TeamRole::Manager => "manager",
        TeamRole::Member => "member",
    }
}

/// Remove a team: its members leave it first (each with what follows for
/// their drives); refused (409) when one of its drives would keep no
/// manager.
pub(crate) async fn delete(state: &AppState, team: Uuid, by: Uuid) -> Result<(), HttpError> {
    let exists: Option<i64> = sqlx::query_scalar("SELECT 1 FROM teams WHERE id = ?1")
        .bind(team)
        .fetch_optional(state.db.reader())
        .await?;
    exists.ok_or_else(HttpError::not_found)?;
    // A drive whose managers all come from this team.
    let orphaned: Option<String> = sqlx::query_scalar(
        "SELECT d.name FROM drives d
         WHERE d.kind = 'shared'
           AND (d.team_id = ?1 OR d.id IN (SELECT drive_id FROM grants
                                           WHERE team_id = ?1 AND node_id IS NULL))
           AND NOT EXISTS (
             SELECT 1 FROM grants g WHERE g.drive_id = d.id AND g.node_id IS NULL
               AND g.role = 'manager'
               AND (g.user_id IS NOT NULL
                    OR (g.team_id != ?1
                        AND EXISTS (SELECT 1 FROM team_members m WHERE m.team_id = g.team_id))))
         LIMIT 1",
    )
    .bind(team)
    .fetch_optional(state.db.reader())
    .await?;
    if let Some(name) = orphaned {
        return Err(HttpError::conflict(format!(
            "The drive {name} would keep no manager: give it another first"
        )));
    }
    let members: Vec<Uuid> =
        sqlx::query_scalar("SELECT user_id FROM team_members WHERE team_id = ?1")
            .bind(team)
            .fetch_all(state.db.reader())
            .await?;
    for member in members {
        set_member(state, team, member, None, Some(Actor::Superadmin(by))).await?;
    }
    sqlx::query("DELETE FROM teams WHERE id = ?1")
        .bind(team)
        .execute(state.db.writer())
        .await?;
    state.files.settle_mounts().await?;
    Ok(())
}

/// A user by email (any case), not disabled.
async fn user_by_email(state: &AppState, email: &str) -> Result<Uuid, HttpError> {
    sqlx::query_scalar(
        "SELECT id FROM users WHERE email = ?1 COLLATE NOCASE AND disabled_at IS NULL",
    )
    .bind(email.trim())
    .fetch_optional(state.db.reader())
    .await?
    .ok_or_else(|| HttpError::bad_request("No user has that email"))
}

fn clean_name(name: &str) -> Result<String, HttpError> {
    let name = name.trim();
    if name.is_empty() || name.chars().count() > 100 {
        return Err(HttpError::bad_request(
            "A team's name is 1 to 100 characters",
        ));
    }
    Ok(name.to_string())
}

#[utoipa::path(
    get,
    path = "",
    tag = "Teams",
    operation_id = "listMyTeams",
    summary = "My teams",
    description = "The teams the user is in, by name, each with its members, drives and limit.",
    responses(
        (status = 200, description = "The user's teams", body = Vec<Team>),
        (status = 401, description = "Not signed in", body = ErrorBody),
    ),
    security(("session" = []), ("api_key" = [])),
)]
async fn list_my_teams(
    State(state): State<AppState>,
    user: AuthUser,
) -> Result<Json<Vec<Team>>, HttpError> {
    let ids: Vec<Uuid> = sqlx::query_scalar(
        "SELECT t.id FROM teams t JOIN team_members m ON m.team_id = t.id
         WHERE m.user_id = ?1 ORDER BY t.name COLLATE NOCASE",
    )
    .bind(user.0.id)
    .fetch_all(state.db.reader())
    .await?;
    let mut out = Vec::with_capacity(ids.len());
    for id in ids {
        out.push(team_view(&state, id, user.0.id).await?);
    }
    Ok(Json(out))
}

#[utoipa::path(
    get,
    path = "/{id}",
    tag = "Teams",
    operation_id = "getTeam",
    summary = "A team of mine",
    params(("id" = Uuid, Path, description = "The team")),
    responses(
        (status = 200, description = "The team", body = Team),
        (status = 401, description = "Not signed in", body = ErrorBody),
        (status = 404, description = "Not a team of the user's", body = ErrorBody),
    ),
    security(("session" = []), ("api_key" = [])),
)]
async fn get_team(
    State(state): State<AppState>,
    user: AuthUser,
    Path(id): Path<Uuid>,
) -> Result<Json<Team>, HttpError> {
    role_in(&state, id, user.0.id).await?;
    Ok(Json(team_view(&state, id, user.0.id).await?))
}

/// Someone to add to a team.
#[derive(Deserialize, ToSchema)]
pub struct NewTeamMember {
    /// The user's email.
    pub email: String,
    /// `member` by default.
    #[serde(default = "member")]
    pub role: TeamRole,
}

fn member() -> TeamRole {
    TeamRole::Member
}

/// A member's role in a team.
#[derive(Deserialize, ToSchema)]
pub struct TeamMemberRole {
    pub role: TeamRole,
}

#[utoipa::path(
    post,
    path = "/{id}/members",
    tag = "Teams",
    operation_id = "addTeamMember",
    summary = "Add a member to a team",
    description = "By its managers. They get the team's drives (and what is shared with the team) at once. Adding someone who is a member already sets their role.",
    params(("id" = Uuid, Path, description = "The team")),
    request_body(content = NewTeamMember, content_type = "application/json"),
    responses(
        (status = 200, description = "The team now", body = Team),
        (status = 400, description = "No such user", body = ErrorBody),
        (status = 401, description = "Not signed in", body = ErrorBody),
        (status = 403, description = "Not a manager of the team, or a read-only key", body = ErrorBody),
        (status = 404, description = "Not a team of the user's", body = ErrorBody),
        (status = 415, description = "The body must be JSON (Content-Type: application/json)", body = ErrorBody),
    ),
    security(("session" = []), ("api_key" = [])),
)]
async fn add_team_member(
    State(state): State<AppState>,
    user: AuthUser,
    Path(id): Path<Uuid>,
    ApiJson(body): ApiJson<NewTeamMember>,
) -> Result<Json<Team>, HttpError> {
    managed(&state, id, user.0.id).await?;
    let member = user_by_email(&state, &body.email).await?;
    set_member(
        &state,
        id,
        member,
        Some(body.role),
        Some(Actor::TeamManager(user.0.id)),
    )
    .await?;
    Ok(Json(team_view(&state, id, user.0.id).await?))
}

#[utoipa::path(
    patch,
    path = "/{id}/members/{user_id}",
    tag = "Teams",
    operation_id = "updateTeamMember",
    summary = "Make a member a manager of a team, or a member again",
    params(
        ("id" = Uuid, Path, description = "The team"),
        ("user_id" = Uuid, Path, description = "The member"),
    ),
    request_body(content = TeamMemberRole, content_type = "application/json"),
    responses(
        (status = 200, description = "The team now", body = Team),
        (status = 401, description = "Not signed in", body = ErrorBody),
        (status = 403, description = "Not a manager of the team, or a read-only key", body = ErrorBody),
        (status = 404, description = "Not a team of the user's, or not a member", body = ErrorBody),
        (status = 409, description = "A drive of the team would keep no manager", body = ErrorBody),
        (status = 415, description = "The body must be JSON (Content-Type: application/json)", body = ErrorBody),
    ),
    security(("session" = []), ("api_key" = [])),
)]
async fn update_team_member(
    State(state): State<AppState>,
    user: AuthUser,
    Path((id, member)): Path<(Uuid, Uuid)>,
    ApiJson(body): ApiJson<TeamMemberRole>,
) -> Result<Json<Team>, HttpError> {
    managed(&state, id, user.0.id).await?;
    role_in(&state, id, member).await?;
    set_member(
        &state,
        id,
        member,
        Some(body.role),
        Some(Actor::TeamManager(user.0.id)),
    )
    .await?;
    Ok(Json(team_view(&state, id, user.0.id).await?))
}

#[utoipa::path(
    delete,
    path = "/{id}/members/{user_id}",
    tag = "Teams",
    operation_id = "removeTeamMember",
    summary = "Take a member out of a team",
    description = "By its managers, or by the member themselves (leaving). They lose the team's drives and what was shared with the team; what they placed of it leaves their files.",
    params(
        ("id" = Uuid, Path, description = "The team"),
        ("user_id" = Uuid, Path, description = "The member"),
    ),
    responses(
        (status = 204, description = "Taken out"),
        (status = 401, description = "Not signed in", body = ErrorBody),
        (status = 403, description = "Not a manager of the team, or a read-only key", body = ErrorBody),
        (status = 404, description = "Not a team of the user's, or not a member", body = ErrorBody),
        (status = 409, description = "A drive of the team would keep no manager", body = ErrorBody),
    ),
    security(("session" = []), ("api_key" = [])),
)]
async fn remove_team_member(
    State(state): State<AppState>,
    user: AuthUser,
    Path((id, member)): Path<(Uuid, Uuid)>,
) -> Result<StatusCode, HttpError> {
    let actor = if member == user.0.id {
        role_in(&state, id, member).await?;
        // Leaving: their own doing.
        None
    } else {
        managed(&state, id, user.0.id).await?;
        role_in(&state, id, member).await?;
        Some(Actor::TeamManager(user.0.id))
    };
    set_member(&state, id, member, None, actor).await?;
    Ok(StatusCode::NO_CONTENT)
}

/// A new shared drive of a team.
#[derive(Deserialize, ToSchema)]
pub struct NewTeamDrive {
    /// 1 to 100 characters, unique among the shared drives (any case).
    pub name: String,
    /// The role the team's members get on it; `editor` by default. Its
    /// managers are the team's.
    #[serde(default = "editor")]
    pub role: Role,
    /// Where it is stored (`GET /backends/choices?team_id=`): left out, or
    /// the server's disk's id, the server's disk; else a global backend
    /// the team's managers may use.
    pub backend_id: Option<Uuid>,
}

fn editor() -> Role {
    Role::Editor
}

#[utoipa::path(
    post,
    path = "/{id}/drives",
    tag = "Teams",
    operation_id = "createTeamDrive",
    summary = "Make a shared drive of a team",
    description = "By the team's managers, unless the superadmin's setting `drives.create` is `superadmin`. Its folder is in the data dir, or its prefix in the chosen backend (`backend_id`); its managers are the team's, its members get `role` (editor by default). Counted against the team's storage limit (and the backend's).",
    params(("id" = Uuid, Path, description = "The team")),
    request_body(content = NewTeamDrive, content_type = "application/json"),
    responses(
        (status = 201, description = "Made", body = crate::routes::drives::Drive),
        (status = 400, description = "An invalid name or role, or no such storage", body = ErrorBody),
        (status = 401, description = "Not signed in", body = ErrorBody),
        (status = 403, description = "Not a manager of the team, the setting does not allow it, a storage the team may not use, or a read-only key", body = ErrorBody),
        (status = 404, description = "Not a team of the user's", body = ErrorBody),
        (status = 409, description = "A shared drive has that name", body = ErrorBody),
        (status = 503, description = "The storage backend does not answer (`code` `storage_unavailable`)", body = ErrorBody),
        (status = 415, description = "The body must be JSON (Content-Type: application/json)", body = ErrorBody),
    ),
    security(("session" = []), ("api_key" = [])),
)]
async fn create_team_drive(
    State(state): State<AppState>,
    user: AuthUser,
    Path(id): Path<Uuid>,
    ApiJson(body): ApiJson<NewTeamDrive>,
) -> Result<Response, HttpError> {
    managed(&state, id, user.0.id).await?;
    let env = settings::Env::of_state(&state);
    let who = settings::resolve(&state.db, &env, "drives.create", For::default()).await?;
    if who.str() == Some("superadmin") {
        return Err(HttpError::new(
            StatusCode::FORBIDDEN,
            "Only the superadmin makes shared drives here",
        ));
    }
    if matches!(body.role, Role::Owner | Role::Manager) {
        return Err(HttpError::bad_request(
            "The team's members are editors, viewers or upload only: its managers manage it",
        ));
    }
    let backend = crate::routes::backends::chosen(body.backend_id);
    crate::routes::backends::check_maker(
        &state,
        backend,
        user.0.id,
        crate::routes::backends::Maker::TeamManager(id),
    )
    .await?;
    let drive =
        crate::routes::drives::make_shared(&state, &body.name, None, backend, None, user.0.id)
            .await?;
    sqlx::query("UPDATE drives SET team_id = ?1 WHERE id = ?2")
        .bind(id)
        .bind(drive)
        .execute(state.db.writer())
        .await?;
    state
        .files
        .grant(drive, None, Grantee::Team(id), body.role, user.0.id)
        .await?;
    Ok((
        StatusCode::CREATED,
        Json(crate::routes::drives::view_for(&state, drive, user.0.id).await?),
    )
        .into_response())
}

#[utoipa::path(
    get,
    path = "/{id}/settings",
    tag = "Teams",
    operation_id = "listTeamSettings",
    summary = "A team's settings",
    description = "Each setting with a team level: the team's mode (`locked`, `default`, `range`, or null: not set), value and range, and what the superadmin (or an environment variable) leaves it (`above`; with `above.locked_by`, it cannot be set here).",
    params(("id" = Uuid, Path, description = "The team")),
    responses(
        (status = 200, description = "The team's settings", body = Vec<LevelSetting>),
        (status = 401, description = "Not signed in", body = ErrorBody),
        (status = 404, description = "Not a team of the user's", body = ErrorBody),
    ),
    security(("session" = []), ("api_key" = [])),
)]
async fn list_team_settings(
    State(state): State<AppState>,
    user: AuthUser,
    Path(id): Path<Uuid>,
) -> Result<Json<Vec<LevelSetting>>, HttpError> {
    role_in(&state, id, user.0.id).await?;
    Ok(Json(
        settings::level_view(&state, Level::Team, Some(id), For::default()).await?,
    ))
}

#[utoipa::path(
    put,
    path = "/{id}/settings/{key}",
    tag = "Teams",
    operation_id = "setTeamSetting",
    summary = "Set a team's setting",
    description = "By the team's managers, within what the superadmin left open: locked for the team's members (and its drives), a default they may change, or a range. A setting the superadmin locked is refused (403); a value outside the superadmin's range too (400).",
    params(
        ("id" = Uuid, Path, description = "The team"),
        ("key" = String, Path, description = "The setting, e.g. `retention.trash_days`"),
    ),
    request_body(content = LevelValue, content_type = "application/json"),
    responses(
        (status = 200, description = "The setting now", body = LevelSetting),
        (status = 400, description = "An invalid value or range, or no team level", body = ErrorBody),
        (status = 401, description = "Not signed in", body = ErrorBody),
        (status = 403, description = "Not a manager of the team, locked above, or a read-only key", body = ErrorBody),
        (status = 404, description = "Not a team of the user's, or no such setting", body = ErrorBody),
        (status = 415, description = "The body must be JSON (Content-Type: application/json)", body = ErrorBody),
    ),
    security(("session" = []), ("api_key" = [])),
)]
async fn set_team_setting(
    State(state): State<AppState>,
    user: AuthUser,
    Path((id, key)): Path<(Uuid, String)>,
    ApiJson(body): ApiJson<LevelValue>,
) -> Result<Json<LevelSetting>, HttpError> {
    managed(&state, id, user.0.id).await?;
    settings::set(&state, Level::Team, Some(id), &key, &body, For::default()).await?;
    Ok(Json(one_level(&state, Level::Team, Some(id), &key).await?))
}

#[utoipa::path(
    delete,
    path = "/{id}/settings/{key}",
    tag = "Teams",
    operation_id = "unsetTeamSetting",
    summary = "Leave a team's setting not set",
    params(
        ("id" = Uuid, Path, description = "The team"),
        ("key" = String, Path, description = "The setting"),
    ),
    responses(
        (status = 200, description = "The setting now", body = LevelSetting),
        (status = 401, description = "Not signed in", body = ErrorBody),
        (status = 403, description = "Not a manager of the team, or a read-only key", body = ErrorBody),
        (status = 404, description = "Not a team of the user's, or no such setting", body = ErrorBody),
    ),
    security(("session" = []), ("api_key" = [])),
)]
async fn unset_team_setting(
    State(state): State<AppState>,
    user: AuthUser,
    Path((id, key)): Path<(Uuid, String)>,
) -> Result<Json<LevelSetting>, HttpError> {
    managed(&state, id, user.0.id).await?;
    settings::unset(&state.db, Level::Team, Some(id), &key).await?;
    Ok(Json(one_level(&state, Level::Team, Some(id), &key).await?))
}

/// One setting of a level, as its page shows it.
pub(crate) async fn one_level(
    state: &AppState,
    level: Level,
    level_id: Option<Uuid>,
    key: &str,
) -> Result<LevelSetting, HttpError> {
    let def = settings::def(key).ok_or_else(HttpError::not_found)?;
    let to = match level {
        Level::Drive => For {
            user: None,
            drive: level_id,
        },
        _ => For::default(),
    };
    let env = settings::Env::of_state(state);
    let above = settings::resolve_def(&state.db, &env, def, to, Some(level)).await?;
    let here = settings::row(&state.db, level, level_id, key).await?;
    Ok(settings::level_setting(def, here, above))
}

#[utoipa::path(
    get,
    path = "/teams",
    tag = "Superadmin",
    operation_id = "listTeams",
    summary = "The teams",
    description = "Every team, by name, with its members, drives and limit.",
    responses(
        (status = 200, description = "The teams", body = Vec<Team>),
        (status = 401, description = "Not signed in", body = ErrorBody),
        (status = 403, description = "Superadmin only, with a session (never a token) whose second factor was confirmed within 15 minutes: else `code` is `step_up_required` (confirm it, `/auth/step-up/…`) or `second_factor_required` (add one first)", body = ErrorBody),
    ),
    security(("session" = [])),
)]
async fn list_teams(
    State(state): State<AppState>,
    me: Superadmin,
) -> Result<Json<Vec<Team>>, HttpError> {
    let ids: Vec<Uuid> = sqlx::query_scalar("SELECT id FROM teams ORDER BY name COLLATE NOCASE")
        .fetch_all(state.db.reader())
        .await?;
    let mut out = Vec::with_capacity(ids.len());
    for id in ids {
        out.push(team_view(&state, id, me.0.id).await?);
    }
    Ok(Json(out))
}

/// A new team.
#[derive(Deserialize, ToSchema)]
pub struct NewTeam {
    /// 1 to 100 characters, unique (any case).
    pub name: String,
    /// Its first managers, by email.
    #[serde(default)]
    pub managers: Vec<String>,
    /// Its members, by email.
    #[serde(default)]
    pub members: Vec<String>,
    /// Bytes for all its drives together; left out or null: no limit.
    pub storage_limit: Option<i64>,
}

#[utoipa::path(
    post,
    path = "/teams",
    tag = "Superadmin",
    operation_id = "createTeam",
    summary = "Make a team",
    request_body(content = NewTeam, content_type = "application/json"),
    responses(
        (status = 201, description = "Made", body = Team),
        (status = 400, description = "An invalid name or limit, or no such user", body = ErrorBody),
        (status = 401, description = "Not signed in", body = ErrorBody),
        (status = 403, description = "Superadmin only, with a session (never a token) whose second factor was confirmed within 15 minutes: else `code` is `step_up_required` (confirm it, `/auth/step-up/…`) or `second_factor_required` (add one first)", body = ErrorBody),
        (status = 409, description = "A team has that name", body = ErrorBody),
        (status = 415, description = "The body must be JSON (Content-Type: application/json)", body = ErrorBody),
    ),
    security(("session" = [])),
)]
async fn create_team(
    State(state): State<AppState>,
    me: Superadmin,
    ApiJson(body): ApiJson<NewTeam>,
) -> Result<Response, HttpError> {
    let name = clean_name(&body.name)?;
    check_limit(body.storage_limit)?;
    let mut people = Vec::new();
    for email in &body.managers {
        people.push((user_by_email(&state, email).await?, TeamRole::Manager));
    }
    for email in &body.members {
        let id = user_by_email(&state, email).await?;
        if !people.iter().any(|(p, _)| *p == id) {
            people.push((id, TeamRole::Member));
        }
    }
    let id = Uuid::now_v7();
    let made = sqlx::query(
        "INSERT INTO teams (id, name, storage_limit, created_at) VALUES (?1, ?2, ?3, ?4)",
    )
    .bind(id)
    .bind(&name)
    .bind(body.storage_limit)
    .bind(crate::auth::now_iso())
    .execute(state.db.writer())
    .await;
    match made {
        Ok(_) => {}
        Err(sqlx::Error::Database(e)) if e.is_unique_violation() => {
            return Err(HttpError::conflict("A team has that name already"));
        }
        Err(e) => return Err(e.into()),
    }
    for (person, role) in people {
        set_member(
            &state,
            id,
            person,
            Some(role),
            Some(Actor::Superadmin(me.0.id)),
        )
        .await?;
    }
    Ok((
        StatusCode::CREATED,
        Json(team_view(&state, id, me.0.id).await?),
    )
        .into_response())
}

fn check_limit(limit: Option<i64>) -> Result<(), HttpError> {
    if limit.is_some_and(|l| l < 0) {
        return Err(HttpError::bad_request(
            "Expected a storage limit of 0 bytes or more",
        ));
    }
    Ok(())
}

/// What to change of a team; what is left out stays.
#[derive(Deserialize, ToSchema)]
pub struct UpdateTeam {
    pub name: Option<String>,
    /// Bytes; null for no limit.
    #[serde(default, deserialize_with = "crate::util::present")]
    #[schema(value_type = Option<i64>, nullable)]
    pub storage_limit: Option<Option<i64>>,
}

#[utoipa::path(
    patch,
    path = "/teams/{id}",
    tag = "Superadmin",
    operation_id = "updateTeam",
    summary = "Rename a team, or set its storage limit",
    params(("id" = Uuid, Path, description = "The team")),
    request_body(content = UpdateTeam, content_type = "application/json"),
    responses(
        (status = 200, description = "The team now", body = Team),
        (status = 400, description = "An invalid name or limit", body = ErrorBody),
        (status = 401, description = "Not signed in", body = ErrorBody),
        (status = 403, description = "Superadmin only, with a session (never a token) whose second factor was confirmed within 15 minutes: else `code` is `step_up_required` (confirm it, `/auth/step-up/…`) or `second_factor_required` (add one first)", body = ErrorBody),
        (status = 404, description = "No such team", body = ErrorBody),
        (status = 409, description = "A team has that name", body = ErrorBody),
        (status = 415, description = "The body must be JSON (Content-Type: application/json)", body = ErrorBody),
    ),
    security(("session" = [])),
)]
async fn update_team(
    State(state): State<AppState>,
    me: Superadmin,
    Path(id): Path<Uuid>,
    ApiJson(body): ApiJson<UpdateTeam>,
) -> Result<Json<Team>, HttpError> {
    team_view(&state, id, me.0.id).await?;
    if let Some(name) = &body.name {
        let name = clean_name(name)?;
        let renamed = sqlx::query("UPDATE teams SET name = ?1 WHERE id = ?2")
            .bind(&name)
            .bind(id)
            .execute(state.db.writer())
            .await;
        match renamed {
            Ok(_) => {}
            Err(sqlx::Error::Database(e)) if e.is_unique_violation() => {
                return Err(HttpError::conflict("A team has that name already"));
            }
            Err(e) => return Err(e.into()),
        }
    }
    if let Some(limit) = body.storage_limit {
        check_limit(limit)?;
        sqlx::query("UPDATE teams SET storage_limit = ?1 WHERE id = ?2")
            .bind(limit)
            .bind(id)
            .execute(state.db.writer())
            .await?;
    }
    Ok(Json(team_view(&state, id, me.0.id).await?))
}

#[utoipa::path(
    delete,
    path = "/teams/{id}",
    tag = "Superadmin",
    operation_id = "deleteTeam",
    summary = "Remove a team",
    description = "Its members leave it (losing its drives and what was shared with it), its grants and settings go; its drives stay, without a team. Refused (409) while a drive of it would keep no manager.",
    params(("id" = Uuid, Path, description = "The team")),
    responses(
        (status = 204, description = "Removed"),
        (status = 401, description = "Not signed in", body = ErrorBody),
        (status = 403, description = "Superadmin only, with a session (never a token) whose second factor was confirmed within 15 minutes: else `code` is `step_up_required` (confirm it, `/auth/step-up/…`) or `second_factor_required` (add one first)", body = ErrorBody),
        (status = 404, description = "No such team", body = ErrorBody),
        (status = 409, description = "A drive of it would keep no manager", body = ErrorBody),
    ),
    security(("session" = [])),
)]
async fn delete_team(
    State(state): State<AppState>,
    me: Superadmin,
    Path(id): Path<Uuid>,
) -> Result<StatusCode, HttpError> {
    delete(&state, id, me.0.id).await?;
    Ok(StatusCode::NO_CONTENT)
}

#[utoipa::path(
    put,
    path = "/teams/{id}/members/{user_id}",
    tag = "Superadmin",
    operation_id = "setTeamMemberAsSuperadmin",
    summary = "Put a user in a team, as manager or member",
    description = "How the superadmin names a team's managers. Noted in the user's account activity.",
    params(
        ("id" = Uuid, Path, description = "The team"),
        ("user_id" = Uuid, Path, description = "The user"),
    ),
    request_body(content = TeamMemberRole, content_type = "application/json"),
    responses(
        (status = 200, description = "The team now", body = Team),
        (status = 401, description = "Not signed in", body = ErrorBody),
        (status = 403, description = "Superadmin only, with a session (never a token) whose second factor was confirmed within 15 minutes: else `code` is `step_up_required` (confirm it, `/auth/step-up/…`) or `second_factor_required` (add one first)", body = ErrorBody),
        (status = 404, description = "No such team or user", body = ErrorBody),
        (status = 409, description = "A drive of the team would keep no manager", body = ErrorBody),
        (status = 415, description = "The body must be JSON (Content-Type: application/json)", body = ErrorBody),
    ),
    security(("session" = [])),
)]
async fn set_team_member_as_superadmin(
    State(state): State<AppState>,
    me: Superadmin,
    Path((id, member)): Path<(Uuid, Uuid)>,
    ApiJson(body): ApiJson<TeamMemberRole>,
) -> Result<Json<Team>, HttpError> {
    team_view(&state, id, me.0.id).await?;
    let exists: Option<i64> = sqlx::query_scalar("SELECT 1 FROM users WHERE id = ?1")
        .bind(member)
        .fetch_optional(state.db.reader())
        .await?;
    exists.ok_or_else(HttpError::not_found)?;
    set_member(
        &state,
        id,
        member,
        Some(body.role),
        Some(Actor::Superadmin(me.0.id)),
    )
    .await?;
    Ok(Json(team_view(&state, id, me.0.id).await?))
}

#[utoipa::path(
    delete,
    path = "/teams/{id}/members/{user_id}",
    tag = "Superadmin",
    operation_id = "removeTeamMemberAsSuperadmin",
    summary = "Take a user out of a team",
    description = "Noted in the user's account activity.",
    params(
        ("id" = Uuid, Path, description = "The team"),
        ("user_id" = Uuid, Path, description = "The user"),
    ),
    responses(
        (status = 200, description = "The team now", body = Team),
        (status = 401, description = "Not signed in", body = ErrorBody),
        (status = 403, description = "Superadmin only, with a session (never a token) whose second factor was confirmed within 15 minutes: else `code` is `step_up_required` (confirm it, `/auth/step-up/…`) or `second_factor_required` (add one first)", body = ErrorBody),
        (status = 404, description = "No such team, or not a member", body = ErrorBody),
        (status = 409, description = "A drive of the team would keep no manager", body = ErrorBody),
    ),
    security(("session" = [])),
)]
async fn remove_team_member_as_superadmin(
    State(state): State<AppState>,
    me: Superadmin,
    Path((id, member)): Path<(Uuid, Uuid)>,
) -> Result<Json<Team>, HttpError> {
    team_view(&state, id, me.0.id).await?;
    role_in(&state, id, member).await?;
    set_member(&state, id, member, None, Some(Actor::Superadmin(me.0.id))).await?;
    Ok(Json(team_view(&state, id, me.0.id).await?))
}
