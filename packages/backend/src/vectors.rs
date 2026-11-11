//! Where the embeddings are kept and searched: a [`VectorStore`], so that
//! the engine can be swapped (pgvector, a vector database, ...) without
//! touching what uses it. The one here is [`TursoStore`]: Turso (the Rust
//! SQLite) in a file of its own, `<DATA_DIR>/vectors.db`, with its
//! `vector32` and `vector_distance_cos`; the nearest are found by scanning
//! a user's rows, which is fast enough for a personal tree.
//!
//! The vectors are derived data: they can always be made again from the
//! tree and the extracted text, so the file is not part of the backup, and
//! it is emptied when the embeddings model changes ([`VectorStore::reset`]).

use async_trait::async_trait;
use uuid::Uuid;

/// What a vector stands for.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum VectorKind {
    /// A folder: its path and a few of its file names.
    Folder,
    /// A piece of a file's text.
    Chunk,
}

impl VectorKind {
    pub fn as_str(self) -> &'static str {
        match self {
            VectorKind::Folder => "folder",
            VectorKind::Chunk => "chunk",
        }
    }
}

/// A vector to keep.
#[derive(Clone, Debug)]
pub struct Entry {
    pub user_id: Uuid,
    pub kind: VectorKind,
    /// Unique per user and kind: a folder's id, or `<file id>:<n>`.
    pub key: String,
    /// The node it belongs to.
    pub node_id: Uuid,
    /// What it was made from, to know when it is out of date.
    pub fingerprint: String,
    pub vector: Vec<f32>,
}

/// One that is near.
#[derive(Clone, Debug, PartialEq)]
pub struct Near {
    pub key: String,
    pub node_id: Uuid,
    /// Cosine distance: 0 is the same direction, 2 the opposite.
    pub distance: f32,
}

/// The vectors' engine.
#[async_trait]
pub trait VectorStore: Send + Sync {
    /// The embeddings model the vectors were made with (`None` when there
    /// are none yet). A store whose model is not the one in use is reset.
    async fn model(&self) -> anyhow::Result<Option<String>>;

    /// Remove every vector, and remember the model the next ones are made
    /// with.
    async fn reset(&self, model: &str) -> anyhow::Result<()>;

    /// Keep these vectors, replacing those with the same key.
    async fn put(&self, entries: &[Entry]) -> anyhow::Result<()>;

    /// The keys a user has of a kind, with their fingerprints.
    async fn fingerprints(
        &self,
        user_id: Uuid,
        kind: VectorKind,
    ) -> anyhow::Result<Vec<(String, String)>>;

    /// Remove some keys of a user.
    async fn remove(&self, user_id: Uuid, kind: VectorKind, keys: &[String]) -> anyhow::Result<()>;

    /// The `k` vectors of a user and kind nearest to `query`, the nearest
    /// first.
    async fn nearest(
        &self,
        user_id: Uuid,
        kind: VectorKind,
        query: &[f32],
        k: usize,
    ) -> anyhow::Result<Vec<Near>>;
}

/// The store a `VECTOR_DATABASE_URL` names: `turso:<path>` (`turso::memory:`
/// for the tests). Another engine (pgvector, ...) is another scheme here.
pub async fn open(url: &str) -> anyhow::Result<std::sync::Arc<dyn VectorStore>> {
    match url.split_once(':') {
        Some(("turso", path)) if !path.is_empty() => {
            Ok(std::sync::Arc::new(TursoStore::open(path).await?))
        }
        Some(("postgres" | "postgresql", _)) => anyhow::bail!(
            "VECTOR_DATABASE_URL: PostgreSQL (pgvector) is not supported yet; use turso:<path>"
        ),
        _ => anyhow::bail!("VECTOR_DATABASE_URL must be turso:<path>, not `{url}`"),
    }
}

/// Turso as the vector store.
pub struct TursoStore {
    // One connection, used in turn: the store's writes are small and its
    // reads quick.
    connection: tokio::sync::Mutex<turso::Connection>,
    _database: turso::Database,
}

impl TursoStore {
    /// The store in a file (`:memory:` for the tests), its tables made if
    /// they are not there.
    pub async fn open(path: &str) -> anyhow::Result<Self> {
        if path != ":memory:"
            && let Some(parent) = std::path::Path::new(path)
                .parent()
                .filter(|p| !p.as_os_str().is_empty())
        {
            std::fs::create_dir_all(parent)?;
        }
        let database = turso::Builder::new_local(path).build().await?;
        let connection = database.connect()?;
        connection
            .execute_batch(
                "CREATE TABLE IF NOT EXISTS vectors (
                   user_id TEXT NOT NULL,
                   kind TEXT NOT NULL,
                   key TEXT NOT NULL,
                   node_id TEXT NOT NULL,
                   fingerprint TEXT NOT NULL,
                   embedding BLOB NOT NULL,
                   PRIMARY KEY (user_id, kind, key)
                 );
                 CREATE TABLE IF NOT EXISTS meta (key TEXT PRIMARY KEY, value TEXT NOT NULL);",
            )
            .await?;
        Ok(TursoStore {
            connection: tokio::sync::Mutex::new(connection),
            _database: database,
        })
    }
}

/// A vector as `vector32` reads it: `[0.1,0.2,...]`.
fn literal(vector: &[f32]) -> String {
    let parts: Vec<String> = vector.iter().map(|v| format!("{v}")).collect();
    format!("[{}]", parts.join(","))
}

#[async_trait]
impl VectorStore for TursoStore {
    async fn model(&self) -> anyhow::Result<Option<String>> {
        let connection = self.connection.lock().await;
        let mut rows = connection
            .query("SELECT value FROM meta WHERE key = 'model'", ())
            .await?;
        Ok(match rows.next().await? {
            Some(row) => Some(row.get::<String>(0)?),
            None => None,
        })
    }

    async fn reset(&self, model: &str) -> anyhow::Result<()> {
        let connection = self.connection.lock().await;
        connection.execute("DELETE FROM vectors", ()).await?;
        connection
            .execute(
                "INSERT INTO meta (key, value) VALUES ('model', ?1)
                 ON CONFLICT (key) DO UPDATE SET value = ?1",
                [model.to_string()],
            )
            .await?;
        Ok(())
    }

    async fn put(&self, entries: &[Entry]) -> anyhow::Result<()> {
        let connection = self.connection.lock().await;
        for entry in entries {
            connection
                .execute(
                    "INSERT INTO vectors (user_id, kind, key, node_id, fingerprint, embedding)
                     VALUES (?1, ?2, ?3, ?4, ?5, vector32(?6))
                     ON CONFLICT (user_id, kind, key) DO UPDATE SET
                       node_id = ?4, fingerprint = ?5, embedding = vector32(?6)",
                    [
                        entry.user_id.to_string(),
                        entry.kind.as_str().to_string(),
                        entry.key.clone(),
                        entry.node_id.to_string(),
                        entry.fingerprint.clone(),
                        literal(&entry.vector),
                    ],
                )
                .await?;
        }
        Ok(())
    }

    async fn fingerprints(
        &self,
        user_id: Uuid,
        kind: VectorKind,
    ) -> anyhow::Result<Vec<(String, String)>> {
        let connection = self.connection.lock().await;
        let mut rows = connection
            .query(
                "SELECT key, fingerprint FROM vectors WHERE user_id = ?1 AND kind = ?2",
                [user_id.to_string(), kind.as_str().to_string()],
            )
            .await?;
        let mut out = Vec::new();
        while let Some(row) = rows.next().await? {
            out.push((row.get::<String>(0)?, row.get::<String>(1)?));
        }
        Ok(out)
    }

    async fn remove(&self, user_id: Uuid, kind: VectorKind, keys: &[String]) -> anyhow::Result<()> {
        let connection = self.connection.lock().await;
        for key in keys {
            connection
                .execute(
                    "DELETE FROM vectors WHERE user_id = ?1 AND kind = ?2 AND key = ?3",
                    [user_id.to_string(), kind.as_str().to_string(), key.clone()],
                )
                .await?;
        }
        Ok(())
    }

    async fn nearest(
        &self,
        user_id: Uuid,
        kind: VectorKind,
        query: &[f32],
        k: usize,
    ) -> anyhow::Result<Vec<Near>> {
        let connection = self.connection.lock().await;
        let mut rows = connection
            .query(
                "SELECT key, node_id, vector_distance_cos(embedding, vector32(?3)) AS distance
                 FROM vectors WHERE user_id = ?1 AND kind = ?2
                 ORDER BY distance LIMIT ?4",
                [
                    user_id.to_string(),
                    kind.as_str().to_string(),
                    literal(query),
                    k.to_string(),
                ],
            )
            .await?;
        let mut out = Vec::new();
        while let Some(row) = rows.next().await? {
            out.push(Near {
                key: row.get::<String>(0)?,
                node_id: row.get::<String>(1)?.parse()?,
                distance: row.get::<f64>(2)? as f32,
            });
        }
        Ok(out)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn entry(user: Uuid, key: &str, vector: Vec<f32>) -> Entry {
        Entry {
            user_id: user,
            kind: VectorKind::Folder,
            key: key.into(),
            node_id: Uuid::now_v7(),
            fingerprint: format!("fp-{key}"),
            vector,
        }
    }

    #[tokio::test]
    async fn the_url_names_the_engine() {
        assert!(open("turso::memory:").await.is_ok());
        let err = |r: anyhow::Result<std::sync::Arc<dyn VectorStore>>| match r {
            Err(e) => e.to_string(),
            Ok(_) => panic!("opened"),
        };
        assert!(err(open("postgres://db/x").await).contains("not supported yet"));
        assert!(err(open("vectors.db").await).contains("must be turso:"));
    }

    #[tokio::test]
    async fn the_nearest_come_first_and_the_store_follows_the_model() {
        let store = TursoStore::open(":memory:").await.unwrap();
        assert_eq!(store.model().await.unwrap(), None);
        store.reset("nomic").await.unwrap();
        assert_eq!(store.model().await.unwrap().as_deref(), Some("nomic"));
        let (me, other) = (Uuid::now_v7(), Uuid::now_v7());
        store
            .put(&[
                entry(me, "bills", vec![1.0, 0.0, 0.0]),
                entry(me, "health", vec![0.0, 1.0, 0.0]),
                entry(me, "both", vec![0.7, 0.7, 0.0]),
                entry(other, "theirs", vec![1.0, 0.0, 0.0]),
            ])
            .await
            .unwrap();
        let near = store
            .nearest(me, VectorKind::Folder, &[0.9, 0.1, 0.0], 2)
            .await
            .unwrap();
        let keys: Vec<&str> = near.iter().map(|n| n.key.as_str()).collect();
        assert_eq!(keys, ["bills", "both"]);
        assert!(near[0].distance < 0.02);

        // Replaced by key; removed by key; gone with a new model.
        store
            .put(&[entry(me, "bills", vec![0.0, 0.0, 1.0])])
            .await
            .unwrap();
        let near = store
            .nearest(me, VectorKind::Folder, &[0.9, 0.1, 0.0], 1)
            .await
            .unwrap();
        assert_eq!(near[0].key, "both");
        store
            .remove(me, VectorKind::Folder, &["both".into()])
            .await
            .unwrap();
        let mut keys: Vec<String> = store
            .fingerprints(me, VectorKind::Folder)
            .await
            .unwrap()
            .into_iter()
            .map(|(k, _)| k)
            .collect();
        keys.sort();
        assert_eq!(keys, ["bills", "health"]);
        assert!(
            store
                .fingerprints(me, VectorKind::Chunk)
                .await
                .unwrap()
                .is_empty()
        );
        store.reset("other").await.unwrap();
        assert!(
            store
                .fingerprints(me, VectorKind::Folder)
                .await
                .unwrap()
                .is_empty()
        );
    }
}
