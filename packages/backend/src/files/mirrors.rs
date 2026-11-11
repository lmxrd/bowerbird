//! Mirrors (docs/M7D_DESIGN.md): a one-way, read-only copy of a drive in
//! one of its user's own storage backends, as plain files at their paths
//! under `<prefix>/<mirror id>/`.
//!
//! **A mirror is a drive** (`kind = 'mirror'`, the `mirrors` row beside
//! it, the same id): its tree, trash and versions are a drive's, in its
//! backend, and its user has it as a viewer, so browsing and downloading
//! are the files API's and every write is refused by the roles. Only the
//! copy writes there, through the operations of `ops.rs` (journaled, with
//! the trash and the versions): a file deleted or replaced in the drive
//! goes to the mirror's trash or versions, kept by its user's retention
//! (`retention::rules`).
//!
//! **What is copied** is what the user sees of the drive (`roles.rs`):
//! limited folders they are not in, and others' uploads where they may only
//! upload, are left out; placed shares are not nodes of My drive, so they
//! are left out too. Encrypted folders are copied as the server holds them;
//! each node of a mirror knows the node it copies (`nodes.source_id`), the
//! ids an encrypted folder's keys and names are sealed with.
//!
//! **A copy** compares the drive's index with the mirror's, by the node
//! each copy is of: what is new is copied, what changed (its hash) is
//! written over (the old content becomes a version), what moved is moved,
//! and what the user no longer sees goes to the mirror's trash. A node of
//! the mirror that copies nothing yet (a stop between writing a file and
//! indexing it, then found by the scan) is taken for the drive's node of
//! the same name and kind in the same folder, so a copy a stop cut short
//! is finished where it was, not done again.
//!
//! **When** ([`Schedule`]): the user's change feed tells whether the drive
//! changed since the last copy (`mirrors.seen_seq`); [`Files::run_mirrors`]
//! (every [`TICK`], and when woken) copies those that are due. A copy a
//! stop interrupted (`running_since`) is finished at the next start,
//! whatever the schedule. A mirror whose storage (or whose drive's) is
//! locked waits until it is unlocked.
//!
//! **Stopped** ([`Stopped`]): the drive was deleted, the user lost access
//! to it (or has upload only), its managers forbid mirroring it
//! (`mirrors.allowed`), or mirroring shared drives is off
//! (`mirrors.shared_drives`). The mirror stays, with what it has; one whose
//! reason goes (access given back, the setting on again) goes on. When
//! it stopped (`stopped_at`) is noted the first time a look finds it
//! stopped ([`Files::mirror_stop`]: the loop's, every [`TICK`], or a
//! client's), and at once when the drive is deleted (a trigger,
//! migration 0032); it is cleared when the mirror goes on.

use super::drives::{self, DriveKind, DriveRow};
use super::ops::WriteTarget;
use super::roles::{self, Role};
use super::{FileError, Files, Kind, Node, Tree};
use crate::storage::{Storage, join};
use serde::{Deserialize, Serialize};
use std::collections::{HashMap, HashSet, VecDeque};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::time::{Duration, SystemTime};
use utoipa::ToSchema;
use uuid::Uuid;

/// How often the mirrors are looked at: a continuous one copies a change
/// within about a minute (the change, the next look, the copy).
pub const TICK: Duration = Duration::from_secs(20);

/// How long a failed copy waits before it is tried again.
const RETRY: Duration = Duration::from_secs(5 * 60);

/// The refusal (a conflict) of what waits for a mirror's copy to end:
/// removing or ejecting it.
pub const COPY_UNDER_WAY: &str = "A copy is under way: try again once it is done";

/// When a mirror copies.
#[derive(
    Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize, ToSchema, sqlx::Type,
)]
#[serde(rename_all = "snake_case")]
#[sqlx(type_name = "TEXT", rename_all = "snake_case")]
#[schema(as = MirrorSchedule)]
pub enum Schedule {
    /// Within about a minute of a change in the drive (the default).
    #[default]
    Continuous,
    /// At most once an hour, when something changed.
    Hourly,
    /// At most once a day, when something changed.
    Daily,
    /// Only when asked ("Mirror now": `POST /mirrors/{id}/run`).
    Manual,
}

impl Schedule {
    /// How long after the last copy the next may be (None: only when
    /// asked).
    fn every(self) -> Option<Duration> {
        match self {
            Schedule::Continuous => Some(Duration::ZERO),
            Schedule::Hourly => Some(Duration::from_secs(3_600)),
            Schedule::Daily => Some(Duration::from_secs(86_400)),
            Schedule::Manual => None,
        }
    }
}

/// Why a mirror stopped copying, for a client to act on (the words are
/// [`Stopped::reason`]).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, ToSchema)]
#[serde(rename_all = "snake_case")]
#[schema(as = MirrorStopped)]
pub enum Stopped {
    /// The drive was deleted: the mirror keeps what it had.
    DriveDeleted,
    /// The user no longer has the drive, or only to upload.
    AccessLost,
    /// The drive's managers forbid mirroring it (`mirrors.allowed`).
    Forbidden,
    /// Mirroring shared drives is not allowed here
    /// (`mirrors.shared_drives`).
    SharedDrivesOff,
}

impl Stopped {
    /// The code a refusal carries (`ErrorBody.code`).
    pub fn code(self) -> &'static str {
        match self {
            Stopped::DriveDeleted => "drive_deleted",
            Stopped::AccessLost => "access_lost",
            Stopped::Forbidden => "mirroring_forbidden",
            Stopped::SharedDrivesOff => "shared_drives_off",
        }
    }

    /// Why, fit to show.
    pub fn reason(self) -> &'static str {
        match self {
            Stopped::DriveDeleted => "The drive was deleted",
            Stopped::AccessLost => "You no longer have this drive (or may only upload to it)",
            Stopped::Forbidden => "The drive's managers do not allow mirroring it",
            Stopped::SharedDrivesOff => "Mirroring shared drives is not allowed here",
        }
    }
}

/// Where a mirror is, for a client to show (and act on).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, ToSchema)]
#[serde(rename_all = "snake_case")]
pub enum MirrorStatus {
    /// It holds what the drive has (as of its last copy).
    UpToDate,
    /// The drive changed (or a copy was asked for): the next copy, by the
    /// schedule or "Mirror now", takes it.
    Pending,
    /// Copying now (`progress`).
    Copying,
    /// Its storage (or the drive's) has private credentials no session
    /// holds now: it copies once its owner signs in and unlocks them.
    WaitingForUnlock,
    /// The last copy failed (`error` says why); tried again in a few
    /// minutes.
    Failing,
    /// It copies no more (`stopped` says why); what it has stays.
    Stopped,
}

/// How far a copy is.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, ToSchema)]
#[schema(as = MirrorProgress)]
pub struct Progress {
    /// Files copied (new or changed) so far, of `files_total`.
    pub files_done: i64,
    pub files_total: i64,
    /// Their bytes.
    pub bytes_done: i64,
    pub bytes_total: i64,
}

/// Where a mirror is now ([`Files::mirror_status`]).
#[derive(Clone, Debug)]
pub struct MirrorState {
    pub status: MirrorStatus,
    /// Why it stopped, with `status` `stopped`.
    pub stopped: Option<Stopped>,
    /// When it stopped, with `stopped`.
    pub stopped_at: Option<String>,
    /// How far its copy is, with `status` `copying`.
    pub progress: Option<Progress>,
}

/// What runs now (in memory): the copies under way, with their progress,
/// and the wake-up of the loop that starts them.
#[derive(Default)]
pub struct Mirroring {
    copies: Mutex<HashMap<Uuid, Progress>>,
    wake: tokio::sync::Notify,
}

impl Mirroring {
    /// Claim a mirror for a copy (or an eject): false when one runs.
    fn claim(&self, id: Uuid) -> bool {
        let mut copies = self.copies.lock().expect("not poisoned");
        if copies.contains_key(&id) {
            return false;
        }
        copies.insert(id, Progress::default());
        true
    }

    fn release(&self, id: Uuid) {
        self.copies.lock().expect("not poisoned").remove(&id);
    }

    /// A copy's progress, while it runs.
    pub fn progress(&self, id: Uuid) -> Option<Progress> {
        self.copies.lock().expect("not poisoned").get(&id).copied()
    }

    fn update(&self, id: Option<Uuid>, change: impl FnOnce(&mut Progress)) {
        if let Some(id) = id
            && let Some(progress) = self.copies.lock().expect("not poisoned").get_mut(&id)
        {
            change(progress);
        }
    }
}

/// A row of `mirrors`.
#[derive(Clone, Debug, sqlx::FromRow)]
pub struct MirrorRow {
    pub id: Uuid,
    pub user_id: Uuid,
    pub source_id: Option<Uuid>,
    pub schedule: Schedule,
    pub seen_seq: i64,
    pub again: bool,
    pub requested: bool,
    pub running_since: Option<String>,
    pub last_try_at: Option<String>,
    pub last_copy_at: Option<String>,
    pub error: Option<String>,
    pub created_at: String,
    /// When it was first seen stopped (migration 0032); NULL while it goes on.
    pub stopped_at: Option<String>,
}

const MIRROR_COLUMNS: &str = "id, user_id, source_id, schedule, seen_seq, again, requested, \
     running_since, last_try_at, last_copy_at, error, created_at, stopped_at";

impl MirrorRow {
    pub async fn get<'e>(
        db: impl sqlx::SqliteExecutor<'e>,
        id: Uuid,
    ) -> sqlx::Result<Option<MirrorRow>> {
        sqlx::query_as(sqlx::AssertSqlSafe(format!(
            "SELECT {MIRROR_COLUMNS} FROM mirrors WHERE id = ?1"
        )))
        .bind(id)
        .fetch_optional(db)
        .await
    }

    /// A user's mirrors, the oldest first.
    pub async fn of_user<'e>(
        db: impl sqlx::SqliteExecutor<'e>,
        user: Uuid,
    ) -> sqlx::Result<Vec<MirrorRow>> {
        sqlx::query_as(sqlx::AssertSqlSafe(format!(
            "SELECT {MIRROR_COLUMNS} FROM mirrors WHERE user_id = ?1 ORDER BY created_at, id"
        )))
        .bind(user)
        .fetch_all(db)
        .await
    }
}

/// A node of the drive mirrored that the user sees, with its path there.
#[derive(Clone, Debug)]
struct Seen {
    id: Uuid,
    parent: Option<Uuid>,
    name: String,
    kind: Kind,
    size: i64,
    hash: Option<Vec<u8>>,
    path: String,
    /// The top of an encrypted folder.
    sealed: bool,
}

/// A node of the mirror, as the copy keeps track of it.
#[derive(Clone, Debug, sqlx::FromRow)]
struct Copied {
    id: Uuid,
    parent_id: Option<Uuid>,
    name: String,
    kind: Kind,
    size: i64,
    hash: Option<Vec<u8>>,
    version: i64,
    source_id: Option<Uuid>,
}

/// The mirror's index during a copy: its nodes, what is in each folder,
/// and which copies which.
struct Index {
    nodes: HashMap<Uuid, Copied>,
    children: HashMap<Uuid, HashSet<Uuid>>,
    by_source: HashMap<Uuid, Uuid>,
}

impl Index {
    fn new(copied: Vec<Copied>) -> Index {
        let mut index = Index {
            nodes: HashMap::with_capacity(copied.len()),
            children: HashMap::new(),
            by_source: HashMap::new(),
        };
        for node in copied {
            index.insert(node);
        }
        index
    }

    fn insert(&mut self, node: Copied) {
        if let Some(old) = self.nodes.get(&node.id)
            && let Some(parent) = old.parent_id
            && let Some(siblings) = self.children.get_mut(&parent)
        {
            siblings.remove(&node.id);
        }
        if let Some(parent) = node.parent_id {
            self.children.entry(parent).or_default().insert(node.id);
        }
        if let Some(source) = node.source_id {
            self.by_source.insert(source, node.id);
        }
        self.nodes.insert(node.id, node);
    }

    fn child_named(&self, parent: Uuid, name: &str, except: Option<Uuid>) -> Option<&Copied> {
        self.children
            .get(&parent)?
            .iter()
            .filter_map(|id| self.nodes.get(id))
            .find(|n| n.name == name && Some(n.id) != except)
    }

    /// A node and what is under it.
    fn subtree(&self, top: Uuid) -> Vec<Uuid> {
        let mut out = vec![top];
        let mut at = 0;
        while at < out.len() {
            if let Some(children) = self.children.get(&out[at]) {
                out.extend(children.iter().copied());
            }
            at += 1;
        }
        out
    }

    /// A node and what is under it went (to the trash).
    fn forget(&mut self, top: Uuid) {
        if let Some(parent) = self.nodes.get(&top).and_then(|n| n.parent_id)
            && let Some(siblings) = self.children.get_mut(&parent)
        {
            siblings.remove(&top);
        }
        for id in self.subtree(top) {
            self.children.remove(&id);
            if let Some(node) = self.nodes.remove(&id)
                && let Some(source) = node.source_id
                && self.by_source.get(&source) == Some(&id)
            {
                self.by_source.remove(&source);
            }
        }
    }

    fn set(&mut self, node: &Node, hash: Option<Vec<u8>>) {
        self.insert(Copied {
            id: node.id,
            parent_id: node.parent_id,
            name: node.name.clone(),
            kind: node.kind,
            size: node.size,
            hash,
            version: node.version,
            source_id: node.source_id,
        });
    }
}

/// Whether a step of a copy that failed stops the whole copy (the storage
/// fails or is full, the database), or only skips that item for the next
/// copy (it changed or went meanwhile).
fn stops_the_copy(e: &FileError) -> bool {
    !matches!(
        e,
        FileError::NotFound | FileError::Conflict(_) | FileError::Invalid(_)
    )
}

/// Why a copy failed, fit to show.
fn failure(e: &FileError) -> String {
    match e {
        FileError::Down(reason) => reason.clone(),
        FileError::Full => "Not enough room in the storage the mirror is in".into(),
        FileError::Unavailable(_) => "The drive's folder is not available".into(),
        _ => "The copy failed; it is tried again in a few minutes".into(),
    }
}

impl Files {
    /// The mirrors under way, and the loop's wake-up.
    pub fn mirroring(&self) -> &Mirroring {
        &self.mirroring
    }

    /// Look at the mirrors now rather than at the next tick.
    pub fn wake_mirrors(&self) {
        self.mirroring.wake.notify_one();
    }

    /// Every [`TICK`] (or when woken): the mirrors that are due copy, each
    /// as a task of its own. The first look is at once: a copy a stop
    /// interrupted is finished.
    pub(crate) fn spawn_mirrors(self: &Arc<Self>) {
        let files = self.clone();
        tokio::spawn(async move {
            loop {
                let tick = files.clone();
                tokio::spawn(async move { tick.run_mirrors().await });
                tokio::select! {
                    _ = tokio::time::sleep(TICK) => {}
                    _ = files.mirroring.wake.notified() => {}
                }
            }
        });
    }

    /// Copy every mirror that is due now, and wait for those copies.
    pub async fn run_mirrors(self: &Arc<Self>) {
        let rows: Vec<MirrorRow> = match sqlx::query_as(sqlx::AssertSqlSafe(format!(
            "SELECT {MIRROR_COLUMNS} FROM mirrors WHERE source_id IS NOT NULL"
        )))
        .fetch_all(self.db.reader())
        .await
        {
            Ok(rows) => rows,
            Err(e) => {
                tracing::warn!("cannot list the mirrors: {e}");
                return;
            }
        };
        let mut copies = Vec::new();
        for row in rows {
            match self.mirror_due(&row).await {
                Ok(true) => {}
                Ok(false) => continue,
                Err(e) => {
                    tracing::warn!("cannot tell whether mirror {} is due: {e}", row.id);
                    continue;
                }
            }
            if !self.mirroring.claim(row.id) {
                continue;
            }
            let files = self.clone();
            copies.push(tokio::spawn(async move {
                if let Err(e) = files.mirror_once(&row).await {
                    tracing::warn!("mirror {} could not copy: {e}", row.id);
                }
                files.mirroring.release(row.id);
                files.announce(row.user_id, super::Topic::Changes);
            }));
        }
        for copy in copies {
            let _ = copy.await;
        }
    }

    /// Why a user may not mirror a drive (or their mirror of it stops);
    /// None: they may.
    pub async fn mirror_refusal(
        &self,
        user: Uuid,
        source: Option<Uuid>,
    ) -> Result<Option<Stopped>, FileError> {
        let Some(source) = source else {
            return Ok(Some(Stopped::DriveDeleted));
        };
        let Some(row) = DriveRow::get(self.db.reader(), source).await? else {
            return Ok(Some(Stopped::DriveDeleted));
        };
        match row.kind {
            // My drive: by its user alone.
            DriveKind::My if row.user_id == Some(user) => Ok(None),
            DriveKind::My | DriveKind::Mirror | DriveKind::Remote => Ok(Some(Stopped::AccessLost)),
            DriveKind::Shared => {
                let role = roles::drive_role(self.db.reader(), user, source).await?;
                if role.is_none_or(|r| r < Role::Viewer) {
                    return Ok(Some(Stopped::AccessLost));
                }
                let env = crate::settings::Env::of_files(self);
                let to = crate::settings::For::drive(source);
                let on = crate::settings::resolve(&self.db, &env, "mirrors.shared_drives", to)
                    .await?
                    .bool();
                if !on {
                    return Ok(Some(Stopped::SharedDrivesOff));
                }
                let allowed = crate::settings::resolve(&self.db, &env, "mirrors.allowed", to)
                    .await?
                    .bool();
                if !allowed {
                    return Ok(Some(Stopped::Forbidden));
                }
                Ok(None)
            }
        }
    }

    /// Whether a mirror's storage, or its drive's, waits for its private
    /// credentials to be unlocked.
    pub async fn mirror_locked(&self, row: &MirrorRow) -> Result<bool, FileError> {
        for drive in [Some(row.id), row.source_id].into_iter().flatten() {
            if let Some((crate::backends::Unavailable::Locked, _)) = self.unavailable(drive).await?
            {
                return Ok(true);
            }
        }
        Ok(false)
    }

    /// The last row of the user's change feed about the drive mirrored.
    async fn feed_mark(&self, row: &MirrorRow) -> Result<i64, FileError> {
        Ok(sqlx::query_scalar(
            "SELECT coalesce(max(seq), 0) FROM changes WHERE user_id = ?1 AND drive_id = ?2",
        )
        .bind(row.user_id)
        .bind(row.source_id)
        .fetch_one(self.db.reader())
        .await?)
    }

    /// Whether the drive changed since the mirror's last complete copy (or
    /// it has none yet, or that copy left something for the next).
    pub async fn mirror_pending(&self, row: &MirrorRow) -> Result<bool, FileError> {
        Ok(row.last_copy_at.is_none() || row.again || self.feed_mark(row).await? > row.seen_seq)
    }

    /// Whether a mirror copies now: not stopped, not locked; a copy a stop
    /// interrupted, or one asked for; else by its schedule, when the drive
    /// changed, and not within a few minutes of a failed try.
    async fn mirror_due(&self, row: &MirrorRow) -> Result<bool, FileError> {
        if self.mirror_stop(row).await?.is_some() || self.mirror_locked(row).await? {
            return Ok(false);
        }
        if row.running_since.is_some() || row.requested {
            return Ok(true);
        }
        let Some(every) = row.schedule.every() else {
            return Ok(false);
        };
        let since = |at: &Option<String>, d: Duration| -> bool {
            at.as_deref()
                .is_none_or(|at| at <= crate::auth::iso(SystemTime::now() - d).as_str())
        };
        if row.error.is_some() && !since(&row.last_try_at, RETRY) {
            return Ok(false);
        }
        Ok(since(&row.last_copy_at, every) && self.mirror_pending(row).await?)
    }

    /// Why a mirror is stopped now, and since when (None: it goes on).
    /// The time is noted the first time a look finds it stopped (the
    /// drive's deletion notes its own: a trigger, migration 0032), and
    /// cleared when it goes on again, so that the next stop has its own.
    pub async fn mirror_stop(
        &self,
        row: &MirrorRow,
    ) -> Result<Option<(Stopped, String)>, FileError> {
        let stopped = self.mirror_refusal(row.user_id, row.source_id).await?;
        match (stopped, &row.stopped_at) {
            (Some(stopped), Some(at)) => Ok(Some((stopped, at.clone()))),
            (Some(stopped), None) => {
                let at: String = sqlx::query_scalar(
                    "UPDATE mirrors SET stopped_at = coalesce(stopped_at, ?2) WHERE id = ?1
                     RETURNING stopped_at",
                )
                .bind(row.id)
                .bind(crate::auth::iso(SystemTime::now()))
                .fetch_one(self.db.writer())
                .await?;
                Ok(Some((stopped, at)))
            }
            (None, Some(_)) => {
                sqlx::query("UPDATE mirrors SET stopped_at = NULL WHERE id = ?1")
                    .bind(row.id)
                    .execute(self.db.writer())
                    .await?;
                Ok(None)
            }
            (None, None) => Ok(None),
        }
    }

    /// Where a mirror is now, why and since when it stopped, and how far
    /// its copy is.
    pub async fn mirror_status(&self, row: &MirrorRow) -> Result<MirrorState, FileError> {
        let state = |status, progress| MirrorState {
            status,
            stopped: None,
            stopped_at: None,
            progress,
        };
        if let Some(progress) = self.mirroring.progress(row.id) {
            return Ok(state(MirrorStatus::Copying, Some(progress)));
        }
        if let Some((stopped, at)) = self.mirror_stop(row).await? {
            return Ok(MirrorState {
                status: MirrorStatus::Stopped,
                stopped: Some(stopped),
                stopped_at: Some(at),
                progress: None,
            });
        }
        if self.mirror_locked(row).await? {
            return Ok(state(MirrorStatus::WaitingForUnlock, None));
        }
        if row.error.is_some() {
            return Ok(state(MirrorStatus::Failing, None));
        }
        if row.requested || row.running_since.is_some() || self.mirror_pending(row).await? {
            return Ok(state(MirrorStatus::Pending, None));
        }
        Ok(state(MirrorStatus::UpToDate, None))
    }

    /// Make a mirror of a drive in a backend of the user's own (the
    /// caller checked they may): its drive, which they view, its root, and
    /// its row. The first copy is the loop's (woken now).
    pub async fn create_mirror(
        self: &Arc<Self>,
        user: Uuid,
        source: Uuid,
        backend: Uuid,
        schedule: Schedule,
    ) -> Result<Uuid, FileError> {
        let row = DriveRow::get(self.db.reader(), source)
            .await?
            .ok_or(FileError::NotFound)?;
        let id = Uuid::now_v7();
        let mut tx = self.db.writer().begin().await?;
        sqlx::query(
            "INSERT INTO drives (id, kind, name, created_by, backend_id, storage_limit)
             VALUES (?1, 'mirror', ?2, ?3, ?4, (SELECT drive_limit FROM backends WHERE id = ?4))",
        )
        .bind(id)
        .bind(drives::display_name(&row))
        .bind(user)
        .bind(backend)
        .execute(&mut *tx)
        .await?;
        sqlx::query(
            "INSERT INTO mirrors (id, user_id, source_id, schedule) VALUES (?1, ?2, ?3, ?4)",
        )
        .bind(id)
        .bind(user)
        .bind(source)
        .bind(schedule)
        .execute(&mut *tx)
        .await?;
        // The user views it: the roles refuse every write of theirs.
        sqlx::query(
            "INSERT INTO grants (id, drive_id, user_id, role, created_by)
             VALUES (?1, ?2, ?3, 'viewer', ?3)",
        )
        .bind(Uuid::now_v7())
        .bind(id)
        .bind(user)
        .execute(&mut *tx)
        .await?;
        tx.commit().await?;
        let root = match self.root_node(id).await {
            Ok(root) => root,
            Err(e) => {
                // Not left half made: nothing is in it yet.
                if let Err(e) = self.forget_drive(id).await {
                    tracing::warn!("cannot take back the mirror {id}: {e}");
                }
                return Err(e);
            }
        };
        // The user's apps get the drive.
        let mut tx = self.db.writer().begin().await?;
        drives::root_row(
            &mut tx,
            user,
            id,
            root.id,
            "create",
            Some(root.version),
            Some(&drives::display_name(&row)),
        )
        .await?;
        tx.commit().await?;
        self.announce(user, super::Topic::Changes);
        self.wake_mirrors();
        Ok(id)
    }

    /// Ask for a copy now ("Mirror now"): at the loop's next look, whatever
    /// the schedule (once the storage is unlocked).
    pub async fn request_mirror(&self, id: Uuid) -> Result<(), FileError> {
        sqlx::query("UPDATE mirrors SET requested = 1 WHERE id = ?1")
            .bind(id)
            .execute(self.db.writer())
            .await?;
        self.wake_mirrors();
        Ok(())
    }

    /// One copy of a mirror (claimed by the caller): its drive's changes
    /// since the last into it, then the row says how it went.
    async fn mirror_once(self: &Arc<Self>, row: &MirrorRow) -> Result<(), FileError> {
        let Some(source) = row.source_id else {
            return Ok(());
        };
        if self
            .mirror_refusal(row.user_id, row.source_id)
            .await?
            .is_some()
        {
            return Ok(());
        }
        // Read before the copy: a change made meanwhile is the next copy's.
        let mark = self.feed_mark(row).await?;
        let now = crate::auth::now_iso();
        sqlx::query(
            "UPDATE mirrors SET running_since = coalesce(running_since, ?2), last_try_at = ?2
             WHERE id = ?1",
        )
        .bind(row.id)
        .bind(&now)
        .execute(self.db.writer())
        .await?;
        let copied = self
            .copy_tree(source, row.id, row.user_id, Some(row.id))
            .await;
        // The user's keys of the encrypted folders copied so far, whatever
        // came of the copy: they open the copies once the drive's go.
        if let Err(e) = super::e2e::keep_for_mirrors(&self.db, Some(row.id), None).await {
            tracing::warn!("mirror {}: cannot keep the keys of its copies: {e}", row.id);
        }
        match &copied {
            Ok(again) => {
                let name = DriveRow::get(self.db.reader(), source)
                    .await?
                    .map(|d| drives::display_name(&d));
                sqlx::query(
                    "UPDATE mirrors SET running_since = NULL, requested = 0, error = NULL,
                       last_copy_at = ?2, seen_seq = max(seen_seq, ?3), again = ?4
                     WHERE id = ?1",
                )
                .bind(row.id)
                .bind(crate::auth::now_iso())
                .bind(mark)
                .bind(*again)
                .execute(self.db.writer())
                .await?;
                if let Some(name) = name {
                    sqlx::query("UPDATE drives SET name = ?2 WHERE id = ?1 AND kind = 'mirror'")
                        .bind(row.id)
                        .bind(name)
                        .execute(self.db.writer())
                        .await?;
                }
            }
            Err(e) => {
                sqlx::query(
                    "UPDATE mirrors SET running_since = NULL, requested = 0, error = ?2
                     WHERE id = ?1",
                )
                .bind(row.id)
                .bind(failure(e))
                .execute(self.db.writer())
                .await?;
            }
        }
        copied.map(|_| ())
    }

    /// What of a drive a user sees, the parents first (the root first): a
    /// folder hidden from them hides what is under it.
    async fn seen_nodes(&self, drive: Uuid, user: Uuid) -> Result<Vec<Seen>, FileError> {
        #[derive(sqlx::FromRow)]
        struct Row {
            id: Uuid,
            parent_id: Option<Uuid>,
            name: String,
            kind: Kind,
            size: i64,
            hash: Option<Vec<u8>>,
            limited: bool,
            created_by: Option<Uuid>,
            e2e_root: Option<Uuid>,
            granted: bool,
        }
        let rows: Vec<Row> = sqlx::query_as(
            "SELECT n.id, n.parent_id, n.name, n.kind, n.size, n.hash, n.limited, n.created_by,
                    n.e2e_root, EXISTS (SELECT 1 FROM grants g WHERE g.node_id = n.id) AS granted
             FROM nodes n WHERE n.drive_id = ?1",
        )
        .bind(drive)
        .fetch_all(self.db.reader())
        .await?;
        let mut children: HashMap<Option<Uuid>, Vec<Row>> = HashMap::new();
        for row in rows {
            children.entry(row.parent_id).or_default().push(row);
        }
        let Some(root) = children.remove(&None).and_then(|mut r| r.pop()) else {
            return Ok(Vec::new());
        };
        let mut conn = self.db.reader().acquire().await?;
        let Some(role) = roles::resolve(&mut conn, user, root.id).await? else {
            return Ok(Vec::new());
        };
        let mut out = vec![Seen {
            id: root.id,
            parent: None,
            name: String::new(),
            kind: Kind::Folder,
            size: 0,
            hash: None,
            path: String::new(),
            sealed: false,
        }];
        let mut queue = VecDeque::from([(root.id, role.role, String::new())]);
        while let Some((folder, role, path)) = queue.pop_front() {
            let mut rows = children.remove(&Some(folder)).unwrap_or_default();
            rows.sort_by(|a, b| a.name.cmp(&b.name));
            for row in rows {
                // As the listings: a child limited or with grants of its
                // own is resolved on its own, else it has its folder's
                // role, and upload only sees what the user added.
                let role = if row.limited || row.granted {
                    roles::resolve(&mut conn, user, row.id)
                        .await?
                        .map(|r| r.role)
                } else if role == Role::UploadOnly && row.created_by != Some(user) {
                    None
                } else {
                    Some(role)
                };
                let Some(role) = role else { continue };
                let child_path = join(&path, &row.name);
                if row.kind == Kind::Folder {
                    queue.push_back((row.id, role, child_path.clone()));
                }
                out.push(Seen {
                    id: row.id,
                    parent: Some(folder),
                    sealed: row.e2e_root == Some(row.id),
                    name: row.name,
                    kind: row.kind,
                    size: row.size,
                    hash: row.hash,
                    path: child_path,
                });
            }
        }
        Ok(out)
    }

    /// Copy what `user` sees of the drive `source` into the drive `target`
    /// (a mirror, or a drive ejected from one), each node of it knowing the
    /// one it copies. Answers whether something was left for the next copy
    /// (a file not hashed yet, or one that changed or went meanwhile);
    /// `progress`: the mirror whose progress it is.
    pub(crate) async fn copy_tree(
        self: &Arc<Self>,
        source: Uuid,
        target: Uuid,
        user: Uuid,
        progress: Option<Uuid>,
    ) -> Result<bool, FileError> {
        let from = self.tree(source).await?;
        // Not while a scan is changing the drive's index.
        let seen = {
            let _guard = from.read_lock().await;
            self.seen_nodes(source, user).await?
        };
        let root = self.root_node(target).await?;
        let copied: Vec<Copied> = sqlx::query_as(
            "SELECT id, parent_id, name, kind, size, hash, version, source_id FROM nodes
             WHERE drive_id = ?1",
        )
        .bind(target)
        .fetch_all(self.db.reader())
        .await?;
        let mut index = Index::new(copied);
        let visible: HashSet<Uuid> = seen.iter().map(|s| s.id).collect();
        let Some(source_root) = seen.first() else {
            // Nothing seen (no longer theirs): nothing changes here.
            return Ok(false);
        };
        let mut placed: HashMap<Uuid, Uuid> = HashMap::from([(source_root.id, root.id)]);
        let mut again = false;

        // What is to be copied, for the progress.
        let changed = |s: &Seen, index: &Index| -> bool {
            match index
                .by_source
                .get(&s.id)
                .and_then(|id| index.nodes.get(id))
            {
                None => true,
                Some(c) if c.kind != s.kind => true,
                Some(c) => match &s.hash {
                    Some(hash) => c.hash.as_ref() != Some(hash),
                    // Not hashed yet: a new size is a change; else the
                    // next copy looks again.
                    None => c.size != s.size,
                },
            }
        };
        let (files, bytes) = seen
            .iter()
            .filter(|s| s.kind == Kind::File && changed(s, &index))
            .fold((0, 0), |(n, b), s| (n + 1, b + s.size));
        self.mirroring.update(progress, |p| {
            p.files_total = files;
            p.bytes_total = bytes;
        });

        for s in seen.iter().skip(1) {
            let Some(&parent) = s.parent.as_ref().and_then(|p| placed.get(p)) else {
                // Its folder could not be copied: neither can it.
                continue;
            };
            let step = self
                .copy_node(
                    &from.storage,
                    target,
                    s,
                    parent,
                    &visible,
                    &mut index,
                    progress,
                )
                .await;
            match step {
                Ok((id, left)) => {
                    placed.insert(s.id, id);
                    again |= left;
                }
                Err(e) if stops_the_copy(&e) => return Err(e),
                Err(e) => {
                    tracing::info!("mirror {target}: {} is left for the next copy: {e}", s.path);
                    again = true;
                }
            }
        }

        // What the user no longer sees goes to the mirror's trash: the
        // topmost of each part that went.
        let gone: HashSet<Uuid> = index
            .nodes
            .values()
            .filter(|n| n.parent_id.is_some())
            .filter(|n| n.source_id.is_none_or(|s| !visible.contains(&s)))
            .map(|n| n.id)
            .collect();
        let tops: Vec<(Uuid, i64)> = index
            .nodes
            .values()
            .filter(|n| gone.contains(&n.id))
            .filter(|n| n.parent_id.is_none_or(|p| !gone.contains(&p)))
            .map(|n| (n.id, n.version))
            .collect();
        for (id, version) in tops {
            match self.trash(target, id, version).await {
                Ok(_) => index.forget(id),
                Err(e) if stops_the_copy(&e) => return Err(e),
                Err(e) => {
                    tracing::info!("mirror {target}: a deletion is left for the next copy: {e}");
                    again = true;
                }
            }
        }
        Ok(again)
    }

    /// Copy one node the user sees into the folder `parent` of the target:
    /// taken over, moved, written over, or made. Answers the copy's id, and
    /// whether something of it was left for the next copy.
    #[allow(clippy::too_many_arguments)]
    async fn copy_node(
        self: &Arc<Self>,
        from: &Storage,
        target: Uuid,
        s: &Seen,
        parent: Uuid,
        visible: &HashSet<Uuid>,
        index: &mut Index,
        progress: Option<Uuid>,
    ) -> Result<(Uuid, bool), FileError> {
        let mut copy = index.by_source.get(&s.id).copied();
        // A node of the same name and kind there that copies nothing the
        // user sees (written before a stop, found by the scan): this one's.
        if copy.is_none()
            && let Some(found) = index.child_named(parent, &s.name, None)
            && found.kind == s.kind
            && found
                .source_id
                .is_none_or(|other| !visible.contains(&other))
        {
            let found = found.id;
            sqlx::query("UPDATE nodes SET source_id = ?2 WHERE id = ?1")
                .bind(found)
                .bind(s.id)
                .execute(self.db.writer())
                .await?;
            let node = index.nodes.get_mut(&found).expect("in the index");
            if let Some(old) = node.source_id.replace(s.id) {
                index.by_source.remove(&old);
            }
            index.by_source.insert(s.id, found);
            copy = Some(found);
        }
        // A file that became a folder (or the other way): made anew.
        if let Some(id) = copy
            && index.nodes.get(&id).is_some_and(|c| c.kind != s.kind)
        {
            let version = index.nodes[&id].version;
            self.trash(target, id, version).await?;
            index.forget(id);
            copy = None;
        }
        let Some(id) = copy else {
            self.clear_way(target, parent, &s.name, None, visible, index)
                .await?;
            let node = match s.kind {
                Kind::Folder => {
                    let node = self
                        .copy_folder(target, parent, &s.name, s.id, s.sealed)
                        .await?;
                    index.set(&node, None);
                    node
                }
                Kind::File => {
                    let target_of = WriteTarget::Copy {
                        parent,
                        name: s.name.clone(),
                        source: s.id,
                    };
                    self.copy_content(from, target, s, target_of, index, progress)
                        .await?
                }
            };
            return Ok((node.id, false));
        };
        let here = index.nodes[&id].clone();
        if here.parent_id != Some(parent) || here.name != s.name {
            self.clear_way(target, parent, &s.name, Some(id), visible, index)
                .await?;
            let node = self
                .move_copy(target, id, parent, &s.name, here.version)
                .await?;
            index.set(&node, here.hash.clone());
        }
        if s.kind == Kind::File {
            match &s.hash {
                Some(hash) if here.hash.as_ref() != Some(hash) => {}
                Some(_) => return Ok((id, false)),
                // Not hashed yet: the next copy looks again, unless its
                // size tells already.
                None if here.size == s.size => return Ok((id, true)),
                None => {}
            }
            let version = index.nodes[&id].version;
            self.copy_content(
                from,
                target,
                s,
                WriteTarget::Replace {
                    id,
                    if_version: version,
                    source: None,
                },
                index,
                progress,
            )
            .await?;
        }
        Ok((id, false))
    }

    /// The bytes of a file the user sees, written into the target (over
    /// its copy, which keeps the old ones as a version, or as a new file).
    async fn copy_content(
        self: &Arc<Self>,
        from: &Storage,
        target: Uuid,
        s: &Seen,
        to: WriteTarget,
        index: &mut Index,
        progress: Option<Uuid>,
    ) -> Result<Node, FileError> {
        let temp = self.temp_file()?;
        if let Err(e) = from.copy_out(&s.path, &temp).await {
            let _ = std::fs::remove_file(&temp);
            return Err(match e.kind() {
                std::io::ErrorKind::NotFound => FileError::NotFound,
                _ => e.into(),
            });
        }
        let size = std::fs::metadata(&temp)
            .map(|m| m.len() as i64)
            .unwrap_or(0);
        let node = self.write_file(target, to, &temp).await?;
        let hash: Option<Vec<u8>> = sqlx::query_scalar("SELECT hash FROM nodes WHERE id = ?1")
            .bind(node.id)
            .fetch_optional(self.db.reader())
            .await?
            .flatten();
        index.set(&node, hash);
        self.mirroring.update(progress, |p| {
            p.files_done += 1;
            p.bytes_done += size;
        });
        Ok(node)
    }

    /// A place for a copy in the uploads folder (moved into the tree from
    /// there, as every write).
    fn temp_file(&self) -> Result<PathBuf, FileError> {
        let dir = self.uploads_dir();
        std::fs::create_dir_all(&dir)?;
        Ok(dir.join(format!("mirror-{}", Uuid::now_v7())))
    }

    /// Make room for a name in a folder of the target: what is there and
    /// copies nothing the user sees goes to the trash (when nothing under
    /// it is still to be moved out); a copy that is going elsewhere steps
    /// aside under another name until it is moved there.
    async fn clear_way(
        self: &Arc<Self>,
        target: Uuid,
        parent: Uuid,
        name: &str,
        except: Option<Uuid>,
        visible: &HashSet<Uuid>,
        index: &mut Index,
    ) -> Result<(), FileError> {
        let Some(there) = index.child_named(parent, name, except).cloned() else {
            return Ok(());
        };
        let still_seen = index.subtree(there.id).iter().any(|id| {
            index.nodes[id]
                .source_id
                .is_some_and(|s| visible.contains(&s))
        });
        if !still_seen {
            self.trash(target, there.id, there.version).await?;
            index.forget(there.id);
            return Ok(());
        }
        let aside = format!("{name}~{}", &there.id.simple().to_string()[..8]);
        let node = self
            .move_copy(target, there.id, parent, &aside, there.version)
            .await?;
        index.set(&node, there.hash.clone());
        Ok(())
    }

    /// Remove a mirror with its copy (its files, trash and versions in its
    /// storage, when that answers). Not while it copies.
    pub async fn delete_mirror(&self, mirror: Uuid) -> Result<(), FileError> {
        if !self.mirroring.claim(mirror) {
            return Err(FileError::Conflict(COPY_UNDER_WAY.into()));
        }
        let deleted = self.delete_drive(mirror).await;
        self.mirroring.release(mirror);
        deleted
    }

    /// Make a mirror a drive where it is ("Eject: make it a drive"): a
    /// shared drive on the same storage, at the same place, its user its
    /// manager; nothing is copied, and mirroring stops.
    pub async fn eject_in_place(&self, mirror: Uuid, name: &str) -> Result<(), FileError> {
        let name = drives::clean_name(name)?;
        let row = MirrorRow::get(self.db.reader(), mirror)
            .await?
            .ok_or(FileError::NotFound)?;
        if !self.mirroring.claim(mirror) {
            return Err(FileError::Conflict(COPY_UNDER_WAY.into()));
        }
        let made = async {
            let mut tx = self.db.writer().begin().await?;
            let done = sqlx::query(
                "UPDATE drives SET kind = 'shared', name = ?2 WHERE id = ?1 AND kind = 'mirror'",
            )
            .bind(mirror)
            .bind(&name)
            .execute(&mut *tx)
            .await;
            match done {
                Ok(done) if done.rows_affected() == 0 => return Err(FileError::NotFound),
                Ok(_) => {}
                Err(sqlx::Error::Database(e)) if e.is_unique_violation() => {
                    return Err(FileError::Conflict(
                        "A shared drive has that name already: choose another".into(),
                    ));
                }
                Err(e) => return Err(e.into()),
            }
            sqlx::query("DELETE FROM mirrors WHERE id = ?1")
                .bind(mirror)
                .execute(&mut *tx)
                .await?;
            sqlx::query(
                "UPDATE grants SET role = 'manager'
                 WHERE drive_id = ?1 AND node_id IS NULL AND user_id = ?2",
            )
            .bind(mirror)
            .bind(row.user_id)
            .execute(&mut *tx)
            .await?;
            tx.commit().await?;
            Ok(())
        }
        .await;
        self.mirroring.release(mirror);
        made?;
        self.announce(row.user_id, super::Topic::Changes);
        Ok(())
    }

    /// Make a new drive of a mirror's current contents ("Eject: make a
    /// copy"), on the same storage, its user its manager; the mirror goes
    /// on. Each node of it knows the node of the drive mirrored it copies
    /// (an encrypted folder's ids). Not while it copies, and the mirror
    /// does not copy meanwhile (it is claimed).
    pub async fn eject_copy(
        self: &Arc<Self>,
        mirror: Uuid,
        name: &str,
        user: Uuid,
    ) -> Result<Uuid, FileError> {
        let drive = DriveRow::get(self.db.reader(), mirror)
            .await?
            .filter(|d| d.kind == DriveKind::Mirror)
            .ok_or(FileError::NotFound)?;
        if !self.mirroring.claim(mirror) {
            return Err(FileError::Conflict(COPY_UNDER_WAY.into()));
        }
        let made = self.eject_copy_claimed(&drive, name, user).await;
        self.mirroring.release(mirror);
        made
    }

    async fn eject_copy_claimed(
        self: &Arc<Self>,
        drive: &DriveRow,
        name: &str,
        user: Uuid,
    ) -> Result<Uuid, FileError> {
        let mirror = drive.id;
        let id = self
            .create_shared_drive_in(name, None, drive.backend_id, user)
            .await?;
        let made = async {
            self.root_node(id).await?;
            self.grant(
                id,
                None,
                super::grants::Grantee::User(user),
                Role::Manager,
                user,
            )
            .await?;
            self.copy_tree(mirror, id, user, None).await
        }
        .await;
        match made {
            Ok(_) => {}
            Err(e) => {
                if let Err(e) = self.delete_drive(id).await {
                    tracing::warn!("cannot take back the drive {id}: {e}");
                }
                return Err(e);
            }
        }
        // The ids of the drive mirrored, not of the mirror.
        sqlx::query(
            "UPDATE nodes SET source_id = (SELECT m.source_id FROM nodes m WHERE m.id = nodes.source_id)
             WHERE drive_id = ?1 AND source_id IS NOT NULL",
        )
        .bind(id)
        .execute(self.db.writer())
        .await?;
        // The keys the mirror kept of its encrypted folders, the user's own,
        // for their copies in the new drive.
        sqlx::query(
            "INSERT INTO e2e_copy_keys (folder_id, drive_id, source_id, generation, user_id, wrapped)
             SELECT n.id, n.drive_id, k.source_id, k.generation, k.user_id, k.wrapped
             FROM nodes n JOIN e2e_copy_keys k
               ON k.source_id = n.source_id AND k.drive_id = ?2 AND k.user_id = ?3
             WHERE n.drive_id = ?1 AND n.e2e_root = n.id
             ON CONFLICT DO NOTHING",
        )
        .bind(id)
        .bind(mirror)
        .bind(user)
        .execute(self.db.writer())
        .await?;
        Ok(id)
    }

    /// Where in the drive mirrored the copy of a mirror's node (or the
    /// folder at a path of it) is: its folder there now, when the user
    /// still sees it; else the folder at the same path; else the root.
    async fn place_in_source(
        self: &Arc<Self>,
        source: Uuid,
        mirror_folder: Option<Uuid>,
        path: &str,
    ) -> Result<Uuid, FileError> {
        if let Some(folder) = mirror_folder {
            let copied: Option<Option<Uuid>> =
                sqlx::query_scalar("SELECT source_id FROM nodes WHERE id = ?1")
                    .bind(folder)
                    .fetch_optional(self.db.reader())
                    .await?;
            if let Some(Some(original)) = copied
                && let Ok(node) = self.folder(source, original).await
            {
                return Ok(node.id);
            }
        }
        match self.resolve(source, Path::new(path)).await {
            Ok(node) if node.kind == Kind::Folder => Ok(node.id),
            _ => Ok(self.root_node(source).await?.id),
        }
    }

    /// Copy an item of a mirror's trash back into the drive mirrored, where
    /// it was (`check`: whether the user may add there, refused otherwise).
    /// Answers the new node there.
    pub async fn restore_into_source(
        self: &Arc<Self>,
        mirror: &MirrorRow,
        item: Uuid,
        user: Uuid,
    ) -> Result<Node, FileError> {
        let source = mirror.source_id.ok_or(FileError::NotFound)?;
        let (name, kind, original_parent, original_path): (String, Kind, Option<Uuid>, String) =
            sqlx::query_as(
                "SELECT name, kind, original_parent_id, original_path FROM trash
                 WHERE id = ?1 AND drive_id = ?2",
            )
            .bind(item)
            .bind(mirror.id)
            .fetch_optional(self.db.reader())
            .await?
            .ok_or(FileError::NotFound)?;
        let folder_path = original_path
            .rsplit_once('/')
            .map(|(parent, _)| parent)
            .unwrap_or("");
        let parent = self
            .place_in_source(source, original_parent, folder_path)
            .await?;
        self.access_for(user, parent, super::shares::Need::Create)
            .await?;
        let storage = self.storage(mirror.id).await?;
        let from = Tree::trash_path(item);
        let node = match kind {
            Kind::File => {
                let temp = self.temp_file()?;
                if let Err(e) = storage.copy_out(&from, &temp).await {
                    let _ = std::fs::remove_file(&temp);
                    return Err(e.into());
                }
                self.write_file(source, WriteTarget::New { parent, name }, &temp)
                    .await?
            }
            Kind::Folder => {
                let temp = self.temp_file()?;
                let size = download_folder(&storage, &from, &temp).await;
                let size = match size {
                    Ok(size) => size,
                    Err(e) => {
                        let _ = std::fs::remove_dir_all(&temp);
                        return Err(e.into());
                    }
                };
                let placed = match self.check_room(source, size).await {
                    Ok(()) => self.import_dir(source, parent, &name, &temp).await,
                    Err(e) => Err(e),
                };
                let _ = std::fs::remove_dir_all(&temp);
                placed?
            }
        };
        self.mark_created(node.id, user).await?;
        self.node(source, node.id).await
    }

    /// Copy a version of a mirror's file back into the drive mirrored: over
    /// the file it copies (whose content becomes a version there), or, when
    /// that is gone, as a new file where it was. Answers the node there.
    pub async fn restore_version_into_source(
        self: &Arc<Self>,
        mirror: &MirrorRow,
        version: Uuid,
        user: Uuid,
        by: Option<super::versions::Source>,
    ) -> Result<Node, FileError> {
        let source = mirror.source_id.ok_or(FileError::NotFound)?;
        let (node_id, trashed): (Uuid, Option<Uuid>) = sqlx::query_as(
            "SELECT node_id, trash_id FROM versions WHERE id = ?1 AND drive_id = ?2",
        )
        .bind(version)
        .bind(mirror.id)
        .fetch_optional(self.db.reader())
        .await?
        .ok_or(FileError::NotFound)?;
        if trashed.is_some() {
            return Err(FileError::Conflict(
                "Its file is in the mirror's trash: restore that instead".into(),
            ));
        }
        let copy = self.node(mirror.id, node_id).await?;
        let storage = self.storage(mirror.id).await?;
        let original = match copy.source_id {
            Some(original) => self.node(source, original).await.ok(),
            None => None,
        };
        let target = match original {
            Some(file) if file.kind == Kind::File => {
                let access = self
                    .access_for(user, file.id, super::shares::Need::Change)
                    .await?;
                WriteTarget::Replace {
                    id: file.id,
                    if_version: access.node.version,
                    source: by,
                }
            }
            _ => {
                let path = self.rel_path(copy.id).await?;
                let folder_path = path.rsplit_once('/').map(|(p, _)| p).unwrap_or("");
                let parent = self
                    .place_in_source(source, copy.parent_id, folder_path)
                    .await?;
                self.access_for(user, parent, super::shares::Need::Create)
                    .await?;
                WriteTarget::New {
                    parent,
                    name: copy.name.clone(),
                }
            }
        };
        let new = matches!(target, WriteTarget::New { .. });
        let temp = self.temp_file()?;
        if let Err(e) = storage.copy_out(&Tree::version_path(version), &temp).await {
            let _ = std::fs::remove_file(&temp);
            return Err(e.into());
        }
        let node = self.write_file(source, target, &temp).await?;
        if new {
            self.mark_created(node.id, user).await?;
        }
        Ok(node)
    }
}

/// Download a folder of a drive's storage (a trashed one) to this machine,
/// whole; answers its files' bytes.
async fn download_folder(storage: &Storage, rel: &str, to: &Path) -> std::io::Result<u64> {
    let mut size = 0;
    let mut folders = vec![(rel.to_string(), to.to_path_buf())];
    while let Some((at, local)) = folders.pop() {
        std::fs::create_dir_all(&local)?;
        for name in storage.names_in(&at).await? {
            let path = join(&at, &name);
            match storage.kind_of(&path).await? {
                Some(Kind::Folder) => folders.push((path, local.join(&name))),
                Some(Kind::File) => {
                    storage.copy_out(&path, &local.join(&name)).await?;
                    size += std::fs::metadata(local.join(&name))?.len();
                }
                None => {}
            }
        }
    }
    Ok(size)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn copied(id: Uuid, parent: Option<Uuid>, name: &str, source: Option<Uuid>) -> Copied {
        Copied {
            id,
            parent_id: parent,
            name: name.into(),
            kind: Kind::Folder,
            size: 0,
            hash: None,
            version: 1,
            source_id: source,
        }
    }

    /// The copy's index finds a child by its exact name, follows a move,
    /// and forgets a trashed folder with what was under it.
    #[test]
    fn the_index_follows_moves_and_trashes() {
        let ids: Vec<Uuid> = (0..5).map(|_| Uuid::now_v7()).collect();
        let (root, a, b, inner, source) = (ids[0], ids[1], ids[2], ids[3], ids[4]);
        let mut index = Index::new(vec![
            copied(root, None, "", None),
            copied(a, Some(root), "A", None),
            copied(b, Some(root), "a", Some(source)),
            copied(inner, Some(a), "x", None),
        ]);
        // Names that differ only in case are two names.
        assert_eq!(index.child_named(root, "A", None).map(|n| n.id), Some(a));
        assert_eq!(index.child_named(root, "a", None).map(|n| n.id), Some(b));
        assert_eq!(index.child_named(root, "A", Some(a)).map(|n| n.id), None);
        assert_eq!(index.by_source.get(&source), Some(&b));
        let mut under = index.subtree(a);
        under.sort();
        let mut want = vec![a, inner];
        want.sort();
        assert_eq!(under, want);
        // b moves into A.
        index.insert(copied(b, Some(a), "b", Some(source)));
        assert!(index.child_named(root, "a", None).is_none());
        assert_eq!(index.child_named(a, "b", None).map(|n| n.id), Some(b));
        // A goes, with b and x.
        index.forget(a);
        assert!(index.child_named(root, "A", None).is_none());
        assert!(!index.nodes.contains_key(&inner));
        assert!(index.by_source.is_empty());
        assert_eq!(index.subtree(root), [root]);
    }

    /// A copy stops on what fails for every item (the storage, the room);
    /// an item that changed or went meanwhile is left for the next copy.
    #[test]
    fn what_stops_a_copy() {
        assert!(stops_the_copy(&FileError::Down("down".into())));
        assert!(stops_the_copy(&FileError::Full));
        assert!(!stops_the_copy(&FileError::NotFound));
        assert!(!stops_the_copy(&FileError::Conflict("changed".into())));
        assert_eq!(
            failure(&FileError::Down("The storage cannot be reached".into())),
            "The storage cannot be reached"
        );
        assert_eq!(Schedule::Manual.every(), None);
        assert_eq!(Schedule::Continuous.every(), Some(Duration::ZERO));
        assert!(Schedule::Daily.every() > Schedule::Hourly.every());
    }
}
