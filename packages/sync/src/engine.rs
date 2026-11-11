//! The engine (docs/M5_DESIGN.md §3).
//!
//! It is level-triggered: a pass compares three views of each synced
//! folder — the server's tree (the `remote` mirror, kept by the change
//! feed), the local disk (a scan), and what both agreed on last time (the
//! `entries`) — and applies what differs. Nothing is queued as an event:
//! a change made while the server was away, or while paused, is simply
//! still different at the next pass. The watcher, the live events, the
//! poll and the hourly full pass only say when to look.
//!
//! A pass, per folder:
//!
//! 1. **Moves**: a node whose place changed on one side is moved on the
//!    other (by id on the server; by inode, else hash, locally), parents
//!    first, so a moved folder is moved, not sent again.
//! 2. **Deletes**: gone on one side and unchanged on the other → to the
//!    other side's trash. Changed on the other side → kept, and sent again.
//! 3. **Creates and changes**, the transfers two at a time: only one side
//!    changed → applied; both → a **conflict copy** of the local one beside
//!    the server's (`Report (conflict, MacBook, 2026-11-07).docx`), which
//!    the next pass uploads. Uploads are at the version last seen
//!    (`if_version`), so a file changed on the server meanwhile becomes a
//!    conflict, never an overwrite.
//!
//! Passes repeat until one finds nothing to do (a conflict copy, a folder
//! made on one side, are applied by the next).
//!
//! **Roles** (docs/M7B_DESIGN.md §7): each node of the mirror has the
//! user's `my_role`, as the listings say it. What the role does not allow
//! is not sent, and is reported (`Status::not_synced`), the local files
//! kept as they are: something new or changed where the user may only view,
//! a change, rename or move where they may only upload (or view). A delete
//! the user may not make brings the server's copy back. What disappears
//! from the feed without being in the drive's trash is no longer seen
//! (limited access, a grant removed): left here as it is, never deleted,
//! until it is seen again.
//!
//! **Storage** (docs/M7C_DESIGN.md §2, §4): a drive whose storage backend
//! fails or is locked (`available` false, or a 503 `storage_unavailable`
//! during the pass), or which is being moved to other storage (`moving_to`,
//! or a 409 read-only), is not synced: its folders wait
//! (`Status::waiting`), nothing is deleted on either side, and what changed
//! here is kept and listed as not synced with the reason, then sent at the
//! first pass once the storage is back or the move done.

use crate::Error;
use crate::e2e::{E2e, PrivateKey};
use crate::ignore::Ignore;
use crate::local::{self, Kind, Stat, Trash, child, is_within, join, name_of, parent_of, rebase};
use crate::names::{self, Naming, Taken};
use crate::on_demand::{self, PinAction, Placeholder, Placeholders};
use crate::remote::{self, PLAIN_MAX, Remote, Role, Target};
use crate::state::{Entry, Folder, Mode, State, Upload};
use crate::status::{
    self, Activity, ActivityKind, Conflict, Event, Held, NotSynced, Phase, Status, Waiting,
};
use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, RwLock, Weak};
use std::time::Duration;
use tokio::sync::{Notify, broadcast, watch};
use tokio::task::JoinSet;
use uuid::Uuid;

/// Why an encrypted folder is not on demand (`Engine::may_be_on_demand`).
pub const ENCRYPTED_ON_DEMAND: &str =
    "An encrypted folder cannot be on demand: add it to sync in full";

/// How the engine is set up.
#[derive(Clone, Debug)]
pub struct Config {
    /// The server's address, e.g. `https://files.example.com`.
    pub server: String,
    /// The device's token.
    pub token: String,
    /// The state database; None keeps it in memory (the tests).
    pub state: Option<PathBuf>,
    /// This computer's name, in conflict copies.
    pub device_name: String,
    /// The drop box folder: what is put there goes to the drop box.
    pub drop_box: Option<PathBuf>,
    /// The names never synced (`ignore::DEFAULT` to start with).
    pub ignore: Vec<String>,
    /// Where local deletes go.
    pub trash: Trash,
    /// The local names' rules; None: this system's, and whether each
    /// folder's disk is case-insensitive.
    pub naming: Option<Naming>,
    /// The fallback poll of the change feed, when the live events are away.
    pub poll: Duration,
    /// A full pass at least this often, so that a missed event is found.
    pub full_pass: Duration,
    /// Quiet this long after a change before a pass starts.
    pub debounce: Duration,
    /// A file modified this recently is still being written: it waits.
    pub settle: Duration,
    /// At most this many transfers at once.
    pub transfers: usize,
    /// The placeholders of the on-demand folders. None: this system's
    /// (Windows' Cloud Files), or, where there are none here (macOS, whose
    /// File Provider extension keeps them), the engine leaves such folders
    /// alone. The tests give their own.
    pub placeholders: Option<on_demand::Provider>,
}

impl Config {
    pub fn new(server: &str, token: &str, device_name: &str) -> Self {
        Config {
            server: server.trim_end_matches('/').to_string(),
            token: token.to_string(),
            state: None,
            device_name: device_name.to_string(),
            drop_box: None,
            ignore: crate::ignore::DEFAULT
                .iter()
                .map(|s| s.to_string())
                .collect(),
            trash: Trash::System,
            naming: None,
            poll: Duration::from_secs(300),
            full_pass: Duration::from_secs(3600),
            debounce: Duration::from_secs(2),
            settle: Duration::from_secs(2),
            transfers: 2,
            placeholders: None,
        }
    }
}

/// The engine of one account. Cloning it gives another handle on it.
#[derive(Clone)]
pub struct Engine {
    inner: Arc<Inner>,
}

struct Settings {
    ignore: Ignore,
    drop_box: Option<PathBuf>,
}

struct Inner {
    config: Config,
    remote: Remote,
    state: State,
    status: watch::Sender<Status>,
    events: broadcast::Sender<Event>,
    /// One pass at a time; the folder changes wait for it too.
    pass: tokio::sync::Mutex<()>,
    wake: Notify,
    paused: AtomicBool,
    /// A file was waited for: another pass soon.
    again: AtomicBool,
    settings: RwLock<Settings>,
    naming: Mutex<HashMap<i64, Naming>>,
    stop: watch::Sender<bool>,
    watcher: Mutex<Option<notify::RecommendedWatcher>>,
    /// The placeholders of the on-demand folders the engine keeps itself.
    placeholders: Option<Arc<dyn Placeholders>>,
    /// The end-to-end-encrypted folders (`e2e.rs`).
    e2e: E2e,
    /// What the pass under way does not send, for the status at its end.
    not_synced: Mutex<Vec<NotSynced>>,
    /// The nodes gone from the feed, found deleted (false) or no longer
    /// seen (true), so that the server is asked once.
    vanished: Mutex<HashMap<Uuid, bool>>,
}

/// A node of the server's tree under a synced folder, with its local path.
#[derive(Clone, Debug)]
struct RNode {
    id: Uuid,
    parent: Uuid,
    name: String,
    kind: Kind,
    version: i64,
    path: String,
    /// A file's size and time, when known (for a placeholder).
    size: Option<i64>,
    modified: Option<String>,
    /// The encrypted folder it is in, if any (its name is decrypted).
    e2e: Option<Uuid>,
    /// What the user may do with it; None when not told (all of it).
    role: Option<Role>,
}

/// The entries of a folder, by node and by local path.
#[derive(Default)]
struct Base {
    by_id: HashMap<String, Entry>,
    by_path: HashMap<String, String>,
}

impl Base {
    fn new(entries: Vec<Entry>) -> Self {
        let mut base = Base::default();
        for entry in entries {
            base.put(entry);
        }
        base
    }

    fn put(&mut self, entry: Entry) {
        if let Some(old) = self.by_id.get(&entry.node_id)
            && self.by_path.get(&old.path) == Some(&entry.node_id)
        {
            self.by_path.remove(&old.path);
        }
        self.by_path
            .insert(entry.path.clone(), entry.node_id.clone());
        self.by_id.insert(entry.node_id.clone(), entry);
    }

    fn remove(&mut self, id: &str) {
        if let Some(old) = self.by_id.remove(id)
            && self.by_path.get(&old.path).map(String::as_str) == Some(id)
        {
            self.by_path.remove(&old.path);
        }
    }

    fn at(&self, path: &str) -> Option<&Entry> {
        self.by_path.get(path).and_then(|id| self.by_id.get(id))
    }

    /// The entries under `from` (not its own) move under `to`.
    fn rebase(&mut self, from: &str, to: &str) {
        let moved: Vec<Entry> = self
            .by_id
            .values()
            .filter(|e| e.path != from && is_within(&e.path, from))
            .cloned()
            .collect();
        for mut entry in moved {
            self.by_path.remove(&entry.path);
            entry.path = rebase(&entry.path, from, to);
            self.put(entry);
        }
    }

    /// The entries under `path` (its own too), deepest first.
    fn under(&self, path: &str) -> Vec<Entry> {
        let mut out: Vec<Entry> = self
            .by_id
            .values()
            .filter(|e| is_within(&e.path, path))
            .cloned()
            .collect();
        out.sort_by_key(|e| std::cmp::Reverse(depth(&e.path)));
        out
    }
}

fn depth(path: &str) -> usize {
    path.matches('/').count()
}

/// A transfer, run two at a time.
#[derive(Debug)]
enum Job {
    /// The server's file to the local place (`before`: what was there).
    Download { node: RNode, before: Option<Stat> },
    /// Changed on both sides, or there on both sides without being synced
    /// before: the same content is adopted, else a conflict copy is made.
    Meet { node: RNode, local: Stat },
    /// A local change to the server, at the version last seen.
    Replace { entry: Entry, local: Stat },
    /// A new local file to the server.
    Upload {
        parent: Uuid,
        path: String,
        local: Stat,
    },
}

impl Engine {
    /// The engine of an account, its state opened (made if new). Nothing
    /// runs until [`Engine::start`] (or [`Engine::sync`]).
    pub async fn open(config: Config) -> Result<Engine, Error> {
        let state = match &config.state {
            Some(path) => State::open(path).await?,
            None => State::memory().await?,
        };
        let remote = Remote::new(&config.server, &config.token)?;
        let placeholders = match &config.placeholders {
            Some(provider) => Some(provider.0.clone()),
            None => on_demand::system(&remote),
        };
        let folders = state.folders().await?.len();
        let (status, _) = watch::channel(Status {
            folders,
            ..Status::default()
        });
        let (events, _) = broadcast::channel(64);
        let (stop, _) = watch::channel(false);
        // Uploads to encrypted folders are encrypted here first: beside the
        // state, so that one under way carries on after a restart.
        let scratch = match &config.state {
            Some(path) => parent_dir(path).join("e2e-uploads"),
            None => std::env::temp_dir().join(format!("organizer-e2e-{}", Uuid::new_v4().simple())),
        };
        let e2e = E2e::load(&state, scratch).await?;
        let settings = Settings {
            ignore: Ignore::new(config.ignore.clone()),
            drop_box: config.drop_box.clone(),
        };
        Ok(Engine {
            inner: Arc::new(Inner {
                config,
                remote,
                state,
                status,
                events,
                pass: tokio::sync::Mutex::new(()),
                wake: Notify::new(),
                paused: AtomicBool::new(false),
                again: AtomicBool::new(false),
                settings: RwLock::new(settings),
                naming: Mutex::new(HashMap::new()),
                stop,
                watcher: Mutex::new(None),
                placeholders,
                e2e,
                not_synced: Mutex::new(Vec::new()),
                vanished: Mutex::new(HashMap::new()),
            }),
        })
    }

    /// Run in the background: the passes, the watcher, the live events and
    /// the poll, until [`Engine::stop`].
    pub fn start(&self) {
        let _ = self.inner.stop.send(false);
        if let Some(placeholders) = self.inner.placeholders.clone() {
            let state = self.inner.state.clone();
            tokio::spawn(async move {
                for folder in state.folders().await.unwrap_or_default() {
                    if folder.mode() == Mode::OnDemand
                        && let Err(e) = connect(placeholders.clone(), &folder).await
                    {
                        tracing::warn!("{} is not on demand: {e}", folder.local().display());
                    }
                }
            });
        }
        self.inner.watch_folders();
        tokio::spawn(Inner::run(self.inner.clone()));
        tokio::spawn(Inner::listen(Arc::downgrade(&self.inner)));
        self.inner.wake.notify_one();
    }

    /// Stop the background work (a pass under way finishes its transfer).
    pub fn stop(&self) {
        let _ = self.inner.stop.send(true);
        self.inner.watcher.lock().unwrap().take();
    }

    /// Passes until one finds nothing to do: everything in sync, now.
    pub async fn sync(&self) -> Result<(), Error> {
        self.inner.sync().await
    }

    /// Ask for a pass soon (the background loop).
    pub fn wake(&self) {
        self.inner.wake.notify_one();
    }

    pub fn status(&self) -> watch::Receiver<Status> {
        self.inner.status.subscribe()
    }

    pub fn events(&self) -> broadcast::Receiver<Event> {
        self.inner.events.subscribe()
    }

    pub fn pause(&self) {
        self.inner.paused.store(true, Ordering::SeqCst);
        self.inner.update(|s| s.phase = Phase::Paused);
    }

    pub fn resume(&self) {
        self.inner.paused.store(false, Ordering::SeqCst);
        self.inner.update(|s| s.phase = Phase::UpToDate);
        self.inner.wake.notify_one();
    }

    pub fn is_paused(&self) -> bool {
        self.inner.paused.load(Ordering::SeqCst)
    }

    pub fn remote(&self) -> &Remote {
        &self.inner.remote
    }

    /// Unlock the encrypted folders with the encryption passphrase: the
    /// private key, for the app to keep in the system's credential store.
    pub async fn unlock(&self, passphrase: &str) -> Result<PrivateKey, Error> {
        let key = crate::e2e::unlock(&self.inner.remote, passphrase).await?;
        self.set_private_key(Some(key.clone()));
        Ok(key)
    }

    /// The private key kept by the app (unlocked), or none (locked: the
    /// encrypted folders are left alone, on both sides).
    pub fn set_private_key(&self, key: Option<PrivateKey>) {
        let unlocked = key.is_some();
        self.inner.e2e.set_private(key);
        self.inner.update(|s| {
            s.encryption.unlocked = unlocked;
            s.encryption.set_up |= unlocked;
        });
        self.inner.wake.notify_one();
    }

    pub fn is_unlocked(&self) -> bool {
        self.inner.e2e.unlocked()
    }

    pub async fn folders(&self) -> Result<Vec<Folder>, Error> {
        self.inner.state.folders().await
    }

    /// What the user may do with a synced folder's top, as last listed;
    /// None before it was (or while it is unavailable).
    pub async fn role(&self, folder: &Folder) -> Result<Option<remote::Role>, Error> {
        Ok(self
            .inner
            .state
            .remote(folder.remote_id())
            .await?
            .and_then(|row| row.role()))
    }

    /// Sync a folder of the server (`remote_id`) with a local place: made
    /// if missing; not inside another synced folder or the drop box folder,
    /// nor around one. On demand, the engine makes it a sync root of the
    /// system's placeholders (Windows); where it has none (macOS), the
    /// place is the File Provider domain's, which the app added, and the
    /// engine only lists the folder.
    pub async fn add_folder(
        &self,
        remote_id: Uuid,
        local: &Path,
        mode: Mode,
    ) -> Result<Folder, Error> {
        let _pass = self.inner.pass.lock().await;
        if !local.is_absolute() {
            return Err(Error::Invalid("The local place must be a full path".into()));
        }
        let overlaps = |a: &Path, b: &Path| a.starts_with(b) || b.starts_with(a);
        for folder in self.inner.state.folders().await? {
            if overlaps(local, &folder.local()) {
                return Err(Error::Invalid(format!(
                    "{} is synced already, or holds a synced folder",
                    local.display()
                )));
            }
        }
        if let Some(drop_box) = self.inner.settings.read().unwrap().drop_box.as_deref()
            && overlaps(local, drop_box)
        {
            return Err(Error::Invalid(
                "The drop box folder cannot be synced, nor hold a synced folder".into(),
            ));
        }
        if mode == Mode::OnDemand {
            self.may_be_on_demand(remote_id).await?;
        }
        let placeholders = match mode {
            Mode::Sync => None,
            Mode::OnDemand => self.inner.placeholders.clone(),
        };
        if mode == Mode::Sync || placeholders.is_some() {
            std::fs::create_dir_all(local)?;
        }
        // The drive it is in, for the settings' list of drives; asked again
        // at a pass when the server cannot say now.
        let drive_id = match self.inner.remote.node(remote_id).await {
            Ok(node) => node.drive_id,
            Err(_) => None,
        };
        let folder = self
            .inner
            .state
            .add_folder(remote_id, local, mode, drive_id.as_deref())
            .await?;
        if let Some(placeholders) = placeholders
            && let Err(e) = connect(placeholders, &folder).await
        {
            self.inner.state.remove_folder(folder.id).await?;
            return Err(Error::Invalid(format!(
                "On demand is not available here: {e}"
            )));
        }
        let count = self.inner.state.folders().await?.len();
        self.inner.update(|s| s.folders = count);
        self.inner.watch_folders();
        self.inner.wake.notify_one();
        Ok(folder)
    }

    /// Whether a folder of the server may be kept on demand: not an
    /// encrypted folder, nor one inside one. A placeholder's bytes come
    /// straight from the server when the system asks (Windows' fetch, macOS'
    /// extension), sealed, so such folders sync in full; the encrypted
    /// folders inside an on-demand one are left out of it.
    pub async fn may_be_on_demand(&self, remote_id: Uuid) -> Result<(), Error> {
        if self.inner.remote.node(remote_id).await?.e2e_root.is_some() {
            return Err(Error::Invalid(ENCRYPTED_ON_DEMAND.into()));
        }
        Ok(())
    }

    /// Stop syncing a folder. Its local files stay where they are.
    pub async fn remove_folder(&self, id: i64) -> Result<(), Error> {
        let _pass = self.inner.pass.lock().await;
        let folder = self.folder(id).await?;
        if let Some(placeholders) = self.inner.placeholders_of(&folder) {
            let (root, account) = (folder.local(), folder.remote_id.clone());
            tokio::task::spawn_blocking(move || placeholders.release(&root, &account))
                .await
                .map_err(|e| Error::Invalid(e.to_string()))??;
        }
        self.inner.state.remove_folder(id).await?;
        self.inner.state.remove_remote(folder.remote_id()).await?;
        let count = self.inner.state.folders().await?.len();
        self.inner.update(|s| {
            s.folders = count;
            s.unavailable.retain(|f| *f != id);
        });
        self.inner.watch_folders();
        Ok(())
    }

    /// Move a synced folder's local place: its files are moved there.
    pub async fn move_folder(&self, id: i64, to: &Path) -> Result<(), Error> {
        let _pass = self.inner.pass.lock().await;
        let folder = self.folder(id).await?;
        if folder.mode() == Mode::OnDemand {
            return Err(Error::Invalid(
                "An on-demand folder stays where it is: remove it and add it again elsewhere"
                    .into(),
            ));
        }
        if !to.is_absolute() {
            return Err(Error::Invalid("The local place must be a full path".into()));
        }
        if to.exists() {
            return Err(Error::Invalid(format!("{} exists already", to.display())));
        }
        if let Some(parent) = to.parent() {
            std::fs::create_dir_all(parent)?;
        }
        if folder.local().exists() {
            std::fs::rename(folder.local(), to)
                .map_err(|e| Error::Invalid(format!("The folder could not be moved there: {e}")))?;
        }
        self.inner.state.move_folder(id, to).await?;
        self.inner.naming.lock().unwrap().remove(&id);
        self.inner.watch_folders();
        self.inner.wake.notify_one();
        Ok(())
    }

    async fn folder(&self, id: i64) -> Result<Folder, Error> {
        self.inner
            .state
            .folders()
            .await?
            .into_iter()
            .find(|f| f.id == id)
            .ok_or(Error::NotFound)
    }

    pub fn ignore(&self) -> Vec<String> {
        self.inner
            .settings
            .read()
            .unwrap()
            .ignore
            .patterns()
            .to_vec()
    }

    pub fn set_ignore(&self, patterns: Vec<String>) {
        self.inner.settings.write().unwrap().ignore = Ignore::new(patterns);
        self.inner.wake.notify_one();
    }

    pub fn drop_box(&self) -> Option<PathBuf> {
        self.inner.settings.read().unwrap().drop_box.clone()
    }

    /// Turn the drop box folder on (at this place) or off.
    pub fn set_drop_box(&self, dir: Option<PathBuf>) -> Result<(), Error> {
        if let Some(dir) = &dir {
            std::fs::create_dir_all(dir)?;
        }
        self.inner.settings.write().unwrap().drop_box = dir;
        self.inner.watch_folders();
        self.inner.wake.notify_one();
        Ok(())
    }

    /// The URL of a resumable upload under way for a file of a folder (0:
    /// the drop box folder), for the tests.
    #[doc(hidden)]
    pub async fn upload_in_progress(&self, folder: i64, path: &str) -> Option<String> {
        self.inner
            .state
            .upload(folder, path)
            .await
            .ok()
            .flatten()
            .map(|u| u.url)
    }

    /// For the tests: the resumable uploads' bodies break off after this
    /// many bytes.
    #[doc(hidden)]
    pub fn break_uploads_after(&self, bytes: Option<u64>) {
        self.inner.remote.break_uploads_after(bytes);
    }

    /// Close the state database: the engine is done with.
    pub async fn close(&self) {
        self.stop();
        let _pass = self.inner.pass.lock().await;
        self.inner.state.close().await;
    }
}

impl Inner {
    fn update(&self, f: impl FnOnce(&mut Status)) {
        self.status.send_modify(f);
    }

    /// Whether the engine keeps this folder: a full-sync one, or an
    /// on-demand one when it has the system's placeholders.
    fn keeps(&self, folder: &Folder) -> bool {
        folder.mode() == Mode::Sync || self.placeholders.is_some()
    }

    /// The placeholders of an on-demand folder the engine keeps.
    fn placeholders_of(&self, folder: &Folder) -> Option<Arc<dyn Placeholders>> {
        match folder.mode() {
            Mode::Sync => None,
            Mode::OnDemand => self.placeholders.clone(),
        }
    }

    /// One job done that is not worth telling (a placeholder made).
    fn done_quietly(&self) {
        self.update(|s| {
            s.done += 1;
            s.pending = s.pending.saturating_sub(1);
        });
    }

    fn activity(&self, kind: ActivityKind, folder: &Folder, path: &str) {
        let name = folder
            .local()
            .file_name()
            .map(|n| n.to_string_lossy().into_owned())
            .unwrap_or_default();
        let path = child(&name, path);
        self.update(|s| {
            s.recent.insert(
                0,
                Activity {
                    kind,
                    path,
                    at: status::now(),
                },
            );
            s.recent.truncate(status::RECENT);
            s.done += 1;
            s.pending = s.pending.saturating_sub(1);
        });
    }

    /// A local place not synced, for the status at the end of the pass:
    /// the shallowest only (what is in a folder held is held with it).
    fn hold(&self, folder: &Folder, path: &str, why: Held) {
        self.hold_because(folder, path, why, None);
    }

    /// [`Inner::hold`], with the server's words for why.
    fn hold_because(&self, folder: &Folder, path: &str, why: Held, reason: Option<&str>) {
        let name = folder
            .local()
            .file_name()
            .map(|n| n.to_string_lossy().into_owned())
            .unwrap_or_default();
        let path = child(&name, path);
        let mut held = self.not_synced.lock().unwrap();
        if held.len() < status::NOT_SYNCED && !held.iter().any(|h| is_within(&path, &h.path)) {
            held.push(NotSynced {
                path,
                why,
                reason: reason.map(String::from),
            });
        }
    }

    fn ignore(&self) -> Ignore {
        self.settings.read().unwrap().ignore.clone()
    }

    fn naming(&self, folder: &Folder) -> Naming {
        if let Some(naming) = self.config.naming {
            return naming;
        }
        *self
            .naming
            .lock()
            .unwrap()
            .entry(folder.id)
            .or_insert_with(|| Naming::for_disk(names::case_insensitive(&folder.local())))
    }

    /// A failure of one item: told, and the pass goes on; a server away or
    /// a token refused stops it.
    fn tolerate<T>(&self, result: Result<T, Error>) -> Result<Option<T>, Error> {
        match result {
            Ok(value) => Ok(Some(value)),
            Err(
                e @ (Error::Offline(_)
                | Error::Unauthorized
                | Error::Db(_)
                | Error::StorageUnavailable(_)
                | Error::ReadOnly(_)),
            ) => Err(e),
            Err(e) => {
                tracing::warn!("sync: {e}");
                let message = e.to_string();
                self.update(|s| s.error = Some(message.clone()));
                let _ = self.events.send(Event::Error { message });
                Ok(None)
            }
        }
    }

    // --- The background loop ---------------------------------------------

    async fn run(self: Arc<Self>) {
        let mut stop = self.stop.subscribe();
        let mut poll = tokio::time::interval(self.config.poll);
        let mut full = tokio::time::interval(self.config.full_pass);
        let mut retry = Duration::from_secs(15);
        loop {
            tokio::select! {
                _ = self.wake.notified() => {}
                _ = poll.tick() => {}
                _ = full.tick() => {}
                _ = stop.changed() => return,
            }
            // Quiet for a moment first: a burst of changes is one pass.
            loop {
                tokio::select! {
                    _ = self.wake.notified() => continue,
                    _ = tokio::time::sleep(self.config.debounce) => break,
                    _ = stop.changed() => return,
                }
            }
            if *stop.borrow() {
                return;
            }
            if self.paused.load(Ordering::SeqCst) {
                self.update(|s| s.phase = Phase::Paused);
                continue;
            }
            match self.sync().await {
                Ok(()) => {
                    retry = Duration::from_secs(15);
                    if self.again.swap(false, Ordering::SeqCst) {
                        self.wake_after(self.config.settle.max(Duration::from_secs(1)));
                    }
                }
                Err(Error::Unauthorized) => {
                    self.update(|s| {
                        s.phase = Phase::Error;
                        s.error = Some(Error::Unauthorized.to_string());
                    });
                    let _ = self.events.send(Event::SignedOut);
                    return;
                }
                Err(e) => {
                    self.wake_after(retry);
                    retry = (retry * 2).min(Duration::from_secs(300));
                    tracing::info!("sync: {e}; again in {retry:?}");
                }
            }
        }
    }

    fn wake_after(self: &Arc<Self>, after: Duration) {
        let inner = Arc::downgrade(self);
        tokio::spawn(async move {
            tokio::time::sleep(after).await;
            if let Some(inner) = inner.upgrade() {
                inner.wake.notify_one();
            }
        });
    }

    /// The live events (`/api/events`): `changes` wakes a pass, `drop-box`
    /// is told to the app. Connected again after a break, with a pass then
    /// (what was missed meanwhile is in the feed).
    async fn listen(inner: Weak<Self>) {
        let mut backoff = Duration::from_secs(1);
        loop {
            let Some(this) = inner.upgrade() else { return };
            let mut stop = this.stop.subscribe();
            if *stop.borrow() {
                return;
            }
            let remote = this.remote.clone();
            drop(this);
            let connected = tokio::select! {
                r = remote.events() => r,
                _ = stop.changed() => return,
            };
            if let Ok(mut response) = connected {
                backoff = Duration::from_secs(1);
                if let Some(this) = inner.upgrade() {
                    this.wake.notify_one();
                }
                let mut buffer = String::new();
                loop {
                    let chunk = tokio::select! {
                        c = response.chunk() => c,
                        _ = stop.changed() => return,
                    };
                    let Ok(Some(chunk)) = chunk else { break };
                    buffer.push_str(&String::from_utf8_lossy(&chunk));
                    while let Some(end) = buffer.find("\n\n") {
                        let block: String = buffer.drain(..end + 2).collect();
                        let event = block
                            .lines()
                            .find_map(|l| l.strip_prefix("event:"))
                            .map(str::trim);
                        let Some(this) = inner.upgrade() else { return };
                        match event {
                            Some("changes") => {
                                this.wake.notify_one();
                                let _ = this.events.send(Event::Changes);
                            }
                            Some("drop-box") => {
                                let _ = this.events.send(Event::DropBox);
                            }
                            _ => {}
                        }
                    }
                }
            }
            tokio::select! {
                _ = tokio::time::sleep(backoff) => {}
                _ = stop.changed() => return,
            }
            backoff = (backoff * 2).min(Duration::from_secs(60));
        }
    }

    /// Watch the synced folders and the drop box folder: any event wakes a
    /// pass (after the debounce).
    fn watch_folders(self: &Arc<Self>) {
        use notify::{RecursiveMode, Watcher};
        if *self.stop.borrow() {
            return;
        }
        let inner = Arc::downgrade(self);
        let watcher = notify::recommended_watcher(move |event: notify::Result<notify::Event>| {
            if event.is_ok()
                && let Some(inner) = inner.upgrade()
            {
                inner.wake.notify_one();
            }
        });
        let mut watcher = match watcher {
            Ok(watcher) => watcher,
            Err(e) => {
                tracing::warn!("no file watcher ({e}): the hourly pass and the poll only");
                return;
            }
        };
        let state = self.state.clone();
        let drop_box = self.settings.read().unwrap().drop_box.clone();
        let this = self.clone();
        tokio::spawn(async move {
            let folders = state.folders().await.unwrap_or_default();
            let kept = folders.iter().filter(|f| this.keeps(f)).map(Folder::local);
            for path in kept.chain(drop_box) {
                if path.exists()
                    && let Err(e) = watcher.watch(&path, RecursiveMode::Recursive)
                {
                    tracing::warn!("cannot watch {}: {e}", path.display());
                }
            }
            *this.watcher.lock().unwrap() = Some(watcher);
        });
    }

    // --- A pass ----------------------------------------------------------

    async fn sync(self: &Arc<Self>) -> Result<(), Error> {
        let _pass = self.pass.lock().await;
        self.update(|s| {
            s.phase = Phase::Syncing;
            s.pending = 0;
            s.done = 0;
            s.error = None;
        });
        let mut result = self.encryption_round().await;
        for _ in 0..8 {
            if result.is_err() {
                break;
            }
            match self.pass_once().await {
                Ok(0) => break,
                Ok(_) => continue,
                Err(e) => {
                    result = Err(e);
                    break;
                }
            }
        }
        let paused = self.paused.load(Ordering::SeqCst);
        self.update(|s| {
            s.pending = 0;
            match &result {
                Ok(()) => {
                    s.phase = if paused {
                        Phase::Paused
                    } else if s.error.is_some() {
                        Phase::Error
                    } else {
                        Phase::UpToDate
                    };
                    s.synced_at = Some(status::now());
                }
                Err(e) if e.is_offline() => s.phase = Phase::Offline,
                Err(e) => {
                    s.phase = Phase::Error;
                    s.error = Some(e.to_string());
                }
            }
        });
        result
    }

    /// What the account's encryption asks of this device (`e2e.rs`), once
    /// a round; the folder keys are read again.
    async fn encryption_round(&self) -> Result<(), Error> {
        self.e2e.forget_keys();
        let Some(round) = self.tolerate(self.e2e.round(&self.remote).await)? else {
            return Ok(());
        };
        let unlocked = self.e2e.unlocked();
        self.update(|s| {
            s.encryption.set_up = round.set_up;
            s.encryption.unlocked = unlocked;
        });
        if round.forgotten {
            let _ = self.events.send(Event::Locked);
        }
        Ok(())
    }

    /// One pass: the server's changes, then each folder, then the drop box
    /// folder. The number of changes applied.
    async fn pass_once(self: &Arc<Self>) -> Result<usize, Error> {
        let all = self.state.folders().await?;
        self.note_drives(&all).await?;
        let folders: Vec<Folder> = all.into_iter().filter(|f| self.keeps(f)).collect();
        self.refresh_remote(&folders).await?;
        let mut work = 0;
        self.e2e.take_skipped();
        self.not_synced.lock().unwrap().clear();
        let mut unavailable = Vec::new();
        let mut waiting = Vec::new();
        let storage = self.drive_storage(&folders).await?;
        for folder in &folders {
            if self.paused.load(Ordering::SeqCst) {
                return Ok(0);
            }
            // Its top is gone from the server, or from the user's drives
            // (they left the drive, or it was deleted): not synced, and
            // its files here are kept. Synced again if it comes back.
            if self.state.remote(folder.remote_id()).await?.is_none() {
                unavailable.push(folder.id);
                continue;
            }
            // Its drive's storage cannot be used now, or the drive is being
            // moved (docs/M7C_DESIGN.md §2, §4): it waits, nothing deleted,
            // what changed here kept for when it is back.
            let wait = folder.drive_id.as_ref().and_then(|d| storage.get(d));
            if let Some((why, reason)) = wait {
                waiting.push(self.wait(folder, *why, reason).await?);
                continue;
            }
            match self.reconcile(folder).await {
                Ok(done) => work += done,
                // Found during the pass: the same, from then on.
                Err(Error::StorageUnavailable(reason)) => {
                    waiting.push(self.wait(folder, Held::Unavailable, &reason).await?);
                }
                Err(Error::ReadOnly(reason)) => {
                    waiting.push(self.wait(folder, Held::Moving, &reason).await?);
                }
                Err(e) => return Err(e),
            }
        }
        let skipped = self.e2e.take_skipped();
        let not_synced = std::mem::take(&mut *self.not_synced.lock().unwrap());
        self.update(|s| {
            s.encryption.skipped = skipped;
            s.unavailable = unavailable;
            s.not_synced = not_synced;
            s.waiting = waiting;
        });
        let drop_box = self.settings.read().unwrap().drop_box.clone();
        if let Some(dir) = drop_box {
            work += self.drop_box(&dir).await?;
        }
        Ok(work)
    }

    /// The drives of the synced folders whose storage cannot be used now
    /// (`unavailable`, with the server's reason) or which are being moved
    /// (`moving`, read-only until done), by drive id. One listing of the
    /// user's drives a pass; a drive not in it is the unavailable folders'
    /// case (left, or deleted).
    async fn drive_storage(
        &self,
        folders: &[Folder],
    ) -> Result<HashMap<String, (Held, String)>, Error> {
        if folders.iter().all(|f| f.drive_id.is_none()) {
            return Ok(HashMap::new());
        }
        let drives = match self.remote.drives().await {
            Ok(drives) => drives,
            Err(e @ (Error::Offline(_) | Error::Unauthorized)) => return Err(e),
            // Not known this time: the requests themselves will say.
            Err(e) => {
                tracing::warn!("sync: the drives: {e}");
                return Ok(HashMap::new());
            }
        };
        Ok(drives
            .into_iter()
            .filter_map(|drive| Some((drive.id.clone(), storage_wait(&drive)?)))
            .collect())
    }

    /// A synced folder waits for its drive's storage: what was changed here
    /// since it was last synced is listed as not synced, with the reason,
    /// and kept as it is. Nothing is asked of the server.
    async fn wait(&self, folder: &Folder, why: Held, reason: &str) -> Result<Waiting, Error> {
        let root = folder.local();
        if root.exists() {
            let top = self.state.remote(folder.remote_id()).await?;
            let ignore = self
                .ignore()
                .at_drive_root(top.is_some_and(|t| t.parent_id.is_none()));
            let local = local::scan(&root, &ignore)?;
            let mut entries = self.state.entries(folder.id).await?;
            entries.retain(|e| !ignore.ignores_path(&e.path));
            entries.sort_by(|a, b| a.path.cmp(&b.path));
            let agreed: HashMap<&str, &Entry> =
                entries.iter().map(|e| (e.path.as_str(), e)).collect();
            // Made or changed here (a file whose size or time differs from
            // what was agreed: not hashed, the list is what it looks like),
            // then deleted here.
            for (path, stat) in &local {
                let changed = match agreed.get(path.as_str()) {
                    None => true,
                    Some(entry) => {
                        entry.kind() != stat.kind
                            || (stat.kind == Kind::File && stat.differs(entry.size, entry.mtime_ns))
                    }
                };
                if changed {
                    self.hold_because(folder, path, why, Some(reason));
                }
            }
            for entry in &entries {
                if !local.contains_key(&entry.path) {
                    self.hold_because(folder, &entry.path, why, Some(reason));
                }
            }
        }
        Ok(Waiting {
            folder: folder.id,
            why,
            reason: reason.to_string(),
        })
    }

    /// The drive of each folder not told yet (one added before drives, or
    /// while the server was away), for the settings' list of drives.
    async fn note_drives(&self, folders: &[Folder]) -> Result<(), Error> {
        for folder in folders.iter().filter(|f| f.drive_id.is_none()) {
            match self.remote.node(folder.remote_id()).await {
                Ok(node) => {
                    if let Some(drive) = node.drive_id {
                        self.state.set_folder_drive(folder.id, &drive).await?;
                    }
                }
                Err(e @ (Error::Offline(_) | Error::Unauthorized)) => return Err(e),
                // Gone: the pass says it is unavailable.
                Err(_) => {}
            }
        }
        Ok(())
    }

    // --- The server's tree -----------------------------------------------

    /// The mirror brought up to date: from the feed, or (no cursor yet, or
    /// one too old) a full listing of the synced folders.
    async fn refresh_remote(&self, folders: &[Folder]) -> Result<(), Error> {
        if folders.is_empty() {
            return Ok(());
        }
        let roots: HashSet<Uuid> = folders.iter().map(Folder::remote_id).collect();
        match self.state.meta("cursor").await? {
            None => self.bootstrap(folders).await?,
            Some(cursor) => match self.follow(cursor, &roots).await {
                Err(Error::Gone) => {
                    tracing::info!("the change feed's cursor is too old: listing everything again");
                    self.state.clear_remote().await?;
                    self.bootstrap(folders).await?;
                }
                other => other?,
            },
        }
        // Each folder's top asked again. One added since is listed once; so
        // is one whose role changed, which no change of the feed tells (a
        // drive's role, a grant): the roles under it follow. One the server
        // no longer has, or the user no longer sees, is unavailable.
        for folder in folders {
            let id = folder.remote_id();
            let node = match self.remote.node(id).await {
                Ok(node) => node,
                Err(Error::NotFound) => {
                    self.state.remove_remote(id).await?;
                    continue;
                }
                // Its drive's storage is away: the folder waits, its mirror
                // kept as it is (`pass_once`).
                Err(Error::StorageUnavailable(_)) => continue,
                Err(e) => return Err(e),
            };
            let listed = self.state.remote(id).await?;
            if listed.is_none_or(|row| row.role() != node.role) {
                // Listed anew, not over the old: what the new role no longer
                // sees (others' files, for upload only) is gone from it.
                self.state.remove_remote(id).await?;
                match self.list_subtree(id, true).await {
                    Err(Error::NotFound | Error::StorageUnavailable(_)) => {}
                    other => other?,
                }
            }
        }
        Ok(())
    }

    /// The cursor of now, then everything under each synced folder (what
    /// changes meanwhile comes again from the feed, which is harmless).
    async fn bootstrap(&self, folders: &[Folder]) -> Result<(), Error> {
        let cursor = self.remote.changes(None).await?.cursor;
        for folder in folders {
            match self.list_subtree(folder.remote_id(), true).await {
                Err(Error::NotFound | Error::StorageUnavailable(_)) => {}
                other => other?,
            }
        }
        self.state.set_meta("cursor", Some(&cursor)).await
    }

    /// A folder's subtree into the mirror (the folder's own row too, when
    /// `own`).
    async fn list_subtree(&self, id: Uuid, own: bool) -> Result<(), Error> {
        if own {
            let node = self.remote.node(id).await?;
            self.state.put_remote(&node).await?;
            self.e2e
                .note(&self.state, node.id, node.e2e_root, true)
                .await?;
        }
        let mut folders = vec![id];
        while let Some(folder) = folders.pop() {
            let children = match self.remote.children(folder).await {
                // A folder inside deleted meanwhile: the feed says so next.
                Err(Error::NotFound) if folder != id => continue,
                other => other?,
            };
            for node in children {
                self.state.put_remote(&node).await?;
                self.e2e
                    .note(&self.state, node.id, node.e2e_root, false)
                    .await?;
                if node.kind == Kind::Folder {
                    folders.push(node.id);
                }
            }
        }
        Ok(())
    }

    async fn follow(&self, mut cursor: String, roots: &HashSet<Uuid>) -> Result<(), Error> {
        loop {
            let page = self.remote.changes(Some(&cursor)).await?;
            for change in page.changes {
                self.apply_change(change, roots).await?;
            }
            cursor = page.cursor;
            self.state.set_meta("cursor", Some(&cursor)).await?;
            if !page.has_more {
                return Ok(());
            }
        }
    }

    /// One change of the feed into the mirror: kept when it is (or comes)
    /// under a synced folder; a folder that comes in is listed, as what is
    /// in it was not followed.
    ///
    /// The feed covers every drive of the user's (docs/M7A_DESIGN.md §2):
    /// the changes of a drive not synced here are not under a synced
    /// folder, and are let go, the drives' roots (parent null: the user
    /// joined or left a drive) too. A synced drive's root deleted (the user
    /// left it, or it was deleted) takes its tree out of the mirror, and the
    /// folder is unavailable (`pass_once`); its root coming back (they
    /// joined again) is listed anew, as any synced folder that comes back.
    async fn apply_change(
        &self,
        change: crate::api::types::Change,
        roots: &HashSet<Uuid>,
    ) -> Result<(), Error> {
        let id = change.node_id;
        if change.op == "delete" {
            return self.state.remove_remote(id).await;
        }
        let (Some(kind), Some(name), Some(version)) = (change.kind, change.name, change.version)
        else {
            return Ok(());
        };
        let kind = remote::kind(kind);
        let before = self.state.remote(id).await?;
        let known = before.is_some();
        let parent = match change.parent_id {
            Some(parent) => self.state.remote(parent).await?,
            None => None,
        };
        let in_scope = roots.contains(&id) || parent.is_some();
        // The feed does not say the role: a node keeps its own where it
        // stays (a synced folder's top always), else takes its new
        // parent's. A role changed since is listed anew (`refresh_remote`).
        let stays = before.as_ref().is_some_and(|b| {
            roots.contains(&id) || b.parent_id == change.parent_id.map(|p| p.to_string())
        });
        let role = match (&before, &parent) {
            (Some(before), _) if stays => before.role(),
            (_, Some(parent)) => parent.role(),
            _ => None,
        };
        if in_scope {
            // The feed does not say a file's size: asked for when needed.
            self.state
                .put_remote(&remote::Node {
                    id,
                    parent_id: change.parent_id,
                    name,
                    kind,
                    version,
                    size: None,
                    modified: None,
                    e2e_root: change.e2e_root,
                    drive_id: Some(change.drive_id),
                    role,
                    source_id: None,
                })
                .await?;
            self.e2e
                .note(&self.state, id, change.e2e_root, roots.contains(&id))
                .await?;
            // A synced folder coming back too: what is in it was not
            // followed while it was away, and an empty mirror of it would
            // be taken for everything deleted.
            if kind == Kind::Folder && !known {
                match self.list_subtree(id, false).await {
                    // Gone again since: its delete is further on.
                    Err(Error::NotFound) => {}
                    other => other?,
                }
            }
        } else if known {
            self.state.remove_remote(id).await?;
        }
        Ok(())
    }

    /// The server's tree under a folder, from the mirror, with the local
    /// path of each node: the name it has locally when the server's did not
    /// change, else the server's made safe here.
    async fn remote_tree(
        &self,
        folder: &Folder,
        base: &Base,
        naming: Naming,
        ignore: &Ignore,
    ) -> Result<(HashMap<Uuid, RNode>, Vec<String>), Error> {
        let rows = self.state.remote_rows().await?;
        let mut children: HashMap<Uuid, Vec<crate::e2e::Kid>> = HashMap::new();
        // A file's size and time, for a placeholder.
        let mut sizes: HashMap<Uuid, (Option<i64>, Option<String>)> = HashMap::new();
        let mut roles: HashMap<Uuid, Option<Role>> = HashMap::new();
        for row in rows {
            let (Ok(id), Some(Ok(parent))) = (
                row.node_id.parse::<Uuid>(),
                row.parent_id.as_deref().map(str::parse::<Uuid>),
            ) else {
                continue;
            };
            let kind = row.kind.parse().unwrap_or(Kind::File);
            roles.insert(id, row.role());
            children
                .entry(parent)
                .or_default()
                .push((id, row.name, kind, row.version));
            sizes.insert(id, (row.size, row.modified));
        }
        // An on-demand folder's placeholders could only show the server's
        // sealed bytes: the encrypted folders in it are left out of it.
        let plain_only = folder.mode() == Mode::OnDemand;
        let mut out = HashMap::new();
        // The local places of what cannot be read (`e2e.rs`): left alone.
        let mut hidden = Vec::new();
        let root = folder.remote_id();
        let mut stack = vec![(root, String::new(), self.e2e.region_of_root(root))];
        while let Some((parent, parent_path, region)) = stack.pop() {
            let Some(kids) = children.remove(&parent) else {
                continue;
            };
            let (kids, mut unreadable): (Vec<_>, Vec<_>) = if plain_only {
                kids.into_iter()
                    .partition(|kid| region.is_none() && !self.e2e.is_top(kid.0))
            } else {
                (kids, Vec::new())
            };
            // Names in an encrypted folder decrypted: all below is on them.
            let (kids, locked) = self
                .e2e
                .readable(&self.remote, parent, region, kids)
                .await?;
            unreadable.extend(locked);
            let mut regions: HashMap<Uuid, Option<Uuid>> =
                kids.iter().map(|(kid, region)| (kid.0, *region)).collect();
            let mut kids: Vec<_> = kids.into_iter().map(|(kid, _)| kid).collect();
            kids.sort_by(|a, b| a.1.cmp(&b.1));
            let mut taken = Taken::default();
            let mut names: HashMap<Uuid, String> = HashMap::new();
            // The local names kept first, so a new twin does not take one.
            for (id, name, ..) in &kids {
                if let Some(entry) = base.by_id.get(&id.to_string())
                    && entry.name == *name
                    && parent_of(&entry.path) == parent_path
                {
                    let local = name_of(&entry.path).to_string();
                    taken.insert(&local, naming);
                    names.insert(*id, local);
                }
            }
            for (id, name, kind, version) in kids {
                let (size, modified) = sizes.remove(&id).unwrap_or_default();
                if ignore.ignored(&name) {
                    continue;
                }
                let local = match names.remove(&id) {
                    Some(local) => local,
                    None => taken.local_name(&name, naming),
                };
                let path = child(&parent_path, &local);
                let e2e = regions.remove(&id).flatten();
                if kind == Kind::Folder {
                    stack.push((id, path.clone(), e2e));
                }
                out.insert(
                    id,
                    RNode {
                        id,
                        parent,
                        name,
                        kind,
                        version,
                        path,
                        size,
                        modified,
                        e2e,
                        role: roles.get(&id).copied().flatten(),
                    },
                );
            }
            for (id, name, _, _) in unreadable {
                if let Some(entry) = base.by_id.get(&id.to_string()) {
                    hidden.push(entry.path.clone());
                }
                hidden.push(child(&parent_path, &taken.local_name(&name, naming)));
            }
        }
        Ok((out, hidden))
    }

    // --- A folder --------------------------------------------------------

    async fn reconcile(self: &Arc<Self>, folder: &Folder) -> Result<usize, Error> {
        let root = folder.local();
        let mut base = Base::new(self.state.entries(folder.id).await?);
        // Gone from the server: unavailable (`pass_once`), left alone.
        let Some(top) = self.state.remote(folder.remote_id()).await? else {
            return Ok(0);
        };
        // A drive's root (My drive's, a shared drive's) has no parent: its
        // `.organizer` is the server's own, never sent.
        let ignore = self.ignore().at_drive_root(top.parent_id.is_none());
        let top_role = top.role();
        // An encrypted folder is never on demand (`may_be_on_demand`).
        if folder.mode() == Mode::OnDemand && self.e2e.region_of_root(folder.remote_id()).is_some()
        {
            self.update(|s| s.error = Some(ENCRYPTED_ON_DEMAND.into()));
            return Ok(0);
        }
        // An encrypted folder that cannot be read here is left alone.
        if let Some(top) = self.e2e.region_of_root(folder.remote_id())
            && self.e2e.keys(&self.remote, top).await?.is_none()
        {
            self.e2e.skip();
            return Ok(0);
        }
        if !root.exists() {
            if !base.by_id.is_empty() {
                // Unmounted, or moved away: never taken for a mass delete.
                let message = format!("{} is missing: it is not synced", root.display());
                self.update(|s| s.error = Some(message));
                return Ok(0);
            }
            std::fs::create_dir_all(&root)?;
        }
        let naming = self.naming(folder);
        // A name ignored since it was synced is let go of on both sides,
        // never taken for a delete.
        let ignored: Vec<String> = base
            .by_id
            .values()
            .filter(|e| ignore.ignores_path(&e.path))
            .map(|e| e.node_id.clone())
            .collect();
        for id in ignored {
            self.state.remove_entry(folder.id, &id).await?;
            base.remove(&id);
        }
        let mut local = local::scan(&root, &ignore)?;
        let (mut remote, hidden) = self.remote_tree(folder, &base, naming, &ignore).await?;
        hide(&hidden, &mut base, &mut local, &mut remote);
        let mut work = 0;
        work += self
            .moves(folder, top_role, &mut base, &mut local, &mut remote)
            .await?;
        work += self
            .deletes(folder, &mut base, &mut local, &mut remote)
            .await?;
        work += self
            .creates_and_changes(folder, top_role, &mut base, &mut local, &mut remote)
            .await?;
        if let Some(placeholders) = self.placeholders_of(folder) {
            self.pins(folder, placeholders, &ignore).await?;
        }
        Ok(work)
    }

    /// What the user asked of an on-demand folder's files: "Always keep on
    /// this device" downloads one, "Free up space" frees it. Not counted as
    /// work: neither changes what is agreed on.
    async fn pins(
        self: &Arc<Self>,
        folder: &Folder,
        placeholders: Arc<dyn Placeholders>,
        ignore: &Ignore,
    ) -> Result<(), Error> {
        let root = folder.local();
        for (path, stat) in local::scan(&root, ignore)? {
            if stat.kind != Kind::File {
                continue;
            }
            let full = join(&root, &path);
            let Ok(Some(state)) = placeholders.state(&full) else {
                continue;
            };
            match state.pin {
                PinAction::Nothing => {}
                PinAction::Hydrate => {
                    // Off the engine's tasks: the system calls the fetch,
                    // which runs on them.
                    let placeholders = placeholders.clone();
                    let done = tokio::task::spawn_blocking(move || placeholders.hydrate(&full))
                        .await
                        .map_err(|e| Error::Invalid(e.to_string()))?;
                    if self.tolerate(done.map_err(Error::from))?.is_some() {
                        self.activity(ActivityKind::Download, folder, &path);
                    }
                }
                PinAction::Dehydrate => {
                    self.tolerate(placeholders.dehydrate(&full).map_err(Error::from))?;
                }
            }
        }
        Ok(())
    }

    /// Where an agreed node is locally now: at its path, or moved (the same
    /// inode, else for a file the same hash, at a path not agreed on).
    async fn local_place(
        &self,
        root: &Path,
        entry: &Entry,
        base: &Base,
        local: &std::collections::BTreeMap<String, Stat>,
        claimed: &HashSet<String>,
        placeholders: Option<&dyn Placeholders>,
    ) -> Result<Option<String>, Error> {
        if let Some(stat) = local.get(&entry.path)
            && stat.kind == entry.kind()
        {
            return Ok(Some(entry.path.clone()));
        }
        let candidates = local
            .iter()
            .filter(|(path, stat)| {
                stat.kind == entry.kind() && base.at(path).is_none() && !claimed.contains(*path)
            })
            .map(|(path, stat)| (path.clone(), *stat));
        if let Some(inode) = entry.inode {
            for (path, stat) in candidates.clone() {
                if stat.inode == Some(inode) {
                    return Ok(Some(path));
                }
            }
        }
        if let Some(placeholders) = placeholders {
            // A placeholder says whose it is; hashing it would download it.
            if entry.kind() == Kind::File {
                for (path, _) in candidates {
                    let whose = placeholders
                        .state(&join(root, &path))
                        .ok()
                        .flatten()
                        .and_then(|s| s.identity);
                    if whose.is_some_and(|i| Uuid::from_bytes(i.id).to_string() == entry.node_id) {
                        return Ok(Some(path));
                    }
                }
            }
            return Ok(None);
        }
        if entry.kind() == Kind::File
            && let Some(hash) = &entry.hash
            && entry.inode.is_none()
        {
            for (path, stat) in candidates {
                if Some(stat.size) == entry.size
                    && &local::hash_file(&join(root, &path)).await? == hash
                {
                    return Ok(Some(path));
                }
            }
        }
        Ok(None)
    }

    /// 1. Moves and renames, parents first.
    async fn moves(
        self: &Arc<Self>,
        folder: &Folder,
        top: Option<Role>,
        base: &mut Base,
        local: &mut std::collections::BTreeMap<String, Stat>,
        remote: &mut HashMap<Uuid, RNode>,
    ) -> Result<usize, Error> {
        let root = folder.local();
        let placeholders = self.placeholders_of(folder);
        let mut ids: Vec<(usize, String, String)> = base
            .by_id
            .values()
            .map(|e| (depth(&e.path), e.path.clone(), e.node_id.clone()))
            .collect();
        ids.sort();
        let mut claimed: HashSet<String> = HashSet::new();
        let mut work = 0;
        for (_, _, id) in ids {
            let Some(entry) = base.by_id.get(&id).cloned() else {
                continue;
            };
            let Ok(uuid) = id.parse::<Uuid>() else {
                continue;
            };
            let Some(r) = remote.get(&uuid).cloned() else {
                continue;
            };
            let Some(at) = self
                .local_place(
                    &root,
                    &entry,
                    base,
                    local,
                    &claimed,
                    placeholders.as_deref(),
                )
                .await?
            else {
                continue;
            };
            claimed.insert(at.clone());
            let remote_moved = r.parent.to_string() != entry.parent_id || r.name != entry.name;
            let local_moved = at != entry.path;
            if !remote_moved && !local_moved {
                continue;
            }
            if remote_moved {
                // The server's place wins (when both moved, too).
                if at != r.path {
                    if local.contains_key(&r.path) {
                        self.tolerate::<()>(Err(Error::Invalid(format!(
                            "{} cannot be moved to {}: something is there",
                            at, r.path
                        ))))?;
                        continue;
                    }
                    let to = join(&root, &r.path);
                    if let Some(parent) = to.parent() {
                        std::fs::create_dir_all(parent)?;
                    }
                    std::fs::rename(join(&root, &at), &to)?;
                    rebase_local(local, &at, &r.path);
                }
                let mut moved = entry.clone();
                moved.path = r.path.clone();
                moved.parent_id = r.parent.to_string();
                moved.name = r.name.clone();
                // Only the move happened: one version step.
                if r.version == entry.version + 1 {
                    moved.version = r.version;
                }
                self.state
                    .move_entries(folder.id, &entry.path, &r.path)
                    .await?;
                base.rebase(&entry.path, &r.path);
                self.state.put_entry(&moved).await?;
                base.put(moved);
                self.activity(ActivityKind::Move, folder, &r.path);
                work += 1;
                continue;
            }
            // Moved here only: the server is told, when the role allows it
            // (a change of the node, an addition where it goes). Else it is
            // left as it is on both sides, out of the rest of the pass:
            // neither a delete there nor something new here.
            let into = role_in(parent_of(&at), top, base, remote);
            if let Some(why) = refused(r.role, Role::may_change).or(refused(into, Role::may_add)) {
                self.hold(folder, &at, why);
                for under in base.under(&entry.path) {
                    base.remove(&under.node_id);
                }
                local.retain(|p, _| !is_within(p, &at));
                remote.retain(|_, n| !is_within(&n.path, &r.path));
                continue;
            }
            let Some(parent) =
                self.tolerate(self.remote_dir(folder, parent_of(&at), base, local).await)?
            else {
                continue;
            };
            let name = name_of(&at);
            // Across an encrypted folder's edge the server moves nothing:
            // let go of, it goes to the trash there and is sent anew.
            let Some(Some(named)) = self.tolerate(
                self.e2e
                    .move_name(&self.state, &self.remote, uuid, r.e2e, parent, name)
                    .await,
            )?
            else {
                continue;
            };
            let rename = match named.plain {
                // Sealed for its (new) parent: sent at every move.
                Some(_) => Some(named.send.as_str()),
                None => (name != name_of(&entry.path)).then_some(name),
            };
            let reparent = (parent != r.parent).then_some(parent);
            let moved = match self
                .remote
                .move_node(uuid, reparent, rename, r.version)
                .await
            {
                Ok(node) => node,
                // Changed meanwhile: the next pass sees it.
                Err(Error::Conflict(_) | Error::NotFound) => continue,
                Err(e) => {
                    self.tolerate::<()>(Err(e))?;
                    continue;
                }
            };
            self.state.put_remote(&moved).await?;
            let shown = named.shown(&moved.name);
            let mut entry_moved = entry.clone();
            entry_moved.path = at.clone();
            entry_moved.parent_id = parent.to_string();
            entry_moved.name = shown.clone();
            if r.version == entry.version {
                entry_moved.version = moved.version;
            }
            self.state.move_entries(folder.id, &entry.path, &at).await?;
            base.rebase(&entry.path, &at);
            self.state.put_entry(&entry_moved).await?;
            base.put(entry_moved);
            // The server's tree follows, its subtree's paths too.
            for node in remote.values_mut() {
                if node.id == uuid {
                    node.parent = parent;
                    node.name = shown.clone();
                    node.version = moved.version;
                    node.path = at.clone();
                } else if is_within(&node.path, &r.path) {
                    node.path = rebase(&node.path, &r.path, &at);
                }
            }
            self.activity(ActivityKind::Move, folder, &at);
            work += 1;
        }
        Ok(work)
    }

    /// Whether a local file is as agreed: the same size and time, or else
    /// the same hash (then the agreed size and time are brought up to date).
    async fn unchanged(
        &self,
        root: &Path,
        entry: &Entry,
        stat: &Stat,
        placeholders: Option<&dyn Placeholders>,
    ) -> Result<bool, Error> {
        if entry.kind() == Kind::Folder || !stat.differs(entry.size, entry.mtime_ns) {
            return Ok(true);
        }
        // A placeholder not downloaded, or in sync at the version agreed
        // on, is as agreed (the system clears "in sync" on a write); it is
        // not hashed, which would download it.
        let placeholder = match placeholders {
            Some(p) => p.state(&join(root, &entry.path)).ok().flatten(),
            None => None,
        };
        let as_agreed = placeholder.is_some_and(|s| {
            !s.hydrated || (s.in_sync && s.identity.is_some_and(|i| i.version == entry.version))
        });
        let hash = if as_agreed {
            entry.hash.clone()
        } else {
            Some(local::hash_file(&join(root, &entry.path)).await?)
        };
        if hash.as_ref() != entry.hash.as_ref() {
            return Ok(false);
        }
        let mut entry = entry.clone();
        entry.size = Some(stat.size);
        entry.mtime_ns = Some(stat.mtime_ns);
        entry.inode = stat.inode;
        self.state.put_entry(&entry).await?;
        Ok(true)
    }

    /// 2. Deletes: gone on one side and unchanged on the other.
    async fn deletes(
        self: &Arc<Self>,
        folder: &Folder,
        base: &mut Base,
        local: &mut std::collections::BTreeMap<String, Stat>,
        remote: &mut HashMap<Uuid, RNode>,
    ) -> Result<usize, Error> {
        let root = folder.local();
        let placeholders = self.placeholders_of(folder);
        let mut work = 0;
        let in_remote = |remote: &HashMap<Uuid, RNode>, e: &Entry| {
            e.node_id
                .parse::<Uuid>()
                .is_ok_and(|id| remote.contains_key(&id))
        };

        // Gone from the server, but not deleted: no longer seen. Left as
        // it is here, out of the pass, until it is seen again; let go of
        // once the user removed it here too.
        self.vanished
            .lock()
            .unwrap()
            .retain(|id, _| !remote.contains_key(id));
        let gone: Vec<Entry> = base
            .by_id
            .values()
            .filter(|e| !in_remote(remote, e))
            .cloned()
            .collect();
        let gone_ids: HashSet<&str> = gone.iter().map(|e| e.node_id.as_str()).collect();
        let mut trashes: HashMap<String, Option<HashSet<Uuid>>> = HashMap::new();
        let mut unseen = Vec::new();
        for top in gone
            .iter()
            .filter(|e| !gone_ids.contains(e.parent_id.as_str()))
        {
            let Ok(id) = top.node_id.parse::<Uuid>() else {
                continue;
            };
            if !self.no_longer_seen(folder, top, id, &mut trashes).await? {
                continue;
            }
            if local.contains_key(&top.path) {
                self.hold(folder, &top.path, Held::Hidden);
                unseen.push(top.path.clone());
            } else {
                for under in base.under(&top.path) {
                    self.state.remove_entry(folder.id, &under.node_id).await?;
                    base.remove(&under.node_id);
                }
            }
        }
        hide(&unseen, base, local, remote);

        // Gone from the server: files first, then the folders left empty.
        let mut gone: Vec<Entry> = base
            .by_id
            .values()
            .filter(|e| !in_remote(remote, e))
            .cloned()
            .collect();
        gone.sort_by_key(|e| (e.kind() == Kind::Folder, std::cmp::Reverse(depth(&e.path))));
        for entry in gone {
            match local.get(&entry.path).copied() {
                Some(stat) if stat.kind == Kind::File && entry.kind() == Kind::File => {
                    if self
                        .unchanged(&root, &entry, &stat, placeholders.as_deref())
                        .await?
                    {
                        let path = join(&root, &entry.path);
                        // Not downloaded: nothing of it is here to keep.
                        let empty = placeholders.as_ref().is_some_and(|p| {
                            p.state(&path).ok().flatten().is_some_and(|s| !s.hydrated)
                        });
                        if empty {
                            std::fs::remove_file(&path)?;
                        } else {
                            self.config.trash.put(&path)?;
                        }
                        local.remove(&entry.path);
                        self.activity(ActivityKind::Delete, folder, &entry.path);
                        work += 1;
                    }
                    // Changed here: kept, and sent again as a new file.
                }
                Some(stat) if stat.kind == Kind::Folder && entry.kind() == Kind::Folder => {
                    let empty = !local
                        .keys()
                        .any(|p| p != &entry.path && is_within(p, &entry.path));
                    if empty {
                        self.config.trash.put(&join(&root, &entry.path))?;
                        local.remove(&entry.path);
                        self.activity(ActivityKind::Delete, folder, &entry.path);
                        work += 1;
                    }
                }
                _ => {}
            }
            self.state.remove_entry(folder.id, &entry.node_id).await?;
            base.remove(&entry.node_id);
        }

        // Gone from here: folders first (one delete takes what is in it),
        // when nothing in them changed on the server.
        let mut gone: Vec<Entry> = base
            .by_id
            .values()
            .filter(|e| !local.contains_key(&e.path))
            .cloned()
            .collect();
        gone.sort_by_key(|e| (e.kind() == Kind::File, depth(&e.path)));
        for entry in gone {
            if !base.by_id.contains_key(&entry.node_id) {
                continue; // under a folder deleted just now
            }
            let Ok(id) = entry.node_id.parse::<Uuid>() else {
                continue;
            };
            let Some(r) = remote.get(&id).cloned() else {
                continue;
            };
            let under = base.under(&entry.path);
            // Not the user's to delete (view only, upload only): the
            // server's copy comes back, as new to this side.
            if r.role.is_some_and(|role| !role.may_change()) {
                for gone in under {
                    self.state.remove_entry(folder.id, &gone.node_id).await?;
                    base.remove(&gone.node_id);
                }
                self.activity(ActivityKind::Restore, folder, &entry.path);
                continue;
            }
            let untouched = r.version == entry.version
                && remote
                    .values()
                    .filter(|n| n.id != id && is_within(&n.path, &r.path))
                    .all(|n| {
                        base.by_id
                            .get(&n.id.to_string())
                            .is_some_and(|e| e.version == n.version)
                    });
            if untouched {
                match self.remote.delete(id, r.version).await {
                    Ok(()) | Err(Error::NotFound) => {}
                    // Changed meanwhile: it comes back from the server.
                    Err(Error::Conflict(_)) => {}
                    Err(e) => {
                        self.tolerate::<()>(Err(e))?;
                        continue;
                    }
                }
                self.state.remove_remote(id).await?;
                remote.retain(|_, n| !is_within(&n.path, &r.path));
                self.activity(ActivityKind::Delete, folder, &entry.path);
                work += 1;
            }
            // Changed on the server, or deleted there just now: what is
            // there comes back (the entries go, so it is new to this side).
            for gone in under {
                self.state.remove_entry(folder.id, &gone.node_id).await?;
                base.remove(&gone.node_id);
            }
        }
        Ok(work)
    }

    /// Whether a node gone from the mirror is only no longer seen by the
    /// user (limited access, a grant removed, upload only), rather than
    /// deleted or moved out of the synced folder: the server does not
    /// answer for it, and its drive's trash (`trashes`, read once a pass)
    /// does not have it. A trash that cannot be read tells nothing, and
    /// nothing is deleted here on a guess: no longer seen.
    async fn no_longer_seen(
        &self,
        folder: &Folder,
        entry: &Entry,
        id: Uuid,
        trashes: &mut HashMap<String, Option<HashSet<Uuid>>>,
    ) -> Result<bool, Error> {
        if let Some(&unseen) = self.vanished.lock().unwrap().get(&id) {
            return Ok(unseen);
        }
        let unseen = match self.remote.node(id).await {
            // There still, elsewhere: moved out of the synced folder.
            Ok(_) => false,
            Err(Error::NotFound) => {
                // Its drive is its parent's (the server moves nothing
                // between drives).
                let parent = match entry.parent_id.parse::<Uuid>() {
                    Ok(parent) => self.state.remote(parent).await?,
                    Err(_) => None,
                };
                match parent
                    .and_then(|p| p.drive_id)
                    .or_else(|| folder.drive_id.clone())
                {
                    None => true,
                    Some(drive) => {
                        if !trashes.contains_key(&drive) {
                            let trashed = match self.remote.trashed(&drive).await {
                                Ok(ids) => Some(ids.into_iter().collect()),
                                Err(e @ (Error::Offline(_) | Error::Unauthorized)) => {
                                    return Err(e);
                                }
                                Err(_) => None,
                            };
                            trashes.insert(drive.clone(), trashed);
                        }
                        !trashes[&drive].as_ref().is_some_and(|t| t.contains(&id))
                    }
                }
            }
            Err(e @ (Error::Offline(_) | Error::Unauthorized)) => return Err(e),
            // Asked again at the next pass.
            Err(_) => return Ok(true),
        };
        self.vanished.lock().unwrap().insert(id, unseen);
        Ok(unseen)
    }

    /// The node of a local folder on the server, made there (with its
    /// parents) when it is not yet.
    fn remote_dir<'a>(
        self: &'a Arc<Self>,
        folder: &'a Folder,
        path: &'a str,
        base: &'a mut Base,
        local: &'a std::collections::BTreeMap<String, Stat>,
    ) -> std::pin::Pin<Box<dyn std::future::Future<Output = Result<Uuid, Error>> + Send + 'a>> {
        Box::pin(async move {
            if path.is_empty() {
                return Ok(folder.remote_id());
            }
            if let Some(entry) = base.at(path)
                && entry.kind() == Kind::Folder
                && let Ok(id) = entry.node_id.parse()
            {
                return Ok(id);
            }
            let parent = self
                .remote_dir(folder, parent_of(path), &mut *base, local)
                .await?;
            let named = self
                .e2e
                .new_name(
                    &self.state,
                    &self.remote,
                    parent,
                    &names::nfc(name_of(path)),
                )
                .await?;
            let node = self.remote.create_folder(parent, &named.send).await?;
            self.state.put_remote(&node).await?;
            let stat = local.get(path).copied();
            let entry = Entry {
                folder: folder.id,
                node_id: node.id.to_string(),
                path: path.to_string(),
                parent_id: parent.to_string(),
                name: named.shown(&node.name),
                kind: Kind::Folder.as_str().into(),
                version: node.version,
                hash: None,
                size: Some(0),
                mtime_ns: stat.map(|s| s.mtime_ns),
                inode: stat.and_then(|s| s.inode),
            };
            self.state.put_entry(&entry).await?;
            base.put(entry);
            self.activity(ActivityKind::Upload, folder, path);
            Ok(node.id)
        })
    }

    /// 3. What is new or changed on either side.
    async fn creates_and_changes(
        self: &Arc<Self>,
        folder: &Folder,
        top: Option<Role>,
        base: &mut Base,
        local: &mut std::collections::BTreeMap<String, Stat>,
        remote: &mut HashMap<Uuid, RNode>,
    ) -> Result<usize, Error> {
        let root = folder.local();
        let placeholders = self.placeholders_of(folder);
        let mut work = 0;
        let mut jobs: Vec<Job> = Vec::new();
        let mut handled: HashSet<String> = HashSet::new();
        let settle_ns = self.config.settle.as_nanos() as i64;
        let now_ns = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_nanos() as i64)
            .unwrap_or(0);
        let settling = |stat: &Stat| stat.kind == Kind::File && now_ns - stat.mtime_ns < settle_ns;

        // New on the server, parents first.
        let mut fresh: Vec<RNode> = remote
            .values()
            .filter(|n| !base.by_id.contains_key(&n.id.to_string()))
            .cloned()
            .collect();
        fresh.sort_by_key(|n| (depth(&n.path), n.path.clone()));
        for node in fresh {
            if base.at(&node.path).is_some() {
                // Its place is another node's still (a move to come).
                continue;
            }
            handled.insert(node.path.clone());
            match (node.kind, local.get(&node.path).copied()) {
                (Kind::Folder, Some(stat)) if stat.kind == Kind::Folder => {
                    self.put_folder_entry(folder, base, &node, Some(stat))
                        .await?;
                }
                (Kind::Folder, None) => {
                    std::fs::create_dir_all(join(&root, &node.path))?;
                    let stat = local::stat(&join(&root, &node.path));
                    if let Some(stat) = stat {
                        local.insert(node.path.clone(), stat);
                    }
                    self.put_folder_entry(folder, base, &node, stat).await?;
                    self.activity(ActivityKind::Download, folder, &node.path);
                    work += 1;
                }
                (Kind::File, Some(stat)) if stat.kind == Kind::File => {
                    if settling(&stat) {
                        self.again.store(true, Ordering::SeqCst);
                        continue;
                    }
                    jobs.push(Job::Meet { node, local: stat });
                }
                (Kind::File, None) => jobs.push(Job::Download { node, before: None }),
                (_, Some(_)) => {
                    self.tolerate::<()>(Err(Error::Invalid(format!(
                        "{}: a file on one side, a folder on the other",
                        node.path
                    ))))?;
                }
            }
        }

        // Agreed on before, and changed since on either side.
        let entries: Vec<Entry> = base
            .by_id
            .values()
            .filter(|e| e.kind() == Kind::File)
            .cloned()
            .collect();
        for entry in entries {
            let Ok(id) = entry.node_id.parse::<Uuid>() else {
                continue;
            };
            let (Some(node), Some(stat)) =
                (remote.get(&id).cloned(), local.get(&entry.path).copied())
            else {
                continue;
            };
            if stat.kind != Kind::File {
                continue;
            }
            handled.insert(entry.path.clone());
            let remote_changed = node.version != entry.version;
            let local_changed = stat.differs(entry.size, entry.mtime_ns);
            if local_changed && settling(&stat) {
                self.again.store(true, Ordering::SeqCst);
                continue;
            }
            let local_changed = local_changed
                && !self
                    .unchanged(&root, &entry, &stat, placeholders.as_deref())
                    .await?;
            match (remote_changed, local_changed) {
                (false, false) => {}
                (true, false) => jobs.push(Job::Download {
                    node,
                    before: Some(stat),
                }),
                // Changed here where the user may not change it: kept as it
                // is, not sent.
                (false, true) => match refused(node.role, Role::may_change) {
                    Some(why) => self.hold(folder, &entry.path, why),
                    None => jobs.push(Job::Replace { entry, local: stat }),
                },
                (true, true) => jobs.push(Job::Meet { node, local: stat }),
            }
        }

        // New here, parents first: folders made now, files sent below.
        let fresh: Vec<(String, Stat)> = local
            .iter()
            .filter(|(path, _)| base.at(path).is_none() && !handled.contains(*path))
            .map(|(p, s)| (p.clone(), *s))
            .collect();
        let mut fresh = fresh;
        fresh.sort_by_key(|(p, _)| (depth(p), p.clone()));
        let remote_paths: HashSet<&str> = remote.values().map(|n| n.path.as_str()).collect();
        for (path, stat) in fresh {
            // The server has something there, met once it is agreed on.
            if remote_paths.contains(path.as_str()) {
                continue;
            }
            // Made here where the user may only view: kept, not sent.
            if let Some(why) = refused(role_in(parent_of(&path), top, base, remote), Role::may_add)
            {
                self.hold(folder, &path, why);
                continue;
            }
            match stat.kind {
                Kind::Folder => {
                    if self
                        .tolerate(self.remote_dir(folder, &path, base, local).await)?
                        .is_some()
                    {
                        work += 1;
                    }
                }
                Kind::File => {
                    if settling(&stat) {
                        self.again.store(true, Ordering::SeqCst);
                        continue;
                    }
                    let Some(parent) = self
                        .tolerate(self.remote_dir(folder, parent_of(&path), base, local).await)?
                    else {
                        continue;
                    };
                    jobs.push(Job::Upload {
                        parent,
                        path,
                        local: stat,
                    });
                }
            }
        }

        work += self.run_jobs(folder, jobs).await?;
        Ok(work)
    }

    async fn put_folder_entry(
        &self,
        folder: &Folder,
        base: &mut Base,
        node: &RNode,
        stat: Option<Stat>,
    ) -> Result<(), Error> {
        let entry = Entry {
            folder: folder.id,
            node_id: node.id.to_string(),
            path: node.path.clone(),
            parent_id: node.parent.to_string(),
            name: node.name.clone(),
            kind: Kind::Folder.as_str().into(),
            version: node.version,
            hash: None,
            size: Some(0),
            mtime_ns: stat.map(|s| s.mtime_ns),
            inode: stat.and_then(|s| s.inode),
        };
        self.state.put_entry(&entry).await?;
        base.put(entry);
        Ok(())
    }

    /// The transfers, `config.transfers` at a time. A server that goes away
    /// stops them all; another failure stops only its own.
    async fn run_jobs(self: &Arc<Self>, folder: &Folder, jobs: Vec<Job>) -> Result<usize, Error> {
        if jobs.is_empty() {
            return Ok(0);
        }
        self.update(|s| s.pending += jobs.len());
        let mut work = 0;
        let mut set = JoinSet::new();
        let mut jobs = jobs.into_iter();
        let mut fatal = None;
        loop {
            while set.len() < self.config.transfers.max(1) && fatal.is_none() {
                let Some(job) = jobs.next() else { break };
                let this = self.clone();
                let folder = folder.clone();
                set.spawn(async move { this.run_job(&folder, job).await });
            }
            let Some(done) = set.join_next().await else {
                break;
            };
            match done.map_err(|e| Error::Invalid(e.to_string())) {
                Ok(Ok(true)) => work += 1,
                Ok(Ok(false)) => {}
                Ok(Err(e)) | Err(e) => match self.tolerate::<()>(Err(e)) {
                    Ok(_) => {}
                    Err(e) => {
                        fatal.get_or_insert(e);
                    }
                },
            }
        }
        match fatal {
            Some(e) => Err(e),
            None => Ok(work),
        }
    }

    /// One transfer; whether it changed anything.
    async fn run_job(self: &Arc<Self>, folder: &Folder, job: Job) -> Result<bool, Error> {
        let root = folder.local();
        let placeholders = self.placeholders_of(folder);
        match job {
            Job::Download { node, before } => {
                let path = join(&root, &node.path);
                if let Some(placeholders) = &placeholders {
                    let state = placeholders.state(&path).ok().flatten();
                    let fresh = before.is_none();
                    if fresh || state.is_some_and(|s| !s.hydrated) {
                        if fresh && local::stat(&path).is_some() {
                            return Ok(false); // made here meanwhile: the next pass
                        }
                        // Not here, or not downloaded: a placeholder of the
                        // server's version, downloaded when opened.
                        let file = self.placeholder(&node).await?;
                        if fresh {
                            placeholders.create(&path, &file)?;
                        } else {
                            placeholders.update(&path, &file)?;
                        }
                        self.put_placeholder_entry(folder, &node, &path, file.identity.version)
                            .await?;
                        self.done_quietly();
                        return Ok(true);
                    }
                }
                let keys = self.e2e.require(&self.remote, node.e2e).await?;
                let downloaded = self.remote.download(node.id, parent_dir(&path)).await?;
                let downloaded = self.e2e.open_download(keys, downloaded).await?;
                // Changed here meanwhile: both are kept.
                if local::stat(&path) != before {
                    self.conflict_copy(folder, &node.path)?;
                }
                std::fs::rename(&downloaded.temp, &path)?;
                self.adopt(placeholders.as_deref(), &node, &path, downloaded.version)
                    .await?;
                self.put_file_entry(folder, &node, &path, downloaded.version, downloaded.hash)
                    .await?;
                self.activity(ActivityKind::Download, folder, &node.path);
                Ok(true)
            }
            Job::Meet { node, local: stat } => {
                let path = join(&root, &node.path);
                // This node's own placeholder (the agreement was lost):
                // agreed on again as it is.
                if let Some(placeholders) = &placeholders
                    && let Ok(Some(state)) = placeholders.state(&path)
                    && let Some(identity) = state.identity
                    && Uuid::from_bytes(identity.id) == node.id
                    && (!state.hydrated || state.in_sync)
                {
                    self.put_placeholder_entry(folder, &node, &path, identity.version)
                        .await?;
                    return Ok(false);
                }
                let keys = self.e2e.require(&self.remote, node.e2e).await?;
                let downloaded = self.remote.download(node.id, parent_dir(&path)).await?;
                let downloaded = self.e2e.open_download(keys, downloaded).await?;
                let same = local::stat(&path) == Some(stat)
                    && local::hash_file(&path).await? == downloaded.hash;
                if same {
                    let _ = std::fs::remove_file(&downloaded.temp);
                    self.adopt(placeholders.as_deref(), &node, &path, downloaded.version)
                        .await?;
                    self.put_file_entry(folder, &node, &path, downloaded.version, downloaded.hash)
                        .await?;
                    return Ok(false);
                }
                if path.exists() {
                    self.conflict_copy(folder, &node.path)?;
                }
                std::fs::rename(&downloaded.temp, &path)?;
                self.adopt(placeholders.as_deref(), &node, &path, downloaded.version)
                    .await?;
                self.put_file_entry(folder, &node, &path, downloaded.version, downloaded.hash)
                    .await?;
                Ok(true)
            }
            Job::Replace { entry, local: stat } => {
                let path = join(&root, &entry.path);
                if local::stat(&path) != Some(stat) {
                    return Ok(false); // changed again: the next pass
                }
                let id: Uuid = entry.node_id.parse().unwrap_or_default();
                let region = self.e2e.region(&self.state, id).await?;
                let keys = self.e2e.require(&self.remote, region).await?;
                let hash = local::hash_file(&path).await?;
                // In an encrypted folder, its encrypted copy is what goes.
                let source = self
                    .e2e
                    .source(
                        &self.state,
                        keys.as_deref(),
                        folder.id,
                        &entry.path,
                        &path,
                        stat,
                    )
                    .await?;
                let sent = if (source.stat.size as u64) < PLAIN_MAX {
                    self.remote
                        .replace_plain(id, entry.version, &source.path)
                        .await
                } else {
                    self.resumable(
                        folder.id,
                        &entry.path,
                        &source.path,
                        source.stat,
                        Target::Replace {
                            id,
                            if_version: entry.version,
                        },
                    )
                    .await
                };
                self.e2e.sent(&source, &sent);
                let node = match sent {
                    Ok(node) => node,
                    Err(Error::Conflict(_)) => {
                        // Changed on the server since: a conflict, and the
                        // server's version comes down at the next pass.
                        let mut stale = entry.clone();
                        stale.size = None;
                        self.state.put_entry(&stale).await?;
                        return Ok(true);
                    }
                    Err(e) => return Err(e),
                };
                self.state.put_remote(&node).await?;
                self.adopt_sent(placeholders.as_deref(), &node, &path)?;
                let after = local::stat(&path).unwrap_or(stat);
                let mut entry = entry;
                entry.version = node.version;
                entry.hash = Some(hash);
                entry.size = Some(after.size);
                entry.mtime_ns = Some(after.mtime_ns);
                entry.inode = after.inode;
                self.state.put_entry(&entry).await?;
                self.activity(ActivityKind::Upload, folder, &entry.path);
                Ok(true)
            }
            Job::Upload {
                parent,
                path: relative,
                local: stat,
            } => {
                let path = join(&root, &relative);
                if local::stat(&path) != Some(stat) {
                    return Ok(false);
                }
                let name = names::nfc(name_of(&relative));
                let named = match self
                    .e2e
                    .new_name(&self.state, &self.remote, parent, &name)
                    .await
                {
                    Ok(named) => named,
                    // Taken in the encrypted folder: met at the next pass.
                    Err(Error::Conflict(_)) => return Ok(false),
                    Err(e) => return Err(e),
                };
                let hash = local::hash_file(&path).await?;
                let source = self
                    .e2e
                    .source(
                        &self.state,
                        named.keys.as_deref(),
                        folder.id,
                        &relative,
                        &path,
                        stat,
                    )
                    .await?;
                let sent = if (source.stat.size as u64) < PLAIN_MAX {
                    self.remote
                        .upload_new_plain(parent, &named.send, &source.path)
                        .await
                } else {
                    self.resumable(
                        folder.id,
                        &relative,
                        &source.path,
                        source.stat,
                        Target::New {
                            parent,
                            name: &named.send,
                        },
                    )
                    .await
                };
                self.e2e.sent(&source, &sent);
                let node = match sent {
                    Ok(node) => node,
                    // Made on the server meanwhile: met at the next pass.
                    Err(Error::Conflict(_)) => return Ok(false),
                    Err(e) => return Err(e),
                };
                self.state.put_remote(&node).await?;
                self.adopt_sent(placeholders.as_deref(), &node, &path)?;
                let after = local::stat(&path).unwrap_or(stat);
                self.state
                    .put_entry(&Entry {
                        folder: folder.id,
                        node_id: node.id.to_string(),
                        path: relative.clone(),
                        parent_id: parent.to_string(),
                        name: named.shown(&node.name),
                        kind: Kind::File.as_str().into(),
                        version: node.version,
                        hash: Some(hash),
                        size: Some(after.size),
                        mtime_ns: Some(after.mtime_ns),
                        inode: after.inode,
                    })
                    .await?;
                self.activity(ActivityKind::Upload, folder, &relative);
                Ok(true)
            }
        }
    }

    async fn put_file_entry(
        &self,
        folder: &Folder,
        node: &RNode,
        path: &Path,
        version: i64,
        hash: String,
    ) -> Result<(), Error> {
        let stat = local::stat(path);
        self.state
            .put_entry(&Entry {
                folder: folder.id,
                node_id: node.id.to_string(),
                path: node.path.clone(),
                parent_id: node.parent.to_string(),
                name: node.name.clone(),
                kind: Kind::File.as_str().into(),
                version,
                hash: Some(hash),
                size: stat.map(|s| s.size),
                mtime_ns: stat.map(|s| s.mtime_ns),
                inode: stat.and_then(|s| s.inode),
            })
            .await
    }

    /// A file of the server as a placeholder shows it: its size and time
    /// from the mirror, or asked for (the change feed does not say them).
    async fn placeholder(&self, node: &RNode) -> Result<Placeholder, Error> {
        let (version, size, modified) = match (node.size, &node.modified) {
            (Some(size), Some(modified)) => (node.version, size, modified.clone()),
            _ => {
                let fresh = self.remote.node(node.id).await?;
                (
                    fresh.version,
                    fresh.size.unwrap_or(0),
                    fresh.modified.unwrap_or_default(),
                )
            }
        };
        Ok(Placeholder {
            identity: on_demand::identity(node.id, version),
            size: size.max(0) as u64,
            modified_ns: on_demand::unix_ns(&modified),
        })
    }

    /// A placeholder agreed on: no hash (its content is not here).
    async fn put_placeholder_entry(
        &self,
        folder: &Folder,
        node: &RNode,
        path: &Path,
        version: i64,
    ) -> Result<(), Error> {
        let stat = local::stat(path);
        self.state
            .put_entry(&Entry {
                folder: folder.id,
                node_id: node.id.to_string(),
                path: node.path.clone(),
                parent_id: node.parent.to_string(),
                name: node.name.clone(),
                kind: Kind::File.as_str().into(),
                version,
                hash: None,
                size: stat.map(|s| s.size),
                mtime_ns: stat.map(|s| s.mtime_ns),
                inode: stat.and_then(|s| s.inode),
            })
            .await
    }

    /// A file just downloaded into an on-demand folder becomes the
    /// server's placeholder, in sync (so that it can be freed again).
    async fn adopt(
        &self,
        placeholders: Option<&dyn Placeholders>,
        node: &RNode,
        path: &Path,
        version: i64,
    ) -> Result<(), Error> {
        let Some(placeholders) = placeholders else {
            return Ok(());
        };
        let stat = local::stat(path);
        placeholders.adopt(
            path,
            &Placeholder {
                identity: on_demand::identity(node.id, version),
                size: stat.map_or(0, |s| s.size.max(0) as u64),
                modified_ns: stat.map_or(0, |s| s.mtime_ns),
            },
        )?;
        Ok(())
    }

    /// A file just sent from an on-demand folder: the same, at the version
    /// the server gave it.
    fn adopt_sent(
        &self,
        placeholders: Option<&dyn Placeholders>,
        node: &remote::Node,
        path: &Path,
    ) -> Result<(), Error> {
        let Some(placeholders) = placeholders else {
            return Ok(());
        };
        let stat = local::stat(path);
        placeholders.adopt(
            path,
            &Placeholder {
                identity: on_demand::identity(node.id, node.version),
                size: stat.map_or(0, |s| s.size.max(0) as u64),
                modified_ns: stat.map_or(0, |s| s.mtime_ns),
            },
        )?;
        Ok(())
    }

    /// The local file at `relative` moves aside as a conflict copy (sent to
    /// the server at the next pass, as a new file); the user is told.
    fn conflict_copy(&self, folder: &Folder, relative: &str) -> Result<(), Error> {
        let root = folder.local();
        let date = jiff::Zoned::now().date().to_string();
        let base = names::conflict_name(name_of(relative), &self.config.device_name, &date);
        let mut copy = child(parent_of(relative), &base);
        let mut n = 2;
        while join(&root, &copy).exists() {
            copy = child(
                parent_of(relative),
                &names::with_suffix(&base, &format!(" {n}")),
            );
            n += 1;
        }
        std::fs::rename(join(&root, relative), join(&root, &copy))?;
        tracing::info!("conflict: {relative} kept as {copy}");
        let name = folder
            .local()
            .file_name()
            .map(|n| n.to_string_lossy().into_owned())
            .unwrap_or_default();
        let (path, copy) = (child(&name, relative), child(&name, &copy));
        self.update(|s| {
            s.conflicts.insert(
                0,
                Conflict {
                    path: path.clone(),
                    copy: copy.clone(),
                    at: status::now(),
                },
            );
            s.conflicts.truncate(status::CONFLICTS);
        });
        self.activity(ActivityKind::Conflict, folder, relative);
        let _ = self.events.send(Event::Conflict { path, copy });
        Ok(())
    }

    /// A resumable upload, carried on from where a break left it when the
    /// file is as it was then.
    async fn resumable(
        &self,
        folder: i64,
        relative: &str,
        path: &Path,
        stat: Stat,
        target: Target<'_>,
    ) -> Result<remote::Node, Error> {
        let made = self
            .resumable_json(folder, relative, path, stat, target)
            .await?;
        let node: crate::api::types::Node = serde_json::from_value(made)
            .map_err(|e| Error::Invalid(format!("An unexpected answer: {e}")))?;
        Ok(node.into())
    }

    async fn resumable_json(
        &self,
        folder: i64,
        relative: &str,
        path: &Path,
        stat: Stat,
        target: Target<'_>,
    ) -> Result<serde_json::Value, Error> {
        let mut resume = None;
        if let Some(upload) = self.state.upload(folder, relative).await? {
            if upload.size == stat.size && upload.mtime_ns == stat.mtime_ns {
                resume = Some(upload.url);
            } else {
                self.remote.cancel_upload(&upload.url).await;
                self.state.remove_upload(folder, relative).await?;
            }
        }
        let resumed = match resume {
            Some(url) => self
                .remote
                .upload_offset(&url)
                .await?
                .map(|offset| (url, offset)),
            None => None,
        };
        let (url, offset) = match resumed {
            Some(at) => at,
            None => {
                let url = self.remote.create_upload(&target, stat.size as u64).await?;
                self.state
                    .put_upload(
                        folder,
                        relative,
                        &Upload {
                            url: url.clone(),
                            size: stat.size,
                            mtime_ns: stat.mtime_ns,
                        },
                    )
                    .await?;
                (url, 0)
            }
        };
        match self.remote.finish_upload(&url, path, offset).await {
            // A break: carried on at the next pass.
            Err(e) if e.waits() => Err(e),
            result => {
                self.state.remove_upload(folder, relative).await?;
                result
            }
        }
    }

    // --- The drop box folder ---------------------------------------------

    /// What is in the drop box folder goes to the drop box, and is removed
    /// here once the server has it.
    async fn drop_box(&self, dir: &Path) -> Result<usize, Error> {
        std::fs::create_dir_all(dir)?;
        let ignore = self.ignore();
        let found = local::scan(dir, &ignore)?;
        let settle_ns = self.config.settle.as_nanos() as i64;
        let now_ns = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_nanos() as i64)
            .unwrap_or(0);
        let mut work = 0;
        let mut folders = Vec::new();
        for (relative, stat) in &found {
            if stat.kind == Kind::Folder {
                folders.push(relative.clone());
                continue;
            }
            if now_ns - stat.mtime_ns < settle_ns {
                self.again.store(true, Ordering::SeqCst);
                continue;
            }
            let path = join(dir, relative);
            let name = names::nfc(name_of(relative));
            self.update(|s| s.pending += 1);
            let sent = self
                .resumable_json(0, relative, &path, *stat, Target::DropBox { name: &name })
                .await;
            if self.tolerate(sent)?.is_some() {
                std::fs::remove_file(&path)?;
                self.update(|s| {
                    s.recent.insert(
                        0,
                        Activity {
                            kind: ActivityKind::DropBox,
                            path: name.clone(),
                            at: status::now(),
                        },
                    );
                    s.recent.truncate(status::RECENT);
                    s.done += 1;
                    s.pending = s.pending.saturating_sub(1);
                });
                work += 1;
            }
        }
        // The folders emptied by it go too.
        folders.sort_by_key(|p| std::cmp::Reverse(depth(p)));
        for relative in folders {
            let _ = std::fs::remove_dir(join(dir, &relative));
        }
        if work > 0 {
            let _ = self.events.send(Event::DropBox);
        }
        Ok(work)
    }
}

/// An on-demand folder made a sync root and connected (off the engine's
/// tasks: the system's calls block).
async fn connect(placeholders: Arc<dyn Placeholders>, folder: &Folder) -> Result<(), Error> {
    let root = folder.local();
    let account = folder.remote_id.clone();
    let name = root
        .file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_else(|| "Organizer".into());
    tokio::task::spawn_blocking(move || placeholders.connect(&root, &account, &name))
        .await
        .map_err(|e| Error::Invalid(e.to_string()))??;
    Ok(())
}

/// What cannot be read (an encrypted folder locked here, or without a key
/// for this user yet) is left out of the pass, on both sides and in what
/// they agreed on: never taken for a delete, a move or something new.
fn hide(
    paths: &[String],
    base: &mut Base,
    local: &mut std::collections::BTreeMap<String, Stat>,
    remote: &mut HashMap<Uuid, RNode>,
) {
    for path in paths {
        let ids: Vec<String> = base
            .by_id
            .values()
            .filter(|e| is_within(&e.path, path))
            .map(|e| e.node_id.clone())
            .collect();
        for id in ids {
            base.remove(&id);
        }
        local.retain(|p, _| !is_within(p, path));
        remote.retain(|_, n| !is_within(&n.path, path));
    }
}

/// What the user may do in a local folder of the pass (`""`: the synced
/// folder's top, whose role is `top`): its node's role, or that of the
/// nearest folder above it the server has (a folder made here is not yet).
fn role_in(
    dir: &str,
    top: Option<Role>,
    base: &Base,
    remote: &HashMap<Uuid, RNode>,
) -> Option<Role> {
    let mut dir = dir;
    while !dir.is_empty() {
        let node = base
            .at(dir)
            .and_then(|e| e.node_id.parse::<Uuid>().ok())
            .and_then(|id| remote.get(&id));
        if let Some(node) = node {
            return node.role;
        }
        dir = parent_of(dir);
    }
    top
}

/// Why a role does not allow something (`allows`), when it does not. A
/// role not told allows it: the server has the last word.
fn refused(role: Option<Role>, allows: fn(Role) -> bool) -> Option<Held> {
    match role? {
        role if allows(role) => None,
        Role::UploadOnly => Some(Held::UploadOnly),
        _ => Some(Held::ViewOnly),
    }
}

/// The local paths under `from` move under `to`.
/// Whether a drive's folders wait (docs/M7C_DESIGN.md §2, §4): its storage
/// unavailable, with the server's reason; or being moved, read-only until
/// done. None: synced as usual.
pub fn storage_wait(drive: &crate::api::types::Drive) -> Option<(Held, String)> {
    if !drive.available {
        let reason = drive
            .unavailable_reason
            .clone()
            .unwrap_or_else(|| "Its storage cannot be reached".into());
        return Some((Held::Unavailable, reason));
    }
    let to = drive.moving_to.as_ref()?;
    Some((
        Held::Moving,
        format!(
            "Being moved to {}: read-only until that is done, then what changed here is sent",
            to.name
        ),
    ))
}

fn rebase_local(local: &mut std::collections::BTreeMap<String, Stat>, from: &str, to: &str) {
    let moved: Vec<(String, Stat)> = local
        .iter()
        .filter(|(p, _)| is_within(p, from))
        .map(|(p, s)| (p.clone(), *s))
        .collect();
    for (path, stat) in moved {
        local.remove(&path);
        local.insert(rebase(&path, from, to), stat);
    }
}

fn parent_dir(path: &Path) -> &Path {
    path.parent().unwrap_or(path)
}
