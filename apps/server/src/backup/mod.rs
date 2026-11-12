//! Backups of the whole server (docs/M3_DESIGN.md §3): the database, every
//! user's files folder, and the data dir's trash, versions and drop box,
//! into a rustic (restic-format) repository: deduplicated, compressed and
//! encrypted, on a local path or S3.
//!
//! The settings are a superadmin's (the `backup` document of `server_settings`), with `BACKUP_*`
//! variables on top that lock their field, as the `AI_*` ones do. A
//! scheduler queues a `backup` job when one is due; the job runs rustic
//! (`run.rs`), which is synchronous, on a thread of its own.

pub mod cli;
pub mod run;

use crate::db::Db;
use crate::jobs::{BoxFuture, Handler, Job};
use crate::state::AppState;
use serde::{Deserialize, Serialize};
use std::sync::{Arc, RwLock};
use std::time::Duration;
use utoipa::ToSchema;

/// The job's kind.
pub const JOB: &str = "backup";

/// The backup settings, as stored or as in effect.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize, sqlx::FromRow)]
pub struct Settings {
    /// A path, or `s3://endpoint/bucket/path` (the URL's host is the
    /// endpoint; `s3+http://` for plain http). Empty: no backups.
    pub url: String,
    pub region: String,
    pub access_key: String,
    pub secret_key: String,
    pub password: String,
    pub schedule: String,
    pub keep_daily: i64,
    pub keep_weekly: i64,
    pub keep_monthly: i64,
}

impl Settings {
    /// Backups are set up: a repository and its password.
    pub fn enabled(&self) -> bool {
        !self.url.is_empty() && !self.password.is_empty()
    }

    /// The repository is on S3: its endpoint, bucket and prefix.
    pub fn s3(&self) -> Option<(String, String, String)> {
        let (endpoint, bucket, prefix) = crate::config::parse_s3_url(&self.url).ok()?;
        Some((endpoint, bucket, prefix))
    }

    pub fn validate(&self) -> Result<(), String> {
        Schedule::parse(&self.schedule)?;
        for (field, value) in [
            ("keep_daily", self.keep_daily),
            ("keep_weekly", self.keep_weekly),
            ("keep_monthly", self.keep_monthly),
        ] {
            if !(0..=1_000).contains(&value) {
                return Err(format!("{field} must be between 0 and 1000"));
            }
        }
        if self.keep_daily + self.keep_weekly + self.keep_monthly == 0 {
            return Err("Keep at least one daily, weekly or monthly backup".into());
        }
        if self.url.is_empty() {
            return Ok(());
        }
        match self.s3() {
            Some(_) => {
                if self.access_key.is_empty() || self.secret_key.is_empty() {
                    return Err(
                        "An S3 backup needs BACKUP_ACCESS_KEY / BACKUP_SECRET_KEY or their settings fields"
                            .into(),
                    );
                }
            }
            None if !self.url.starts_with("file://")
                && !std::path::Path::new(self.url.trim_start_matches("file://")).is_absolute() =>
            {
                return Err(
                    "The backup URL must be an absolute path, file://…, or s3://endpoint/bucket/path"
                        .into(),
                );
            }
            None => {}
        }
        if self.password.is_empty() {
            return Err("A password is needed: it encrypts the backups".into());
        }
        Ok(())
    }
}

/// When backups run.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Schedule {
    /// Every day at this time (the server's time zone).
    Daily { hour: i8, minute: i8 },
    /// Every so many hours.
    Every { hours: i64 },
}

impl Schedule {
    /// `daily HH:MM` or `every Nh`.
    pub fn parse(text: &str) -> Result<Self, String> {
        let bad = || format!("The schedule must be `daily HH:MM` or `every Nh`, not `{text}`");
        let text = text.trim().to_lowercase();
        if let Some(time) = text.strip_prefix("daily ") {
            let (h, m) = time.trim().split_once(':').ok_or_else(bad)?;
            let (hour, minute): (i8, i8) =
                (h.parse().map_err(|_| bad())?, m.parse().map_err(|_| bad())?);
            if !(0..24).contains(&hour) || !(0..60).contains(&minute) {
                return Err(bad());
            }
            return Ok(Schedule::Daily { hour, minute });
        }
        if let Some(hours) = text
            .strip_prefix("every ")
            .and_then(|h| h.trim().strip_suffix('h'))
        {
            let hours: i64 = hours.trim().parse().map_err(|_| bad())?;
            if !(1..=24 * 30).contains(&hours) {
                return Err("Every 1 to 720 hours".into());
            }
            return Ok(Schedule::Every { hours });
        }
        Err(bad())
    }

    /// When the next backup is due, after one that started at `last`
    /// (`None`: never), seen at `now`.
    pub fn next(&self, last: Option<jiff::Timestamp>, now: &jiff::Zoned) -> jiff::Timestamp {
        match *self {
            Schedule::Every { hours } => match last {
                Some(last) => last + jiff::SignedDuration::from_hours(hours),
                None => now.timestamp(),
            },
            Schedule::Daily { hour, minute } => {
                // Today's time, or tomorrow's when it has passed since the
                // last run.
                let today = now
                    .with()
                    .hour(hour)
                    .minute(minute)
                    .second(0)
                    .subsec_nanosecond(0)
                    .build()
                    .map(|z| z.timestamp())
                    .unwrap_or(now.timestamp());
                let yesterday = today - jiff::SignedDuration::from_hours(24);
                match last {
                    // Missed (the server was off): at once.
                    Some(last) if last < yesterday => now.timestamp(),
                    None => now.timestamp(),
                    Some(last) if last < today => today,
                    Some(_) => today + jiff::SignedDuration::from_hours(24),
                }
            }
        }
    }
}

/// The `BACKUP_*` variables.
#[derive(Clone, Debug, Default)]
pub struct Env {
    pub url: Option<String>,
    pub region: Option<String>,
    pub access_key: Option<String>,
    pub secret_key: Option<String>,
    pub password: Option<String>,
    pub schedule: Option<String>,
    /// `BACKUP_KEEP`: daily, weekly, monthly.
    pub keep: Option<(i64, i64, i64)>,
}

impl Env {
    pub fn from_env() -> Result<Self, String> {
        use crate::config::var;
        let keep = match var("BACKUP_KEEP") {
            None => None,
            Some(v) => {
                let parts: Vec<i64> = v
                    .split(',')
                    .map(|p| p.trim().parse())
                    .collect::<Result<_, _>>()
                    .map_err(|_| {
                        format!("BACKUP_KEEP must be daily,weekly,monthly (7,4,12), not `{v}`")
                    })?;
                match parts[..] {
                    [d, w, m] => Some((d, w, m)),
                    _ => {
                        return Err(format!(
                            "BACKUP_KEEP must be daily,weekly,monthly (7,4,12), not `{v}`"
                        ));
                    }
                }
            }
        };
        let env = Env {
            url: var("BACKUP_URL"),
            region: var("BACKUP_REGION"),
            access_key: var("BACKUP_ACCESS_KEY"),
            secret_key: var("BACKUP_SECRET_KEY"),
            password: var("BACKUP_PASSWORD"),
            schedule: var("BACKUP_SCHEDULE"),
            keep,
        };
        if let Some(schedule) = &env.schedule {
            Schedule::parse(schedule).map_err(|e| format!("BACKUP_SCHEDULE: {e}"))?;
        }
        Ok(env)
    }

    /// These over the stored settings.
    pub fn apply(&self, stored: &Settings) -> Settings {
        let pick = |v: &Option<String>, s: &String| v.clone().unwrap_or_else(|| s.clone());
        let (keep_daily, keep_weekly, keep_monthly) =
            self.keep
                .unwrap_or((stored.keep_daily, stored.keep_weekly, stored.keep_monthly));
        Settings {
            url: pick(&self.url, &stored.url),
            region: pick(&self.region, &stored.region),
            access_key: pick(&self.access_key, &stored.access_key),
            secret_key: pick(&self.secret_key, &stored.secret_key),
            password: pick(&self.password, &stored.password),
            schedule: pick(&self.schedule, &stored.schedule),
            keep_daily,
            keep_weekly,
            keep_monthly,
        }
    }

    /// The fields a variable sets.
    pub fn locked(&self) -> Vec<&'static str> {
        let mut out: Vec<&'static str> = [
            ("url", self.url.is_some()),
            ("region", self.region.is_some()),
            ("access_key", self.access_key.is_some()),
            ("secret_key", self.secret_key.is_some()),
            ("password", self.password.is_some()),
            ("schedule", self.schedule.is_some()),
        ]
        .into_iter()
        .filter_map(|(f, set)| set.then_some(f))
        .collect();
        if self.keep.is_some() {
            out.extend(["keep_daily", "keep_weekly", "keep_monthly"]);
        }
        out
    }
}

/// The variable that sets a field.
pub fn variable(field: &str) -> String {
    match field {
        "keep_daily" | "keep_weekly" | "keep_monthly" => "BACKUP_KEEP".into(),
        field => format!("BACKUP_{}", field.to_uppercase()),
    }
}

/// The stored settings.
pub async fn stored(db: &Db) -> Result<Settings, sqlx::Error> {
    crate::server_settings::load(db.reader(), "backup").await
}

/// How the last backups went.
#[derive(Clone, Debug, Default, Serialize, Deserialize, ToSchema, sqlx::FromRow)]
pub struct BackupStatus {
    #[schema(required = true)]
    pub last_started: Option<String>,
    #[schema(required = true)]
    pub last_finished: Option<String>,
    /// Why the last run failed; absent when it did not.
    #[schema(required = true)]
    pub last_error: Option<String>,
    #[schema(required = true)]
    pub last_snapshot: Option<String>,
    /// The bytes the last snapshot added to the repository.
    #[schema(required = true)]
    pub last_added: Option<i64>,
    /// The bytes backed up by the last snapshot, in all.
    #[schema(required = true)]
    pub last_total: Option<i64>,
    /// The snapshots in the repository after the last run.
    #[schema(required = true)]
    pub snapshots: Option<i64>,
    #[schema(required = true)]
    pub last_check: Option<String>,
}

pub async fn status(db: &Db) -> Result<BackupStatus, sqlx::Error> {
    crate::server_settings::load(db.reader(), "backup_status").await
}

/// The backup settings in effect.
pub struct Backup {
    env: Env,
    settings: RwLock<Settings>,
    /// One save at a time.
    saving: tokio::sync::Mutex<()>,
}

impl Backup {
    pub async fn load(db: &Db, env: Env) -> Self {
        let stored = stored(db).await.unwrap_or_default();
        Backup {
            settings: RwLock::new(env.apply(&stored)),
            env,
            saving: tokio::sync::Mutex::new(()),
        }
    }

    pub fn settings(&self) -> Settings {
        self.settings.read().expect("not poisoned").clone()
    }

    pub fn env(&self) -> &Env {
        &self.env
    }

    pub fn replace(&self, settings: Settings) {
        *self.settings.write().expect("not poisoned") = settings;
    }

    pub async fn lock_saves(&self) -> tokio::sync::MutexGuard<'_, ()> {
        self.saving.lock().await
    }

    /// When the next backup is due; `None` without backups.
    pub async fn next_due(&self, db: &Db) -> Result<Option<jiff::Timestamp>, sqlx::Error> {
        let settings = self.settings();
        if !settings.enabled() {
            return Ok(None);
        }
        let Ok(schedule) = Schedule::parse(&settings.schedule) else {
            return Ok(None);
        };
        let last = status(db).await?.last_started.and_then(|t| t.parse().ok());
        Ok(Some(schedule.next(last, &jiff::Zoned::now())))
    }
}

/// Queue a backup unless one is queued or running already. Answers
/// whether one was queued.
pub async fn queue(state: &AppState) -> Result<bool, sqlx::Error> {
    let pending: i64 = sqlx::query_scalar(
        "SELECT count(*) FROM jobs WHERE kind = ?1 AND status IN ('queued', 'running')",
    )
    .bind(JOB)
    .fetch_one(state.db.reader())
    .await?;
    if pending > 0 {
        return Ok(false);
    }
    state.jobs.add(JOB, &(), None).await?;
    Ok(true)
}

/// Queue a backup whenever one is due, from start. Looked at every minute.
pub async fn schedule(state: AppState) {
    loop {
        match state.backup.next_due(&state.db).await {
            Ok(Some(due)) if due <= jiff::Timestamp::now() => {
                if let Err(e) = queue(&state).await {
                    tracing::warn!("cannot queue a backup: {e}");
                }
            }
            Ok(_) => {}
            Err(e) => tracing::warn!("cannot read when the next backup is due: {e}"),
        }
        tokio::time::sleep(Duration::from_secs(60)).await;
    }
}

/// The job that backs up.
pub struct BackupJob;

impl Handler for BackupJob {
    fn kind(&self) -> &'static str {
        JOB
    }

    fn run<'a>(&'a self, state: &'a AppState, _job: Job) -> BoxFuture<'a, Result<(), String>> {
        Box::pin(async move { run::backup(state).await.map_err(|e| format!("{e:#}")) })
    }

    /// The backup failed its last attempt: the superadmins are told.
    fn give_up<'a>(&'a self, state: &'a AppState, _job: Job, _error: String) -> BoxFuture<'a, ()> {
        Box::pin(async move {
            if let Err(e) = crate::push::notify_superadmins(state, crate::push::Kind::Backup).await
            {
                tracing::warn!("cannot tell the superadmins the backup failed: {e:#}");
            }
        })
    }
}

/// The handlers of the backups' jobs.
pub fn handlers() -> Vec<Arc<dyn Handler>> {
    vec![Arc::new(BackupJob)]
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn schedules_are_read() {
        assert_eq!(
            Schedule::parse("daily 03:00"),
            Ok(Schedule::Daily { hour: 3, minute: 0 })
        );
        assert_eq!(
            Schedule::parse(" Every 6h "),
            Ok(Schedule::Every { hours: 6 })
        );
        for bad in [
            "daily 25:00",
            "daily 3",
            "every 0h",
            "every six hours",
            "weekly",
        ] {
            assert!(Schedule::parse(bad).is_err(), "{bad}");
        }
    }

    #[test]
    fn the_next_backup_follows_the_schedule() {
        let now: jiff::Zoned = "2026-11-07T10:00:00+00:00[UTC]".parse().unwrap();
        let at = |s: &str| s.parse::<jiff::Timestamp>().unwrap();
        let daily = Schedule::Daily { hour: 3, minute: 0 };
        // Never ran: at once.
        assert_eq!(daily.next(None, &now), now.timestamp());
        // Ran today at 3: tomorrow at 3.
        assert_eq!(
            daily.next(Some(at("2026-11-07T03:00:01Z")), &now),
            at("2026-11-08T03:00:00Z")
        );
        // Ran yesterday at 3: today's is due (in the past: at once).
        assert_eq!(
            daily.next(Some(at("2026-11-06T03:00:00Z")), &now),
            at("2026-11-07T03:00:00Z")
        );
        // Ran long ago: at once.
        assert_eq!(
            daily.next(Some(at("2026-11-01T03:00:00Z")), &now),
            now.timestamp()
        );
        let every = Schedule::Every { hours: 6 };
        assert_eq!(
            every.next(Some(at("2026-11-07T08:00:00Z")), &now),
            at("2026-11-07T14:00:00Z")
        );
    }

    #[test]
    fn settings_are_checked() {
        let ok = Settings {
            url: "/mnt/backup".into(),
            password: "secret".into(),
            schedule: "daily 03:00".into(),
            keep_daily: 7,
            ..Settings::default()
        };
        assert_eq!(ok.validate(), Ok(()));
        let s3 = Settings {
            url: "s3://s3.example.com/bucket/organizer".into(),
            ..ok.clone()
        };
        assert!(s3.validate().unwrap_err().contains("BACKUP_ACCESS_KEY"));
        assert_eq!(
            s3.s3(),
            Some((
                "https://s3.example.com".into(),
                "bucket".into(),
                "organizer".into()
            ))
        );
        let relative = Settings {
            url: "backup".into(),
            ..ok.clone()
        };
        assert!(relative.validate().is_err());
        let nothing_kept = Settings {
            keep_daily: 0,
            ..ok
        };
        assert!(nothing_kept.validate().is_err());
    }

    #[test]
    fn variables_lock_their_fields() {
        let env = Env {
            password: Some("p".into()),
            keep: Some((1, 2, 3)),
            ..Env::default()
        };
        let settings = env.apply(&Settings::default());
        assert_eq!(settings.password, "p");
        assert_eq!((settings.keep_daily, settings.keep_monthly), (1, 3));
        assert_eq!(
            env.locked(),
            ["password", "keep_daily", "keep_weekly", "keep_monthly"]
        );
        assert_eq!(variable("keep_weekly"), "BACKUP_KEEP");
        assert_eq!(variable("secret_key"), "BACKUP_SECRET_KEY");
    }
}
