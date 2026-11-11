//! The account activity (docs/M7B_DESIGN.md §5): what the superadmin, a
//! team's manager or the server's command line did to an account, which
//! its user sees in
//! Settings → Security → Account activity (`GET /me/activity`). Changes
//! of settings that reach everyone are not in it: they show on the
//! settings themselves.

use serde::{Deserialize, Serialize};
use utoipa::ToSchema;
use uuid::Uuid;

/// Who acted.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Actor {
    /// A superadmin, by their id.
    Superadmin(Uuid),
    /// A manager of a team, by their id: adding to the team, taking out of
    /// it, changing the team role.
    TeamManager(Uuid),
    /// `organizer superadmin …`, `organizer two-factor reset …`.
    CommandLine,
}

/// What was done to the account.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize, ToSchema, sqlx::Type)]
#[serde(rename_all = "snake_case")]
#[sqlx(type_name = "TEXT", rename_all = "snake_case")]
#[schema(as = ActivityAction)]
pub enum Action {
    /// The account was made (an invite of the superadmin's was used).
    Created,
    Disabled,
    Enabled,
    /// Made a superadmin.
    SuperadminGranted,
    /// No longer a superadmin.
    SuperadminRevoked,
    /// The storage limit changed (`storage_limit`, null: none).
    StorageLimit,
    /// The files folder changed.
    FilesFolder,
    /// Added to a team (`team`, `role`).
    TeamAdded,
    /// Taken out of a team (`team`).
    TeamRemoved,
    /// Their role in a team changed (`team`, `role`).
    TeamRole,
    /// Two-factor authentication was reset: the app, the recovery codes
    /// and the passkeys are gone, and every session ended.
    TwoFactorReset,
    /// The password was reset.
    PasswordReset,
    /// Their access to a shared drive changed (`drive`, `role`; null role:
    /// taken out).
    DriveAccess,
}

/// What an action was about.
#[derive(Clone, Debug, Default, Serialize, Deserialize, ToSchema)]
#[schema(as = ActivityDetail)]
pub struct Detail {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub team: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub drive: Option<String>,
    /// A role: in a team (`manager`, `member`) or on a drive.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub role: Option<String>,
    /// Bytes.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub storage_limit: Option<i64>,
}

/// Note an action on a user's account.
pub async fn log<'e>(
    db: impl sqlx::SqliteExecutor<'e>,
    user: Uuid,
    actor: Actor,
    action: Action,
    detail: Detail,
) -> sqlx::Result<()> {
    let (actor, actor_id) = match actor {
        Actor::Superadmin(id) => ("superadmin", Some(id)),
        Actor::TeamManager(id) => ("team_manager", Some(id)),
        Actor::CommandLine => ("command_line", None),
    };
    sqlx::query(
        "INSERT INTO activity (id, user_id, at, actor, actor_id, action, detail)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)",
    )
    .bind(Uuid::now_v7())
    .bind(user)
    .bind(crate::auth::now_iso())
    .bind(actor)
    .bind(actor_id)
    .bind(action)
    .bind(serde_json::to_string(&detail).expect("a detail serializes"))
    .execute(db)
    .await?;
    Ok(())
}

/// Who did it, as the user sees it.
#[derive(Serialize, ToSchema)]
pub struct ActivityActor {
    /// `superadmin`, `team_manager` (a manager of the team in `detail`) or
    /// `command_line` ("the server's command line").
    pub kind: String,
    /// The superadmin's or team manager's email; null for the command
    /// line, or someone removed since. Always present.
    #[schema(required = true)]
    pub email: Option<String>,
}

/// One entry of the account activity.
#[derive(Serialize, ToSchema)]
pub struct ActivityEntry {
    pub id: Uuid,
    pub at: String,
    pub actor: ActivityActor,
    pub action: Action,
    pub detail: Detail,
}

#[derive(sqlx::FromRow)]
struct Row {
    id: Uuid,
    at: String,
    actor: String,
    email: Option<String>,
    action: Action,
    detail: String,
}

/// A user's activity, the newest first: at most `limit`, before the entry
/// `before` (its id) when given.
pub async fn of(
    db: &crate::db::Db,
    user: Uuid,
    before: Option<Uuid>,
    limit: i64,
) -> sqlx::Result<Vec<ActivityEntry>> {
    let rows: Vec<Row> = sqlx::query_as(
        "SELECT a.id, a.at, a.actor, u.email, a.action, a.detail
         FROM activity a LEFT JOIN users u ON u.id = a.actor_id
         WHERE a.user_id = ?1 AND (?2 IS NULL OR a.id < ?2)
         ORDER BY a.id DESC LIMIT ?3",
    )
    .bind(user)
    .bind(before)
    .bind(limit)
    .fetch_all(db.reader())
    .await?;
    Ok(rows
        .into_iter()
        .map(|r| ActivityEntry {
            id: r.id,
            at: r.at,
            actor: ActivityActor {
                kind: r.actor,
                email: r.email,
            },
            action: r.action,
            detail: serde_json::from_str(&r.detail).unwrap_or_default(),
        })
        .collect())
}
