//! End-to-end-encrypted folders (docs/E2E_SPEC.md, docs/M6E_DESIGN.md) as
//! the engine sees them: synced decrypted on disk, sealed on the server.
//!
//! - The **mirror** keeps the server's names as they are (sealed); the
//!   engine's view of the tree (`remote_tree`) decrypts them, so the
//!   entries, the local paths, the moves and the conflicts are all on
//!   decrypted names. Which nodes are encrypted is known from the tops
//!   (an encrypted folder, `e2e_root` its own id) and the synced folders
//!   inside one: everything under them is in their folder (a *region*).
//! - **Downloads** are decrypted into a second temporary file, hashed as
//!   plain text; **uploads** are encrypted (with the newest key) into a
//!   scratch file first, sent by its size, and their names sealed for the
//!   parent folder; a move or a rename seals the name again. Nothing moves
//!   across an encrypted folder's edge: the engine lets go of the node,
//!   which then goes to the trash on one side and is sent anew on the other.
//! - **Locked** (no private key here), or without a key for this user yet,
//!   an encrypted folder is *hidden* from the pass: never deleted, moved or
//!   sent, on either side, until it can be read again.
//! - The private key is the app's to keep (the system's credential store);
//!   the folder keys are fetched once per round and kept in memory only.
//! - **In a mirror** (docs/M7D_DESIGN.md) an encrypted folder is copied
//!   sealed, with no keys of its own: its keys are asked by the folder it
//!   copies (`source_id`, when its own id has none), and its names open
//!   with their parents' sources. A mirror is read-only: nothing is sealed
//!   there.
//! - **Shared from another server** (docs/E2E_SPEC.md §12, a drive of kind
//!   `remote`) an encrypted folder is opened the same way: its keys,
//!   answered by its id here, are wrapped for the owner's folder (the
//!   answer's `folder_id`), and its names open with their parents'
//!   `source_id`, the owner's ids. Unlike a mirror the role may allow
//!   writing: new names, renames and moves are sealed with the parent's
//!   source too, which a folder made a moment ago may not have yet: it is
//!   asked again, and nothing is sealed under it until it has one.

use crate::Error;
use crate::api::types;
use crate::local::Stat;
use crate::remote::{Downloaded, Remote};
use crate::state::State;
use organizer_e2e_crypto::{self as crypto, contents, folder, keys, names};
use std::collections::{HashMap, HashSet};
use std::io::Write;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex, RwLock};
use uuid::Uuid;

/// The meta key of the tops and synced folders that are encrypted.
const TOPS: &str = "e2e_tops";

/// The user's unlocked private key (X25519).
#[derive(Clone, PartialEq, Eq)]
pub struct PrivateKey([u8; 32]);

impl PrivateKey {
    /// As text, for the system's credential store.
    pub fn encode(&self) -> String {
        crypto::b64(&self.0)
    }

    pub fn decode(text: &str) -> Option<Self> {
        let bytes = crypto::unb64(text.trim()).ok()?;
        Some(PrivateKey(bytes.try_into().ok()?))
    }

    pub fn public_key(&self) -> [u8; 32] {
        keys::public_key(&self.0)
    }
}

impl std::fmt::Debug for PrivateKey {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("PrivateKey(..)")
    }
}

/// The user's private key out of its passphrase wrapping (GET
/// /api/encryption), checked against their public key.
pub async fn unlock(remote: &Remote, passphrase: &str) -> Result<PrivateKey, Error> {
    let account = remote
        .api
        .get_encryption()
        .await
        .map_err(Error::from_api)?
        .into_inner();
    let (Some(public), Some(wrap)) = (account.public_key, account.passphrase) else {
        return Err(Error::Invalid(
            "Encryption is not set up for this account".into(),
        ));
    };
    let passphrase = passphrase.to_string();
    let private = tokio::task::spawn_blocking(move || {
        let salt = crypto::unb64(&wrap.salt).ok()?;
        let wrapped = crypto::unb64(&wrap.wrapped).ok()?;
        let iterations = u32::try_from(wrap.iterations).ok()?;
        keys::unwrap_with_passphrase(&wrapped, &passphrase, &salt, iterations).ok()
    })
    .await
    .map_err(|e| Error::Invalid(e.to_string()))?
    .map(PrivateKey)
    .ok_or_else(|| Error::Invalid("Wrong passphrase".into()))?;
    if crypto::b64(&private.public_key()) != public {
        return Err(Error::Invalid(
            "This key does not match the account's: unlock again".into(),
        ));
    }
    Ok(private)
}

/// An encrypted folder's keys, by generation, as this user holds them.
#[derive(Debug, Default)]
pub struct FolderKeys {
    /// The newest generation, which new names and contents use.
    newest: u32,
    keys: HashMap<u32, [u8; 32]>,
}

impl FolderKeys {
    pub fn key(&self, generation: u32) -> Option<[u8; 32]> {
        self.keys.get(&generation).copied()
    }

    /// The newest key, for what is written now.
    fn newest(&self) -> Result<(u32, [u8; 32]), Error> {
        self.key(self.newest)
            .map(|key| (self.newest, key))
            .ok_or_else(|| Error::Invalid("The encrypted folder has no key for you yet".into()))
    }

    /// A sealed name opened, if it is of a key held and opens for `parent`.
    pub fn name(&self, parent: Uuid, sealed: &str) -> Option<String> {
        let key = self.key(names::generation(sealed).ok()?)?;
        names::decrypt(&key, parent, sealed).ok()
    }

    /// A name sealed for `parent` with the newest key.
    pub fn seal(&self, parent: Uuid, name: &str) -> Result<String, Error> {
        let (generation, key) = self.newest()?;
        names::encrypt(&key, generation, parent, &crate::names::nfc(name))
            .map_err(|_| Error::Invalid(format!("{name}: too long a name for an encrypted folder")))
    }
}

/// A name to send to the server: sealed in an encrypted folder (with the
/// folder's keys), else as it is.
pub struct Named {
    pub send: String,
    /// The plain name, when `send` is sealed.
    pub plain: Option<String>,
    pub keys: Option<Arc<FolderKeys>>,
}

impl Named {
    /// The name the entries keep: the plain one, else the server's.
    pub fn shown(&self, server: &str) -> String {
        self.plain.clone().unwrap_or_else(|| server.to_string())
    }
}

/// What an upload sends: the file itself, or its encrypted copy.
pub struct Source {
    pub path: PathBuf,
    /// The size sent (encrypted, for an encrypted copy), and the local
    /// file's modification time, which resuming an upload checks.
    pub stat: Stat,
    scratch: bool,
}

/// A node of the server's tree as a pass sees it: id, name, kind, version.
pub type Kid = (Uuid, String, crate::local::Kind, i64);

/// The engine's part: the private key, the folder keys, the regions.
pub struct E2e {
    private: RwLock<Option<PrivateKey>>,
    /// Per encrypted folder: its keys, None when this user has none (yet).
    cache: Mutex<HashMap<Uuid, Option<Arc<FolderKeys>>>>,
    /// The tops (their own id) and the synced folders inside one (its top):
    /// whatever is under them is in that encrypted folder.
    tops: Mutex<HashMap<Uuid, Uuid>>,
    /// Encrypted folders hidden in this pass: locked, or without a key.
    skipped: AtomicUsize,
    /// Where uploads are encrypted to before they are sent.
    scratch: PathBuf,
    /// In a mirror (docs/M7D_DESIGN.md), or shared from another server
    /// (docs/E2E_SPEC.md §12), an encrypted folder has no keys of its own:
    /// its keys and names are sealed with the ids of the drive mirrored, or
    /// of the owner's server. The copies' tops found so (their keys
    /// answered for the source's folder), and the nodes' sources as the
    /// server told them (`source_id`; only those known are kept: a folder
    /// made a moment ago gets its own once the other server has it).
    copies: Mutex<HashSet<Uuid>>,
    sources: Mutex<HashMap<Uuid, Uuid>>,
}

impl E2e {
    pub async fn load(state: &State, scratch: PathBuf) -> Result<Self, Error> {
        let tops = state
            .meta(TOPS)
            .await?
            .and_then(|json| serde_json::from_str(&json).ok())
            .unwrap_or_default();
        Ok(E2e {
            private: RwLock::new(None),
            cache: Mutex::new(HashMap::new()),
            tops: Mutex::new(tops),
            skipped: AtomicUsize::new(0),
            scratch,
            copies: Mutex::new(HashSet::new()),
            sources: Mutex::new(HashMap::new()),
        })
    }

    /// The node that `id` copies (of the drive mirrored, or on the owner's
    /// server), if it is a copy: kept once known, asked again while not.
    async fn source_of(&self, remote: &Remote, id: Uuid) -> Result<Option<Uuid>, Error> {
        if let Some(known) = self.sources.lock().unwrap().get(&id) {
            return Ok(Some(*known));
        }
        let source = match remote.node(id).await {
            Ok(node) => node.source_id,
            Err(Error::NotFound) => None,
            Err(e) => return Err(e),
        };
        if let Some(source) = source {
            self.sources.lock().unwrap().insert(id, source);
        }
        Ok(source)
    }

    /// The id a name in the folder `parent` of the encrypted folder `top`
    /// was sealed for: the parent's own, but in a copy its source's.
    pub async fn sealed_for(
        &self,
        remote: &Remote,
        top: Uuid,
        parent: Uuid,
    ) -> Result<Uuid, Error> {
        if !self.copies.lock().unwrap().contains(&top) {
            return Ok(parent);
        }
        Ok(self.source_of(remote, parent).await?.unwrap_or(parent))
    }

    /// The id a new name in `parent` is sealed for (a new file or folder, a
    /// rename, a move): as `sealed_for`, but in a copy (a folder shared
    /// from another server) the parent's source must be known: a folder
    /// not seen on the owner's server yet is listed again, and nothing is
    /// sealed under it while it has none (tried again at the next pass).
    pub async fn sealing_for(
        &self,
        remote: &Remote,
        top: Uuid,
        parent: Uuid,
    ) -> Result<Uuid, Error> {
        if !self.copies.lock().unwrap().contains(&top) {
            return Ok(parent);
        }
        self.source_of(remote, parent).await?.ok_or_else(|| {
            Error::Invalid(
                "A folder is not on its owner's server yet: what goes in it is sent once it is"
                    .into(),
            )
        })
    }

    pub fn set_private(&self, key: Option<PrivateKey>) {
        *self.private.write().unwrap() = key;
        self.forget_keys();
    }

    pub fn unlocked(&self) -> bool {
        self.private.read().unwrap().is_some()
    }

    fn private(&self) -> Option<PrivateKey> {
        self.private.read().unwrap().clone()
    }

    /// The folder keys are fetched again (a new round: a generation may
    /// have been added, a member removed).
    pub fn forget_keys(&self) {
        self.cache.lock().unwrap().clear();
    }

    /// Start of a pass: nothing hidden yet. Answers how many were in the last.
    pub fn take_skipped(&self) -> usize {
        self.skipped.swap(0, Ordering::SeqCst)
    }

    pub fn skip(&self) {
        self.skipped.fetch_add(1, Ordering::SeqCst);
    }

    /// What the server says of a node (`e2e_root`), kept when it is a top,
    /// or a synced folder (`synced`) inside an encrypted folder.
    pub async fn note(
        &self,
        state: &State,
        id: Uuid,
        e2e_root: Option<Uuid>,
        synced: bool,
    ) -> Result<(), Error> {
        let Some(top) = e2e_root.filter(|top| *top == id || synced) else {
            return Ok(());
        };
        let json = {
            let mut tops = self.tops.lock().unwrap();
            if tops.get(&id) == Some(&top) {
                return Ok(());
            }
            tops.insert(id, top);
            serde_json::to_string(&*tops).unwrap_or_default()
        };
        state.set_meta(TOPS, Some(&json)).await
    }

    /// The encrypted folder a synced folder is (in), from what was noted.
    pub fn region_of_root(&self, id: Uuid) -> Option<Uuid> {
        self.tops.lock().unwrap().get(&id).copied()
    }

    /// Whether a node is an encrypted folder's top, as noted.
    pub fn is_top(&self, id: Uuid) -> bool {
        self.tops.lock().unwrap().get(&id) == Some(&id)
    }

    /// The encrypted folder a node of the mirror is in: up its parents to a
    /// top or an encrypted synced folder.
    pub async fn region(&self, state: &State, id: Uuid) -> Result<Option<Uuid>, Error> {
        let mut at = id;
        for _ in 0..4096 {
            if let Some(top) = self.region_of_root(at) {
                return Ok(Some(top));
            }
            match state
                .remote(at)
                .await?
                .and_then(|row| row.parent_id)
                .and_then(|p| p.parse().ok())
            {
                Some(parent) => at = parent,
                None => return Ok(None),
            }
        }
        Ok(None)
    }

    /// An encrypted folder's keys: None while locked, or while this user
    /// has none (not given one yet, or the folder has none yet).
    pub async fn keys(&self, remote: &Remote, top: Uuid) -> Result<Option<Arc<FolderKeys>>, Error> {
        let Some(private) = self.private() else {
            return Ok(None);
        };
        if let Some(cached) = self.cache.lock().unwrap().get(&top) {
            return Ok(cached.clone());
        }
        let ask = |id: Uuid| async move {
            match remote.api.get_folder_keys(&id).await {
                Ok(found) => Ok(Some(found.into_inner())),
                Err(e) => match Error::from_api(e) {
                    Error::NotFound | Error::Server(400, _) => Ok(None),
                    other => Err(other),
                },
            }
        };
        // The keys are wrapped for the folder they were made for: a copy's
        // in a mirror are its source's.
        let mut sealed = top;
        let mut found = ask(top).await?;
        // A copy the mirror kept its user's keys of (docs/M7D_DESIGN.md
        // §9), or a folder shared from another server (docs/E2E_SPEC.md
        // §12): answered by its id here, wrapped for the drive's folder or
        // the owner's, which the answer's `folder_id` names.
        if let Some(answer) = &found
            && let Ok(folder) = answer.folder_id.to_string().parse::<Uuid>()
            && folder != top
        {
            sealed = folder;
            self.copies.lock().unwrap().insert(top);
        }
        if found.is_none()
            && let Some(source) = self.source_of(remote, top).await?
        {
            found = ask(source).await?;
            sealed = source;
            self.copies.lock().unwrap().insert(top);
        }
        let keys = found.map(|found| {
            let mut keys = FolderKeys {
                newest: u32::try_from(found.generation).unwrap_or(0),
                keys: HashMap::new(),
            };
            for wrapped in found.keys {
                let Ok(generation) = u32::try_from(wrapped.generation) else {
                    continue;
                };
                let key = crypto::unb64(&wrapped.wrapped)
                    .ok()
                    .and_then(|w| folder::unwrap(&w, sealed, generation, &private.0).ok());
                match key {
                    Some(key) => {
                        keys.keys.insert(generation, key);
                    }
                    None => tracing::warn!("e2e: a key of {sealed} ({generation}) does not open"),
                }
            }
            Arc::new(keys)
        });
        let keys = keys.filter(|k| !k.keys.is_empty());
        self.cache.lock().unwrap().insert(top, keys.clone());
        Ok(keys)
    }

    /// The keys of a region, which must be readable (an error otherwise);
    /// none for a plain one.
    pub async fn require(
        &self,
        remote: &Remote,
        region: Option<Uuid>,
    ) -> Result<Option<Arc<FolderKeys>>, Error> {
        let Some(top) = region else {
            return Ok(None);
        };
        match self.keys(remote, top).await? {
            Some(keys) => Ok(Some(keys)),
            None => Err(Error::Invalid(
                "The encrypted folder is locked on this computer".into(),
            )),
        }
    }

    /// A folder's children with their names opened (and the encrypted
    /// folder each is in), and those that cannot be read: hidden.
    pub async fn readable(
        &self,
        remote: &Remote,
        parent: Uuid,
        region: Option<Uuid>,
        kids: Vec<Kid>,
    ) -> Result<(Vec<(Kid, Option<Uuid>)>, Vec<Kid>), Error> {
        let mut shown = Vec::new();
        let mut hidden = Vec::new();
        for kid in kids {
            if let Some(top) = region {
                let Some(keys) = self.keys(remote, top).await? else {
                    hidden.push(kid);
                    continue;
                };
                let sealed_for = self.sealed_for(remote, top, parent).await?;
                match keys.name(sealed_for, &kid.1) {
                    Some(name) => shown.push(((kid.0, name, kid.2, kid.3), Some(top))),
                    None => {
                        tracing::warn!("e2e: the name of {} does not open", kid.0);
                        hidden.push(kid);
                    }
                }
            } else if self.is_top(kid.0) {
                if self.keys(remote, kid.0).await?.is_some() {
                    let id = kid.0;
                    shown.push((kid, Some(id)));
                } else {
                    self.skip();
                    hidden.push(kid);
                }
            } else {
                shown.push((kid, None));
            }
        }
        Ok((shown, hidden))
    }

    /// The name of something new in `parent`: sealed in an encrypted
    /// folder, where the device checks that no other has it (the server
    /// cannot): a clash is a conflict, as the server's 409 is.
    pub async fn new_name(
        &self,
        state: &State,
        remote: &Remote,
        parent: Uuid,
        name: &str,
    ) -> Result<Named, Error> {
        let region = self.region(state, parent).await?;
        let (Some(top), Some(keys)) = (region, self.require(remote, region).await?) else {
            return Ok(Named {
                send: name.to_string(),
                plain: None,
                keys: None,
            });
        };
        let sealing = self.sealing_for(remote, top, parent).await?;
        let wanted = name.to_lowercase();
        for sibling in state.remote_children(parent).await? {
            if keys
                .name(sealing, &sibling.name)
                .is_some_and(|n| n.to_lowercase() == wanted)
            {
                return Err(Error::Conflict(format!("{name} is there already")));
            }
        }
        Ok(Named {
            send: keys.seal(sealing, name)?,
            plain: Some(name.to_string()),
            keys: Some(keys),
        })
    }

    /// The name a move (or a rename) of `node` into `parent` sends: sealed
    /// again for its (new) parent inside an encrypted folder. None when the
    /// move would cross an encrypted folder's edge, which the server
    /// refuses.
    pub async fn move_name(
        &self,
        state: &State,
        remote: &Remote,
        node: Uuid,
        region: Option<Uuid>,
        parent: Uuid,
        name: &str,
    ) -> Result<Option<Named>, Error> {
        let to = self.region(state, parent).await?;
        let top = region == Some(node);
        let allowed = if top { to.is_none() } else { region == to };
        if !allowed {
            return Ok(None);
        }
        match region.filter(|_| !top) {
            Some(encrypted) => {
                let keys = self
                    .require(remote, region)
                    .await?
                    .expect("a region's keys");
                let sealing = self.sealing_for(remote, encrypted, parent).await?;
                let plain = crate::names::nfc(name);
                Ok(Some(Named {
                    send: keys.seal(sealing, &plain)?,
                    plain: Some(plain),
                    keys: Some(keys),
                }))
            }
            None => Ok(Some(Named {
                send: name.to_string(),
                plain: None,
                keys: None,
            })),
        }
    }

    /// A download decrypted, into another temporary file beside it, hashed
    /// as plain text (as the local file is).
    pub async fn open_download(
        &self,
        keys: Option<Arc<FolderKeys>>,
        downloaded: Downloaded,
    ) -> Result<Downloaded, Error> {
        let Some(keys) = keys else {
            return Ok(downloaded);
        };
        let encrypted = downloaded.temp.clone();
        let plain = encrypted.with_file_name(format!(
            "{}download-{}",
            crate::ignore::OWN_PREFIX,
            Uuid::new_v4().simple()
        ));
        let to = plain.clone();
        let opened = tokio::task::spawn_blocking(move || -> Result<String, Error> {
            let mut from = std::io::BufReader::new(std::fs::File::open(&encrypted)?);
            let file = std::fs::File::create(&to)?;
            let mut out = Hashing {
                inner: std::io::BufWriter::new(file),
                hasher: blake3::Hasher::new(),
            };
            contents::decrypt_io(|g| keys.key(g), &mut from, &mut out).map_err(|e| match e {
                contents::IoError::Io(e) => Error::Io(e),
                contents::IoError::Crypto(e) => {
                    Error::Invalid(format!("A file of an encrypted folder does not open: {e}"))
                }
            })?;
            out.inner.flush()?;
            out.inner.get_ref().sync_all()?;
            Ok(out.hasher.finalize().to_hex().to_string())
        })
        .await
        .map_err(|e| Error::Invalid(e.to_string()))
        .and_then(|r| r);
        let _ = std::fs::remove_file(&downloaded.temp);
        match opened {
            Ok(hash) => Ok(Downloaded {
                temp: plain,
                version: downloaded.version,
                hash,
            }),
            Err(e) => {
                let _ = std::fs::remove_file(&plain);
                Err(e)
            }
        }
    }

    /// What an upload of the local file at `path` sends: the file itself,
    /// or (in an encrypted folder) its encrypted copy in the scratch
    /// folder. The copy of an upload under way is kept, so that it carries
    /// on with the same bytes; else the file is encrypted again.
    pub async fn source(
        &self,
        state: &State,
        keys: Option<&FolderKeys>,
        folder: i64,
        relative: &str,
        path: &Path,
        stat: Stat,
    ) -> Result<Source, Error> {
        let Some(keys) = keys else {
            return Ok(Source {
                path: path.to_path_buf(),
                stat,
                scratch: false,
            });
        };
        let (generation, key) = keys.newest()?;
        let size = contents::encrypted_size(stat.size as u64) as i64;
        let copy = self.scratch.join(format!(
            "{folder}-{}",
            &blake3::hash(relative.as_bytes()).to_hex()[..32]
        ));
        let under_way = state
            .upload(folder, relative)
            .await?
            .is_some_and(|u| u.size == size && u.mtime_ns == stat.mtime_ns);
        let kept = std::fs::metadata(&copy).is_ok_and(|m| m.len() as i64 == size);
        let size = if under_way && kept {
            size
        } else {
            std::fs::create_dir_all(&self.scratch)?;
            let (from, to) = (path.to_path_buf(), copy.clone());
            tokio::task::spawn_blocking(move || -> std::io::Result<u64> {
                let mut reader = std::io::BufReader::new(std::fs::File::open(from)?);
                let mut writer = std::io::BufWriter::new(std::fs::File::create(to)?);
                let written = contents::encrypt_io(
                    &key,
                    generation,
                    &crypto::random_32(),
                    &mut reader,
                    &mut writer,
                )?;
                writer.flush()?;
                Ok(written)
            })
            .await
            .map_err(|e| Error::Invalid(e.to_string()))?? as i64
        };
        Ok(Source {
            path: copy,
            stat: Stat { size, ..stat },
            scratch: true,
        })
    }

    /// An upload is over: its encrypted copy goes, but while it waits for
    /// the server to come back (it carries on then).
    pub fn sent<T>(&self, source: &Source, result: &Result<T, Error>) {
        if source.scratch && !matches!(result, Err(e) if e.waits()) {
            let _ = std::fs::remove_file(&source.path);
        }
    }

    /// A device's round: the account's keys checked (encryption set up;
    /// the private key here still the account's), then what the server
    /// asks of an unlocked device (`GET /encryption/pending`): wrap the
    /// folders' keys for members who lack them, and make a new generation
    /// where one is needed.
    pub async fn round(&self, remote: &Remote) -> Result<Round, Error> {
        let account = match remote.api.get_encryption().await {
            Ok(account) => account.into_inner(),
            // A server without encrypted folders.
            Err(e) => match Error::from_api(e) {
                Error::NotFound => return Ok(Round::default()),
                other => return Err(other),
            },
        };
        let Some(private) = self.private() else {
            return Ok(Round {
                set_up: account.set_up,
                forgotten: false,
            });
        };
        if !account.set_up || account.public_key != Some(crypto::b64(&private.public_key())) {
            self.set_private(None);
            return Ok(Round {
                set_up: account.set_up,
                forgotten: true,
            });
        }
        let tasks = remote
            .api
            .list_encryption_tasks()
            .await
            .map_err(Error::from_api)?
            .into_inner();
        for task in tasks {
            match self.task(remote, &private, &task).await {
                Ok(()) => {}
                Err(e @ (Error::Offline(_) | Error::Unauthorized)) => return Err(e),
                Err(e) => tracing::warn!("e2e: the keys of {}: {e}", task.folder_id),
            }
        }
        Ok(Round {
            set_up: true,
            forgotten: false,
        })
    }

    async fn task(
        &self,
        remote: &Remote,
        private: &PrivateKey,
        task: &types::EncryptionTask,
    ) -> Result<(), Error> {
        let top = task.folder_id;
        let found = remote
            .api
            .get_folder_keys(&top)
            .await
            .map_err(Error::from_api)?
            .into_inner();
        let held: HashMap<i64, [u8; 32]> = found
            .keys
            .iter()
            .filter_map(|w| {
                let generation = u32::try_from(w.generation).ok()?;
                let wrapped = crypto::unb64(&w.wrapped).ok()?;
                let key = folder::unwrap(&wrapped, top, generation, &private.0).ok()?;
                Some((w.generation, key))
            })
            .collect();
        let keys = missing_keys(top, &found.members, &held)?;
        if !keys.is_empty() {
            remote
                .api
                .add_folder_keys(&top, &types::NewFolderKeys { keys })
                .await
                .map_err(Error::from_api)?;
        }
        if found.needs_new_key {
            let generation = found.generation + 1;
            let keys = generation_keys(top, &found.members, generation, &crypto::random_32())?;
            match remote
                .api
                .add_folder_keys(&top, &types::NewFolderKeys { keys })
                .await
                .map_err(Error::from_api)
            {
                // Another device made it first: read again next round.
                Ok(_) | Err(Error::Conflict(_)) => {}
                Err(e) => return Err(e),
            }
            self.cache.lock().unwrap().remove(&top);
        }
        Ok(())
    }
}

/// What a round found.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Round {
    /// The account has set up encryption.
    pub set_up: bool,
    /// The private key here was not the account's (encryption set up again
    /// elsewhere): forgotten, locked.
    pub forgotten: bool,
}

/// The keys for the members waiting for generations this device holds
/// (`held`): each member with a public key, for each generation they lack.
/// A member on another server (`remote`, docs/E2E_SPEC.md §12) is wrapped
/// for as any, by their `user_id` (the share's id there): never looked up
/// as a user here.
fn missing_keys(
    top: Uuid,
    members: &[types::Member],
    held: &HashMap<i64, [u8; 32]>,
) -> Result<Vec<types::MemberKey>, Error> {
    let mut keys = Vec::new();
    for member in members {
        let Some(public) = member_key(member) else {
            continue;
        };
        for generation in &member.missing {
            if let (Some(key), Ok(g)) = (held.get(generation), u32::try_from(*generation)) {
                keys.push(types::MemberKey {
                    user_id: member.user_id,
                    generation: *generation,
                    wrapped: crypto::b64(&wrap(key, top, g, &public)?),
                });
            }
        }
    }
    Ok(keys)
}

/// A new generation's key wrapped for every member with a public key, on
/// another server too (the server refuses one that leaves any out).
fn generation_keys(
    top: Uuid,
    members: &[types::Member],
    generation: i64,
    key: &[u8; 32],
) -> Result<Vec<types::MemberKey>, Error> {
    let g = u32::try_from(generation).map_err(|_| Error::Invalid("Too many generations".into()))?;
    let mut keys = Vec::new();
    for member in members {
        if let Some(public) = member_key(member) {
            keys.push(types::MemberKey {
                user_id: member.user_id,
                generation,
                wrapped: crypto::b64(&wrap(key, top, g, &public)?),
            });
        }
    }
    Ok(keys)
}

fn member_key(member: &types::Member) -> Option<[u8; 32]> {
    let bytes = crypto::unb64(member.public_key.as_deref()?).ok()?;
    bytes.try_into().ok()
}

fn wrap(key: &[u8; 32], top: Uuid, generation: u32, public: &[u8; 32]) -> Result<Vec<u8>, Error> {
    folder::wrap(key, top, generation, public)
        .map_err(|e| Error::Invalid(format!("A member's key is refused: {e}")))
}

/// A writer that hashes what goes through it.
struct Hashing<W: Write> {
    inner: W,
    hasher: blake3::Hasher,
}

impl<W: Write> Write for Hashing<W> {
    fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
        let n = self.inner.write(buf)?;
        self.hasher.update(&buf[..n]);
        Ok(n)
    }

    fn flush(&mut self) -> std::io::Result<()> {
        self.inner.flush()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn keys_of(pairs: &[(u32, [u8; 32])], newest: u32) -> FolderKeys {
        FolderKeys {
            newest,
            keys: pairs.iter().copied().collect(),
        }
    }

    #[test]
    fn names_open_with_the_key_of_their_generation_and_seal_with_the_newest() {
        let (one, two) = (crypto::random_32(), crypto::random_32());
        let keys = keys_of(&[(1, one), (2, two)], 2);
        let parent = Uuid::new_v4();
        let old = names::encrypt(&one, 1, parent, "old.txt").unwrap();
        assert_eq!(keys.name(parent, &old).as_deref(), Some("old.txt"));
        assert_eq!(keys.name(Uuid::new_v4(), &old), None);
        let sealed = keys.seal(parent, "new.txt").unwrap();
        assert_eq!(names::generation(&sealed).unwrap(), 2);
        assert_eq!(keys.name(parent, &sealed).as_deref(), Some("new.txt"));
        // A plain name is no sealed one.
        assert_eq!(keys.name(parent, "plain.txt"), None);
    }

    #[test]
    fn without_the_newest_key_nothing_is_written() {
        let keys = keys_of(&[(1, crypto::random_32())], 2);
        assert!(keys.seal(Uuid::new_v4(), "a.txt").is_err());
        assert!(FolderKeys::default().seal(Uuid::new_v4(), "a.txt").is_err());
    }

    /// A member: on another server when `remote` is given (their address;
    /// `user_id` the share's id), waiting for encryption without a key.
    fn member(public: Option<&[u8; 32]>, missing: &[i64], remote: Option<&str>) -> types::Member {
        types::Member {
            user_id: Uuid::new_v4(),
            email: remote.unwrap_or("bea@example.com").to_string(),
            public_key: public.map(|p| crypto::b64(p)),
            missing: missing.to_vec(),
            remote: remote.map(str::to_string),
        }
    }

    #[test]
    fn the_owner_wraps_for_a_member_on_another_server_by_the_shares_id() {
        let top = Uuid::new_v4();
        let theirs = keys::Identity::generate();
        let remote = member(Some(&theirs.public), &[1, 2], Some("bob@b.example"));
        let waiting = member(None, &[1], Some("eve@c.example"));
        let key = crypto::random_32();
        let held = HashMap::from([(1, key)]);
        let keys = missing_keys(top, &[remote.clone(), waiting], &held).unwrap();
        // Only the generation held, only for who has a public key, by the
        // share's id; it opens with their private key and the owner's folder.
        assert_eq!(keys.len(), 1);
        assert_eq!((keys[0].user_id, keys[0].generation), (remote.user_id, 1));
        let wrapped = crypto::unb64(&keys[0].wrapped).unwrap();
        assert_eq!(
            folder::unwrap(&wrapped, top, 1, &theirs.private).unwrap(),
            key
        );
    }

    #[test]
    fn a_new_generation_includes_the_members_on_other_servers_with_a_key() {
        let top = Uuid::new_v4();
        let (local, theirs) = (keys::Identity::generate(), keys::Identity::generate());
        let members = [
            member(Some(&local.public), &[], None),
            member(Some(&theirs.public), &[], Some("bob@b.example")),
            member(None, &[], Some("eve@c.example")),
        ];
        let key = crypto::random_32();
        let keys = generation_keys(top, &members, 3, &key).unwrap();
        let ids: Vec<Uuid> = keys.iter().map(|k| k.user_id).collect();
        assert_eq!(ids, [members[0].user_id, members[1].user_id]);
        let wrapped = crypto::unb64(&keys[1].wrapped).unwrap();
        assert_eq!(
            folder::unwrap(&wrapped, top, 3, &theirs.private).unwrap(),
            key
        );
    }

    #[test]
    fn a_private_key_round_trips_as_text_and_hides_in_debug() {
        let key = PrivateKey(crypto::random_32());
        assert_eq!(PrivateKey::decode(&key.encode()), Some(key.clone()));
        assert_eq!(PrivateKey::decode("short"), None);
        assert_eq!(format!("{key:?}"), "PrivateKey(..)");
    }
}
