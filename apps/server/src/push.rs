//! Push notifications (docs/M6D_DESIGN.md): a notice reaches a user's
//! browsers and phones when Organizer is not open.
//!
//! What is pushed is a **wake-up with no content**: the same constant body
//! for every notice (encrypted anyway, for Web Push), so nothing about the
//! user's files passes through Google, Mozilla, Apple or ntfy. The app or
//! the browser's service worker then asks this server what is new
//! (`GET /api/notifications/news`) and shows that.
//!
//! - **Web Push** (RFC 8030, encrypted per RFC 8291, the server identified
//!   by VAPID, RFC 8292) reaches browsers and the Android app through
//!   UnifiedPush. web-push-native builds the encrypted request; our reqwest
//!   sends it. The VAPID key pair is made at first start and kept in
//!   `push_keys` (`VAPID_PRIVATE_KEY` wins over it).
//! - **APNs** reaches the iOS app: HTTP/2, a provider token (an ES256 JWT,
//!   signed with jwt-simple, which web-push-native brings and re-exports)
//!   from the team's `.p8` key, all set by the `APNS_*` variables. Without
//!   them, iOS devices are kept but nothing is sent to them.
//!
//! An endpoint the push service says is gone (404 or 410; APNs' 410 or a
//! bad device token) is removed. A push endpoint is a URL a user gives, so
//! it goes through the guard of the drop box's links (`drop_box/link.rs`):
//! https and a public address, unless `LINKS_ALLOW_PRIVATE` (a UnifiedPush
//! distributor on the local network).

use crate::auth::{iso, now_iso};
use crate::db::Db;
use crate::state::AppState;
use serde::{Deserialize, Serialize};
use std::collections::HashSet;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant, SystemTime};
use utoipa::ToSchema;
use uuid::Uuid;
use web_push_native::jwt_simple::prelude::{
    Claims, ECDSAP256KeyPairLike as _, ECDSAP256PublicKeyLike as _, ES256KeyPair,
};
use web_push_native::jwt_simple::reexports::ct_codecs::{
    Base64UrlSafeNoPadding, Decoder as _, Encoder as _,
};
use web_push_native::{Auth, WebPushBuilder, p256};

/// What every push carries, whatever the notice: nothing.
pub const PAYLOAD: &[u8] = b"{}";
/// The text of an APNs notice, the same for every one: the app asks the
/// server for the rest when it is opened.
pub const APNS_TITLE: &str = "Organizer";
pub const APNS_BODY: &str = "Something new for you";
/// At most one notice of items to review per user in this time.
pub const REVIEW_EVERY: Duration = Duration::from_secs(15 * 60);
/// How long after a change of the drop box its items are counted, so that
/// a drop of several files is one notice with their number.
const REVIEW_SETTLE: Duration = Duration::from_secs(5);
/// How long a push service keeps a notice for a device that is offline.
const TTL: Duration = Duration::from_secs(12 * 60 * 60);
const TIMEOUT: Duration = Duration::from_secs(15);
/// The notices "what's new" answers: the last day's.
const NEWS_WITHIN: Duration = Duration::from_secs(24 * 60 * 60);
/// Notices are kept a week.
const KEEP_NOTICES: Duration = Duration::from_secs(7 * 24 * 60 * 60);
/// The most devices one user may have.
pub const MAX_DEVICES: i64 = 20;
/// APNs wants a provider token renewed after 20 minutes at the earliest
/// and an hour at the latest.
const APNS_TOKEN_AGE: Duration = Duration::from_secs(40 * 60);
pub const APNS_HOST: &str = "https://api.push.apple.com";
pub const APNS_SANDBOX_HOST: &str = "https://api.sandbox.push.apple.com";
pub const APNS_DEFAULT_TOPIC: &str = "eu.vardalabs.organizer";

/// What a notice is about; each is a switch in the user's settings.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize, ToSchema, sqlx::Type)]
#[serde(rename_all = "snake_case")]
#[sqlx(rename_all = "snake_case")]
// A schema name of its own: the files' `Kind` (file, folder) has that one.
#[schema(as = NoticeKind)]
pub enum Kind {
    /// Items of the drop box wait for review.
    Review,
    /// Something was shared with the user.
    Shared,
    /// A backup failed (superadmins).
    Backup,
}

/// How a device is reached.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize, ToSchema, sqlx::Type)]
#[serde(rename_all = "lowercase")]
#[sqlx(rename_all = "lowercase")]
pub enum DeviceKind {
    /// A browser (Web Push).
    Web,
    /// The Android app, through the user's UnifiedPush distributor (Web Push).
    Unifiedpush,
    /// The iOS app (APNs; the endpoint is its device token).
    Apns,
}

/// The `VAPID_PRIVATE_KEY` and `APNS_*` variables.
#[derive(Clone, Default)]
pub struct Env {
    /// base64url, the key's 32 raw bytes (what `web-push generate-vapid-keys`
    /// prints).
    pub vapid_private_key: Option<String>,
    pub apns: Option<ApnsEnv>,
}

/// How to reach APNs.
#[derive(Clone)]
pub struct ApnsEnv {
    /// The `.p8` file's contents (PKCS#8 PEM).
    pub key: String,
    pub key_id: String,
    pub team_id: String,
    /// The app's bundle id.
    pub topic: String,
    /// `APNS_HOST` or `APNS_SANDBOX_HOST` (`APNS_SANDBOX`); a stand-in in
    /// the tests, over plain HTTP/2.
    pub base_url: String,
}

impl Env {
    pub fn from_env() -> Result<Env, String> {
        use crate::config::{flag, var};
        let apns = match (var("APNS_KEY"), var("APNS_KEY_ID"), var("APNS_TEAM_ID")) {
            (None, None, None) => None,
            (Some(key), Some(key_id), Some(team_id)) => Some(ApnsEnv {
                // A key pasted on one line, with `\n` for its line breaks.
                key: key.replace("\\n", "\n"),
                key_id,
                team_id,
                topic: var("APNS_TOPIC").unwrap_or_else(|| APNS_DEFAULT_TOPIC.into()),
                base_url: if flag("APNS_SANDBOX")?.unwrap_or(false) {
                    APNS_SANDBOX_HOST.into()
                } else {
                    APNS_HOST.into()
                },
            }),
            _ => {
                return Err(
                    "APNS_KEY, APNS_KEY_ID and APNS_TEAM_ID go together: set all three, or none"
                        .into(),
                );
            }
        };
        Ok(Env {
            vapid_private_key: var("VAPID_PRIVATE_KEY"),
            apns,
        })
    }
}

/// The push senders and their keys.
pub struct Push {
    vapid: ES256KeyPair,
    /// The VAPID public key, base64url (uncompressed point): what a browser
    /// subscribes with (`applicationServerKey`).
    vapid_public_key: String,
    apns: Option<Apns>,
    /// Web Push requests: no redirects, public addresses only (unless
    /// `LINKS_ALLOW_PRIVATE`).
    client: reqwest::Client,
    allow_private: bool,
    /// The users whose items to review will be counted soon.
    review_pending: Mutex<HashSet<Uuid>>,
    /// One look at the items to review at a time, so two cannot both send.
    review_lock: tokio::sync::Mutex<()>,
}

struct Apns {
    key: ES256KeyPair,
    team_id: String,
    topic: String,
    base_url: String,
    client: reqwest::Client,
    /// The provider token and when it was made.
    token: Mutex<Option<(String, Instant)>>,
}

impl Apns {
    fn new(env: ApnsEnv) -> anyhow::Result<Apns> {
        let key = ES256KeyPair::from_pem(env.key.trim())
            .map_err(|e| anyhow::anyhow!("APNS_KEY is not a .p8 key: {e}"))?
            .with_key_id(&env.key_id);
        Ok(Apns {
            key,
            team_id: env.team_id,
            topic: env.topic,
            base_url: env.base_url.trim_end_matches('/').to_string(),
            client: reqwest::Client::builder()
                .timeout(TIMEOUT)
                .redirect(reqwest::redirect::Policy::none())
                // APNs speaks HTTP/2 only.
                .http2_prior_knowledge()
                .build()
                .expect("an HTTP client"),
            token: Mutex::new(None),
        })
    }

    /// The provider token: `{alg: ES256, kid}` and `{iss: team, iat}`,
    /// reused until it is 40 minutes old.
    fn token(&self) -> anyhow::Result<String> {
        let mut cached = self.token.lock().expect("not poisoned");
        if let Some((token, made)) = cached.as_ref()
            && made.elapsed() < APNS_TOKEN_AGE
        {
            return Ok(token.clone());
        }
        let token = provider_token(&self.key, &self.team_id)?;
        *cached = Some((token.clone(), Instant::now()));
        Ok(token)
    }

    fn forget_token(&self) {
        *self.token.lock().expect("not poisoned") = None;
    }
}

/// An APNs provider token: only `iss` and `iat`, as Apple documents it.
pub fn provider_token(key: &ES256KeyPair, team_id: &str) -> anyhow::Result<String> {
    let mut claims = Claims::create(web_push_native::jwt_simple::prelude::Duration::from_secs(
        3600,
    ))
    .with_issuer(team_id);
    claims.expires_at = None;
    claims.invalid_before = None;
    key.sign(claims)
        .map_err(|e| anyhow::anyhow!("cannot sign the APNs token: {e}"))
}

/// The body of every APNs notice: a fixed alert (so it shows while the app
/// is not running) that also wakes a running app to fetch what is new.
pub fn apns_payload() -> serde_json::Value {
    serde_json::json!({
        "aps": {
            "alert": { "title": APNS_TITLE, "body": APNS_BODY },
            "sound": "default",
            "content-available": 1,
        }
    })
}

/// What came of one push.
#[derive(Debug, PartialEq)]
enum Sent {
    Delivered,
    /// The device is gone: remove it.
    Gone,
    /// Not sent now (an APNs device without APNs set up).
    Skipped,
    Failed(String),
}

/// base64url without padding, as Web Push writes keys.
pub fn b64url(bytes: &[u8]) -> String {
    Base64UrlSafeNoPadding::encode_to_string(bytes).expect("base64 of bytes")
}

/// base64url with or without padding.
pub fn from_b64url(text: &str) -> Option<Vec<u8>> {
    Base64UrlSafeNoPadding::decode_to_vec(text.trim().trim_end_matches('='), None).ok()
}

impl Push {
    /// The senders, with the VAPID key of the database (made now if there
    /// is none) unless `VAPID_PRIVATE_KEY` gives one.
    pub async fn load(db: &Db, env: Env, allow_private: bool) -> anyhow::Result<Push> {
        let vapid = match env.vapid_private_key {
            Some(key) => vapid_key(&key).ok_or_else(|| {
                anyhow::anyhow!("VAPID_PRIVATE_KEY is not a base64url P-256 private key")
            })?,
            None => {
                let stored: Option<(String,)> =
                    sqlx::query_as("SELECT vapid_private_key FROM push_keys WHERE id = 1")
                        .fetch_optional(db.reader())
                        .await?;
                match stored.and_then(|(key,)| vapid_key(&key)) {
                    Some(key) => key,
                    None => {
                        let key = ES256KeyPair::generate();
                        sqlx::query(
                            "INSERT INTO push_keys (id, vapid_private_key) VALUES (1, ?1)
                             ON CONFLICT (id) DO UPDATE SET vapid_private_key = ?1",
                        )
                        .bind(b64url(&key.to_bytes()))
                        .execute(db.writer())
                        .await?;
                        key
                    }
                }
            }
        };
        let vapid_public_key = b64url(&vapid.public_key().public_key().to_bytes_uncompressed());
        let apns = env.apns.map(Apns::new).transpose()?;
        let mut client = reqwest::Client::builder()
            .timeout(TIMEOUT)
            .user_agent(concat!("Organizer/", env!("CARGO_PKG_VERSION")))
            .redirect(reqwest::redirect::Policy::none());
        if !allow_private {
            client = client.dns_resolver(Arc::new(crate::drop_box::link::PublicOnly));
        }
        Ok(Push {
            vapid,
            vapid_public_key,
            apns,
            client: client.build().expect("an HTTP client"),
            allow_private,
            review_pending: Mutex::new(HashSet::new()),
            review_lock: tokio::sync::Mutex::new(()),
        })
    }

    pub fn vapid_public_key(&self) -> &str {
        &self.vapid_public_key
    }

    /// Whether APNs is set up (the `APNS_*` variables).
    pub fn apns(&self) -> bool {
        self.apns.is_some()
    }

    /// Whether a Web Push endpoint may be used: https (or, with
    /// `LINKS_ALLOW_PRIVATE`, http) on an address that is allowed.
    pub fn endpoint_allowed(&self, endpoint: &str) -> bool {
        let Ok(url) = reqwest::Url::parse(endpoint) else {
            return false;
        };
        (url.scheme() == "https" || self.allow_private)
            && crate::drop_box::link::allowed(&url, self.allow_private)
    }

    async fn send_web(&self, device: &Device, contact: &str) -> Sent {
        let built = (|| -> anyhow::Result<reqwest::Request> {
            let p256dh = device
                .p256dh
                .as_deref()
                .and_then(from_b64url)
                .ok_or_else(|| anyhow::anyhow!("no client key"))?;
            let auth = device
                .auth
                .as_deref()
                .and_then(from_b64url)
                .filter(|a| a.len() == 16)
                .ok_or_else(|| anyhow::anyhow!("no auth secret"))?;
            let mut secret = Auth::default();
            secret.copy_from_slice(&auth);
            let request = WebPushBuilder::new(
                device.endpoint.parse()?,
                p256::PublicKey::from_sec1_bytes(&p256dh)?,
                secret,
            )
            .with_valid_duration(TTL)
            .with_vapid(&self.vapid, contact)
            .build(PAYLOAD)?;
            let mut request = reqwest::Request::try_from(request)?;
            // A wake-up not yet delivered is replaced by the next one
            // (RFC 8030 §5.4), and none is urgent.
            let headers = request.headers_mut();
            headers.insert("topic", "organizer".parse()?);
            headers.insert("urgency", "normal".parse()?);
            Ok(request)
        })();
        let request = match built {
            Ok(request) => request,
            Err(e) => return Sent::Failed(format!("{e:#}")),
        };
        if !self.endpoint_allowed(request.url().as_str()) {
            return Sent::Failed("the endpoint is not allowed".into());
        }
        match self.client.execute(request).await {
            Ok(response) => match response.status().as_u16() {
                200..=299 => Sent::Delivered,
                404 | 410 => Sent::Gone,
                status => Sent::Failed(format!("the push service answered {status}")),
            },
            Err(e) => Sent::Failed(format!("{e}")),
        }
    }

    async fn send_apns(&self, device: &Device) -> Sent {
        let Some(apns) = &self.apns else {
            return Sent::Skipped;
        };
        for attempt in 0..2 {
            let token = match apns.token() {
                Ok(token) => token,
                Err(e) => return Sent::Failed(format!("{e:#}")),
            };
            let expiration = SystemTime::now()
                .duration_since(SystemTime::UNIX_EPOCH)
                .unwrap_or_default()
                + TTL;
            let response = apns
                .client
                .post(format!("{}/3/device/{}", apns.base_url, device.endpoint))
                .bearer_auth(token)
                .header("apns-topic", &apns.topic)
                .header("apns-push-type", "alert")
                .header("apns-priority", "10")
                .header("apns-expiration", expiration.as_secs().to_string())
                .header("apns-collapse-id", "organizer")
                .json(&apns_payload())
                .send()
                .await;
            let response = match response {
                Ok(response) => response,
                Err(e) => return Sent::Failed(format!("{e}")),
            };
            let status = response.status().as_u16();
            if (200..300).contains(&status) {
                return Sent::Delivered;
            }
            let reason = response
                .json::<serde_json::Value>()
                .await
                .ok()
                .and_then(|v| v["reason"].as_str().map(String::from))
                .unwrap_or_default();
            match (status, reason.as_str()) {
                (410, _) | (400, "BadDeviceToken") | (400, "DeviceTokenNotForTopic") => {
                    return Sent::Gone;
                }
                (403, "ExpiredProviderToken") if attempt == 0 => apns.forget_token(),
                _ => return Sent::Failed(format!("APNs answered {status} {reason}")),
            }
        }
        Sent::Failed("APNs refused the provider token".into())
    }
}

/// A VAPID private key from base64url (its 32 raw bytes).
fn vapid_key(text: &str) -> Option<ES256KeyPair> {
    ES256KeyPair::from_bytes(&from_b64url(text)?).ok()
}

/// A device of the user's, as stored.
#[derive(Clone, Debug, sqlx::FromRow)]
pub struct Device {
    pub id: Uuid,
    pub user_id: Uuid,
    pub kind: DeviceKind,
    pub name: String,
    pub endpoint: String,
    pub p256dh: Option<String>,
    pub auth: Option<String>,
    pub created_at: String,
    pub last_notice_at: Option<String>,
}

pub const DEVICE_COLUMNS: &str =
    "id, user_id, kind, name, endpoint, p256dh, auth, created_at, last_notice_at";

/// A user's switches.
#[derive(Clone, Copy, Debug, PartialEq, Serialize, Deserialize, ToSchema, sqlx::FromRow)]
pub struct Settings {
    /// Items waiting for review: at most one notice a quarter hour, with
    /// the count.
    pub review: bool,
    /// Something shared with the user.
    pub shared: bool,
    /// A failed backup (superadmins only).
    pub backup: bool,
}

impl Default for Settings {
    fn default() -> Self {
        Settings {
            review: true,
            shared: true,
            backup: true,
        }
    }
}

impl Settings {
    pub fn wants(&self, kind: Kind) -> bool {
        match kind {
            Kind::Review => self.review,
            Kind::Shared => self.shared,
            Kind::Backup => self.backup,
        }
    }
}

/// A user's switches: the `notifications.*` settings (theirs, their
/// teams', the superadmin's).
pub async fn settings_of(db: &Db, user_id: Uuid) -> Result<Settings, sqlx::Error> {
    let env = crate::settings::Env::default();
    let to = crate::settings::For::user(user_id);
    let get = |key: &'static str| {
        let env = env.clone();
        async move {
            crate::settings::resolve(db, &env, key, to)
                .await
                .map(|r| r.bool())
        }
    };
    Ok(Settings {
        review: get("notifications.review").await?,
        shared: get("notifications.shared").await?,
        backup: get("notifications.backup").await?,
    })
}

/// Store a user's switches: those that changed, each refused when a level
/// above locked it.
pub async fn save_settings(
    state: &crate::state::AppState,
    user_id: Uuid,
    s: Settings,
) -> Result<(), crate::error::HttpError> {
    let now = settings_of(&state.db, user_id).await?;
    for (key, was, wanted) in [
        ("notifications.review", now.review, s.review),
        ("notifications.shared", now.shared, s.shared),
        ("notifications.backup", now.backup, s.backup),
    ] {
        if was != wanted {
            crate::settings::set_own(state, user_id, key, wanted.into()).await?;
        }
    }
    Ok(())
}

/// The statuses of the drop box's items that wait for the user: the same
/// as the sidebar's badge (`WAITING` in the frontend's `lib/drop-box.ts`).
const WAITING: &str = "'proposed', 'needs_manual', 'rejected', 'failed', 'waiting', 'undone'";

/// How many of the user's items wait for review, and when the last of them
/// changed.
pub async fn waiting(db: &Db, user_id: Uuid) -> Result<(i64, Option<String>), sqlx::Error> {
    sqlx::query_as(sqlx::AssertSqlSafe(format!(
        "SELECT count(*), max(updated_at) FROM inbox_items
         WHERE user_id = ?1 AND status IN ({WAITING})"
    )))
    .bind(user_id)
    .fetch_one(db.reader())
    .await
}

/// Tell a user about something, in the background: the call returns at
/// once. What the sharing code calls (`notify(state, user, Kind::Shared)`).
pub fn notify(state: &AppState, user_id: Uuid, kind: Kind) {
    let state = state.clone();
    tokio::spawn(async move {
        if let Err(e) = notify_now(&state, user_id, kind).await {
            tracing::warn!("cannot send the notice: {e:#}");
        }
    });
}

/// [`notify`], waiting until it is sent. Items to review go through
/// [`check_review`] and its quarter hour.
pub async fn notify_now(state: &AppState, user_id: Uuid, kind: Kind) -> anyhow::Result<()> {
    if kind == Kind::Review {
        return check_review(state, user_id).await;
    }
    if !settings_of(&state.db, user_id).await?.wants(kind) {
        return Ok(());
    }
    if kind == Kind::Backup {
        let superadmin: Option<(bool,)> =
            sqlx::query_as("SELECT is_superadmin FROM users WHERE id = ?1 AND disabled_at IS NULL")
                .bind(user_id)
                .fetch_optional(state.db.reader())
                .await?;
        if superadmin != Some((true,)) {
            return Ok(());
        }
    }
    record(state, user_id, kind, None).await?;
    deliver(state, user_id).await
}

/// Tell every superadmin (a failed backup).
pub async fn notify_superadmins(state: &AppState, kind: Kind) -> anyhow::Result<()> {
    let superadmins: Vec<(Uuid,)> =
        sqlx::query_as("SELECT id FROM users WHERE is_superadmin = 1 AND disabled_at IS NULL")
            .fetch_all(state.db.reader())
            .await?;
    for (superadmin,) in superadmins {
        notify_now(state, superadmin, kind).await?;
    }
    Ok(())
}

/// Look at a user's items to review: a notice with their count when one
/// came since the last notice, and that was a quarter hour ago or more;
/// sooner, the look is made again once the quarter hour is over.
pub async fn check_review(state: &AppState, user_id: Uuid) -> anyhow::Result<()> {
    let _one_at_a_time = state.push.review_lock.lock().await;
    if !settings_of(&state.db, user_id).await?.review || !has_devices(state, user_id).await? {
        return Ok(());
    }
    let (count, newest) = waiting(&state.db, user_id).await?;
    let Some(newest) = newest.filter(|_| count > 0) else {
        return Ok(());
    };
    let last: Option<(Option<String>,)> =
        sqlx::query_as("SELECT review_notified_at FROM notification_settings WHERE user_id = ?1")
            .bind(user_id)
            .fetch_optional(state.db.reader())
            .await?;
    if let Some(last) = last.and_then(|(at,)| at) {
        if newest <= last {
            // Nothing new since the last notice.
            return Ok(());
        }
        let since = last
            .parse::<jiff::Timestamp>()
            .ok()
            .and_then(|t| jiff::Timestamp::now().duration_since(t).try_into().ok())
            .unwrap_or(REVIEW_EVERY);
        if since < REVIEW_EVERY {
            schedule_review(state, user_id, REVIEW_EVERY - since);
            return Ok(());
        }
    }
    sqlx::query(
        "INSERT INTO notification_settings (user_id, review_notified_at) VALUES (?1, ?2)
         ON CONFLICT (user_id) DO UPDATE SET review_notified_at = ?2",
    )
    .bind(user_id)
    .bind(now_iso())
    .execute(state.db.writer())
    .await?;
    record(state, user_id, Kind::Review, Some(count)).await?;
    deliver(state, user_id).await
}

/// Look at the user's items to review after a while; once, however often
/// it is asked for in the meantime.
pub fn schedule_review(state: &AppState, user_id: Uuid, after: Duration) {
    if !state
        .push
        .review_pending
        .lock()
        .expect("not poisoned")
        .insert(user_id)
    {
        return;
    }
    let state = state.clone();
    tokio::spawn(async move {
        tokio::time::sleep(after).await;
        state
            .push
            .review_pending
            .lock()
            .expect("not poisoned")
            .remove(&user_id);
        if let Err(e) = check_review(&state, user_id).await {
            tracing::warn!("cannot send the notice of items to review: {e:#}");
        }
    });
}

/// Follow the drop box's changes: its items are counted a few seconds after
/// they change. Spawned by `main`.
pub async fn run(state: AppState) {
    let mut events = state.files.subscribe();
    loop {
        match events.recv().await {
            Ok((user_id, crate::files::Topic::DropBox)) => {
                schedule_review(&state, user_id, REVIEW_SETTLE);
            }
            Ok(_) => {}
            Err(tokio::sync::broadcast::error::RecvError::Lagged(_)) => {}
            Err(tokio::sync::broadcast::error::RecvError::Closed) => return,
        }
    }
}

async fn has_devices(state: &AppState, user_id: Uuid) -> Result<bool, sqlx::Error> {
    let (n,): (i64,) = sqlx::query_as("SELECT count(*) FROM push_subscriptions WHERE user_id = ?1")
        .bind(user_id)
        .fetch_one(state.db.reader())
        .await?;
    Ok(n > 0)
}

/// Keep the notice for "what's new", and drop the user's notices older than
/// a week.
async fn record(
    state: &AppState,
    user_id: Uuid,
    kind: Kind,
    count: Option<i64>,
) -> Result<(), sqlx::Error> {
    let mut tx = state.db.writer().begin().await?;
    sqlx::query("DELETE FROM notices WHERE user_id = ?1 AND created_at < ?2")
        .bind(user_id)
        .bind(iso(SystemTime::now() - KEEP_NOTICES))
        .execute(&mut *tx)
        .await?;
    sqlx::query(
        "INSERT INTO notices (id, user_id, kind, count, created_at) VALUES (?1, ?2, ?3, ?4, ?5)",
    )
    .bind(Uuid::now_v7())
    .bind(user_id)
    .bind(kind)
    .bind(count)
    .bind(now_iso())
    .execute(&mut *tx)
    .await?;
    tx.commit().await
}

/// Wake every device of the user. A device that is gone is removed; one
/// that got it is marked with the time.
pub async fn deliver(state: &AppState, user_id: Uuid) -> anyhow::Result<()> {
    let devices: Vec<Device> = sqlx::query_as(sqlx::AssertSqlSafe(format!(
        "SELECT {DEVICE_COLUMNS} FROM push_subscriptions WHERE user_id = ?1"
    )))
    .bind(user_id)
    .fetch_all(state.db.reader())
    .await?;
    let contact = state
        .public_url
        .clone()
        .unwrap_or_else(|| "mailto:organizer@localhost".into());
    for device in devices {
        let sent = match device.kind {
            DeviceKind::Web | DeviceKind::Unifiedpush => {
                state.push.send_web(&device, &contact).await
            }
            DeviceKind::Apns => state.push.send_apns(&device).await,
        };
        match sent {
            Sent::Delivered => {
                sqlx::query("UPDATE push_subscriptions SET last_notice_at = ?2 WHERE id = ?1")
                    .bind(device.id)
                    .bind(now_iso())
                    .execute(state.db.writer())
                    .await?;
            }
            Sent::Gone => {
                tracing::info!("a push endpoint is gone: its device is removed");
                sqlx::query("DELETE FROM push_subscriptions WHERE id = ?1")
                    .bind(device.id)
                    .execute(state.db.writer())
                    .await?;
            }
            Sent::Skipped => {}
            // The address of the push service is not logged: it identifies
            // the device.
            Sent::Failed(e) => tracing::warn!("a push failed: {e}"),
        }
    }
    Ok(())
}

/// A notice as the apps show it.
#[derive(Clone, Debug, Serialize, ToSchema)]
pub struct Notice {
    pub id: Uuid,
    pub kind: Kind,
    /// The notification's title, e.g. "Drop box".
    pub title: String,
    /// Its text, e.g. "3 items to review".
    pub body: String,
    /// The page it opens, e.g. `/drop-box`.
    pub path: String,
    pub created_at: String,
}

/// The words of a notice.
pub fn words(kind: Kind, count: Option<i64>) -> (String, String, String) {
    match kind {
        Kind::Review => {
            let n = count.unwrap_or(0);
            (
                "Drop box".into(),
                if n == 1 {
                    "1 item to review".into()
                } else {
                    format!("{n} items to review")
                },
                "/drop-box".into(),
            )
        }
        Kind::Shared => (
            "Shared with you".into(),
            "Something was shared with you".into(),
            "/shared".into(),
        ),
        Kind::Backup => (
            "Backup failed".into(),
            "The last backup failed".into(),
            "/settings/backups".into(),
        ),
    }
}

/// The notices of the last day, newest first.
pub async fn news(db: &Db, user_id: Uuid) -> Result<Vec<Notice>, sqlx::Error> {
    let rows: Vec<(Uuid, Kind, Option<i64>, String)> = sqlx::query_as(
        "SELECT id, kind, count, created_at FROM notices
         WHERE user_id = ?1 AND created_at >= ?2 ORDER BY created_at DESC, id DESC LIMIT 20",
    )
    .bind(user_id)
    .bind(iso(SystemTime::now() - NEWS_WITHIN))
    .fetch_all(db.reader())
    .await?;
    Ok(rows
        .into_iter()
        .map(|(id, kind, count, created_at)| {
            let (title, body, path) = words(kind, count);
            Notice {
                id,
                kind,
                title,
                body,
                path,
                created_at,
            }
        })
        .collect())
}

#[cfg(test)]
mod tests {
    use super::*;
    use web_push_native::jwt_simple::prelude::NoCustomClaims;

    #[test]
    fn the_apns_token_has_the_team_and_key_and_nothing_else() {
        let key = ES256KeyPair::generate().with_key_id("ABC123DEFG");
        let token = provider_token(&key, "TEAM123456").unwrap();
        let parts: Vec<&str> = token.split('.').collect();
        let header: serde_json::Value =
            serde_json::from_slice(&from_b64url(parts[0]).unwrap()).unwrap();
        assert_eq!(header["alg"], "ES256");
        assert_eq!(header["kid"], "ABC123DEFG");
        let claims: serde_json::Value =
            serde_json::from_slice(&from_b64url(parts[1]).unwrap()).unwrap();
        let mut names: Vec<&String> = claims.as_object().unwrap().keys().collect();
        names.sort();
        assert_eq!(names, ["iat", "iss"]);
        assert_eq!(claims["iss"], "TEAM123456");
        key.public_key()
            .verify_token::<NoCustomClaims>(&token, None)
            .unwrap();
    }

    #[test]
    fn the_apns_payload_says_nothing_of_the_notice() {
        let payload = apns_payload();
        assert_eq!(payload["aps"]["alert"]["body"], APNS_BODY);
        assert_eq!(payload["aps"]["content-available"], 1);
        assert_eq!(payload.as_object().unwrap().len(), 1);
    }

    #[test]
    fn the_notice_kind_is_a_schema_of_its_own() {
        let spec: serde_json::Value = serde_json::from_str(&crate::openapi::spec_json()).unwrap();
        let schemas = &spec["components"]["schemas"];
        assert_eq!(
            schemas["Notice"]["properties"]["kind"]["$ref"],
            "#/components/schemas/NoticeKind"
        );
        assert_eq!(
            schemas["NoticeKind"]["enum"],
            serde_json::json!(["review", "shared", "backup"])
        );
    }

    #[test]
    fn notices_are_worded() {
        assert_eq!(words(Kind::Review, Some(1)).1, "1 item to review");
        assert_eq!(words(Kind::Review, Some(3)).1, "3 items to review");
        assert_eq!(words(Kind::Backup, None).2, "/settings/backups");
    }

    #[test]
    fn an_apns_key_is_read_and_its_token_reused() {
        // A .p8 key is read (and nothing else is), and its token reused.
        let key = ES256KeyPair::generate().to_pem().unwrap();
        let apns = Apns::new(ApnsEnv {
            key,
            key_id: "K".into(),
            team_id: "T".into(),
            topic: APNS_DEFAULT_TOPIC.into(),
            base_url: APNS_HOST.into(),
        })
        .unwrap();
        assert_eq!(apns.token().unwrap(), apns.token().unwrap());
        assert!(
            Apns::new(ApnsEnv {
                key: "not a key".into(),
                key_id: "K".into(),
                team_id: "T".into(),
                topic: APNS_DEFAULT_TOPIC.into(),
                base_url: APNS_HOST.into(),
            })
            .is_err()
        );
    }

    #[test]
    fn keys_are_base64url() {
        let bytes = [0xfbu8, 0xff, 0x01];
        assert_eq!(b64url(&bytes), "-_8B");
        assert_eq!(from_b64url("-_8B").unwrap(), bytes);
        assert_eq!(from_b64url("-_8B=").unwrap(), bytes);
        assert!(from_b64url("***").is_none());
    }
}
