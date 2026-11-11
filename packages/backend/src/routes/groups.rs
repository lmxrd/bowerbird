//! M6b's groups (docs/M6B_DESIGN.md §4) are teams now (docs/M7B_DESIGN.md
//! §2, §3, `routes/teams.rs`): these endpoints answer for one release,
//! over the teams, a group's members being the team's (its managers too).
//! A grant to a team reaches its members as they are now and later: a
//! member who leaves loses it (and it goes from their files).

use crate::error::{ErrorBody, HttpError};
use crate::extract::ApiJson;
use crate::routes::auth::Superadmin;
use crate::routes::shares::Person;
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

/// Nested under /superadmin.
pub fn superadmin_router() -> OpenApiRouter<AppState> {
    OpenApiRouter::new()
        .routes(routes!(list_groups, create_group))
        .routes(routes!(update_group, delete_group))
}

/// A group, with its members.
#[derive(Serialize, ToSchema)]
pub struct Group {
    pub id: Uuid,
    pub name: String,
    /// By email.
    pub members: Vec<Person>,
    pub created_at: String,
}

async fn group(state: &AppState, id: Uuid) -> Result<Group, HttpError> {
    let (name, created_at): (String, String) =
        sqlx::query_as("SELECT name, created_at FROM teams WHERE id = ?1")
            .bind(id)
            .fetch_optional(state.db.reader())
            .await?
            .ok_or_else(HttpError::not_found)?;
    let members: Vec<Person> = sqlx::query_as(
        "SELECT u.id, u.email FROM team_members m JOIN users u ON u.id = m.user_id
         WHERE m.team_id = ?1 ORDER BY u.email",
    )
    .bind(id)
    .fetch_all(state.db.reader())
    .await?;
    Ok(Group {
        id,
        name,
        members,
        created_at,
    })
}

#[utoipa::path(
    get,
    path = "/groups",
    tag = "Superadmin",
    operation_id = "listGroups",
    summary = "The groups",
    description = "Deprecated, answered for one release: `GET /superadmin/teams`. The teams, by name, each with its members.",
    responses(
        (status = 200, description = "The groups", body = Vec<Group>),
        (status = 401, description = "Not signed in", body = ErrorBody),
        (status = 403, description = "Superadmin only, with a session (never a token) whose second factor was confirmed within 15 minutes: else `code` is `step_up_required` (confirm it, `/auth/step-up/…`) or `second_factor_required` (add one first)", body = ErrorBody),
    ),
    security(("session" = [])),
)]
async fn list_groups(
    State(state): State<AppState>,
    _: Superadmin,
) -> Result<Json<Vec<Group>>, HttpError> {
    let ids: Vec<Uuid> = sqlx::query_scalar("SELECT id FROM teams ORDER BY name, id")
        .fetch_all(state.db.reader())
        .await?;
    let mut out = Vec::with_capacity(ids.len());
    for id in ids {
        out.push(group(&state, id).await?);
    }
    Ok(Json(out))
}

/// A group's name and members.
#[derive(Deserialize, ToSchema)]
pub struct GroupBody {
    /// Unique, ignoring case.
    pub name: Option<String>,
    /// The user ids of every member: replaces the members.
    pub members: Option<Vec<Uuid>>,
}

fn clean_name(name: &str) -> Result<String, HttpError> {
    let name = name.trim();
    if name.is_empty() || name.chars().count() > 100 {
        return Err(HttpError::bad_request(
            "A group's name is 1 to 100 characters",
        ));
    }
    Ok(name.to_string())
}

/// Set a group's name, and its members: who joins is a member of the
/// team, who is not named leaves it (each with what follows for their
/// drives and shares).
async fn save(
    state: &AppState,
    id: Uuid,
    body: &GroupBody,
    new: bool,
    by: Uuid,
) -> Result<(), HttpError> {
    let name = body.name.as_deref().map(clean_name).transpose()?;
    if let Some(members) = &body.members {
        for member in members {
            let exists: Option<i64> = sqlx::query_scalar("SELECT 1 FROM users WHERE id = ?1")
                .bind(member)
                .fetch_optional(state.db.reader())
                .await?;
            if exists.is_none() {
                return Err(HttpError::bad_request("No such user"));
            }
        }
    }
    let saved = if new {
        sqlx::query("INSERT INTO teams (id, name, created_at) VALUES (?1, ?2, ?3)")
            .bind(id)
            .bind(name.as_deref().unwrap_or_default())
            .bind(crate::auth::now_iso())
            .execute(state.db.writer())
            .await
    } else if let Some(name) = &name {
        sqlx::query("UPDATE teams SET name = ?1 WHERE id = ?2")
            .bind(name)
            .bind(id)
            .execute(state.db.writer())
            .await
    } else {
        Ok(Default::default())
    };
    match saved {
        Ok(_) => {}
        Err(sqlx::Error::Database(e)) if e.is_unique_violation() => {
            return Err(HttpError::conflict("A group has that name already"));
        }
        Err(e) => return Err(e.into()),
    }
    if let Some(members) = &body.members {
        let now: Vec<Uuid> =
            sqlx::query_scalar("SELECT user_id FROM team_members WHERE team_id = ?1")
                .bind(id)
                .fetch_all(state.db.reader())
                .await?;
        for gone in now.iter().filter(|u| !members.contains(u)) {
            crate::routes::teams::set_member(
                state,
                id,
                *gone,
                None,
                Some(crate::activity::Actor::Superadmin(by)),
            )
            .await?;
        }
        for member in members.iter().filter(|u| !now.contains(u)) {
            crate::routes::teams::set_member(
                state,
                id,
                *member,
                Some(crate::files::grants::TeamRole::Member),
                Some(crate::activity::Actor::Superadmin(by)),
            )
            .await?;
        }
    }
    Ok(())
}

#[utoipa::path(
    post,
    path = "/groups",
    tag = "Superadmin",
    operation_id = "createGroup",
    summary = "Make a group",
    description = "Deprecated, answered for one release: `POST /superadmin/teams`. A team, its members members (no manager of its own).",
    request_body(content = GroupBody, content_type = "application/json"),
    responses(
        (status = 201, description = "Made", body = Group),
        (status = 400, description = "No name, or no such user", body = ErrorBody),
        (status = 401, description = "Not signed in", body = ErrorBody),
        (status = 403, description = "Superadmin only, with a session (never a token) whose second factor was confirmed within 15 minutes: else `code` is `step_up_required` (confirm it, `/auth/step-up/…`) or `second_factor_required` (add one first)", body = ErrorBody),
        (status = 409, description = "A group has that name", body = ErrorBody),
        (status = 415, description = "The body must be JSON (Content-Type: application/json)", body = ErrorBody),
    ),
    security(("session" = [])),
)]
async fn create_group(
    State(state): State<AppState>,
    me: Superadmin,
    ApiJson(body): ApiJson<GroupBody>,
) -> Result<Response, HttpError> {
    if body.name.is_none() {
        return Err(HttpError::bad_request("A group needs a name"));
    }
    let id = Uuid::now_v7();
    save(&state, id, &body, true, me.0.id).await?;
    Ok((StatusCode::CREATED, Json(group(&state, id).await?)).into_response())
}

#[utoipa::path(
    patch,
    path = "/groups/{id}",
    tag = "Superadmin",
    operation_id = "updateGroup",
    summary = "Rename a group, or set its members",
    description = "Deprecated, answered for one release: the team's endpoints. A member who leaves loses what was shared with the team, and it goes from their files.",
    params(("id" = Uuid, Path, description = "The group id")),
    request_body(content = GroupBody, content_type = "application/json"),
    responses(
        (status = 200, description = "Changed", body = Group),
        (status = 400, description = "An empty name, or no such user", body = ErrorBody),
        (status = 401, description = "Not signed in", body = ErrorBody),
        (status = 403, description = "Superadmin only, with a session (never a token) whose second factor was confirmed within 15 minutes: else `code` is `step_up_required` (confirm it, `/auth/step-up/…`) or `second_factor_required` (add one first)", body = ErrorBody),
        (status = 404, description = "Not found", body = ErrorBody),
        (status = 409, description = "A group has that name", body = ErrorBody),
        (status = 415, description = "The body must be JSON (Content-Type: application/json)", body = ErrorBody),
    ),
    security(("session" = [])),
)]
async fn update_group(
    State(state): State<AppState>,
    me: Superadmin,
    Path(id): Path<Uuid>,
    ApiJson(body): ApiJson<GroupBody>,
) -> Result<Json<Group>, HttpError> {
    group(&state, id).await?;
    save(&state, id, &body, false, me.0.id).await?;
    Ok(Json(group(&state, id).await?))
}

#[utoipa::path(
    delete,
    path = "/groups/{id}",
    tag = "Superadmin",
    operation_id = "deleteGroup",
    summary = "Delete a group",
    description = "Deprecated, answered for one release: `DELETE /superadmin/teams/{id}`. With its grants: its members lose what was shared with it.",
    params(("id" = Uuid, Path, description = "The group id")),
    responses(
        (status = 204, description = "Deleted"),
        (status = 401, description = "Not signed in", body = ErrorBody),
        (status = 403, description = "Superadmin only, with a session (never a token) whose second factor was confirmed within 15 minutes: else `code` is `step_up_required` (confirm it, `/auth/step-up/…`) or `second_factor_required` (add one first)", body = ErrorBody),
        (status = 404, description = "Not found", body = ErrorBody),
    ),
    security(("session" = [])),
)]
async fn delete_group(
    State(state): State<AppState>,
    me: Superadmin,
    Path(id): Path<Uuid>,
) -> Result<StatusCode, HttpError> {
    crate::routes::teams::delete(&state, id, me.0.id).await?;
    Ok(StatusCode::NO_CONTENT)
}
