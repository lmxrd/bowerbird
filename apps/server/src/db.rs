use sqlx::SqlitePool;
use sqlx::sqlite::{SqliteConnectOptions, SqliteJournalMode, SqlitePoolOptions, SqliteSynchronous};
use std::str::FromStr;
use std::sync::Arc;
use std::time::Duration;

/// How many read-only connections serve the reads.
const READERS: u32 = 4;

/// The database: a pool of one writer connection and a pool of read-only
/// ones. SQLite has a single writer, so the writer pool has a single
/// connection: writes queue up for it in order (instead of failing with
/// "database is locked"), and a transaction holds it until it ends. In WAL
/// mode readers never block the writer, nor each other.
///
/// Never run a query on the writer pool while holding a transaction (or a
/// connection) from it: with one connection that waits for itself. Inside
/// a transaction, use the transaction.
pub struct Database {
    writer: SqlitePool,
    reader: SqlitePool,
}

pub type Db = Arc<Database>;

impl Database {
    /// For everything that changes data, and for reads that must be part of
    /// such a change (inside its transaction).
    pub fn writer(&self) -> &SqlitePool {
        &self.writer
    }

    /// For reads. An in-memory database (the tests) is private to its one
    /// connection, so there it is the writer pool.
    pub fn reader(&self) -> &SqlitePool {
        &self.reader
    }

    /// Close the connections, which checkpoints the WAL: call on a clean
    /// stop, after the last write.
    pub async fn close(&self) {
        self.reader.close().await;
        self.writer.close().await;
    }
}

/// Open the database (`:memory:` for the tests) and bring its schema up to
/// date: the files in `migrations/`, applied in order and recorded in
/// `_sqlx_migrations`.
pub async fn open(path: &str) -> anyhow::Result<Db> {
    use anyhow::Context;

    let memory = path == ":memory:";
    // The database's directory may not exist yet (a fresh clone, a new
    // volume): create it.
    if !memory
        && let Some(parent) = std::path::Path::new(path)
            .parent()
            .filter(|p| !p.as_os_str().is_empty())
    {
        std::fs::create_dir_all(parent).with_context(|| format!("cannot create {parent:?}"))?;
    }

    let options = if memory {
        SqliteConnectOptions::from_str("sqlite::memory:")?
    } else {
        SqliteConnectOptions::new()
            .filename(path)
            .create_if_missing(true)
            .journal_mode(SqliteJournalMode::Wal)
    }
    // NORMAL is safe in WAL mode: a crash of the process loses nothing, and a
    // power cut can lose the last transactions but never corrupts the file.
    .synchronous(SqliteSynchronous::Normal)
    .foreign_keys(true)
    .busy_timeout(Duration::from_secs(5));

    // The connections live as long as the pool: an in-memory database
    // disappears with its connection.
    let pool = |options: SqliteConnectOptions, connections: u32| {
        SqlitePoolOptions::new()
            .max_connections(connections)
            .min_connections(1)
            .idle_timeout(None)
            .max_lifetime(None)
            .connect_lazy_with(options)
    };
    let writer = pool(options.clone(), 1);
    sqlx::migrate!("./migrations")
        .run(&writer)
        .await
        .with_context(|| format!("cannot migrate the database {path}"))?;

    // The readers open after the schema exists, so they see a finished
    // database in WAL mode.
    let reader = if memory {
        writer.clone()
    } else {
        pool(options.create_if_missing(false).read_only(true), READERS)
    };
    Ok(Arc::new(Database { writer, reader }))
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The tests run on an in-memory database, where the reader and the
    /// writer are one connection; this one uses a real file, as production.
    #[tokio::test]
    async fn readers_see_the_writes_and_do_not_block_the_writer() {
        let dir = std::env::temp_dir().join(format!("organizer-db-test-{}", std::process::id()));
        let path = dir.join("test.db");
        let db = open(path.to_str().unwrap()).await.unwrap();

        // A transaction holds the only writer connection ...
        let mut tx = db.writer().begin().await.unwrap();
        sqlx::query("CREATE TABLE t (v TEXT NOT NULL)")
            .execute(&mut *tx)
            .await
            .unwrap();
        sqlx::query("INSERT INTO t (v) VALUES ('a')")
            .execute(&mut *tx)
            .await
            .unwrap();
        tx.commit().await.unwrap();
        let held = db.writer().acquire().await.unwrap();

        // ... and a read goes through anyway, seeing the committed write.
        let v: String = tokio::time::timeout(
            Duration::from_secs(2),
            sqlx::query_scalar("SELECT v FROM t").fetch_one(db.reader()),
        )
        .await
        .expect("a reader waited for the writer")
        .unwrap();
        assert_eq!(v, "a");
        // And it really is read-only.
        assert!(
            sqlx::query("DELETE FROM t")
                .execute(db.reader())
                .await
                .is_err()
        );

        drop(held);
        db.close().await;
        let _ = std::fs::remove_dir_all(dir);
    }
}
