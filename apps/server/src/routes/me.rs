//! The signed-in user's own: their account activity (docs/M7B_DESIGN.md
//! §5) and their primary team (§3).

use crate::activity::{self, ActivityEntry};
use crate::error::{ErrorBody, HttpError};
use crate::extract::ApiJson;
use crate::routes::auth::SessionUser;
use crate::state::AppState;
use axum::Json;
use axum::extract::{Query, State};
use axum::http::StatusCode;
use serde::Deserialize;
use utoipa::{IntoParams, ToSchema};
use utoipa_axum::router::OpenApiRouter;
use utoipa_axum::routes;
use uuid::Uuid;

/// Nested under /me.
pub fn router() -> OpenApiRouter<AppState> {
    OpenApiRouter::new()
        .routes(routes!(my_activity))
        .routes(routes!(set_primary_team))
}

#[derive(Deserialize, IntoParams)]
#[into_params(parameter_in = Query)]
pub struct ActivityParams {
    /// The `id` of the last entry of the previous page.
    pub before: Option<Uuid>,
    /// 1 to 200; 50 by default.
    pub limit: Option<i64>,
}

#[utoipa::path(
    get,
    path = "/activity",
    tag = "Account",
    operation_id = "getMyActivity",
    summary = "My account activity",
    description = "What the superadmin, a team's manager or the server's command line did to the user's account (made it, disabled it, changed its role, storage limit, files folder or teams, reset its two-factor authentication or password, changed its access to a shared drive; a team's manager: added them to the team, took them out, changed their team role), the newest first, paged by `before`. A session only.",
    params(ActivityParams),
    responses(
        (status = 200, description = "The entries", body = Vec<ActivityEntry>),
        (status = 401, description = "Not signed in", body = ErrorBody),
        (status = 403, description = "A token, not a session", body = ErrorBody),
    ),
    security(("session" = [])),
)]
async fn my_activity(
    State(state): State<AppState>,
    user: SessionUser,
    Query(params): Query<ActivityParams>,
) -> Result<Json<Vec<ActivityEntry>>, HttpError> {
    let limit = params.limit.unwrap_or(50).clamp(1, 200);
    Ok(Json(
        activity::of(&state.db, user.0.id, params.before, limit).await?,
    ))
}

/// The team whose defaults the user gets.
#[derive(Deserialize, ToSchema)]
pub struct PrimaryTeam {
    /// One of the user's teams.
    pub team_id: Uuid,
}

#[utoipa::path(
    put,
    path = "/primary-team",
    tag = "Account",
    operation_id = "setPrimaryTeam",
    summary = "Choose my primary team",
    description = "Where the user's defaults come from when they are in several teams (until they choose, the first they joined).",
    request_body(content = PrimaryTeam, content_type = "application/json"),
    responses(
        (status = 204, description = "Chosen"),
        (status = 401, description = "Not signed in", body = ErrorBody),
        (status = 403, description = "A token, not a session", body = ErrorBody),
        (status = 404, description = "Not a team of the user's", body = ErrorBody),
        (status = 415, description = "The body must be JSON (Content-Type: application/json)", body = ErrorBody),
    ),
    security(("session" = [])),
)]
async fn set_primary_team(
    State(state): State<AppState>,
    user: SessionUser,
    ApiJson(body): ApiJson<PrimaryTeam>,
) -> Result<StatusCode, HttpError> {
    let member: Option<i64> =
        sqlx::query_scalar("SELECT 1 FROM team_members WHERE team_id = ?1 AND user_id = ?2")
            .bind(body.team_id)
            .bind(user.0.id)
            .fetch_optional(state.db.reader())
            .await?;
    member.ok_or_else(HttpError::not_found)?;
    sqlx::query("UPDATE users SET primary_team_id = ?1 WHERE id = ?2")
        .bind(body.team_id)
        .bind(user.0.id)
        .execute(state.db.writer())
        .await?;
    Ok(StatusCode::NO_CONTENT)
}
