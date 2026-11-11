//! Changing who has access (docs/M7B_DESIGN.md §1): grants on items and
//! drives, limited access, and handing an item over to a new owner. The
//! rules of what a grant gives are in `roles.rs`; these are the writes,
//! each in one transaction that also tells the change feed of every user
//! for whom something appeared or went:
//!
//! - a user who got a drive gets its root (`create`, with its name), one
//!   who lost it its root as deleted;
//! - a drive's user who now sees an item gets it and what is under it as
//!   created, one who no longer does gets it as deleted;
//! - the shares placed by those who lost an item leave their files
//!   (`settle_mounts`).
//!
//! A shared drive never loses its last manager.

use super::drives::{DriveKind, DriveRow};
use super::roles::{self, GRANT_COLUMNS, GrantRow, Role};
use super::{FileError, Files, Topic};
use uuid::Uuid;

/// Who a grant is to.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Grantee {
    User(Uuid),
    Team(Uuid),
}

/// What a user had or has, for the feed.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct Seen {
    /// Their role on the drive itself.
    drive: Option<Role>,
    /// Whether they see the item changed (the drive's root for a change on
    /// the drive).
    item: bool,
}

/// The users a change of access may concern: the drive's, the grantee's
/// (a team's members), and those who placed a share of the item.
async fn concerned(
    conn: &mut sqlx::SqliteConnection,
    drive: Uuid,
    node: Uuid,
    also: &[Uuid],
) -> sqlx::Result<Vec<Uuid>> {
    let mut users: Vec<Uuid> = super::drives::users_of(&mut *conn, drive)
        .await?
        .into_iter()
        .map(|(u, _)| u)
        .collect();
    let placed: Vec<Uuid> = sqlx::query_scalar(
        "WITH RECURSIVE up (id, parent_id) AS (
           SELECT id, parent_id FROM nodes WHERE id = ?1
           UNION ALL
           SELECT n.id, n.parent_id FROM nodes n JOIN up ON n.id = up.parent_id
         )
         SELECT DISTINCT m.user_id FROM share_mounts m JOIN up ON m.node_id = up.id",
    )
    .bind(node)
    .fetch_all(&mut *conn)
    .await?;
    for user in placed.into_iter().chain(also.iter().copied()) {
        if !users.contains(&user) {
            users.push(user);
        }
    }
    Ok(users)
}

/// The users a grantee stands for.
async fn members_of(conn: &mut sqlx::SqliteConnection, to: Grantee) -> sqlx::Result<Vec<Uuid>> {
    match to {
        Grantee::User(user) => Ok(vec![user]),
        Grantee::Team(team) => {
            sqlx::query_scalar("SELECT user_id FROM team_members WHERE team_id = ?1")
                .bind(team)
                .fetch_all(conn)
                .await
        }
    }
}

async fn seen_by(
    conn: &mut sqlx::SqliteConnection,
    users: &[Uuid],
    drive: Uuid,
    item: Uuid,
) -> sqlx::Result<Vec<(Uuid, Seen)>> {
    let mut out = Vec::with_capacity(users.len());
    for user in users {
        let drive_role = roles::drive_role(&mut *conn, *user, drive).await?;
        let item = roles::sees(&mut *conn, *user, item).await?;
        out.push((
            *user,
            Seen {
                drive: drive_role,
                item,
            },
        ));
    }
    Ok(out)
}

/// The drive's root and its name for the feed.
async fn root_of(
    conn: &mut sqlx::SqliteConnection,
    drive: Uuid,
) -> sqlx::Result<Option<(Uuid, i64)>> {
    sqlx::query_as("SELECT id, version FROM nodes WHERE drive_id = ?1 AND parent_id IS NULL")
        .bind(drive)
        .fetch_optional(conn)
        .await
}

/// A change of access, done in `change` between two looks at who sees
/// what: the feed rows for what appeared and went, and the check that a
/// shared drive keeps a manager.
async fn changing<F>(
    files: &Files,
    drive: Uuid,
    node: Option<Uuid>,
    also: &[Uuid],
    change: F,
) -> Result<(), FileError>
where
    F: for<'c> FnOnce(
        &'c mut sqlx::SqliteConnection,
    ) -> std::pin::Pin<
        Box<dyn std::future::Future<Output = Result<(), FileError>> + Send + 'c>,
    >,
{
    let row = DriveRow::get(files.db.reader(), drive)
        .await?
        .ok_or(FileError::NotFound)?;
    let mut tx = files.db.writer().begin().await?;
    // A drive that had a manager keeps one (one made without, by the
    // server itself, may get its members first).
    let was_managed = managed(&mut tx, &row).await?;
    let root = root_of(&mut tx, drive).await?;
    let Some(item) = node.or(root.map(|(id, _)| id)) else {
        // Not indexed yet: nothing to tell anyone.
        change(&mut tx).await?;
        if was_managed {
            still_managed(&mut tx, &row).await?;
        }
        tx.commit().await?;
        return Ok(());
    };
    let users = concerned(&mut tx, drive, item, also).await?;
    let before = seen_by(&mut tx, &users, drive, item).await?;
    change(&mut tx).await?;
    if was_managed {
        still_managed(&mut tx, &row).await?;
    }
    let after = seen_by(&mut tx, &users, drive, item).await?;
    let name = super::drives::display_name(&row);
    for ((user, was), (_, now)) in before.iter().zip(&after) {
        match (was.drive, now.drive, root) {
            (None, Some(_), Some((root, version))) => {
                super::drives::root_row(
                    &mut tx,
                    *user,
                    drive,
                    root,
                    "create",
                    Some(version),
                    Some(&name),
                )
                .await?;
                continue;
            }
            (Some(_), None, Some((root, _))) => {
                super::drives::root_row(&mut tx, *user, drive, root, "delete", None, None).await?;
                continue;
            }
            (Some(a), Some(b), Some((root, version)))
                if node.is_none() && a != b && (a == Role::UploadOnly || b == Role::UploadOnly) =>
            {
                // What they see of the whole drive changed: it goes and comes
                // back, so the apps list it again.
                super::drives::root_row(&mut tx, *user, drive, root, "delete", None, None).await?;
                super::drives::root_row(
                    &mut tx,
                    *user,
                    drive,
                    root,
                    "create",
                    Some(version),
                    Some(&name),
                )
                .await?;
                continue;
            }
            _ => {}
        }
        if now.drive.is_none() || node.is_none() {
            continue;
        }
        match (was.item, now.item) {
            (false, true) => super::appear_in_drive(&mut tx, *user, drive, item).await?,
            (true, false) => {
                super::feed_row(
                    &mut tx, *user, drive, item, "delete", None, None, None, None,
                )
                .await?
            }
            _ => {}
        }
    }
    tx.commit().await?;
    for (user, _) in &after {
        files.announce(*user, Topic::Changes);
    }
    files.settle_mounts().await
}

/// Whether a shared drive has a manager (My drive always has its user).
async fn managed(conn: &mut sqlx::SqliteConnection, drive: &DriveRow) -> sqlx::Result<bool> {
    if drive.kind != DriveKind::Shared {
        return Ok(true);
    }
    sqlx::query_scalar(
        "SELECT EXISTS (SELECT 1 FROM drive_users WHERE drive_id = ?1 AND role = 'manager')",
    )
    .bind(drive.id)
    .fetch_one(conn)
    .await
}

/// A shared drive keeps a manager: refused (409) otherwise.
async fn still_managed(
    conn: &mut sqlx::SqliteConnection,
    drive: &DriveRow,
) -> Result<(), FileError> {
    if managed(conn, drive).await? {
        Ok(())
    } else {
        Err(FileError::Conflict(
            "A shared drive keeps a manager: name another first".into(),
        ))
    }
}

/// A grant by id.
pub async fn grant_row<'e>(
    db: impl sqlx::SqliteExecutor<'e>,
    id: Uuid,
) -> sqlx::Result<Option<GrantRow>> {
    sqlx::query_as(sqlx::AssertSqlSafe(format!(
        "SELECT {GRANT_COLUMNS} FROM grants g WHERE g.id = ?1"
    )))
    .bind(id)
    .fetch_optional(db)
    .await
}

/// The grants on an item (`node`), or on a drive itself (`node` None),
/// the oldest first.
pub async fn grants_of<'e>(
    db: impl sqlx::SqliteExecutor<'e>,
    drive: Uuid,
    node: Option<Uuid>,
) -> sqlx::Result<Vec<GrantRow>> {
    sqlx::query_as(sqlx::AssertSqlSafe(format!(
        "SELECT {GRANT_COLUMNS} FROM grants g
         WHERE g.drive_id = ?1 AND g.node_id IS ?2
         ORDER BY g.created_at, g.id"
    )))
    .bind(drive)
    .bind(node)
    .fetch_all(db)
    .await
}

impl Files {
    /// Give a user or a team a role on an item of a drive, or on a whole
    /// shared drive (`node` None). Never `owner`: that is
    /// [`Files::change_owner`]. A grant they have there already is a
    /// conflict.
    pub async fn grant(
        &self,
        drive: Uuid,
        node: Option<Uuid>,
        to: Grantee,
        role: Role,
        by: Uuid,
    ) -> Result<GrantRow, FileError> {
        if role == Role::Owner {
            return Err(FileError::Invalid(
                "The owner is changed by handing the item over".into(),
            ));
        }
        let row = DriveRow::get(self.db.reader(), drive)
            .await?
            .ok_or(FileError::NotFound)?;
        match node {
            None if row.kind == DriveKind::My => {
                return Err(FileError::Invalid(
                    "Give access to the folders and files of My drive, not the whole drive".into(),
                ));
            }
            Some(id) => {
                let node = self.node(drive, id).await?;
                if node.parent_id.is_none() {
                    return Err(FileError::Invalid(
                        "Give access to the files and folders in it, or to the whole drive".into(),
                    ));
                }
                // Its members get its keys: the encrypted folder is shared whole.
                if node.e2e_root.is_some_and(|root| root != id) {
                    return Err(FileError::Invalid(
                        "Share the encrypted folder itself, not what is in it".into(),
                    ));
                }
            }
            None => {}
        }
        let (user, team) = match to {
            Grantee::User(user) => {
                let enabled: Option<i64> =
                    sqlx::query_scalar("SELECT 1 FROM users WHERE id = ?1 AND disabled_at IS NULL")
                        .bind(user)
                        .fetch_optional(self.db.reader())
                        .await?;
                enabled.ok_or(FileError::NotFound)?;
                (Some(user), None)
            }
            Grantee::Team(team) => {
                let exists: Option<i64> = sqlx::query_scalar("SELECT 1 FROM teams WHERE id = ?1")
                    .bind(team)
                    .fetch_optional(self.db.reader())
                    .await?;
                exists.ok_or(FileError::NotFound)?;
                (None, Some(team))
            }
        };
        let id = Uuid::now_v7();
        let also = {
            let mut conn = self.db.reader().acquire().await?;
            members_of(&mut conn, to).await?
        };
        changing(self, drive, node, &also, move |tx| {
            Box::pin(async move {
                let made = sqlx::query(
                    "INSERT INTO grants (id, drive_id, node_id, user_id, team_id, role, created_by,
                       created_at)
                     VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8)",
                )
                .bind(id)
                .bind(drive)
                .bind(node)
                .bind(user)
                .bind(team)
                .bind(role)
                .bind(by)
                .bind(crate::auth::now_iso())
                .execute(&mut *tx)
                .await;
                match made {
                    Ok(_) => Ok(()),
                    Err(sqlx::Error::Database(e)) if e.is_unique_violation() => {
                        Err(FileError::Conflict(
                            "They have access here already: change their role".into(),
                        ))
                    }
                    Err(e) => Err(e.into()),
                }
            })
        })
        .await?;
        grant_row(self.db.reader(), id)
            .await?
            .ok_or(FileError::NotFound)
    }

    /// Change a grant's role (never to or from `owner`).
    pub async fn change_grant(&self, id: Uuid, role: Role) -> Result<GrantRow, FileError> {
        let grant = grant_row(self.db.reader(), id)
            .await?
            .ok_or(FileError::NotFound)?;
        if role == Role::Owner || grant.role == Role::Owner {
            return Err(FileError::Invalid(
                "The owner is changed by handing the item over".into(),
            ));
        }
        let also = self.grantee_users(&grant).await?;
        changing(self, grant.drive_id, grant.node_id, &also, move |tx| {
            Box::pin(async move {
                sqlx::query("UPDATE grants SET role = ?1 WHERE id = ?2")
                    .bind(role)
                    .bind(id)
                    .execute(&mut *tx)
                    .await?;
                Ok(())
            })
        })
        .await?;
        grant_row(self.db.reader(), id)
            .await?
            .ok_or(FileError::NotFound)
    }

    /// Take a grant away (never the owner's).
    pub async fn revoke(&self, id: Uuid) -> Result<(), FileError> {
        let grant = grant_row(self.db.reader(), id)
            .await?
            .ok_or(FileError::NotFound)?;
        if grant.role == Role::Owner {
            return Err(FileError::Invalid(
                "The owner cannot be taken away: hand the item over instead".into(),
            ));
        }
        let also = self.grantee_users(&grant).await?;
        changing(self, grant.drive_id, grant.node_id, &also, move |tx| {
            Box::pin(async move {
                sqlx::query("DELETE FROM grants WHERE id = ?1")
                    .bind(id)
                    .execute(&mut *tx)
                    .await?;
                Ok(())
            })
        })
        .await
    }

    async fn grantee_users(&self, grant: &GrantRow) -> Result<Vec<Uuid>, FileError> {
        let to = match (grant.user_id, grant.team_id) {
            (Some(user), _) => Grantee::User(user),
            (_, Some(team)) => Grantee::Team(team),
            _ => return Ok(Vec::new()),
        };
        let mut conn = self.db.reader().acquire().await?;
        Ok(members_of(&mut conn, to).await?)
    }

    /// Give a shared drive to a team (its managers manage it, its members
    /// get the team's grant, editor unless one is there), or to none.
    pub async fn set_drive_team(&self, drive: Uuid, team: Option<Uuid>) -> Result<(), FileError> {
        let row = DriveRow::get(self.db.reader(), drive)
            .await?
            .filter(|d| d.kind == DriveKind::Shared)
            .ok_or(FileError::NotFound)?;
        let mut also = Vec::new();
        {
            let mut conn = self.db.reader().acquire().await?;
            for team in [row_team(&mut conn, drive).await?, team]
                .into_iter()
                .flatten()
            {
                also.extend(members_of(&mut conn, Grantee::Team(team)).await?);
            }
        }
        if let Some(team) = team {
            let exists: Option<i64> = sqlx::query_scalar("SELECT 1 FROM teams WHERE id = ?1")
                .bind(team)
                .fetch_optional(self.db.reader())
                .await?;
            exists.ok_or_else(|| FileError::Invalid("No such team".into()))?;
        }
        let by = row.created_by;
        changing(self, drive, None, &also, move |tx| {
            Box::pin(async move {
                sqlx::query("UPDATE drives SET team_id = ?1 WHERE id = ?2")
                    .bind(team)
                    .bind(drive)
                    .execute(&mut *tx)
                    .await?;
                if let Some(team) = team {
                    sqlx::query(
                        "INSERT OR IGNORE INTO grants (id, drive_id, node_id, team_id, role,
                           created_by, created_at)
                         VALUES (?1, ?2, NULL, ?3, 'editor', ?4, ?5)",
                    )
                    .bind(Uuid::now_v7())
                    .bind(drive)
                    .bind(team)
                    .bind(by)
                    .bind(crate::auth::now_iso())
                    .execute(&mut *tx)
                    .await?;
                }
                Ok(())
            })
        })
        .await
    }

    /// M7a's drive members, as grants (docs/M7B_DESIGN.md §2): a user's
    /// own grant on a shared drive, manager when they manage it, else
    /// editor; made, or changed. Answers whether it changed anything.
    pub async fn set_member(
        &self,
        drive: Uuid,
        user: Uuid,
        manages: bool,
    ) -> Result<bool, FileError> {
        let role = if manages { Role::Manager } else { Role::Editor };
        let own = grants_of(self.db.reader(), drive, None)
            .await?
            .into_iter()
            .find(|g| g.user_id == Some(user));
        match own {
            Some(grant) if grant.role == role => Ok(false),
            Some(grant) => {
                self.change_grant(grant.id, role).await?;
                Ok(true)
            }
            None => {
                let by = DriveRow::get(self.db.reader(), drive)
                    .await?
                    .and_then(|d| d.created_by)
                    .unwrap_or(user);
                self.grant(drive, None, Grantee::User(user), role, by)
                    .await?;
                Ok(true)
            }
        }
    }

    /// Take a user's own grant on a drive away (not found without one).
    pub async fn remove_member(&self, drive: Uuid, user: Uuid) -> Result<(), FileError> {
        let own = grants_of(self.db.reader(), drive, None)
            .await?
            .into_iter()
            .find(|g| g.user_id == Some(user))
            .ok_or(FileError::NotFound)?;
        self.revoke(own.id).await
    }

    /// Turn limited access on a folder on or off.
    pub async fn set_limited(&self, node: Uuid, limited: bool) -> Result<(), FileError> {
        let drive = self.owner_of(node).await?;
        let found = self.node(drive, node).await?;
        if found.kind != super::Kind::Folder || found.parent_id.is_none() {
            return Err(FileError::Invalid(
                "Limited access is for the folders in a drive".into(),
            ));
        }
        changing(self, drive, Some(node), &[], move |tx| {
            Box::pin(async move {
                sqlx::query("UPDATE nodes SET limited = ?1 WHERE id = ?2")
                    .bind(limited)
                    .bind(node)
                    .execute(&mut *tx)
                    .await?;
                Ok(())
            })
        })
        .await?;
        // The shares with other servers above it see it come or go.
        crate::federation::outgoing::log_access(self, node).await?;
        Ok(())
    }

    /// Hand an item of a My drive over to another user: they become its
    /// owner, and the previous owner its manager. The item stays where it
    /// is (docs/M7B_DESIGN.md §10.4).
    pub async fn change_owner(
        &self,
        node: Uuid,
        new_owner: Uuid,
        previous: Uuid,
    ) -> Result<(), FileError> {
        let drive = self.owner_of(node).await?;
        let row = DriveRow::get(self.db.reader(), drive)
            .await?
            .ok_or(FileError::NotFound)?;
        if row.kind != DriveKind::My {
            return Err(FileError::Invalid(
                "A shared drive has no owner: its managers decide".into(),
            ));
        }
        let found = self.node(drive, node).await?;
        if found.parent_id.is_none() {
            return Err(FileError::Invalid(
                "My drive itself cannot be handed over".into(),
            ));
        }
        if new_owner == previous {
            return Err(FileError::Invalid("They own it already".into()));
        }
        let enabled: Option<i64> =
            sqlx::query_scalar("SELECT 1 FROM users WHERE id = ?1 AND disabled_at IS NULL")
                .bind(new_owner)
                .fetch_optional(self.db.reader())
                .await?;
        enabled.ok_or(FileError::NotFound)?;
        let drive_user = row.user_id;
        changing(self, drive, Some(node), &[new_owner, previous], move |tx| {
            Box::pin(async move {
                // Their own grants there give way to what they are now.
                sqlx::query(
                    "DELETE FROM grants WHERE node_id = ?1 AND (role = 'owner' OR user_id IN (?2, ?3))",
                )
                .bind(node)
                .bind(new_owner)
                .bind(previous)
                .execute(&mut *tx)
                .await?;
                let now = crate::auth::now_iso();
                // Back to My drive's user: they own it as everything else.
                if Some(new_owner) != drive_user {
                    sqlx::query(
                        "INSERT INTO grants (id, drive_id, node_id, user_id, role, created_by, created_at)
                         VALUES (?1, ?2, ?3, ?4, 'owner', ?5, ?6)",
                    )
                    .bind(Uuid::now_v7())
                    .bind(drive)
                    .bind(node)
                    .bind(new_owner)
                    .bind(previous)
                    .bind(&now)
                    .execute(&mut *tx)
                    .await?;
                }
                sqlx::query(
                    "INSERT INTO grants (id, drive_id, node_id, user_id, role, created_by, created_at)
                     VALUES (?1, ?2, ?3, ?4, 'manager', ?5, ?6)",
                )
                .bind(Uuid::now_v7())
                .bind(drive)
                .bind(node)
                .bind(previous)
                .bind(new_owner)
                .bind(&now)
                .execute(&mut *tx)
                .await?;
                Ok(())
            })
        })
        .await
    }

    /// The user who owns an item: the nearest owner grant, else My drive's
    /// user; None in a shared drive.
    pub async fn owner_user(&self, node: Uuid) -> Result<Option<Uuid>, FileError> {
        let found: Option<Uuid> = sqlx::query_scalar(
            "WITH RECURSIVE up (id, parent_id, depth) AS (
               SELECT id, parent_id, 0 FROM nodes WHERE id = ?1
               UNION ALL
               SELECT n.id, n.parent_id, up.depth + 1 FROM nodes n JOIN up ON n.id = up.parent_id
             )
             SELECT g.user_id FROM grants g JOIN up ON g.node_id = up.id
             WHERE g.role = 'owner' ORDER BY up.depth LIMIT 1",
        )
        .bind(node)
        .fetch_optional(self.db.reader())
        .await?;
        if found.is_some() {
            return Ok(found);
        }
        let drive = self.owner_of(node).await?;
        Ok(
            sqlx::query_scalar("SELECT user_id FROM drives WHERE id = ?1")
                .bind(drive)
                .fetch_optional(self.db.reader())
                .await?
                .flatten(),
        )
    }
}

/// A user's role in a team.
#[derive(
    Clone,
    Copy,
    Debug,
    PartialEq,
    Eq,
    serde::Serialize,
    serde::Deserialize,
    utoipa::ToSchema,
    sqlx::Type,
)]
#[serde(rename_all = "snake_case")]
#[sqlx(type_name = "TEXT", rename_all = "snake_case")]
pub enum TeamRole {
    /// Adds and removes members and managers, makes the team's shared
    /// drives, sets the team's settings; manages the team's drives.
    Manager,
    Member,
}

impl Files {
    /// Add a user to a team, change their role in it, or take them out of
    /// it (`role` None). They get or lose the drives the team has (their
    /// feed gets those drives' roots), and the shares placed through the
    /// team leave their files. A team drive keeps a manager.
    pub async fn set_team_member(
        &self,
        team: Uuid,
        user: Uuid,
        role: Option<TeamRole>,
    ) -> Result<(), FileError> {
        let mut tx = self.db.writer().begin().await?;
        let before = drive_roles(&mut tx, user).await?;
        match role {
            Some(role) => {
                sqlx::query(
                    "INSERT INTO team_members (team_id, user_id, role, joined_at)
                     VALUES (?1, ?2, ?3, ?4)
                     ON CONFLICT (team_id, user_id) DO UPDATE SET role = ?3",
                )
                .bind(team)
                .bind(user)
                .bind(role)
                .bind(crate::auth::now_iso())
                .execute(&mut *tx)
                .await?;
            }
            None => {
                let gone =
                    sqlx::query("DELETE FROM team_members WHERE team_id = ?1 AND user_id = ?2")
                        .bind(team)
                        .bind(user)
                        .execute(&mut *tx)
                        .await?
                        .rows_affected();
                if gone == 0 {
                    return Err(FileError::NotFound);
                }
            }
        }
        let after = drive_roles(&mut tx, user).await?;
        // The drives they managed keep a manager.
        for (drive, was) in &before {
            if *was >= Role::Manager
                && after.iter().all(|(d, r)| d != drive || *r < Role::Manager)
                && let Some(row) = DriveRow::get(&mut *tx, *drive).await?
            {
                still_managed(&mut tx, &row).await?;
            }
        }
        for (drive, _) in &after {
            if before.iter().any(|(d, _)| d == drive) {
                continue;
            }
            let root = root_of(&mut tx, *drive).await?;
            let row = DriveRow::get(&mut *tx, *drive).await?;
            if let (Some((root, version)), Some(row)) = (root, row) {
                let name = super::drives::display_name(&row);
                super::drives::root_row(
                    &mut tx,
                    user,
                    *drive,
                    root,
                    "create",
                    Some(version),
                    Some(&name),
                )
                .await?;
            }
        }
        for (drive, _) in &before {
            if after.iter().any(|(d, _)| d == drive) {
                continue;
            }
            if let Some((root, _)) = root_of(&mut tx, *drive).await? {
                super::drives::root_row(&mut tx, user, *drive, root, "delete", None, None).await?;
            }
        }
        tx.commit().await?;
        self.announce(user, Topic::Changes);
        self.settle_mounts().await
    }
}

/// The team a drive belongs to.
async fn row_team(conn: &mut sqlx::SqliteConnection, drive: Uuid) -> sqlx::Result<Option<Uuid>> {
    Ok(
        sqlx::query_scalar("SELECT team_id FROM drives WHERE id = ?1")
            .bind(drive)
            .fetch_optional(conn)
            .await?
            .flatten(),
    )
}

/// The shared drives a user has, with their highest role on each.
async fn drive_roles(
    conn: &mut sqlx::SqliteConnection,
    user: Uuid,
) -> sqlx::Result<Vec<(Uuid, Role)>> {
    let rows: Vec<(Uuid, Role)> = sqlx::query_as(
        "SELECT u.drive_id, u.role FROM drive_users u JOIN drives d ON d.id = u.drive_id
         WHERE u.user_id = ?1 AND d.kind = 'shared'",
    )
    .bind(user)
    .fetch_all(conn)
    .await?;
    let mut out: Vec<(Uuid, Role)> = Vec::new();
    for (drive, role) in rows {
        match out.iter_mut().find(|(d, _)| *d == drive) {
            Some(found) => found.1 = found.1.max(role),
            None => out.push((drive, role)),
        }
    }
    Ok(out)
}
