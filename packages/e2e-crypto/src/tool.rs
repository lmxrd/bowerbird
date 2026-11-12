//! `organizer-decrypt`: an encrypted folder decrypted by hand, with the
//! passphrase or the recovery key, so that nobody depends on the apps to
//! read their own files (docs/M6E_DESIGN.md §7, docs/E2E_SPEC.md §10).
//!
//! Two sources: a running server (its URL and an API key: the keys, the
//! names and the contents through the API), or a restored backup (the
//! database and the files folder, read directly; the server need not run).
//! Both go through [`Source`], and [`decrypt_folder`] does the rest.

use crate::{contents, folder, keys, names, unb64};
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use uuid::Uuid;

/// What went wrong, fit to show.
#[derive(Debug, thiserror::Error)]
pub enum ToolError {
    #[error("{0}")]
    Usage(String),
    #[error("{0}")]
    Source(String),
    #[error("{0}")]
    Crypto(#[from] crate::Error),
    #[error(transparent)]
    Io(#[from] std::io::Error),
}

fn source_error(e: impl std::fmt::Display) -> ToolError {
    ToolError::Source(e.to_string())
}

/// The user's key material, as the server keeps it (base64url).
#[derive(Clone, Debug, serde::Deserialize)]
pub struct KeyMaterial {
    pub public_key: String,
    pub passphrase_salt: String,
    pub passphrase_iterations: u32,
    pub passphrase_wrapped: String,
    pub recovery_wrapped: String,
}

/// What unlocks the private key.
pub enum Secret {
    Passphrase(String),
    /// As shown: 13 groups of 4 base32 characters.
    RecoveryKey(String),
}

/// The private key out of the key material, with the passphrase or the
/// recovery key; checked against the public key.
pub fn unlock(material: &KeyMaterial, secret: &Secret) -> Result<[u8; 32], ToolError> {
    let private = match secret {
        Secret::Passphrase(passphrase) => keys::unwrap_with_passphrase(
            &unb64(&material.passphrase_wrapped)?,
            passphrase,
            &unb64(&material.passphrase_salt)?,
            material.passphrase_iterations,
        ),
        Secret::RecoveryKey(text) => keys::unwrap_with_recovery(
            &unb64(&material.recovery_wrapped)?,
            &keys::decode_recovery(text)?,
        ),
    }
    .map_err(|e| match e {
        crate::Error::Decrypt => ToolError::Usage(match secret {
            Secret::Passphrase(_) => "Wrong passphrase".into(),
            Secret::RecoveryKey(_) => "Wrong recovery key".into(),
        }),
        e => e.into(),
    })?;
    if keys::public_key(&private).to_vec() != unb64(&material.public_key)? {
        return Err(ToolError::Source(
            "The private key does not match the public key".into(),
        ));
    }
    Ok(private)
}

/// A file or folder of a listing.
#[derive(Clone, Debug)]
pub struct Entry {
    pub id: Uuid,
    pub name: String,
    pub folder: bool,
}

/// The folder to decrypt: an encrypted folder (the top one), by its path
/// in the user's files (its name is plain) or by its id.
pub enum Which {
    Path(String),
    Id(Uuid),
}

/// Where the keys, names and contents come from.
#[allow(async_fn_in_trait)]
pub trait Source {
    /// The user's key material; None if they never set up encryption.
    async fn key_material(&self) -> Result<Option<KeyMaterial>, ToolError>;
    /// The folder: its id and its encrypted folder (`e2e_root`).
    async fn find(&self, which: &Which) -> Result<(Uuid, Option<Uuid>), ToolError>;
    /// The user's wrapped keys of an encrypted folder, by generation.
    async fn folder_keys(&self, root: Uuid) -> Result<Vec<(u32, Vec<u8>)>, ToolError>;
    async fn children(&self, folder: Uuid) -> Result<Vec<Entry>, ToolError>;
    /// A file's (encrypted) contents into `to`.
    async fn download(&self, file: Uuid, to: &Path) -> Result<(), ToolError>;
}

/// What a run did.
#[derive(Debug, Default)]
pub struct Report {
    pub files: usize,
    pub folders: usize,
    /// What could not be decrypted (its id and why): left out.
    pub failed: Vec<(Uuid, String)>,
}

/// Decrypt an encrypted folder into `out` (made if it is not there; what
/// is in it already is not overwritten).
pub async fn decrypt_folder(
    source: &impl Source,
    secret: &Secret,
    which: &Which,
    out: &Path,
) -> Result<Report, ToolError> {
    let material = source
        .key_material()
        .await?
        .ok_or_else(|| ToolError::Usage("Encryption was never set up for this user".into()))?;
    let private = unlock(&material, secret)?;
    let (id, root) = source.find(which).await?;
    let root = root.ok_or_else(|| ToolError::Usage("That folder is not encrypted".into()))?;
    if root != id {
        return Err(ToolError::Usage(
            "Name the encrypted folder itself (the one with the lock), not a folder in it".into(),
        ));
    }
    let mut keys = HashMap::new();
    for (generation, wrapped) in source.folder_keys(root).await? {
        keys.insert(
            generation,
            folder::unwrap(&wrapped, root, generation, &private)?,
        );
    }
    if keys.is_empty() {
        return Err(ToolError::Usage(
            "No key of this folder was ever given to this user".into(),
        ));
    }
    std::fs::create_dir_all(out)?;
    let mut report = Report::default();
    let mut todo: Vec<(Uuid, PathBuf)> = vec![(root, out.to_path_buf())];
    while let Some((folder_id, at)) = todo.pop() {
        for entry in source.children(folder_id).await? {
            let name = match names::generation(&entry.name).and_then(|g| {
                let key = keys.get(&g).ok_or(crate::Error::NoKey(g))?;
                names::decrypt(key, folder_id, &entry.name)
            }) {
                Ok(name) => name,
                Err(e) => {
                    report.failed.push((entry.id, format!("its name: {e}")));
                    continue;
                }
            };
            // A name that is not a plain file name is not written.
            if name.contains(['/', '\\', '\0']) || name == "." || name == ".." {
                report
                    .failed
                    .push((entry.id, format!("a name that cannot be: {name:?}")));
                continue;
            }
            let path = at.join(&name);
            if entry.folder {
                std::fs::create_dir_all(&path)?;
                report.folders += 1;
                todo.push((entry.id, path));
                continue;
            }
            if path.exists() {
                report
                    .failed
                    .push((entry.id, format!("{} exists already", path.display())));
                continue;
            }
            let encrypted = at.join(format!(".{}.organizer-encrypted", entry.id));
            source.download(entry.id, &encrypted).await?;
            let partial = at.join(format!(".{}.organizer-partial", entry.id));
            let result = (|| -> Result<(), contents::IoError> {
                let mut from = std::fs::File::open(&encrypted)?;
                let mut to = std::fs::File::create(&partial)?;
                contents::decrypt_io(|g| keys.get(&g).copied(), &mut from, &mut to)?;
                to.sync_all()?;
                Ok(())
            })();
            let _ = std::fs::remove_file(&encrypted);
            match result {
                Ok(()) => {
                    std::fs::rename(&partial, &path)?;
                    report.files += 1;
                }
                Err(e) => {
                    let _ = std::fs::remove_file(&partial);
                    report.failed.push((entry.id, format!("{name}: {e}")));
                }
            }
        }
    }
    Ok(report)
}

/// A running server, through its API with an API key.
pub struct Server {
    url: String,
    key: String,
    http: reqwest::Client,
}

impl Server {
    pub fn new(url: &str, key: &str) -> Self {
        Server {
            url: url.trim_end_matches('/').to_string(),
            key: key.to_string(),
            http: reqwest::Client::new(),
        }
    }

    async fn get(&self, path: &str) -> Result<reqwest::Response, ToolError> {
        let response = self
            .http
            .get(format!("{}/api{path}", self.url))
            .bearer_auth(&self.key)
            .send()
            .await
            .map_err(source_error)?;
        if !response.status().is_success() {
            let status = response.status();
            let body: serde_json::Value = response.json().await.unwrap_or_default();
            return Err(ToolError::Source(format!(
                "{path}: {status} {}",
                body["error"].as_str().unwrap_or("")
            )));
        }
        Ok(response)
    }

    async fn json(&self, path: &str) -> Result<serde_json::Value, ToolError> {
        self.get(path).await?.json().await.map_err(source_error)
    }
}

fn uuid_of(value: &serde_json::Value) -> Option<Uuid> {
    value.as_str().and_then(|s| s.parse().ok())
}

impl Source for Server {
    async fn key_material(&self) -> Result<Option<KeyMaterial>, ToolError> {
        let keys = self.json("/encryption").await?;
        if keys["set_up"] != true {
            return Ok(None);
        }
        let text = |v: &serde_json::Value| v.as_str().unwrap_or_default().to_string();
        Ok(Some(KeyMaterial {
            public_key: text(&keys["public_key"]),
            passphrase_salt: text(&keys["passphrase"]["salt"]),
            passphrase_iterations: keys["passphrase"]["iterations"].as_u64().unwrap_or(0) as u32,
            passphrase_wrapped: text(&keys["passphrase"]["wrapped"]),
            recovery_wrapped: text(&keys["recovery_wrapped"]),
        }))
    }

    async fn find(&self, which: &Which) -> Result<(Uuid, Option<Uuid>), ToolError> {
        let node = match which {
            Which::Path(path) => {
                let mut url = reqwest::Url::parse("http://x/files/by-path").expect("a URL");
                url.query_pairs_mut().append_pair("path", path);
                self.json(&format!("/files/by-path?{}", url.query().unwrap_or("")))
                    .await?
            }
            Which::Id(id) => self.json(&format!("/files/{id}")).await?,
        };
        let id = uuid_of(&node["id"]).ok_or_else(|| ToolError::Source("no id".into()))?;
        Ok((id, uuid_of(&node["e2e_root"])))
    }

    async fn folder_keys(&self, root: Uuid) -> Result<Vec<(u32, Vec<u8>)>, ToolError> {
        let keys = self.json(&format!("/encryption/folders/{root}")).await?;
        let mut out = Vec::new();
        for key in keys["keys"].as_array().into_iter().flatten() {
            out.push((
                key["generation"].as_u64().unwrap_or(0) as u32,
                unb64(key["wrapped"].as_str().unwrap_or_default())?,
            ));
        }
        Ok(out)
    }

    async fn children(&self, folder: Uuid) -> Result<Vec<Entry>, ToolError> {
        let mut out = Vec::new();
        let mut after: Option<String> = None;
        loop {
            let mut path = format!("/files/{folder}/children?limit=1000");
            if let Some(after) = &after {
                path.push_str(&format!("&after={after}"));
            }
            let page = self.json(&path).await?;
            for item in page["items"].as_array().into_iter().flatten() {
                out.push(Entry {
                    id: uuid_of(&item["id"]).ok_or_else(|| ToolError::Source("no id".into()))?,
                    name: item["name"].as_str().unwrap_or_default().to_string(),
                    folder: item["kind"] == "folder",
                });
            }
            match page["next"].as_str() {
                Some(next) => after = Some(next.to_string()),
                None => return Ok(out),
            }
        }
    }

    async fn download(&self, file: Uuid, to: &Path) -> Result<(), ToolError> {
        use std::io::Write;
        let mut response = self.get(&format!("/files/{file}/content")).await?;
        let mut out = std::fs::File::create(to)?;
        while let Some(chunk) = response.chunk().await.map_err(source_error)? {
            out.write_all(&chunk)?;
        }
        out.sync_all()?;
        Ok(())
    }
}

/// A restored backup: the database (`DATA_DIR/organizer.db`, or another
/// file) read only, and the user's files folder on disk.
pub struct Backup {
    db: sqlx::SqlitePool,
    user: Uuid,
    files_root: PathBuf,
}

impl Backup {
    /// The backup of `email`'s files: `database` the restored database
    /// file, `data_dir` the restored data folder (where a user's own files
    /// folder is by default), `files_dir` the user's files folder when it
    /// is elsewhere (`FILES_DIR`, a folder chosen in the settings, or
    /// restored to another place).
    pub async fn open(
        database: &Path,
        data_dir: Option<&Path>,
        email: &str,
        files_dir: Option<&Path>,
    ) -> Result<Self, ToolError> {
        use sqlx::sqlite::SqliteConnectOptions;
        if !database.is_file() {
            return Err(ToolError::Usage(format!(
                "No database at {}",
                database.display()
            )));
        }
        let options = SqliteConnectOptions::new()
            .filename(database)
            .read_only(true);
        let db = sqlx::SqlitePool::connect_with(options)
            .await
            .map_err(source_error)?;
        let row: Option<(Uuid, Option<String>)> =
            sqlx::query_as("SELECT id, files_root FROM users WHERE email = ?1 COLLATE NOCASE")
                .bind(email)
                .fetch_optional(&db)
                .await
                .map_err(source_error)?;
        let (user, stored_root) =
            row.ok_or_else(|| ToolError::Usage(format!("No user {email} in that database")))?;
        let files_root = match (files_dir, stored_root) {
            (Some(dir), _) => dir.to_path_buf(),
            (None, Some(root)) => PathBuf::from(root),
            (None, None) => data_dir
                .ok_or_else(|| ToolError::Usage("Give the data folder with --data-dir".into()))?
                .join("files")
                .join("users")
                .join(user.to_string())
                .join("files"),
        };
        if !files_root.is_dir() {
            let hint = "give FILES_DIR's folder with --files-dir";
            return Err(ToolError::Usage(format!(
                "No files folder at {} ({hint})",
                files_root.display()
            )));
        }
        Ok(Backup {
            db,
            user,
            files_root,
        })
    }

    /// Where a node is on disk: the names from the root down.
    async fn disk_path(&self, id: Uuid) -> Result<PathBuf, ToolError> {
        let names: Vec<String> = sqlx::query_scalar(
            "WITH RECURSIVE up (id, parent_id, name, depth) AS (
               SELECT id, parent_id, name, 0 FROM nodes WHERE id = ?1
               UNION ALL
               SELECT n.id, n.parent_id, n.name, up.depth + 1
               FROM nodes n JOIN up ON n.id = up.parent_id
             )
             SELECT name FROM up WHERE parent_id IS NOT NULL ORDER BY depth DESC",
        )
        .bind(id)
        .fetch_all(&self.db)
        .await
        .map_err(source_error)?;
        Ok(names
            .iter()
            .fold(self.files_root.clone(), |path, name| path.join(name)))
    }
}

impl Source for Backup {
    async fn key_material(&self) -> Result<Option<KeyMaterial>, ToolError> {
        let row: Option<(String, String, i64, String, String)> = sqlx::query_as(
            "SELECT public_key, passphrase_salt, passphrase_iterations, passphrase_wrapped,
               recovery_wrapped FROM e2e_keys WHERE user_id = ?1",
        )
        .bind(self.user)
        .fetch_optional(&self.db)
        .await
        .map_err(source_error)?;
        Ok(row.map(
            |(public_key, salt, iterations, wrapped, recovery)| KeyMaterial {
                public_key,
                passphrase_salt: salt,
                passphrase_iterations: iterations as u32,
                passphrase_wrapped: wrapped,
                recovery_wrapped: recovery,
            },
        ))
    }

    async fn find(&self, which: &Which) -> Result<(Uuid, Option<Uuid>), ToolError> {
        let not_found = || ToolError::Usage("No such folder".into());
        match which {
            Which::Id(id) => sqlx::query_as("SELECT id, e2e_root FROM nodes WHERE id = ?1")
                .bind(id)
                .fetch_optional(&self.db)
                .await
                .map_err(source_error)?
                .ok_or_else(not_found),
            Which::Path(path) => {
                // A path is from the user's My drive, whose id is the
                // user's (docs/M7A_DESIGN.md §1).
                let mut at: (Uuid, Option<Uuid>) = sqlx::query_as(
                    "SELECT id, e2e_root FROM nodes WHERE drive_id = ?1 AND parent_id IS NULL",
                )
                .bind(self.user)
                .fetch_optional(&self.db)
                .await
                .map_err(source_error)?
                .ok_or_else(not_found)?;
                for name in path.split('/').filter(|n| !n.is_empty()) {
                    at = sqlx::query_as(
                        "SELECT id, e2e_root FROM nodes WHERE parent_id = ?1 AND name = ?2",
                    )
                    .bind(at.0)
                    .bind(name)
                    .fetch_optional(&self.db)
                    .await
                    .map_err(source_error)?
                    .ok_or_else(not_found)?;
                }
                Ok(at)
            }
        }
    }

    async fn folder_keys(&self, root: Uuid) -> Result<Vec<(u32, Vec<u8>)>, ToolError> {
        let rows: Vec<(i64, String)> = sqlx::query_as(
            "SELECT generation, wrapped FROM e2e_folder_keys WHERE root_id = ?1 AND user_id = ?2",
        )
        .bind(root)
        .bind(self.user)
        .fetch_all(&self.db)
        .await
        .map_err(source_error)?;
        rows.into_iter()
            .map(|(g, w)| Ok((g as u32, unb64(&w)?)))
            .collect()
    }

    async fn children(&self, folder: Uuid) -> Result<Vec<Entry>, ToolError> {
        let rows: Vec<(Uuid, String, String)> =
            sqlx::query_as("SELECT id, name, kind FROM nodes WHERE parent_id = ?1")
                .bind(folder)
                .fetch_all(&self.db)
                .await
                .map_err(source_error)?;
        Ok(rows
            .into_iter()
            .map(|(id, name, kind)| Entry {
                id,
                name,
                folder: kind == "folder",
            })
            .collect())
    }

    async fn download(&self, file: Uuid, to: &Path) -> Result<(), ToolError> {
        std::fs::copy(self.disk_path(file).await?, to)?;
        Ok(())
    }
}

/// The command line's help.
pub const USAGE: &str = "\
organizer-decrypt: decrypt an encrypted folder of Organizer by hand.

From a running server (with an API key, Settings → API keys):
  organizer-decrypt --server https://files.example.com --api-key org_... \\
                    --folder /Private --out ./Private

From a restored backup (the server need not run):
  organizer-decrypt --data-dir /restore/data --user me@example.com \\
                    --folder /Private --out ./Private
  (--database FILE when the database is elsewhere than DATA_DIR/organizer.db;
   --files-dir DIR for the user's files folder: FILES_DIR, a folder chosen
   in the settings, or wherever it was restored)

--folder names the encrypted folder (the one with the lock) by its path in
your files, or --folder-id by its id. The passphrase is read from
ORGANIZER_PASSPHRASE, or asked for; with --recovery-key, the recovery key
is read from ORGANIZER_RECOVERY_KEY, or asked for. Nothing in --out is
overwritten.";

/// The command line, parsed.
pub struct Args {
    pub server: Option<String>,
    pub api_key: Option<String>,
    pub data_dir: Option<PathBuf>,
    pub database: Option<PathBuf>,
    pub files_dir: Option<PathBuf>,
    pub user: Option<String>,
    pub which: Which,
    pub out: PathBuf,
    pub recovery: bool,
}

impl Args {
    pub fn parse(args: impl IntoIterator<Item = String>) -> Result<Args, ToolError> {
        let mut values: HashMap<String, String> = HashMap::new();
        let mut recovery = false;
        let mut args = args.into_iter();
        while let Some(arg) = args.next() {
            match arg.as_str() {
                "--recovery-key" => recovery = true,
                "--help" | "-h" => return Err(ToolError::Usage(USAGE.into())),
                "--server" | "--api-key" | "--data-dir" | "--database" | "--files-dir"
                | "--user" | "--folder" | "--folder-id" | "--out" => {
                    let value = args
                        .next()
                        .ok_or_else(|| ToolError::Usage(format!("{arg} needs a value")))?;
                    values.insert(arg.trim_start_matches("--").to_string(), value);
                }
                other => {
                    return Err(ToolError::Usage(format!(
                        "Unknown argument {other}\n\n{USAGE}"
                    )));
                }
            }
        }
        let which = match (values.remove("folder"), values.remove("folder-id")) {
            (Some(path), None) => Which::Path(path),
            (None, Some(id)) => Which::Id(
                id.parse()
                    .map_err(|_| ToolError::Usage("--folder-id is not an id".into()))?,
            ),
            _ => {
                return Err(ToolError::Usage(format!(
                    "Give --folder or --folder-id\n\n{USAGE}"
                )));
            }
        };
        let out = values
            .remove("out")
            .map(PathBuf::from)
            .ok_or_else(|| ToolError::Usage(format!("Give --out\n\n{USAGE}")))?;
        let args = Args {
            server: values.remove("server"),
            api_key: values.remove("api-key"),
            data_dir: values.remove("data-dir").map(PathBuf::from),
            database: values.remove("database").map(PathBuf::from),
            files_dir: values.remove("files-dir").map(PathBuf::from),
            user: values.remove("user"),
            which,
            out,
            recovery,
        };
        let online = args.server.is_some() || args.api_key.is_some();
        let offline = args.data_dir.is_some() || args.database.is_some() || args.user.is_some();
        match (online, offline) {
            (true, false) if args.server.is_some() && args.api_key.is_some() => Ok(args),
            (false, true) if args.user.is_some() => Ok(args),
            _ => Err(ToolError::Usage(format!(
                "Give either --server and --api-key, or --data-dir (or --database) and --user\n\n{USAGE}"
            ))),
        }
    }
}

/// Run the command line with a secret already read.
pub async fn run(args: &Args, secret: &Secret) -> Result<Report, ToolError> {
    match (&args.server, &args.api_key) {
        (Some(url), Some(key)) => {
            decrypt_folder(&Server::new(url, key), secret, &args.which, &args.out).await
        }
        _ => {
            let database = match (&args.database, &args.data_dir) {
                (Some(file), _) => file.clone(),
                (None, Some(dir)) => dir.join("organizer.db"),
                (None, None) => {
                    return Err(ToolError::Usage("Give --data-dir or --database".into()));
                }
            };
            let backup = Backup::open(
                &database,
                args.data_dir.as_deref(),
                args.user.as_deref().unwrap_or_default(),
                args.files_dir.as_deref(),
            )
            .await?;
            decrypt_folder(&backup, secret, &args.which, &args.out).await
        }
    }
}
