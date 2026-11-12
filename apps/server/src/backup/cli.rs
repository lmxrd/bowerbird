//! `organizer backup …`: the backups from the command line, for the superadmin,
//! with the server stopped. The settings are the stored ones under the
//! `BACKUP_*` variables when the database is there, the variables alone
//! when it is not (the server was lost: `BACKUP_URL` and
//! `BACKUP_PASSWORD` are then needed).
//!
//! ```text
//! organizer backup snapshots
//! organizer backup restore [--snapshot <id>] [--to <dir>] [--force]
//! ```
//!
//! A restore puts every folder of the snapshot back where it was (under
//! `--to` when given, to look first), then the database copy at
//! `DATABASE_URL`. It refuses to replace a database that is there unless
//! `--force`. The server rescans the folders when it starts.

use super::run::DATABASE_COPY;
use super::{Env, Settings};
use anyhow::Context;
use std::path::{Path, PathBuf};

const USAGE: &str = "usage: organizer backup snapshots
       organizer backup restore [--snapshot <id>] [--to <dir>] [--force]";

/// Run `organizer backup <args>`.
pub async fn main(args: &[String]) -> anyhow::Result<()> {
    let data_dir = crate::config::data_dir();
    let db_path =
        PathBuf::from(crate::config::database_path(&data_dir).map_err(anyhow::Error::msg)?);
    let settings = settings(&db_path).await?;
    anyhow::ensure!(
        settings.enabled(),
        "Backups are not set up: set BACKUP_URL and BACKUP_PASSWORD (and BACKUP_ACCESS_KEY / BACKUP_SECRET_KEY for s3://)"
    );
    match args.first().map(String::as_str) {
        Some("snapshots") => {
            for snapshot in super::run::snapshots(settings, data_dir).await? {
                println!("{}  {}", snapshot.id, snapshot.time);
            }
            Ok(())
        }
        Some("restore") => {
            let mut snapshot = "latest".to_string();
            let mut to: Option<PathBuf> = None;
            let mut force = false;
            let mut rest = args[1..].iter();
            while let Some(arg) = rest.next() {
                match arg.as_str() {
                    "--snapshot" => snapshot = rest.next().context(USAGE)?.clone(),
                    "--to" => to = Some(rest.next().context(USAGE)?.into()),
                    "--force" => force = true,
                    _ => anyhow::bail!("{USAGE}"),
                }
            }
            restore(settings, data_dir, &db_path, &snapshot, to, force).await
        }
        _ => anyhow::bail!("{USAGE}"),
    }
}

/// The stored settings under the variables, or the variables alone.
async fn settings(db_path: &Path) -> anyhow::Result<Settings> {
    let env = Env::from_env().map_err(anyhow::Error::msg)?;
    if !db_path.exists() {
        return Ok(env.apply(&Settings {
            schedule: "daily 03:00".into(),
            ..Settings::default()
        }));
    }
    let db = crate::db::open(&db_path.to_string_lossy()).await?;
    let stored = super::stored(&db).await.unwrap_or_default();
    db.close().await;
    Ok(env.apply(&stored))
}

async fn restore(
    settings: Settings,
    data_dir: PathBuf,
    db_path: &Path,
    snapshot: &str,
    to: Option<PathBuf>,
    force: bool,
) -> anyhow::Result<()> {
    let database_target = match &to {
        Some(to) => to.join(DATABASE_COPY),
        None => db_path.to_path_buf(),
    };
    anyhow::ensure!(
        force || !database_target.exists(),
        "{} is there: stop the server and use --force to replace it",
        database_target.display()
    );
    let snapshot = snapshot.to_string();
    let target = to.clone();
    let (manifest, paths) =
        super::run::off_thread_restore_all(settings, data_dir, snapshot, target).await?;
    let copy = match &to {
        Some(to) => to.join(
            manifest
                .database
                .strip_prefix("/")
                .unwrap_or(&manifest.database),
        ),
        None => manifest.database.clone(),
    };
    for path in &paths {
        println!("restored {}", path.display());
    }
    if let Some(parent) = database_target.parent() {
        std::fs::create_dir_all(parent)?;
    }
    // The database's own WAL files would not match the copy.
    for suffix in ["-wal", "-shm"] {
        let _ = std::fs::remove_file(format!("{}{suffix}", database_target.display()));
    }
    std::fs::copy(&copy, &database_target)
        .with_context(|| format!("cannot put the database at {}", database_target.display()))?;
    let _ = std::fs::remove_file(&copy);
    println!("restored the database to {}", database_target.display());
    Ok(())
}
