//! Roles (docs/M7B_DESIGN.md §1): one model for every drive and every item
//! in it. A **grant** gives a user or a team a role on an item (and what
//! is under it) or on a whole shared drive; what a user has on an item is
//! the highest role of every grant that reaches it, theirs and their
//! teams'.
//!
//! - My drive's user is the owner of everything in it, but for an item
//!   whose owner was changed (an `owner` grant on it, the nearest above
//!   counts): its new owner owns it, and My drive's user manages it.
//! - A shared drive has no owner: its managers (manager grants on the
//!   drive, and the managers of the drive's team) keep access to all of it.
//! - **Limited access** on a folder: from there down only the grants made
//!   on it or below count; everyone else does not see it at all. The owner
//!   and the drive's managers ("keepers") keep theirs.
//! - **Upload only**: the user sees the folder they were given and what
//!   they added themselves (`nodes.created_by`), nothing else.
//! - A grant on an item given by someone who is neither its owner nor a
//!   manager of its drive gives no more than its giver still has there
//!   (M6b's re-share rule), so taking theirs takes it from those they gave
//!   it to.
//!
//! Every question "what may this user do here" goes through [`resolve`]
//! (or [`Files::access`](super::Files::access), which uses it); the
//! listings use [`HIDDEN`] to leave out what a user does not see, and the
//! change feed [`sees`].

use super::FileError;
use super::shares::{Need, Perms};
use serde::{Deserialize, Serialize};
use std::future::Future;
use std::pin::Pin;
use utoipa::ToSchema;
use uuid::Uuid;

/// What a user or a team may do with an item or a drive, from the least
/// to the most.
#[derive(
    Clone,
    Copy,
    Debug,
    PartialEq,
    Eq,
    PartialOrd,
    Ord,
    Hash,
    Serialize,
    Deserialize,
    ToSchema,
    sqlx::Type,
)]
#[serde(rename_all = "snake_case")]
#[sqlx(type_name = "TEXT", rename_all = "snake_case")]
pub enum Role {
    /// Adds files and folders, and sees only what they added.
    UploadOnly,
    /// Reads and downloads.
    Viewer,
    /// Reads, writes, renames, moves and deletes (to the trash).
    Editor,
    /// An editor who also decides who has access, and the item's or the
    /// drive's settings.
    Manager,
    /// Everything, including deleting for good and handing over. Only on
    /// items of a My drive: a shared drive has no owner.
    Owner,
}

impl Role {
    pub fn as_str(self) -> &'static str {
        match self {
            Role::UploadOnly => "upload_only",
            Role::Viewer => "viewer",
            Role::Editor => "editor",
            Role::Manager => "manager",
            Role::Owner => "owner",
        }
    }

    /// Whether the role allows what an operation needs.
    pub fn allows(self, need: Need) -> bool {
        match need {
            // Upload only sees only what it may see (`resolve` leaves out
            // the rest): that much it may read.
            Need::Read => true,
            Need::Create => self != Role::Viewer,
            Need::Change | Need::Delete | Need::Link => self >= Role::Editor,
            Need::Share => self >= Role::Manager,
            Need::Own => self == Role::Owner,
        }
    }

    /// M6b's permissions for the role, which the files' `permissions` and
    /// the old shares API still show.
    pub fn perms(self) -> Perms {
        match self {
            Role::UploadOnly => Perms {
                create: true,
                ..Perms::default()
            },
            Role::Viewer => Perms::default(),
            Role::Editor => Perms {
                create: true,
                change: true,
                delete: true,
                share: false,
            },
            Role::Manager | Role::Owner => Perms::ALL,
        }
    }

    /// The role an M6b share's permissions became (§2): read only →
    /// viewer; with create, change or delete → editor; with re-share →
    /// manager.
    pub fn of_perms(perms: Perms) -> Role {
        if perms.share {
            Role::Manager
        } else if perms.create || perms.change || perms.delete {
            Role::Editor
        } else {
            Role::Viewer
        }
    }
}

/// A grant, as stored.
#[derive(Clone, Debug, sqlx::FromRow)]
pub struct GrantRow {
    pub id: Uuid,
    pub drive_id: Uuid,
    /// None: the whole drive.
    pub node_id: Option<Uuid>,
    pub user_id: Option<Uuid>,
    pub team_id: Option<Uuid>,
    pub role: Role,
    pub created_by: Option<Uuid>,
    pub created_at: String,
}

pub const GRANT_COLUMNS: &str =
    "g.id, g.drive_id, g.node_id, g.user_id, g.team_id, g.role, g.created_by, g.created_at";

/// A node on the way from an item up to its drive's root.
#[derive(Clone, Debug, sqlx::FromRow)]
pub struct Up {
    pub id: Uuid,
    pub parent_id: Option<Uuid>,
    pub drive_id: Uuid,
    pub limited: bool,
    pub created_by: Option<Uuid>,
}

/// The nodes from `?1` up to the root, `?1` first.
const CHAIN: &str = "WITH RECURSIVE up (id, parent_id, drive_id, limited, created_by, depth) AS (
       SELECT id, parent_id, drive_id, limited, created_by, 0 FROM nodes WHERE id = ?1
       UNION ALL
       SELECT n.id, n.parent_id, n.drive_id, n.limited, n.created_by, up.depth + 1
       FROM nodes n JOIN up ON n.id = up.parent_id
     )
     SELECT id, parent_id, drive_id, limited, created_by FROM up ORDER BY depth";

/// A node and the folders above it, the node first; empty when it is not
/// there.
pub async fn chain(conn: &mut sqlx::SqliteConnection, node: Uuid) -> sqlx::Result<Vec<Up>> {
    sqlx::query_as(CHAIN).bind(node).fetch_all(conn).await
}

/// What a drive is, for the roles.
#[derive(Clone, Copy, Debug, sqlx::FromRow)]
struct DriveFacts {
    /// My drive's user; None for a shared drive.
    user_id: Option<Uuid>,
    team_id: Option<Uuid>,
}

/// The teams of a user, and whether they manage each.
pub async fn teams_of(
    conn: &mut sqlx::SqliteConnection,
    user: Uuid,
) -> sqlx::Result<Vec<(Uuid, bool)>> {
    sqlx::query_as(
        "SELECT team_id, role = 'manager' FROM team_members WHERE user_id = ?1 ORDER BY joined_at",
    )
    .bind(user)
    .fetch_all(conn)
    .await
}

/// What a user has on an item.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Resolved {
    pub role: Role,
    /// The drive the item is in.
    pub drive: Uuid,
    /// The user has the drive itself (My drive's user, a member of a
    /// shared drive), not only this item through a grant on it.
    pub own: bool,
    /// The owner, or a manager of the drive: limited access does not
    /// hide anything from them.
    pub keeper: bool,
}

type BoxFuture<'a, T> = Pin<Box<dyn Future<Output = T> + Send + 'a>>;

/// What `user` has on a node: None when nothing reaches it, or when it is
/// hidden from them (limited access, upload only).
pub async fn resolve(
    conn: &mut sqlx::SqliteConnection,
    user: Uuid,
    node: Uuid,
) -> sqlx::Result<Option<Resolved>> {
    let chain = chain(&mut *conn, node).await?;
    resolve_chain(conn, user, chain, Vec::new()).await
}

/// [`resolve`] for a node as if it were under `parent` (where it was
/// before a move).
pub async fn resolve_under(
    conn: &mut sqlx::SqliteConnection,
    user: Uuid,
    node: Uuid,
    parent: Uuid,
) -> sqlx::Result<Option<Resolved>> {
    let mut own = chain(&mut *conn, node).await?;
    own.truncate(1);
    let Some(first) = own.first_mut() else {
        return Ok(None);
    };
    first.parent_id = Some(parent);
    own.extend(chain(&mut *conn, parent).await?);
    resolve_chain(conn, user, own, Vec::new()).await
}

/// Whether `user` sees a node.
pub async fn sees(conn: &mut sqlx::SqliteConnection, user: Uuid, node: Uuid) -> sqlx::Result<bool> {
    Ok(resolve(conn, user, node).await?.is_some())
}

fn resolve_chain(
    conn: &mut sqlx::SqliteConnection,
    user: Uuid,
    chain: Vec<Up>,
    followed: Vec<Uuid>,
) -> BoxFuture<'_, sqlx::Result<Option<Resolved>>> {
    Box::pin(async move {
        let Some(first) = chain.first().cloned() else {
            return Ok(None);
        };
        let drive_id = first.drive_id;
        let Some(drive) =
            sqlx::query_as::<_, DriveFacts>("SELECT user_id, team_id FROM drives WHERE id = ?1")
                .bind(drive_id)
                .fetch_optional(&mut *conn)
                .await?
        else {
            return Ok(None);
        };
        let teams = teams_of(&mut *conn, user).await?;
        let ids: Vec<Uuid> = chain.iter().map(|u| u.id).collect();
        let grants = grants_on(&mut *conn, drive_id, &ids, user).await?;
        let mine = |g: &GrantRow| {
            g.user_id == Some(user)
                || g.team_id
                    .is_some_and(|t| teams.iter().any(|(m, _)| *m == t))
        };

        // The owner: the nearest owner grant, else My drive's user.
        let owner = chain
            .iter()
            .find_map(|up| {
                grants
                    .iter()
                    .find(|g| g.node_id == Some(up.id) && g.role == Role::Owner)
                    .and_then(|g| g.user_id)
            })
            .or(drive.user_id);
        let drive_grants: Vec<&GrantRow> = grants
            .iter()
            .filter(|g| g.node_id.is_none() && mine(g))
            .collect();
        let manages_drive = drive.user_id == Some(user)
            || drive_grants.iter().any(|g| g.role >= Role::Manager)
            || drive
                .team_id
                .is_some_and(|t| teams.iter().any(|(m, manager)| *m == t && *manager));
        let own = drive.user_id == Some(user) || !drive_grants.is_empty() || manages_drive;
        if owner == Some(user) {
            return Ok(Some(Resolved {
                role: Role::Owner,
                drive: drive_id,
                own,
                keeper: true,
            }));
        }

        // The grants from the item up: a limited folder cuts what is above.
        let mut best: Option<Role> = None;
        let mut cut = false;
        for up in &chain {
            for g in grants
                .iter()
                .filter(|g| g.node_id == Some(up.id) && mine(g))
            {
                let role = effective(conn, g, &followed).await?;
                best = best.max(role);
            }
            if up.limited {
                cut = true;
                break;
            }
        }
        if !cut {
            best = best.max(drive_grants.iter().map(|g| g.role).max());
        }
        if manages_drive {
            best = best.max(Some(Role::Manager));
        }
        let Some(role) = best else {
            return Ok(None);
        };
        // Upload only: the folder given, and what they added.
        if role == Role::UploadOnly {
            let given = grants
                .iter()
                .any(|g| mine(g) && g.node_id == Some(first.id))
                || (first.parent_id.is_none() && !drive_grants.is_empty());
            if !(given || first.created_by == Some(user)) {
                return Ok(None);
            }
        }
        Ok(Some(Resolved {
            role,
            drive: drive_id,
            own,
            keeper: manages_drive,
        }))
    })
}

/// What an item grant gives: its role, cut to what its giver still has
/// there (a giver who is the owner or a manager of the drive has at least
/// manager, so nothing is cut). A drive grant is given by its managers or
/// the superadmin: never cut.
async fn effective(
    conn: &mut sqlx::SqliteConnection,
    grant: &GrantRow,
    followed: &[Uuid],
) -> sqlx::Result<Option<Role>> {
    // Not the nearest owner (that one was found above): it manages.
    let role = grant.role.min(Role::Manager);
    let (Some(node), Some(giver)) = (grant.node_id, grant.created_by) else {
        return Ok(Some(role));
    };
    if followed.contains(&grant.id) {
        // A loop of re-shares gives nothing more.
        return Ok(None);
    }
    let mut followed = followed.to_vec();
    followed.push(grant.id);
    let chain = chain(&mut *conn, node).await?;
    let theirs = resolve_chain(conn, giver, chain, followed).await?;
    Ok(theirs.map(|t| role.min(t.role)))
}

/// The grants on these nodes and on their drive that may count for a
/// user (not declined by them), and every owner grant on them.
async fn grants_on(
    conn: &mut sqlx::SqliteConnection,
    drive: Uuid,
    nodes: &[Uuid],
    user: Uuid,
) -> sqlx::Result<Vec<GrantRow>> {
    // The chain is short: its ids are bound one by one.
    let marks = (0..nodes.len())
        .map(|i| format!("?{}", i + 3))
        .collect::<Vec<_>>()
        .join(", ");
    let in_nodes = if nodes.is_empty() {
        String::from("0")
    } else {
        format!("g.node_id IN ({marks})")
    };
    let sql = format!(
        "SELECT {GRANT_COLUMNS} FROM grants g
         WHERE g.drive_id = ?1 AND (g.node_id IS NULL OR {in_nodes})
           AND NOT EXISTS (SELECT 1 FROM grant_declines d WHERE d.grant_id = g.id AND d.user_id = ?2)"
    );
    let mut query = sqlx::query_as(sqlx::AssertSqlSafe(sql))
        .bind(drive)
        .bind(user);
    for id in nodes {
        query = query.bind(*id);
    }
    query.fetch_all(conn).await
}

/// The highest role a user has on a drive itself: My drive's user is its
/// owner; a shared drive's, from its grants (theirs and their teams') and
/// its team's managers. None: not a drive of theirs.
pub async fn drive_role<'e>(
    db: impl sqlx::SqliteExecutor<'e>,
    user: Uuid,
    drive: Uuid,
) -> sqlx::Result<Option<Role>> {
    let roles: Vec<Role> =
        sqlx::query_scalar("SELECT role FROM drive_users WHERE drive_id = ?1 AND user_id = ?2")
            .bind(drive)
            .bind(user)
            .fetch_all(db)
            .await?;
    Ok(roles.into_iter().max())
}

/// Whether a child row (`c`) of a folder is hidden from the user `?U` who
/// lists the folder: a limited folder they do not keep and have no grant
/// on, or, when they may only upload there, what someone else added. The
/// fragment takes `{u}` (the user's placeholder), `{keeper}` and
/// `{upload_only}` (0 or 1) through [`hidden`].
pub fn hidden(alias: &str, user: &str, keeper: bool, upload_only: bool) -> String {
    let granted = format!(
        "EXISTS (SELECT 1 FROM grants g WHERE g.node_id = {alias}.id
           AND (g.user_id = {user}
                OR g.team_id IN (SELECT team_id FROM team_members WHERE user_id = {user}))
           AND NOT EXISTS (SELECT 1 FROM grant_declines d WHERE d.grant_id = g.id AND d.user_id = {user}))"
    );
    let mut parts = Vec::new();
    if !keeper {
        parts.push(format!("({alias}.limited = 1 AND NOT {granted})"));
    }
    if upload_only {
        parts.push(format!(
            "({alias}.created_by IS NOT {user} AND NOT {granted})"
        ));
    }
    if parts.is_empty() {
        "0".into()
    } else {
        format!("({})", parts.join(" OR "))
    }
}

/// The role on a child listed in a folder the user has `parent` on: the
/// folder's, unless the child is limited or has grants of its own (then
/// it is resolved on its own).
pub async fn child_role(
    db: &crate::db::Db,
    user: Uuid,
    parent: Role,
    child: Uuid,
) -> Result<Option<Role>, FileError> {
    let own: Option<i64> = sqlx::query_scalar(
        "SELECT 1 FROM nodes n WHERE n.id = ?1
           AND (n.limited = 1 OR EXISTS (SELECT 1 FROM grants g WHERE g.node_id = n.id))",
    )
    .bind(child)
    .fetch_optional(db.reader())
    .await?;
    if own.is_none() {
        return Ok(Some(parent));
    }
    let mut conn = db.reader().acquire().await?;
    Ok(resolve(&mut conn, user, child).await?.map(|r| r.role))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::files::shares::Need;

    /// Each role allows what the table of §1 says, and nothing more.
    #[test]
    fn each_role_allows_its_operations() {
        use Need::*;
        let table = [
            (
                Role::UploadOnly,
                [true, true, false, false, false, false, false],
            ),
            (
                Role::Viewer,
                [true, false, false, false, false, false, false],
            ),
            (Role::Editor, [true, true, true, true, true, false, false]),
            (Role::Manager, [true, true, true, true, true, true, false]),
            (Role::Owner, [true, true, true, true, true, true, true]),
        ];
        for (role, allowed) in table {
            for (need, ok) in [Read, Create, Change, Delete, Link, Share, Own]
                .into_iter()
                .zip(allowed)
            {
                assert_eq!(role.allows(need), ok, "{role:?} {need:?}");
            }
        }
        assert!(Role::UploadOnly < Role::Viewer && Role::Manager < Role::Owner);
    }

    /// A database at 0025, with M6b's shares (to a user, to a group, one
    /// declined), a group, M7a's drive members and every settings table:
    /// after the migrations, grants with the roles of §2, a team with the
    /// group's members, nodes added by their drive's user, and the
    /// settings as levels.
    #[tokio::test]
    async fn a_database_at_0025_becomes_grants_teams_and_settings() {
        let dir = std::env::temp_dir().join(format!("organizer-roles-{}", Uuid::now_v7()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("organizer.db");
        let (alice, bob, carol) = (Uuid::now_v7(), Uuid::now_v7(), Uuid::now_v7());
        let (drive, root, docs, d_root) = (
            Uuid::now_v7(),
            Uuid::now_v7(),
            Uuid::now_v7(),
            Uuid::now_v7(),
        );
        let (group, to_bob, to_group) = (Uuid::now_v7(), Uuid::now_v7(), Uuid::now_v7());
        {
            let options = sqlx::sqlite::SqliteConnectOptions::new()
                .filename(&path)
                .create_if_missing(true)
                .foreign_keys(true);
            let pool = sqlx::sqlite::SqlitePoolOptions::new()
                .max_connections(1)
                .connect_with(options)
                .await
                .unwrap();
            let mut before = sqlx::migrate!("./migrations");
            before.migrations = std::borrow::Cow::Owned(
                before
                    .migrations
                    .iter()
                    .filter(|m| m.version <= 25)
                    .cloned()
                    .collect(),
            );
            before.run(&pool).await.unwrap();
            let rows: Vec<(&str, Vec<Uuid>)> = vec![
                (
                    "INSERT INTO users (id, email, is_superadmin) VALUES (?1, 'alice@example.com', 1)",
                    vec![alice],
                ),
                (
                    "INSERT INTO users (id, email) VALUES (?1, 'bob@example.com')",
                    vec![bob],
                ),
                (
                    "INSERT INTO users (id, email) VALUES (?1, 'carol@example.com')",
                    vec![carol],
                ),
                (
                    "INSERT INTO drives (id, kind, name, created_by) VALUES (?1, 'shared', 'Team', ?2)",
                    vec![drive, alice],
                ),
                (
                    "INSERT INTO nodes (id, drive_id, parent_id, name, kind) VALUES (?1, ?2, NULL, '', 'folder')",
                    vec![root, alice],
                ),
                (
                    "INSERT INTO nodes (id, drive_id, parent_id, name, kind) VALUES (?1, ?2, ?3, 'Docs', 'folder')",
                    vec![docs, alice, root],
                ),
                (
                    "INSERT INTO nodes (id, drive_id, parent_id, name, kind) VALUES (?1, ?2, NULL, '', 'folder')",
                    vec![d_root, drive],
                ),
                (
                    "INSERT INTO drive_members (drive_id, user_id, manages) VALUES (?1, ?2, 1)",
                    vec![drive, bob],
                ),
                (
                    "INSERT INTO drive_members (drive_id, user_id, manages) VALUES (?1, ?2, 0)",
                    vec![drive, carol],
                ),
                (
                    "INSERT INTO groups (id, name) VALUES (?1, 'Friends')",
                    vec![group],
                ),
                (
                    "INSERT INTO group_members (group_id, user_id) VALUES (?1, ?2)",
                    vec![group, carol],
                ),
                (
                    "INSERT INTO shares (id, node_id, drive_id, user_id, created_by) VALUES (?1, ?2, ?3, ?4, ?3)",
                    vec![to_bob, docs, alice, bob],
                ),
                (
                    "INSERT INTO shares (id, node_id, drive_id, group_id, can_create, created_by)
                     VALUES (?1, ?2, ?3, ?4, 1, ?3)",
                    vec![to_group, docs, alice, group],
                ),
                (
                    "INSERT INTO share_declines (share_id, user_id) VALUES (?1, ?2)",
                    vec![to_bob, bob],
                ),
                (
                    "INSERT INTO drop_box_settings (user_id, pending_mode, read_contents) VALUES (?1, 'stay', 'always')",
                    vec![alice],
                ),
                (
                    "INSERT INTO retention (user_id, trash_days) VALUES (?1, 7)",
                    vec![alice],
                ),
                (
                    "INSERT INTO notification_settings (user_id, shared, review_notified_at)
                     VALUES (?1, 0, '2026-01-01T00:00:00.000Z')",
                    vec![alice],
                ),
                ("UPDATE retention_defaults SET trash_days = 60", vec![]),
            ];
            for (sql, binds) in rows {
                let mut query = sqlx::query(sql);
                for bind in binds {
                    query = query.bind(bind);
                }
                query.execute(&pool).await.unwrap();
            }
            pool.close().await;
        }

        let db = crate::db::open(path.to_str().unwrap()).await.unwrap();
        let mut conn = db.reader().acquire().await.unwrap();
        // Shares: read only → viewer (declined: gone for bob), with
        // create → editor, to the team that was the group.
        let grants: Vec<(Option<Uuid>, Option<Uuid>, Role)> = sqlx::query_as(
            "SELECT user_id, team_id, role FROM grants WHERE node_id = ?1 ORDER BY role",
        )
        .bind(docs)
        .fetch_all(&mut *conn)
        .await
        .unwrap();
        assert_eq!(
            grants,
            [
                (None, Some(group), Role::Editor),
                (Some(bob), None, Role::Viewer)
            ]
        );
        assert_eq!(
            resolve(&mut conn, carol, docs)
                .await
                .unwrap()
                .map(|r| r.role),
            Some(Role::Editor)
        );
        assert_eq!(
            resolve(&mut conn, bob, docs).await.unwrap(),
            None,
            "declined"
        );
        assert_eq!(
            resolve(&mut conn, alice, docs)
                .await
                .unwrap()
                .map(|r| r.role),
            Some(Role::Owner)
        );
        // Drive members: who managed it → manager, the others → editor.
        assert_eq!(
            drive_role(&mut *conn, bob, drive).await.unwrap(),
            Some(Role::Manager)
        );
        assert_eq!(
            drive_role(&mut *conn, carol, drive).await.unwrap(),
            Some(Role::Editor)
        );
        assert_eq!(
            drive_role(&mut *conn, alice, drive).await.unwrap(),
            None,
            "the superadmin who made it is not in it"
        );
        // The group is a team, its member a member, no manager of its own.
        let members: Vec<(Uuid, String)> =
            sqlx::query_as("SELECT user_id, role FROM team_members WHERE team_id = ?1")
                .bind(group)
                .fetch_all(&mut *conn)
                .await
                .unwrap();
        assert_eq!(members, [(carol, "member".to_string())]);
        // What exists was added by its drive's user.
        let by: Option<Uuid> = sqlx::query_scalar("SELECT created_by FROM nodes WHERE id = ?1")
            .bind(docs)
            .fetch_one(&mut *conn)
            .await
            .unwrap();
        assert_eq!(by, Some(alice));
        drop(conn);
        // The settings: each user's as user rows, the defaults as the
        // superadmin's.
        let none = crate::settings::Env::default();
        let get = |key: &'static str, user: Uuid| {
            let (db, none) = (db.clone(), none.clone());
            async move {
                crate::settings::resolve(&db, &none, key, crate::settings::For::user(user))
                    .await
                    .unwrap()
                    .value
            }
        };
        assert_eq!(get("drop_box.pending_mode", alice).await, "stay");
        assert_eq!(get("drop_box.pending_mode", bob).await, "move_pending");
        assert_eq!(get("retention.trash_days", alice).await, 7);
        assert_eq!(get("retention.trash_days", bob).await, 60);
        assert_eq!(get("notifications.shared", alice).await, false);
        assert_eq!(get("notifications.review", alice).await, true);
        let kept: Option<String> = sqlx::query_scalar(
            "SELECT review_notified_at FROM notification_settings WHERE user_id = ?1",
        )
        .bind(alice)
        .fetch_one(db.reader())
        .await
        .unwrap();
        assert_eq!(kept.as_deref(), Some("2026-01-01T00:00:00.000Z"));
        let failed: Vec<(String,)> = sqlx::query_as("PRAGMA foreign_key_check")
            .fetch_all(db.reader())
            .await
            .unwrap();
        assert!(failed.is_empty());
        db.close().await;
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn shares_become_roles() {
        assert_eq!(Role::of_perms(Perms::default()), Role::Viewer);
        let create = Perms {
            create: true,
            ..Perms::default()
        };
        assert_eq!(Role::of_perms(create), Role::Editor);
        assert_eq!(Role::of_perms(Perms::ALL), Role::Manager);
        assert_eq!(Role::of_perms(Role::Editor.perms()), Role::Editor);
    }
}
