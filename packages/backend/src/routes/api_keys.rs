//! The signed-in user's tokens: API keys for scripts and devices for the
//! apps (`Authorization: Bearer <token>`), and app passwords for WebDAV
//! clients (the password of Basic authentication). One table and one kind
//! of token (`api_keys.kind`), as Nextcloud does. Managed with the session
//! only (a token cannot list, make or revoke tokens); the token is shown
//! once, on creation.

use crate::auth::{API_KEY_PREFIX, KeyKind, Scope, iso};
use crate::error::{ErrorBody, HttpError};
use crate::extract::ApiJson;
use crate::routes::auth::SessionUser;
use crate::state::AppState;
use crate::util::{random_hex, sha256_hex};
use axum::Json;
use axum::extract::{Path, State};
use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use serde::{Deserialize, Serialize};
use std::time::{Duration, SystemTime};
use utoipa::ToSchema;
use utoipa_axum::router::OpenApiRouter;
use utoipa_axum::routes;
use uuid::Uuid;

pub fn router() -> OpenApiRouter<AppState> {
    OpenApiRouter::new()
        .routes(routes!(list_keys, create_key))
        .routes(routes!(revoke_key))
}

/// The most keys one user may have, so a script gone wrong stops somewhere.
const MAX_KEYS: i64 = 50;

/// An API key, without its token.
#[derive(Serialize, ToSchema)]
pub struct ApiKey {
    pub id: Uuid,
    /// An API key, a device (made by an app's sign-in) or an app password.
    pub kind: KeyKind,
    pub name: String,
    /// The start of the token, to tell keys apart, e.g. `org_3f9a1c`.
    pub prefix: String,
    pub scope: Scope,
    pub created_at: String,
    /// Null until the key is used; updated at most once a minute.
    #[schema(required = true)]
    pub last_used_at: Option<String>,
    /// Null for a key that does not expire.
    #[schema(required = true)]
    pub expires_at: Option<String>,
}

#[derive(sqlx::FromRow)]
struct ApiKeyRow {
    id: Uuid,
    kind: String,
    name: String,
    prefix: String,
    scope: String,
    created_at: String,
    last_used_at: Option<String>,
    expires_at: Option<String>,
}

impl ApiKeyRow {
    fn into_key(self) -> ApiKey {
        ApiKey {
            id: self.id,
            // The table's CHECK allows nothing else.
            kind: KeyKind::parse(&self.kind).unwrap_or(KeyKind::Key),
            name: self.name,
            prefix: self.prefix,
            // The table's CHECK allows nothing else.
            scope: Scope::parse(&self.scope).unwrap_or(Scope::Read),
            created_at: self.created_at,
            last_used_at: self.last_used_at,
            expires_at: self.expires_at,
        }
    }
}

#[derive(Deserialize, ToSchema)]
pub struct CreateApiKey {
    /// What the key is for, 1 to 100 characters.
    pub name: String,
    /// `key` (the default) or `app_password`; devices are made by the
    /// apps' sign-in.
    #[serde(default = "default_kind")]
    pub kind: KeyKind,
    /// `read` or `write` (the old `admin` is taken as `write`).
    pub scope: Scope,
    /// 1 to 3650; left out for a key that does not expire.
    #[serde(default)]
    pub expires_in_days: Option<u32>,
}

fn default_kind() -> KeyKind {
    KeyKind::Key
}

/// A new key, with its token: shown this once.
#[derive(Serialize, ToSchema)]
pub struct CreatedApiKey {
    pub key: ApiKey,
    /// `org_` and 64 hex characters.
    pub token: String,
}

const COLUMNS: &str = "id, kind, name, prefix, scope, created_at, last_used_at, expires_at";

#[utoipa::path(
    get,
    path = "/api-keys",
    tag = "Account",
    operation_id = "listApiKeys",
    summary = "List your API keys",
    description = "Your own keys, newest first, expired ones included. Not with an API key.",
    responses(
        (status = 200, description = "The keys", body = Vec<ApiKey>),
        (status = 401, description = "Not signed in", body = ErrorBody),
        (status = 403, description = "Not with an API key", body = ErrorBody),
    ),
    security(("session" = [])),
)]
async fn list_keys(
    State(state): State<AppState>,
    SessionUser(user): SessionUser,
) -> Result<Json<Vec<ApiKey>>, HttpError> {
    let rows: Vec<ApiKeyRow> = sqlx::query_as(sqlx::AssertSqlSafe(format!(
        "SELECT {COLUMNS} FROM api_keys WHERE user_id = ?1 ORDER BY id DESC"
    )))
    .bind(user.id)
    .fetch_all(state.db.reader())
    .await?;
    Ok(Json(rows.into_iter().map(ApiKeyRow::into_key).collect()))
}

#[utoipa::path(
    post,
    path = "/api-keys",
    tag = "Account",
    operation_id = "createApiKey",
    summary = "Create an API key",
    description = "Answers the token, shown this once: send it as `Authorization: Bearer <token>`. A `read` key may only GET; a `write` key may also change things. No key reaches your account's settings or keys, nor the superadmin's endpoints (those take a session with a second factor confirmed lately); `admin`, the old scope, is taken as `write`. At most 50 keys per user.",
    request_body(content = CreateApiKey, content_type = "application/json"),
    responses(
        (status = 201, description = "Created", body = CreatedApiKey),
        (status = 400, description = "The request is invalid", body = ErrorBody),
        (status = 401, description = "Not signed in", body = ErrorBody),
        (status = 403, description = "Not with an API key", body = ErrorBody),
        (status = 409, description = "Too many keys", body = ErrorBody),
        (status = 415, description = "The body must be JSON (Content-Type: application/json)", body = ErrorBody),
    ),
    security(("session" = [])),
)]
async fn create_key(
    State(state): State<AppState>,
    SessionUser(user): SessionUser,
    ApiJson(body): ApiJson<CreateApiKey>,
) -> Result<Response, HttpError> {
    let name = body.name.trim();
    if name.is_empty() || name.chars().count() > 100 {
        return Err(HttpError::bad_request(
            "Expected a name of 1 to 100 characters",
        ));
    }
    if body.kind == KeyKind::Device {
        return Err(HttpError::bad_request(
            "Devices are made by the apps' sign-in, not here",
        ));
    }
    let expires_at = match body.expires_in_days {
        None => None,
        Some(days @ 1..=3650) => Some(iso(
            SystemTime::now() + Duration::from_secs(days as u64 * 86_400)
        )),
        Some(_) => {
            return Err(HttpError::bad_request(
                "Expected an expiry of 1 to 3650 days",
            ));
        }
    };
    let created = issue(
        &state,
        user.id,
        body.kind,
        name,
        body.scope,
        expires_at.as_deref(),
    )
    .await?;
    Ok((StatusCode::CREATED, Json(created)).into_response())
}

#[utoipa::path(
    delete,
    path = "/api-keys/{id}",
    tag = "Account",
    operation_id = "revokeApiKey",
    summary = "Revoke an API key",
    description = "It stops working at once. Not with an API key.",
    params(("id" = Uuid, Path, description = "The key id")),
    responses(
        (status = 204, description = "Revoked"),
        (status = 401, description = "Not signed in", body = ErrorBody),
        (status = 403, description = "Not with an API key", body = ErrorBody),
        (status = 404, description = "Not found", body = ErrorBody),
    ),
    security(("session" = [])),
)]
async fn revoke_key(
    State(state): State<AppState>,
    SessionUser(user): SessionUser,
    Path(id): Path<Uuid>,
) -> Result<Response, HttpError> {
    let deleted = sqlx::query("DELETE FROM api_keys WHERE id = ?1 AND user_id = ?2")
        .bind(id)
        .bind(user.id)
        .execute(state.db.writer())
        .await?
        .rows_affected();
    if deleted == 0 {
        return Err(HttpError::new(StatusCode::NOT_FOUND, "Not found"));
    }
    Ok(StatusCode::NO_CONTENT.into_response())
}

/// Make a token for a user: an API key, an app password, or a device's
/// (`routes/device.rs`). At most 50 per user.
pub(crate) async fn issue(
    state: &AppState,
    user_id: Uuid,
    kind: KeyKind,
    name: &str,
    scope: Scope,
    expires_at: Option<&str>,
) -> Result<CreatedApiKey, HttpError> {
    let token = format!("{API_KEY_PREFIX}{}", random_hex(32));
    let prefix = token[..API_KEY_PREFIX.len() + 6].to_string();

    let mut tx = state.db.writer().begin().await?;
    let count: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM api_keys WHERE user_id = ?1")
        .bind(user_id)
        .fetch_one(&mut *tx)
        .await?;
    if count >= MAX_KEYS {
        return Err(HttpError::new(
            StatusCode::CONFLICT,
            "Too many API keys: revoke one first",
        ));
    }
    let row: ApiKeyRow = sqlx::query_as(sqlx::AssertSqlSafe(format!(
        "INSERT INTO api_keys (id, user_id, kind, name, prefix, token_hash, scope, expires_at)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8) RETURNING {COLUMNS}"
    )))
    .bind(Uuid::now_v7())
    .bind(user_id)
    .bind(kind.as_str())
    .bind(name)
    .bind(&prefix)
    .bind(sha256_hex(&token))
    .bind(scope.as_str())
    .bind(expires_at)
    .fetch_one(&mut *tx)
    .await?;
    tx.commit().await?;
    Ok(CreatedApiKey {
        key: row.into_key(),
        token,
    })
}
