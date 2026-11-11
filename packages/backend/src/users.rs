//! More than one user (docs/M6A_DESIGN.md): how people join (invite links,
//! open sign-up, single sign-on making accounts) and what a superadmin does to
//! a user (the routes are `routes/users.rs`).
//!
//! The two switches are the `signup_settings` row with `SIGNUP_OPEN` and
//! `OIDC_CREATE_USERS` on top (a variable locks its switch), read when they
//! are needed: they are two integers, not worth keeping in memory.
//! With `OIDC_ONLY`, passwords are off for everyone: an invite is then
//! accepted through single sign-on only, and open sign-up is single
//! sign-on making accounts.

use crate::auth::{iso, now_iso};
use crate::db::Db;
use crate::util::{random_hex, sha256_hex};
use serde::Serialize;
use sqlx::SqliteConnection;
use std::time::{Duration, SystemTime};
use utoipa::ToSchema;
use uuid::Uuid;

/// How long an invite link works.
pub const INVITE_DAYS: u64 = 7;

/// The `SIGNUP_OPEN` and `OIDC_CREATE_USERS` variables: each one that is
/// set wins over its switch and locks it.
#[derive(Clone, Copy, Debug, Default)]
pub struct Env {
    pub open_signup: Option<bool>,
    pub oidc_create_users: Option<bool>,
}

impl Env {
    /// The variables. A value that is not understood stops the start.
    pub fn from_env() -> Result<Self, String> {
        use crate::config::flag;
        Ok(Env {
            open_signup: flag("SIGNUP_OPEN")?,
            oidc_create_users: flag("OIDC_CREATE_USERS")?,
        })
    }

    /// The switches set by a variable, by their field names.
    pub fn locked(&self) -> Vec<&'static str> {
        let mut locked = Vec::new();
        if self.open_signup.is_some() {
            locked.push("open_signup");
        }
        if self.oidc_create_users.is_some() {
            locked.push("oidc_create_users");
        }
        locked
    }

    /// The stored switches with the variables on top.
    pub fn apply(&self, stored: Joining) -> Joining {
        Joining {
            open_signup: self.open_signup.unwrap_or(stored.open_signup),
            oidc_create_users: self.oidc_create_users.unwrap_or(stored.oidc_create_users),
        }
    }
}

/// How people may join besides an invite.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, ToSchema, sqlx::FromRow)]
pub struct Joining {
    /// "Create an account" on the login page.
    pub open_signup: bool,
    /// Someone the single sign-on provider signs in, with a verified email
    /// no account has, gets an account.
    pub oidc_create_users: bool,
}

/// The stored switches, without the variables.
pub async fn stored(db: &Db) -> sqlx::Result<Joining> {
    sqlx::query_as("SELECT open_signup, oidc_create_users FROM signup_settings WHERE id = 1")
        .fetch_one(db.reader())
        .await
}

/// The switches in effect.
pub async fn joining(state: &crate::state::AppState) -> sqlx::Result<Joining> {
    Ok(state.joining.apply(stored(&state.db).await?))
}

/// Whether open sign-up takes an email and a password: it is on, and
/// passwords are allowed.
pub async fn password_signup(state: &crate::state::AppState) -> sqlx::Result<bool> {
    Ok(state.oidc.passwords_allowed() && joining(state).await?.open_signup)
}

/// Whether single sign-on makes an account for someone it signs in who has
/// none. With `OIDC_ONLY`, open sign-up means this too.
pub async fn oidc_creates_users(state: &crate::state::AppState) -> sqlx::Result<bool> {
    let joining = joining(state).await?;
    Ok(joining.oidc_create_users || (!state.oidc.passwords_allowed() && joining.open_signup))
}

/// Whether the login page offers "Create an account": with a password, or
/// with single sign-on when that is the only way in.
pub async fn signup_offered(state: &crate::state::AppState) -> sqlx::Result<bool> {
    if state.oidc.passwords_allowed() {
        password_signup(state).await
    } else {
        Ok(state.oidc.settings().enabled() && joining(state).await?.open_signup)
    }
}

/// A new user, not a superadmin unless said: their tree is the default folder
/// in the data dir. None when the email is taken.
pub async fn create(
    tx: &mut SqliteConnection,
    email: &str,
    password_hash: Option<&str>,
    is_superadmin: bool,
) -> sqlx::Result<Option<Uuid>> {
    let id = Uuid::now_v7();
    let inserted = sqlx::query(
        "INSERT INTO users (id, email, password_hash, is_superadmin) VALUES (?1, ?2, ?3, ?4)
         ON CONFLICT (email) DO NOTHING",
    )
    .bind(id)
    .bind(email)
    .bind(password_hash)
    .bind(is_superadmin)
    .execute(&mut *tx)
    .await?
    .rows_affected();
    Ok((inserted == 1).then_some(id))
}

/// A new invite: its id and its token (shown in the link).
pub async fn make_invite(
    db: &Db,
    email: Option<&str>,
    is_superadmin: bool,
    created_by: Uuid,
) -> sqlx::Result<Uuid> {
    let id = Uuid::now_v7();
    let token = random_hex(32);
    let now = SystemTime::now();
    sqlx::query(
        "INSERT INTO invites (id, token_hash, token, email, is_superadmin, created_by, created_at,
           expires_at)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8)",
    )
    .bind(id)
    .bind(sha256_hex(&token))
    .bind(&token)
    .bind(email)
    .bind(is_superadmin)
    .bind(created_by)
    .bind(iso(now))
    .bind(iso(now + Duration::from_secs(INVITE_DAYS * 86_400)))
    .execute(db.writer())
    .await?;
    Ok(id)
}

/// A pending invite of a token: unused and unexpired.
#[derive(Clone, Debug, sqlx::FromRow)]
pub struct PendingInvite {
    pub id: Uuid,
    pub email: Option<String>,
    pub is_superadmin: bool,
}

/// The pending invite of a token, if any.
pub async fn pending_invite<'e>(
    db: impl sqlx::SqliteExecutor<'e>,
    token: &str,
) -> sqlx::Result<Option<PendingInvite>> {
    if token.len() > 200 {
        return Ok(None);
    }
    sqlx::query_as(
        "SELECT id, email, is_superadmin = 1 AS is_superadmin FROM invites
         WHERE token_hash = ?1 AND used_at IS NULL AND expires_at > ?2",
    )
    .bind(sha256_hex(token))
    .bind(now_iso())
    .fetch_optional(db)
    .await
}

/// Who made the pending invite of a token (a superadmin), if anyone.
pub async fn invite_maker<'e>(
    db: impl sqlx::SqliteExecutor<'e>,
    token: &str,
) -> sqlx::Result<Option<Uuid>> {
    if token.len() > 200 {
        return Ok(None);
    }
    Ok(
        sqlx::query_scalar("SELECT created_by FROM invites WHERE token_hash = ?1")
            .bind(sha256_hex(token))
            .fetch_optional(db)
            .await?
            .flatten(),
    )
}

/// Note in a new account's activity that a superadmin's invite made it.
pub async fn note_invited(
    tx: &mut SqliteConnection,
    user: Uuid,
    maker: Option<Uuid>,
) -> sqlx::Result<()> {
    let Some(maker) = maker else { return Ok(()) };
    crate::activity::log(
        tx,
        user,
        crate::activity::Actor::Superadmin(maker),
        crate::activity::Action::Created,
        Default::default(),
    )
    .await
}

/// Use an invite up for an email: whether the new user is a superadmin, or
/// none when the invite is not pending or is for another email. Its token
/// is forgotten.
pub async fn take_invite(
    tx: &mut SqliteConnection,
    token: &str,
    email: &str,
) -> sqlx::Result<Option<bool>> {
    let Some(invite) = pending_invite(&mut *tx, token).await? else {
        return Ok(None);
    };
    if invite.email.as_deref().is_some_and(|e| e != email) {
        return Ok(None);
    }
    sqlx::query("UPDATE invites SET used_at = ?1, token = NULL WHERE id = ?2")
        .bind(now_iso())
        .bind(invite.id)
        .execute(&mut *tx)
        .await?;
    Ok(Some(invite.is_superadmin))
}

/// How many superadmins who are not disabled there are besides this user: the
/// last one cannot be demoted, disabled or removed.
pub async fn other_superadmins(tx: &mut SqliteConnection, user_id: Uuid) -> sqlx::Result<i64> {
    sqlx::query_scalar(
        "SELECT COUNT(*) FROM users WHERE is_superadmin = 1 AND disabled_at IS NULL AND id != ?1",
    )
    .bind(user_id)
    .fetch_one(&mut *tx)
    .await
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_variable_wins_over_its_switch_and_locks_it() {
        let stored = Joining {
            open_signup: true,
            oidc_create_users: false,
        };
        assert_eq!(Env::default().apply(stored), stored);
        assert!(Env::default().locked().is_empty());
        let env = Env {
            open_signup: Some(false),
            oidc_create_users: None,
        };
        assert_eq!(
            env.apply(stored),
            Joining {
                open_signup: false,
                oidc_create_users: false,
            }
        );
        assert_eq!(env.locked(), ["open_signup"]);
    }

    #[tokio::test]
    async fn an_invite_is_used_once_and_for_its_email() {
        let db = crate::db::open(":memory:").await.unwrap();
        let superadmin = Uuid::now_v7();
        sqlx::query("INSERT INTO users (id, email, is_superadmin) VALUES (?1, 'a@example.com', 1)")
            .bind(superadmin)
            .execute(db.writer())
            .await
            .unwrap();
        let id = make_invite(&db, Some("b@example.com"), false, superadmin)
            .await
            .unwrap();
        let token: String = sqlx::query_scalar("SELECT token FROM invites WHERE id = ?1")
            .bind(id)
            .fetch_one(db.reader())
            .await
            .unwrap();
        let mut tx = db.writer().begin().await.unwrap();
        assert_eq!(
            take_invite(&mut tx, &token, "c@example.com").await.unwrap(),
            None
        );
        assert_eq!(
            take_invite(&mut tx, &token, "b@example.com").await.unwrap(),
            Some(false)
        );
        assert_eq!(
            take_invite(&mut tx, &token, "b@example.com").await.unwrap(),
            None
        );
        tx.commit().await.unwrap();
        let kept: Option<String> = sqlx::query_scalar("SELECT token FROM invites WHERE id = ?1")
            .bind(id)
            .fetch_one(db.reader())
            .await
            .unwrap();
        assert_eq!(kept, None, "a used invite's token is forgotten");
    }
}
