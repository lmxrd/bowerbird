//! What rustic does for the backups: back up, forget and prune, check, list
//! the snapshots and what is in them, restore. rustic is synchronous (and
//! its S3 backend starts a Tokio runtime of its own), so each of these runs
//! on a thread of its own.
//!
//! Each snapshot's description is a small manifest, [`Manifest`]: where
//! each user's files folder was, so that a user's file is found in a
//! snapshot even after their folder moved, and where the database copy is.

use super::{BackupStatus, Settings};
use crate::state::AppState;
use anyhow::Context;
use rustic_backend::BackendOptions;
use rustic_core::{
    BackupOptions, CheckOptions, ConfigOptions, Credentials, KeepOptions, KeyOptions,
    LocalDestination, LsOptions, PathList, PruneOptions, Repository, RepositoryOptions,
    RestoreOptions, SnapshotOptions,
};
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use utoipa::ToSchema;
use uuid::Uuid;

/// The database copy's name, in the staging folder.
pub const DATABASE_COPY: &str = "organizer.db";
/// How often the repository's structure is checked.
const CHECK_EVERY: jiff::SignedDuration = jiff::SignedDuration::from_hours(24 * 7);

/// A snapshot's description.
#[derive(Clone, Debug, Default, Serialize, Deserialize)]
pub struct Manifest {
    /// Where the database copy was.
    pub database: PathBuf,
    /// Each user's files folder (their My drive's).
    pub users: BTreeMap<Uuid, PathBuf>,
    /// Each shared drive's folder (none before drives).
    #[serde(default)]
    pub drives: BTreeMap<Uuid, PathBuf>,
}

/// A snapshot, as the API shows it.
#[derive(Clone, Debug, Serialize, ToSchema)]
#[schema(as = BackupSnapshot)]
pub struct Snapshot {
    /// The snapshot's id (hex).
    pub id: String,
    pub time: String,
    /// The bytes backed up, in all.
    #[schema(required = true)]
    pub size: Option<u64>,
}

/// A file or folder in a snapshot.
#[derive(Clone, Debug, Serialize, ToSchema)]
pub struct SnapshotEntry {
    pub name: String,
    pub kind: crate::files::Kind,
    /// In bytes; 0 for a folder.
    pub size: u64,
    #[schema(required = true)]
    pub modified: Option<String>,
}

/// Run `f` on a thread of its own.
async fn off_thread<T: Send + 'static>(
    f: impl FnOnce() -> anyhow::Result<T> + Send + 'static,
) -> anyhow::Result<T> {
    let (tx, rx) = tokio::sync::oneshot::channel();
    std::thread::spawn(move || {
        let _ = tx.send(f());
    });
    rx.await.context("the backup thread stopped")?
}

/// The repository the settings name, not opened yet.
fn repository(settings: &Settings, cache: &Path) -> anyhow::Result<Repository<()>> {
    let mut backend = BackendOptions::default();
    match settings.s3() {
        Some((endpoint, bucket, prefix)) => {
            backend = backend.repository("opendal:s3");
            let mut options = BTreeMap::new();
            options.insert("bucket".to_string(), bucket);
            options.insert("root".to_string(), format!("/{prefix}"));
            options.insert("endpoint".to_string(), endpoint);
            options.insert(
                "region".to_string(),
                if settings.region.is_empty() {
                    "us-east-1".to_string()
                } else {
                    settings.region.clone()
                },
            );
            options.insert("access_key_id".to_string(), settings.access_key.clone());
            options.insert("secret_access_key".to_string(), settings.secret_key.clone());
            backend = backend.options(options);
        }
        None => backend = backend.repository(settings.url.clone()),
    }
    let backends = backend.to_backends()?;
    let options = RepositoryOptions::default().cache_dir(cache.to_path_buf());
    Ok(Repository::new(&options, &backends)?)
}

/// Where the backups keep their staging folder and cache, in the data dir.
fn work_dir(data_dir: &Path) -> PathBuf {
    data_dir.join("backup")
}

/// Open the repository; with `create`, make it when there is none.
/// Answers whether it was made.
fn open(
    settings: &Settings,
    data_dir: &Path,
    create: bool,
) -> anyhow::Result<(Repository<rustic_core::OpenStatus>, bool)> {
    let cache = work_dir(data_dir).join("cache");
    let repo = repository(settings, &cache)?;
    let credentials = Credentials::password(&settings.password);
    if repo.config_id()?.is_none() {
        anyhow::ensure!(create, "There is no repository there yet");
        let repo = repo.init(
            &credentials,
            &KeyOptions::default(),
            &ConfigOptions::default(),
        )?;
        return Ok((repo, true));
    }
    Ok((repo.open(&credentials)?, false))
}

/// What a test of the settings found.
#[derive(Clone, Debug, Serialize, ToSchema)]
#[schema(as = BackupTestResult)]
pub struct TestResult {
    /// The repository exists; otherwise the first backup makes it.
    pub exists: bool,
    /// Its snapshots (0 when it does not exist).
    pub snapshots: usize,
}

/// Open the repository the settings name, as a test: it need not exist
/// yet, but where it is must be reachable, and an existing one must open
/// with the password.
pub async fn test(settings: Settings, data_dir: PathBuf) -> anyhow::Result<TestResult> {
    off_thread(move || {
        let cache = work_dir(&data_dir).join("cache");
        let repo = repository(&settings, &cache)?;
        if repo.config_id()?.is_none() {
            return Ok(TestResult {
                exists: false,
                snapshots: 0,
            });
        }
        let repo = repo
            .open(&Credentials::password(&settings.password))
            .context("The repository does not open with this password")?;
        Ok(TestResult {
            exists: true,
            snapshots: repo.get_all_snapshots()?.len(),
        })
    })
    .await
}

/// Back up now: the database copy, the users' folders of the data dir,
/// and each user's files folder; then forget and prune by the keep rules,
/// and check the repository once a week. The status is kept up to date.
pub async fn backup(state: &AppState) -> anyhow::Result<()> {
    let settings = state.backup.settings();
    anyhow::ensure!(settings.enabled(), "Backups are not set up");
    let started = crate::auth::iso(std::time::SystemTime::now());
    update_status(state, |status| status.last_started = Some(started)).await?;
    let result = run_backup(state, settings).await;
    let finished = crate::auth::iso(std::time::SystemTime::now());
    match &result {
        Ok(done) => {
            update_status(state, |status| {
                status.last_finished = Some(finished.clone());
                status.last_error = None;
                status.last_snapshot = Some(done.snapshot.clone());
                status.last_added = Some(done.added as i64);
                status.last_total = Some(done.total as i64);
                status.snapshots = Some(done.snapshots as i64);
                if done.checked {
                    status.last_check = Some(finished.clone());
                }
            })
            .await?;
        }
        Err(e) => {
            tracing::warn!("the backup failed: {e:#}");
            let error = format!("{e:#}");
            update_status(state, |status| {
                status.last_finished = Some(finished.clone());
                status.last_error = Some(error);
            })
            .await?;
        }
    }
    result.map(|_| ())
}

/// Change the stored backup status, in one transaction.
async fn update_status(
    state: &AppState,
    change: impl FnOnce(&mut BackupStatus),
) -> anyhow::Result<()> {
    let mut tx = state.db.writer().begin().await?;
    let mut status: BackupStatus = crate::server_settings::load(&mut *tx, "backup_status").await?;
    change(&mut status);
    crate::server_settings::save(&mut *tx, "backup_status", &status).await?;
    tx.commit().await?;
    Ok(())
}

/// What a backup takes, and its manifest: the database's copy in
/// `staging`, the users' folders in the data dir and each user's files
/// root outside it, and the shared drives on the server's disk (in the data
/// dir, or each in the folder chosen for it).
pub async fn sources(
    state: &AppState,
    staging: &Path,
    copy: PathBuf,
) -> anyhow::Result<(Vec<PathBuf>, Manifest)> {
    let data_dir = state.files.data_dir().to_path_buf();
    let local_base = state.files.local_base().to_path_buf();
    let real = |p: &Path| std::fs::canonicalize(p).unwrap_or_else(|_| p.to_path_buf());
    // Each user's files folder; those under STORAGE_URL come with it.
    let roots: Vec<(Uuid, Option<String>)> = sqlx::query_as("SELECT id, files_root FROM users")
        .fetch_all(state.db.reader())
        .await?;
    let users_dir = real(&local_base.join("users"));
    let drives_dir = real(&local_base.join("drives"));
    let mut manifest = Manifest {
        database: copy,
        users: BTreeMap::new(),
        drives: BTreeMap::new(),
    };
    let mut sources = vec![staging.to_path_buf()];
    // The key files, so that a backup alone restores the server's secrets.
    for key in ["secret.key", "ocm-key"] {
        let file = data_dir.join(key);
        if file.is_file() {
            sources.push(file);
        }
    }
    if users_dir.is_dir() {
        sources.push(users_dir.clone());
    }
    // The shared drives on the server's storage: under STORAGE_URL, or each
    // in the folder chosen for it (its trash and versions inside). Drives
    // in a storage backend are not backed up by the server
    // (docs/M7C_DESIGN.md §5): the provider's versioning is the way.
    if drives_dir.is_dir() {
        sources.push(drives_dir.clone());
    }
    let shared: Vec<Uuid> =
        sqlx::query_scalar("SELECT id FROM drives WHERE kind = 'shared' AND backend_id IS NULL")
            .fetch_all(state.db.reader())
            .await?;
    for drive in shared {
        let Ok((root, _)) = state.files.drive_root(drive).await else {
            continue;
        };
        let root = real(&root);
        if !root.starts_with(&drives_dir) && root.is_dir() && !sources.contains(&root) {
            sources.push(root.clone());
        }
        manifest.drives.insert(drive, root);
    }
    for (user, files_root) in roots {
        let stored = crate::files::StoredRoot { files_root };
        let root = real(&state.files.root_for(user, &stored));
        if !root.starts_with(&users_dir) && root.is_dir() && !sources.contains(&root) {
            sources.push(root.clone());
        }
        manifest.users.insert(user, root);
    }
    Ok((sources, manifest))
}

struct Done {
    snapshot: String,
    added: u64,
    total: u64,
    snapshots: usize,
    checked: bool,
}

async fn run_backup(state: &AppState, settings: Settings) -> anyhow::Result<Done> {
    let data_dir = state.files.data_dir().to_path_buf();
    let staging = work_dir(&data_dir).join("staging");
    std::fs::create_dir_all(&staging)?;
    // The paths as rustic records them (no symbolic links: /var is
    // /private/var on a Mac), so that the manifest's match the snapshot's.
    let real = |p: &Path| std::fs::canonicalize(p).unwrap_or_else(|_| p.to_path_buf());
    let staging = real(&staging);
    // A consistent copy of the database at one moment, while it runs.
    let copy = staging.join(DATABASE_COPY);
    let _ = std::fs::remove_file(&copy);
    sqlx::query("VACUUM INTO ?1")
        .bind(copy.to_string_lossy().as_ref())
        .execute(state.db.writer())
        .await
        .context("cannot copy the database")?;

    let (sources, manifest) = sources(state, &staging, copy.clone()).await?;
    let status: BackupStatus =
        crate::server_settings::load(state.db.reader(), "backup_status").await?;
    let last_check = status.last_check;
    let check_due = last_check
        .and_then(|t| t.parse::<jiff::Timestamp>().ok())
        .is_none_or(|t| t + CHECK_EVERY < jiff::Timestamp::now());

    let done = off_thread(move || {
        let (repo, _) = open(&settings, &data_dir, true)?;
        let repo = repo.to_indexed_ids()?;
        let paths: Vec<String> = sources
            .iter()
            .map(|p| p.to_string_lossy().into_owned())
            .collect();
        let source = PathList::from_iter(paths.iter().map(PathBuf::from))
            .sanitize()?
            .merge();
        let snapshot = SnapshotOptions::default()
            .description(serde_json::to_string(&manifest)?)
            .to_snapshot()?;
        let snapshot = repo.backup(&BackupOptions::default(), &source, snapshot)?;
        let summary = snapshot.summary.clone().unwrap_or_default();
        let repo = repo.drop_index();

        // Forget by the keep rules (the newest is always kept), then prune.
        let keep = KeepOptions::default()
            .keep_last(1)
            .keep_daily(settings.keep_daily as i32)
            .keep_weekly(settings.keep_weekly as i32)
            .keep_monthly(settings.keep_monthly as i32);
        let all = repo.get_all_snapshots()?;
        let now = jiff::Zoned::now();
        let forget: Vec<_> = keep
            .apply(all, &now)?
            .into_iter()
            .filter(|f| !f.keep)
            .map(|f| f.snapshot.id)
            .collect();
        if !forget.is_empty() {
            repo.delete_snapshots(&forget)?;
            let options = PruneOptions::default();
            let plan = repo.prune_plan(&options)?;
            repo.prune(&options, plan)?;
        }
        if check_due {
            repo.check(CheckOptions::default())?.is_ok()?;
        }
        Ok(Done {
            snapshot: snapshot.id.to_string(),
            added: summary.data_added,
            total: summary.total_bytes_processed,
            snapshots: repo.get_all_snapshots()?.len(),
            checked: check_due,
        })
    })
    .await;
    let _ = std::fs::remove_file(&copy);
    done
}

/// The snapshots, the newest first.
pub async fn snapshots(settings: Settings, data_dir: PathBuf) -> anyhow::Result<Vec<Snapshot>> {
    off_thread(move || {
        let (repo, _) = open(&settings, &data_dir, false)?;
        let mut all = repo.get_all_snapshots()?;
        all.sort_by(|a, b| b.time.cmp(&a.time));
        Ok(all
            .into_iter()
            .map(|s| Snapshot {
                id: s.id.to_string(),
                time: s.time.timestamp().to_string(),
                size: s.summary.map(|s| s.total_bytes_processed),
            })
            .collect())
    })
    .await
}

/// A user's files folder in a snapshot, from its manifest.
fn user_root(description: Option<&str>, user: Uuid) -> anyhow::Result<PathBuf> {
    let manifest: Manifest = serde_json::from_str(description.unwrap_or_default())
        .context("This snapshot was not made by Organizer")?;
    manifest
        .users
        .get(&user)
        .cloned()
        .ok_or_else(|| anyhow::anyhow!("Your files are not in this snapshot"))
}

/// A path inside a user's files folder, made safe: no `..`, no root.
pub fn inner_path(path: &str) -> anyhow::Result<PathBuf> {
    let mut out = PathBuf::new();
    for part in path.split('/').filter(|p| !p.is_empty() && *p != ".") {
        anyhow::ensure!(part != "..", "A path cannot go up");
        // Organizer's own folder at the root is never one of the files.
        anyhow::ensure!(
            !(out.as_os_str().is_empty() && part == crate::files::ORGANIZER_DIR),
            "Not in this snapshot"
        );
        out.push(part);
    }
    Ok(out)
}

/// What a folder of a user's files was in a snapshot.
pub async fn list(
    settings: Settings,
    data_dir: PathBuf,
    snapshot: String,
    user: Uuid,
    path: String,
) -> anyhow::Result<Vec<SnapshotEntry>> {
    off_thread(move || {
        let (repo, _) = open(&settings, &data_dir, false)?;
        let repo = repo.to_indexed()?;
        let snap = repo.get_snapshot_from_str(&snapshot, |_| true)?;
        let root = user_root(snap.description.as_deref(), user)?;
        let inner = inner_path(&path)?;
        let at_root = inner.components().next().is_none();
        let full = root.join(inner);
        let node = repo
            .node_from_snapshot_and_path(&snap, &full.to_string_lossy())
            .map_err(|_| anyhow::anyhow!("Not in this snapshot"))?;
        anyhow::ensure!(node.is_dir(), "Not a folder");
        let tree = repo.get_tree(&node.subtree.context("a folder without a tree")?)?;
        let mut out: Vec<SnapshotEntry> = tree
            .nodes
            .iter()
            .filter(|n| n.is_dir() || n.is_file())
            // Organizer's own folder (the trash, the versions) is not one of
            // the user's files.
            .filter(|n| !(at_root && n.name() == std::ffi::OsStr::new(crate::files::ORGANIZER_DIR)))
            .map(|n| SnapshotEntry {
                name: n.name().to_string_lossy().into_owned(),
                kind: if n.is_dir() {
                    crate::files::Kind::Folder
                } else {
                    crate::files::Kind::File
                },
                size: if n.is_dir() { 0 } else { n.meta.size },
                modified: n.meta.mtime.as_ref().map(|t| t.to_string()),
            })
            .collect();
        out.sort_by(|a, b| {
            (a.kind != crate::files::Kind::Folder, a.name.to_lowercase())
                .cmp(&(b.kind != crate::files::Kind::Folder, b.name.to_lowercase()))
        });
        Ok(out)
    })
    .await
}

/// Restore a file or folder of a user's files from a snapshot into `dest`
/// (which must not exist), and answer whether it is a folder and the
/// snapshot's time.
pub async fn restore_path(
    settings: Settings,
    data_dir: PathBuf,
    snapshot: String,
    user: Uuid,
    path: String,
    dest: PathBuf,
) -> anyhow::Result<(bool, jiff::Timestamp)> {
    off_thread(move || {
        let (repo, _) = open(&settings, &data_dir, false)?;
        let repo = repo.to_indexed()?;
        let snap = repo.get_snapshot_from_str(&snapshot, |_| true)?;
        let root = user_root(snap.description.as_deref(), user)?;
        let inner = inner_path(&path)?;
        anyhow::ensure!(
            inner.components().next().is_some(),
            "Choose a file or folder"
        );
        let full = root.join(inner);
        let node = repo
            .node_from_snapshot_and_path(&snap, &full.to_string_lossy())
            .map_err(|_| anyhow::anyhow!("Not in this snapshot"))?;
        let folder = node.is_dir();
        restore_node(&repo, &node, &dest, folder)?;
        Ok((folder, snap.time.timestamp()))
    })
    .await
}

fn restore_node(
    repo: &Repository<rustic_core::IndexedFullStatus>,
    node: &rustic_core::repofile::Node,
    dest: &Path,
    folder: bool,
) -> anyhow::Result<()> {
    let mut target = dest.to_string_lossy().into_owned();
    if folder && !target.ends_with('/') {
        target.push('/');
    }
    let destination = LocalDestination::new(&target, true, !folder)?;
    let options = RestoreOptions::default();
    let streamer = repo.ls(node, &LsOptions::default())?;
    let plan = repo.prepare_restore(&options, streamer.clone(), &destination, false)?;
    repo.restore(plan, &options, streamer, &destination)?;
    Ok(())
}

/// Restore every folder of a snapshot where it was (under `to` when
/// given), for the command line. Answers its manifest and the folders.
pub async fn off_thread_restore_all(
    settings: Settings,
    data_dir: PathBuf,
    snapshot: String,
    to: Option<PathBuf>,
) -> anyhow::Result<(Manifest, Vec<PathBuf>)> {
    off_thread(move || {
        let (repo, _) = open(&settings, &data_dir, false)?;
        let repo = repo.to_indexed()?;
        let snap = repo.get_snapshot_from_str(&snapshot, |_| true)?;
        let manifest: Manifest =
            serde_json::from_str(snap.description.as_deref().unwrap_or_default())
                .context("This snapshot was not made by Organizer")?;
        let mut restored = Vec::new();
        for path in snap.paths.iter() {
            let node = repo.node_from_snapshot_and_path(&snap, path)?;
            let dest = match &to {
                Some(to) => to.join(path.trim_start_matches('/')),
                None => PathBuf::from(path),
            };
            if let Some(parent) = dest.parent() {
                std::fs::create_dir_all(parent)?;
            }
            restore_node(&repo, &node, &dest, node.is_dir())?;
            restored.push(dest);
        }
        Ok((manifest, restored))
    })
    .await
}
