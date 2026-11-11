//! The settings with levels (docs/M7B_DESIGN.md §4, §6): a user's own
//! (`/settings`), the superadmin's (`/superadmin/settings`) and a drive's
//! (`/drives/{id}/settings`); a team's are in `routes/teams.rs`. The
//! resolver and the rules are in `settings.rs`.

use crate::error::{ErrorBody, HttpError};
use crate::extract::ApiJson;
use crate::files::roles::Role;
use crate::routes::auth::{AuthUser, Superadmin};
use crate::routes::teams::one_level;
use crate::settings::{self, For, Level, LevelSetting, LevelValue, UserSetting};
use crate::state::AppState;
use axum::Json;
use axum::extract::{Path, State};
use axum::http::StatusCode;
use serde::Deserialize;
use serde_json::Value;
use utoipa::ToSchema;
use utoipa_axum::router::OpenApiRouter;
use utoipa_axum::routes;
use uuid::Uuid;

/// Nested under /settings.
pub fn router() -> OpenApiRouter<AppState> {
    OpenApiRouter::new()
        .routes(routes!(list_my_settings))
        .routes(routes!(set_my_setting, unset_my_setting))
}

/// Nested under /superadmin.
pub fn superadmin_router() -> OpenApiRouter<AppState> {
    OpenApiRouter::new()
        .routes(routes!(list_superadmin_settings))
        .routes(routes!(set_superadmin_setting, unset_superadmin_setting))
}

/// Nested under /drives.
pub fn drives_router() -> OpenApiRouter<AppState> {
    OpenApiRouter::new()
        .routes(routes!(list_drive_settings))
        .routes(routes!(set_drive_setting, unset_drive_setting))
}

#[utoipa::path(
    get,
    path = "",
    tag = "Settings",
    operation_id = "listMySettings",
    summary = "My settings",
    description = "Every setting with levels as it applies to the user: its value, where it comes from (`source`: `builtin`, `env`, `superadmin`, `team` with its name, `user`), who locked it (`locked_by`; the clients show it disabled with \"Set by your administrator\" or \"Set by <team>\"), the range the levels above leave (`min`, `max`), and whether they may change it (`may_change`).",
    responses(
        (status = 200, description = "The settings", body = Vec<UserSetting>),
        (status = 401, description = "Not signed in", body = ErrorBody),
    ),
    security(("session" = []), ("api_key" = [])),
)]
async fn list_my_settings(
    State(state): State<AppState>,
    user: AuthUser,
) -> Result<Json<Vec<UserSetting>>, HttpError> {
    Ok(Json(settings::user_view(&state, user.0.id).await?))
}

/// A user's value of a setting.
#[derive(Deserialize, ToSchema)]
pub struct MyValue {
    /// JSON of the setting's kind (a boolean, number, string, list, or
    /// null where the kind allows it).
    pub value: Value,
}

/// One of the user's settings, as it applies now.
async fn mine(state: &AppState, user: Uuid, key: &str) -> Result<UserSetting, HttpError> {
    settings::user_view(state, user)
        .await?
        .into_iter()
        .find(|s| s.resolved.key == key)
        .ok_or_else(HttpError::not_found)
}

#[utoipa::path(
    put,
    path = "/{key}",
    tag = "Settings",
    operation_id = "setMySetting",
    summary = "Change one of my settings",
    description = "Within what the levels above leave: a setting they locked is refused (403), a number outside their range too (400).",
    params(("key" = String, Path, description = "The setting, e.g. `drop_box.pending_mode`")),
    request_body(content = MyValue, content_type = "application/json"),
    responses(
        (status = 200, description = "The setting now", body = UserSetting),
        (status = 400, description = "An invalid value, or no user level", body = ErrorBody),
        (status = 401, description = "Not signed in", body = ErrorBody),
        (status = 403, description = "Locked above, or a read-only key", body = ErrorBody),
        (status = 404, description = "No such setting", body = ErrorBody),
        (status = 415, description = "The body must be JSON (Content-Type: application/json)", body = ErrorBody),
    ),
    security(("session" = []), ("api_key" = [])),
)]
async fn set_my_setting(
    State(state): State<AppState>,
    user: AuthUser,
    Path(key): Path<String>,
    ApiJson(body): ApiJson<MyValue>,
) -> Result<Json<UserSetting>, HttpError> {
    settings::set_own(&state, user.0.id, &key, body.value).await?;
    Ok(Json(mine(&state, user.0.id, &key).await?))
}

#[utoipa::path(
    delete,
    path = "/{key}",
    tag = "Settings",
    operation_id = "unsetMySetting",
    summary = "Go back to the default of one of my settings",
    description = "What the levels above give (or the built-in default) applies again.",
    params(("key" = String, Path, description = "The setting")),
    responses(
        (status = 200, description = "The setting now", body = UserSetting),
        (status = 401, description = "Not signed in", body = ErrorBody),
        (status = 403, description = "A read-only key", body = ErrorBody),
        (status = 404, description = "No such setting", body = ErrorBody),
    ),
    security(("session" = []), ("api_key" = [])),
)]
async fn unset_my_setting(
    State(state): State<AppState>,
    user: AuthUser,
    Path(key): Path<String>,
) -> Result<Json<UserSetting>, HttpError> {
    settings::unset(&state.db, Level::User, Some(user.0.id), &key).await?;
    Ok(Json(mine(&state, user.0.id, &key).await?))
}

#[utoipa::path(
    get,
    path = "/settings",
    tag = "Superadmin",
    operation_id = "listSuperadminSettings",
    summary = "The superadmin's settings",
    description = "Each setting with a superadmin level: its mode here (`locked`, `default`, `range`, or null: not set), value and range, and what an environment variable leaves it (`above`: with `above.locked_by`, it cannot be set).",
    responses(
        (status = 200, description = "The settings", body = Vec<LevelSetting>),
        (status = 401, description = "Not signed in", body = ErrorBody),
        (status = 403, description = "Superadmin only, with a session (never a token) whose second factor was confirmed within 15 minutes: else `code` is `step_up_required` (confirm it, `/auth/step-up/…`) or `second_factor_required` (add one first)", body = ErrorBody),
    ),
    security(("session" = [])),
)]
async fn list_superadmin_settings(
    State(state): State<AppState>,
    _: Superadmin,
) -> Result<Json<Vec<LevelSetting>>, HttpError> {
    Ok(Json(
        settings::level_view(&state, Level::Superadmin, None, For::default()).await?,
    ))
}

#[utoipa::path(
    put,
    path = "/settings/{key}",
    tag = "Superadmin",
    operation_id = "setSuperadminSetting",
    summary = "Set one of the superadmin's settings",
    description = "Locked for everyone (shown to them with \"Set by your administrator\"), a default the teams and users may change, or a range (a number: at least, at most) they choose within. A setting an environment variable sets is refused (403).",
    params(("key" = String, Path, description = "The setting")),
    request_body(content = LevelValue, content_type = "application/json"),
    responses(
        (status = 200, description = "The setting now", body = LevelSetting),
        (status = 400, description = "An invalid value or range", body = ErrorBody),
        (status = 401, description = "Not signed in", body = ErrorBody),
        (status = 403, description = "Set by an environment variable; or superadmin only, with a session (never a token) whose second factor was confirmed within 15 minutes: else `code` is `step_up_required` (confirm it, `/auth/step-up/…`) or `second_factor_required` (add one first)", body = ErrorBody),
        (status = 404, description = "No such setting", body = ErrorBody),
        (status = 415, description = "The body must be JSON (Content-Type: application/json)", body = ErrorBody),
    ),
    security(("session" = [])),
)]
async fn set_superadmin_setting(
    State(state): State<AppState>,
    _: Superadmin,
    Path(key): Path<String>,
    ApiJson(body): ApiJson<LevelValue>,
) -> Result<Json<LevelSetting>, HttpError> {
    settings::set(&state, Level::Superadmin, None, &key, &body, For::default()).await?;
    Ok(Json(
        one_level(&state, Level::Superadmin, None, &key).await?,
    ))
}

#[utoipa::path(
    delete,
    path = "/settings/{key}",
    tag = "Superadmin",
    operation_id = "unsetSuperadminSetting",
    summary = "Leave one of the superadmin's settings not set",
    params(("key" = String, Path, description = "The setting")),
    responses(
        (status = 200, description = "The setting now", body = LevelSetting),
        (status = 401, description = "Not signed in", body = ErrorBody),
        (status = 403, description = "Superadmin only, with a session (never a token) whose second factor was confirmed within 15 minutes: else `code` is `step_up_required` (confirm it, `/auth/step-up/…`) or `second_factor_required` (add one first)", body = ErrorBody),
        (status = 404, description = "No such setting", body = ErrorBody),
    ),
    security(("session" = [])),
)]
async fn unset_superadmin_setting(
    State(state): State<AppState>,
    _: Superadmin,
    Path(key): Path<String>,
) -> Result<Json<LevelSetting>, HttpError> {
    settings::unset(&state.db, Level::Superadmin, None, &key).await?;
    Ok(Json(
        one_level(&state, Level::Superadmin, None, &key).await?,
    ))
}

/// A drive of the user's, with their role on it.
async fn drive_of(state: &AppState, user: Uuid, id: &str) -> Result<(Uuid, Role), HttpError> {
    let id: Uuid = id.parse().map_err(|_| HttpError::not_found())?;
    let role = crate::files::roles::drive_role(state.db.reader(), user, id)
        .await?
        .ok_or_else(HttpError::not_found)?;
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

#[utoipa::path(
    get,
    path = "/{id}/settings",
    tag = "Drives",
    operation_id = "listDriveSettings",
    summary = "A drive's settings",
    description = "Each setting with a drive level (public links; for a shared drive, the trash and versions and whether it may be mirrored too): the drive's mode, value and range, and what the levels above leave it (for a shared drive: the superadmin's and its team's; for My drive: its user's).",
    params(("id" = String, Path, description = "The drive's id")),
    responses(
        (status = 200, description = "The drive's settings", body = Vec<LevelSetting>),
        (status = 401, description = "Not signed in", body = ErrorBody),
        (status = 404, description = "Not a drive of the user's", body = ErrorBody),
    ),
    security(("session" = []), ("api_key" = [])),
)]
async fn list_drive_settings(
    State(state): State<AppState>,
    user: AuthUser,
    Path(id): Path<String>,
) -> Result<Json<Vec<LevelSetting>>, HttpError> {
    let (drive, _) = drive_of(&state, user.0.id, &id).await?;
    let mut out =
        settings::level_view(&state, Level::Drive, Some(drive), For::drive(drive)).await?;
    // My drive's trash and versions are its user's own settings; only its
    // user mirrors it.
    if drive == crate::files::drives::my(user.0.id) {
        out.retain(|s| !s.key.starts_with("retention.") && s.key != "mirrors.allowed");
    }
    Ok(Json(out))
}

#[utoipa::path(
    put,
    path = "/{id}/settings/{key}",
    tag = "Drives",
    operation_id = "setDriveSetting",
    summary = "Set a drive's setting",
    description = "By its managers (My drive: its user), within what the levels above leave.",
    params(
        ("id" = String, Path, description = "The drive's id"),
        ("key" = String, Path, description = "The setting"),
    ),
    request_body(content = LevelValue, content_type = "application/json"),
    responses(
        (status = 200, description = "The setting now", body = LevelSetting),
        (status = 400, description = "An invalid value or range, or no drive level", body = ErrorBody),
        (status = 401, description = "Not signed in", body = ErrorBody),
        (status = 403, description = "Not a manager of it, locked above, or a read-only key", body = ErrorBody),
        (status = 404, description = "Not a drive of the user's, or no such setting", body = ErrorBody),
        (status = 415, description = "The body must be JSON (Content-Type: application/json)", body = ErrorBody),
    ),
    security(("session" = []), ("api_key" = [])),
)]
async fn set_drive_setting(
    State(state): State<AppState>,
    user: AuthUser,
    Path((id, key)): Path<(String, String)>,
    ApiJson(body): ApiJson<LevelValue>,
) -> Result<Json<LevelSetting>, HttpError> {
    let (drive, role) = drive_of(&state, user.0.id, &id).await?;
    manages(role)?;
    if drive == crate::files::drives::my(user.0.id) && key.starts_with("retention.") {
        return Err(HttpError::bad_request(
            "My drive's trash and versions are your own settings: PUT /settings/{key}",
        ));
    }
    settings::set(
        &state,
        Level::Drive,
        Some(drive),
        &key,
        &body,
        For::drive(drive),
    )
    .await?;
    Ok(Json(
        one_level(&state, Level::Drive, Some(drive), &key).await?,
    ))
}

#[utoipa::path(
    delete,
    path = "/{id}/settings/{key}",
    tag = "Drives",
    operation_id = "unsetDriveSetting",
    summary = "Leave a drive's setting not set",
    params(
        ("id" = String, Path, description = "The drive's id"),
        ("key" = String, Path, description = "The setting"),
    ),
    responses(
        (status = 200, description = "The setting now", body = LevelSetting),
        (status = 401, description = "Not signed in", body = ErrorBody),
        (status = 403, description = "Not a manager of it, or a read-only key", body = ErrorBody),
        (status = 404, description = "Not a drive of the user's, or no such setting", body = ErrorBody),
    ),
    security(("session" = []), ("api_key" = [])),
)]
async fn unset_drive_setting(
    State(state): State<AppState>,
    user: AuthUser,
    Path((id, key)): Path<(String, String)>,
) -> Result<Json<LevelSetting>, HttpError> {
    let (drive, role) = drive_of(&state, user.0.id, &id).await?;
    manages(role)?;
    settings::unset(&state.db, Level::Drive, Some(drive), &key).await?;
    Ok(Json(
        one_level(&state, Level::Drive, Some(drive), &key).await?,
    ))
}
