//! A file's earlier contents (`files/versions.rs`), and the rules that say
//! how long they and the trash are kept (`retention.rs`).

use crate::error::{ErrorBody, HttpError};
use crate::extract::ApiJson;
use crate::files::Node;
use crate::files::shares::Need;
use crate::files::versions::Version;
use crate::retention::{self, Partial, Rules};
use crate::routes::auth::{AuthUser, Superadmin};
use crate::routes::files::access;
use crate::routes::files::serve_file;
use crate::state::AppState;
use axum::Json;
use axum::extract::{Path, Query, State};
use axum::http::{HeaderMap, StatusCode};
use axum::response::Response;
use serde::{Deserialize, Serialize};
use utoipa::{IntoParams, ToSchema};
use utoipa_axum::router::OpenApiRouter;
use utoipa_axum::routes;
use uuid::Uuid;

/// Under /files.
pub fn router() -> OpenApiRouter<AppState> {
    OpenApiRouter::new()
        .routes(routes!(list_versions))
        .routes(routes!(download_version))
        .routes(routes!(restore_version))
        .routes(routes!(delete_version))
}

/// The signed-in user's rules, at /retention.
pub fn retention_router() -> OpenApiRouter<AppState> {
    OpenApiRouter::new().routes(routes!(get_retention, update_retention))
}

/// The defaults, under /superadmin.
pub fn superadmin_router() -> OpenApiRouter<AppState> {
    OpenApiRouter::new().routes(routes!(get_retention_defaults, update_retention_defaults))
}

#[utoipa::path(
    get,
    path = "/{id}/versions",
    tag = "Files",
    operation_id = "listVersions",
    summary = "A file's earlier contents",
    description = "Kept when a write through Organizer (the web app, an app, WebDAV) replaced the file, and thinned by the user's rules: the newest first. Changes made directly in the files folder have none.",
    params(("id" = Uuid, Path, description = "The file id")),
    responses(
        (status = 200, description = "The versions", body = Vec<Version>),
        (status = 400, description = "A folder", body = ErrorBody),
        (status = 401, description = "Not signed in", body = ErrorBody),
        (status = 404, description = "Not found", body = ErrorBody),
    ),
    security(("session" = []), ("api_key" = [])),
)]
async fn list_versions(
    State(state): State<AppState>,
    user: AuthUser,
    Path(id): Path<Uuid>,
) -> Result<Json<Vec<Version>>, HttpError> {
    // A shared file's versions are its owner's.
    let file = access(&state, &user, id, Need::Read).await?;
    Ok(Json(state.files.versions(file.owner, id).await?))
}

#[derive(Deserialize, IntoParams)]
#[into_params(parameter_in = Query)]
pub struct VersionDownloadParams {
    /// Shown in the browser rather than saved.
    #[serde(default)]
    pub inline: bool,
}

#[utoipa::path(
    get,
    path = "/{id}/versions/{version_id}/content",
    tag = "Files",
    operation_id = "downloadVersion",
    summary = "Download an earlier content",
    description = "Under the file's name, with `Range` for a part.",
    params(
        ("id" = Uuid, Path, description = "The file id"),
        ("version_id" = Uuid, Path, description = "The version id"),
        VersionDownloadParams,
    ),
    responses(
        (status = 200, description = "The contents", content_type = "application/octet-stream"),
        (status = 206, description = "The range asked for", content_type = "application/octet-stream"),
        (status = 401, description = "Not signed in", body = ErrorBody),
        (status = 404, description = "Not found", body = ErrorBody),
    ),
    security(("session" = []), ("api_key" = [])),
)]
async fn download_version(
    State(state): State<AppState>,
    user: AuthUser,
    Path((id, version_id)): Path<(Uuid, Uuid)>,
    Query(params): Query<VersionDownloadParams>,
    headers: HeaderMap,
) -> Result<Response, HttpError> {
    let file = access(&state, &user, id, Need::Read).await?;
    let node = file.view();
    let (_, path) = state.files.version(file.owner, id, version_id).await?;
    let storage = state.files.storage(file.owner).await?;
    // A version never changes: its id is its tag.
    let etag = format!("\"v-{version_id}\"");
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

#[derive(Deserialize, IntoParams)]
#[into_params(parameter_in = Query)]
pub struct RestoreVersionParams {
    /// The file's current version: the restore is refused (409) when it
    /// changed in the meantime.
    pub if_version: i64,
}

#[utoipa::path(
    post,
    path = "/{id}/versions/{version_id}/restore",
    tag = "Files",
    operation_id = "restoreVersion",
    summary = "Make an earlier content current",
    description = "A write like any other: the contents it replaces are kept as a version, so a restore can be undone.",
    params(
        ("id" = Uuid, Path, description = "The file id"),
        ("version_id" = Uuid, Path, description = "The version id"),
        RestoreVersionParams,
    ),
    responses(
        (status = 200, description = "The file, with its new version", body = Node),
        (status = 401, description = "Not signed in", body = ErrorBody),
        (status = 403, description = "A read-only API key", body = ErrorBody),
        (status = 404, description = "Not found", body = ErrorBody),
        (status = 409, description = "The file changed in the meantime", body = ErrorBody),
    ),
    security(("session" = []), ("api_key" = [])),
)]
async fn restore_version(
    State(state): State<AppState>,
    user: AuthUser,
    Path((id, version_id)): Path<(Uuid, Uuid)>,
    Query(params): Query<RestoreVersionParams>,
) -> Result<Json<Node>, HttpError> {
    let source = Some(user.source());
    let file = access(&state, &user, id, Need::Change).await?;
    let node = state
        .files
        .restore_version(file.owner, id, version_id, params.if_version, source)
        .await?;
    Ok(Json(
        crate::routes::files::seen(&state, user.0.id, node).await?,
    ))
}

#[utoipa::path(
    delete,
    path = "/{id}/versions/{version_id}",
    tag = "Files",
    operation_id = "deleteVersion",
    summary = "Delete an earlier content for good",
    params(
        ("id" = Uuid, Path, description = "The file id"),
        ("version_id" = Uuid, Path, description = "The version id"),
    ),
    responses(
        (status = 204, description = "Deleted"),
        (status = 401, description = "Not signed in", body = ErrorBody),
        (status = 403, description = "A read-only API key", body = ErrorBody),
        (status = 404, description = "Not found", body = ErrorBody),
    ),
    security(("session" = []), ("api_key" = [])),
)]
async fn delete_version(
    State(state): State<AppState>,
    user: AuthUser,
    Path((id, version_id)): Path<(Uuid, Uuid)>,
) -> Result<StatusCode, HttpError> {
    // Deleting for good is the owner's alone.
    let file = access(&state, &user, id, Need::Read).await?;
    if !file.allows(Need::Own) {
        return Err(HttpError::new(
            StatusCode::FORBIDDEN,
            "Only the owner can delete a version for good",
        ));
    }
    state
        .files
        .delete_version(file.owner, id, version_id)
        .await?;
    Ok(StatusCode::NO_CONTENT)
}

/// How long the trash and the versions are kept.
#[derive(Serialize, ToSchema)]
pub struct Retention {
    /// The rules in effect.
    pub rules: Rules,
    /// The rules under them: the superadmin's defaults (for a user), or the
    /// built-in ones (for the defaults).
    pub defaults: Rules,
    /// The rules set by an environment variable (`trash_days` by
    /// `TRASH_DAYS`, and so on), which cannot be changed.
    pub locked: Vec<String>,
    /// The bytes the user's versions take (absent for the defaults).
    pub versions_size: Option<i64>,
    /// The bytes the user's trash takes, its files (absent for the
    /// defaults).
    pub trash_size: Option<i64>,
}

fn locked(state: &AppState) -> Vec<String> {
    state
        .files
        .retention_env()
        .fields()
        .into_iter()
        .map(String::from)
        .collect()
}

/// A change that touches a locked rule.
fn check_unlocked(state: &AppState, body: &Partial) -> Result<(), HttpError> {
    let env = state.files.retention_env().fields();
    match body.fields().into_iter().find(|f| env.contains(f)) {
        Some(field) => Err(HttpError::bad_request(format!(
            "{field} is set by the {} environment variable",
            retention::variable(field)
        ))),
        None => Ok(()),
    }
}

async fn user_view(state: &AppState, user: Uuid) -> Result<Retention, HttpError> {
    let (versions, trash) = state.files.kept_sizes(user).await?;
    Ok(Retention {
        rules: retention::rules(&state.files, user).await?,
        defaults: retention::above_user(&state.files, user).await?,
        locked: locked(state),
        versions_size: Some(versions),
        trash_size: Some(trash),
    })
}

#[utoipa::path(
    get,
    path = "",
    tag = "Files",
    operation_id = "getRetention",
    summary = "How long your trash and versions are kept",
    responses(
        (status = 200, description = "The rules", body = Retention),
        (status = 401, description = "Not signed in", body = ErrorBody),
    ),
    security(("session" = []), ("api_key" = [])),
)]
async fn get_retention(
    State(state): State<AppState>,
    user: AuthUser,
) -> Result<Json<Retention>, HttpError> {
    Ok(Json(user_view(&state, user.0.id).await?))
}

#[utoipa::path(
    put,
    path = "",
    tag = "Files",
    operation_id = "updateRetention",
    summary = "Change how long your trash and versions are kept",
    description = "A rule that is left out stays as it is; `null` goes back to the default. Fewer days or versions apply within the hour: what falls outside is deleted for good. A rule set by an environment variable cannot be changed (400).",
    request_body(content = Partial, content_type = "application/json"),
    responses(
        (status = 200, description = "The rules", body = Retention),
        (status = 400, description = "A rule out of range or locked", body = ErrorBody),
        (status = 401, description = "Not signed in", body = ErrorBody),
        (status = 403, description = "A read-only API key", body = ErrorBody),
    ),
    security(("session" = []), ("api_key" = [])),
)]
async fn update_retention(
    State(state): State<AppState>,
    user: AuthUser,
    ApiJson(body): ApiJson<serde_json::Value>,
) -> Result<Json<Retention>, HttpError> {
    let user_id = user.0.id;
    // Left out stays; null is the default: a JSON object of the four.
    let object = body
        .as_object()
        .ok_or_else(|| HttpError::bad_request("Expected an object"))?;
    let mut mine = retention::changes(&state.db, user_id).await?;
    let mut touched = Partial::default();
    for (field, slot, touch) in [
        ("trash_days", &mut mine.trash_days, &mut touched.trash_days),
        (
            "versions_hours",
            &mut mine.versions_hours,
            &mut touched.versions_hours,
        ),
        (
            "versions_days",
            &mut mine.versions_days,
            &mut touched.versions_days,
        ),
        (
            "versions_months",
            &mut mine.versions_months,
            &mut touched.versions_months,
        ),
    ] {
        let Some(value) = object.get(field) else {
            continue;
        };
        *slot = match value {
            serde_json::Value::Null => None,
            value => Some(value.as_i64().ok_or_else(|| {
                HttpError::bad_request(format!("{field} must be a whole number or null"))
            })?),
        };
        *touch = Some(0);
    }
    if let Some(field) = object.keys().find(|k| {
        ![
            "trash_days",
            "versions_hours",
            "versions_days",
            "versions_months",
        ]
        .contains(&k.as_str())
    }) {
        return Err(HttpError::bad_request(format!("Unknown rule {field}")));
    }
    check_unlocked(&state, &touched)?;
    mine.over(Rules::default())
        .validate()
        .map_err(HttpError::bad_request)?;
    let values = [
        mine.trash_days,
        mine.versions_hours,
        mine.versions_days,
        mine.versions_months,
    ];
    let touched = [
        touched.trash_days,
        touched.versions_hours,
        touched.versions_days,
        touched.versions_months,
    ];
    for ((key, value), touched) in retention::KEYS.iter().zip(values).zip(touched) {
        if touched.is_none() {
            continue;
        }
        match value {
            Some(value) => {
                crate::settings::set_own(&state, user_id, key, value.into()).await?;
            }
            None => {
                crate::settings::unset(&state.db, crate::settings::Level::User, Some(user_id), key)
                    .await?
            }
        }
    }
    Ok(Json(user_view(&state, user_id).await?))
}

async fn defaults_view(state: &AppState) -> Result<Retention, HttpError> {
    Ok(Retention {
        rules: retention::defaults(&state.files).await?,
        defaults: Rules::default(),
        locked: locked(state),
        versions_size: None,
        trash_size: None,
    })
}

#[utoipa::path(
    get,
    path = "/retention",
    tag = "Superadmin",
    operation_id = "getRetentionDefaults",
    summary = "The default trash and version rules",
    description = "What a user has until they change it.",
    responses(
        (status = 200, description = "The defaults", body = Retention),
        (status = 401, description = "Not signed in", body = ErrorBody),
        (status = 403, description = "Superadmin only, with a session (never a token) whose second factor was confirmed within 15 minutes: else `code` is `step_up_required` (confirm it, `/auth/step-up/…`) or `second_factor_required` (add one first)", body = ErrorBody),
    ),
    security(("session" = [])),
)]
async fn get_retention_defaults(
    State(state): State<AppState>,
    _: Superadmin,
) -> Result<Json<Retention>, HttpError> {
    Ok(Json(defaults_view(&state).await?))
}

#[utoipa::path(
    put,
    path = "/retention",
    tag = "Superadmin",
    operation_id = "updateRetentionDefaults",
    summary = "Change the default trash and version rules",
    description = "A rule that is left out stays as it is. Users who did not change a rule follow the new default within the hour. A rule set by an environment variable cannot be changed (400).",
    request_body(content = Partial, content_type = "application/json"),
    responses(
        (status = 200, description = "The defaults", body = Retention),
        (status = 400, description = "A rule out of range or locked", body = ErrorBody),
        (status = 401, description = "Not signed in", body = ErrorBody),
        (status = 403, description = "Superadmin only, with a session (never a token) whose second factor was confirmed within 15 minutes: else `code` is `step_up_required` (confirm it, `/auth/step-up/…`) or `second_factor_required` (add one first)", body = ErrorBody),
    ),
    security(("session" = [])),
)]
async fn update_retention_defaults(
    State(state): State<AppState>,
    _: Superadmin,
    ApiJson(body): ApiJson<Partial>,
) -> Result<Json<Retention>, HttpError> {
    check_unlocked(&state, &body)?;
    body.over(retention::defaults(&state.files).await?)
        .validate()
        .map_err(HttpError::bad_request)?;
    let values = [
        body.trash_days,
        body.versions_hours,
        body.versions_days,
        body.versions_months,
    ];
    for (key, value) in retention::KEYS.iter().zip(values) {
        let Some(value) = value else { continue };
        // A rule the superadmin locked stays locked, with its new value.
        let mode = crate::settings::row(&state.db, crate::settings::Level::Superadmin, None, key)
            .await?
            .map(|r| r.mode)
            .filter(|m| *m != crate::settings::Mode::Range)
            .unwrap_or(crate::settings::Mode::Default);
        crate::settings::set(
            &state,
            crate::settings::Level::Superadmin,
            None,
            key,
            &crate::settings::LevelValue {
                mode,
                value: Some(value.into()),
                min: None,
                max: None,
            },
            crate::settings::For::default(),
        )
        .await?;
    }
    Ok(Json(defaults_view(&state).await?))
}
