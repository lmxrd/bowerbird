//! The storage layer (docs/M7C_DESIGN.md §4): where a drive's bytes are,
//! for every backend, the server's disk included. Every feature reaches a
//! drive's files through a [`Storage`] (an opendal operator over the drive's
//! folder or prefix), so listing, uploads and downloads, the trash and
//! versions, search and the AI's reading, WebDAV, links and backups' restores
//! work on every backend alike. The index (`nodes`) is the truth for what
//! Organizer writes; a listing (`walk`) finds what was changed outside it.
//!
//! Paths are relative to the drive's root, the names joined with `/` (""
//! for the root). In a bucket a drive is at `<prefix>/<drive id>/`
//! ([`drive_base`]), with its tree as plain objects at their paths and its
//! trash and versions under `.organizer/`, as on disk.
//!
//! On the server's disk a few things go to the filesystem itself, as opendal
//! offers nothing for them: the listing (symbolic links are skipped, never
//! followed), the watcher, a version kept as a hard link, and an upload
//! moved in from the uploads folder by a rename, which is instant and
//! atomic where a copy through the storage layer would write the bytes
//! again. Everything else goes through opendal there too.
//!
//! S3 has no rename: a move is a copy of each object and the delete of the
//! old one ([`Storage::move_objects`]), which carries on where it stopped
//! when run again (the journal does, after a crash).

use crate::files::Kind;
use opendal::{ErrorKind, Operator};
use std::io;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

/// The size of the pieces a file is read, written and copied in.
const PIECE: u64 = 8 * 1024 * 1024;

/// A drive's storage.
#[derive(Clone)]
pub struct Storage {
    op: Operator,
    /// The drive's key prefix: "" where the operator is rooted at the
    /// drive's folder (the disk), `<prefix>/<drive id>/` in a bucket.
    base: String,
    /// The drive's folder on this machine (the server's disk).
    local: Option<PathBuf>,
    /// The backend's health, told of each failure and success (none for the
    /// disk: a missing folder is `FileError::Unavailable` as before).
    health: Option<Arc<Health>>,
    /// Another server's WebDAV (a share from it, docs/M7E_DESIGN.md §5):
    /// a move is a MOVE there; for a shared file, its name in the drive
    /// and its key (the share is the file itself).
    dav: Option<Dav>,
}

/// What a storage on another server's WebDAV is.
#[derive(Clone, Debug)]
pub struct Dav {
    /// A shared file: its name in the drive and its key there.
    pub single: Option<(String, String)>,
}

/// What a listing found: a file or folder of a drive, from its root.
#[derive(Debug)]
pub struct Entry {
    /// The names from the root down, joined with `/`.
    pub path: String,
    pub name: String,
    /// The parent's path ("" for the root's children).
    pub parent: String,
    pub kind: Kind,
    pub size: i64,
    pub mtime_ns: i64,
}

/// Whether a backend answers, and why not: the reason is fit to show (it
/// says nothing of addresses or keys).
#[derive(Debug, Default)]
pub struct Health {
    error: Mutex<Option<String>>,
}

impl Health {
    /// Why the backend fails now; None while it answers.
    pub fn error(&self) -> Option<String> {
        self.error.lock().expect("not poisoned").clone()
    }

    pub fn fail(&self, reason: &str) {
        let mut error = self.error.lock().expect("not poisoned");
        if error.as_deref() != Some(reason) {
            tracing::warn!("a storage backend fails: {reason}");
        }
        *error = Some(reason.to_string());
    }

    pub fn ok(&self) {
        let mut error = self.error.lock().expect("not poisoned");
        if error.is_some() {
            tracing::info!("a storage backend answers again");
            *error = None;
        }
    }
}

/// The backend failed (unreachable, credentials refused): carried in an
/// `io::Error` so that `FileError` can tell it from the disk's errors and
/// answer its reason (`FileError::Down`).
#[derive(Debug)]
pub struct Down(pub String);

impl std::fmt::Display for Down {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.0)
    }
}

impl std::error::Error for Down {}

/// Why a backend failed, in words fit to show to a drive's members.
pub fn reason(error: &opendal::Error) -> &'static str {
    match error.kind() {
        ErrorKind::PermissionDenied => "The storage refused its credentials",
        ErrorKind::ConfigInvalid => "The storage's settings are not valid",
        ErrorKind::RateLimited => "The storage is too busy right now",
        _ => "The storage cannot be reached",
    }
}

/// Where a drive is in a bucket: `<prefix>/<drive id>/`.
pub fn drive_base(prefix: &str, drive: uuid::Uuid) -> String {
    match prefix.trim_matches('/') {
        "" => format!("{drive}/"),
        prefix => format!("{prefix}/{drive}/"),
    }
}

/// `a/b` and `c`: `a/b/c`; the root ("") and `c`: `c`.
pub fn join(parent: &str, name: &str) -> String {
    if parent.is_empty() {
        name.to_string()
    } else {
        format!("{parent}/{name}")
    }
}

impl Storage {
    /// A drive's folder on the server's disk.
    pub fn disk(root: &Path) -> io::Result<Storage> {
        let op = Operator::new(opendal::services::Fs::default().root(&root.to_string_lossy()))
            .map_err(io::Error::other)?;
        Ok(Storage {
            op,
            base: String::new(),
            local: Some(root.to_path_buf()),
            health: None,
            dav: None,
        })
    }

    /// A file of this machine outside every drive (a drop-box item staged
    /// in the data dir) as a storage and its path in it, for the readers
    /// that take any file the same way.
    pub fn local_file(path: &Path) -> io::Result<(Storage, String)> {
        let parent = path.parent().unwrap_or(Path::new("/"));
        let name = path
            .file_name()
            .map(|n| n.to_string_lossy().into_owned())
            .unwrap_or_default();
        Ok((Storage::disk(parent)?, name))
    }

    /// A drive at `base` of a backend's operator.
    pub fn remote(op: Operator, base: String, health: Arc<Health>) -> Storage {
        Storage {
            op,
            base,
            local: None,
            health: Some(health),
            dav: None,
        }
    }

    /// A share from another server, over its WebDAV (an operator rooted
    /// at the shared folder, or at a shared file's parent).
    pub fn webdav(op: Operator, dav: Dav, health: Arc<Health>) -> Storage {
        Storage {
            op,
            base: String::new(),
            local: None,
            health: Some(health),
            dav: Some(dav),
        }
    }

    /// On another server's WebDAV: no trash or versions of ours there.
    pub fn is_remote(&self) -> bool {
        self.dav.is_some()
    }

    /// The ETag of what is at a path (another server's WebDAV says when
    /// anything under a folder changed); None when it gives none.
    pub async fn etag(&self, rel: &str) -> io::Result<Option<String>> {
        let key = if rel.is_empty() && self.single().is_none() {
            self.dir_key(rel)
        } else {
            self.key(rel)
        };
        let meta = self.op.stat(&key).await.map_err(|e| self.io(e))?;
        Ok(self.ok(meta.etag().map(str::to_string)))
    }

    fn single(&self) -> Option<&(String, String)> {
        self.dav.as_ref().and_then(|d| d.single.as_ref())
    }

    /// The drive's folder on this machine, for the server's disk.
    pub fn local_root(&self) -> Option<&Path> {
        self.local.as_deref()
    }

    /// Where a path is on this machine, for the server's disk.
    pub fn local_path(&self, rel: &str) -> Option<PathBuf> {
        let root = self.local.as_ref()?;
        Some(
            rel.split('/')
                .filter(|p| !p.is_empty())
                .fold(root.clone(), |p, n| p.join(n)),
        )
    }

    /// The object key of a file.
    pub fn key(&self, rel: &str) -> String {
        if let Some((name, key)) = self.single()
            && rel == name
        {
            return key.clone();
        }
        format!("{}{rel}", self.base)
    }

    /// The key of a folder (its prefix, with the trailing `/`).
    pub fn dir_key(&self, rel: &str) -> String {
        if rel.is_empty() {
            self.base.clone()
        } else {
            format!("{}{rel}/", self.base)
        }
    }

    /// An opendal error as an `io::Error` of the same kind; a failure of the
    /// backend itself is noted in its health and carried as [`Down`].
    fn io(&self, error: opendal::Error) -> io::Error {
        let kind = match error.kind() {
            ErrorKind::NotFound => io::ErrorKind::NotFound,
            ErrorKind::AlreadyExists => io::ErrorKind::AlreadyExists,
            ErrorKind::IsADirectory | ErrorKind::NotADirectory | ErrorKind::IsSameFile => {
                io::ErrorKind::InvalidInput
            }
            _ => match &self.health {
                Some(health) => {
                    let reason = reason(&error);
                    tracing::debug!("storage error: {error}");
                    health.fail(reason);
                    return io::Error::other(Down(reason.to_string()));
                }
                None => io::ErrorKind::Other,
            },
        };
        io::Error::new(kind, error)
    }

    /// A call that went through: the backend answers.
    fn ok<T>(&self, value: T) -> T {
        if let Some(health) = &self.health {
            health.ok();
        }
        value
    }

    /// Whether the backend answers: a listing of the drive's prefix.
    pub async fn check(&self) -> io::Result<()> {
        if let Some(root) = &self.local {
            if !root.is_dir() {
                return Err(io::Error::new(
                    io::ErrorKind::NotFound,
                    format!("{} does not exist or is not a folder", root.display()),
                ));
            }
            return Ok(());
        }
        let listed = self.op.list_with(&self.base).limit(1).await;
        listed.map(|_| self.ok(())).map_err(|e| self.io(e))
    }

    /// What is at a path: a file, a folder, or nothing.
    pub async fn kind_of(&self, rel: &str) -> io::Result<Option<Kind>> {
        if let Some(path) = self.local_path(rel) {
            return Ok(match std::fs::symlink_metadata(&path) {
                Ok(m) if m.is_dir() => Some(Kind::Folder),
                Ok(_) => Some(Kind::File),
                Err(e) if e.kind() == io::ErrorKind::NotFound => None,
                Err(e) => return Err(e),
            });
        }
        if !rel.is_empty() {
            match self.op.stat(&self.key(rel)).await {
                Ok(meta) if meta.is_file() => return Ok(self.ok(Some(Kind::File))),
                Ok(_) => return Ok(self.ok(Some(Kind::Folder))),
                Err(e) if e.kind() == ErrorKind::NotFound => {}
                Err(e) => return Err(self.io(e)),
            }
        }
        // A folder on S3 is its marker, or only the objects under it.
        let under = self
            .op
            .list_with(&self.dir_key(rel))
            .limit(1)
            .await
            .map_err(|e| self.io(e))?;
        Ok(self.ok((!under.is_empty()).then_some(Kind::Folder)))
    }

    pub async fn exists(&self, rel: &str) -> io::Result<bool> {
        Ok(self.kind_of(rel).await?.is_some())
    }

    /// A file's size and modification time (nanoseconds since 1970).
    pub async fn file_meta(&self, rel: &str) -> io::Result<(i64, i64)> {
        if let Some(path) = self.local_path(rel) {
            let meta = std::fs::metadata(path)?;
            let size = if meta.is_file() { meta.len() as i64 } else { 0 };
            return Ok((size, crate::files::mtime_ns(&meta)));
        }
        let meta = self.op.stat(&self.key(rel)).await.map_err(|e| self.io(e))?;
        Ok(self.ok((meta.content_length() as i64, mtime_of(&meta))))
    }

    /// A new folder; `AlreadyExists` when something is there.
    pub async fn create_dir(&self, rel: &str) -> io::Result<()> {
        if let Some(path) = self.local_path(rel) {
            return std::fs::create_dir(path);
        }
        if self.exists(rel).await? {
            return Err(io::Error::new(
                io::ErrorKind::AlreadyExists,
                "something has that name",
            ));
        }
        let made = self.op.create_dir(&self.dir_key(rel)).await;
        made.map(|()| self.ok(())).map_err(|e| self.io(e))
    }

    /// Whether a move is a rename (the disk) or copies every object (S3).
    pub fn renames(&self) -> bool {
        self.local.is_some() || self.dav.is_some()
    }

    /// Move a file or a folder (with everything in it) within the drive.
    pub async fn rename(&self, from: &str, to: &str) -> io::Result<()> {
        if let (Some(from), Some(to)) = (self.local_path(from), self.local_path(to)) {
            return crate::files::move_path(&from, &to);
        }
        // WebDAV moves a whole collection with one MOVE.
        if self.dav.is_some() {
            if self.single().is_some() {
                return Err(io::Error::new(
                    io::ErrorKind::InvalidInput,
                    "a shared file cannot be moved",
                ));
            }
            let (from, to) = match self.kind_of(from).await? {
                Some(Kind::Folder) => (self.dir_key(from), self.dir_key(to)),
                Some(Kind::File) => (self.key(from), self.key(to)),
                None => return Err(io::Error::new(io::ErrorKind::NotFound, "nothing there")),
            };
            let moved = self.op.rename(&from, &to).await;
            return moved.map(|()| self.ok(())).map_err(|e| self.io(e));
        }
        self.move_objects(from, to).await
    }

    /// Move what is at `from` to `to` object by object: each one copied,
    /// then the original deleted. Run again after a stop, it carries on
    /// with what is left (nothing left: done).
    pub async fn move_objects(&self, from: &str, to: &str) -> io::Result<()> {
        match self.op.stat(&self.key(from)).await {
            Ok(meta) if meta.is_file() => {
                self.copy_object(&self.key(from), &self.key(to)).await?;
                let deleted = self.op.delete(&self.key(from)).await;
                return deleted.map(|()| self.ok(())).map_err(|e| self.io(e));
            }
            Ok(_) => {}
            Err(e) if e.kind() == ErrorKind::NotFound => {}
            Err(e) => return Err(self.io(e)),
        }
        let (from_dir, to_dir) = (self.dir_key(from), self.dir_key(to));
        let entries = self
            .op
            .list_with(&from_dir)
            .recursive(true)
            .await
            .map_err(|e| self.io(e))?;
        if entries.is_empty() {
            return Ok(());
        }
        // The new folder first: an empty one is only its marker.
        self.op.create_dir(&to_dir).await.map_err(|e| self.io(e))?;
        for entry in entries {
            let Some(rest) = entry.path().strip_prefix(from_dir.as_str()) else {
                continue;
            };
            if rest.is_empty() {
                continue;
            }
            let target = format!("{to_dir}{rest}");
            if entry.metadata().is_dir() {
                self.op.create_dir(&target).await.map_err(|e| self.io(e))?;
            } else {
                self.copy_object(entry.path(), &target).await?;
            }
            self.op.delete(entry.path()).await.map_err(|e| self.io(e))?;
        }
        let deleted = self.op.delete(&from_dir).await;
        deleted.map(|()| self.ok(())).map_err(|e| self.io(e))
    }

    /// Copy one object within the backend: the backend's own copy where it
    /// has one (S3), else read and written again.
    async fn copy_object(&self, from: &str, to: &str) -> io::Result<()> {
        if self.op.info().capability().copy {
            let copied = self.op.copy(from, to).await;
            return copied.map(|_| self.ok(())).map_err(|e| self.io(e));
        }
        copy_between(&self.op, from, &self.op, to)
            .await
            .map_err(|e| self.io(e))
    }

    /// Delete a file or a folder with everything in it; nothing there is
    /// `NotFound`.
    pub async fn remove_all(&self, rel: &str) -> io::Result<()> {
        if let Some(path) = self.local_path(rel) {
            return if path.is_dir() {
                std::fs::remove_dir_all(path)
            } else {
                std::fs::remove_file(path)
            };
        }
        match self.kind_of(rel).await? {
            None => Err(io::Error::new(io::ErrorKind::NotFound, "nothing there")),
            Some(Kind::File) => {
                let deleted = self.op.delete(&self.key(rel)).await;
                deleted.map(|()| self.ok(())).map_err(|e| self.io(e))
            }
            Some(Kind::Folder) => {
                let deleted = self
                    .op
                    .delete_with(&self.dir_key(rel))
                    .recursive(true)
                    .await;
                deleted.map(|()| self.ok(())).map_err(|e| self.io(e))
            }
        }
    }

    /// Put a file received on this machine (in the uploads folder) at a
    /// path, over what is there; the local file is gone afterwards.
    pub async fn put_file(&self, temp: &Path, rel: &str) -> io::Result<()> {
        if let Some(path) = self.local_path(rel) {
            return crate::files::move_path(temp, &path);
        }
        self.upload(temp, &self.key(rel)).await?;
        tokio::fs::remove_file(temp).await
    }

    /// Put a folder of this machine (a folder dropped into the drop box, a
    /// restored one) at a path, whole; the local one is gone afterwards.
    pub async fn put_dir(&self, dir: &Path, rel: &str) -> io::Result<()> {
        if let Some(path) = self.local_path(rel) {
            return crate::files::move_path(dir, &path);
        }
        self.op
            .create_dir(&self.dir_key(rel))
            .await
            .map_err(|e| self.io(e))?;
        let mut folders = vec![(dir.to_path_buf(), rel.to_string())];
        while let Some((local, at)) = folders.pop() {
            for entry in std::fs::read_dir(&local)? {
                let entry = entry?;
                let Some(name) = entry.file_name().to_str().map(String::from) else {
                    continue;
                };
                let meta = std::fs::symlink_metadata(entry.path())?;
                let target = join(&at, &name);
                if meta.is_dir() {
                    self.op
                        .create_dir(&self.dir_key(&target))
                        .await
                        .map_err(|e| self.io(e))?;
                    folders.push((entry.path(), target));
                } else if meta.is_file() {
                    self.upload(&entry.path(), &self.key(&target)).await?;
                }
            }
        }
        self.ok(());
        tokio::fs::remove_dir_all(dir).await
    }

    /// Move a file or folder out of the drive to this machine (the drop box
    /// taking an item back).
    pub async fn take_out(&self, rel: &str, to: &Path) -> io::Result<()> {
        if let Some(path) = self.local_path(rel) {
            return crate::files::move_path(&path, to);
        }
        match self.kind_of(rel).await? {
            None => return Err(io::Error::new(io::ErrorKind::NotFound, "nothing there")),
            Some(Kind::File) => self.download(&self.key(rel), to).await?,
            Some(Kind::Folder) => {
                let dir = self.dir_key(rel);
                std::fs::create_dir_all(to)?;
                let entries = self
                    .op
                    .list_with(&dir)
                    .recursive(true)
                    .await
                    .map_err(|e| self.io(e))?;
                for entry in entries {
                    let Some(rest) = entry.path().strip_prefix(dir.as_str()) else {
                        continue;
                    };
                    let target = rest
                        .split('/')
                        .filter(|p| !p.is_empty())
                        .fold(to.to_path_buf(), |p, n| p.join(n));
                    if entry.metadata().is_dir() {
                        std::fs::create_dir_all(&target)?;
                    } else {
                        if let Some(parent) = target.parent() {
                            std::fs::create_dir_all(parent)?;
                        }
                        self.download(entry.path(), &target).await?;
                    }
                }
            }
        }
        self.remove_all(rel).await
    }

    /// Keep a copy of a file at another path of the drive (a version): a
    /// hard link on the disk where it can be, a copy elsewhere.
    pub async fn keep_copy(&self, from: &str, to: &str) -> io::Result<()> {
        if let (Some(from), Some(to)) = (self.local_path(from), self.local_path(to)) {
            if let Some(parent) = to.parent() {
                std::fs::create_dir_all(parent)?;
            }
            return match std::fs::hard_link(&from, &to) {
                Ok(()) => Ok(()),
                Err(_) => std::fs::copy(&from, &to).map(|_| ()),
            };
        }
        self.copy_object(&self.key(from), &self.key(to)).await
    }

    /// Copy a file of the drive to this machine (a version restored goes
    /// through the uploads folder as any write).
    pub async fn copy_out(&self, rel: &str, to: &Path) -> io::Result<()> {
        if let Some(path) = self.local_path(rel) {
            return tokio::fs::copy(path, to).await.map(|_| ());
        }
        self.download(&self.key(rel), to).await
    }

    /// A reader of a file, and its size.
    pub async fn reader(&self, rel: &str) -> io::Result<(opendal::Reader, u64)> {
        let key = self.key(rel);
        let meta = self.op.stat(&key).await.map_err(|e| self.io(e))?;
        if meta.is_dir() {
            return Err(io::Error::new(io::ErrorKind::InvalidInput, "a folder"));
        }
        let reader = self.op.reader(&key).await.map_err(|e| self.io(e))?;
        Ok(self.ok((reader, meta.content_length())))
    }

    /// A file's bytes, read whole (the callers check its size first).
    pub async fn read(&self, rel: &str) -> io::Result<Vec<u8>> {
        if let Some(path) = self.local_path(rel) {
            return tokio::fs::read(path).await;
        }
        let read = self.op.read(&self.key(rel)).await;
        read.map(|b| self.ok(b.to_vec())).map_err(|e| self.io(e))
    }

    /// The BLAKE3 hash of a file; None when it cannot be read.
    pub async fn hash(&self, rel: &str) -> Option<Vec<u8>> {
        if let Some(path) = self.local_path(rel) {
            return crate::files::scan::hash_file(&path).await;
        }
        let (reader, size) = self.reader(rel).await.ok()?;
        let mut hasher = blake3::Hasher::new();
        let mut at = 0;
        while at < size {
            let end = (at + PIECE).min(size);
            let piece = reader.read(at..end).await.map_err(|e| self.io(e)).ok()?;
            for chunk in piece {
                hasher.update(&chunk);
            }
            at = end;
        }
        Some(hasher.finalize().as_bytes().to_vec())
    }

    /// The names in a folder (the versions' sweep).
    pub async fn names_in(&self, rel: &str) -> io::Result<Vec<String>> {
        if let Some(path) = self.local_path(rel) {
            return match std::fs::read_dir(path) {
                Ok(entries) => Ok(entries
                    .flatten()
                    .filter_map(|e| e.file_name().to_str().map(String::from))
                    .collect()),
                Err(e) if e.kind() == io::ErrorKind::NotFound => Ok(Vec::new()),
                Err(e) => Err(e),
            };
        }
        let dir = self.dir_key(rel);
        let entries = self.op.list(&dir).await.map_err(|e| self.io(e))?;
        Ok(self.ok(entries
            .iter()
            .filter_map(|e| e.path().strip_prefix(dir.as_str()))
            .map(|n| n.trim_end_matches('/').to_string())
            .filter(|n| !n.is_empty())
            .collect()))
    }

    /// Every file and folder of the drive, parents before their children,
    /// but Organizer's own folder.
    pub async fn walk(&self) -> io::Result<Vec<Entry>> {
        if let Some(root) = &self.local {
            let root = root.clone();
            return tokio::task::spawn_blocking(move || walk_local(&root))
                .await
                .map_err(io::Error::other)?;
        }
        // A shared file is the drive's one file.
        if let Some((name, key)) = self.single() {
            let meta = self.op.stat(key).await.map_err(|e| self.io(e))?;
            return Ok(self.ok(entries_of_objects(vec![(
                name.clone(),
                Some((meta.content_length() as i64, mtime_of(&meta))),
            )])));
        }
        let listed = self
            .op
            .list_with(&self.base)
            .recursive(true)
            .await
            .map_err(|e| self.io(e))?;
        let mut objects = Vec::with_capacity(listed.len());
        for entry in listed {
            let Some(rel) = entry.path().strip_prefix(self.base.as_str()) else {
                continue;
            };
            let meta = entry.metadata();
            if meta.is_dir() {
                objects.push((rel.trim_end_matches('/').to_string(), None));
                continue;
            }
            // Not every listing says the size and time: asked then.
            let (size, mtime) = if meta.last_modified().is_some() {
                (meta.content_length() as i64, mtime_of(meta))
            } else {
                self.file_meta(rel).await?
            };
            objects.push((rel.to_string(), Some((size, mtime))));
        }
        self.ok(());
        Ok(entries_of_objects(objects))
    }

    /// Copy every object of this drive (its trash and versions too) to
    /// another storage, but what is there already with the same size: a
    /// drive moving to another backend, carried on after a stop.
    pub async fn copy_all_to(&self, to: &Storage) -> io::Result<u64> {
        let listed = self
            .op
            .list_with(&self.base)
            .recursive(true)
            .await
            .map_err(|e| self.io(e))?;
        let mut copied = 0;
        for entry in listed {
            let Some(rel) = entry.path().strip_prefix(self.base.as_str()) else {
                continue;
            };
            if rel.is_empty() {
                continue;
            }
            if entry.metadata().is_dir() {
                let rel = rel.trim_end_matches('/');
                if !to.exists(rel).await? {
                    to.op
                        .create_dir(&to.dir_key(rel))
                        .await
                        .map_err(|e| to.io(e))?;
                }
                continue;
            }
            let size = self
                .op
                .stat(entry.path())
                .await
                .map_err(|e| self.io(e))?
                .content_length();
            match to.op.stat(&to.key(rel)).await {
                Ok(meta) if meta.content_length() == size => continue,
                _ => {}
            }
            copy_between(&self.op, entry.path(), &to.op, &to.key(rel))
                .await
                .map_err(|e| to.io(e))?;
            copied += size;
        }
        Ok(copied)
    }

    /// Delete everything of the drive here (a drive moved away).
    pub async fn remove_drive(&self) -> io::Result<()> {
        if let Some(root) = &self.local {
            return match std::fs::remove_dir_all(root) {
                Err(e) if e.kind() != io::ErrorKind::NotFound => Err(e),
                _ => Ok(()),
            };
        }
        let deleted = self.op.delete_with(&self.base).recursive(true).await;
        deleted.map(|()| self.ok(())).map_err(|e| self.io(e))
    }

    /// Upload a local file to a key, streamed in pieces (a multipart
    /// upload on S3).
    async fn upload(&self, local: &Path, key: &str) -> io::Result<()> {
        use tokio::io::AsyncReadExt;
        // WebDAV takes a file in one PUT, read whole: opendal's WebDAV
        // writer (0.58) is a `OneShotWriter`, which keeps every piece
        // written in memory and refuses a second `write`, and its PUT takes
        // that one `Buffer`; WebDAV itself has no multipart or append to
        // send it in pieces. Streaming it would mean a PUT of our own beside
        // opendal (its token, its errors, its health): not worth it while
        // shares from other servers are the only WebDAV storage.
        if self.dav.is_some() {
            let bytes = tokio::fs::read(local).await?;
            let written = self.op.write(key, bytes).await;
            return written.map(|_| self.ok(())).map_err(|e| self.io(e));
        }
        let mut file = tokio::fs::File::open(local).await?;
        let mut writer = self
            .op
            .writer_with(key)
            .chunk(PIECE as usize)
            .await
            .map_err(|e| self.io(e))?;
        loop {
            let mut piece = Vec::with_capacity(PIECE as usize);
            let read = (&mut file).take(PIECE).read_to_end(&mut piece).await?;
            if read == 0 {
                break;
            }
            if let Err(e) = writer.write(piece).await {
                let _ = writer.abort().await;
                return Err(self.io(e));
            }
        }
        writer.close().await.map_err(|e| self.io(e))?;
        self.ok(());
        Ok(())
    }

    /// Download a key to a local file.
    async fn download(&self, key: &str, to: &Path) -> io::Result<()> {
        use tokio::io::AsyncWriteExt;
        let size = self
            .op
            .stat(key)
            .await
            .map_err(|e| self.io(e))?
            .content_length();
        let reader = self.op.reader(key).await.map_err(|e| self.io(e))?;
        let mut file = tokio::fs::File::create(to).await?;
        let mut at = 0;
        while at < size {
            let end = (at + PIECE).min(size);
            let piece = reader.read(at..end).await.map_err(|e| self.io(e))?;
            for chunk in piece {
                file.write_all(&chunk).await?;
            }
            at = end;
        }
        file.flush().await?;
        self.ok(());
        Ok(())
    }
}

/// Copy an object from one operator to another, in pieces.
async fn copy_between(
    from: &Operator,
    from_key: &str,
    to: &Operator,
    to_key: &str,
) -> opendal::Result<()> {
    let size = from.stat(from_key).await?.content_length();
    let reader = from.reader(from_key).await?;
    let mut writer = to.writer_with(to_key).chunk(PIECE as usize).await?;
    let mut at = 0;
    while at < size {
        let end = (at + PIECE).min(size);
        let piece = reader.read(at..end).await?;
        if let Err(e) = writer.write(piece).await {
            let _ = writer.abort().await;
            return Err(e);
        }
        at = end;
    }
    writer.close().await?;
    Ok(())
}

/// The modification time of an object, in nanoseconds since 1970 (0 when
/// the backend does not say).
fn mtime_of(meta: &opendal::Metadata) -> i64 {
    meta.last_modified()
        .map(|t| t.into_inner().as_nanosecond() as i64)
        .unwrap_or(0)
}

/// What a bucket's listing holds, as the tree's entries: a folder for each
/// marker and each prefix an object is under (S3 has no folders of its
/// own), Organizer's own folder and names that cannot be left out. Each
/// object is its path from the drive's root, with its size and time (a
/// folder's marker has none).
pub fn entries_of_objects(objects: Vec<(String, Option<(i64, i64)>)>) -> Vec<Entry> {
    use std::collections::BTreeMap;
    let mut found: BTreeMap<String, Option<(i64, i64)>> = BTreeMap::new();
    for (path, meta) in objects {
        let parts: Vec<&str> = path.split('/').collect();
        // Empty names (`a//b`, a key ending in `/` that is not a marker) and
        // `.`/`..` are not paths of a tree.
        if path.is_empty()
            || parts
                .iter()
                .any(|p| p.is_empty() || *p == "." || *p == "..")
        {
            continue;
        }
        if parts[0] == crate::files::ORGANIZER_DIR {
            continue;
        }
        // A file and a folder of the same name (`x` and `x/y`): the folder
        // wins, as its files would have nowhere to go.
        for depth in 1..parts.len() {
            found.insert(parts[..depth].join("/"), None);
        }
        match meta {
            Some(meta) => {
                found.entry(path).or_insert(Some(meta));
            }
            None => {
                found.insert(path, None);
            }
        }
    }
    let mut entries: Vec<Entry> = found
        .into_iter()
        .map(|(path, meta)| {
            let (parent, name) = match path.rsplit_once('/') {
                Some((parent, name)) => (parent.to_string(), name.to_string()),
                None => (String::new(), path.clone()),
            };
            let (kind, size, mtime_ns) = match meta {
                Some((size, mtime)) => (Kind::File, size, mtime),
                None => (Kind::Folder, 0, 0),
            };
            Entry {
                path,
                name,
                parent,
                kind,
                size,
                mtime_ns,
            }
        })
        .collect();
    entries.sort_by(|a, b| {
        a.path
            .matches('/')
            .count()
            .cmp(&b.path.matches('/').count())
            .then(a.path.cmp(&b.path))
    });
    entries
}

/// Every file and folder under a folder of this machine, parents before
/// their children. Symbolic links are not followed (they could leave the
/// tree, or loop) and not indexed; names that are not valid UTF-8 are
/// skipped with a warning.
fn walk_local(root: &Path) -> io::Result<Vec<Entry>> {
    if !root.is_dir() {
        return Err(io::Error::new(
            io::ErrorKind::NotFound,
            format!("{} does not exist or is not a folder", root.display()),
        ));
    }
    let mut entries = Vec::new();
    let mut folders: Vec<(PathBuf, String)> = vec![(root.to_path_buf(), String::new())];
    while let Some((dir, prefix)) = folders.pop() {
        let mut children: Vec<_> = std::fs::read_dir(&dir)?.collect::<Result<_, _>>()?;
        children.sort_by_key(|c| c.file_name());
        for child in children {
            let Some(name) = child.file_name().to_str().map(String::from) else {
                tracing::warn!(
                    "skipped {}: the name is not valid UTF-8",
                    child.path().display()
                );
                continue;
            };
            let metadata = match std::fs::symlink_metadata(child.path()) {
                Ok(metadata) => metadata,
                // Gone since the listing: the next scan settles it.
                Err(e) if e.kind() == io::ErrorKind::NotFound => continue,
                Err(e) => return Err(e),
            };
            if metadata.is_symlink() {
                continue;
            }
            // Organizer's own folder (the trash, the versions) is the
            // drive's, never one of its files.
            if prefix.is_empty() && name == crate::files::ORGANIZER_DIR {
                continue;
            }
            let path = join(&prefix, &name);
            let kind = if metadata.is_dir() {
                Kind::Folder
            } else if metadata.is_file() {
                Kind::File
            } else {
                // Sockets, devices, pipes: not files anyone stores.
                continue;
            };
            entries.push(Entry {
                path: path.clone(),
                name,
                parent: prefix.clone(),
                kind,
                size: if kind == Kind::File {
                    metadata.len() as i64
                } else {
                    0
                },
                mtime_ns: crate::files::mtime_ns(&metadata),
            });
            if kind == Kind::Folder {
                folders.push((child.path(), path));
            }
        }
    }
    // Parents before children: a parent's path is a prefix of its
    // children's, so it sorts first by depth.
    entries.sort_by_key(|e| e.path.matches('/').count());
    Ok(entries)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// opendal's in-memory service (built in through rustic_backend's
    /// features, not ours): like S3, no rename, folders as markers; it
    /// has no copy either, so the read-and-write copy is the one used.
    fn memory() -> Operator {
        Operator::new(opendal::services::Memory::default()).unwrap()
    }

    fn remote() -> Storage {
        Storage::remote(
            memory(),
            drive_base("org", uuid::Uuid::nil()),
            Arc::new(Health::default()),
        )
    }

    #[test]
    fn drives_are_at_their_id_under_the_prefix() {
        let drive = uuid::Uuid::nil();
        assert_eq!(drive_base("", drive), format!("{drive}/"));
        assert_eq!(drive_base("/org/", drive), format!("org/{drive}/"));
        assert_eq!(drive_base("a/b", drive), format!("a/b/{drive}/"));
        let storage = remote();
        assert_eq!(storage.key("a/b.txt"), format!("org/{drive}/a/b.txt"));
        assert_eq!(storage.dir_key("a"), format!("org/{drive}/a/"));
        assert_eq!(storage.dir_key(""), format!("org/{drive}/"));
    }

    #[test]
    fn a_listing_of_keys_is_a_tree() {
        let entries = entries_of_objects(vec![
            ("Docs".into(), None),
            ("Photos/2024/a.jpg".into(), Some((3, 7))),
            ("readme.md".into(), Some((1, 1))),
            (".organizer/trash/x".into(), Some((1, 1))),
            ("bad//name".into(), Some((1, 1))),
            ("x".into(), Some((1, 1))),
            ("x/y".into(), Some((2, 2))),
        ]);
        let got: Vec<(&str, Kind, &str)> = entries
            .iter()
            .map(|e| (e.path.as_str(), e.kind, e.parent.as_str()))
            .collect();
        assert_eq!(
            got,
            vec![
                ("Docs", Kind::Folder, ""),
                ("Photos", Kind::Folder, ""),
                ("readme.md", Kind::File, ""),
                ("x", Kind::Folder, ""),
                ("Photos/2024", Kind::Folder, "Photos"),
                ("x/y", Kind::File, "x"),
                ("Photos/2024/a.jpg", Kind::File, "Photos/2024"),
            ]
        );
        let photo = entries.iter().find(|e| e.name == "a.jpg").unwrap();
        assert_eq!((photo.size, photo.mtime_ns), (3, 7));
    }

    #[tokio::test]
    async fn a_folder_moves_object_by_object_and_carries_on_after_a_stop() {
        let storage = remote();
        storage.create_dir("A").await.unwrap();
        for (path, content) in [("A/one", "1"), ("A/sub/two", "22"), ("A/sub/three", "333")] {
            storage
                .op
                .write(&storage.key(path), content.as_bytes().to_vec())
                .await
                .unwrap();
        }
        // A stop part-way: one object is at the new place already, its
        // original gone.
        storage
            .copy_object(&storage.key("A/one"), &storage.key("B/one"))
            .await
            .unwrap();
        storage.op.delete(&storage.key("A/one")).await.unwrap();

        storage.move_objects("A", "B").await.unwrap();
        assert!(!storage.exists("A").await.unwrap());
        assert_eq!(storage.kind_of("B").await.unwrap(), Some(Kind::Folder));
        assert_eq!(storage.read("B/one").await.unwrap(), b"1");
        assert_eq!(storage.read("B/sub/three").await.unwrap(), b"333");
        // Done already: nothing to do.
        storage.move_objects("A", "B").await.unwrap();

        let walked: Vec<String> = storage
            .walk()
            .await
            .unwrap()
            .into_iter()
            .map(|e| e.path)
            .collect();
        assert_eq!(walked, ["B", "B/one", "B/sub", "B/sub/three", "B/sub/two"]);
        // A file alone.
        storage.move_objects("B/one", "B/uno").await.unwrap();
        assert!(!storage.exists("B/one").await.unwrap());
        assert_eq!(
            storage.hash("B/uno").await,
            Some(blake3::hash(b"1").as_bytes().to_vec())
        );
    }

    #[tokio::test]
    async fn a_drive_is_copied_to_other_storage_once() {
        let from = remote();
        from.create_dir("A").await.unwrap();
        from.op
            .write(&from.key("A/f"), b"abc".to_vec())
            .await
            .unwrap();
        from.op
            .write(&from.key(".organizer/trash/t"), b"t".to_vec())
            .await
            .unwrap();
        let dir = std::env::temp_dir().join(format!("organizer-storage-{}", uuid::Uuid::now_v7()));
        std::fs::create_dir_all(&dir).unwrap();
        let to = Storage::disk(&dir).unwrap();
        assert_eq!(from.copy_all_to(&to).await.unwrap(), 4);
        assert_eq!(std::fs::read(dir.join("A/f")).unwrap(), b"abc");
        assert_eq!(std::fs::read(dir.join(".organizer/trash/t")).unwrap(), b"t");
        // Again: what is there already is not copied twice.
        assert_eq!(from.copy_all_to(&to).await.unwrap(), 0);
        std::fs::remove_dir_all(dir).unwrap();
    }

    #[tokio::test]
    async fn a_failing_backend_says_why_and_recovers() {
        let health = Arc::new(Health::default());
        let storage = Storage::remote(memory(), String::new(), health.clone());
        let e = storage.io(opendal::Error::new(ErrorKind::PermissionDenied, "403"));
        assert_eq!(
            health.error().as_deref(),
            Some("The storage refused its credentials")
        );
        assert!(e.get_ref().unwrap().downcast_ref::<Down>().is_some());
        storage.check().await.unwrap();
        assert_eq!(health.error(), None);
    }
}
