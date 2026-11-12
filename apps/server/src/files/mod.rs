//! The users' trees: where each one is on disk (the files root, §4 of
//! docs/M1_DESIGN.md), the index of what is in it (`nodes`), the reconciler
//! that keeps the index true to the disk (`scan.rs`, `watch.rs`), and the
//! changes Organizer makes itself, journaled (`ops.rs`, `journal.rs`).
//!
//! The disk is the source of truth. Everything that touches a user's tree,
//! a scan or an operation, holds that tree's lock, so a scan never sees an
//! operation half done and two operations never cross.

pub mod drives;
pub mod e2e;
pub mod grants;
pub mod journal;
pub mod mirrors;
pub mod moves;
pub mod names;
pub mod ops;
pub mod quota;
pub mod receive;
pub mod roles;
pub mod scan;
pub mod shares;
pub mod versions;
mod watch;

use crate::db::Db;
use serde::Serialize;
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::time::{Duration, SystemTime, UNIX_EPOCH};
use utoipa::ToSchema;
use uuid::Uuid;

/// How long the change feed keeps its rows (docs/M1_DESIGN.md §6).
pub const CHANGES_KEPT: Duration = Duration::from_secs(30 * 86_400);

/// A file or folder of a drive's tree, as the API sends it.
#[derive(Clone, Debug, Serialize, sqlx::FromRow, ToSchema)]
pub struct Node {
    pub id: Uuid,
    /// The drive it is in (docs/M7A_DESIGN.md §2): a string, a UUID for
    /// this server's drives. A placed share's top answers the drive it is
    /// really in.
    #[schema(value_type = String)]
    pub drive_id: Uuid,
    /// Null for the root folder.
    #[schema(required = true)]
    pub parent_id: Option<Uuid>,
    /// Empty for the root folder.
    pub name: String,
    pub kind: Kind,
    /// In bytes; 0 for a folder.
    pub size: i64,
    /// Guessed from the name; null for a folder.
    #[schema(required = true)]
    pub mime: Option<String>,
    /// Bumped on every change of content, name or place: the ETag, and what
    /// a write sends back as `if_version`.
    pub version: i64,
    /// False for a name made outside Organizer that other systems refuse.
    pub portable: bool,
    pub created_at: String,
    pub updated_at: String,
    /// The end-to-end-encrypted folder this is in (docs/E2E_SPEC.md): the
    /// top encrypted folder's id (its own for that folder), whose keys
    /// open the names and contents under it; null when not encrypted.
    #[schema(required = true)]
    pub e2e_root: Option<Uuid>,
    /// In a mirror (docs/M7D_DESIGN.md), and a drive ejected from one: the
    /// node of the drive mirrored that this is a copy of; in an encrypted
    /// folder shared from another server (docs/M7E_DESIGN.md §10,
    /// `organizer-e2e`): the node's id on the owner's server; null
    /// elsewhere. An encrypted folder's keys and names are sealed with the
    /// ids there (its folder key with the top's, each name with its
    /// parent's): ask `/encryption/folders/{source_id}` and open names with
    /// the parent's `source_id`.
    #[sqlx(default)]
    #[schema(required = true)]
    pub source_id: Option<Uuid>,
    /// What the signed-in user may do with it (docs/M7B_DESIGN.md §1):
    /// the clients offer only what it allows. Null only where no user is
    /// asking (never in an answer to a signed-in user's request).
    #[sqlx(skip)]
    #[schema(required = true)]
    pub my_role: Option<roles::Role>,
    /// Shared from another server (docs/M7E_DESIGN.md §5): that server,
    /// `host[:port]`; its files go through ours. Null for this server's.
    #[sqlx(skip)]
    #[schema(required = true)]
    pub remote: Option<String>,
}

impl Node {
    /// The node with the user's role on it.
    pub fn with_role(mut self, role: roles::Role) -> Self {
        self.my_role = Some(role);
        self
    }
}

/// A file or a folder.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, ToSchema, sqlx::Type)]
#[serde(rename_all = "lowercase")]
#[sqlx(type_name = "TEXT", rename_all = "lowercase")]
pub enum Kind {
    File,
    Folder,
}

impl Kind {
    pub fn as_str(self) -> &'static str {
        match self {
            Kind::File => "file",
            Kind::Folder => "folder",
        }
    }
}

/// The columns of a [`Node`].
pub const NODE_COLUMNS: &str = "id, drive_id, parent_id, name, kind, size, mime, version, portable, created_at, updated_at, e2e_root, source_id";

/// Why something cannot be done to a tree. `From` turns it into the API's
/// answer (see `error.rs`).
#[derive(Debug, thiserror::Error)]
pub enum FileError {
    #[error("not found")]
    NotFound,
    /// Fit to show: a name clash, a version that is not the current one.
    #[error("{0}")]
    Conflict(String),
    /// The drive is being moved to another backend: read-only until that
    /// is done ([`READ_ONLY`], `code` `read_only`).
    #[error("{}", READ_ONLY)]
    ReadOnly,
    /// Fit to show: a name or a target that cannot be.
    #[error("{0}")]
    Invalid(String),
    /// The files root is missing, or empty while the index is not (an
    /// unmounted share): nothing is changed until it is back.
    #[error("the files root is not available: {0}")]
    Unavailable(String),
    /// A body larger than the endpoint takes (its limit in bytes).
    #[error("too large")]
    TooLarge(u64),
    /// The write would take the user past their storage limit.
    #[error("over the storage limit")]
    Full,
    /// A shared file or folder the user may not do this to (fit to show).
    #[error("{0}")]
    Forbidden(String),
    /// The drive's storage backend fails, or its private credentials are
    /// locked (docs/M7C_DESIGN.md §3, §4): the reason, fit to show.
    #[error("{0}")]
    Down(String),
    #[error("database error")]
    Database(#[from] sqlx::Error),
    #[error(transparent)]
    Internal(#[from] anyhow::Error),
}

impl From<std::io::Error> for FileError {
    fn from(e: std::io::Error) -> Self {
        if let Some(down) = e
            .get_ref()
            .and_then(|inner| inner.downcast_ref::<crate::storage::Down>())
        {
            return FileError::Down(down.0.clone());
        }
        FileError::Internal(e.into())
    }
}

/// What a write to a drive being moved to another backend is answered.
pub const READ_ONLY: &str =
    "This drive is being moved to other storage: it is read-only until that is done";

/// The name of Organizer's own folder at the root of every drive: its
/// trash and versions (docs/M7A_DESIGN.md §1). It is the drive's data but
/// never a user's file: the scan, the listings, search, WebDAV, the sync
/// clients and the apps never see it, and no name Organizer makes at a
/// drive's root may take it.
pub const ORGANIZER_DIR: &str = ".organizer";

/// Where a drive's deleted files go, from its root.
pub const TRASH_DIR: &str = ".organizer/trash";
/// Where the earlier contents of a drive's files are kept, from its root.
pub const VERSIONS_DIR: &str = ".organizer/versions";

/// A drive's tree: where its bytes are, and its lock.
pub struct Tree {
    pub drive_id: Uuid,
    /// The drive's storage (docs/M7C_DESIGN.md §4): its folder on the
    /// server's disk, or its prefix in a backend.
    pub storage: crate::storage::Storage,
    /// Held by scans and operations alike.
    lock: tokio::sync::Mutex<()>,
    /// Watches the root while the tree is open; dropped with it.
    watcher: Mutex<Option<notify::RecommendedWatcher>>,
    /// Being moved to another backend: read-only until it is done.
    pub(crate) moving: std::sync::atomic::AtomicBool,
}

impl Tree {
    /// Organizer's own folder in the drive, on this machine (the disk).
    pub fn organizer_dir(&self) -> Option<PathBuf> {
        self.storage.local_path(ORGANIZER_DIR)
    }

    /// Where the drive's deleted files go, on this machine (the disk).
    pub fn trash_dir(&self) -> Option<PathBuf> {
        self.storage.local_path(TRASH_DIR)
    }

    /// Where a trashed node's bytes are, from the drive's root.
    pub fn trash_path(id: Uuid) -> String {
        format!("{TRASH_DIR}/{id}")
    }

    /// Where a version's bytes are, from the drive's root.
    pub fn version_path(id: Uuid) -> String {
        format!("{VERSIONS_DIR}/{id}")
    }

    /// On the disk, the trash and versions folders are made before a rename
    /// into them; a bucket has no folders to make.
    pub(crate) fn prepare(&self) -> std::io::Result<()> {
        for dir in [TRASH_DIR, VERSIONS_DIR] {
            if let Some(path) = self.storage.local_path(dir) {
                std::fs::create_dir_all(path)?;
            }
        }
        Ok(())
    }

    /// The lock for a change: refused while the drive is being moved.
    pub(crate) async fn write_lock(&self) -> Result<tokio::sync::MutexGuard<'_, ()>, FileError> {
        let guard = self.lock.lock().await;
        if self.moving.load(std::sync::atomic::Ordering::SeqCst) {
            return Err(FileError::ReadOnly);
        }
        Ok(guard)
    }

    /// The lock, for a scan or a read that must not see a change half made.
    pub(crate) async fn read_lock(&self) -> tokio::sync::MutexGuard<'_, ()> {
        self.lock.lock().await
    }
}

/// Every drive's tree, and where Organizer keeps its own files.
pub struct Files {
    db: Db,
    /// One lock per upload in progress: its requests take turns.
    pub(crate) upload_locks: Mutex<HashMap<Uuid, Arc<tokio::sync::Mutex<()>>>>,
    data_dir: PathBuf,
    /// `FILES_DIR`: the files root of the first superadmin (`owns_files_dir`),
    /// which then cannot be changed in the settings.
    files_dir: Option<PathBuf>,
    trees: Mutex<HashMap<Uuid, Arc<Tree>>>,
    /// Watch the trees and scan them on a timer: off in the tests, which
    /// scan when they want to.
    background: bool,
    /// What changed for which user: the live events (SSE).
    events: tokio::sync::broadcast::Sender<(Uuid, Topic)>,
    /// The `TRASH_DAYS` and `VERSIONS_*` variables (`retention.rs`).
    retention_env: Mutex<crate::retention::Partial>,
    /// The last row of the change feed whose user was told of it: the
    /// recipients of a placed share get rows from someone else's change.
    feed_mark: Arc<std::sync::atomic::AtomicI64>,
    /// The storage backends the drives are in (docs/M7C_DESIGN.md).
    backends: Arc<crate::backends::Backends>,
    /// The mirrors' copies under way (docs/M7D_DESIGN.md).
    mirroring: mirrors::Mirroring,
    /// Federation with other servers (docs/M7E_DESIGN.md).
    federation: Arc<crate::federation::Federation>,
}

/// What a live event is about.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Topic {
    /// The change feed has new rows.
    Changes,
    /// An item of the drop box changed.
    DropBox,
    /// The search index and the embeddings must be looked at again (the
    /// embeddings model changed). Internal: not sent to the apps.
    Reindex,
}

impl Topic {
    /// The event's name in the stream.
    pub fn event(self) -> &'static str {
        match self {
            Topic::Changes => "changes",
            Topic::DropBox => "drop-box",
            Topic::Reindex => "reindex",
        }
    }
}

impl Files {
    pub fn new(db: Db, data_dir: PathBuf, files_dir: Option<PathBuf>) -> Arc<Self> {
        Self::make(db, data_dir, files_dir, false)
    }

    /// The service the server runs: trees are watched and rescanned.
    pub fn with_background(db: Db, data_dir: PathBuf, files_dir: Option<PathBuf>) -> Arc<Self> {
        Self::make(db, data_dir, files_dir, true)
    }

    fn make(db: Db, data_dir: PathBuf, files_dir: Option<PathBuf>, background: bool) -> Arc<Self> {
        Arc::new(Files {
            backends: Arc::new(crate::backends::Backends::new(db.clone(), data_dir.clone())),
            federation: Arc::new(crate::federation::Federation::new(
                db.clone(),
                data_dir.clone(),
            )),
            db,
            upload_locks: Mutex::new(HashMap::new()),
            data_dir,
            files_dir,
            trees: Mutex::new(HashMap::new()),
            background,
            events: tokio::sync::broadcast::channel(64).0,
            retention_env: Mutex::new(Default::default()),
            feed_mark: Default::default(),
            mirroring: Default::default(),
        })
    }

    /// The live events: each user's id with what changed for them.
    pub fn subscribe(&self) -> tokio::sync::broadcast::Receiver<(Uuid, Topic)> {
        self.events.subscribe()
    }

    /// Tell the open apps of a user that their feed has new rows, and
    /// those of the other users whose feeds got rows since the last time
    /// (the recipients of a share the change is in).
    pub(crate) fn notify(&self, user_id: Uuid) {
        self.announce(user_id, Topic::Changes);
        if tokio::runtime::Handle::try_current().is_err() {
            return;
        }
        let db = self.db.clone();
        let events = self.events.clone();
        let mark = self.feed_mark.clone();
        tokio::spawn(async move {
            use std::sync::atomic::Ordering;
            let from = mark.load(Ordering::SeqCst);
            let rows: Vec<(Uuid, i64)> = match sqlx::query_as(
                "SELECT user_id, max(seq) FROM changes WHERE seq > ?1 GROUP BY user_id",
            )
            .bind(from)
            .fetch_all(db.reader())
            .await
            {
                Ok(rows) => rows,
                Err(e) => {
                    tracing::warn!("cannot read the change feed: {e}");
                    return;
                }
            };
            if let Some(last) = rows.iter().map(|(_, seq)| *seq).max() {
                mark.fetch_max(last, Ordering::SeqCst);
            }
            for (other, _) in rows {
                if other != user_id {
                    let _ = events.send((other, Topic::Changes));
                }
            }
        });
    }

    /// Tell the open apps of a user that something changed.
    pub fn announce(&self, user_id: Uuid, topic: Topic) {
        // Nobody listening is fine.
        let _ = self.events.send((user_id, topic));
    }

    /// The `TRASH_DAYS` and `VERSIONS_*` variables, which win over every
    /// user's rules.
    pub fn retention_env(&self) -> crate::retention::Partial {
        *self.retention_env.lock().expect("not poisoned")
    }

    pub fn set_retention_env(&self, env: crate::retention::Partial) {
        *self.retention_env.lock().expect("not poisoned") = env;
    }

    /// Where uploads are written before they are moved into a tree.
    pub fn uploads_dir(&self) -> PathBuf {
        self.data_dir.join("uploads")
    }

    pub fn db(&self) -> &Db {
        &self.db
    }

    /// The storage backends.
    /// Federation with other servers (docs/M7E_DESIGN.md).
    pub fn federation(&self) -> &crate::federation::Federation {
        &self.federation
    }

    pub fn backends(&self) -> &crate::backends::Backends {
        &self.backends
    }

    pub fn data_dir(&self) -> &Path {
        &self.data_dir
    }

    /// `FILES_DIR`, when it is set.
    pub fn files_dir(&self) -> Option<&Path> {
        self.files_dir.as_deref()
    }

    /// Where a user's tree is: `FILES_DIR` for the first superadmin (the
    /// row's `owns_files_dir`), else the stored root, else the default in
    /// the data dir.
    pub fn root_for(&self, user_id: Uuid, stored: &StoredRoot) -> PathBuf {
        if let Some(dir) = &self.files_dir
            && stored.owns_files_dir
        {
            return dir.clone();
        }
        match &stored.files_root {
            Some(root) => PathBuf::from(root),
            None => self.default_root(user_id),
        }
    }

    /// Whether `FILES_DIR` sets this user's folder, so that it cannot be
    /// changed.
    pub fn root_locked(&self, stored: &StoredRoot) -> bool {
        self.files_dir.is_some() && stored.owns_files_dir
    }

    /// The default files root, in the data dir.
    pub fn default_root(&self, user_id: Uuid) -> PathBuf {
        self.user_dir(user_id).join("files")
    }

    /// Organizer's own folder for a user, in the data dir: never inside the
    /// user's tree.
    pub fn user_dir(&self, user_id: Uuid) -> PathBuf {
        self.data_dir.join("users").join(user_id.to_string())
    }

    /// Where a shared drive's tree is by default.
    pub fn shared_default_root(&self, drive_id: Uuid) -> PathBuf {
        self.data_dir.join("drives").join(drive_id.to_string())
    }

    /// At start: settle what the journal left, move the trash and the
    /// versions into their drives (once, from before drives), then open
    /// every drive's tree and index it, each in the background. Prunes the
    /// change feed hourly.
    pub async fn start(self: &Arc<Self>) -> anyhow::Result<()> {
        // The server key is made now, at the first start, not when the
        // first backend is added.
        self.backends.key()?;
        journal::recover(self, None).await?;
        self.move_into_drives().await;
        // The shares from other servers are followed (`federation::incoming`).
        let drives: Vec<Uuid> = sqlx::query_scalar("SELECT id FROM drives WHERE kind != 'remote'")
            .fetch_all(self.db.reader())
            .await?;
        for drive in drives {
            let files = self.clone();
            tokio::spawn(async move { files.open_and_scan(drive).await });
        }
        // The moves between backends a stop interrupted carry on.
        self.resume_moves().await?;
        if self.background {
            // The mirrors copy when due; a copy a stop cut short is
            // finished at the first look.
            self.spawn_mirrors();
            let files = self.clone();
            tokio::spawn(async move {
                loop {
                    if let Err(e) = prune_changes(&files.db).await {
                        tracing::warn!("cannot prune the change feed: {e}");
                    }
                    if let Err(e) = crate::routes::uploads::expire(&files).await {
                        tracing::warn!("cannot remove the expired uploads: {e}");
                    }
                    tokio::time::sleep(Duration::from_secs(3_600)).await;
                }
            });
            // The secrets of sessions that ended are dropped, and their
            // backends' drives closed.
            let files = self.clone();
            tokio::spawn(async move {
                loop {
                    tokio::time::sleep(Duration::from_secs(60)).await;
                    files.close_locked().await;
                }
            });
        }
        Ok(())
    }

    /// Close the trees of a backend's drives: they are opened anew with its
    /// new settings (or found locked).
    pub async fn forget_backend(&self, backend: Uuid) {
        let drives: Vec<Uuid> =
            match sqlx::query_scalar("SELECT id FROM drives WHERE backend_id = ?1")
                .bind(backend)
                .fetch_all(self.db.reader())
                .await
            {
                Ok(drives) => drives,
                Err(e) => {
                    tracing::warn!("cannot list a backend's drives: {e}");
                    return;
                }
            };
        for drive in drives {
            self.forget(drive).await;
        }
    }

    /// Close the trees of the drives whose private credentials no session
    /// holds any more: they are locked until someone unlocks them again.
    pub async fn close_locked(&self) {
        let locked = match self.backends.sweep_sessions().await {
            Ok(locked) => locked,
            Err(e) => {
                tracing::warn!("cannot look at the sessions: {e}");
                return;
            }
        };
        for backend in locked {
            self.forget_backend(backend).await;
        }
    }

    /// Open a drive's tree and index it; a failure is logged (the tree is
    /// tried again by the next scan).
    pub async fn open_and_scan(self: &Arc<Self>, drive_id: Uuid) {
        match self.scan(drive_id).await {
            Ok(report) => tracing::info!("indexed the tree of drive {drive_id}: {report}"),
            Err(e) => tracing::warn!("cannot index the tree of drive {drive_id}: {e}"),
        }
    }

    /// Where a drive's tree is, and whether it is a default one in the data
    /// dir (made when missing).
    pub async fn drive_root(&self, drive_id: Uuid) -> Result<(PathBuf, bool), FileError> {
        let drive = drives::DriveRow::get(self.db.reader(), drive_id)
            .await?
            .ok_or(FileError::NotFound)?;
        Ok(match drive.kind {
            drives::DriveKind::My => {
                let user_id = drive.user_id.ok_or(FileError::NotFound)?;
                let stored = StoredRoot::of(self.db.reader(), user_id)
                    .await?
                    .ok_or(FileError::NotFound)?;
                let root = self.root_for(user_id, &stored);
                let default = root == self.default_root(user_id);
                (root, default)
            }
            // Always in its user's own storage, never on the server's disk.
            drives::DriveKind::Mirror | drives::DriveKind::Remote => {
                return Err(FileError::NotFound);
            }
            drives::DriveKind::Shared => match drive.root {
                Some(root) => (PathBuf::from(root), false),
                None => (self.shared_default_root(drive_id), true),
            },
        })
    }

    /// A drive's tree, opened on first use: a default root is made when it
    /// is missing; a folder someone chose must exist.
    pub async fn tree(self: &Arc<Self>, drive_id: Uuid) -> Result<Arc<Tree>, FileError> {
        if let Some(tree) = self.trees.lock().unwrap().get(&drive_id) {
            return Ok(tree.clone());
        }
        let (backend, kind): (Option<Uuid>, drives::DriveKind) =
            sqlx::query_as("SELECT backend_id, kind FROM drives WHERE id = ?1")
                .bind(drive_id)
                .fetch_optional(self.db.reader())
                .await?
                .ok_or(FileError::NotFound)?;
        let (storage, listing) = match backend {
            // A share from another server: its WebDAV, followed by
            // `federation::incoming` rather than watched.
            None if kind == drives::DriveKind::Remote => (
                crate::federation::incoming::storage(self, drive_id).await?,
                None,
            ),
            None => (self.disk_storage(drive_id).await?, None),
            Some(backend) => {
                let row = crate::backends::BackendRow::get(self.db.reader(), backend)
                    .await?
                    .ok_or(FileError::NotFound)?;
                let listing = row
                    .list_every
                    .map(|secs| Duration::from_secs(secs.max(60) as u64));
                (self.backends.storage(&row, drive_id).await?, listing)
            }
        };
        let moving: Option<i64> =
            sqlx::query_scalar("SELECT 1 FROM drive_moves WHERE drive_id = ?1")
                .bind(drive_id)
                .fetch_optional(self.db.reader())
                .await?;
        let tree = Arc::new(Tree {
            drive_id,
            storage,
            lock: tokio::sync::Mutex::new(()),
            watcher: Mutex::new(None),
            moving: moving.is_some().into(),
        });
        tree.prepare()?;
        if self.background {
            watch::spawn(Arc::downgrade(self), &tree, listing);
        }
        let mut trees = self.trees.lock().unwrap();
        Ok(trees.entry(drive_id).or_insert(tree).clone())
    }

    /// A drive's storage on the server's disk: a default root is made when
    /// it is missing; a folder someone chose must exist.
    async fn disk_storage(&self, drive_id: Uuid) -> Result<crate::storage::Storage, FileError> {
        let (root, default) = self.drive_root(drive_id).await?;
        if default {
            std::fs::create_dir_all(&root)?;
        } else if !root.is_dir() {
            return Err(FileError::Unavailable(format!(
                "{} does not exist or is not a folder",
                root.display()
            )));
        }
        Ok(crate::storage::Storage::disk(&root)?)
    }

    /// A drive's storage, whether or not its tree is open.
    pub async fn storage(
        self: &Arc<Self>,
        drive_id: Uuid,
    ) -> Result<crate::storage::Storage, FileError> {
        Ok(self.tree(drive_id).await?.storage.clone())
    }

    /// The path of a node from its drive's root (`Documents/Taxes`; "" for
    /// the root).
    pub async fn rel_path(&self, id: Uuid) -> Result<String, FileError> {
        Ok(path_of(self.db.reader(), id).await?.join("/"))
    }

    /// Whether a drive is locked or failing now, and why (fit to show);
    /// None: it can be used. Its backend fails or its private credentials
    /// are locked (the server's disk answers as its folder does: the
    /// drive's root is then missing).
    pub async fn unavailable(
        &self,
        drive_id: Uuid,
    ) -> Result<Option<(crate::backends::Unavailable, String)>, FileError> {
        let backend: Option<Uuid> =
            sqlx::query_scalar("SELECT backend_id FROM drives WHERE id = ?1")
                .bind(drive_id)
                .fetch_optional(self.db.reader())
                .await?
                .flatten();
        let Some(backend) = backend else {
            // A share from another server: failing while that server does
            // not answer (or refuses our token).
            if self.remote_host(drive_id).await?.is_some() {
                return Ok(self
                    .federation
                    .health(drive_id)
                    .error()
                    .map(|reason| (crate::backends::Unavailable::Failing, reason)));
            }
            return Ok(None);
        };
        let Some(row) = crate::backends::BackendRow::get(self.db.reader(), backend).await? else {
            return Ok(None);
        };
        Ok(self.backends.unavailable_why(&row).await?)
    }

    /// Whether a drive can be written to now: available, and not being
    /// moved (the drop box leaves the others out).
    pub async fn writable(&self, drive_id: Uuid) -> Result<bool, FileError> {
        let moving: Option<i64> =
            sqlx::query_scalar("SELECT 1 FROM drive_moves WHERE drive_id = ?1")
                .bind(drive_id)
                .fetch_optional(self.db.reader())
                .await?;
        Ok(moving.is_none() && self.unavailable(drive_id).await?.is_none())
    }

    /// Bring the index of a drive's tree in line with its storage.
    pub async fn scan(self: &Arc<Self>, drive_id: Uuid) -> Result<scan::Report, FileError> {
        let tree = self.tree(drive_id).await?;
        let _guard = tree.read_lock().await;
        let report = scan::scan(&self.db, drive_id, &tree.storage).await?;
        drop(_guard);
        if report != scan::Report::default() {
            self.notify(drive_id);
        }
        // Hashing is slow on a large tree: it runs after the scan, without
        // the lock, and stores a hash only for a file that did not change.
        // Never another server's files: that would download them all.
        if tree.storage.is_remote() {
            return Ok(report);
        }
        let files = self.clone();
        let storage = tree.storage.clone();
        tokio::spawn(async move {
            if let Err(e) = scan::hash_pending(&files.db, drive_id, &storage).await {
                tracing::warn!("cannot hash the files of drive {drive_id}: {e}");
            }
        });
        Ok(report)
    }

    /// Set a user's files root (NULL: the default), the root of their My
    /// drive, and index it from scratch. The ids, the change feed and the
    /// journal of the old tree are dropped: clients list everything again.
    /// The trash and the versions go along into the new folder.
    pub async fn set_root(
        self: &Arc<Self>,
        user_id: Uuid,
        root: Option<&Path>,
    ) -> Result<(), FileError> {
        let drive_id = drives::my(user_id);
        let stored = StoredRoot::of(self.db.reader(), user_id)
            .await?
            .ok_or(FileError::NotFound)?;
        if self.root_locked(&stored) {
            return Err(FileError::Invalid(
                "The files folder is set by FILES_DIR".into(),
            ));
        }
        let root = match root {
            Some(root) => Some(self.check_root(root)?),
            None => None,
        };
        if let Some(root) = &root {
            self.check_apart(drive_id, root).await?;
        }
        // Wait for what is running on the old tree, then forget it.
        let old = self.trees.lock().unwrap().remove(&drive_id);
        let _guard = match &old {
            Some(tree) => Some(tree.read_lock().await),
            None => None,
        };
        let old_root = match old.as_ref().and_then(|t| t.storage.local_root()) {
            Some(root) => Some(root.to_path_buf()),
            None => self.drive_root(drive_id).await.ok().map(|(root, _)| root),
        };
        let mut tx = self.db.writer().begin().await?;
        sqlx::query("UPDATE users SET files_root = ?1 WHERE id = ?2")
            .bind(root.as_ref().map(|r| r.to_string_lossy().into_owned()))
            .bind(user_id)
            .execute(&mut *tx)
            .await?;
        // The shares of the old tree go from the recipients' files.
        shares::end_mounts_on(&mut tx, drive_id).await?;
        // Every cursor of the old tree is past: clients list everything.
        sqlx::query("UPDATE users SET feed_epoch = feed_epoch + 1 WHERE id = ?1")
            .bind(user_id)
            .execute(&mut *tx)
            .await?;
        for table in ["nodes", "journal"] {
            sqlx::query(sqlx::AssertSqlSafe(format!(
                "DELETE FROM {table} WHERE drive_id = ?1"
            )))
            .bind(drive_id)
            .execute(&mut *tx)
            .await?;
        }
        sqlx::query("DELETE FROM changes WHERE user_id = ?1")
            .bind(user_id)
            .execute(&mut *tx)
            .await?;
        tx.commit().await?;
        // The trash and the versions are the drive's, wherever its tree is.
        let (new_root, default) = self.drive_root(drive_id).await?;
        if default {
            std::fs::create_dir_all(&new_root)?;
        }
        if let Some(old_root) = old_root
            && old_root != new_root
        {
            drives::move_organizer_dir(
                &old_root.join(ORGANIZER_DIR),
                &new_root.join(ORGANIZER_DIR),
            );
        }
        drop(_guard);
        self.notify(drive_id);
        let files = self.clone();
        tokio::spawn(async move { files.open_and_scan(drive_id).await });
        Ok(())
    }

    /// Whether a folder can be a files root: an existing folder Organizer
    /// can write to, apart from the data dir and the database. Answers its
    /// canonical path.
    pub fn check_root(&self, root: &Path) -> Result<PathBuf, FileError> {
        if !root.is_absolute() {
            return Err(FileError::Invalid("Expected an absolute path".into()));
        }
        let root = root
            .canonicalize()
            .map_err(|_| FileError::Invalid(format!("{} does not exist", root.display())))?;
        if !root.is_dir() {
            return Err(FileError::Invalid(format!(
                "{} is not a folder",
                root.display()
            )));
        }
        let data_dir = self
            .data_dir
            .canonicalize()
            .unwrap_or_else(|_| self.data_dir.clone());
        if root.starts_with(&data_dir) || data_dir.starts_with(&root) {
            return Err(FileError::Invalid(
                "The files folder and the data folder (DATA_DIR) must be apart: neither inside the other".into(),
            ));
        }
        // Organizer must be able to write there: try it.
        let probe = root.join(format!(".organizer-write-test-{}", Uuid::now_v7()));
        std::fs::write(&probe, b"")
            .and_then(|()| std::fs::remove_file(&probe))
            .map_err(|e| {
                FileError::Invalid(format!("Organizer cannot write to {}: {e}", root.display()))
            })?;
        Ok(root)
    }

    /// A drive's folder must be apart from every other drive's (neither
    /// inside the other): two trees over the same files would each take
    /// the other's changes for their own.
    pub async fn check_apart(&self, drive_id: Uuid, root: &Path) -> Result<(), FileError> {
        // Drives in a backend have no folder here.
        let others: Vec<Uuid> = sqlx::query_scalar(
            "SELECT id FROM drives WHERE id != ?1 AND backend_id IS NULL AND kind != 'remote'",
        )
        .bind(drive_id)
        .fetch_all(self.db.reader())
        .await?;
        for other in others {
            let Ok((theirs, _)) = self.drive_root(other).await else {
                continue;
            };
            let theirs = theirs.canonicalize().unwrap_or(theirs);
            if root.starts_with(&theirs) || theirs.starts_with(root) {
                return Err(FileError::Invalid(
                    "That folder is another drive's folder, or holds or is inside one".into(),
                ));
            }
        }
        Ok(())
    }

    /// Forget a drive's tree (the drive or its user is being removed):
    /// what runs on it ends first.
    pub async fn forget(&self, drive_id: Uuid) {
        let old = self.trees.lock().unwrap().remove(&drive_id);
        if let Some(tree) = old {
            let _guard = tree.read_lock().await;
        }
    }
}

/// What a user's row says of where their tree is.
#[derive(Clone, Debug, Default, sqlx::FromRow)]
pub struct StoredRoot {
    /// The folder a superadmin chose; NULL: the default in the data dir.
    pub files_root: Option<String>,
    /// The first superadmin's: `FILES_DIR`, when it is set, is theirs.
    pub owns_files_dir: bool,
}

impl StoredRoot {
    /// A user's, if the user exists.
    pub async fn of<'e>(
        db: impl sqlx::SqliteExecutor<'e>,
        user_id: Uuid,
    ) -> sqlx::Result<Option<StoredRoot>> {
        sqlx::query_as("SELECT files_root, owns_files_dir FROM users WHERE id = ?1")
            .bind(user_id)
            .fetch_optional(db)
            .await
    }
}

/// Delete the change feed's rows that are older than it keeps, and raise
/// each user's floor past them: an older cursor is answered with 410.
pub async fn prune_changes(db: &Db) -> sqlx::Result<()> {
    let cutoff = crate::auth::iso(SystemTime::now() - CHANGES_KEPT);
    let mut tx = db.writer().begin().await?;
    sqlx::query(
        "UPDATE users SET feed_floor = max(feed_floor, (
           SELECT max(seq) FROM changes WHERE user_id = users.id AND at < ?1))
         WHERE EXISTS (SELECT 1 FROM changes WHERE user_id = users.id AND at < ?1)",
    )
    .bind(&cutoff)
    .execute(&mut *tx)
    .await?;
    sqlx::query("DELETE FROM changes WHERE at < ?1")
        .bind(&cutoff)
        .execute(&mut *tx)
        .await?;
    tx.commit().await
}

/// One row of the change feed: what happened to a node, and where it is
/// after it (nothing for a deletion).
pub struct Change<'a> {
    pub node_id: Uuid,
    pub op: &'a str,
    pub kind: Option<Kind>,
    pub parent_id: Option<Uuid>,
    pub name: Option<&'a str>,
    pub version: Option<i64>,
    /// Where it was before a move: the recipients of a placed share see a
    /// node come into it, or go out of it (`shares::fan_out`).
    pub from_parent: Option<Uuid>,
    /// Where a moved node was, from its drive's root: a share with
    /// another server it left is told it went (`federation::outgoing`).
    pub from_path: Option<&'a str>,
}

/// Append to the change feed, inside the transaction of the change: a row
/// for each user who has the drive (My drive's user, a shared drive's
/// members) and sees the node, with the drive, and in the feeds of the
/// users who placed a share it is in. A deletion is recorded before the
/// node's row goes: who sees it is found from where it is. A move into or
/// out of what a user sees (a limited folder, upload only) is the node
/// appearing (with what is under it) or going for them.
pub async fn record_change(
    tx: &mut sqlx::SqliteConnection,
    drive_id: Uuid,
    change: Change<'_>,
) -> sqlx::Result<()> {
    shares::fan_out(&mut *tx, &change).await?;
    // The shares with other servers it is in: their sync-collection.
    crate::federation::outgoing::log_change(&mut *tx, drive_id, &change).await?;
    // Nothing hidden in the drive: every user of it gets the row.
    let hides: bool = sqlx::query_scalar(
        "SELECT EXISTS (SELECT 1 FROM nodes WHERE drive_id = ?1 AND limited = 1)
             OR EXISTS (SELECT 1 FROM drive_users WHERE drive_id = ?1 AND role = 'upload_only')",
    )
    .bind(drive_id)
    .fetch_one(&mut *tx)
    .await?;
    if !hides {
        sqlx::query(
            "INSERT INTO changes (user_id, drive_id, node_id, op, kind, parent_id, name, version)
             SELECT DISTINCT user_id, ?1, ?2, ?3, ?4, ?5, ?6, ?7 FROM drive_users
             WHERE drive_id = ?1",
        )
        .bind(drive_id)
        .bind(change.node_id)
        .bind(change.op)
        .bind(change.kind.map(Kind::as_str))
        .bind(change.parent_id)
        .bind(change.name)
        .bind(change.version)
        .execute(&mut *tx)
        .await?;
        return Ok(());
    }
    let readers: Vec<Uuid> =
        sqlx::query_scalar("SELECT DISTINCT user_id FROM drive_users WHERE drive_id = ?1")
            .bind(drive_id)
            .fetch_all(&mut *tx)
            .await?;
    let id = change.node_id;
    for reader in readers {
        let now = roles::sees(&mut *tx, reader, id).await?;
        let before = match (change.op, change.from_parent) {
            ("move", Some(from)) => roles::resolve_under(&mut *tx, reader, id, from)
                .await?
                .is_some(),
            _ => now,
        };
        match (before, now) {
            (true, true) => {
                feed_row(
                    &mut *tx,
                    reader,
                    drive_id,
                    id,
                    change.op,
                    change.kind,
                    change.parent_id,
                    change.name,
                    change.version,
                )
                .await?
            }
            (false, true) => appear_in_drive(&mut *tx, reader, drive_id, id).await?,
            (true, false) => {
                feed_row(
                    &mut *tx, reader, drive_id, id, "delete", None, None, None, None,
                )
                .await?
            }
            (false, false) => {}
        }
    }
    Ok(())
}

/// One row of a user's change feed.
#[allow(clippy::too_many_arguments)]
pub(crate) async fn feed_row(
    tx: &mut sqlx::SqliteConnection,
    user: Uuid,
    drive_id: Uuid,
    node_id: Uuid,
    op: &str,
    kind: Option<Kind>,
    parent_id: Option<Uuid>,
    name: Option<&str>,
    version: Option<i64>,
) -> sqlx::Result<()> {
    sqlx::query(
        "INSERT INTO changes (user_id, drive_id, node_id, op, kind, parent_id, name, version)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8)",
    )
    .bind(user)
    .bind(drive_id)
    .bind(node_id)
    .bind(op)
    .bind(kind.map(Kind::as_str))
    .bind(parent_id)
    .bind(name)
    .bind(version)
    .execute(tx)
    .await?;
    Ok(())
}

/// A node and what is under it (as far as the user sees it) appear in a
/// drive user's feed, the parents first: it came into what they see.
pub(crate) async fn appear_in_drive(
    tx: &mut sqlx::SqliteConnection,
    user: Uuid,
    drive_id: Uuid,
    top: Uuid,
) -> sqlx::Result<()> {
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
        if !roles::sees(&mut *tx, user, row.id).await? {
            continue;
        }
        feed_row(
            &mut *tx,
            user,
            drive_id,
            row.id,
            "create",
            Some(row.kind),
            row.parent_id,
            Some(&row.name),
            Some(row.version),
        )
        .await?;
    }
    Ok(())
}

/// The names from the root down to a node (empty for the root).
pub async fn path_of<'e>(
    db: impl sqlx::SqliteExecutor<'e>,
    node_id: Uuid,
) -> sqlx::Result<Vec<String>> {
    sqlx::query_scalar(
        "WITH RECURSIVE up (id, parent_id, name, depth) AS (
           SELECT id, parent_id, name, 0 FROM nodes WHERE id = ?1
           UNION ALL
           SELECT n.id, n.parent_id, n.name, up.depth + 1
           FROM nodes n JOIN up ON n.id = up.parent_id
         )
         SELECT name FROM up WHERE parent_id IS NOT NULL ORDER BY depth DESC",
    )
    .bind(node_id)
    .fetch_all(db)
    .await
}

/// The modification time of a file's metadata, in nanoseconds since 1970.
pub fn mtime_ns(metadata: &std::fs::Metadata) -> i64 {
    metadata
        .modified()
        .ok()
        .and_then(|t| t.duration_since(UNIX_EPOCH).ok())
        .map(|d| d.as_nanos() as i64)
        .unwrap_or(0)
}

/// Move a file or folder, also from one filesystem to another (a files
/// root on a share, the trash on the local disk), where a rename cannot.
pub fn move_path(from: &Path, to: &Path) -> std::io::Result<()> {
    match std::fs::rename(from, to) {
        Ok(()) => Ok(()),
        // EXDEV: another filesystem. Copy, then remove the original.
        Err(e) if e.raw_os_error() == Some(18) => {
            copy_all(from, to)?;
            if from.is_dir() {
                std::fs::remove_dir_all(from)
            } else {
                std::fs::remove_file(from)
            }
        }
        Err(e) => Err(e),
    }
}

fn copy_all(from: &Path, to: &Path) -> std::io::Result<()> {
    let metadata = std::fs::symlink_metadata(from)?;
    if metadata.is_dir() {
        std::fs::create_dir(to)?;
        for entry in std::fs::read_dir(from)? {
            let entry = entry?;
            copy_all(&entry.path(), &to.join(entry.file_name()))?;
        }
        Ok(())
    } else {
        std::fs::copy(from, to).map(|_| ())
    }
}

#[cfg(test)]
mod tests;
