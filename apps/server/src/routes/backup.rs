//! Backups (`backup/`): the superadmin's settings, test and run, and each
//! user's view of their own files in the snapshots, to restore a file or
//! folder next to the current one.

use crate::backup::run::{self, Snapshot, SnapshotEntry, TestResult};
use crate::backup::{self, BackupStatus, Settings};
use crate::error::{ErrorBody, HttpError};
use crate::extract::ApiJson;
use crate::files::Node;
use crate::files::ops::WriteTarget;
use crate::routes::auth::{AuthUser, Superadmin};
use crate::state::AppState;
use axum::Json;
use axum::extract::{Path, Query, State};
use axum::http::StatusCode;
use serde::{Deserialize, Serialize};
use utoipa::{IntoParams, ToSchema};
use utoipa_axum::router::OpenApiRouter;
use utoipa_axum::routes;

/// The settings, under /superadmin.
pub fn superadmin_router() -> OpenApiRouter<AppState> {
    OpenApiRouter::new()
        .routes(routes!(get_settings, update_settings))
        .routes(routes!(test_settings))
        .routes(routes!(run_now))
}

/// The snapshots of the signed-in user's files, at /backups.
pub fn router() -> OpenApiRouter<AppState> {
    OpenApiRouter::new()
        .routes(routes!(list_snapshots))
        .routes(routes!(list_snapshot_files))
        .routes(routes!(restore_from_snapshot))
}

/// The backup settings in effect, and how the backups went. The S3 secret
/// key and the password are never sent back.
#[derive(Serialize, ToSchema)]
pub struct BackupSettings {
    /// A repository and a password are set: backups run.
    pub enabled: bool,
    /// An absolute path or `s3://endpoint/bucket/path`; empty when there are no backups.
    pub url: String,
    pub region: String,
    pub access_key: String,
    pub secret_key_set: bool,
    pub password_set: bool,
    /// `daily HH:MM` or `every Nh`.
    pub schedule: String,
    pub keep_daily: i64,
    pub keep_weekly: i64,
    pub keep_monthly: i64,
    /// The fields set by an environment variable (`url` by
    /// `BACKUP_URL`, the keep rules by `BACKUP_KEEP`, ...), which
    /// cannot be changed here.
    pub locked: Vec<String>,
    pub status: BackupStatus,
    /// When the next backup is due; absent without backups.
    #[schema(required = true)]
    pub next_run: Option<String>,
    /// A backup is queued or running.
    pub running: bool,
}

/// The settings to change (or to test); a field left out stays as it is.
#[derive(Deserialize, ToSchema, Default)]
pub struct UpdateBackupSettings {
    pub url: Option<String>,
    pub region: Option<String>,
    pub access_key: Option<String>,
    pub secret_key: Option<String>,
    /// The repository's password. Changing it does not re-encrypt an
    /// existing repository: the new one must be the one it was made with.
    pub password: Option<String>,
    pub schedule: Option<String>,
    pub keep_daily: Option<i64>,
    pub keep_weekly: Option<i64>,
    pub keep_monthly: Option<i64>,
}

async fn view(state: &AppState) -> Result<BackupSettings, HttpError> {
    let settings = state.backup.settings();
    let running: i64 = sqlx::query_scalar(
        "SELECT count(*) FROM jobs WHERE kind = ?1 AND status IN ('queued', 'running')",
    )
    .bind(backup::JOB)
    .fetch_one(state.db.reader())
    .await?;
    Ok(BackupSettings {
        enabled: settings.enabled(),
        secret_key_set: !settings.secret_key.is_empty(),
        password_set: !settings.password.is_empty(),
        locked: state
            .backup
            .env()
            .locked()
            .into_iter()
            .map(String::from)
            .collect(),
        status: backup::status(&state.db).await?,
        next_run: state
            .backup
            .next_due(&state.db)
            .await?
            .map(|t| format!("{t:.3}")),
        running: running > 0,
        url: settings.url,
        region: settings.region,
        access_key: settings.access_key,
        schedule: settings.schedule,
        keep_daily: settings.keep_daily,
        keep_weekly: settings.keep_weekly,
        keep_monthly: settings.keep_monthly,
    })
}

/// The stored row with the changes, and the settings then in effect.
fn merge(
    state: &AppState,
    mut stored: Settings,
    body: UpdateBackupSettings,
) -> Result<(Settings, Settings), HttpError> {
    let env = state.backup.env();
    let locked = env.locked();
    let check = |field: &str| {
        if locked.contains(&field) {
            Err(HttpError::bad_request(format!(
                "{field} is set by the {} environment variable",
                backup::variable(field)
            )))
        } else {
            Ok(())
        }
    };
    for (field, value, slot) in [
        ("url", body.url, &mut stored.url),
        ("region", body.region, &mut stored.region),
        ("access_key", body.access_key, &mut stored.access_key),
        ("secret_key", body.secret_key, &mut stored.secret_key),
        ("password", body.password, &mut stored.password),
        ("schedule", body.schedule, &mut stored.schedule),
    ] {
        if let Some(value) = value {
            check(field)?;
            // The password and the keys are taken as they are.
            *slot = match field {
                "password" | "secret_key" | "access_key" => value,
                _ => value.trim().to_string(),
            };
        }
    }
    for (field, value, slot) in [
        ("keep_daily", body.keep_daily, &mut stored.keep_daily),
        ("keep_weekly", body.keep_weekly, &mut stored.keep_weekly),
        ("keep_monthly", body.keep_monthly, &mut stored.keep_monthly),
    ] {
        if let Some(value) = value {
            check(field)?;
            *slot = value;
        }
    }
    let settings = env.apply(&stored);
    settings.validate().map_err(HttpError::bad_request)?;
    Ok((stored, settings))
}

#[utoipa::path(
    get,
    path = "/backup",
    tag = "Superadmin",
    operation_id = "getBackupSettings",
    summary = "The backups",
    responses(
        (status = 200, description = "The settings and the status", body = BackupSettings),
        (status = 401, description = "Not signed in", body = ErrorBody),
        (status = 403, description = "Superadmin only, with a session (never a token) whose second factor was confirmed within 15 minutes: else `code` is `step_up_required` (confirm it, `/auth/step-up/…`) or `second_factor_required` (add one first)", body = ErrorBody),
    ),
    security(("session" = [])),
)]
async fn get_settings(
    State(state): State<AppState>,
    _: Superadmin,
) -> Result<Json<BackupSettings>, HttpError> {
    Ok(Json(view(&state).await?))
}

#[utoipa::path(
    put,
    path = "/backup",
    tag = "Superadmin",
    operation_id = "updateBackupSettings",
    summary = "Change the backups",
    description = "Takes effect at once. The settings are checked, not the repository: test them first. A field set by an environment variable cannot be changed (400).",
    request_body(content = UpdateBackupSettings, content_type = "application/json"),
    responses(
        (status = 200, description = "The settings and the status", body = BackupSettings),
        (status = 400, description = "Settings that cannot be, or a locked field", body = ErrorBody),
        (status = 401, description = "Not signed in", body = ErrorBody),
        (status = 403, description = "Superadmin only, with a session (never a token) whose second factor was confirmed within 15 minutes: else `code` is `step_up_required` (confirm it, `/auth/step-up/…`) or `second_factor_required` (add one first)", body = ErrorBody),
    ),
    security(("session" = [])),
)]
async fn update_settings(
    State(state): State<AppState>,
    _: Superadmin,
    ApiJson(body): ApiJson<UpdateBackupSettings>,
) -> Result<Json<BackupSettings>, HttpError> {
    let _saving = state.backup.lock_saves().await;
    let (stored, settings) = merge(&state, backup::stored(&state.db).await?, body)?;
    crate::server_settings::save(state.db.writer(), "backup", &stored).await?;
    state.backup.replace(settings);
    Ok(Json(view(&state).await?))
}

#[utoipa::path(
    post,
    path = "/backup/test",
    tag = "Superadmin",
    operation_id = "testBackupSettings",
    summary = "Test the backup settings",
    description = "The settings in effect with these changes (nothing is saved): the repository must be reachable, and open with the password if it exists. One that does not exist yet is made by the first backup.",
    request_body(content = UpdateBackupSettings, content_type = "application/json"),
    responses(
        (status = 200, description = "Reachable", body = TestResult),
        (status = 400, description = "Not reachable, a wrong password, or settings that cannot be", body = ErrorBody),
        (status = 401, description = "Not signed in", body = ErrorBody),
        (status = 403, description = "Superadmin only, with a session (never a token) whose second factor was confirmed within 15 minutes: else `code` is `step_up_required` (confirm it, `/auth/step-up/…`) or `second_factor_required` (add one first)", body = ErrorBody),
    ),
    security(("session" = [])),
)]
async fn test_settings(
    State(state): State<AppState>,
    _: Superadmin,
    ApiJson(body): ApiJson<UpdateBackupSettings>,
) -> Result<Json<TestResult>, HttpError> {
    let (_, settings) = merge(&state, backup::stored(&state.db).await?, body)?;
    if !settings.enabled() {
        return Err(HttpError::bad_request("Set a repository and a password"));
    }
    run::test(settings, state.files.data_dir().to_path_buf())
        .await
        .map(Json)
        .map_err(|e| HttpError::bad_request(format!("{e:#}")))
}

/// Whether a backup was queued.
#[derive(Serialize, ToSchema)]
#[schema(as = BackupQueued)]
pub struct Queued {
    /// False when one was queued or running already.
    pub queued: bool,
}

#[utoipa::path(
    post,
    path = "/backup/run",
    tag = "Superadmin",
    operation_id = "runBackup",
    summary = "Back up now",
    description = "Queued, and run in the background: the status says how it went.",
    responses(
        (status = 202, description = "Queued", body = Queued),
        (status = 400, description = "Backups are not set up", body = ErrorBody),
        (status = 401, description = "Not signed in", body = ErrorBody),
        (status = 403, description = "Superadmin only, with a session (never a token) whose second factor was confirmed within 15 minutes: else `code` is `step_up_required` (confirm it, `/auth/step-up/…`) or `second_factor_required` (add one first)", body = ErrorBody),
    ),
    security(("session" = [])),
)]
async fn run_now(
    State(state): State<AppState>,
    _: Superadmin,
) -> Result<(StatusCode, Json<Queued>), HttpError> {
    if !state.backup.settings().enabled() {
        return Err(HttpError::bad_request("Backups are not set up"));
    }
    let queued = backup::queue(&state).await?;
    Ok((StatusCode::ACCEPTED, Json(Queued { queued })))
}

fn settings_for_users(state: &AppState) -> Result<Settings, HttpError> {
    let settings = state.backup.settings();
    if !settings.enabled() {
        return Err(HttpError::not_found());
    }
    Ok(settings)
}

#[utoipa::path(
    get,
    path = "",
    tag = "Backups",
    operation_id = "listSnapshots",
    summary = "The backups",
    description = "The snapshots of the server's backups, the newest first. 404 when there are no backups.",
    responses(
        (status = 200, description = "The snapshots", body = Vec<Snapshot>),
        (status = 401, description = "Not signed in", body = ErrorBody),
        (status = 404, description = "No backups", body = ErrorBody),
    ),
    security(("session" = []), ("api_key" = [])),
)]
async fn list_snapshots(
    State(state): State<AppState>,
    _user: AuthUser,
) -> Result<Json<Vec<Snapshot>>, HttpError> {
    let settings = settings_for_users(&state)?;
    run::snapshots(settings, state.files.data_dir().to_path_buf())
        .await
        .map(Json)
        .map_err(|e| HttpError::internal(format!("{e:#}")))
}

#[derive(Deserialize, IntoParams)]
#[into_params(parameter_in = Query)]
pub struct SnapshotPath {
    /// A folder of your files, from their root: `Taxes/2025`; empty for
    /// the root.
    #[serde(default)]
    pub path: String,
}

#[utoipa::path(
    get,
    path = "/{snapshot}/files",
    tag = "Backups",
    operation_id = "listSnapshotFiles",
    summary = "A folder of your files in a backup",
    params(("snapshot" = String, Path, description = "The snapshot id"), SnapshotPath),
    responses(
        (status = 200, description = "What was in the folder, folders first", body = Vec<SnapshotEntry>),
        (status = 400, description = "Not a folder, or not in that snapshot", body = ErrorBody),
        (status = 401, description = "Not signed in", body = ErrorBody),
        (status = 404, description = "No backups", body = ErrorBody),
    ),
    security(("session" = []), ("api_key" = [])),
)]
async fn list_snapshot_files(
    State(state): State<AppState>,
    user: AuthUser,
    Path(snapshot): Path<String>,
    Query(params): Query<SnapshotPath>,
) -> Result<Json<Vec<SnapshotEntry>>, HttpError> {
    let settings = settings_for_users(&state)?;
    run::list(
        settings,
        state.files.data_dir().to_path_buf(),
        snapshot,
        user.0.id,
        params.path,
    )
    .await
    .map(Json)
    .map_err(|e| HttpError::bad_request(format!("{e:#}")))
}

/// What to restore.
#[derive(Deserialize, ToSchema)]
pub struct RestoreRequest {
    /// A file or folder of your files, from their root: `Taxes/Return.pdf`.
    pub path: String,
}

#[utoipa::path(
    post,
    path = "/{snapshot}/restore",
    tag = "Backups",
    operation_id = "restoreFromSnapshot",
    summary = "Restore a file or folder from a backup",
    description = "Next to what is there now, never over it: into the folder it was in (the root when that is gone), named with the backup's date, e.g. `Return (2026-11-05).pdf`.",
    params(("snapshot" = String, Path, description = "The snapshot id")),
    request_body(content = RestoreRequest, content_type = "application/json"),
    responses(
        (status = 200, description = "Restored", body = Node),
        (status = 400, description = "Not in that snapshot", body = ErrorBody),
        (status = 401, description = "Not signed in", body = ErrorBody),
        (status = 403, description = "A read-only API key", body = ErrorBody),
        (status = 404, description = "No backups", body = ErrorBody),
    ),
    security(("session" = []), ("api_key" = [])),
)]
async fn restore_from_snapshot(
    State(state): State<AppState>,
    user: AuthUser,
    Path(snapshot): Path<String>,
    ApiJson(body): ApiJson<RestoreRequest>,
) -> Result<Json<Node>, HttpError> {
    let settings = settings_for_users(&state)?;
    let user_id = user.0.id;
    let inner = run::inner_path(&body.path).map_err(|e| HttpError::bad_request(e.to_string()))?;
    let name = inner
        .file_name()
        .ok_or_else(|| HttpError::bad_request("Choose a file or folder"))?
        .to_string_lossy()
        .into_owned();
    std::fs::create_dir_all(state.files.uploads_dir())
        .map_err(|e| HttpError::internal(e.to_string()))?;
    let dest = state
        .files
        .uploads_dir()
        .join(format!("restore-{}", uuid::Uuid::now_v7()));
    let restored = run::restore_path(
        settings,
        state.files.data_dir().to_path_buf(),
        snapshot,
        user_id,
        body.path.clone(),
        dest.clone(),
    )
    .await;
    let (folder, time) = match restored {
        Ok(done) => done,
        Err(e) => {
            remove(&dest);
            return Err(HttpError::bad_request(format!("{e:#}")));
        }
    };
    let placed = place(&state, user_id, &inner, &name, folder, time, &dest).await;
    remove(&dest);
    let placed = placed?;
    state.files.mark_created(placed.id, user_id).await?;
    Ok(Json(
        crate::routes::files::seen(&state, user_id, placed).await?,
    ))
}

fn remove(path: &std::path::Path) {
    let _ = if path.is_dir() {
        std::fs::remove_dir_all(path)
    } else {
        std::fs::remove_file(path)
    };
}

/// Put what was restored into `dest` next to where it was.
async fn place(
    state: &AppState,
    user_id: uuid::Uuid,
    inner: &std::path::Path,
    name: &str,
    folder: bool,
    time: jiff::Timestamp,
    dest: &std::path::Path,
) -> Result<Node, HttpError> {
    // A restore adds to what the user takes: within their storage limit.
    state
        .files
        .check_room(user_id, crate::files::quota::size_on_disk(dest))
        .await?;
    let parent = match inner.parent().filter(|p| p.components().next().is_some()) {
        Some(parent) => match state.files.resolve(user_id, parent).await {
            Ok(node) if node.kind == crate::files::Kind::Folder => node,
            _ => state.files.root_node(user_id).await?,
        },
        None => state.files.root_node(user_id).await?,
    };
    let date = time.strftime("%Y-%m-%d").to_string();
    let mut last = None;
    for n in 1..=20 {
        let label = if n == 1 {
            date.clone()
        } else {
            format!("{date} {n}")
        };
        let candidate = dated(name, &label, folder);
        let result = if folder {
            state
                .files
                .import_dir(user_id, parent.id, &candidate, dest)
                .await
        } else {
            state
                .files
                .place_file(
                    user_id,
                    WriteTarget::New {
                        parent: parent.id,
                        name: candidate,
                    },
                    dest,
                )
                .await
        };
        match result {
            Err(crate::files::FileError::Conflict(e)) => last = Some(e),
            other => return Ok(other?),
        }
    }
    Err(HttpError::new(
        StatusCode::CONFLICT,
        last.unwrap_or_else(|| "The name is taken".into()),
    ))
}

/// `Return.pdf` restored from a backup of a day: `Return (2026-11-05).pdf`.
pub fn dated(name: &str, label: &str, folder: bool) -> String {
    match name
        .rsplit_once('.')
        .filter(|(stem, _)| !folder && !stem.is_empty())
    {
        Some((stem, ext)) => format!("{stem} ({label}).{ext}"),
        None => format!("{name} ({label})"),
    }
}

#[cfg(test)]
mod tests {
    use super::dated;

    #[test]
    fn restored_names_carry_the_date() {
        assert_eq!(
            dated("Return.pdf", "2026-11-05", false),
            "Return (2026-11-05).pdf"
        );
        assert_eq!(
            dated("archive.tar.gz", "2026-11-05", false),
            "archive.tar (2026-11-05).gz"
        );
        assert_eq!(dated(".env", "2026-11-05", false), ".env (2026-11-05)");
        assert_eq!(
            dated("Taxes.2025", "2026-11-05 2", true),
            "Taxes.2025 (2026-11-05 2)"
        );
    }
}
