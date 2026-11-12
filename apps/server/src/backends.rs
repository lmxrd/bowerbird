//! Storage backends (docs/M7C_DESIGN.md §1–§3): where drives are kept.
//! The server's disk is built in (no row; [`DISK`] in the API); the rows of
//! `backends` are S3-compatible storage, global (the superadmin's) or a
//! user's own. This holds what the drives' trees need of them: the
//! operator of each, its health, and the secrets.
//!
//! **Secrets.** A backend's secret is encrypted with the server key
//! (AES-256-GCM): `<DATA_DIR>/secret.key`, 32 random bytes made at the
//! first start, or the file `SECRET_KEY_FILE` names. It is never sent back
//! by the API. With **private credentials** (a user's own backend, opt-in)
//! the server keeps only the secret sealed to the user's encryption key by
//! their device, which it cannot open: a session that unlocks it hands the
//! secret over (`PUT /backends/{id}/unlock`), kept in memory for as long as
//! that session lives. Without one, the backend's drives are locked.
//!
//! **Addresses.** A user's own backend must be at a public address over
//! https unless `BACKENDS_ALLOW_PRIVATE` (as the links' `LINKS_ALLOW_PRIVATE`),
//! so that nobody points the server at its own network: the endpoint is
//! checked when it is set, and every request goes through our HTTP client
//! that resolves names to public addresses only and follows no redirect
//! ([`Guarded`]), so a name that changes its address later gains nothing.
//! S3's own ways of finding credentials (the environment, files, the cloud's
//! metadata service) are off for every backend: only the stored keys count.

use crate::db::Db;
use crate::files::FileError;
use crate::storage::{Health, Storage};
use aes_gcm::aead::{Aead, KeyInit, Payload};
use aes_gcm::{Aes256Gcm, Nonce};
use opendal::Operator;
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, OnceLock};
use std::time::Duration;
use utoipa::ToSchema;
use uuid::Uuid;

/// The server's disk in the API: the built-in backend has no row.
pub const DISK: Uuid = Uuid::nil();

/// The associated data of a sealed secret: a blob of another kind never
/// opens as one.
const SECRET_AD: &[u8] = b"organizer/backend-secret/1";

/// How long a request to a backend may take.
const TIMEOUT: Duration = Duration::from_secs(60);

/// The kind of a backend.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "lowercase")]
pub enum BackendKind {
    /// The server's own disk (built in).
    Disk,
    /// S3-compatible storage.
    S3,
}

/// Why a drive's storage cannot be used now, for a client to act on
/// (the words are `unavailable_reason`).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, ToSchema)]
#[serde(rename_all = "lowercase")]
pub enum Unavailable {
    /// Private credentials that no session of its owner holds now: back
    /// once the owner signs in.
    Locked,
    /// The backend fails (unreachable, credentials refused).
    Failing,
}

/// Who may make drives on a global backend.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize, ToSchema, sqlx::Type)]
#[serde(rename_all = "snake_case")]
#[sqlx(type_name = "TEXT", rename_all = "snake_case")]
pub enum MakeDrives {
    /// The superadmin only.
    Superadmin,
    /// Also the teams' managers, for their team's drives.
    TeamManagers,
    /// Everyone who may make a shared drive.
    Everyone,
    /// The managers of the chosen teams, for those teams' drives.
    Teams,
}

/// A row of `backends`.
#[derive(Clone, Debug, sqlx::FromRow)]
pub struct BackendRow {
    pub id: Uuid,
    pub owner_id: Option<Uuid>,
    pub name: String,
    pub endpoint: String,
    pub region: String,
    pub bucket: String,
    pub prefix: String,
    pub access_key: String,
    pub secret: Option<Vec<u8>>,
    pub sealed_secret: Option<String>,
    pub storage_limit: Option<i64>,
    pub drive_limit: Option<i64>,
    pub make_drives: MakeDrives,
    pub list_every: Option<i64>,
    pub created_at: String,
}

pub const BACKEND_COLUMNS: &str = "id, owner_id, name, endpoint, region, bucket, prefix, access_key, \
     secret, sealed_secret, storage_limit, drive_limit, make_drives, list_every, created_at";

impl BackendRow {
    pub async fn get<'e>(
        db: impl sqlx::SqliteExecutor<'e>,
        id: Uuid,
    ) -> sqlx::Result<Option<BackendRow>> {
        sqlx::query_as(sqlx::AssertSqlSafe(format!(
            "SELECT {BACKEND_COLUMNS} FROM backends WHERE id = ?1"
        )))
        .bind(id)
        .fetch_optional(db)
        .await
    }

    /// Private credentials: the secret is only in the sessions that
    /// unlocked it.
    pub fn private(&self) -> bool {
        self.sealed_secret.is_some()
    }

    /// Where it is, for the pages: `s3://bucket/prefix`.
    pub fn location(&self) -> String {
        match self.prefix.trim_matches('/') {
            "" => format!("s3://{}", self.bucket),
            prefix => format!("s3://{}/{prefix}", self.bucket),
        }
    }
}

/// What a connection to S3 is made of.
#[derive(Clone, Debug)]
pub struct Connection {
    pub endpoint: String,
    pub region: String,
    pub bucket: String,
    pub prefix: String,
    pub access_key: String,
    pub secret: String,
}

impl Connection {
    pub fn of(row: &BackendRow, secret: String) -> Connection {
        Connection {
            endpoint: row.endpoint.clone(),
            region: row.region.clone(),
            bucket: row.bucket.clone(),
            prefix: row.prefix.clone(),
            access_key: row.access_key.clone(),
            secret,
        }
    }
}

/// The backends' operators, health and secrets.
pub struct Backends {
    db: Db,
    data_dir: PathBuf,
    /// `SECRET_KEY_FILE`; None: `<DATA_DIR>/secret.key`.
    key_file: Mutex<Option<PathBuf>>,
    key: OnceLock<[u8; 32]>,
    /// `BACKENDS_ALLOW_PRIVATE`.
    allow_private: AtomicBool,
    health: Mutex<HashMap<Uuid, Arc<Health>>>,
    /// The secrets of private credentials, by backend: each with the hash
    /// of the session that unlocked it.
    unlocked: Mutex<HashMap<Uuid, Vec<(String, String)>>>,
    /// Operators standing in for endpoints (the tests: no network).
    stand_ins: Mutex<HashMap<String, Operator>>,
}

impl Backends {
    pub fn new(db: Db, data_dir: PathBuf) -> Backends {
        Backends {
            db,
            data_dir,
            key_file: Mutex::new(None),
            key: OnceLock::new(),
            allow_private: AtomicBool::new(false),
            health: Mutex::new(HashMap::new()),
            unlocked: Mutex::new(HashMap::new()),
            stand_ins: Mutex::new(HashMap::new()),
        }
    }

    /// `SECRET_KEY_FILE`: the server key is read from there (never made).
    pub fn set_key_file(&self, file: Option<PathBuf>) {
        *self.key_file.lock().expect("not poisoned") = file;
    }

    /// `BACKENDS_ALLOW_PRIVATE`: users' own backends may be at private and
    /// local addresses, and over http.
    pub fn set_allow_private(&self, allow: bool) {
        self.allow_private.store(allow, Ordering::Relaxed);
    }

    pub fn allow_private(&self) -> bool {
        self.allow_private.load(Ordering::Relaxed)
    }

    /// The server key: read, or made at the first start in the data dir.
    pub fn key(&self) -> std::io::Result<[u8; 32]> {
        if let Some(key) = self.key.get() {
            return Ok(*key);
        }
        let named = self.key_file.lock().expect("not poisoned").clone();
        let key = match &named {
            Some(file) => read_key(file)?,
            None => {
                let file = self.data_dir.join("secret.key");
                match read_key(&file) {
                    Ok(key) => key,
                    Err(e) if e.kind() == std::io::ErrorKind::NotFound => make_key(&file)?,
                    Err(e) => return Err(e),
                }
            }
        };
        Ok(*self.key.get_or_init(|| key))
    }

    /// A secret encrypted with the server key: a new nonce, then the
    /// ciphertext and its tag.
    pub fn seal(&self, secret: &str) -> std::io::Result<Vec<u8>> {
        let key = self.key()?;
        let mut nonce = [0u8; 12];
        rand::Rng::fill_bytes(&mut rand::rng(), &mut nonce);
        let sealed = Aes256Gcm::new(&key.into())
            .encrypt(
                &Nonce::from(nonce),
                Payload {
                    msg: secret.as_bytes(),
                    aad: SECRET_AD,
                },
            )
            .map_err(|_| std::io::Error::other("cannot encrypt"))?;
        let mut out = nonce.to_vec();
        out.extend(sealed);
        Ok(out)
    }

    /// A secret out of [`Backends::seal`]'s bytes.
    pub fn open(&self, sealed: &[u8]) -> std::io::Result<String> {
        let key = self.key()?;
        if sealed.len() < 12 + 16 {
            return Err(std::io::Error::other("a sealed secret is too short"));
        }
        let nonce: [u8; 12] = sealed[..12].try_into().expect("12 bytes");
        let plain = Aes256Gcm::new(&key.into())
            .decrypt(
                &Nonce::from(nonce),
                Payload {
                    msg: &sealed[12..],
                    aad: SECRET_AD,
                },
            )
            .map_err(|_| {
                std::io::Error::other(
                    "cannot open a backend's secret: not the server key it was sealed with",
                )
            })?;
        String::from_utf8(plain).map_err(std::io::Error::other)
    }

    /// A backend's health (made on first use).
    pub fn health(&self, backend: Uuid) -> Arc<Health> {
        self.health
            .lock()
            .expect("not poisoned")
            .entry(backend)
            .or_default()
            .clone()
    }

    /// Keep a private backend's secret for a session.
    pub fn unlock(&self, backend: Uuid, session: &str, secret: String) {
        let mut unlocked = self.unlocked.lock().expect("not poisoned");
        let held = unlocked.entry(backend).or_default();
        held.retain(|(s, _)| s != session);
        held.push((session.to_string(), secret));
    }

    /// Forget what a session unlocked of a backend (None: every session's).
    pub fn lock(&self, backend: Uuid, session: Option<&str>) {
        let mut unlocked = self.unlocked.lock().expect("not poisoned");
        match session {
            Some(session) => {
                if let Some(held) = unlocked.get_mut(&backend) {
                    held.retain(|(s, _)| s != session);
                }
            }
            None => {
                unlocked.remove(&backend);
            }
        }
    }

    /// A private backend's secret from a session that is still there.
    async fn unlocked_secret(&self, backend: Uuid) -> sqlx::Result<Option<String>> {
        let held = self
            .unlocked
            .lock()
            .expect("not poisoned")
            .get(&backend)
            .cloned()
            .unwrap_or_default();
        for (session, secret) in held {
            if self.session_lives(&session).await? {
                return Ok(Some(secret));
            }
            self.lock(backend, Some(&session));
        }
        Ok(None)
    }

    async fn session_lives(&self, session: &str) -> sqlx::Result<bool> {
        let now = jiff::Timestamp::now().as_second();
        let found: Option<i64> =
            sqlx::query_scalar("SELECT 1 FROM sessions WHERE id_hash = ?1 AND expires_at > ?2")
                .bind(session)
                .bind(now)
                .fetch_optional(self.db.reader())
                .await?;
        Ok(found.is_some())
    }

    /// Drop the secrets of sessions that ended (signed out, expired): the
    /// backends that lost their last one.
    pub async fn sweep_sessions(&self) -> sqlx::Result<Vec<Uuid>> {
        let held: Vec<(Uuid, Vec<String>)> = self
            .unlocked
            .lock()
            .expect("not poisoned")
            .iter()
            .map(|(b, v)| (*b, v.iter().map(|(s, _)| s.clone()).collect()))
            .collect();
        let mut locked = Vec::new();
        for (backend, sessions) in held {
            for session in sessions {
                if !self.session_lives(&session).await? {
                    self.lock(backend, Some(&session));
                }
            }
            let empty = self
                .unlocked
                .lock()
                .expect("not poisoned")
                .get(&backend)
                .is_none_or(Vec::is_empty);
            if empty {
                self.lock(backend, None);
                locked.push(backend);
            }
        }
        Ok(locked)
    }

    /// Whether a private backend is locked: no living session unlocked it.
    pub async fn locked(&self, row: &BackendRow) -> sqlx::Result<bool> {
        Ok(row.private() && self.unlocked_secret(row.id).await?.is_none())
    }

    /// An operator to use in place of an endpoint's (no network): for the
    /// tests, which have no S3 to talk to.
    #[doc(hidden)]
    pub fn stand_in(&self, endpoint: &str, op: Operator) {
        self.stand_ins
            .lock()
            .expect("not poisoned")
            .insert(endpoint.to_string(), op);
    }

    /// The endpoint is itself again (the tests: a backend that stops
    /// answering).
    #[doc(hidden)]
    pub fn drop_stand_in(&self, endpoint: &str) {
        self.stand_ins
            .lock()
            .expect("not poisoned")
            .remove(endpoint);
    }

    /// A user's own backend whose endpoint an in-memory operator stands in
    /// for ([`Backends::stand_in`]): for the tests of other crates (the
    /// sync engine's), which need one to make a mirror (always on its
    /// user's own storage) and have no S3, nor opendal to make the stand-in.
    #[doc(hidden)]
    pub async fn stand_in_own(&self, user: Uuid) -> Result<Uuid, FileError> {
        let id = Uuid::now_v7();
        let endpoint = format!("https://stand-in-{id}.test");
        let op = Operator::new(opendal::services::Memory::default())
            .map_err(|e| FileError::Internal(e.into()))?;
        self.stand_in(&endpoint, op);
        let secret = self.seal("stand-in")?;
        sqlx::query(
            "INSERT INTO backends (id, owner_id, kind, name, endpoint, bucket, prefix, access_key,
               secret)
             VALUES (?1, ?2, 's3', ?3, ?4, 'bucket', 'organizer', 'stand-in', ?5)",
        )
        .bind(id)
        .bind(user)
        .bind(format!("Stand-in {id}"))
        .bind(endpoint)
        .bind(secret)
        .execute(self.db.writer())
        .await?;
        Ok(id)
    }

    /// The global backend of an `s3://` `STORAGE_URL` (credentials from
    /// `STORAGE_ACCESS_KEY` / `STORAGE_SECRET_KEY`), made or updated at start;
    /// its id, so that new drives are made in the bucket.
    pub async fn ensure_files_backend(
        &self,
        endpoint: &str,
        region: &str,
        bucket: &str,
        prefix: &str,
    ) -> Result<Uuid, String> {
        let access_key = crate::config::storage_access_key()?;
        let secret = self
            .seal(&crate::config::storage_secret_key()?)
            .map_err(|e| e.to_string())?;
        let existing: Option<Uuid> =
            sqlx::query_scalar("SELECT id FROM backends WHERE owner_id IS NULL AND name = 'files'")
                .fetch_optional(self.db.reader())
                .await
                .map_err(|e| e.to_string())?;
        let id = match existing {
            Some(id) => {
                sqlx::query(
                    "UPDATE backends SET endpoint = ?2, region = ?3, bucket = ?4, prefix = ?5,
                       access_key = ?6, secret = ?7, list_every = 3600 WHERE id = ?1",
                )
                .bind(id)
                .bind(endpoint)
                .bind(region)
                .bind(bucket)
                .bind(prefix)
                .bind(&access_key)
                .bind(&secret)
                .execute(self.db.writer())
                .await
                .map_err(|e| e.to_string())?;
                id
            }
            None => {
                let id = Uuid::now_v7();
                sqlx::query(
                    "INSERT INTO backends (id, owner_id, kind, name, endpoint, region, bucket,
                       prefix, access_key, secret, make_drives, list_every)
                     VALUES (?1, NULL, 's3', 'files', ?2, ?3, ?4, ?5, ?6, ?7, 'everyone', 3600)",
                )
                .bind(id)
                .bind(endpoint)
                .bind(region)
                .bind(bucket)
                .bind(prefix)
                .bind(&access_key)
                .bind(&secret)
                .execute(self.db.writer())
                .await
                .map_err(|e| e.to_string())?;
                id
            }
        };
        Ok(id)
    }

    /// Whether an endpoint may be used: https and a public address for a
    /// user's own backend (names are checked again when resolved), unless
    /// `BACKENDS_ALLOW_PRIVATE`. The superadmin's may be anywhere.
    pub fn check_endpoint(&self, endpoint: &str, own: bool) -> Result<(), String> {
        let url = reqwest::Url::parse(endpoint).map_err(|_| {
            "The endpoint must be a URL, such as https://s3.example.com".to_string()
        })?;
        if !matches!(url.scheme(), "http" | "https") || url.host_str().is_none() {
            return Err("The endpoint must be an http or https URL".into());
        }
        if own && !self.allow_private() {
            if url.scheme() != "https" {
                return Err("Your storage's endpoint must use https".into());
            }
            if !crate::drop_box::link::allowed(&url, false) {
                return Err(
                    "Your storage must be at a public address, not one of the server's own network"
                        .into(),
                );
            }
        }
        Ok(())
    }

    /// The operator of a connection, at the bucket's root.
    pub fn operator(&self, conn: &Connection, own: bool) -> Result<Operator, FileError> {
        if let Some(op) = self
            .stand_ins
            .lock()
            .expect("not poisoned")
            .get(&conn.endpoint)
        {
            return Ok(op.clone());
        }
        let region = match conn.region.trim() {
            // What MinIO, Garage and R2 take when none is set.
            "" => "us-east-1",
            region => region,
        };
        let builder = opendal::services::S3::default()
            .endpoint(&conn.endpoint)
            .region(region)
            .bucket(&conn.bucket)
            .access_key_id(&conn.access_key)
            .secret_access_key(&conn.secret)
            .disable_config_load()
            .disable_ec2_metadata();
        let op = Operator::new(builder)
            .map_err(|e| FileError::Invalid(format!("Not a usable S3 configuration: {e}")))?;
        if !own {
            return Ok(op);
        }
        let guarded = Guarded::new(self.allow_private());
        let context = op
            .context()
            .with_http_transport(opendal::HttpTransporter::new(guarded));
        Ok(op.with_context(context))
    }

    /// Whether a connection works: the endpoint is allowed, and an object
    /// can be written in the prefix, read back and deleted. Err: why not,
    /// fit to show.
    pub async fn test(&self, conn: &Connection, own: bool) -> Result<(), String> {
        self.check_endpoint(&conn.endpoint, own)?;
        let op = self.operator(conn, own).map_err(|e| e.to_string())?;
        let probe = format!(
            "{}.organizer-test-{}",
            match conn.prefix.trim_matches('/') {
                "" => String::new(),
                prefix => format!("{prefix}/"),
            },
            Uuid::now_v7()
        );
        let fail = |e: opendal::Error| {
            tracing::info!("a storage backend's test failed: {e}");
            crate::storage::reason(&e).to_string()
        };
        op.write(&probe, b"organizer".to_vec())
            .await
            .map_err(fail)?;
        let read = op.read(&probe).await.map_err(fail)?;
        op.delete(&probe).await.map_err(fail)?;
        if read.to_vec() != b"organizer" {
            return Err("The storage did not give back what was written".into());
        }
        Ok(())
    }

    /// A backend's secret: opened with the server key, or the one a living
    /// session unlocked (None: locked).
    pub async fn secret_of(&self, row: &BackendRow) -> Result<Option<String>, FileError> {
        match &row.secret {
            Some(sealed) => Ok(Some(self.open(sealed)?)),
            None => Ok(self.unlocked_secret(row.id).await?),
        }
    }

    /// A drive's storage in a backend: at `<prefix>/<drive id>/`. A locked
    /// backend's drives are not available.
    pub async fn storage(&self, row: &BackendRow, drive: Uuid) -> Result<Storage, FileError> {
        let Some(secret) = self.secret_of(row).await? else {
            return Err(FileError::Down(self.locked_reason(row).await?));
        };
        let op = self.operator(&Connection::of(row, secret), row.owner_id.is_some())?;
        Ok(Storage::remote(
            op,
            crate::storage::drive_base(&row.prefix, drive),
            self.health(row.id),
        ))
    }

    /// "Locked until <owner> signs in", for a drive's members.
    pub async fn locked_reason(&self, row: &BackendRow) -> sqlx::Result<String> {
        let owner: Option<String> = match row.owner_id {
            Some(owner) => {
                sqlx::query_scalar("SELECT email FROM users WHERE id = ?1")
                    .bind(owner)
                    .fetch_optional(self.db.reader())
                    .await?
            }
            None => None,
        };
        Ok(format!(
            "Locked until {} signs in",
            owner.as_deref().unwrap_or("its owner")
        ))
    }

    /// Why a backend's drives are not available now; None: they are.
    pub async fn unavailable(&self, row: &BackendRow) -> sqlx::Result<Option<String>> {
        Ok(self.unavailable_why(row).await?.map(|(_, reason)| reason))
    }

    /// Whether a backend's drives are locked or failing now, and why in
    /// words; None: they are available.
    pub async fn unavailable_why(
        &self,
        row: &BackendRow,
    ) -> sqlx::Result<Option<(Unavailable, String)>> {
        if self.locked(row).await? {
            return Ok(Some((Unavailable::Locked, self.locked_reason(row).await?)));
        }
        Ok(self
            .health(row.id)
            .error()
            .map(|reason| (Unavailable::Failing, reason)))
    }
}

fn read_key(file: &Path) -> std::io::Result<[u8; 32]> {
    let bytes = std::fs::read(file)?;
    bytes.as_slice().try_into().map_err(|_| {
        std::io::Error::other(format!(
            "{} must hold the 32 bytes of the server key",
            file.display()
        ))
    })
}

/// A new server key, written where only this user reads it.
fn make_key(file: &Path) -> std::io::Result<[u8; 32]> {
    let mut key = [0u8; 32];
    rand::Rng::fill_bytes(&mut rand::rng(), &mut key);
    if let Some(dir) = file.parent() {
        std::fs::create_dir_all(dir)?;
    }
    let mut options = std::fs::OpenOptions::new();
    options.write(true).create_new(true);
    #[cfg(unix)]
    std::os::unix::fs::OpenOptionsExt::mode(&mut options, 0o600);
    std::io::Write::write_all(&mut options.open(file)?, &key)?;
    tracing::info!("made the server key {}", file.display());
    Ok(key)
}

/// Our HTTP client under opendal, for users' own backends and the shares
/// of other servers (`federation::incoming`): names resolve to public
/// addresses only (unless allowed) and no redirect is followed. Written
/// here rather than taking opendal's reqwest transport crate as one more
/// direct dependency: it is these few lines.
pub(crate) struct Guarded {
    client: reqwest::Client,
    /// A bearer token for every request (another server's share), asked
    /// anew once when it is refused (401).
    bearer: Option<Arc<dyn Bearer>>,
}

/// The future of a bearer token.
pub(crate) type TokenFuture<'a> =
    std::pin::Pin<Box<dyn std::future::Future<Output = opendal::Result<String>> + Send + 'a>>;

/// Where a bearer token comes from: the code flow of another server.
pub(crate) trait Bearer: Send + Sync {
    /// The token; a new one when `fresh` (the last one was refused).
    fn token(&self, fresh: bool) -> TokenFuture<'_>;
}

impl Guarded {
    fn new(allow_private: bool) -> Guarded {
        let mut builder = reqwest::Client::builder()
            .timeout(TIMEOUT)
            .redirect(reqwest::redirect::Policy::none());
        if !allow_private {
            builder = builder.dns_resolver(Arc::new(crate::drop_box::link::PublicOnly));
        }
        Guarded {
            client: builder.build().expect("an HTTP client"),
            bearer: None,
        }
    }

    /// On a client of our own, with a bearer token for every request.
    pub(crate) fn with_bearer(client: reqwest::Client, bearer: Arc<dyn Bearer>) -> Guarded {
        Guarded {
            client,
            bearer: Some(bearer),
        }
    }
}

impl opendal::HttpTransport for Guarded {
    async fn fetch(
        &self,
        request: axum::http::Request<opendal::Buffer>,
    ) -> opendal::Result<axum::http::Response<opendal::HttpBody>> {
        use opendal::{Error, ErrorKind};
        let (parts, body) = request.into_parts();
        let failed = |e: reqwest::Error| {
            Error::new(ErrorKind::Unexpected, "send http request")
                .with_temporary(e.is_request() || e.is_body() || e.is_connect())
                .set_source(e)
        };
        let url = reqwest::Url::parse(&parts.uri.to_string()).map_err(|e| {
            Error::new(ErrorKind::Unexpected, "request url is invalid").set_source(e)
        })?;
        let is_head = parts.method == axum::http::Method::HEAD;
        let body = (!body.is_empty()).then(|| body.to_bytes());
        let send = |token: Option<String>| {
            let mut builder = self
                .client
                .request(parts.method.clone(), url.clone())
                .headers(parts.headers.clone())
                .version(parts.version);
            if let Some(token) = token {
                builder = builder.bearer_auth(token);
            }
            if let Some(body) = &body {
                builder = builder.body(body.clone());
            }
            builder.send()
        };
        let mut response = match &self.bearer {
            None => send(None).await.map_err(failed)?,
            Some(bearer) => {
                let first = send(Some(bearer.token(false).await?))
                    .await
                    .map_err(failed)?;
                // A token that ended early: a new one, once.
                if first.status() == reqwest::StatusCode::UNAUTHORIZED {
                    send(Some(bearer.token(true).await?))
                        .await
                        .map_err(failed)?
                } else {
                    first
                }
            }
        };
        let size = if is_head
            || response
                .headers()
                .contains_key(axum::http::header::CONTENT_ENCODING)
        {
            None
        } else {
            response.content_length()
        };
        let mut out = axum::http::Response::builder()
            .status(response.status())
            .version(response.version())
            .extension(parts.uri);
        std::mem::swap(
            out.headers_mut().expect("a builder"),
            response.headers_mut(),
        );
        // The body as it comes, through a channel: a stream opendal can own.
        let (sender, receiver) = tokio::sync::mpsc::channel(4);
        tokio::spawn(async move {
            loop {
                let next = match response.chunk().await {
                    Ok(Some(chunk)) if chunk.is_empty() => continue,
                    Ok(Some(chunk)) => Ok(opendal::Buffer::from(chunk)),
                    Ok(None) => return,
                    Err(e) => Err(failed(e)),
                };
                let stop = next.is_err();
                if sender.send(next).await.is_err() || stop {
                    return;
                }
            }
        });
        Ok(out
            .body(opendal::HttpBody::new(Chunks(receiver), size))
            .expect("a response"))
    }
}

struct Chunks(tokio::sync::mpsc::Receiver<opendal::Result<opendal::Buffer>>);

impl futures_core::Stream for Chunks {
    type Item = opendal::Result<opendal::Buffer>;

    fn poll_next(
        mut self: std::pin::Pin<&mut Self>,
        cx: &mut std::task::Context<'_>,
    ) -> std::task::Poll<Option<Self::Item>> {
        self.0.poll_recv(cx)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    async fn fresh() -> (Backends, PathBuf) {
        let db = crate::db::open(":memory:").await.unwrap();
        let dir = std::env::temp_dir().join(format!("organizer-backends-{}", Uuid::now_v7()));
        (Backends::new(db, dir.clone()), dir)
    }

    #[tokio::test]
    async fn secrets_round_trip_under_the_server_key_made_at_first_use() {
        let (backends, dir) = fresh().await;
        let sealed = backends.seal("s3cret").unwrap();
        assert!(!sealed.windows(6).any(|w| w == b"s3cret"));
        assert_eq!(backends.open(&sealed).unwrap(), "s3cret");
        let key = std::fs::read(dir.join("secret.key")).unwrap();
        assert_eq!(key.len(), 32);
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let mode = std::fs::metadata(dir.join("secret.key"))
                .unwrap()
                .permissions()
                .mode();
            assert_eq!(mode & 0o777, 0o600);
        }
        // The same key at the next start; another key opens nothing.
        let (again, other) = fresh().await;
        again.set_key_file(Some(dir.join("secret.key")));
        assert_eq!(again.open(&sealed).unwrap(), "s3cret");
        let (stranger, _) = fresh().await;
        std::fs::create_dir_all(&other).unwrap();
        std::fs::write(other.join("other.key"), [7u8; 32]).unwrap();
        stranger.set_key_file(Some(other.join("other.key")));
        assert!(stranger.open(&sealed).is_err());
        // A key file that is not 32 bytes stops the start.
        let (broken, _) = fresh().await;
        std::fs::write(other.join("short.key"), b"short").unwrap();
        broken.set_key_file(Some(other.join("short.key")));
        assert!(broken.key().is_err());
        std::fs::remove_dir_all(other).unwrap();
        std::fs::remove_dir_all(dir).unwrap();
    }

    #[tokio::test]
    async fn users_endpoints_must_be_public_and_https() {
        let (backends, _) = fresh().await;
        for refused in [
            "http://8.8.8.8",
            "https://127.0.0.1:9000",
            "https://10.0.0.5",
            "https://[::1]",
            "https://169.254.169.254",
            "ftp://8.8.8.8",
            "not a url",
        ] {
            assert!(backends.check_endpoint(refused, true).is_err(), "{refused}");
        }
        backends.check_endpoint("https://8.8.8.8", true).unwrap();
        // The superadmin's may be anywhere; and with BACKENDS_ALLOW_PRIVATE,
        // the users' too.
        backends
            .check_endpoint("http://127.0.0.1:9000", false)
            .unwrap();
        backends.set_allow_private(true);
        backends
            .check_endpoint("http://10.0.0.5:9000", true)
            .unwrap();
    }
}
