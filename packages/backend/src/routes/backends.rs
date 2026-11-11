//! Storage backends (docs/M7C_DESIGN.md §2, §3, §6): the superadmin's
//! global ones (`/superadmin/backends`), users' own (`/backends`, when the
//! `storage.own_backends` setting allows it), where a user may make a
//! drive (`/backends/choices`), unlocking private credentials for a
//! session, and moving a drive to another backend (`/drives/{id}/move`,
//! `/superadmin/drives/{id}/move`). The operators, health and secrets are
//! `backends.rs`; a drive's bytes go through `storage.rs`.
//!
//! A secret is never sent back: only whether one is set. A connection is
//! tested before it is stored (an object written, read back and deleted
//! in the prefix), and again when it changes.

use crate::backends::{BACKEND_COLUMNS, BackendKind, BackendRow, Connection, DISK, MakeDrives};
use crate::error::{ErrorBody, HttpError};
use crate::extract::ApiJson;
use crate::files::FileError;
use crate::files::drives::{DriveKind, DriveRow};
use crate::files::roles::{self, Role};
use crate::routes::auth::{AuthUser, SessionUser, Superadmin};
use crate::session::AuthSession;
use crate::settings::{self, For};
use crate::state::AppState;
use axum::Json;
use axum::extract::{Path, Query, State};
use axum::http::StatusCode;
use serde::{Deserialize, Serialize};
use utoipa::{IntoParams, ToSchema};
use utoipa_axum::router::OpenApiRouter;
use utoipa_axum::routes;
use uuid::Uuid;

/// Nested under /backends.
pub fn router() -> OpenApiRouter<AppState> {
    OpenApiRouter::new()
        .routes(routes!(list_backends, add_backend))
        .routes(routes!(test_backend_settings))
        .routes(routes!(backend_choices))
        .routes(routes!(change_backend, remove_backend))
        .routes(routes!(test_backend))
        .routes(routes!(unlock_backend, lock_backend))
}

/// Nested under /superadmin.
pub fn superadmin_router() -> OpenApiRouter<AppState> {
    OpenApiRouter::new()
        .routes(routes!(list_global_backends, add_global_backend))
        .routes(routes!(test_global_backend_settings))
        .routes(routes!(change_global_backend, remove_global_backend))
        .routes(routes!(test_global_backend))
        .routes(routes!(move_shared_drive))
}

/// Nested under /drives.
pub fn drives_router() -> OpenApiRouter<AppState> {
    OpenApiRouter::new().routes(routes!(move_drive))
}

const SUPERADMIN_ONLY: &str = "Superadmin only, with a session (never a token) whose second factor was confirmed within 15 minutes: else `code` is `step_up_required` (confirm it, `/auth/step-up/…`) or `second_factor_required` (add one first)";

/// Whether a backend's drives can be used now.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, ToSchema)]
#[serde(rename_all = "lowercase")]
pub enum BackendStatus {
    /// It answers.
    Ok,
    /// It failed (unreachable, credentials refused): `error` says why.
    Failing,
    /// Private credentials that no session of its owner holds now.
    Locked,
}

/// Whose storage a backend is.
#[derive(Serialize, ToSchema)]
pub struct BackendOwner {
    pub id: Uuid,
    pub email: String,
}

/// A storage backend, as its manager sees it (the superadmin for the
/// server's disk and the global ones, a user for their own).
#[derive(Serialize, ToSchema)]
pub struct Backend {
    /// The server's disk is `00000000-0000-0000-0000-000000000000`.
    pub id: Uuid,
    pub kind: BackendKind,
    pub name: String,
    /// S3's endpoint, region, bucket and prefix (each drive is at
    /// `<prefix>/<drive id>/`); null for the server's disk.
    #[schema(required = true)]
    pub endpoint: Option<String>,
    #[schema(required = true)]
    pub region: Option<String>,
    #[schema(required = true)]
    pub bucket: Option<String>,
    #[schema(required = true)]
    pub prefix: Option<String>,
    #[schema(required = true)]
    pub access_key: Option<String>,
    /// A secret is stored; it is never sent back.
    pub secret_set: bool,
    /// Private credentials: the secret is sealed to the owner's encryption
    /// key, and the drives work only while a session of theirs unlocked it.
    pub private: bool,
    /// A private backend's secret, sealed to its owner's encryption key
    /// (docs/E2E_SPEC.md §11): the owner's client opens it in each new
    /// session and unlocks the backend with it. The server cannot read
    /// it; null for every other backend.
    #[schema(required = true)]
    pub sealed_secret: Option<String>,
    /// Bytes for all its drives together; null: no limit.
    #[schema(required = true)]
    pub limit: Option<i64>,
    /// The limit each new drive on it gets; null: none.
    #[schema(required = true)]
    pub drive_limit: Option<i64>,
    /// Bytes its drives take (files, trash, versions).
    pub used: i64,
    /// How many drives it holds.
    pub drives: i64,
    /// Who may make drives on it (a global backend).
    pub make_drives: MakeDrives,
    /// The teams whose managers may make drives on it (`make_drives`
    /// `teams`).
    pub team_ids: Vec<Uuid>,
    /// How often its drives are listed for changes made outside
    /// Organizer, in minutes; 0: never. The server's disk is watched.
    pub list_every_minutes: i64,
    /// A user's own storage: who; null for the disk and global backends.
    #[schema(required = true)]
    pub owner: Option<BackendOwner>,
    pub status: BackendStatus,
    /// Why it fails or is locked, fit to show; null when it answers.
    #[schema(required = true)]
    pub error: Option<String>,
    /// The server's backups cover its drives: the server's disk only.
    pub backed_up: bool,
    #[schema(required = true)]
    pub created_at: Option<String>,
}

/// Where a drive is stored, as its members see it.
#[derive(Clone, Serialize, ToSchema)]
pub struct BackendRef {
    pub id: Uuid,
    pub name: String,
    pub kind: BackendKind,
    /// A user's own storage: their email (the members are warned that the
    /// drive goes if they remove it); null otherwise.
    #[schema(required = true)]
    pub owner: Option<String>,
    /// The server's backups cover it: the server's disk only.
    pub backed_up: bool,
}

/// The server's disk, as a drive's place.
pub fn disk_ref() -> BackendRef {
    BackendRef {
        id: DISK,
        name: "The server's disk".into(),
        kind: BackendKind::Disk,
        owner: None,
        backed_up: true,
    }
}

/// A backend as a drive's place (None: the server's disk).
pub async fn backend_ref(state: &AppState, backend: Option<Uuid>) -> Result<BackendRef, HttpError> {
    let Some(backend) = backend else {
        return Ok(disk_ref());
    };
    let row = BackendRow::get(state.db.reader(), backend)
        .await?
        .ok_or_else(HttpError::not_found)?;
    Ok(BackendRef {
        id: row.id,
        name: row.name.clone(),
        kind: BackendKind::S3,
        owner: owner_of(state, &row).await?.map(|o| o.email),
        backed_up: false,
    })
}

async fn owner_of(state: &AppState, row: &BackendRow) -> Result<Option<BackendOwner>, HttpError> {
    let Some(owner) = row.owner_id else {
        return Ok(None);
    };
    let email: String = sqlx::query_scalar("SELECT email FROM users WHERE id = ?1")
        .bind(owner)
        .fetch_one(state.db.reader())
        .await?;
    Ok(Some(BackendOwner { id: owner, email }))
}

async fn view(state: &AppState, row: &BackendRow) -> Result<Backend, HttpError> {
    let backends = state.files.backends();
    let locked = backends.locked(row).await?;
    let error = backends.unavailable(row).await?;
    let drives: i64 = sqlx::query_scalar("SELECT count(*) FROM drives WHERE backend_id = ?1")
        .bind(row.id)
        .fetch_one(state.db.reader())
        .await?;
    let team_ids: Vec<Uuid> =
        sqlx::query_scalar("SELECT team_id FROM backend_teams WHERE backend_id = ?1")
            .bind(row.id)
            .fetch_all(state.db.reader())
            .await?;
    Ok(Backend {
        id: row.id,
        kind: BackendKind::S3,
        name: row.name.clone(),
        endpoint: Some(row.endpoint.clone()),
        region: Some(row.region.clone()),
        bucket: Some(row.bucket.clone()),
        prefix: Some(row.prefix.clone()),
        access_key: Some(row.access_key.clone()),
        secret_set: true,
        private: row.private(),
        sealed_secret: if row.private() {
            row.sealed_secret.clone()
        } else {
            None
        },
        limit: row.storage_limit,
        drive_limit: row.drive_limit,
        used: state.files.backend_usage(row.id).await?.used,
        drives,
        make_drives: row.make_drives,
        team_ids,
        list_every_minutes: row.list_every.map_or(0, |s| s / 60),
        owner: owner_of(state, row).await?,
        status: if locked {
            BackendStatus::Locked
        } else if error.is_some() {
            BackendStatus::Failing
        } else {
            BackendStatus::Ok
        },
        error,
        backed_up: false,
        created_at: Some(row.created_at.clone()),
    })
}

/// The server's disk, as the superadmin's list shows it.
async fn disk_view(state: &AppState) -> Result<Backend, HttpError> {
    let drives: i64 = sqlx::query_scalar("SELECT count(*) FROM drives WHERE backend_id IS NULL")
        .fetch_one(state.db.reader())
        .await?;
    Ok(Backend {
        id: DISK,
        kind: BackendKind::Disk,
        name: disk_ref().name,
        endpoint: None,
        region: None,
        bucket: None,
        prefix: None,
        access_key: None,
        secret_set: false,
        private: false,
        sealed_secret: None,
        limit: None,
        drive_limit: None,
        used: state.files.disk_usage().await?,
        drives,
        make_drives: MakeDrives::Everyone,
        team_ids: Vec::new(),
        list_every_minutes: 0,
        owner: None,
        status: BackendStatus::Ok,
        error: None,
        backed_up: true,
        created_at: None,
    })
}

/// A backend's row: a global one (`owner` None) or a user's own.
async fn row_of(state: &AppState, id: Uuid, owner: Option<Uuid>) -> Result<BackendRow, HttpError> {
    BackendRow::get(state.db.reader(), id)
        .await?
        .filter(|row| row.owner_id == owner)
        .ok_or_else(HttpError::not_found)
}

/// A new S3-compatible backend.
#[derive(Deserialize, ToSchema)]
pub struct NewBackend {
    /// 1 to 100 characters, unique among the global backends (or the
    /// user's own).
    pub name: String,
    /// The S3 endpoint, e.g. `https://s3.eu-central-1.amazonaws.com`. A
    /// user's own must be https at a public address (unless the server
    /// sets `BACKENDS_ALLOW_PRIVATE`).
    pub endpoint: String,
    /// Left out: `us-east-1` (what MinIO, Garage and R2 take).
    #[serde(default)]
    pub region: String,
    pub bucket: String,
    /// A folder in the bucket; left out: its top.
    #[serde(default)]
    pub prefix: String,
    pub access_key: String,
    /// The secret access key: tested, then kept encrypted with the server
    /// key and never sent back. With private credentials it is only kept
    /// in memory for this session (and used for the test).
    pub secret: String,
    /// Private credentials (a user's own backend only): `sealed_secret` is
    /// stored instead, and the drives work only while a session of the
    /// user has unlocked it (`PUT /backends/{id}/unlock`).
    #[serde(default)]
    pub private: bool,
    /// With `private`: the secret sealed to the user's encryption key by
    /// their device (base64, at most 4 KB), which the server cannot open.
    pub sealed_secret: Option<String>,
    /// Bytes for all its drives together; left out: no limit.
    pub limit: Option<i64>,
    /// The limit each new drive on it gets; left out: none.
    pub drive_limit: Option<i64>,
    /// Who may make drives on it (a global backend; left out: the
    /// superadmin only).
    pub make_drives: Option<MakeDrives>,
    /// With `make_drives` `teams`: whose managers.
    #[serde(default)]
    pub team_ids: Vec<Uuid>,
    /// How often its drives are listed for changes made outside
    /// Organizer, in minutes (at least 1); 0: never; left out: hourly.
    pub list_every_minutes: Option<i64>,
}

/// What to change of a backend; a field left out stays as it is.
#[derive(Deserialize, ToSchema)]
pub struct BackendChange {
    pub name: Option<String>,
    pub endpoint: Option<String>,
    pub region: Option<String>,
    /// Only while it holds no drive (409): its drives are where it is.
    pub bucket: Option<String>,
    /// Only while it holds no drive (409).
    pub prefix: Option<String>,
    pub access_key: Option<String>,
    /// A new secret (tested first). With private credentials, give
    /// `sealed_secret` with it.
    pub secret: Option<String>,
    pub sealed_secret: Option<String>,
    /// Bytes; null for no limit.
    #[serde(default, deserialize_with = "crate::util::present")]
    #[schema(value_type = Option<i64>, nullable)]
    pub limit: Option<Option<i64>>,
    /// Bytes; null for none.
    #[serde(default, deserialize_with = "crate::util::present")]
    #[schema(value_type = Option<i64>, nullable)]
    pub drive_limit: Option<Option<i64>>,
    pub make_drives: Option<MakeDrives>,
    pub team_ids: Option<Vec<Uuid>>,
    /// Minutes; 0: never.
    pub list_every_minutes: Option<i64>,
}

/// What a connection test found.
#[derive(Serialize, ToSchema)]
pub struct BackendTest {
    pub ok: bool,
    /// Why not, fit to show; null when it works.
    #[schema(required = true)]
    pub error: Option<String>,
}

fn clean_name(name: &str) -> Result<String, HttpError> {
    let name = name.trim();
    if name.is_empty() || name.chars().count() > 100 {
        return Err(HttpError::bad_request(
            "A storage's name is 1 to 100 characters",
        ));
    }
    Ok(name.to_string())
}

fn clean_prefix(prefix: &str) -> Result<String, HttpError> {
    let prefix = prefix.trim().trim_matches('/');
    if prefix
        .split('/')
        .any(|p| p == "." || p == ".." || (p.is_empty() && !prefix.is_empty()))
    {
        return Err(HttpError::bad_request("Not a usable prefix"));
    }
    Ok(prefix.to_string())
}

fn check_limit(limit: Option<i64>) -> Result<(), HttpError> {
    if limit.is_some_and(|l| l < 0) {
        return Err(HttpError::bad_request("A storage limit is 0 bytes or more"));
    }
    Ok(())
}

/// Minutes as the row's seconds (None: never).
fn list_every(minutes: i64) -> Result<Option<i64>, HttpError> {
    match minutes {
        0 => Ok(None),
        1..=525_600 => Ok(Some(minutes * 60)),
        _ => Err(HttpError::bad_request(
            "The listing is every 1 to 525,600 minutes, or 0 for never",
        )),
    }
}

/// A connection is tested; a failure is the request's (400), with why.
async fn tested(state: &AppState, conn: &Connection, own: bool) -> Result<(), HttpError> {
    state
        .files
        .backends()
        .test(conn, own)
        .await
        .map_err(HttpError::bad_request)
}

/// The hash of the request's session, for what it unlocks.
fn session_hash(auth: &AuthSession) -> Result<String, HttpError> {
    auth.session
        .id()
        .map(|id| crate::session::id_hash(&id))
        .ok_or_else(HttpError::unauthorized)
}

/// Whether the user may have their own storage (the `storage.own_backends`
/// setting, from the superadmin or a team of theirs).
async fn own_allowed(state: &AppState, user: Uuid) -> Result<bool, HttpError> {
    let env = settings::Env::of_state(state);
    Ok(
        settings::resolve(&state.db, &env, "storage.own_backends", For::user(user))
            .await?
            .bool(),
    )
}

pub(crate) async fn require_own_allowed(state: &AppState, user: Uuid) -> Result<(), HttpError> {
    if !own_allowed(state, user).await? {
        return Err(HttpError::new(
            StatusCode::FORBIDDEN,
            "Adding your own storage is not allowed here",
        ));
    }
    Ok(())
}

/// Add a backend: tested, stored, and (private credentials) unlocked for
/// this session.
async fn add(
    state: &AppState,
    body: NewBackend,
    owner: Option<Uuid>,
    session: Option<String>,
) -> Result<BackendRow, HttpError> {
    let name = clean_name(&body.name)?;
    let prefix = clean_prefix(&body.prefix)?;
    check_limit(body.limit)?;
    check_limit(body.drive_limit)?;
    if body.bucket.trim().is_empty() || body.access_key.trim().is_empty() || body.secret.is_empty()
    {
        return Err(HttpError::bad_request(
            "A bucket, an access key and a secret are needed",
        ));
    }
    let list_every = list_every(body.list_every_minutes.unwrap_or(60))?;
    let conn = Connection {
        endpoint: body.endpoint.trim().to_string(),
        region: body.region.trim().to_string(),
        bucket: body.bucket.trim().to_string(),
        prefix: prefix.clone(),
        access_key: body.access_key.trim().to_string(),
        secret: body.secret.clone(),
    };
    let (secret, sealed) = if body.private {
        let Some(user) = owner else {
            return Err(HttpError::bad_request(
                "Private credentials are for your own storage",
            ));
        };
        crate::routes::encryption::keys_of(state, user)
            .await?
            .ok_or_else(|| {
                HttpError::bad_request(
                    "Set up encryption first: private credentials are sealed to your key",
                )
            })?;
        let sealed = body
            .sealed_secret
            .as_deref()
            .map(str::trim)
            .filter(|s| !s.is_empty() && s.len() <= 4_096)
            .ok_or_else(|| {
                HttpError::bad_request("Private credentials need the sealed secret (at most 4 KB)")
            })?;
        (None, Some(sealed.to_string()))
    } else {
        (
            Some(
                state
                    .files
                    .backends()
                    .seal(&conn.secret)
                    .map_err(|e| HttpError::internal(e.to_string()))?,
            ),
            None,
        )
    };
    tested(state, &conn, owner.is_some()).await?;
    let id = Uuid::now_v7();
    let mut tx = state.db.writer().begin().await?;
    let made = sqlx::query(
        "INSERT INTO backends (id, owner_id, kind, name, endpoint, region, bucket, prefix,
           access_key, secret, sealed_secret, storage_limit, drive_limit, make_drives, list_every)
         VALUES (?1, ?2, 's3', ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?14)",
    )
    .bind(id)
    .bind(owner)
    .bind(&name)
    .bind(&conn.endpoint)
    .bind(&conn.region)
    .bind(&conn.bucket)
    .bind(&conn.prefix)
    .bind(&conn.access_key)
    .bind(&secret)
    .bind(&sealed)
    .bind(body.limit)
    .bind(body.drive_limit)
    .bind(match owner {
        Some(_) => MakeDrives::Superadmin,
        None => body.make_drives.unwrap_or(MakeDrives::Superadmin),
    })
    .bind(list_every)
    .execute(&mut *tx)
    .await;
    match made {
        Ok(_) => {}
        Err(sqlx::Error::Database(e)) if e.is_unique_violation() => {
            return Err(HttpError::conflict("A storage has that name already"));
        }
        Err(e) => return Err(e.into()),
    }
    if owner.is_none() {
        set_teams(&mut tx, id, &body.team_ids).await?;
    }
    tx.commit().await?;
    if sealed.is_some()
        && let Some(session) = session
    {
        state.files.backends().unlock(id, &session, conn.secret);
    }
    BackendRow::get(state.db.reader(), id)
        .await?
        .ok_or_else(HttpError::not_found)
}

async fn set_teams(
    tx: &mut sqlx::SqliteConnection,
    backend: Uuid,
    teams: &[Uuid],
) -> Result<(), HttpError> {
    sqlx::query("DELETE FROM backend_teams WHERE backend_id = ?1")
        .bind(backend)
        .execute(&mut *tx)
        .await?;
    for team in teams {
        let added = sqlx::query("INSERT INTO backend_teams (backend_id, team_id) VALUES (?1, ?2)")
            .bind(backend)
            .bind(team)
            .execute(&mut *tx)
            .await;
        match added {
            Ok(_) => {}
            Err(sqlx::Error::Database(e)) if e.is_foreign_key_violation() => {
                return Err(HttpError::bad_request("No such team"));
            }
            Err(e) => return Err(e.into()),
        }
    }
    Ok(())
}

/// Change a backend; a new connection is tested first. Its drives' trees
/// are opened anew with it.
async fn change(
    state: &AppState,
    row: BackendRow,
    body: BackendChange,
    session: Option<String>,
) -> Result<BackendRow, HttpError> {
    let own = row.owner_id.is_some();
    let drives: i64 = sqlx::query_scalar(
        "SELECT (SELECT count(*) FROM drives WHERE backend_id = ?1)
              + (SELECT count(*) FROM drive_moves WHERE to_backend = ?1)",
    )
    .bind(row.id)
    .fetch_one(state.db.reader())
    .await?;
    let moves_data = body
        .bucket
        .as_deref()
        .is_some_and(|b| b.trim() != row.bucket)
        || body
            .prefix
            .as_deref()
            .is_some_and(|p| clean_prefix(p).ok().as_deref() != Some(row.prefix.as_str()));
    if moves_data && drives > 0 {
        return Err(HttpError::conflict(
            "Its drives are in that bucket and prefix: move or delete them first",
        ));
    }
    let name = body.name.as_deref().map(clean_name).transpose()?;
    let prefix = body.prefix.as_deref().map(clean_prefix).transpose()?;
    if let Some(limit) = body.limit {
        check_limit(limit)?;
    }
    if let Some(limit) = body.drive_limit {
        check_limit(limit)?;
    }
    let list_every = body.list_every_minutes.map(list_every).transpose()?;
    let connects = body.endpoint.is_some()
        || body.region.is_some()
        || moves_data
        || body.access_key.is_some()
        || body.secret.is_some();
    let backends = state.files.backends();
    let mut secret_blob = row.secret.clone();
    let mut sealed = row.sealed_secret.clone();
    if connects {
        let secret = match &body.secret {
            Some(secret) => secret.clone(),
            None => backends
                .secret_of(&row)
                .await?
                .ok_or_else(|| HttpError::bad_request("Unlock it first: its secret is locked"))?,
        };
        let conn = Connection {
            endpoint: body
                .endpoint
                .as_deref()
                .map_or(row.endpoint.clone(), |e| e.trim().to_string()),
            region: body
                .region
                .as_deref()
                .map_or(row.region.clone(), |r| r.trim().to_string()),
            bucket: body
                .bucket
                .as_deref()
                .map_or(row.bucket.clone(), |b| b.trim().to_string()),
            prefix: prefix.clone().unwrap_or(row.prefix.clone()),
            access_key: body
                .access_key
                .as_deref()
                .map_or(row.access_key.clone(), |a| a.trim().to_string()),
            secret: secret.clone(),
        };
        tested(state, &conn, own).await?;
        if body.secret.is_some() {
            if row.private() {
                sealed = Some(
                    body.sealed_secret
                        .as_deref()
                        .map(str::trim)
                        .filter(|s| !s.is_empty() && s.len() <= 4_096)
                        .ok_or_else(|| {
                            HttpError::bad_request("A new private secret needs its sealed secret")
                        })?
                        .to_string(),
                );
                if let Some(session) = &session {
                    backends.lock(row.id, None);
                    backends.unlock(row.id, session, secret);
                }
            } else {
                secret_blob = Some(
                    backends
                        .seal(&secret)
                        .map_err(|e| HttpError::internal(e.to_string()))?,
                );
            }
        }
    }
    let mut tx = state.db.writer().begin().await?;
    let changed = sqlx::query(
        "UPDATE backends SET name = coalesce(?2, name), endpoint = coalesce(?3, endpoint),
           region = coalesce(?4, region), bucket = coalesce(?5, bucket),
           prefix = coalesce(?6, prefix), access_key = coalesce(?7, access_key),
           secret = ?8, sealed_secret = ?9,
           storage_limit = CASE WHEN ?10 THEN ?11 ELSE storage_limit END,
           drive_limit = CASE WHEN ?12 THEN ?13 ELSE drive_limit END,
           make_drives = coalesce(?14, make_drives),
           list_every = CASE WHEN ?15 THEN ?16 ELSE list_every END
         WHERE id = ?1",
    )
    .bind(row.id)
    .bind(&name)
    .bind(body.endpoint.as_deref().map(str::trim))
    .bind(body.region.as_deref().map(str::trim))
    .bind(body.bucket.as_deref().map(str::trim))
    .bind(&prefix)
    .bind(body.access_key.as_deref().map(str::trim))
    .bind(&secret_blob)
    .bind(&sealed)
    .bind(body.limit.is_some())
    .bind(body.limit.flatten())
    .bind(body.drive_limit.is_some())
    .bind(body.drive_limit.flatten())
    .bind(if own { None } else { body.make_drives })
    .bind(list_every.is_some())
    .bind(list_every.flatten())
    .execute(&mut *tx)
    .await;
    match changed {
        Ok(_) => {}
        Err(sqlx::Error::Database(e)) if e.is_unique_violation() => {
            return Err(HttpError::conflict("A storage has that name already"));
        }
        Err(e) => return Err(e.into()),
    }
    if !own && let Some(teams) = &body.team_ids {
        set_teams(&mut tx, row.id, teams).await?;
    }
    tx.commit().await?;
    if connects || list_every.is_some() {
        if connects {
            backends.health(row.id).ok();
        }
        state.files.forget_backend(row.id).await;
    }
    BackendRow::get(state.db.reader(), row.id)
        .await?
        .ok_or_else(HttpError::not_found)
}

/// Test a stored backend's connection; its health follows.
async fn test_row(state: &AppState, row: &BackendRow) -> Result<BackendTest, HttpError> {
    let backends = state.files.backends();
    let Some(secret) = backends.secret_of(row).await? else {
        return Ok(BackendTest {
            ok: false,
            error: Some(backends.locked_reason(row).await?),
        });
    };
    let health = backends.health(row.id);
    match backends
        .test(&Connection::of(row, secret), row.owner_id.is_some())
        .await
    {
        Ok(()) => {
            health.ok();
            Ok(BackendTest {
                ok: true,
                error: None,
            })
        }
        Err(reason) => {
            health.fail(&reason);
            Ok(BackendTest {
                ok: false,
                error: Some(reason),
            })
        }
    }
}

/// Test settings not stored yet ("Test connection" before adding).
async fn test_settings(state: &AppState, body: &NewBackend, own: bool) -> BackendTest {
    let conn = match clean_prefix(&body.prefix) {
        Ok(prefix) => Connection {
            endpoint: body.endpoint.trim().to_string(),
            region: body.region.trim().to_string(),
            bucket: body.bucket.trim().to_string(),
            prefix,
            access_key: body.access_key.trim().to_string(),
            secret: body.secret.clone(),
        },
        Err(_) => {
            return BackendTest {
                ok: false,
                error: Some("Not a usable prefix".into()),
            };
        }
    };
    match state.files.backends().test(&conn, own).await {
        Ok(()) => BackendTest {
            ok: true,
            error: None,
        },
        Err(reason) => BackendTest {
            ok: false,
            error: Some(reason),
        },
    }
}

/// Remove a backend. A global one holding drives (or a drive moving to
/// it) is refused; a user's own takes its drives out of Organizer (the
/// bucket keeps their objects: it is the user's).
async fn remove(state: &AppState, row: &BackendRow) -> Result<(), HttpError> {
    let drives: Vec<Uuid> = sqlx::query_scalar("SELECT id FROM drives WHERE backend_id = ?1")
        .bind(row.id)
        .fetch_all(state.db.reader())
        .await?;
    let moving: i64 = sqlx::query_scalar(
        "SELECT count(*) FROM drive_moves WHERE to_backend = ?1 OR from_backend = ?1",
    )
    .bind(row.id)
    .fetch_one(state.db.reader())
    .await?;
    if moving > 0 {
        return Err(HttpError::conflict(
            "A drive is being moved to or from it: wait until that is done",
        ));
    }
    if row.owner_id.is_none() && !drives.is_empty() {
        return Err(HttpError::conflict(
            "It holds drives: move or delete them first",
        ));
    }
    for drive in drives {
        state.files.forget_drive(drive).await?;
    }
    sqlx::query("DELETE FROM backends WHERE id = ?1")
        .bind(row.id)
        .execute(state.db.writer())
        .await?;
    state.files.backends().lock(row.id, None);
    Ok(())
}

/// A removed user's own storage: its drives leave Organizer first.
pub(crate) async fn forget_own_drives(state: &AppState, user: Uuid) -> Result<(), HttpError> {
    let drives: Vec<Uuid> = sqlx::query_scalar(
        "SELECT d.id FROM drives d JOIN backends b ON b.id = d.backend_id WHERE b.owner_id = ?1",
    )
    .bind(user)
    .fetch_all(state.db.reader())
    .await?;
    for drive in drives {
        state.files.forget_drive(drive).await?;
    }
    Ok(())
}

/// Who asks for a drive on a backend.
#[derive(Clone, Copy, PartialEq, Eq)]
pub enum Maker {
    /// The superadmin, through their endpoints.
    Superadmin,
    /// A user making a shared drive of their own (`POST /drives`).
    User,
    /// A team's manager, for that team (`POST /teams/{id}/drives`).
    TeamManager(Uuid),
}

/// Whether a drive may be made on (or moved to) a backend (None: the
/// server's disk, which the drives' own rules cover): the superadmin on
/// any global one; a user's own by that user alone, as their shared
/// drive; a global one as its `make_drives` says.
pub async fn check_maker(
    state: &AppState,
    backend: Option<Uuid>,
    user: Uuid,
    maker: Maker,
) -> Result<(), HttpError> {
    let Some(backend) = backend.filter(|b| *b != DISK) else {
        return Ok(());
    };
    let row = BackendRow::get(state.db.reader(), backend)
        .await?
        .ok_or_else(|| HttpError::bad_request("No such storage"))?;
    let refused = || {
        Err(HttpError::new(
            StatusCode::FORBIDDEN,
            "You may not make drives on that storage",
        ))
    };
    match row.owner_id {
        Some(owner) => {
            if maker != Maker::User || owner != user {
                return refused();
            }
            require_own_allowed(state, user).await
        }
        None => match (maker, row.make_drives) {
            (Maker::Superadmin, _) => Ok(()),
            (_, MakeDrives::Everyone) => Ok(()),
            (Maker::TeamManager(_), MakeDrives::TeamManagers) => Ok(()),
            (Maker::TeamManager(team), MakeDrives::Teams) => {
                let listed: Option<i64> = sqlx::query_scalar(
                    "SELECT 1 FROM backend_teams WHERE backend_id = ?1 AND team_id = ?2",
                )
                .bind(backend)
                .bind(team)
                .fetch_optional(state.db.reader())
                .await?;
                if listed.is_some() { Ok(()) } else { refused() }
            }
            _ => refused(),
        },
    }
}

/// The backend a request names: the disk's id or none is the disk.
pub fn chosen(backend: Option<Uuid>) -> Option<Uuid> {
    backend.filter(|b| *b != DISK)
}

#[utoipa::path(
    get,
    path = "/backends",
    tag = "Superadmin",
    operation_id = "listGlobalBackends",
    summary = "The storage backends",
    description = "The server's disk first, then the global S3-compatible backends, by name.",
    responses(
        (status = 200, description = "The backends", body = Vec<Backend>),
        (status = 401, description = "Not signed in", body = ErrorBody),
        (status = 403, description = SUPERADMIN_ONLY, body = ErrorBody),
    ),
    security(("session" = [])),
)]
async fn list_global_backends(
    State(state): State<AppState>,
    _: Superadmin,
) -> Result<Json<Vec<Backend>>, HttpError> {
    let rows: Vec<BackendRow> = sqlx::query_as(sqlx::AssertSqlSafe(format!(
        "SELECT {BACKEND_COLUMNS} FROM backends WHERE owner_id IS NULL ORDER BY name COLLATE NOCASE"
    )))
    .fetch_all(state.db.reader())
    .await?;
    let mut out = vec![disk_view(&state).await?];
    for row in rows {
        out.push(view(&state, &row).await?);
    }
    Ok(Json(out))
}

#[utoipa::path(
    post,
    path = "/backends",
    tag = "Superadmin",
    operation_id = "addGlobalBackend",
    summary = "Add an S3-compatible backend",
    description = "Its connection is tested first (an object written, read back and deleted in the prefix). Its secret is kept encrypted with the server key and never sent back. Its endpoint may be anywhere, the server's own network too. Drives on it are not in the server's backups: the provider's versioning or replication is the way.",
    request_body(content = NewBackend, content_type = "application/json"),
    responses(
        (status = 201, description = "Added", body = Backend),
        (status = 400, description = "Invalid, or the connection failed (`error` says why)", body = ErrorBody),
        (status = 401, description = "Not signed in", body = ErrorBody),
        (status = 403, description = SUPERADMIN_ONLY, body = ErrorBody),
        (status = 409, description = "A global backend has that name", body = ErrorBody),
        (status = 415, description = "The body must be JSON (Content-Type: application/json)", body = ErrorBody),
    ),
    security(("session" = [])),
)]
async fn add_global_backend(
    State(state): State<AppState>,
    _: Superadmin,
    ApiJson(body): ApiJson<NewBackend>,
) -> Result<(StatusCode, Json<Backend>), HttpError> {
    if body.private {
        return Err(HttpError::bad_request(
            "Private credentials are for users' own storage",
        ));
    }
    let row = add(&state, body, None, None).await?;
    Ok((StatusCode::CREATED, Json(view(&state, &row).await?)))
}

#[utoipa::path(
    post,
    path = "/backends/test",
    tag = "Superadmin",
    operation_id = "testGlobalBackendSettings",
    summary = "Test an S3 connection before adding it",
    request_body(content = NewBackend, content_type = "application/json"),
    responses(
        (status = 200, description = "What the test found", body = BackendTest),
        (status = 401, description = "Not signed in", body = ErrorBody),
        (status = 403, description = SUPERADMIN_ONLY, body = ErrorBody),
        (status = 415, description = "The body must be JSON (Content-Type: application/json)", body = ErrorBody),
    ),
    security(("session" = [])),
)]
async fn test_global_backend_settings(
    State(state): State<AppState>,
    _: Superadmin,
    ApiJson(body): ApiJson<NewBackend>,
) -> Json<BackendTest> {
    Json(test_settings(&state, &body, false).await)
}

#[utoipa::path(
    patch,
    path = "/backends/{id}",
    tag = "Superadmin",
    operation_id = "changeGlobalBackend",
    summary = "Change a global backend",
    description = "A new connection (endpoint, region, credentials) is tested first. The bucket and prefix change only while it holds no drive.",
    params(("id" = Uuid, Path, description = "The backend's id")),
    request_body(content = BackendChange, content_type = "application/json"),
    responses(
        (status = 200, description = "Changed", body = Backend),
        (status = 400, description = "Invalid, or the connection failed", body = ErrorBody),
        (status = 401, description = "Not signed in", body = ErrorBody),
        (status = 403, description = SUPERADMIN_ONLY, body = ErrorBody),
        (status = 404, description = "No such global backend", body = ErrorBody),
        (status = 409, description = "The name is taken, or its drives keep its bucket and prefix", body = ErrorBody),
        (status = 415, description = "The body must be JSON (Content-Type: application/json)", body = ErrorBody),
    ),
    security(("session" = [])),
)]
async fn change_global_backend(
    State(state): State<AppState>,
    _: Superadmin,
    Path(id): Path<Uuid>,
    ApiJson(body): ApiJson<BackendChange>,
) -> Result<Json<Backend>, HttpError> {
    let row = row_of(&state, id, None).await?;
    let row = change(&state, row, body, None).await?;
    Ok(Json(view(&state, &row).await?))
}

#[utoipa::path(
    delete,
    path = "/backends/{id}",
    tag = "Superadmin",
    operation_id = "removeGlobalBackend",
    summary = "Remove a global backend",
    description = "Only once it holds no drive (move or delete them first). Nothing in the bucket is deleted.",
    params(("id" = Uuid, Path, description = "The backend's id")),
    responses(
        (status = 204, description = "Removed"),
        (status = 401, description = "Not signed in", body = ErrorBody),
        (status = 403, description = SUPERADMIN_ONLY, body = ErrorBody),
        (status = 404, description = "No such global backend", body = ErrorBody),
        (status = 409, description = "It holds drives, or a drive is moving", body = ErrorBody),
    ),
    security(("session" = [])),
)]
async fn remove_global_backend(
    State(state): State<AppState>,
    _: Superadmin,
    Path(id): Path<Uuid>,
) -> Result<StatusCode, HttpError> {
    let row = row_of(&state, id, None).await?;
    remove(&state, &row).await?;
    Ok(StatusCode::NO_CONTENT)
}

#[utoipa::path(
    post,
    path = "/backends/{id}/test",
    tag = "Superadmin",
    operation_id = "testGlobalBackend",
    summary = "Test a global backend's connection",
    description = "Its status follows: a failure marks its drives unavailable with the reason, a success makes them available again.",
    params(("id" = Uuid, Path, description = "The backend's id")),
    responses(
        (status = 200, description = "What the test found", body = BackendTest),
        (status = 401, description = "Not signed in", body = ErrorBody),
        (status = 403, description = SUPERADMIN_ONLY, body = ErrorBody),
        (status = 404, description = "No such global backend", body = ErrorBody),
    ),
    security(("session" = [])),
)]
async fn test_global_backend(
    State(state): State<AppState>,
    _: Superadmin,
    Path(id): Path<Uuid>,
) -> Result<Json<BackendTest>, HttpError> {
    let row = row_of(&state, id, None).await?;
    Ok(Json(test_row(&state, &row).await?))
}

#[utoipa::path(
    get,
    path = "",
    tag = "Storage",
    operation_id = "listBackends",
    summary = "Your own storage",
    description = "The S3-compatible backends the user added, by name. Drives on them are never in the server's backups.",
    responses(
        (status = 200, description = "Their backends", body = Vec<Backend>),
        (status = 401, description = "Not signed in", body = ErrorBody),
    ),
    security(("session" = []), ("api_key" = [])),
)]
async fn list_backends(
    State(state): State<AppState>,
    user: AuthUser,
) -> Result<Json<Vec<Backend>>, HttpError> {
    let rows: Vec<BackendRow> = sqlx::query_as(sqlx::AssertSqlSafe(format!(
        "SELECT {BACKEND_COLUMNS} FROM backends WHERE owner_id = ?1 ORDER BY name COLLATE NOCASE"
    )))
    .bind(user.0.id)
    .fetch_all(state.db.reader())
    .await?;
    let mut out = Vec::with_capacity(rows.len());
    for row in rows {
        out.push(view(&state, &row).await?);
    }
    Ok(Json(out))
}

#[utoipa::path(
    post,
    path = "",
    tag = "Storage",
    operation_id = "addBackend",
    summary = "Add your own S3-compatible storage",
    description = "When the setting `storage.own_backends` allows it. The endpoint must be https at a public address (unless the server sets `BACKENDS_ALLOW_PRIVATE`); the connection is tested first. The secret is kept encrypted with the server key, or, with `private`, only the sealed secret is kept and this session holds the secret (it is unlocked for it).",
    request_body(content = NewBackend, content_type = "application/json"),
    responses(
        (status = 201, description = "Added", body = Backend),
        (status = 400, description = "Invalid, an address that is not allowed, or the connection failed", body = ErrorBody),
        (status = 401, description = "Not signed in", body = ErrorBody),
        (status = 403, description = "Not allowed by the setting, or not with an API key", body = ErrorBody),
        (status = 409, description = "One of your backends has that name", body = ErrorBody),
        (status = 415, description = "The body must be JSON (Content-Type: application/json)", body = ErrorBody),
    ),
    security(("session" = [])),
)]
async fn add_backend(
    State(state): State<AppState>,
    user: SessionUser,
    auth: AuthSession,
    ApiJson(body): ApiJson<NewBackend>,
) -> Result<(StatusCode, Json<Backend>), HttpError> {
    require_own_allowed(&state, user.0.id).await?;
    let session = session_hash(&auth)?;
    let row = add(&state, body, Some(user.0.id), Some(session)).await?;
    Ok((StatusCode::CREATED, Json(view(&state, &row).await?)))
}

#[utoipa::path(
    post,
    path = "/test",
    tag = "Storage",
    operation_id = "testBackendSettings",
    summary = "Test your own S3 connection before adding it",
    request_body(content = NewBackend, content_type = "application/json"),
    responses(
        (status = 200, description = "What the test found", body = BackendTest),
        (status = 401, description = "Not signed in", body = ErrorBody),
        (status = 403, description = "Not allowed by the setting, or not with an API key", body = ErrorBody),
        (status = 415, description = "The body must be JSON (Content-Type: application/json)", body = ErrorBody),
    ),
    security(("session" = [])),
)]
async fn test_backend_settings(
    State(state): State<AppState>,
    user: SessionUser,
    ApiJson(body): ApiJson<NewBackend>,
) -> Result<Json<BackendTest>, HttpError> {
    require_own_allowed(&state, user.0.id).await?;
    Ok(Json(test_settings(&state, &body, true).await))
}

/// A place a new drive may be stored.
#[derive(Serialize, ToSchema)]
pub struct BackendChoice {
    #[serde(flatten)]
    pub backend: BackendRef,
    /// Its drives cannot be used now (why: `error`).
    pub available: bool,
    #[schema(required = true)]
    pub error: Option<String>,
}

/// For whom the choices are.
#[derive(Deserialize, IntoParams)]
#[into_params(parameter_in = Query)]
pub struct ChoicesParams {
    /// A team's new drive (its managers): the backends they may use for
    /// it. Left out: the user's own new shared drive.
    pub team_id: Option<Uuid>,
}

#[utoipa::path(
    get,
    path = "/choices",
    tag = "Storage",
    operation_id = "backendChoices",
    summary = "Where a new drive may be stored",
    description = "The server's disk first, then the global backends the user may make drives on (as each one's `make_drives` says), then their own storage (for their own shared drives, not a team's). Whether they may make a drive at all is the drive endpoints' rules (`drives.create`).",
    params(ChoicesParams),
    responses(
        (status = 200, description = "The places", body = Vec<BackendChoice>),
        (status = 401, description = "Not signed in", body = ErrorBody),
    ),
    security(("session" = []), ("api_key" = [])),
)]
async fn backend_choices(
    State(state): State<AppState>,
    user: AuthUser,
    Query(params): Query<ChoicesParams>,
) -> Result<Json<Vec<BackendChoice>>, HttpError> {
    let maker = match params.team_id {
        Some(team) => Maker::TeamManager(team),
        None => Maker::User,
    };
    let rows: Vec<BackendRow> = sqlx::query_as(sqlx::AssertSqlSafe(format!(
        "SELECT {BACKEND_COLUMNS} FROM backends WHERE owner_id IS NULL OR owner_id = ?1
         ORDER BY owner_id IS NOT NULL, name COLLATE NOCASE"
    )))
    .bind(user.0.id)
    .fetch_all(state.db.reader())
    .await?;
    let mut out = vec![BackendChoice {
        backend: disk_ref(),
        available: true,
        error: None,
    }];
    for row in rows {
        if check_maker(&state, Some(row.id), user.0.id, maker)
            .await
            .is_err()
        {
            continue;
        }
        let error = state.files.backends().unavailable(&row).await?;
        out.push(BackendChoice {
            backend: backend_ref(&state, Some(row.id)).await?,
            available: error.is_none(),
            error,
        });
    }
    Ok(Json(out))
}

#[utoipa::path(
    patch,
    path = "/{id}",
    tag = "Storage",
    operation_id = "changeBackend",
    summary = "Change your own storage",
    description = "A new connection is tested first; the bucket and prefix change only while it holds no drive. A new private secret comes with its sealed secret, and is unlocked for this session.",
    params(("id" = Uuid, Path, description = "The backend's id")),
    request_body(content = BackendChange, content_type = "application/json"),
    responses(
        (status = 200, description = "Changed", body = Backend),
        (status = 400, description = "Invalid, or the connection failed", body = ErrorBody),
        (status = 401, description = "Not signed in", body = ErrorBody),
        (status = 403, description = "Not with an API key", body = ErrorBody),
        (status = 404, description = "Not one of yours", body = ErrorBody),
        (status = 409, description = "The name is taken, or its drives keep its bucket and prefix", body = ErrorBody),
        (status = 415, description = "The body must be JSON (Content-Type: application/json)", body = ErrorBody),
    ),
    security(("session" = [])),
)]
async fn change_backend(
    State(state): State<AppState>,
    user: SessionUser,
    auth: AuthSession,
    Path(id): Path<Uuid>,
    ApiJson(body): ApiJson<BackendChange>,
) -> Result<Json<Backend>, HttpError> {
    let row = row_of(&state, id, Some(user.0.id)).await?;
    let row = change(&state, row, body, Some(session_hash(&auth)?)).await?;
    Ok(Json(view(&state, &row).await?))
}

#[utoipa::path(
    delete,
    path = "/{id}",
    tag = "Storage",
    operation_id = "removeBackend",
    summary = "Remove your own storage",
    description = "Its drives go from Organizer with it, for every member; the objects in your bucket are left as they are.",
    params(("id" = Uuid, Path, description = "The backend's id")),
    responses(
        (status = 204, description = "Removed"),
        (status = 401, description = "Not signed in", body = ErrorBody),
        (status = 403, description = "Not with an API key", body = ErrorBody),
        (status = 404, description = "Not one of yours", body = ErrorBody),
        (status = 409, description = "A drive is being moved to or from it", body = ErrorBody),
    ),
    security(("session" = [])),
)]
async fn remove_backend(
    State(state): State<AppState>,
    user: SessionUser,
    Path(id): Path<Uuid>,
) -> Result<StatusCode, HttpError> {
    let row = row_of(&state, id, Some(user.0.id)).await?;
    remove(&state, &row).await?;
    Ok(StatusCode::NO_CONTENT)
}

#[utoipa::path(
    post,
    path = "/{id}/test",
    tag = "Storage",
    operation_id = "testBackend",
    summary = "Test your own storage's connection",
    params(("id" = Uuid, Path, description = "The backend's id")),
    responses(
        (status = 200, description = "What the test found", body = BackendTest),
        (status = 401, description = "Not signed in", body = ErrorBody),
        (status = 403, description = "Not with an API key", body = ErrorBody),
        (status = 404, description = "Not one of yours", body = ErrorBody),
    ),
    security(("session" = [])),
)]
async fn test_backend(
    State(state): State<AppState>,
    user: SessionUser,
    Path(id): Path<Uuid>,
) -> Result<Json<BackendTest>, HttpError> {
    let row = row_of(&state, id, Some(user.0.id)).await?;
    Ok(Json(test_row(&state, &row).await?))
}

/// The secret of private credentials, opened by the user's device.
#[derive(Deserialize, ToSchema)]
pub struct UnlockBackend {
    pub secret: String,
}

#[utoipa::path(
    put,
    path = "/{id}/unlock",
    tag = "Storage",
    operation_id = "unlockBackend",
    summary = "Unlock your storage's private credentials for this session",
    description = "The device opened the sealed secret with the user's encryption key and hands it over: it is tested, then kept in memory for as long as this session lives (never stored). Its drives are available meanwhile.",
    params(("id" = Uuid, Path, description = "The backend's id")),
    request_body(content = UnlockBackend, content_type = "application/json"),
    responses(
        (status = 200, description = "Unlocked", body = Backend),
        (status = 400, description = "Not private credentials, or the secret does not work", body = ErrorBody),
        (status = 401, description = "Not signed in", body = ErrorBody),
        (status = 403, description = "Not with an API key", body = ErrorBody),
        (status = 404, description = "Not one of yours", body = ErrorBody),
        (status = 415, description = "The body must be JSON (Content-Type: application/json)", body = ErrorBody),
        (status = 429, description = "Too many attempts, try again later", body = ErrorBody),
    ),
    security(("session" = [])),
)]
async fn unlock_backend(
    State(state): State<AppState>,
    user: SessionUser,
    auth: AuthSession,
    Path(id): Path<Uuid>,
    ApiJson(body): ApiJson<UnlockBackend>,
) -> Result<Json<Backend>, HttpError> {
    let row = row_of(&state, id, Some(user.0.id)).await?;
    if !row.private() {
        return Err(HttpError::bad_request(
            "Its secret is kept by the server: there is nothing to unlock",
        ));
    }
    let session = session_hash(&auth)?;
    // A secret tried: within the user's limits, as a code is.
    state.limits.user(user.0.id, None)?;
    tested(&state, &Connection::of(&row, body.secret.clone()), true).await?;
    let backends = state.files.backends();
    let was_locked = backends.locked(&row).await?;
    backends.unlock(row.id, &session, body.secret);
    backends.health(row.id).ok();
    if was_locked {
        // What a stop left in its drives' journal is settled now.
        if let Err(e) = crate::files::journal::recover(&state.files, Some(row.id)).await {
            tracing::warn!("cannot settle the journal of backend {}: {e}", row.id);
        }
    }
    Ok(Json(view(&state, &row).await?))
}

#[utoipa::path(
    delete,
    path = "/{id}/unlock",
    tag = "Storage",
    operation_id = "lockBackend",
    summary = "Lock your storage's private credentials again",
    description = "This session forgets the secret. Its drives stay available while another session of yours holds it.",
    params(("id" = Uuid, Path, description = "The backend's id")),
    responses(
        (status = 200, description = "Locked for this session", body = Backend),
        (status = 401, description = "Not signed in", body = ErrorBody),
        (status = 403, description = "Not with an API key", body = ErrorBody),
        (status = 404, description = "Not one of yours", body = ErrorBody),
    ),
    security(("session" = [])),
)]
async fn lock_backend(
    State(state): State<AppState>,
    user: SessionUser,
    auth: AuthSession,
    Path(id): Path<Uuid>,
) -> Result<Json<Backend>, HttpError> {
    let row = row_of(&state, id, Some(user.0.id)).await?;
    let backends = state.files.backends();
    backends.lock(row.id, Some(&session_hash(&auth)?));
    if backends.locked(&row).await? {
        state.files.forget_backend(row.id).await;
    }
    Ok(Json(view(&state, &row).await?))
}

/// Where to move a drive.
#[derive(Deserialize, ToSchema)]
pub struct MoveDrive {
    /// The backend; the server's disk is `00000000-0000-0000-0000-000000000000`.
    pub backend_id: Uuid,
}

#[utoipa::path(
    post,
    path = "/{id}/move",
    tag = "Drives",
    operation_id = "moveDrive",
    summary = "Move a shared drive to other storage",
    description = "By who manages it, to a backend they may make drives on. A job copies the drive (its files, trash and versions) and switches over when done; the drive is read-only meanwhile (writes: 409), and `Drive.moving_to` says where it goes.",
    params(("id" = String, Path, description = "The drive's id")),
    request_body(content = MoveDrive, content_type = "application/json"),
    responses(
        (status = 202, description = "Moving", body = crate::routes::drives::Drive),
        (status = 400, description = "My drive, there already, or no such storage", body = ErrorBody),
        (status = 401, description = "Not signed in", body = ErrorBody),
        (status = 403, description = "Not who manages it, not a storage they may use, or a read-only key", body = ErrorBody),
        (status = 404, description = "Not a drive of the user's", body = ErrorBody),
        (status = 409, description = "Being moved already", body = ErrorBody),
        (status = 503, description = "The drive's or the target's storage does not answer (`code` `storage_unavailable`)", body = ErrorBody),
    ),
    security(("session" = []), ("api_key" = [])),
)]
async fn move_drive(
    State(state): State<AppState>,
    user: AuthUser,
    Path(id): Path<String>,
    ApiJson(body): ApiJson<MoveDrive>,
) -> Result<(StatusCode, Json<crate::routes::drives::Drive>), HttpError> {
    let drive: Uuid = id.parse().map_err(|_| HttpError::not_found())?;
    let role = roles::drive_role(state.db.reader(), user.0.id, drive)
        .await?
        .ok_or_else(HttpError::not_found)?;
    let row = DriveRow::get(state.db.reader(), drive)
        .await?
        .ok_or_else(HttpError::not_found)?;
    if row.kind != DriveKind::Shared {
        return Err(HttpError::bad_request(
            "My drive stays on the server's disk",
        ));
    }
    if role < Role::Manager {
        return Err(HttpError::new(
            StatusCode::FORBIDDEN,
            "Only who manages this drive can do that",
        ));
    }
    let to = chosen(Some(body.backend_id));
    let maker = match team_of(&state, drive).await? {
        Some(team) => Maker::TeamManager(team),
        None => Maker::User,
    };
    check_maker(&state, to, user.0.id, maker).await?;
    state.files.move_drive(drive, to).await.map_err(moving)?;
    Ok((
        StatusCode::ACCEPTED,
        Json(crate::routes::drives::view_for(&state, drive, user.0.id).await?),
    ))
}

#[utoipa::path(
    post,
    path = "/drives/{id}/move",
    tag = "Superadmin",
    operation_id = "moveSharedDrive",
    summary = "Move a shared drive to other storage",
    description = "To the server's disk or any global backend. A job copies the drive and switches over when done; it is read-only meanwhile.",
    params(("id" = String, Path, description = "The drive's id")),
    request_body(content = MoveDrive, content_type = "application/json"),
    responses(
        (status = 202, description = "Moving", body = crate::routes::drives::SharedDrive),
        (status = 400, description = "There already, or no such storage", body = ErrorBody),
        (status = 401, description = "Not signed in", body = ErrorBody),
        (status = 403, description = SUPERADMIN_ONLY, body = ErrorBody),
        (status = 404, description = "No such shared drive", body = ErrorBody),
        (status = 409, description = "Being moved already", body = ErrorBody),
        (status = 503, description = "The drive's or the target's storage does not answer (`code` `storage_unavailable`)", body = ErrorBody),
    ),
    security(("session" = [])),
)]
async fn move_shared_drive(
    State(state): State<AppState>,
    me: Superadmin,
    Path(id): Path<String>,
    ApiJson(body): ApiJson<MoveDrive>,
) -> Result<(StatusCode, Json<crate::routes::drives::SharedDrive>), HttpError> {
    let drive: Uuid = id.parse().map_err(|_| HttpError::not_found())?;
    DriveRow::get(state.db.reader(), drive)
        .await?
        .filter(|d| d.kind == DriveKind::Shared)
        .ok_or_else(HttpError::not_found)?;
    let to = chosen(Some(body.backend_id));
    check_maker(&state, to, me.0.id, Maker::Superadmin).await?;
    state.files.move_drive(drive, to).await.map_err(moving)?;
    Ok((
        StatusCode::ACCEPTED,
        Json(crate::routes::drives::shared_view(&state, drive, me.0.id).await?),
    ))
}

/// A move's refusal: a target that is not a backend is the request's.
fn moving(error: FileError) -> HttpError {
    match error {
        FileError::NotFound => HttpError::bad_request("No such storage"),
        e => e.into(),
    }
}

async fn team_of(state: &AppState, drive: Uuid) -> Result<Option<Uuid>, HttpError> {
    Ok(
        sqlx::query_scalar("SELECT team_id FROM drives WHERE id = ?1")
            .bind(drive)
            .fetch_optional(state.db.reader())
            .await?
            .flatten(),
    )
}
