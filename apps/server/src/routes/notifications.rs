//! Settings → Notifications and the apps' push registration
//! (docs/M6D_DESIGN.md, `push.rs`): the user's switches, their devices,
//! and "what's new", which a device asks for once a content-free push
//! woke it. Devices register with their token (`write`), so these take
//! API keys too.

use crate::error::{ErrorBody, HttpError};
use crate::extract::ApiJson;
use crate::push::{self, DEVICE_COLUMNS, Device, DeviceKind, Notice, Settings};
use crate::routes::auth::AuthUser;
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

pub fn router() -> OpenApiRouter<AppState> {
    OpenApiRouter::new()
        .routes(routes!(get_notifications))
        .routes(routes!(update_notification_settings))
        .routes(routes!(add_push_subscription))
        .routes(routes!(remove_push_subscription))
        .routes(routes!(get_news))
}

/// A device that gets notices.
#[derive(Serialize, ToSchema)]
pub struct Subscription {
    pub id: Uuid,
    pub kind: DeviceKind,
    pub name: String,
    /// The push service's URL, or the device token: how a browser or an
    /// app finds its own row.
    pub endpoint: String,
    pub created_at: String,
    /// Null until it got a notice.
    #[schema(required = true)]
    pub last_notice_at: Option<String>,
    /// False for the iPhone app while APNs is not set up on the server:
    /// it is kept, but gets nothing yet.
    pub available: bool,
}

impl Subscription {
    fn of(device: Device, apns: bool) -> Subscription {
        Subscription {
            available: device.kind != DeviceKind::Apns || apns,
            id: device.id,
            kind: device.kind,
            name: device.name,
            endpoint: device.endpoint,
            created_at: device.created_at,
            last_notice_at: device.last_notice_at,
        }
    }
}

/// Everything Settings → Notifications shows.
#[derive(Serialize, ToSchema)]
pub struct Notifications {
    pub settings: Settings,
    /// The user's devices, newest first.
    pub subscriptions: Vec<Subscription>,
    /// The server's VAPID public key, base64url: a browser's
    /// `applicationServerKey`, UnifiedPush's VAPID key.
    pub vapid_public_key: String,
    /// Whether APNs is set up (`APNS_*`): without it the iPhone app gets
    /// no notices.
    pub apns: bool,
}

#[utoipa::path(
    get,
    path = "/notifications",
    tag = "Notifications",
    operation_id = "getNotifications",
    summary = "Your notification settings and devices",
    responses(
        (status = 200, description = "The switches, the devices and the server's push keys", body = Notifications),
        (status = 401, description = "Not signed in", body = ErrorBody),
    ),
    security(("session" = []), ("api_key" = [])),
)]
async fn get_notifications(
    State(state): State<AppState>,
    AuthUser(user, _): AuthUser,
) -> Result<Json<Notifications>, HttpError> {
    let devices: Vec<Device> = sqlx::query_as(sqlx::AssertSqlSafe(format!(
        "SELECT {DEVICE_COLUMNS} FROM push_subscriptions WHERE user_id = ?1 ORDER BY id DESC"
    )))
    .bind(user.id)
    .fetch_all(state.db.reader())
    .await?;
    let apns = state.push.apns();
    Ok(Json(Notifications {
        settings: push::settings_of(&state.db, user.id).await?,
        subscriptions: devices
            .into_iter()
            .map(|d| Subscription::of(d, apns))
            .collect(),
        vapid_public_key: state.push.vapid_public_key().to_string(),
        apns,
    }))
}

#[utoipa::path(
    put,
    path = "/notifications/settings",
    tag = "Notifications",
    operation_id = "updateNotificationSettings",
    summary = "Choose what you are notified about",
    description = "Replaces the three switches. `backup` matters for superadmins only.",
    request_body(content = Settings, content_type = "application/json"),
    responses(
        (status = 200, description = "The switches now in effect", body = Settings),
        (status = 401, description = "Not signed in", body = ErrorBody),
        (status = 403, description = "A read-only API key, or a switch a level above locked", body = ErrorBody),
        (status = 415, description = "The body must be JSON (Content-Type: application/json)", body = ErrorBody),
    ),
    security(("session" = []), ("api_key" = [])),
)]
async fn update_notification_settings(
    State(state): State<AppState>,
    AuthUser(user, _): AuthUser,
    ApiJson(body): ApiJson<Settings>,
) -> Result<Json<Settings>, HttpError> {
    push::save_settings(&state, user.id, body).await?;
    Ok(Json(push::settings_of(&state.db, user.id).await?))
}

/// A Web Push subscription's keys, base64url, as `PushSubscription.toJSON()`
/// gives them.
#[derive(Deserialize, ToSchema)]
pub struct SubscriptionKeys {
    /// The client's P-256 public key (uncompressed point).
    pub p256dh: String,
    /// The 16-byte auth secret.
    pub auth: String,
}

#[derive(Deserialize, ToSchema)]
pub struct NewSubscription {
    pub kind: DeviceKind,
    /// What the list shows, 1 to 100 characters: "Firefox on Linux".
    pub name: String,
    /// The push endpoint (https), or for `apns` the device token in hex.
    pub endpoint: String,
    /// Web Push only (`web`, `unifiedpush`).
    #[serde(default)]
    pub keys: Option<SubscriptionKeys>,
}

#[utoipa::path(
    post,
    path = "/notifications/subscriptions",
    tag = "Notifications",
    operation_id = "addPushSubscription",
    summary = "Get notices on this device",
    description = "A browser's or app's push endpoint (and for Web Push its keys). The same endpoint again replaces its row (a new key, a new name, or another user signed in on that device). At most 20 per user. What a device then gets is a wake-up with no content: it asks `GET /notifications/news` what is new.",
    request_body(content = NewSubscription, content_type = "application/json"),
    responses(
        (status = 201, description = "Registered", body = Subscription),
        (status = 400, description = "The endpoint or the keys are not valid", body = ErrorBody),
        (status = 401, description = "Not signed in", body = ErrorBody),
        (status = 403, description = "A read-only API key", body = ErrorBody),
        (status = 409, description = "Too many devices", body = ErrorBody),
        (status = 415, description = "The body must be JSON (Content-Type: application/json)", body = ErrorBody),
    ),
    security(("session" = []), ("api_key" = [])),
)]
async fn add_push_subscription(
    State(state): State<AppState>,
    AuthUser(user, _): AuthUser,
    ApiJson(body): ApiJson<NewSubscription>,
) -> Result<Response, HttpError> {
    let name = body.name.trim();
    if name.is_empty() || name.chars().count() > 100 {
        return Err(HttpError::bad_request(
            "Expected a name of 1 to 100 characters",
        ));
    }
    let endpoint = body.endpoint.trim().to_string();
    let (p256dh, auth) = match body.kind {
        DeviceKind::Apns => {
            if !(32..=200).contains(&endpoint.len())
                || !endpoint.chars().all(|c| c.is_ascii_hexdigit())
            {
                return Err(HttpError::bad_request("Expected the device token in hex"));
            }
            (None, None)
        }
        DeviceKind::Web | DeviceKind::Unifiedpush => {
            if endpoint.len() > 2000 || !state.push.endpoint_allowed(&endpoint) {
                return Err(HttpError::bad_request(
                    "Expected an https push endpoint on a public address",
                ));
            }
            let keys = body
                .keys
                .ok_or_else(|| HttpError::bad_request("Expected the subscription's keys"))?;
            let valid_key = push::from_b64url(&keys.p256dh)
                .is_some_and(|k| web_push_native::p256::PublicKey::from_sec1_bytes(&k).is_ok());
            let valid_auth = push::from_b64url(&keys.auth).is_some_and(|a| a.len() == 16);
            if !valid_key || !valid_auth {
                return Err(HttpError::bad_request(
                    "Expected a P-256 key and a 16-byte auth secret, base64url",
                ));
            }
            (Some(keys.p256dh), Some(keys.auth))
        }
    };
    let endpoint = if body.kind == DeviceKind::Apns {
        endpoint.to_lowercase()
    } else {
        endpoint
    };
    let mut tx = state.db.writer().begin().await?;
    let (count,): (i64,) = sqlx::query_as(
        "SELECT count(*) FROM push_subscriptions WHERE user_id = ?1 AND endpoint != ?2",
    )
    .bind(user.id)
    .bind(&endpoint)
    .fetch_one(&mut *tx)
    .await?;
    if count >= push::MAX_DEVICES {
        return Err(HttpError::conflict(
            "Too many devices: remove one you no longer use",
        ));
    }
    let device: Device = sqlx::query_as(sqlx::AssertSqlSafe(format!(
        "INSERT INTO push_subscriptions (id, user_id, kind, name, endpoint, p256dh, auth, created_at)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8)
         ON CONFLICT (endpoint) DO UPDATE SET user_id = ?2, kind = ?3, name = ?4,
           p256dh = ?6, auth = ?7
         RETURNING {DEVICE_COLUMNS}"
    )))
    .bind(Uuid::now_v7())
    .bind(user.id)
    .bind(body.kind)
    .bind(name)
    .bind(&endpoint)
    .bind(p256dh)
    .bind(auth)
    .bind(crate::auth::now_iso())
    .fetch_one(&mut *tx)
    .await?;
    tx.commit().await?;
    Ok((
        StatusCode::CREATED,
        Json(Subscription::of(device, state.push.apns())),
    )
        .into_response())
}

#[utoipa::path(
    delete,
    path = "/notifications/subscriptions/{id}",
    tag = "Notifications",
    operation_id = "removePushSubscription",
    summary = "Stop notices on a device",
    params(("id" = Uuid, Path, description = "The device")),
    responses(
        (status = 204, description = "Removed"),
        (status = 401, description = "Not signed in", body = ErrorBody),
        (status = 403, description = "A read-only API key", body = ErrorBody),
        (status = 404, description = "No such device of yours", body = ErrorBody),
    ),
    security(("session" = []), ("api_key" = [])),
)]
async fn remove_push_subscription(
    State(state): State<AppState>,
    AuthUser(user, _): AuthUser,
    Path(id): Path<Uuid>,
) -> Result<StatusCode, HttpError> {
    let removed = sqlx::query("DELETE FROM push_subscriptions WHERE id = ?1 AND user_id = ?2")
        .bind(id)
        .bind(user.id)
        .execute(state.db.writer())
        .await?;
    if removed.rows_affected() == 0 {
        return Err(HttpError::not_found());
    }
    Ok(StatusCode::NO_CONTENT)
}

/// What is new for the user: what a device shows after a push.
#[derive(Serialize, ToSchema)]
pub struct News {
    /// How many items of the drop box wait for review now.
    pub review: i64,
    /// The notices of the last day, newest first (at most 20): a device
    /// shows the first.
    pub notices: Vec<Notice>,
}

#[utoipa::path(
    get,
    path = "/notifications/news",
    tag = "Notifications",
    operation_id = "getNews",
    summary = "What's new for you",
    description = "What a device asks once a push woke it: the push itself says nothing.",
    responses(
        (status = 200, description = "The count to review and the last day's notices", body = News),
        (status = 401, description = "Not signed in", body = ErrorBody),
    ),
    security(("session" = []), ("api_key" = [])),
)]
async fn get_news(
    State(state): State<AppState>,
    AuthUser(user, _): AuthUser,
) -> Result<Json<News>, HttpError> {
    Ok(Json(News {
        review: push::waiting(&state.db, user.id).await?.0,
        notices: push::news(&state.db, user.id).await?,
    }))
}
