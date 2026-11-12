//! Watching a tree. On the server's disk the filesystem's events (notify)
//! start a scan soon after the changes stop, and a scan every few minutes
//! catches what sends no events (NFS and SMB shares, some container bind
//! mounts); a watcher that cannot start leaves the periodic scan alone,
//! which is enough. A bucket sends no events: its drives are listed every
//! so often, as their backend says (hourly by default; or never), to find
//! what was changed outside Organizer (docs/M7C_DESIGN.md §4).

use super::{Files, Tree};
use notify::{RecursiveMode, Watcher};
use std::sync::{Arc, Weak};
use std::time::Duration;

/// How long the events must stop before a scan: a copy of many files is
/// one scan, not one per file.
const SETTLE: Duration = Duration::from_secs(2);
/// The disk's scan that does not wait for events.
const PERIOD: Duration = Duration::from_secs(10 * 60);

/// Watch a tree and scan it on its changes and on a timer, for as long as
/// the tree is open (its watcher goes with it) and the service lives. A
/// tree in a backend has the timer only (`listing`: none, when its backend
/// lists never).
pub(super) fn spawn(files: Weak<Files>, tree: &Arc<Tree>, listing: Option<Duration>) {
    let (events, mut changed) = tokio::sync::mpsc::channel::<()>(1);
    let (watching, period) = match (tree.storage.local_root(), tree.organizer_dir()) {
        (Some(root), Some(own)) => (watch_disk(tree, root, own, events), PERIOD),
        _ => match listing {
            Some(period) => (false, period),
            None => return,
        },
    };
    let drive_id = tree.drive_id;
    let tree = Arc::downgrade(tree);
    tokio::spawn(async move {
        let mut timer = tokio::time::interval(period);
        timer.tick().await;
        loop {
            tokio::select! {
                event = changed.recv(), if watching => {
                    // The watcher is gone with its tree (a new root): this
                    // task ends, the new tree has its own.
                    if event.is_none() {
                        return;
                    }
                    tokio::time::sleep(SETTLE).await;
                    while changed.try_recv().is_ok() {}
                }
                _ = timer.tick() => {}
            }
            // The tree was replaced (a new root) or the service stopped.
            if tree.upgrade().is_none() {
                return;
            }
            let Some(files) = files.upgrade() else { return };
            match files.scan(drive_id).await {
                Ok(report) if report != Default::default() => {
                    tracing::info!("rescanned the tree of drive {drive_id}: {report}");
                }
                Ok(_) => {}
                Err(e) => tracing::warn!("cannot rescan the tree of drive {drive_id}: {e}"),
            }
        }
    });
}

/// The filesystem's events of a tree on the disk; whether it is watched.
fn watch_disk(
    tree: &Tree,
    root: &std::path::Path,
    own: std::path::PathBuf,
    events: tokio::sync::mpsc::Sender<()>,
) -> bool {
    // Organizer's own folder (the trash, the versions) changes with every
    // delete and replace: never a reason to scan.
    let watcher = notify::recommended_watcher(move |result: notify::Result<notify::Event>| {
        if let Ok(event) = result
            && (event.paths.is_empty() || event.paths.iter().any(|p| !p.starts_with(&own)))
        {
            // Full means a scan is already due: this event is in it.
            let _ = events.try_send(());
        }
    })
    .and_then(|mut watcher| {
        watcher.watch(root, RecursiveMode::Recursive)?;
        Ok(watcher)
    });
    match watcher {
        Ok(watcher) => {
            *tree.watcher.lock().unwrap() = Some(watcher);
            true
        }
        Err(e) => {
            tracing::warn!(
                "cannot watch {} ({e}): changes made outside Organizer are seen every {} minutes",
                root.display(),
                PERIOD.as_secs() / 60
            );
            false
        }
    }
}
