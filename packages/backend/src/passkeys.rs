//! Passkeys (WebAuthn, through webauthn-rs): a passkey signs in on its own,
//! in place of the password and the authenticator code, since it is two
//! factors already (something you have, unlocked by something you are or
//! know). The password stays as a fallback.
//!
//! A passkey belongs to a domain, so they work only with `PUBLIC_URL` set:
//! its host is the relying party, its origin the one the browser must
//! report. Sign-in is discoverable (no email typed): the passkey says whose
//! it is. The apps sign in through the browser, so they get passkeys with
//! no code of their own.
//!
//! A ceremony's state (its challenge) is kept server-side for five minutes
//! in `passkey_challenges` and used once.

use crate::db::Db;
use std::time::{Duration, SystemTime};
use uuid::Uuid;
use webauthn_rs::prelude::*;

/// How long a registration or sign-in may take.
pub const CHALLENGE_TTL: Duration = Duration::from_secs(300);

/// The relying party for `PUBLIC_URL`; `None` without one (or one that is
/// not a URL with a host).
pub fn webauthn(public_url: Option<&str>) -> Option<Webauthn> {
    let origin = Url::parse(public_url?).ok()?;
    let host = origin.host_str()?.to_string();
    WebauthnBuilder::new(&host, &origin)
        .ok()?
        .rp_name("Organizer")
        .build()
        .ok()
}

/// What a challenge is for.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Kind {
    Register,
    SignIn,
    /// A signed-in user confirming one of their passkeys again (step-up).
    StepUp,
}

impl Kind {
    fn as_str(self) -> &'static str {
        match self {
            Kind::Register => "register",
            Kind::SignIn => "sign_in",
            Kind::StepUp => "step_up",
        }
    }
}

/// Keep a ceremony's state; answers its id, which the client sends back.
pub async fn save_challenge<T: serde::Serialize>(
    db: &Db,
    kind: Kind,
    user: Option<Uuid>,
    state: &T,
) -> anyhow::Result<Uuid> {
    let id = Uuid::now_v7();
    // Old ones go on the way.
    sqlx::query("DELETE FROM passkey_challenges WHERE expires_at < ?1")
        .bind(crate::auth::iso(SystemTime::now()))
        .execute(db.writer())
        .await?;
    sqlx::query(
        "INSERT INTO passkey_challenges (id, user_id, kind, state, expires_at)
         VALUES (?1, ?2, ?3, ?4, ?5)",
    )
    .bind(id)
    .bind(user)
    .bind(kind.as_str())
    .bind(serde_json::to_string(state)?)
    .bind(crate::auth::iso(SystemTime::now() + CHALLENGE_TTL))
    .execute(db.writer())
    .await?;
    Ok(id)
}

/// Take a ceremony's state back, once: `None` when it is unknown, expired,
/// of another kind, or another user's.
pub async fn take_challenge<T: serde::de::DeserializeOwned>(
    db: &Db,
    id: Uuid,
    kind: Kind,
    user: Option<Uuid>,
) -> anyhow::Result<Option<T>> {
    let row: Option<(String, Option<Uuid>)> = sqlx::query_as(
        "DELETE FROM passkey_challenges WHERE id = ?1 AND kind = ?2 AND expires_at >= ?3
         RETURNING state, user_id",
    )
    .bind(id)
    .bind(kind.as_str())
    .bind(crate::auth::iso(SystemTime::now()))
    .fetch_optional(db.writer())
    .await?;
    Ok(match row {
        Some((state, owner)) if owner == user => Some(serde_json::from_str(&state)?),
        _ => None,
    })
}

/// A user's passkeys, as webauthn-rs reads them.
pub async fn of_user(db: &Db, user: Uuid) -> anyhow::Result<Vec<(Uuid, Passkey)>> {
    let rows: Vec<(Uuid, String)> =
        sqlx::query_as("SELECT id, passkey FROM passkeys WHERE user_id = ?1")
            .bind(user)
            .fetch_all(db.reader())
            .await?;
    rows.into_iter()
        .map(|(id, json)| Ok((id, serde_json::from_str(&json)?)))
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_relying_party_is_the_public_url() {
        assert!(webauthn(None).is_none());
        assert!(webauthn(Some("not a url")).is_none());
        assert!(webauthn(Some("https://files.example.com")).is_some());
        // Browsers allow passkeys on plain http only for localhost.
        assert!(webauthn(Some("http://localhost:8000")).is_some());
    }
}
