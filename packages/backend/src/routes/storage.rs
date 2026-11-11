//! Where the user's files are: the files root (docs/M1_DESIGN.md §4). Set at
//! setup, changed here by a superadmin, or locked by `FILES_DIR`.

use crate::error::{ErrorBody, HttpError};
use crate::extract::ApiJson;
use crate::routes::auth::Superadmin;
use crate::state::AppState;
use axum::Json;
use axum::extract::State;
use serde::{Deserialize, Serialize};
use std::path::PathBuf;
use utoipa::ToSchema;
use utoipa_axum::router::OpenApiRouter;
use utoipa_axum::routes;

pub fn superadmin_router() -> OpenApiRouter<AppState> {
    OpenApiRouter::new().routes(routes!(get_storage, set_storage))
}

/// Where the signed-in user's files are.
#[derive(Serialize, ToSchema)]
pub struct Storage {
    /// The absolute path of the files folder on the server.
    pub files_root: String,
    /// Whether it is the default, a folder in the data dir.
    pub default: bool,
    /// Whether `FILES_DIR` sets it, so that it cannot be changed here.
    pub locked: bool,
    /// Whether the folder is there: false for a share that is not mounted.
    pub available: bool,
}

/// The files folder to use; null for the default in the data dir.
#[derive(Deserialize, ToSchema)]
pub struct SetStorage {
    /// An absolute path on the server, of an existing folder Organizer can
    /// write to, apart from the data dir.
    #[schema(required = true)]
    pub files_root: Option<String>,
}

/// Where a user's files are: the superadmin's own here, any user's in the list
/// of users (`routes/users.rs`).
pub(crate) async fn storage(state: &AppState, user_id: uuid::Uuid) -> Result<Storage, HttpError> {
    let stored = crate::files::StoredRoot::of(state.db.reader(), user_id)
        .await?
        .ok_or_else(HttpError::not_found)?;
    let root = state.files.root_for(user_id, &stored);
    Ok(Storage {
        default: root == state.files.default_root(user_id),
        locked: state.files.root_locked(&stored),
        available: root.is_dir(),
        files_root: root.to_string_lossy().into_owned(),
    })
}

#[utoipa::path(
    get,
    path = "/storage",
    tag = "Superadmin",
    operation_id = "getStorage",
    summary = "Where the files are",
    responses(
        (status = 200, description = "The files folder", body = Storage),
        (status = 401, description = "Not signed in", body = ErrorBody),
        (status = 403, description = "Superadmin only, with a session (never a token) whose second factor was confirmed within 15 minutes: else `code` is `step_up_required` (confirm it, `/auth/step-up/…`) or `second_factor_required` (add one first)", body = ErrorBody),
    ),
    security(("session" = [])),
)]
async fn get_storage(
    State(state): State<AppState>,
    user: Superadmin,
) -> Result<Json<Storage>, HttpError> {
    Ok(Json(storage(&state, user.0.id).await?))
}

#[utoipa::path(
    patch,
    path = "/storage",
    tag = "Superadmin",
    operation_id = "setStorage",
    summary = "Set where the files are",
    description = "Another folder, such as a share that already holds files: Organizer indexes it where it is and copies nothing. The tree is indexed from scratch in the background: the ids and the change feed of the old one are dropped, and the apps list everything again. Refused while `FILES_DIR` sets the folder.",
    request_body(content = SetStorage, content_type = "application/json"),
    responses(
        (status = 200, description = "Set; indexing has started", body = Storage),
        (status = 400, description = "Not a folder Organizer can use, or set by FILES_DIR", body = ErrorBody),
        (status = 401, description = "Not signed in", body = ErrorBody),
        (status = 403, description = "Superadmin only, with a session (never a token) whose second factor was confirmed within 15 minutes: else `code` is `step_up_required` (confirm it, `/auth/step-up/…`) or `second_factor_required` (add one first)", body = ErrorBody),
        (status = 415, description = "The body must be JSON (Content-Type: application/json)", body = ErrorBody),
    ),
    security(("session" = [])),
)]
async fn set_storage(
    State(state): State<AppState>,
    user: Superadmin,
    ApiJson(body): ApiJson<SetStorage>,
) -> Result<Json<Storage>, HttpError> {
    let root = body
        .files_root
        .map(|r| r.trim().to_string())
        .filter(|r| !r.is_empty())
        .map(PathBuf::from);
    state.files.set_root(user.0.id, root.as_deref()).await?;
    Ok(Json(storage(&state, user.0.id).await?))
}
