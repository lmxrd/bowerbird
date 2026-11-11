//! Shares with people and groups, and the recipients' placed shares
//! (docs/M6B_DESIGN.md §1 and §2).
//!
//! A share gives a user, or the members of a group, read access to a file
//! or folder of its owner's tree and what is under it, and each of create,
//! change, delete and re-share when it says so. A share made by someone
//! with re-share (not the owner) never gives more than they have: it is
//! cut to their own access whenever it is used, so taking theirs away
//! takes it from the people they shared with too.
//!
//! A recipient may place a share in their tree: a **mount**, a row of
//! `share_mounts` that stands for the owner's node under one of the
//! recipient's own folders, with their name for it. Mounts are not on the
//! recipient's disk: listing one lists the owner's folder, and everything
//! done inside goes to the owner's tree (its lock, journal and change feed)
//! after a permission check (`Files::access`). The owner's change rows are
//! copied to the feed of every recipient who placed a share they are in
//! (`fan_out`), so the apps and the desktop sync follow it.
//!
//! No loops: a mount's parent is always a folder of the recipient's own
//! tree, never one inside a mount, and a recipient's mounts never nest (a
//! node is placed at most once, never above or below another placed one).
//! Listing the owner's folders never shows the owner's own mounts, so a
//! share of a folder that holds a mount shares only the owner's files.

use super::{FileError, Files, Kind, NODE_COLUMNS, Node, names};
use serde::{Deserialize, Serialize};
use std::sync::Arc;
use utoipa::ToSchema;
use uuid::Uuid;

/// What a share gives besides reading, which it always gives.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
pub struct Perms {
    /// Upload files and make folders.
    #[serde(default)]
    pub create: bool,
    /// Replace files' contents, rename and move.
    #[serde(default)]
    pub change: bool,
    /// Delete (to the owner's trash).
    #[serde(default)]
    pub delete: bool,
    /// Share with others, never giving more than this.
    #[serde(default)]
    pub share: bool,
}

impl Perms {
    /// The owner's.
    pub const ALL: Perms = Perms {
        create: true,
        change: true,
        delete: true,
        share: true,
    };

    pub fn or(self, other: Perms) -> Perms {
        Perms {
            create: self.create || other.create,
            change: self.change || other.change,
            delete: self.delete || other.delete,
            share: self.share || other.share,
        }
    }

    pub fn and(self, other: Perms) -> Perms {
        Perms {
            create: self.create && other.create,
            change: self.change && other.change,
            delete: self.delete && other.delete,
            share: self.share && other.share,
        }
    }

    /// Whether these give at least everything `other` gives.
    pub fn covers(self, other: Perms) -> bool {
        self.or(other) == self
    }
}

/// What an operation needs.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Need {
    Read,
    /// Add files and folders (upload only may).
    Create,
    Change,
    Delete,
    /// Make a public link (an editor or above).
    Link,
    /// Decide who has access (a manager or above).
    Share,
    /// Delete for good, hand over (the owner; on a shared drive, which
    /// has no owner, its managers: see `Access::require`).
    Own,
}

/// A share placed in a recipient's tree.
#[derive(Clone, Debug, sqlx::FromRow)]
pub struct Mount {
    pub id: Uuid,
    pub user_id: Uuid,
    /// The owner's file or folder.
    pub node_id: Uuid,
    /// A folder of the recipient's own tree.
    pub parent_id: Uuid,
    /// The recipient's name for it.
    pub name: String,
}

const MOUNT_COLUMNS: &str = "m.id, m.user_id, m.node_id, m.parent_id, m.name";

/// What a user may do with a node: theirs, or someone else's through
/// shares.
#[derive(Clone, Debug)]
pub struct Access {
    /// As the owner's tree has it.
    pub node: Node,
    /// The drive whose tree it is in: the operations run there.
    pub owner: Uuid,
    /// In a drive of the user's (My drive, or a shared drive they are a
    /// member of), not reached through a grant on the item alone.
    pub own: bool,
    /// What they may do with it (docs/M7B_DESIGN.md §1).
    pub role: super::roles::Role,
    /// The owner, or a manager of the drive: nothing in it is hidden from
    /// them.
    pub keeper: bool,
    /// The node is the user's placed share (the mount's root).
    pub mount: Option<Mount>,
    /// In a My drive (it has an owner), not a shared drive.
    pub my_drive: bool,
}

impl Access {
    /// The node as the user sees it: a placed share is where they put it,
    /// under their name.
    pub fn view(&self) -> Node {
        let mut node = self.node.clone();
        if let Some(mount) = &self.mount {
            node.parent_id = Some(mount.parent_id);
            node.name = mount.name.clone();
        }
        node
    }

    /// M6b's permissions of the role.
    pub fn perms(&self) -> Perms {
        self.role.perms()
    }

    /// Whether the access gives what is needed. What only the owner may
    /// do, a shared drive's managers do (it has no owner).
    pub fn allows(&self, need: Need) -> bool {
        use super::roles::Role;
        self.role.allows(need)
            || (need == Need::Own && self.role == Role::Manager && self.keeper && !self.my_drive)
    }

    /// Refused (403) unless the access gives what is needed.
    pub fn require(&self, need: Need) -> Result<(), FileError> {
        if self.allows(need) {
            return Ok(());
        }
        let what = match need {
            Need::Read => "read",
            Need::Create => "add to",
            Need::Change => "change",
            Need::Delete => "delete",
            Need::Link => "make a link to",
            Need::Share => "share",
            Need::Own => "do this to",
        };
        Err(FileError::Forbidden(format!(
            "You may not {what} {}: your role ({}) does not allow it",
            self.node.name,
            self.role.as_str().replace('_', " ")
        )))
    }
}

/// The nodes from `?1` up to the root, `?1` included, as `up (id, parent_id)`.
const UP: &str = "WITH RECURSIVE up (id, parent_id) AS (
       SELECT id, parent_id FROM nodes WHERE id = ?1
       UNION ALL
       SELECT n.id, n.parent_id FROM nodes n JOIN up ON n.id = up.parent_id
     )";

/// The nodes under `?1`, without it, as `down (id)`.
const DOWN: &str = "WITH RECURSIVE down (id) AS (
       SELECT id FROM nodes WHERE parent_id = ?1
       UNION ALL
       SELECT n.id FROM nodes n JOIN down ON n.parent_id = down.id
     )";

impl Files {
    /// The drive a node is in.
    pub async fn owner_of(&self, id: Uuid) -> Result<Uuid, FileError> {
        sqlx::query_scalar("SELECT drive_id FROM nodes WHERE id = ?1")
            .bind(id)
            .fetch_optional(self.db.reader())
            .await?
            .ok_or(FileError::NotFound)
    }

    /// What a user may do with a node: in a drive of theirs (My drive, or
    /// a shared drive they are a member of), what their membership gives;
    /// elsewhere, what the shares give. A node the user has neither of is
    /// not found, as if it did not exist.
    pub async fn access(&self, user: Uuid, id: Uuid) -> Result<Access, FileError> {
        let owner = self.owner_of(id).await?;
        let node = self.node(owner, id).await?;
        let resolved = {
            let mut conn = self.db.reader().acquire().await?;
            super::roles::resolve(&mut conn, user, id).await?
        }
        .ok_or(FileError::NotFound)?;
        let mount = if resolved.own {
            None
        } else {
            self.mount_of(user, id).await?
        };
        let kind: Option<super::drives::DriveKind> =
            sqlx::query_scalar("SELECT kind FROM drives WHERE id = ?1")
                .bind(owner)
                .fetch_optional(self.db.reader())
                .await?;
        let mut node = node;
        node.my_role = Some(resolved.role);
        // A share from another server: from where, and followed sooner
        // while someone has it open.
        if kind == Some(super::drives::DriveKind::Remote) {
            node.remote = self.remote_host(owner).await?;
            self.federation().opened(owner);
        }
        Ok(Access {
            node,
            owner,
            own: resolved.own,
            role: resolved.role,
            keeper: resolved.keeper,
            mount,
            my_drive: kind == Some(super::drives::DriveKind::My),
        })
    }

    /// The other server a drive is shared from (a remote drive's); None
    /// for this server's drives.
    pub async fn remote_host(&self, drive: Uuid) -> Result<Option<String>, FileError> {
        Ok(
            sqlx::query_scalar("SELECT host FROM ocm_received WHERE drive_id = ?1")
                .bind(drive)
                .fetch_optional(self.db.reader())
                .await?,
        )
    }

    /// [`Files::access`], refused unless it gives what is needed.
    pub async fn access_for(&self, user: Uuid, id: Uuid, need: Need) -> Result<Access, FileError> {
        let access = self.access(user, id).await?;
        access.require(need)?;
        Ok(access)
    }

    /// The user's placed share of a node, if they placed it.
    pub async fn mount_of(&self, user: Uuid, node_id: Uuid) -> Result<Option<Mount>, FileError> {
        Ok(sqlx::query_as(sqlx::AssertSqlSafe(format!(
            "SELECT {MOUNT_COLUMNS} FROM share_mounts m WHERE m.user_id = ?1 AND m.node_id = ?2"
        )))
        .bind(user)
        .bind(node_id)
        .fetch_optional(self.db.reader())
        .await?)
    }

    /// The user's placed share that a node is in (or is), if any.
    pub async fn mount_over(&self, user: Uuid, node_id: Uuid) -> Result<Option<Mount>, FileError> {
        Ok(sqlx::query_as(sqlx::AssertSqlSafe(format!(
            "{UP}
             SELECT {MOUNT_COLUMNS} FROM share_mounts m JOIN up ON m.node_id = up.id
             WHERE m.user_id = ?2 LIMIT 1"
        )))
        .bind(node_id)
        .bind(user)
        .fetch_optional(self.db.reader())
        .await?)
    }

    /// A child of a folder by name, as the user sees it: a node of the
    /// folder, or (in the user's own folder) a share they placed there.
    pub async fn child_named(
        &self,
        user: Uuid,
        parent: Uuid,
        name: &str,
    ) -> Result<Option<Node>, FileError> {
        let found: Option<Node> = sqlx::query_as(sqlx::AssertSqlSafe(format!(
            "SELECT {NODE_COLUMNS} FROM nodes WHERE parent_id = ?1 AND name IN (?2, ?3)"
        )))
        .bind(parent)
        .bind(name)
        .bind(names::nfc(name))
        .fetch_optional(self.db.reader())
        .await?;
        if found.is_some() {
            return Ok(found);
        }
        let mount: Option<Mount> = sqlx::query_as(sqlx::AssertSqlSafe(format!(
            "SELECT {MOUNT_COLUMNS} FROM share_mounts m
             WHERE m.user_id = ?1 AND m.parent_id = ?2 AND m.name IN (?3, ?4)"
        )))
        .bind(user)
        .bind(parent)
        .bind(name)
        .bind(names::nfc(name))
        .fetch_optional(self.db.reader())
        .await?;
        let Some(mount) = mount else {
            return Ok(None);
        };
        let owner = self.owner_of(mount.node_id).await?;
        let mut node = self.node(owner, mount.node_id).await?;
        node.parent_id = Some(mount.parent_id);
        node.name = mount.name;
        Ok(Some(node))
    }

    /// The node at a path from the user's root, walking into the shares
    /// they placed.
    pub async fn resolve_for(
        self: &Arc<Self>,
        user: Uuid,
        path: &std::path::Path,
    ) -> Result<Node, FileError> {
        let mut node = self.root_node(user).await?;
        for part in path.components() {
            let std::path::Component::Normal(name) = part else {
                continue;
            };
            node = self
                .child_named(user, node.id, &name.to_string_lossy())
                .await?
                .ok_or(FileError::NotFound)?;
        }
        Ok(node)
    }

    /// A node's path as the user sees it, from their root: through the
    /// share they placed it in. None for a share not placed.
    pub async fn view_path(&self, user: Uuid, id: Uuid) -> Result<Option<Vec<String>>, FileError> {
        let owner = self.owner_of(id).await?;
        if super::roles::drive_role(self.db.reader(), user, owner)
            .await?
            .is_some()
        {
            return Ok(Some(super::path_of(self.db.reader(), id).await?));
        }
        let Some(mount) = self.mount_over(user, id).await? else {
            return Ok(None);
        };
        let mut path = super::path_of(self.db.reader(), mount.parent_id).await?;
        path.push(mount.name.clone());
        let full = super::path_of(self.db.reader(), id).await?;
        let top = super::path_of(self.db.reader(), mount.node_id).await?;
        path.extend(full.into_iter().skip(top.len()));
        Ok(Some(path))
    }

    /// Place a share in the user's tree: in one of their own folders,
    /// under its name or another.
    pub async fn place_share(
        self: &Arc<Self>,
        user: Uuid,
        node_id: Uuid,
        parent_id: Uuid,
        name: Option<&str>,
    ) -> Result<Node, FileError> {
        let access = self.access(user, node_id).await?;
        if access.own {
            return Err(FileError::Invalid("This is in your own files".into()));
        }
        if access.mount.is_some() {
            return Err(FileError::Conflict("It is in your files already".into()));
        }
        let name =
            names::new_name(name.unwrap_or(&access.node.name)).map_err(FileError::Invalid)?;
        let tree = self.tree(user).await?;
        let _guard = tree.lock.lock().await;
        // A folder of the user's own: never inside another share, nor
        // inside an encrypted folder (whose names are sealed).
        let parent = self.folder(user, parent_id).await?;
        no_mount_in_encrypted(&parent)?;
        let nested: Option<i64> = sqlx::query_scalar(sqlx::AssertSqlSafe(format!(
            "{UP}, {}
             SELECT 1 FROM share_mounts m
             WHERE m.user_id = ?2
               AND (m.node_id IN (SELECT id FROM up) OR m.node_id IN (SELECT id FROM down))
             LIMIT 1",
            DOWN.trim_start_matches("WITH RECURSIVE ")
        )))
        .bind(node_id)
        .bind(user)
        .fetch_optional(self.db.reader())
        .await?;
        if nested.is_some() {
            return Err(FileError::Conflict(
                "A folder above or inside it is in your files already".into(),
            ));
        }
        self.check_free(parent.id, &name, None).await?;
        let mount = Mount {
            id: Uuid::now_v7(),
            user_id: user,
            node_id,
            parent_id: parent.id,
            name,
        };
        let mut tx = self.db.writer().begin().await?;
        sqlx::query(
            "INSERT INTO share_mounts (id, user_id, node_id, parent_id, name)
             VALUES (?1, ?2, ?3, ?4, ?5)",
        )
        .bind(mount.id)
        .bind(user)
        .bind(node_id)
        .bind(mount.parent_id)
        .bind(&mount.name)
        .execute(&mut *tx)
        .await?;
        appear(&mut tx, &mount, node_id).await?;
        tx.commit().await?;
        self.notify(user);
        let mut node = access.node;
        node.parent_id = Some(mount.parent_id);
        node.name = mount.name;
        Ok(node)
    }

    /// Take a placed share out of the user's tree; the share stays.
    pub async fn unplace_share(&self, user: Uuid, node_id: Uuid) -> Result<(), FileError> {
        let mount = self
            .mount_of(user, node_id)
            .await?
            .ok_or(FileError::NotFound)?;
        let mut tx = self.db.writer().begin().await?;
        remove_mount(&mut tx, &mount).await?;
        tx.commit().await?;
        self.notify(user);
        Ok(())
    }

    /// Rename a placed share, or move it to another of the user's own
    /// folders. `if_version` is the shared node's.
    pub async fn move_mount(
        self: &Arc<Self>,
        user: Uuid,
        node_id: Uuid,
        new_parent: Option<Uuid>,
        new_name: Option<&str>,
        if_version: i64,
    ) -> Result<Node, FileError> {
        let new_name = new_name
            .map(names::new_name)
            .transpose()
            .map_err(FileError::Invalid)?;
        let tree = self.tree(user).await?;
        let _guard = tree.lock.lock().await;
        let access = self.access(user, node_id).await?;
        let mount = access.mount.clone().ok_or(FileError::NotFound)?;
        super::ops::check_version(&access.node, if_version)?;
        let parent = match new_parent {
            Some(parent) => self.folder(user, parent).await.map_err(|e| match e {
                FileError::NotFound => FileError::Invalid(
                    "A shared folder can only go in a folder of your own files".into(),
                ),
                e => e,
            })?,
            None => self.node(user, mount.parent_id).await?,
        };
        no_mount_in_encrypted(&parent)?;
        let name = new_name.unwrap_or_else(|| mount.name.clone());
        self.check_free(parent.id, &name, Some(node_id)).await?;
        let mut tx = self.db.writer().begin().await?;
        sqlx::query("UPDATE share_mounts SET parent_id = ?1, name = ?2 WHERE id = ?3")
            .bind(parent.id)
            .bind(&name)
            .bind(mount.id)
            .execute(&mut *tx)
            .await?;
        insert_row(
            &mut tx,
            user,
            node_id,
            "move",
            Some(access.node.kind),
            Some(parent.id),
            Some(&name),
            Some(access.node.version),
        )
        .await?;
        tx.commit().await?;
        self.notify(user);
        let mut node = access.node;
        node.parent_id = Some(parent.id);
        node.name = name;
        Ok(node)
    }

    /// Take out of the recipients' trees the shares they can no longer
    /// reach (a share removed or declined, a group left, a re-sharer's
    /// access gone).
    pub async fn settle_mounts(&self) -> Result<(), FileError> {
        let mounts: Vec<Mount> = sqlx::query_as(sqlx::AssertSqlSafe(format!(
            "SELECT {MOUNT_COLUMNS} FROM share_mounts m"
        )))
        .fetch_all(self.db.reader())
        .await?;
        for mount in mounts {
            let reaches = {
                let mut conn = self.db.reader().acquire().await?;
                super::roles::sees(&mut conn, mount.user_id, mount.node_id).await?
            };
            if reaches {
                continue;
            }
            let mut tx = self.db.writer().begin().await?;
            remove_mount(&mut tx, &mount).await?;
            tx.commit().await?;
            self.notify(mount.user_id);
        }
        // Who lost access to an encrypted folder loses its keys.
        self.settle_e2e().await
    }

    /// A placed share of the user's in a folder of theirs, by name.
    pub async fn mount_named(
        &self,
        user: Uuid,
        parent: Uuid,
        name: &str,
    ) -> Result<Option<Mount>, FileError> {
        Ok(sqlx::query_as(sqlx::AssertSqlSafe(format!(
            "SELECT {MOUNT_COLUMNS} FROM share_mounts m
             WHERE m.user_id = ?1 AND m.parent_id = ?2 AND m.name IN (?3, ?4)"
        )))
        .bind(user)
        .bind(parent)
        .bind(name)
        .bind(names::nfc(name))
        .fetch_optional(self.db.reader())
        .await?)
    }

    /// The names of a folder's children hidden from the user (limited
    /// access, others' uploads where they may only upload).
    pub async fn hidden_children(
        &self,
        user: Uuid,
        folder: Uuid,
    ) -> Result<Vec<String>, FileError> {
        let access = self.access(user, folder).await?;
        let hidden = super::roles::hidden(
            "nodes",
            "?2",
            access.keeper,
            access.role == super::roles::Role::UploadOnly,
        );
        Ok(sqlx::query_scalar(sqlx::AssertSqlSafe(format!(
            "SELECT name FROM nodes WHERE parent_id = ?1 AND {hidden}"
        )))
        .bind(folder)
        .bind(user)
        .fetch_all(self.db.reader())
        .await?)
    }

    /// The shares the user placed in a folder of theirs.
    pub async fn mounts_in(&self, user: Uuid, parent: Uuid) -> Result<Vec<Mount>, FileError> {
        Ok(sqlx::query_as(sqlx::AssertSqlSafe(format!(
            "SELECT {MOUNT_COLUMNS} FROM share_mounts m WHERE m.user_id = ?1 AND m.parent_id = ?2"
        )))
        .bind(user)
        .bind(parent)
        .fetch_all(self.db.reader())
        .await?)
    }

    /// Rename or move a node as a user (the REST API and WebDAV alike): a
    /// placed share of theirs moves within their own files; their own
    /// node within their tree; a node inside a share within its owner's
    /// tree, with change (and delete, out of its folder) and create where
    /// it goes. Between trees it is refused: a copy is the way.
    pub async fn move_as(
        self: &Arc<Self>,
        user: Uuid,
        id: Uuid,
        new_parent: Option<Uuid>,
        new_name: Option<&str>,
        if_version: i64,
    ) -> Result<Node, FileError> {
        let node = self.access(user, id).await?;
        if node.mount.is_some() {
            return self
                .move_mount(user, id, new_parent, new_name, if_version)
                .await;
        }
        let into = match new_parent {
            Some(parent) if Some(parent) != node.node.parent_id => {
                Some(self.access_for(user, parent, Need::Create).await?)
            }
            _ => None,
        };
        if let Some(into) = &into
            && into.owner != node.owner
        {
            return Err(FileError::Invalid(
                "Cannot move between your files and a shared folder, or between two shares: copy it instead"
                    .into(),
            ));
        }
        node.require(Need::Change)?;
        // Out of its folder: it is taken from there.
        if into.is_some() {
            node.require(Need::Delete)?;
        }
        if !node.own {
            self.inside_share(user, &node).await?;
        }
        self.move_node(node.owner, id, new_parent, new_name, if_version)
            .await
    }

    /// Delete a node as a user: their placed share goes from their files
    /// (answered None: nothing of the owner's is deleted); their own node,
    /// or one inside a share they may delete in, goes to its owner's trash.
    pub async fn delete_as(
        self: &Arc<Self>,
        user: Uuid,
        id: Uuid,
        if_version: i64,
    ) -> Result<Option<super::ops::TrashItem>, FileError> {
        let node = self.access(user, id).await?;
        if node.mount.is_some() {
            super::ops::check_version(&node.node, if_version)?;
            self.unplace_share(user, id).await?;
            return Ok(None);
        }
        node.require(Need::Delete)?;
        if !node.own {
            self.inside_share(user, &node).await?;
        }
        Ok(Some(self.trash(node.owner, id, if_version).await?))
    }

    /// A recipient changes what is inside a share, not the shared item
    /// itself (their placed entry is theirs to change instead): what they
    /// change must have a parent they reach too.
    async fn inside_share(&self, user: Uuid, access: &Access) -> Result<(), FileError> {
        let Some(parent) = access.node.parent_id else {
            return Err(FileError::Forbidden("The root folder".into()));
        };
        match self.access(user, parent).await {
            Ok(_) => Ok(()),
            Err(FileError::NotFound) => Err(FileError::Forbidden(
                "This is the shared item itself: only its owner can change it".into(),
            )),
            Err(e) => Err(e),
        }
    }

    /// The folders of the shares a user placed and can add to, each with
    /// its path as the user sees it and what is under it: where the drop
    /// box may put things.
    pub async fn writable_mounts(
        &self,
        user: Uuid,
    ) -> Result<Vec<(Mount, Vec<String>)>, FileError> {
        let mounts: Vec<Mount> = sqlx::query_as(sqlx::AssertSqlSafe(format!(
            "SELECT {MOUNT_COLUMNS} FROM share_mounts m WHERE m.user_id = ?1"
        )))
        .bind(user)
        .fetch_all(self.db.reader())
        .await?;
        let mut out = Vec::new();
        for mount in mounts {
            let Ok(access) = self.access(user, mount.node_id).await else {
                continue;
            };
            // Never an encrypted folder: the drop box cannot write there;
            // and only for an editor or above, who sees what is in it
            // (upload only sees nothing to choose from).
            // Nor another server's share: the drop box files nothing
            // elsewhere (docs/M7E_DESIGN.md §10).
            if access.node.kind != Kind::Folder
                || !access.allows(Need::Change)
                || access.node.e2e_root.is_some()
                || access.node.remote.is_some()
            {
                continue;
            }
            let mut path = super::path_of(self.db.reader(), mount.parent_id).await?;
            path.push(mount.name.clone());
            out.push((mount, path));
        }
        Ok(out)
    }
}

/// A share is never placed inside an encrypted folder: its name there
/// would be plain where the devices expect sealed ones.
fn no_mount_in_encrypted(parent: &Node) -> Result<(), FileError> {
    if parent.e2e_root.is_some() {
        return Err(FileError::Invalid(
            "A shared item cannot go inside an encrypted folder".into(),
        ));
    }
    Ok(())
}

/// Take every share of a drive's tree out of the recipients' trees, with
/// a row in each one's feed: the drive or its user is being removed, or
/// its tree indexed anew (the ids it had go).
pub(crate) async fn end_mounts_on(
    tx: &mut sqlx::SqliteConnection,
    owner: Uuid,
) -> sqlx::Result<()> {
    let mounts: Vec<Mount> = sqlx::query_as(sqlx::AssertSqlSafe(format!(
        "SELECT {MOUNT_COLUMNS} FROM share_mounts m JOIN nodes n ON n.id = m.node_id
         WHERE n.drive_id = ?1"
    )))
    .bind(owner)
    .fetch_all(&mut *tx)
    .await?;
    for mount in mounts {
        remove_mount(tx, &mount).await?;
    }
    Ok(())
}

/// A change row in a user's feed, about their placed shares: these are
/// in their My drive.
#[allow(clippy::too_many_arguments)]
async fn insert_row(
    tx: &mut sqlx::SqliteConnection,
    user: Uuid,
    node_id: Uuid,
    op: &str,
    kind: Option<Kind>,
    parent_id: Option<Uuid>,
    name: Option<&str>,
    version: Option<i64>,
) -> sqlx::Result<()> {
    sqlx::query(
        "INSERT INTO changes (user_id, drive_id, node_id, op, kind, parent_id, name, version)
         VALUES (?1, ?8, ?2, ?3, ?4, ?5, ?6, ?7)",
    )
    .bind(user)
    .bind(node_id)
    .bind(op)
    .bind(kind.map(Kind::as_str))
    .bind(parent_id)
    .bind(name)
    .bind(version)
    .bind(super::drives::my(user))
    .execute(tx)
    .await?;
    Ok(())
}

/// A mount goes, with its row in its user's feed.
async fn remove_mount(tx: &mut sqlx::SqliteConnection, mount: &Mount) -> sqlx::Result<()> {
    sqlx::query("DELETE FROM share_mounts WHERE id = ?1")
        .bind(mount.id)
        .execute(&mut *tx)
        .await?;
    insert_row(
        tx,
        mount.user_id,
        mount.node_id,
        "delete",
        None,
        None,
        None,
        None,
    )
    .await
}

/// A node and everything under it appear in a mount's user's feed, the
/// parents first: placed, or moved into the share.
async fn appear(tx: &mut sqlx::SqliteConnection, mount: &Mount, top: Uuid) -> sqlx::Result<()> {
    #[derive(sqlx::FromRow)]
    struct Row {
        id: Uuid,
        parent_id: Option<Uuid>,
        name: String,
        kind: Kind,
        version: i64,
    }
    let rows: Vec<Row> = sqlx::query_as(
        "WITH RECURSIVE down (id, parent_id, name, kind, version, depth) AS (
           SELECT id, parent_id, name, kind, version, 0 FROM nodes WHERE id = ?1
           UNION ALL
           SELECT n.id, n.parent_id, n.name, n.kind, n.version, down.depth + 1
           FROM nodes n JOIN down ON n.parent_id = down.id
         )
         SELECT id, parent_id, name, kind, version FROM down ORDER BY depth",
    )
    .bind(top)
    .fetch_all(&mut *tx)
    .await?;
    for row in rows {
        // What is hidden from them (limited access, upload only) does not
        // appear.
        if !super::roles::sees(&mut *tx, mount.user_id, row.id).await? {
            continue;
        }
        let (parent, name) = if row.id == mount.node_id {
            (Some(mount.parent_id), mount.name.as_str())
        } else {
            (row.parent_id, row.name.as_str())
        };
        insert_row(
            tx,
            mount.user_id,
            row.id,
            "create",
            Some(row.kind),
            parent,
            Some(name),
            Some(row.version),
        )
        .await?;
    }
    Ok(())
}

/// The mounts a node is in (or is the top of).
async fn mounts_over(tx: &mut sqlx::SqliteConnection, id: Uuid) -> sqlx::Result<Vec<Mount>> {
    sqlx::query_as(sqlx::AssertSqlSafe(format!(
        "{UP} SELECT {MOUNT_COLUMNS} FROM share_mounts m JOIN up ON m.node_id = up.id"
    )))
    .bind(id)
    .fetch_all(tx)
    .await
}

/// Copy a change of an owner's tree to the feeds of the users who placed a
/// share it is in: as it is, or as the node coming into their share (with
/// what is under it) or going out of it. The top of a mount is where the
/// recipient put it, under their name.
pub(crate) async fn fan_out(
    tx: &mut sqlx::SqliteConnection,
    change: &super::Change<'_>,
) -> sqlx::Result<()> {
    let any: bool = sqlx::query_scalar("SELECT EXISTS (SELECT 1 FROM share_mounts)")
        .fetch_one(&mut *tx)
        .await?;
    if !any {
        return Ok(());
    }
    let id = change.node_id;
    match change.op {
        "delete" => {
            for mount in {
                let over = mounts_over(tx, id).await?;
                seeing(tx, over, id).await?
            } {
                insert_row(tx, mount.user_id, id, "delete", None, None, None, None).await?;
            }
            // The placed shares under it go with it.
            let under: Vec<Mount> = sqlx::query_as(sqlx::AssertSqlSafe(format!(
                "{DOWN} SELECT {MOUNT_COLUMNS} FROM share_mounts m JOIN down ON m.node_id = down.id"
            )))
            .bind(id)
            .fetch_all(&mut *tx)
            .await?;
            for mount in under {
                insert_row(
                    tx,
                    mount.user_id,
                    mount.node_id,
                    "delete",
                    None,
                    None,
                    None,
                    None,
                )
                .await?;
            }
        }
        "move" => {
            let after = {
                let over = mounts_over(tx, id).await?;
                seeing(tx, over, id).await?
            };
            let before = match change.from_parent {
                Some(from) => {
                    let mut before = Vec::new();
                    for mount in mounts_over(tx, from).await? {
                        if super::roles::resolve_under(&mut *tx, mount.user_id, id, from)
                            .await?
                            .is_some()
                        {
                            before.push(mount);
                        }
                    }
                    before.extend(after.iter().filter(|m| m.node_id == id).cloned());
                    before
                }
                None => after.clone(),
            };
            for mount in &after {
                if before.iter().any(|m| m.user_id == mount.user_id) {
                    seen(tx, mount, change).await?;
                } else {
                    appear(tx, mount, id).await?;
                }
            }
            for mount in &before {
                if !after.iter().any(|m| m.user_id == mount.user_id) {
                    insert_row(tx, mount.user_id, id, "delete", None, None, None, None).await?;
                }
            }
        }
        _ => {
            for mount in {
                let over = mounts_over(tx, id).await?;
                seeing(tx, over, id).await?
            } {
                seen(tx, &mount, change).await?;
            }
        }
    }
    Ok(())
}

/// The mounts whose users see a node (limited access and upload only hide
/// it from some).
async fn seeing(
    tx: &mut sqlx::SqliteConnection,
    mounts: Vec<Mount>,
    node: Uuid,
) -> sqlx::Result<Vec<Mount>> {
    let mut out = Vec::with_capacity(mounts.len());
    for mount in mounts {
        if super::roles::sees(&mut *tx, mount.user_id, node).await? {
            out.push(mount);
        }
    }
    Ok(out)
}

/// A change as a mount's user sees it.
async fn seen(
    tx: &mut sqlx::SqliteConnection,
    mount: &Mount,
    change: &super::Change<'_>,
) -> sqlx::Result<()> {
    let (op, parent, name) = if change.node_id == mount.node_id {
        // Their own entry: where they put it. A rename by the owner changes
        // nothing of it but the version.
        let op = if change.op == "move" {
            "update"
        } else {
            change.op
        };
        (op, Some(mount.parent_id), Some(mount.name.as_str()))
    } else {
        (change.op, change.parent_id, change.name)
    };
    insert_row(
        tx,
        mount.user_id,
        change.node_id,
        op,
        change.kind,
        parent,
        name,
        change.version,
    )
    .await
}
