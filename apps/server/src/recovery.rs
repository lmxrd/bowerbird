//! Recovery on the command line (docs/M7A_DESIGN.md §4), run on the server
//! against its database, for when nobody can reach the superadmin's pages:
//!
//! ```text
//! organizer superadmin grant <email>
//! organizer superadmin revoke <email>
//! organizer two-factor reset <email>
//! ```
//!
//! Whoever can run this on the server holds the database anyway: these
//! only spare them editing it by hand. The server may be running (the
//! database is WAL); a change shows at the user's next request.

use crate::activity::Action;
use crate::db::Db;
use anyhow::Context;
use uuid::Uuid;

const USAGE: &str = "usage: organizer superadmin grant <email>
       organizer superadmin revoke <email>
       organizer two-factor reset <email>";

/// Run `organizer superadmin …` or `organizer two-factor …` (`args` after
/// the program's name).
pub async fn main(args: &[String]) -> anyhow::Result<()> {
    let args: Vec<&str> = args.iter().map(String::as_str).collect();
    let (command, email) = match args.as_slice() {
        [group @ ("superadmin" | "two-factor"), action, email] => {
            ((*group, *action), email.to_string())
        }
        _ => anyhow::bail!("{USAGE}"),
    };
    let data_dir = crate::config::data_dir();
    let db_path = crate::config::database_path(&data_dir).map_err(anyhow::Error::msg)?;
    anyhow::ensure!(
        db_path == ":memory:" || std::path::Path::new(&db_path).exists(),
        "No database at {db_path}: set DATA_DIR or DATABASE_URL as the server has them"
    );
    let db = crate::db::open(&db_path).await?;
    let done = match command {
        ("superadmin", "grant") => grant(&db, &email).await,
        ("superadmin", "revoke") => revoke(&db, &email).await,
        ("two-factor", "reset") => reset_two_factor(&db, &email).await,
        _ => Err(anyhow::anyhow!("{USAGE}")),
    };
    db.close().await;
    println!("{}", done?);
    Ok(())
}

/// The user of an email (as the API stores emails: trimmed, lowercase),
/// and whether they are a superadmin and disabled.
async fn user_of<'e>(
    db: impl sqlx::SqliteExecutor<'e>,
    email: &str,
) -> anyhow::Result<(Uuid, bool, bool)> {
    let email = email.trim().to_lowercase();
    sqlx::query_as("SELECT id, is_superadmin, disabled_at IS NOT NULL FROM users WHERE email = ?1")
        .bind(&email)
        .fetch_optional(db)
        .await?
        .with_context(|| format!("No user with the email {email}"))
}

/// Note what the command line did in the account's activity
/// (docs/M7B_DESIGN.md §5).
async fn log(db: &Db, user: uuid::Uuid, action: Action) -> anyhow::Result<()> {
    crate::activity::log(
        db.writer(),
        user,
        crate::activity::Actor::CommandLine,
        action,
        Default::default(),
    )
    .await?;
    Ok(())
}

/// Make a user a superadmin. What it says is done, for the person at the
/// terminal.
pub async fn grant(db: &Db, email: &str) -> anyhow::Result<String> {
    let (id, superadmin, disabled) = user_of(db.reader(), email).await?;
    let mut done = if superadmin {
        format!("{email} is a superadmin already.")
    } else {
        sqlx::query("UPDATE users SET is_superadmin = 1 WHERE id = ?1")
            .bind(id)
            .execute(db.writer())
            .await?;
        log(db, id, Action::SuperadminGranted).await?;
        format!("{email} is a superadmin now.")
    };
    if disabled {
        done.push_str(" The account is disabled: it cannot sign in until it is enabled.");
    }
    Ok(done)
}

/// Make a superadmin a regular user; refused for the last one who is not
/// disabled, as the API refuses it (nobody could then reach the
/// superadmin's pages but through `grant`).
pub async fn revoke(db: &Db, email: &str) -> anyhow::Result<String> {
    let mut tx = db.writer().begin().await?;
    let (id, superadmin, disabled) = user_of(&mut *tx, email).await?;
    if !superadmin {
        return Ok(format!("{email} is not a superadmin."));
    }
    anyhow::ensure!(
        disabled || crate::users::other_superadmins(&mut tx, id).await? > 0,
        "{email} is the last superadmin who is not disabled: grant someone else first"
    );
    sqlx::query("UPDATE users SET is_superadmin = 0 WHERE id = ?1")
        .bind(id)
        .execute(&mut *tx)
        .await?;
    tx.commit().await?;
    log(db, id, Action::SuperadminRevoked).await?;
    Ok(format!("{email} is no longer a superadmin."))
}

/// Remove a user's second factors: the authenticator app (its secret, a
/// setup not confirmed, the recovery codes) and every passkey. They sign
/// in with the password or single sign-on and set them up again. Their
/// sessions end: they may be in the hands of whoever took the lost device.
pub async fn reset_two_factor(db: &Db, email: &str) -> anyhow::Result<String> {
    let (id, _, _) = user_of(db.reader(), email).await?;
    let mut tx = db.writer().begin().await?;
    sqlx::query(
        "UPDATE users SET totp_secret = NULL, totp_pending = NULL, totp_last_step = 0
         WHERE id = ?1",
    )
    .bind(id)
    .execute(&mut *tx)
    .await?;
    for table in ["recovery_codes", "passkeys", "passkey_challenges"] {
        sqlx::query(sqlx::AssertSqlSafe(format!(
            "DELETE FROM {table} WHERE user_id = ?1"
        )))
        .bind(id)
        .execute(&mut *tx)
        .await?;
    }
    // A session names its user in axum-login's record (`session.rs`). The
    // secret is part of the hash sessions are checked against, so those
    // of a user who had the app would end anyway; those of a user with
    // passkeys only would not.
    let sessions = sqlx::query(
        "DELETE FROM sessions
         WHERE json_extract(data, '$.\"axum-login.data\".user_id') = ?1",
    )
    .bind(id.to_string())
    .execute(&mut *tx)
    .await?
    .rows_affected();
    tx.commit().await?;
    log(db, id, Action::TwoFactorReset).await?;
    Ok(format!(
        "{email} has no two-factor authentication nor passkeys now, and was signed out \
         ({sessions} session(s)): they sign in with their password or single sign-on."
    ))
}

#[cfg(test)]
mod tests {
    use super::*;

    async fn user(db: &Db, email: &str, superadmin: bool) -> Uuid {
        let id = Uuid::now_v7();
        sqlx::query("INSERT INTO users (id, email, is_superadmin) VALUES (?1, ?2, ?3)")
            .bind(id)
            .bind(email)
            .bind(superadmin)
            .execute(db.writer())
            .await
            .unwrap();
        id
    }

    async fn is_superadmin(db: &Db, id: Uuid) -> bool {
        sqlx::query_scalar("SELECT is_superadmin FROM users WHERE id = ?1")
            .bind(id)
            .fetch_one(db.reader())
            .await
            .unwrap()
    }

    #[tokio::test]
    async fn superadmins_are_granted_and_revoked_but_never_the_last() {
        let db = crate::db::open(":memory:").await.unwrap();
        let ann = user(&db, "ann@example.com", true).await;
        let bob = user(&db, "bob@example.com", false).await;

        assert!(grant(&db, "nobody@example.com").await.is_err());
        // The last one stays.
        let refused = revoke(&db, "ann@example.com").await.unwrap_err();
        assert!(refused.to_string().contains("last superadmin"), "{refused}");
        assert!(is_superadmin(&db, ann).await);

        // Emails as typed: any case, spaces around.
        grant(&db, " Bob@Example.com ").await.unwrap();
        assert!(is_superadmin(&db, bob).await);
        assert!(
            grant(&db, "bob@example.com")
                .await
                .unwrap()
                .contains("already")
        );
        revoke(&db, "ann@example.com").await.unwrap();
        assert!(!is_superadmin(&db, ann).await);
        assert!(
            revoke(&db, "ann@example.com")
                .await
                .unwrap()
                .contains("not a superadmin")
        );

        // A disabled superadmin does not count, and may go.
        sqlx::query("UPDATE users SET disabled_at = '2026-01-01T00:00:00.000Z' WHERE id = ?1")
            .bind(ann)
            .execute(db.writer())
            .await
            .unwrap();
        assert!(
            grant(&db, "ann@example.com")
                .await
                .unwrap()
                .contains("disabled")
        );
        assert!(revoke(&db, "bob@example.com").await.is_err());
        revoke(&db, "ann@example.com").await.unwrap();
    }

    #[tokio::test]
    async fn a_reset_removes_every_second_factor() {
        let db = crate::db::open(":memory:").await.unwrap();
        let ann = user(&db, "ann@example.com", true).await;
        let bob = user(&db, "bob@example.com", false).await;
        for id in [ann, bob] {
            sqlx::query(
                "UPDATE users SET totp_secret = 'S', totp_pending = 'P', totp_last_step = 9
                 WHERE id = ?1",
            )
            .bind(id)
            .execute(db.writer())
            .await
            .unwrap();
            sqlx::query("INSERT INTO recovery_codes (user_id, code_hash) VALUES (?1, 'h')")
                .bind(id)
                .execute(db.writer())
                .await
                .unwrap();
            sqlx::query(
                "INSERT INTO passkeys (id, user_id, credential_id, name, passkey)
                 VALUES (?1, ?2, ?3, 'Laptop', '{}')",
            )
            .bind(Uuid::now_v7())
            .bind(id)
            .bind(id.as_bytes().as_slice())
            .execute(db.writer())
            .await
            .unwrap();
        }
        assert!(reset_two_factor(&db, "nobody@example.com").await.is_err());
        reset_two_factor(&db, "ann@example.com").await.unwrap();

        let count = |sql: &'static str, id: Uuid| {
            let db = db.clone();
            async move {
                sqlx::query_scalar::<_, i64>(sql)
                    .bind(id)
                    .fetch_one(db.reader())
                    .await
                    .unwrap()
            }
        };
        let totp = "SELECT COUNT(*) FROM users WHERE id = ?1
                    AND (totp_secret IS NOT NULL OR totp_pending IS NOT NULL)";
        let codes = "SELECT COUNT(*) FROM recovery_codes WHERE user_id = ?1";
        let passkeys = "SELECT COUNT(*) FROM passkeys WHERE user_id = ?1";
        for sql in [totp, codes, passkeys] {
            assert_eq!(count(sql, ann).await, 0, "{sql}");
            // Nobody else's.
            assert_eq!(count(sql, bob).await, 1, "{sql}");
        }
    }
}
