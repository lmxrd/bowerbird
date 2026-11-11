use crate::util::sha256_hex;
use argon2::Argon2;
use argon2::password_hash::phc::PasswordHash;
use argon2::password_hash::{PasswordHasher, PasswordVerifier, generate_salt};
use serde::{Deserialize, Serialize};
use sqlx::SqliteExecutor;
use std::time::SystemTime;
use utoipa::ToSchema;
use uuid::Uuid;

pub use crate::session::COOKIE;

/// How long a session lasts after the sign-in.
pub const SESSION_DAYS: u64 = 30;

/// The signed-in user, as the session endpoint returns them.
#[derive(Clone, Serialize, sqlx::FromRow, ToSchema)]
pub struct User {
    pub id: Uuid,
    pub email: String,
    pub is_superadmin: bool,
    /// False for a user who signs in through single sign-on only: they can
    /// set a password without giving a current one.
    pub has_password: bool,
    /// Whether the user is linked to an account at the single sign-on
    /// provider.
    pub single_sign_on: bool,
    /// Whether a code from an authenticator app is asked for at sign-in.
    pub totp_enabled: bool,
    /// The hash of the credentials the user's sessions are checked against
    /// (see `session.rs`); never sent.
    #[serde(skip)]
    #[sqlx(default)]
    pub auth_hash: Vec<u8>,
    /// A superadmin disabled the account: it cannot sign in, and its sessions
    /// and tokens do not work. Never sent (a disabled user is never the
    /// session's).
    #[serde(skip)]
    #[sqlx(default)]
    pub disabled: bool,
    /// When the user was last seen, to record it at most once a minute.
    #[serde(skip)]
    #[sqlx(default)]
    pub last_seen_at: Option<String>,
}

// Not derived: the auth hash stays out of logs.
impl std::fmt::Debug for User {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("User")
            .field("id", &self.id)
            .field("email", &self.email)
            .finish_non_exhaustive()
    }
}

/// Hash a password with argon2id; the PHC string is what gets stored.
pub fn hash_password(password: &str) -> String {
    Argon2::default()
        .hash_password_with_salt(password.as_bytes(), &generate_salt())
        .expect("password hashing cannot fail")
        .to_string()
}

pub fn verify_password(password: &str, stored: &str) -> bool {
    PasswordHash::new(stored)
        .ok()
        .and_then(|hash| {
            Argon2::default()
                .verify_password(password.as_bytes(), &hash)
                .ok()
        })
        .is_some()
}

/// What an API key may do. No key reaches a superadmin endpoint: those take
/// a session whose second factor was confirmed lately (step-up, see
/// `routes/auth.rs`), which a key cannot give.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "lowercase")]
pub enum Scope {
    /// Reads only (GET).
    Read,
    /// Reads and changes. The old `admin` scope is taken as this one: it
    /// meant "write, and the admin endpoints", and keys no longer reach
    /// those (migration 0025 rewrote the stored ones).
    #[serde(alias = "admin")]
    Write,
}

impl Scope {
    pub fn as_str(self) -> &'static str {
        match self {
            Scope::Read => "read",
            Scope::Write => "write",
        }
    }

    pub fn parse(value: &str) -> Option<Scope> {
        match value {
            "read" => Some(Scope::Read),
            // `admin`: see `Scope::Write`.
            "write" | "admin" => Some(Scope::Write),
            _ => None,
        }
    }
}

/// The start of every API key, so that one is recognised in a leak.
pub const API_KEY_PREFIX: &str = "org_";

/// What a token is for (`api_keys.kind`): all three are the same kind of
/// token, listed apart and accepted in different places.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "snake_case")]
pub enum KeyKind {
    /// Made by hand in the settings, for scripts: `Authorization: Bearer`.
    Key,
    /// Made by an app's device login: `Authorization: Bearer`.
    Device,
    /// For WebDAV clients: the password of Basic authentication, and only
    /// there.
    AppPassword,
}

impl KeyKind {
    pub fn as_str(self) -> &'static str {
        match self {
            KeyKind::Key => "key",
            KeyKind::Device => "device",
            KeyKind::AppPassword => "app_password",
        }
    }

    pub fn parse(value: &str) -> Option<KeyKind> {
        match value {
            "key" => Some(KeyKind::Key),
            "device" => Some(KeyKind::Device),
            "app_password" => Some(KeyKind::AppPassword),
            _ => None,
        }
    }
}

/// An API key in use: its id, kind, scope and when it was last used.
pub struct KeyUse {
    pub id: Uuid,
    pub kind: KeyKind,
    pub scope: Scope,
    pub last_used_at: Option<String>,
}

#[derive(sqlx::FromRow)]
struct KeyRow {
    #[sqlx(flatten)]
    user: User,
    key_id: Uuid,
    kind: String,
    scope: String,
    last_used_at: Option<String>,
}

/// The user of a valid, unexpired API key, if any.
pub async fn key_user<'e>(db: impl SqliteExecutor<'e>, token: &str) -> Option<(User, KeyUse)> {
    if !token.starts_with(API_KEY_PREFIX) || token.len() > 200 {
        return None;
    }
    let row = sqlx::query_as::<_, KeyRow>(
        "SELECT u.id, u.email, u.is_superadmin,
                u.password_hash IS NOT NULL AS has_password,
                EXISTS (SELECT 1 FROM user_identities i WHERE i.user_id = u.id)
                  AS single_sign_on,
                u.totp_secret IS NOT NULL AS totp_enabled,
                u.disabled_at IS NOT NULL AS disabled, u.last_seen_at,
                k.id AS key_id, k.kind, k.scope, k.last_used_at
         FROM api_keys k JOIN users u ON u.id = k.user_id
         WHERE k.token_hash = ?1 AND (k.expires_at IS NULL OR k.expires_at > ?2)
           AND u.disabled_at IS NULL",
    )
    .bind(sha256_hex(token))
    .bind(now_iso())
    .fetch_optional(db)
    .await
    .ok()
    .flatten()?;
    let scope = Scope::parse(&row.scope)?;
    let kind = KeyKind::parse(&row.kind)?;
    Some((
        row.user,
        KeyUse {
            id: row.key_id,
            kind,
            scope,
            last_used_at: row.last_used_at,
        },
    ))
}

/// Records that a user was active, at most once a minute (the time it
/// last was is on the loaded user), so that a busy client does not take
/// the one writer for every request. A failure is only logged.
pub async fn note_seen(db: &crate::db::Db, user: &User) {
    let now = SystemTime::now();
    let stale = iso(now - std::time::Duration::from_secs(60));
    if user
        .last_seen_at
        .as_deref()
        .is_some_and(|last| last > stale.as_str())
    {
        return;
    }
    if let Err(e) = sqlx::query("UPDATE users SET last_seen_at = ?1 WHERE id = ?2")
        .bind(iso(now))
        .bind(user.id)
        .execute(db.writer())
        .await
    {
        tracing::warn!("cannot record that user {} was active: {e}", user.id);
    }
}

pub fn now_iso() -> String {
    iso(SystemTime::now())
}

/// `YYYY-MM-DDTHH:MM:SS.mmmZ`, the form times are stored in.
pub fn iso(t: SystemTime) -> String {
    let t = jiff::Timestamp::try_from(t).expect("a time within jiff's range");
    format!("{t:.3}")
}
