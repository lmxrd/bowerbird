//! WebDAV at `/dav/` (docs/M1_DESIGN.md §2.5): the user's tree, for Finder,
//! Explorer, DAVx5, rclone and the like, authenticated with an app password
//! (Basic authentication; any user name).
//!
//! dav-server speaks the protocol; this is its file system. Reads (listings,
//! metadata, downloads) go to dav-server's `LocalFs` over the files root.
//! Every change goes through the journaled operations of `files::ops`, as
//! the REST API's do: a new folder is `create_folder`, a delete goes to the
//! trash, a MOVE is `move_node` (the id stays), and a PUT is written to the
//! uploads folder and placed in the tree whole when it is done. A COPY is
//! made by `LocalFs`, then indexed by a scan.
//!
//! The shares the user placed in their files (docs/M6B_DESIGN.md §2) are
//! folders here too: a path that goes through one is read from the
//! owner's tree (on the server's disk through `LocalFs`; in a storage
//! backend, docs/M7C_DESIGN.md, through the index and the storage layer),
//! and changed through the owner's tree like any change through the API,
//! after the share's permission is checked.
//!
//! End-to-end-encrypted folders (docs/E2E_SPEC.md) are not here at all:
//! a WebDAV client cannot decrypt them. They are left out of listings,
//! and a path into one (or a MOVE or COPY to one) is not found.
//!
//! Not in the OpenAPI document: WebDAV's methods (PROPFIND, MKCOL, MOVE,
//! ...) are not HTTP APIs a client can be generated for.

use crate::auth::{KeyKind, Scope, key_user};
use crate::files::ops::WriteTarget;
use crate::files::shares::Need;
use crate::files::{FileError, Files, Kind};
use crate::state::AppState;
use axum::body::Body;
use axum::extract::{Request, State};
use axum::http::{HeaderValue, StatusCode, header};
use axum::response::{IntoResponse, Response};
use dav_server::davpath::DavPath;
use dav_server::fs::{
    DavDirEntry, DavFile, DavFileSystem, DavMetaData, FsError, FsFuture, FsResult, FsStream,
    OpenOptions, ReadDirMeta,
};
use dav_server::localfs::LocalFs;
use dav_server::memls::MemLs;
use std::io::{Read, Seek, SeekFrom, Write};
use std::path::{Path, PathBuf};
use std::sync::{Arc, OnceLock};
use uuid::Uuid;

/// Where WebDAV is mounted.
pub const PREFIX: &str = "/dav";

/// The locks of WebDAV clients (LOCK, UNLOCK): in memory, one server.
pub(crate) fn locks() -> Box<MemLs> {
    static LOCKS: OnceLock<Box<MemLs>> = OnceLock::new();
    LOCKS.get_or_init(MemLs::new).clone()
}

/// A WebDAV request: the app password, then dav-server over the user's
/// tree.
pub async fn handle(State(state): State<AppState>, request: Request) -> Response {
    let Some((user_id, scope)) = authenticate(&state, request.headers()).await else {
        return (
            StatusCode::UNAUTHORIZED,
            [(
                header::WWW_AUTHENTICATE,
                HeaderValue::from_static("Basic realm=\"Organizer\", charset=\"UTF-8\""),
            )],
            "An app password is required (Settings → App passwords)",
        )
            .into_response();
    };
    let read_only = matches!(
        request.method().as_str(),
        "GET" | "HEAD" | "OPTIONS" | "PROPFIND"
    );
    if scope == Scope::Read && !read_only {
        return (StatusCode::FORBIDDEN, "This app password can only read").into_response();
    }
    let tree = match state.files.tree(user_id).await {
        Ok(tree) => tree,
        Err(e) => return crate::error::HttpError::from(e).into_response(),
    };
    // My drive is always on the server's disk (docs/M7C_DESIGN.md §1).
    let Some(root) = tree.storage.local_root().map(Path::to_path_buf) else {
        return StatusCode::SERVICE_UNAVAILABLE.into_response();
    };
    let fs = TreeFs {
        files: state.files.clone(),
        user_id,
        local: LocalFs::new(&root, false, false, false),
        root,
    };
    // Encrypted folders are hidden: nothing reaches into one; nor into what
    // limited access or upload only hides from the user.
    let destination = request
        .headers()
        .get("destination")
        .and_then(|v| v.to_str().ok())
        .and_then(destination_path);
    for path in [Some(request.uri().path().to_string()), destination]
        .into_iter()
        .flatten()
    {
        if fs.hidden(&path).await {
            return StatusCode::NOT_FOUND.into_response();
        }
    }
    // A folder is deleted whole, as through the API: dav-server would
    // delete what is in it one by one, walking into the shares placed in
    // it, and delete the owner's files where the recipient only meant to
    // take the share out of their files.
    if request.method() == axum::http::Method::DELETE
        && let Some(response) = delete_folder(&fs, request.uri().path()).await
    {
        return response;
    }
    let handler = dav_server::DavHandler::builder()
        .filesystem(Box::new(fs))
        .locksystem(locks())
        .strip_prefix(PREFIX)
        .hide_symlinks(true)
        .build_handler();
    handler.handle(request).await.map(Body::new)
}

/// A DELETE of a folder (None: not one, dav-server answers): the folder
/// goes to its owner's trash whole, or a placed share leaves the user's
/// files.
async fn delete_folder(fs: &TreeFs, uri_path: &str) -> Option<Response> {
    let path = DavPath::new(uri_path.strip_prefix(PREFIX)?).ok()?;
    let node = fs.node(&nfc(&path)).await.ok()?;
    if node.kind != Kind::Folder {
        return None;
    }
    let status = match fs.delete(&nfc(&path)).await {
        Ok(()) => StatusCode::NO_CONTENT,
        Err(FsError::NotFound) => StatusCode::NOT_FOUND,
        Err(FsError::Forbidden) => StatusCode::FORBIDDEN,
        Err(FsError::Exists) => StatusCode::CONFLICT,
        Err(_) => StatusCode::INTERNAL_SERVER_ERROR,
    };
    Some(status.into_response())
}

/// The path of a `Destination` header (an absolute URL, or a path).
pub(crate) fn destination_path(value: &str) -> Option<String> {
    if value.starts_with('/') {
        return Some(value.to_string());
    }
    let rest = &value[value.find("://")? + 3..];
    Some(rest[rest.find('/')?..].to_string())
}

/// The user and the scope of the Basic authentication's password: an app
/// password, nothing else.
pub(crate) async fn authenticate(
    state: &AppState,
    headers: &axum::http::HeaderMap,
) -> Option<(Uuid, Scope)> {
    let value = headers.get(header::AUTHORIZATION)?.to_str().ok()?;
    let (scheme, encoded) = value.split_once(' ')?;
    if !scheme.eq_ignore_ascii_case("basic") {
        return None;
    }
    let decoded = base64_decode(encoded.trim())?;
    let decoded = String::from_utf8(decoded).ok()?;
    let (_, password) = decoded.split_once(':')?;
    let (user, key) = key_user(state.db.reader(), password).await?;
    if key.kind != KeyKind::AppPassword {
        return None;
    }
    crate::auth::note_seen(&state.db, &user).await;
    Some((user.id, key.scope))
}

/// Standard base64, for the Basic authentication header.
fn base64_decode(input: &str) -> Option<Vec<u8>> {
    fn value(c: u8) -> Option<u32> {
        Some(match c {
            b'A'..=b'Z' => c - b'A',
            b'a'..=b'z' => c - b'a' + 26,
            b'0'..=b'9' => c - b'0' + 52,
            b'+' => 62,
            b'/' => 63,
            _ => return None,
        } as u32)
    }
    let input = input.trim_end_matches('=').as_bytes();
    let mut out = Vec::with_capacity(input.len() * 3 / 4);
    for chunk in input.chunks(4) {
        let mut n = 0u32;
        for (i, c) in chunk.iter().enumerate() {
            n |= value(*c)? << (18 - 6 * i);
        }
        let bytes = n.to_be_bytes();
        out.extend_from_slice(&bytes[1..chunk.len()]);
    }
    Some(out)
}

/// The user's tree as dav-server's file system.
#[derive(Clone)]
struct TreeFs {
    files: Arc<Files>,
    user_id: Uuid,
    root: PathBuf,
    local: Box<LocalFs>,
}

/// A path as the index stores it: NFC (Finder sends names decomposed).
pub(crate) fn nfc(path: &DavPath) -> DavPath {
    let raw = String::from_utf8_lossy(path.as_bytes()).into_owned();
    let normalized = crate::files::names::nfc(&raw);
    if normalized == raw {
        return path.clone();
    }
    DavPath::new(&encode(&normalized)).unwrap_or_else(|_| path.clone())
}

/// A path for a DavPath: percent-encoded but for the unreserved bytes.
pub(crate) fn encode(path: &str) -> String {
    let mut encoded = String::new();
    for byte in path.bytes() {
        if byte.is_ascii_alphanumeric() || b"/-._~".contains(&byte) {
            encoded.push(byte as char);
        } else {
            encoded.push_str(&format!("%{byte:02X}"));
        }
    }
    encoded
}

/// The names of a path, from the root.
pub(crate) fn parts(path: &DavPath) -> Vec<String> {
    path.as_rel_ospath()
        .components()
        .filter_map(|c| match c {
            std::path::Component::Normal(name) => Some(name.to_string_lossy().into_owned()),
            _ => None,
        })
        .collect()
}

/// Where a path is: in the user's own tree, or in a share they placed.
enum Spot {
    Own,
    /// In the owner's tree, in its storage: `base` (the shared folder, or
    /// the folder of a shared file, from the drive's root) and the path
    /// under it.
    Shared {
        storage: Box<crate::storage::Storage>,
        base: String,
        rest: DavPath,
    },
}

impl Spot {
    /// dav-server's file system there, and the path in it: `LocalFs` on
    /// the server's disk; None in a storage backend, where the index and
    /// the storage layer answer (`IndexMeta`, `RemoteFile`).
    fn fs(&self, own: &TreeFs, path: &DavPath) -> Option<(Box<LocalFs>, DavPath)> {
        match self {
            Spot::Own => Some((own.local.clone(), path.clone())),
            Spot::Shared {
                storage,
                base,
                rest,
            } => storage
                .local_path(base)
                .map(|base| (LocalFs::new(base, false, false, false), rest.clone())),
        }
    }

    /// Its storage, and the path there from the drive's root.
    fn place(&self, own: &TreeFs, path: &DavPath) -> FsResult<(crate::storage::Storage, String)> {
        let rel = |p: &DavPath| parts(p).join("/");
        match self {
            Spot::Own => Ok((
                crate::storage::Storage::disk(&own.root).map_err(|_| FsError::GeneralFailure)?,
                rel(path),
            )),
            Spot::Shared {
                storage,
                base,
                rest,
            } => Ok(((**storage).clone(), crate::storage::join(base, &rel(rest)))),
        }
    }
}

/// A share placed in a listed folder, as an entry of it.
pub(crate) struct MountEntry {
    pub(crate) name: Vec<u8>,
    pub(crate) meta: Box<dyn DavMetaData>,
}

impl DavDirEntry for MountEntry {
    fn name(&self) -> Vec<u8> {
        self.name.clone()
    }

    fn metadata(&'_ self) -> FsFuture<'_, Box<dyn DavMetaData>> {
        let meta = self.meta.clone();
        Box::pin(async move { Ok(meta) })
    }
}

/// The entries of a listing, one after the other.
pub(crate) struct Entries(pub(crate) std::collections::VecDeque<FsResult<Box<dyn DavDirEntry>>>);

impl futures_core::Stream for Entries {
    type Item = FsResult<Box<dyn DavDirEntry>>;

    fn poll_next(
        mut self: std::pin::Pin<&mut Self>,
        _: &mut std::task::Context<'_>,
    ) -> std::task::Poll<Option<Self::Item>> {
        std::task::Poll::Ready(self.0.pop_front())
    }
}

pub(crate) fn fs_error(error: FileError) -> FsError {
    match error {
        FileError::NotFound => FsError::NotFound,
        FileError::Conflict(_) => FsError::Exists,
        FileError::Invalid(_) | FileError::Forbidden(_) | FileError::ReadOnly => FsError::Forbidden,
        FileError::TooLarge(_) => FsError::TooLarge,
        FileError::Full => FsError::InsufficientStorage,
        other => {
            tracing::warn!("webdav: {other}");
            FsError::GeneralFailure
        }
    }
}

/// The parent folder and the name of a path that may not exist yet.
pub(crate) fn split(path: &DavPath) -> FsResult<(PathBuf, String)> {
    let rel = path.as_rel_ospath();
    let name = rel
        .file_name()
        .ok_or(FsError::Forbidden)?
        .to_string_lossy()
        .into_owned();
    let parent = rel.parent().map(Path::to_path_buf).unwrap_or_default();
    Ok((parent, name))
}

impl TreeFs {
    /// The node at a path, through the shares the user placed.
    async fn node(&self, path: &DavPath) -> FsResult<crate::files::Node> {
        self.files
            .resolve_for(self.user_id, path.as_rel_ospath())
            .await
            .map_err(fs_error)
    }

    /// Whose tree a node is in, if the user may do this to it there.
    async fn owner(&self, id: Uuid, need: Need) -> FsResult<Uuid> {
        Ok(self
            .files
            .access_for(self.user_id, id, need)
            .await
            .map_err(fs_error)?
            .owner)
    }

    /// Where a path is: walked from the user's root, through their own
    /// folders, until it reaches a share they placed (or not).
    async fn locate(&self, path: &DavPath) -> FsResult<Spot> {
        let names = parts(path);
        let mut at = self
            .files
            .root_node(self.user_id)
            .await
            .map_err(fs_error)?
            .id;
        for (i, name) in names.iter().enumerate() {
            if let Some(mount) = self
                .files
                .mount_named(self.user_id, at, name)
                .await
                .map_err(fs_error)?
            {
                let access = self
                    .files
                    .access(self.user_id, mount.node_id)
                    .await
                    .map_err(fs_error)?;
                let storage = self.files.storage(access.owner).await.map_err(fs_error)?;
                let at = self.files.rel_path(mount.node_id).await.map_err(fs_error)?;
                let under = &names[i + 1..];
                let (base, rest) = if access.node.kind == Kind::File {
                    if !under.is_empty() {
                        return Err(FsError::NotFound);
                    }
                    let parent = at.rsplit_once('/').map_or("", |(p, _)| p).to_string();
                    (parent, format!("/{}", access.node.name))
                } else {
                    (at, format!("/{}", under.join("/")))
                };
                let rest = DavPath::new(&encode(&rest)).map_err(|_| FsError::NotFound)?;
                return Ok(Spot::Shared {
                    storage: Box::new(storage),
                    base,
                    rest,
                });
            }
            match self
                .files
                .child_folder(self.user_id, at, name)
                .await
                .map_err(fs_error)?
            {
                Some(folder) => at = folder.id,
                // A file, or not indexed yet: the rest is the user's own.
                None => return Ok(Spot::Own),
            }
        }
        Ok(Spot::Own)
    }

    /// Whether a request's path (with the prefix) is an encrypted folder
    /// or in one, or hidden from the user (inside a placed share: a limited
    /// folder, others' uploads where they may only upload): each folder on
    /// the way is looked at, as far as it exists.
    async fn hidden(&self, uri_path: &str) -> bool {
        let Some(path) = uri_path
            .strip_prefix(PREFIX)
            .and_then(|p| DavPath::new(p).ok())
        else {
            return false;
        };
        let names = parts(&nfc(&path));
        // Organizer's own folder at the root (the trash, the versions) is
        // hidden the same way.
        if names
            .first()
            .is_some_and(|n| n == crate::files::ORGANIZER_DIR)
        {
            return true;
        }
        for depth in 1..=names.len() {
            let prefix: std::path::PathBuf = names[..depth].iter().collect();
            match self.files.resolve_for(self.user_id, &prefix).await {
                Ok(node) if node.e2e_root.is_some() => return true,
                // Someone else's tree (a placed share): as they may see it.
                Ok(node) if node.drive_id != crate::files::drives::my(self.user_id) => {
                    if self.files.access(self.user_id, node.id).await.is_err() {
                        return true;
                    }
                }
                Ok(_) => {}
                Err(_) => return false,
            }
        }
        false
    }

    /// A file read in a share whose drive is in a storage backend: through
    /// the storage layer.
    async fn open_remote(&self, spot: &Spot, path: &DavPath) -> FsResult<Box<dyn DavFile>> {
        let node = self.node(path).await?;
        if node.kind == Kind::Folder {
            return Err(FsError::Forbidden);
        }
        let (storage, rel) = spot.place(self, path)?;
        let (reader, size) = storage.reader(&rel).await.map_err(|e| fs_error(e.into()))?;
        Ok(Box::new(RemoteFile {
            reader,
            size,
            at: 0,
            node,
        }))
    }

    /// A folder's listing in a share whose drive is in a storage backend:
    /// the index's, as the user may see it.
    async fn read_dir_remote(&self, path: &DavPath) -> FsResult<FsStream<Box<dyn DavDirEntry>>> {
        let folder = self.node(path).await?;
        if folder.kind != Kind::Folder {
            return Err(FsError::Forbidden);
        }
        let hidden = self
            .files
            .hidden_children(self.user_id, folder.id)
            .await
            .map_err(fs_error)?;
        let children: Vec<crate::files::Node> = sqlx::query_as(sqlx::AssertSqlSafe(format!(
            "SELECT {} FROM nodes WHERE parent_id = ?1 AND e2e_root IS NULL",
            crate::files::NODE_COLUMNS
        )))
        .bind(folder.id)
        .fetch_all(self.files.db().reader())
        .await
        .map_err(|e| fs_error(e.into()))?;
        let entries = children
            .into_iter()
            .filter(|c| !hidden.contains(&c.name))
            .map(|node| {
                Ok(Box::new(MountEntry {
                    name: node.name.clone().into_bytes(),
                    meta: Box::new(IndexMeta(node)),
                }) as Box<dyn DavDirEntry>)
            })
            .collect();
        Ok(Box::pin(Entries(entries)) as FsStream<Box<dyn DavDirEntry>>)
    }

    async fn delete(&self, path: &DavPath) -> FsResult<()> {
        let node = self.node(path).await?;
        if node.parent_id.is_none() {
            return Err(FsError::Forbidden);
        }
        self.files
            .delete_as(self.user_id, node.id, node.version)
            .await
            .map(|_| ())
            .map_err(fs_error)
    }
}

impl DavFileSystem for TreeFs {
    fn open<'a>(
        &'a self,
        path: &'a DavPath,
        options: OpenOptions,
    ) -> FsFuture<'a, Box<dyn DavFile>> {
        Box::pin(async move {
            let path = nfc(path);
            if !(options.write || options.append || options.truncate || options.create) {
                let spot = self.locate(&path).await?;
                return match spot.fs(self, &path) {
                    Some((fs, at)) => fs.open(&at, options).await,
                    None => self.open_remote(&spot, &path).await,
                };
            }
            let existing = match self.node(&path).await {
                Ok(node) if node.kind == Kind::Folder => return Err(FsError::Forbidden),
                Ok(node) => Some(node),
                Err(FsError::NotFound) => None,
                Err(e) => return Err(e),
            };
            if existing.is_some() && options.create_new {
                return Err(FsError::Exists);
            }
            if existing.is_none() && !(options.create || options.create_new) {
                return Err(FsError::NotFound);
            }
            let target = match &existing {
                Some(node) => {
                    self.owner(node.id, Need::Change).await?;
                    Target::Replace(node.id)
                }
                None => {
                    let (parent, name) = split(&path)?;
                    let parent = self
                        .files
                        .resolve_for(self.user_id, &parent)
                        .await
                        .map_err(fs_error)?;
                    if parent.kind != Kind::Folder {
                        return Err(FsError::Forbidden);
                    }
                    self.owner(parent.id, Need::Create).await?;
                    Target::New(parent.id, name)
                }
            };
            std::fs::create_dir_all(self.files.uploads_dir())?;
            let temp = self
                .files
                .uploads_dir()
                .join(format!("dav-{}", Uuid::now_v7()));
            // A write that keeps the content (append, or no truncate) starts
            // from a copy of it, wherever it is stored.
            let keeps = match &existing {
                Some(node) if !options.truncate => {
                    let owner = self.owner(node.id, Need::Change).await?;
                    let storage = self.files.storage(owner).await.map_err(fs_error)?;
                    let rel = self.files.rel_path(node.id).await.map_err(fs_error)?;
                    storage
                        .copy_out(&rel, &temp)
                        .await
                        .map_err(|e| fs_error(e.into()))?;
                    true
                }
                _ => false,
            };
            let mut file = std::fs::OpenOptions::new()
                .read(true)
                .write(true)
                .create_new(!keeps)
                .open(&temp)?;
            if keeps && options.append {
                file.seek(SeekFrom::End(0))?;
            }
            Ok(Box::new(Upload {
                files: self.files.clone(),
                writer: Writer::User(self.user_id),
                target,
                temp,
                file: Some(file),
            }) as Box<dyn DavFile>)
        })
    }

    fn read_dir<'a>(
        &'a self,
        path: &'a DavPath,
        meta: ReadDirMeta,
    ) -> FsFuture<'a, FsStream<Box<dyn DavDirEntry>>> {
        Box::pin(async move {
            let path = nfc(path);
            let spot = self.locate(&path).await?;
            let Some((fs, at)) = spot.fs(self, &path) else {
                return self.read_dir_remote(&path).await;
            };
            let mut listed = fs.read_dir(&at, meta).await?;
            let Ok(folder) = self.node(&path).await else {
                return Ok(listed);
            };
            // The encrypted folders in it are left out.
            let mut encrypted: Vec<String> = sqlx::query_scalar(
                "SELECT name FROM nodes WHERE parent_id = ?1 AND e2e_root IS NOT NULL",
            )
            .bind(folder.id)
            .fetch_all(self.files.db().reader())
            .await
            .map_err(|e| fs_error(e.into()))?;
            // And Organizer's own folder at the root.
            if matches!(spot, Spot::Own) && folder.parent_id.is_none() {
                encrypted.push(crate::files::ORGANIZER_DIR.to_string());
            }
            // In a placed share: what is hidden from the user.
            if matches!(spot, Spot::Shared { .. }) {
                encrypted.extend(
                    self.files
                        .hidden_children(self.user_id, folder.id)
                        .await
                        .map_err(fs_error)?,
                );
            }
            // The user's own folder: with the shares they placed in it.
            let mounts = match spot {
                Spot::Own => self
                    .files
                    .mounts_in(self.user_id, folder.id)
                    .await
                    .map_err(fs_error)?,
                Spot::Shared { .. } => Vec::new(),
            };
            if mounts.is_empty() && encrypted.is_empty() {
                return Ok(listed);
            }
            let mut entries = std::collections::VecDeque::new();
            while let Some(entry) = std::future::poll_fn(|cx| listed.as_mut().poll_next(cx)).await {
                if let Ok(entry) = &entry {
                    let name = crate::files::names::nfc(&String::from_utf8_lossy(&entry.name()));
                    if encrypted.contains(&name) {
                        continue;
                    }
                }
                entries.push_back(entry);
            }
            for mount in mounts {
                // A placed encrypted folder is hidden too.
                if let Ok(access) = self.files.access(self.user_id, mount.node_id).await
                    && access.node.e2e_root.is_some()
                {
                    continue;
                }
                let mut names = parts(&path);
                names.push(mount.name.clone());
                let Ok(inside) = DavPath::new(&encode(&format!("/{}", names.join("/")))) else {
                    continue;
                };
                let Ok(meta) = self.metadata(&inside).await else {
                    continue;
                };
                entries.push_back(Ok(Box::new(MountEntry {
                    name: mount.name.into_bytes(),
                    meta,
                }) as Box<dyn DavDirEntry>));
            }
            Ok(Box::pin(Entries(entries)) as FsStream<Box<dyn DavDirEntry>>)
        })
    }

    fn metadata<'a>(&'a self, path: &'a DavPath) -> FsFuture<'a, Box<dyn DavMetaData>> {
        Box::pin(async move {
            let path = nfc(path);
            match self.locate(&path).await?.fs(self, &path) {
                Some((fs, at)) => fs.metadata(&at).await,
                None => Ok(Box::new(IndexMeta(self.node(&path).await?)) as Box<dyn DavMetaData>),
            }
        })
    }

    fn symlink_metadata<'a>(&'a self, path: &'a DavPath) -> FsFuture<'a, Box<dyn DavMetaData>> {
        Box::pin(async move {
            let path = nfc(path);
            match self.locate(&path).await?.fs(self, &path) {
                Some((fs, at)) => fs.symlink_metadata(&at).await,
                None => Ok(Box::new(IndexMeta(self.node(&path).await?)) as Box<dyn DavMetaData>),
            }
        })
    }

    fn create_dir<'a>(&'a self, path: &'a DavPath) -> FsFuture<'a, ()> {
        Box::pin(async move {
            let (parent, name) = split(&nfc(path))?;
            let parent = self
                .files
                .resolve_for(self.user_id, &parent)
                .await
                .map_err(fs_error)?;
            let owner = self.owner(parent.id, Need::Create).await?;
            let folder = self
                .files
                .create_folder(owner, parent.id, &name)
                .await
                .map_err(fs_error)?;
            self.files
                .mark_created(folder.id, self.user_id)
                .await
                .map_err(fs_error)
        })
    }

    fn remove_dir<'a>(&'a self, path: &'a DavPath) -> FsFuture<'a, ()> {
        Box::pin(async move { self.delete(&nfc(path)).await })
    }

    fn remove_file<'a>(&'a self, path: &'a DavPath) -> FsFuture<'a, ()> {
        Box::pin(async move { self.delete(&nfc(path)).await })
    }

    fn rename<'a>(&'a self, from: &'a DavPath, to: &'a DavPath) -> FsFuture<'a, ()> {
        Box::pin(async move {
            let node = self.node(&nfc(from)).await?;
            let (parent, name) = split(&nfc(to))?;
            let parent = self
                .files
                .resolve_for(self.user_id, &parent)
                .await
                .map_err(fs_error)?;
            self.files
                .move_as(
                    self.user_id,
                    node.id,
                    Some(parent.id),
                    Some(&name),
                    node.version,
                )
                .await
                .map(|_| ())
                .map_err(fs_error)
        })
    }

    fn copy<'a>(&'a self, from: &'a DavPath, to: &'a DavPath) -> FsFuture<'a, ()> {
        Box::pin(async move {
            // The name is checked like any Organizer makes; the copy itself
            // is LocalFs's, and a scan indexes it.
            let (from, to) = (nfc(from), nfc(to));
            let (parent, name) = split(&to)?;
            crate::files::names::new_name(&name).map_err(|_| FsError::Forbidden)?;
            let parent = self
                .files
                .resolve_for(self.user_id, &parent)
                .await
                .map_err(fs_error)?;
            let owner = self.owner(parent.id, Need::Create).await?;
            let (source, target) = (self.locate(&from).await?, self.locate(&to).await?);
            if owner == self.user_id && matches!(source, Spot::Own) && matches!(target, Spot::Own) {
                self.local.copy(&from, &to).await?;
            } else {
                // From or into a share: the bytes through the storage layer,
                // file by file (dav-server makes the folders and copies
                // each file), by way of the uploads folder.
                let (from_storage, from_rel) = source.place(self, &from)?;
                let (to_storage, to_rel) = target.place(self, &to)?;
                let io = |e: std::io::Error| fs_error(e.into());
                if to_storage.exists(&to_rel).await.map_err(io)? {
                    return Err(FsError::Exists);
                }
                std::fs::create_dir_all(self.files.uploads_dir())?;
                let temp = self
                    .files
                    .uploads_dir()
                    .join(format!("dav-copy-{}", Uuid::now_v7()));
                let copied = match from_storage.copy_out(&from_rel, &temp).await {
                    Ok(()) => to_storage.put_file(&temp, &to_rel).await,
                    Err(e) => Err(e),
                };
                let _ = std::fs::remove_file(&temp);
                copied.map_err(io)?;
            }
            self.files.scan(owner).await.map_err(fs_error)?;
            Ok(())
        })
    }

    fn get_quota(&'_ self) -> FsFuture<'_, (u64, Option<u64>)> {
        self.local.get_quota()
    }
}

/// Where a PUT goes.
#[derive(Debug, Clone)]
pub(crate) enum Target {
    New(Uuid, String),
    Replace(Uuid),
}

/// A file being written over WebDAV: to the uploads folder, then placed
/// in the tree whole on `flush`, at the version it has then (WebDAV's own
/// `If-Match` is dav-server's to check).
pub(crate) struct Upload {
    pub(crate) files: Arc<Files>,
    pub(crate) writer: Writer,
    pub(crate) target: Target,
    pub(crate) temp: PathBuf,
    pub(crate) file: Option<std::fs::File>,
}

/// Who writes a file: a user (in their tree or a share they placed), or
/// someone on another server through a share with them
/// (`federation::dav`), as the share lets them.
pub(crate) enum Writer {
    User(Uuid),
    Share(Box<crate::federation::outgoing::OcmShare>),
}

impl std::fmt::Debug for Upload {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Upload")
            .field("target", &self.target)
            .field("temp", &self.temp)
            .finish_non_exhaustive()
    }
}

impl Drop for Upload {
    fn drop(&mut self) {
        // Not placed (an aborted PUT): its bytes go.
        let _ = std::fs::remove_file(&self.temp);
    }
}

impl Upload {
    fn file(&mut self) -> FsResult<&mut std::fs::File> {
        self.file.as_mut().ok_or(FsError::GeneralFailure)
    }
}

impl DavFile for Upload {
    fn metadata(&'_ mut self) -> FsFuture<'_, Box<dyn DavMetaData>> {
        Box::pin(async move {
            let metadata = self.file()?.metadata()?;
            Ok(Box::new(UploadMeta(metadata)) as Box<dyn DavMetaData>)
        })
    }

    fn write_buf(&'_ mut self, mut buf: Box<dyn bytes::Buf + Send>) -> FsFuture<'_, ()> {
        Box::pin(async move {
            let file = self.file()?;
            while buf.has_remaining() {
                let chunk = buf.chunk();
                file.write_all(chunk)?;
                let n = chunk.len();
                buf.advance(n);
            }
            Ok(())
        })
    }

    fn write_bytes(&'_ mut self, buf: bytes::Bytes) -> FsFuture<'_, ()> {
        Box::pin(async move {
            self.file()?.write_all(&buf)?;
            Ok(())
        })
    }

    fn read_bytes(&'_ mut self, count: usize) -> FsFuture<'_, bytes::Bytes> {
        Box::pin(async move {
            let mut buf = vec![0u8; count];
            let n = self.file()?.read(&mut buf)?;
            buf.truncate(n);
            Ok(bytes::Bytes::from(buf))
        })
    }

    fn seek(&'_ mut self, pos: SeekFrom) -> FsFuture<'_, u64> {
        Box::pin(async move { Ok(self.file()?.seek(pos)?) })
    }

    fn flush(&'_ mut self) -> FsFuture<'_, ()> {
        Box::pin(async move {
            let Some(file) = self.file.take() else {
                return Ok(());
            };
            file.sync_all()?;
            drop(file);
            // Into whose tree: the user's, or a share's owner's (whose
            // limit it counts on), if the share still lets them.
            let (id, need) = match &self.target {
                Target::New(parent, _) => (*parent, Need::Create),
                Target::Replace(id) => (*id, Need::Change),
            };
            let (owner, version) = match &self.writer {
                Writer::User(user) => {
                    let access = self
                        .files
                        .access_for(*user, id, need)
                        .await
                        .map_err(fs_error)?;
                    (access.owner, access.node.version)
                }
                Writer::Share(share) => {
                    let role = crate::federation::outgoing::role_in(&self.files, share, id)
                        .await
                        .map_err(fs_error)?
                        .ok_or(FsError::NotFound)?;
                    if !role.allows(need) {
                        return Err(FsError::Forbidden);
                    }
                    let node = self
                        .files
                        .node(share.drive_id, id)
                        .await
                        .map_err(fs_error)?;
                    (share.drive_id, node.version)
                }
            };
            let target = match self.target.clone() {
                Target::New(parent, name) => WriteTarget::New { parent, name },
                Target::Replace(id) => WriteTarget::Replace {
                    id,
                    if_version: version,
                    source: Some(crate::files::versions::Source::Webdav),
                },
            };
            let new = matches!(target, WriteTarget::New { .. });
            let node = self
                .files
                .write_file(owner, target, &self.temp)
                .await
                .map_err(fs_error)?;
            if new {
                match &self.writer {
                    Writer::User(user) => self
                        .files
                        .mark_created(node.id, *user)
                        .await
                        .map_err(fs_error)?,
                    Writer::Share(share) => {
                        crate::federation::outgoing::mark_created(&self.files, node.id, share.id)
                            .await
                            .map_err(|e| fs_error(e.into()))?
                    }
                }
            }
            Ok(())
        })
    }
}

/// A node's metadata from the index (a share in a storage backend).
#[derive(Debug, Clone)]
pub(crate) struct IndexMeta(pub(crate) crate::files::Node);

impl DavMetaData for IndexMeta {
    fn len(&self) -> u64 {
        self.0.size as u64
    }

    fn modified(&self) -> FsResult<std::time::SystemTime> {
        self.0
            .updated_at
            .parse::<jiff::Timestamp>()
            .map(std::time::SystemTime::from)
            .map_err(|_| FsError::GeneralFailure)
    }

    fn is_dir(&self) -> bool {
        self.0.kind == Kind::Folder
    }

    fn etag(&self) -> Option<String> {
        Some(format!("{}-{}", self.0.id, self.0.version))
    }
}

/// A file read from a storage backend, piece by piece as the client asks.
pub(crate) struct RemoteFile {
    pub(crate) reader: opendal::Reader,
    pub(crate) size: u64,
    pub(crate) at: u64,
    pub(crate) node: crate::files::Node,
}

impl std::fmt::Debug for RemoteFile {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("RemoteFile")
            .field("node", &self.node.id)
            .finish_non_exhaustive()
    }
}

impl DavFile for RemoteFile {
    fn metadata(&'_ mut self) -> FsFuture<'_, Box<dyn DavMetaData>> {
        let meta = IndexMeta(self.node.clone());
        Box::pin(async move { Ok(Box::new(meta) as Box<dyn DavMetaData>) })
    }

    fn write_buf(&'_ mut self, _: Box<dyn bytes::Buf + Send>) -> FsFuture<'_, ()> {
        Box::pin(async { Err(FsError::Forbidden) })
    }

    fn write_bytes(&'_ mut self, _: bytes::Bytes) -> FsFuture<'_, ()> {
        Box::pin(async { Err(FsError::Forbidden) })
    }

    fn read_bytes(&'_ mut self, count: usize) -> FsFuture<'_, bytes::Bytes> {
        Box::pin(async move {
            let end = (self.at + count as u64).min(self.size);
            if end <= self.at {
                return Ok(bytes::Bytes::new());
            }
            let piece = self
                .reader
                .read(self.at..end)
                .await
                .map_err(|_| FsError::GeneralFailure)?;
            self.at = end;
            Ok(piece.to_bytes())
        })
    }

    fn seek(&'_ mut self, pos: SeekFrom) -> FsFuture<'_, u64> {
        Box::pin(async move {
            let at = match pos {
                SeekFrom::Start(at) => at as i64,
                SeekFrom::End(off) => self.size as i64 + off,
                SeekFrom::Current(off) => self.at as i64 + off,
            };
            if at < 0 {
                return Err(FsError::GeneralFailure);
            }
            self.at = at as u64;
            Ok(self.at)
        })
    }

    fn flush(&'_ mut self) -> FsFuture<'_, ()> {
        Box::pin(async { Ok(()) })
    }
}

/// The metadata of a file being written.
#[derive(Debug, Clone)]
pub(crate) struct UploadMeta(pub(crate) std::fs::Metadata);

impl DavMetaData for UploadMeta {
    fn len(&self) -> u64 {
        self.0.len()
    }

    fn modified(&self) -> FsResult<std::time::SystemTime> {
        Ok(self.0.modified()?)
    }

    fn is_dir(&self) -> bool {
        false
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn basic_credentials_are_decoded() {
        // "user:org_abc"
        assert_eq!(
            base64_decode("dXNlcjpvcmdfYWJj").unwrap(),
            b"user:org_abc".to_vec()
        );
        assert_eq!(base64_decode("YQ==").unwrap(), b"a".to_vec());
        assert!(base64_decode("not base64!").is_none());
    }
}
