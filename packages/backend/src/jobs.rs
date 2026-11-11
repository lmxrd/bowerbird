//! The background jobs: a small queue in SQLite (the `jobs` table), so that
//! a job survives a restart. Each kind of job has a handler and a worker,
//! which runs up to the handler's `parallel` jobs at once; the calls to the
//! model are capped further by `AI_CONCURRENCY` (`ai.rs`).
//!
//! A job that fails is tried again after a growing delay (30 seconds, then
//! 2 minutes), three times in all; after the third failure it stays in the
//! table as `failed` with its error, and its handler's `give_up` is told
//! (the drop box marks its item failed, to be retried by hand).

use crate::auth::iso;
use crate::db::Db;
use crate::state::AppState;
use serde::Serialize;
use serde::de::DeserializeOwned;
use std::collections::HashMap;
use std::future::Future;
use std::pin::Pin;
use std::sync::Arc;
use std::time::{Duration, SystemTime};
use tokio::sync::{Notify, Semaphore};

/// How many times a job is run before it is given up.
pub const ATTEMPTS: i64 = 3;
/// The longest a worker sleeps before it looks at the queue again, in case
/// a job was added without telling it (by another process, say).
const IDLE: Duration = Duration::from_secs(60);

pub type BoxFuture<'a, T> = Pin<Box<dyn Future<Output = T> + Send + 'a>>;

/// A job taken from the queue.
#[derive(Debug, Clone, sqlx::FromRow)]
pub struct Job {
    pub id: i64,
    pub kind: String,
    pub payload: String,
    /// The runs before this one.
    pub attempts: i64,
}

impl Job {
    /// The payload, as the type it was queued with.
    pub fn payload<T: DeserializeOwned>(&self) -> Result<T, String> {
        serde_json::from_str(&self.payload).map_err(|e| format!("a payload not understood: {e}"))
    }
}

/// What runs the jobs of one kind.
pub trait Handler: Send + Sync + 'static {
    /// The kind of job, as stored (`analyse`, `extract`, ...).
    fn kind(&self) -> &'static str;

    /// How many of its jobs may run at once.
    fn parallel(&self) -> usize {
        1
    }

    /// Run one job: an error is the reason it failed, and it is tried
    /// again later (unless that was its last attempt).
    fn run<'a>(&'a self, state: &'a AppState, job: Job) -> BoxFuture<'a, Result<(), String>>;

    /// Called once the job failed its last attempt.
    fn give_up<'a>(&'a self, _state: &'a AppState, _job: Job, _error: String) -> BoxFuture<'a, ()> {
        Box::pin(async {})
    }
}

/// The delay before a failed job's next attempt: 30 seconds after the first
/// failure, 2 minutes after the second.
pub fn retry_delay(attempts: i64) -> Duration {
    Duration::from_secs(30 * 4u64.pow(attempts.clamp(1, 4) as u32 - 1))
}

/// The queue and its workers.
pub struct Jobs {
    db: Db,
    /// A worker's wake-up call, by kind: a job was added.
    wake: HashMap<&'static str, Arc<Notify>>,
    handlers: Vec<Arc<dyn Handler>>,
}

impl Jobs {
    /// The queue for these handlers. Nothing runs before [`Jobs::start`].
    pub fn new(db: Db, handlers: Vec<Arc<dyn Handler>>) -> Arc<Self> {
        let wake = handlers
            .iter()
            .map(|h| (h.kind(), Arc::new(Notify::new())))
            .collect();
        Arc::new(Jobs { db, wake, handlers })
    }

    /// Put a job in the queue, to run once `after` has passed (`None`: at
    /// once), and wake its worker.
    pub async fn add<T: Serialize>(
        &self,
        kind: &str,
        payload: &T,
        after: Option<Duration>,
    ) -> Result<i64, sqlx::Error> {
        let payload = serde_json::to_string(payload).expect("a payload that serializes");
        let run_after = iso(SystemTime::now() + after.unwrap_or_default());
        let (id,): (i64,) = sqlx::query_as(
            "INSERT INTO jobs (kind, payload, run_after) VALUES (?1, ?2, ?3) RETURNING id",
        )
        .bind(kind)
        .bind(payload)
        .bind(run_after)
        .fetch_one(self.db.writer())
        .await?;
        if let Some(wake) = self.wake.get(kind) {
            wake.notify_one();
        }
        Ok(id)
    }

    /// Start a worker per kind. The jobs that were running when the server
    /// stopped are queued again first: their run did not finish.
    pub async fn start(self: &Arc<Self>, state: AppState) -> Result<(), sqlx::Error> {
        sqlx::query("UPDATE jobs SET status = 'queued' WHERE status = 'running'")
            .execute(self.db.writer())
            .await?;
        for handler in &self.handlers {
            tokio::spawn(self.clone().work(state.clone(), handler.clone()));
        }
        Ok(())
    }

    /// Run the queue of one kind until the process ends.
    async fn work(self: Arc<Self>, state: AppState, handler: Arc<dyn Handler>) {
        let kind = handler.kind();
        let wake = self.wake[kind].clone();
        let slots = Arc::new(Semaphore::new(handler.parallel().max(1)));
        loop {
            let slot = slots
                .clone()
                .acquire_owned()
                .await
                .expect("the semaphore is never closed");
            match self.claim(kind).await {
                Ok(Some(job)) => {
                    let jobs = self.clone();
                    let handler = handler.clone();
                    let state = state.clone();
                    tokio::spawn(async move {
                        jobs.run(&state, handler, job).await;
                        drop(slot);
                    });
                }
                Ok(None) => {
                    drop(slot);
                    let sleep = self.next_due(kind).await.unwrap_or(IDLE).min(IDLE);
                    tokio::select! {
                        _ = wake.notified() => {}
                        _ = tokio::time::sleep(sleep) => {}
                    }
                }
                Err(e) => {
                    drop(slot);
                    tracing::warn!("cannot read the {kind} jobs: {e}");
                    tokio::time::sleep(Duration::from_secs(5)).await;
                }
            }
        }
    }

    /// Take the oldest job of a kind that is due, marking it running.
    pub async fn claim(&self, kind: &str) -> Result<Option<Job>, sqlx::Error> {
        sqlx::query_as(
            "UPDATE jobs SET status = 'running'
             WHERE id = (SELECT id FROM jobs WHERE kind = ?1 AND status = 'queued' AND run_after <= ?2
                         ORDER BY run_after, id LIMIT 1)
             RETURNING id, kind, payload, attempts",
        )
        .bind(kind)
        .bind(iso(SystemTime::now()))
        .fetch_optional(self.db.writer())
        .await
    }

    /// How long until the next queued job of a kind is due; `None` when
    /// there is none.
    async fn next_due(&self, kind: &str) -> Option<Duration> {
        let next: Option<(String,)> = sqlx::query_as(
            "SELECT run_after FROM jobs WHERE kind = ?1 AND status = 'queued'
             ORDER BY run_after LIMIT 1",
        )
        .bind(kind)
        .fetch_optional(self.db.reader())
        .await
        .ok()?;
        let due: jiff::Timestamp = next?.0.parse().ok()?;
        let now = jiff::Timestamp::now();
        Some(Duration::try_from(due.duration_since(now)).unwrap_or(Duration::ZERO))
    }

    /// Run a claimed job and record what came of it.
    pub async fn run(&self, state: &AppState, handler: Arc<dyn Handler>, job: Job) {
        let id = job.id;
        let kind = job.kind.clone();
        // On a task of its own: a handler that panics fails its job like
        // an error does.
        let result = {
            let (handler, job, state) = (handler.clone(), job.clone(), state.clone());
            match tokio::spawn(async move { handler.run(&state, job).await }).await {
                Ok(result) => result,
                Err(e) => Err(format!("the job panicked: {e}")),
            }
        };
        let recorded = match result {
            Ok(()) => sqlx::query("DELETE FROM jobs WHERE id = ?1")
                .bind(id)
                .execute(self.db.writer())
                .await
                .map(|_| ()),
            Err(error) => {
                let attempts = job.attempts + 1;
                if attempts < ATTEMPTS {
                    tracing::info!("{kind} job {id} failed (attempt {attempts}): {error}");
                    let run_after = iso(SystemTime::now() + retry_delay(attempts));
                    sqlx::query(
                        "UPDATE jobs SET status = 'queued', attempts = ?2, run_after = ?3,
                           last_error = ?4
                         WHERE id = ?1",
                    )
                    .bind(id)
                    .bind(attempts)
                    .bind(run_after)
                    .bind(&error)
                    .execute(self.db.writer())
                    .await
                    .map(|_| ())
                } else {
                    tracing::warn!("{kind} job {id} failed {attempts} times, given up: {error}");
                    let stored = sqlx::query(
                        "UPDATE jobs SET status = 'failed', attempts = ?2, last_error = ?3
                         WHERE id = ?1",
                    )
                    .bind(id)
                    .bind(attempts)
                    .bind(&error)
                    .execute(self.db.writer())
                    .await
                    .map(|_| ());
                    handler.give_up(state, Job { attempts, ..job }, error).await;
                    stored
                }
            }
        };
        if let Err(e) = recorded {
            tracing::warn!("cannot record the end of {kind} job {id}: {e}");
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Mutex;

    /// Fails its first `failures` runs, then succeeds; keeps what it saw.
    struct Flaky {
        failures: i64,
        runs: Mutex<Vec<i64>>,
        given_up: Mutex<Vec<(i64, String)>>,
        done: Notify,
    }

    impl Handler for Flaky {
        fn kind(&self) -> &'static str {
            "flaky"
        }

        fn run<'a>(&'a self, _: &'a AppState, job: Job) -> BoxFuture<'a, Result<(), String>> {
            Box::pin(async move {
                let n: i64 = job.payload()?;
                self.runs.lock().unwrap().push(n);
                if job.attempts < self.failures {
                    return Err(format!("failure {}", job.attempts + 1));
                }
                self.done.notify_one();
                Ok(())
            })
        }

        fn give_up<'a>(&'a self, _: &'a AppState, job: Job, error: String) -> BoxFuture<'a, ()> {
            Box::pin(async move {
                self.given_up.lock().unwrap().push((job.attempts, error));
            })
        }
    }

    fn flaky(failures: i64) -> Arc<Flaky> {
        Arc::new(Flaky {
            failures,
            runs: Mutex::default(),
            given_up: Mutex::default(),
            done: Notify::new(),
        })
    }

    async fn row(db: &Db, id: i64) -> Option<(String, i64, Option<String>)> {
        sqlx::query_as("SELECT status, attempts, last_error FROM jobs WHERE id = ?1")
            .bind(id)
            .fetch_optional(db.writer())
            .await
            .unwrap()
    }

    /// Make a queued job due now.
    async fn due(db: &Db, id: i64) {
        sqlx::query("UPDATE jobs SET run_after = ?2 WHERE id = ?1")
            .bind(id)
            .bind(iso(SystemTime::now()))
            .execute(db.writer())
            .await
            .unwrap();
    }

    #[test]
    fn retries_wait_longer_each_time() {
        assert_eq!(retry_delay(1), Duration::from_secs(30));
        assert_eq!(retry_delay(2), Duration::from_secs(120));
    }

    #[tokio::test]
    async fn a_failing_job_is_retried_then_given_up() {
        let db = crate::db::open(":memory:").await.unwrap();
        let state = AppState::new(db.clone(), std::env::temp_dir()).await;
        let handler = flaky(10);
        let jobs = Jobs::new(db.clone(), vec![handler.clone()]);
        let id = jobs.add("flaky", &7, None).await.unwrap();

        for attempt in 1..ATTEMPTS {
            let job = jobs.claim("flaky").await.unwrap().expect("a due job");
            assert_eq!(row(&db, id).await.unwrap().0, "running");
            jobs.run(&state, handler.clone(), job).await;
            let (status, attempts, error) = row(&db, id).await.unwrap();
            assert_eq!((status.as_str(), attempts), ("queued", attempt));
            assert_eq!(error.unwrap(), format!("failure {attempt}"));
            // Not due again before its delay.
            assert!(jobs.claim("flaky").await.unwrap().is_none());
            due(&db, id).await;
        }
        let job = jobs.claim("flaky").await.unwrap().unwrap();
        jobs.run(&state, handler.clone(), job).await;
        let (status, attempts, _) = row(&db, id).await.unwrap();
        assert_eq!((status.as_str(), attempts), ("failed", ATTEMPTS));
        assert_eq!(
            *handler.given_up.lock().unwrap(),
            [(ATTEMPTS, "failure 3".to_string())]
        );
        assert_eq!(*handler.runs.lock().unwrap(), [7, 7, 7]);
        assert!(jobs.claim("flaky").await.unwrap().is_none());
    }

    #[tokio::test]
    async fn a_job_that_succeeds_leaves_the_queue() {
        let db = crate::db::open(":memory:").await.unwrap();
        let state = AppState::new(db.clone(), std::env::temp_dir()).await;
        let handler = flaky(1);
        let jobs = Jobs::new(db.clone(), vec![handler.clone()]);
        let id = jobs.add("flaky", &1, None).await.unwrap();
        let job = jobs.claim("flaky").await.unwrap().unwrap();
        jobs.run(&state, handler.clone(), job).await;
        due(&db, id).await;
        let job = jobs.claim("flaky").await.unwrap().unwrap();
        jobs.run(&state, handler.clone(), job).await;
        assert_eq!(row(&db, id).await, None);
        assert!(handler.given_up.lock().unwrap().is_empty());
    }

    #[tokio::test]
    async fn the_workers_run_the_queue_and_pick_up_what_was_running() {
        let db = crate::db::open(":memory:").await.unwrap();
        let state = AppState::new(db.clone(), std::env::temp_dir()).await;
        let handler = flaky(0);
        let jobs = Jobs::new(db.clone(), vec![handler.clone()]);
        // A job that was running when the server stopped, and one for later.
        let stopped = jobs.add("flaky", &1, None).await.unwrap();
        jobs.claim("flaky").await.unwrap().unwrap();
        let later = jobs
            .add("flaky", &2, Some(Duration::from_secs(3600)))
            .await
            .unwrap();
        jobs.start(state).await.unwrap();
        tokio::time::timeout(Duration::from_secs(5), handler.done.notified())
            .await
            .expect("the stopped job runs again");
        // A new job wakes the worker at once.
        jobs.add("flaky", &3, None).await.unwrap();
        tokio::time::timeout(Duration::from_secs(5), handler.done.notified())
            .await
            .expect("the new job runs");
        assert_eq!(*handler.runs.lock().unwrap(), [1, 3]);
        assert_eq!(row(&db, stopped).await, None);
        assert_eq!(row(&db, later).await.unwrap().0, "queued");
    }
}
