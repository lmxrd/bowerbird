//! The WebDAV drop box at `/dav-inbox/` (docs/M2_DESIGN.md §9, question
//! 1): a small drive of its own ("Organizer drop box"), so that no folder
//! name of the user's tree is reserved. A file written into it becomes an
//! item of the drop box; `review/` always asks (the item waits for the
//! user), `auto/` accepts at once.
//!
//! The drive shows the items still waiting in it (staged, not placed), so
//! that a client sees the file it just wrote: Finder looks it up right
//! after. Their analysis waits a few seconds: Finder writes a file empty
//! first, then again with its content, and a second write of a waiting
//! item replaces it. Finder's own `._*` and `.DS_Store` files are taken and
//! thrown away. Nothing else can be done there: no folders, no renames.

use crate::dav::{authenticate, fs_error, locks};
use crate::drop_box::{self, Origin, staging_dir};
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
use std::io::{Read, Seek, SeekFrom, Write};
use std::path::PathBuf;
use std::pin::Pin;
use std::task::{Context, Poll};
use std::time::{Duration, SystemTime};
use uuid::Uuid;

/// Where the WebDAV drop box is mounted.
pub const PREFIX: &str = "/dav-inbox";
/// Its folders, besides its top: `review` always asks, `auto` accepts.
const FOLDERS: [&str; 2] = ["review", "auto"];
/// How long a written file waits before it is analysed.
pub const DELAY: Duration = Duration::from_secs(5);
/// How long a folder's analysis waits while a file is being written into it.
const WRITING: Duration = Duration::from_secs(60);

/// A request to the WebDAV drop box: an app password, as for `/dav/`.
pub async fn handle(State(state): State<AppState>, request: Request) -> Response {
    let Some((user_id, scope)) = authenticate(&state, request.headers()).await else {
        return (
            StatusCode::UNAUTHORIZED,
            [(
                header::WWW_AUTHENTICATE,
                HeaderValue::from_static("Basic realm=\"Organizer drop box\", charset=\"UTF-8\""),
            )],
            "An app password is required (Settings → App passwords)",
        )
            .into_response();
    };
    let read_only = matches!(
        request.method().as_str(),
        "GET" | "HEAD" | "OPTIONS" | "PROPFIND"
    );
    if scope == crate::auth::Scope::Read && !read_only {
        return (StatusCode::FORBIDDEN, "This app password can only read").into_response();
    }
    let handler = dav_server::DavHandler::builder()
        .filesystem(Box::new(InboxFs { state, user_id }))
        .locksystem(locks())
        .strip_prefix(PREFIX)
        .build_handler();
    handler.handle(request).await.map(Body::new)
}

/// What a path of the drive is.
#[derive(Debug, PartialEq)]
enum Place {
    /// The top, or one of its folders (`""`, `review`, `auto`).
    Folder(&'static str),
    /// An item of a drive folder (`rest` empty), or a path inside an item
    /// that is a folder.
    Item {
        folder: &'static str,
        name: String,
        rest: Vec<String>,
    },
}

fn place(path: &DavPath) -> FsResult<Place> {
    let raw = String::from_utf8_lossy(path.as_bytes()).into_owned();
    let raw = crate::files::names::nfc(&raw);
    let parts: Vec<&str> = raw.split('/').filter(|p| !p.is_empty()).collect();
    let folder = |name: &str| FOLDERS.iter().copied().find(|f| *f == name);
    let item = |folder: &'static str, parts: &[&str]| Place::Item {
        folder,
        name: parts[0].to_string(),
        rest: parts[1..].iter().map(|p| p.to_string()).collect(),
    };
    Ok(match parts.as_slice() {
        [] => Place::Folder(""),
        [one] if folder(one).is_some() => Place::Folder(folder(one).unwrap()),
        [first, rest @ ..] if folder(first).is_some() => item(folder(first).unwrap(), rest),
        all => item("", all),
    })
}

/// The names of a client's own files, taken and thrown away.
pub fn ignored(name: &str) -> bool {
    name.starts_with("._")
        || matches!(
            name,
            ".DS_Store" | "Thumbs.db" | "desktop.ini" | ".localized"
        )
}

/// A waiting item of the drive.
#[derive(sqlx::FromRow)]
struct Staged {
    id: Uuid,
    name: String,
    kind: drop_box::ItemKind,
    status: drop_box::Status,
}

#[derive(Clone)]
struct InboxFs {
    state: AppState,
    user_id: Uuid,
}

fn db_error(e: sqlx::Error) -> FsError {
    fs_error(e.into())
}

impl InboxFs {
    /// The items waiting in a folder, the newest of each name.
    async fn staged(&self, folder: &str) -> FsResult<Vec<Staged>> {
        let rows: Vec<Staged> = sqlx::query_as(
            "SELECT id, name, kind, status FROM inbox_items
             WHERE user_id = ?1 AND dav_folder = ?2 AND kind IN ('file', 'folder')
               AND node_id IS NULL AND status NOT IN ('confirmed', 'changed', 'auto_accepted')
             ORDER BY id DESC",
        )
        .bind(self.user_id)
        .bind(folder)
        .fetch_all(self.state.db.reader())
        .await
        .map_err(db_error)?;
        let mut seen = std::collections::HashSet::new();
        Ok(rows
            .into_iter()
            .filter(|r| seen.insert(crate::files::names::fold(&r.name)))
            .collect())
    }

    async fn find(&self, folder: &str, name: &str) -> FsResult<Option<Staged>> {
        let wanted = crate::files::names::fold(name);
        Ok(self
            .staged(folder)
            .await?
            .into_iter()
            .find(|s| crate::files::names::fold(&s.name) == wanted))
    }

    /// The staged path of an item, or of a path inside a folder item (each
    /// part checked as a name).
    fn path_of(&self, staged: &Staged, rest: &[String]) -> FsResult<PathBuf> {
        let mut path = staging_dir(&self.state, self.user_id, staged.id).join(&staged.name);
        if !rest.is_empty() && staged.kind != drop_box::ItemKind::Folder {
            return Err(FsError::NotFound);
        }
        for part in rest {
            crate::files::names::new_name(part).map_err(|_| FsError::Forbidden)?;
            path = path.join(part);
        }
        Ok(path)
    }

    /// A folder item still being filled: one that is not analysed yet.
    async fn filling(&self, folder: &str, name: &str) -> FsResult<Staged> {
        let staged = self.find(folder, name).await?.ok_or(FsError::NotFound)?;
        if staged.kind != drop_box::ItemKind::Folder || staged.status != drop_box::Status::Queued {
            return Err(FsError::Forbidden);
        }
        Ok(staged)
    }

    /// Its analysis waits again, and its size is the sum of its files.
    async fn touched(&self, staged: &Staged) -> FsResult<()> {
        let root = staging_dir(&self.state, self.user_id, staged.id).join(&staged.name);
        let size: u64 = if root.is_dir() {
            drop_box::folder_files(&root)
                .iter()
                .filter_map(|f| std::fs::metadata(root.join(f)).ok())
                .map(|m| m.len())
                .sum()
        } else {
            std::fs::metadata(&root).map(|m| m.len()).unwrap_or(0)
        };
        sqlx::query("UPDATE inbox_items SET size = ?2, updated_at = ?3 WHERE id = ?1")
            .bind(staged.id)
            .bind(size as i64)
            .bind(crate::auth::now_iso())
            .execute(self.state.db.writer())
            .await
            .map_err(db_error)?;
        drop_box::queue_after(&self.state, self.user_id, staged.id, Some(DELAY))
            .await
            .map_err(db_error)
    }

    /// A file written into the drive: a new item, the content of the
    /// waiting one of that name that is not analysed yet, or a file inside
    /// a folder item still being filled.
    async fn commit(
        &self,
        folder: &'static str,
        name: &str,
        rest: &[String],
        temp: &std::path::Path,
    ) -> FsResult<()> {
        // Within the user's storage limit, as every write through Organizer.
        let size = std::fs::metadata(temp).map(|m| m.len()).unwrap_or(0);
        if let Err(e) = self.state.files.check_room(self.user_id, size).await {
            let _ = std::fs::remove_file(temp);
            return Err(fs_error(e));
        }
        if !rest.is_empty() {
            let staged = self.filling(folder, name).await?;
            let path = self.path_of(&staged, rest)?;
            if let Some(parent) = path.parent() {
                std::fs::create_dir_all(parent)?;
            }
            crate::files::move_path(temp, &path)?;
            return self.touched(&staged).await;
        }
        if let Some(staged) = self.find(folder, name).await?
            && staged.kind == drop_box::ItemKind::File
            && staged.status == drop_box::Status::Queued
        {
            crate::files::move_path(temp, &self.path_of(&staged, &[])?)?;
            return self.touched(&staged).await;
        }
        drop_box::add_file_from(
            &self.state,
            self.user_id,
            name,
            temp,
            Origin {
                dav_folder: Some(folder),
                delay: Some(DELAY),
                ..Origin::default()
            },
        )
        .await
        .map(|_| ())
        .map_err(fs_error)
    }

    /// Remove a waiting item (not while it is analysed: it may be placed
    /// any moment).
    async fn remove_item(&self, staged: Staged) -> FsResult<()> {
        if staged.status == drop_box::Status::Analysing {
            return Err(FsError::Forbidden);
        }
        let _lock = drop_box::review::LOCK.lock().await;
        sqlx::query("DELETE FROM inbox_items WHERE id = ?1 AND node_id IS NULL")
            .bind(staged.id)
            .execute(self.state.db.writer())
            .await
            .map_err(db_error)?;
        let _ = std::fs::remove_dir_all(staging_dir(&self.state, self.user_id, staged.id));
        self.state
            .files
            .announce(self.user_id, crate::files::Topic::DropBox);
        Ok(())
    }

    async fn remove(&self, path: &DavPath) -> FsResult<()> {
        let Place::Item { folder, name, rest } = place(path)? else {
            return Err(FsError::Forbidden);
        };
        if rest.last().map_or(ignored(&name), |last| ignored(last)) {
            return Ok(());
        }
        if rest.is_empty() {
            let staged = self.find(folder, &name).await?.ok_or(FsError::NotFound)?;
            return self.remove_item(staged).await;
        }
        let staged = self.filling(folder, &name).await?;
        let path = self.path_of(&staged, &rest)?;
        if path.is_dir() {
            std::fs::remove_dir_all(&path)?;
        } else {
            std::fs::remove_file(&path)?;
        }
        self.touched(&staged).await
    }
}

impl DavFileSystem for InboxFs {
    fn open<'a>(
        &'a self,
        path: &'a DavPath,
        options: OpenOptions,
    ) -> FsFuture<'a, Box<dyn DavFile>> {
        Box::pin(async move {
            let Place::Item { folder, name, rest } = place(path)? else {
                return Err(FsError::Forbidden);
            };
            let writing = options.write || options.append || options.truncate || options.create;
            if !writing {
                let staged = self.find(folder, &name).await?.ok_or(FsError::NotFound)?;
                let file = std::fs::File::open(self.path_of(&staged, &rest)?)?;
                return Ok(Box::new(Reading(file)) as Box<dyn DavFile>);
            }
            let last = rest.last().unwrap_or(&name);
            if ignored(last) {
                return Ok(Box::new(Discard) as Box<dyn DavFile>);
            }
            crate::files::names::new_name(last).map_err(|_| FsError::Forbidden)?;
            if !rest.is_empty() {
                // Into a folder that can still take it; its analysis waits
                // for a slow upload, until the write ends (then `DELAY`).
                let staged = self.filling(folder, &name).await?;
                drop_box::queue_after(&self.state, self.user_id, staged.id, Some(WRITING))
                    .await
                    .map_err(db_error)?;
            }
            let uploads = self.state.files.uploads_dir();
            std::fs::create_dir_all(&uploads)?;
            let temp = uploads.join(format!("dav-inbox-{}", Uuid::now_v7()));
            let file = std::fs::OpenOptions::new()
                .read(true)
                .write(true)
                .create_new(true)
                .open(&temp)?;
            Ok(Box::new(Writing {
                fs: self.clone(),
                folder,
                name,
                rest,
                temp,
                file: Some(file),
            }) as Box<dyn DavFile>)
        })
    }

    fn read_dir<'a>(
        &'a self,
        path: &'a DavPath,
        _meta: ReadDirMeta,
    ) -> FsFuture<'a, FsStream<Box<dyn DavDirEntry>>> {
        Box::pin(async move {
            let mut entries: Vec<Box<dyn DavDirEntry>> = Vec::new();
            match place(path)? {
                Place::Folder(folder) => {
                    if folder.is_empty() {
                        for name in FOLDERS {
                            entries.push(Box::new(Entry {
                                name: name.into(),
                                meta: Meta::Dir,
                            }));
                        }
                    }
                    for staged in self.staged(folder).await? {
                        let Ok(path) = self.path_of(&staged, &[]) else {
                            continue;
                        };
                        let meta = match staged.kind {
                            drop_box::ItemKind::Folder => Meta::Dir,
                            _ => match std::fs::metadata(path) {
                                Ok(m) => Meta::File(m),
                                Err(_) => continue,
                            },
                        };
                        entries.push(Box::new(Entry {
                            name: staged.name,
                            meta,
                        }));
                    }
                }
                Place::Item { folder, name, rest } => {
                    let staged = self.find(folder, &name).await?.ok_or(FsError::NotFound)?;
                    let dir = self.path_of(&staged, &rest)?;
                    for entry in std::fs::read_dir(dir)?.flatten() {
                        let Ok(metadata) = entry.metadata() else {
                            continue;
                        };
                        entries.push(Box::new(Entry {
                            name: entry.file_name().to_string_lossy().into_owned(),
                            meta: if metadata.is_dir() {
                                Meta::Dir
                            } else {
                                Meta::File(metadata)
                            },
                        }));
                    }
                }
            }
            Ok(Box::pin(Entries(entries.into_iter())) as FsStream<Box<dyn DavDirEntry>>)
        })
    }

    fn metadata<'a>(&'a self, path: &'a DavPath) -> FsFuture<'a, Box<dyn DavMetaData>> {
        Box::pin(async move {
            match place(path)? {
                Place::Folder(_) => Ok(Box::new(Meta::Dir) as Box<dyn DavMetaData>),
                Place::Item { folder, name, rest } => {
                    let staged = self.find(folder, &name).await?.ok_or(FsError::NotFound)?;
                    let metadata = std::fs::metadata(self.path_of(&staged, &rest)?)?;
                    Ok(Box::new(if metadata.is_dir() {
                        Meta::Dir
                    } else {
                        Meta::File(metadata)
                    }) as Box<dyn DavMetaData>)
                }
            }
        })
    }

    fn remove_file<'a>(&'a self, path: &'a DavPath) -> FsFuture<'a, ()> {
        Box::pin(async move { self.remove(path).await })
    }

    fn remove_dir<'a>(&'a self, path: &'a DavPath) -> FsFuture<'a, ()> {
        Box::pin(async move { self.remove(path).await })
    }

    /// A folder at a drive folder's top is a new folder item, analysed a
    /// while after its last file comes in; one inside it is made in its
    /// staging.
    fn create_dir<'a>(&'a self, path: &'a DavPath) -> FsFuture<'a, ()> {
        Box::pin(async move {
            let Place::Item { folder, name, rest } = place(path)? else {
                return Err(FsError::Forbidden);
            };
            if rest.is_empty() {
                if self.find(folder, &name).await?.is_some() {
                    return Err(FsError::Exists);
                }
                return drop_box::add_empty_folder(&self.state, self.user_id, &name, folder, DELAY)
                    .await
                    .map(|_| ())
                    .map_err(fs_error);
            }
            let staged = self.filling(folder, &name).await?;
            let path = self.path_of(&staged, &rest)?;
            if path.exists() {
                return Err(FsError::Exists);
            }
            std::fs::create_dir_all(&path)?;
            self.touched(&staged).await
        })
    }

    fn rename<'a>(&'a self, _from: &'a DavPath, _to: &'a DavPath) -> FsFuture<'a, ()> {
        Box::pin(async { Err(FsError::Forbidden) })
    }

    fn copy<'a>(&'a self, _from: &'a DavPath, _to: &'a DavPath) -> FsFuture<'a, ()> {
        Box::pin(async { Err(FsError::Forbidden) })
    }
}

/// An entry of a listing.
struct Entry {
    name: String,
    meta: Meta,
}

impl DavDirEntry for Entry {
    fn name(&self) -> Vec<u8> {
        self.name.as_bytes().to_vec()
    }

    fn metadata(&'_ self) -> FsFuture<'_, Box<dyn DavMetaData>> {
        let meta = self.meta.clone();
        Box::pin(async move { Ok(Box::new(meta) as Box<dyn DavMetaData>) })
    }
}

/// A listing, as the stream dav-server reads.
struct Entries(std::vec::IntoIter<Box<dyn DavDirEntry>>);

impl futures_core::Stream for Entries {
    type Item = FsResult<Box<dyn DavDirEntry>>;

    fn poll_next(mut self: Pin<&mut Self>, _cx: &mut Context<'_>) -> Poll<Option<Self::Item>> {
        Poll::Ready(self.0.next().map(Ok))
    }
}

#[derive(Debug, Clone)]
enum Meta {
    Dir,
    File(std::fs::Metadata),
}

impl DavMetaData for Meta {
    fn len(&self) -> u64 {
        match self {
            Meta::Dir => 0,
            Meta::File(m) => m.len(),
        }
    }

    fn modified(&self) -> FsResult<SystemTime> {
        match self {
            Meta::Dir => Ok(SystemTime::UNIX_EPOCH),
            Meta::File(m) => Ok(m.modified()?),
        }
    }

    fn is_dir(&self) -> bool {
        matches!(self, Meta::Dir)
    }
}

/// A waiting item, read back.
#[derive(Debug)]
struct Reading(std::fs::File);

impl DavFile for Reading {
    fn metadata(&'_ mut self) -> FsFuture<'_, Box<dyn DavMetaData>> {
        Box::pin(
            async move { Ok(Box::new(Meta::File(self.0.metadata()?)) as Box<dyn DavMetaData>) },
        )
    }

    fn write_buf(&'_ mut self, _buf: Box<dyn bytes::Buf + Send>) -> FsFuture<'_, ()> {
        Box::pin(async { Err(FsError::Forbidden) })
    }

    fn write_bytes(&'_ mut self, _buf: bytes::Bytes) -> FsFuture<'_, ()> {
        Box::pin(async { Err(FsError::Forbidden) })
    }

    fn read_bytes(&'_ mut self, count: usize) -> FsFuture<'_, bytes::Bytes> {
        Box::pin(async move {
            let mut buf = vec![0u8; count];
            let n = self.0.read(&mut buf)?;
            buf.truncate(n);
            Ok(bytes::Bytes::from(buf))
        })
    }

    fn seek(&'_ mut self, pos: SeekFrom) -> FsFuture<'_, u64> {
        Box::pin(async move { Ok(self.0.seek(pos)?) })
    }

    fn flush(&'_ mut self) -> FsFuture<'_, ()> {
        Box::pin(async { Ok(()) })
    }
}

/// A client's own file: taken, kept nowhere.
#[derive(Debug)]
struct Discard;

impl DavFile for Discard {
    fn metadata(&'_ mut self) -> FsFuture<'_, Box<dyn DavMetaData>> {
        Box::pin(async { Err(FsError::NotFound) })
    }

    fn write_buf(&'_ mut self, _buf: Box<dyn bytes::Buf + Send>) -> FsFuture<'_, ()> {
        Box::pin(async { Ok(()) })
    }

    fn write_bytes(&'_ mut self, _buf: bytes::Bytes) -> FsFuture<'_, ()> {
        Box::pin(async { Ok(()) })
    }

    fn read_bytes(&'_ mut self, _count: usize) -> FsFuture<'_, bytes::Bytes> {
        Box::pin(async { Ok(bytes::Bytes::new()) })
    }

    fn seek(&'_ mut self, _pos: SeekFrom) -> FsFuture<'_, u64> {
        Box::pin(async { Ok(0) })
    }

    fn flush(&'_ mut self) -> FsFuture<'_, ()> {
        Box::pin(async { Ok(()) })
    }
}

/// A file being written into the drive: to the uploads folder, then an
/// item on `flush`.
struct Writing {
    fs: InboxFs,
    folder: &'static str,
    name: String,
    rest: Vec<String>,
    temp: PathBuf,
    file: Option<std::fs::File>,
}

impl std::fmt::Debug for Writing {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Writing")
            .field("folder", &self.folder)
            .field("name", &self.name)
            .finish_non_exhaustive()
    }
}

impl Drop for Writing {
    fn drop(&mut self) {
        // Not committed (an aborted PUT): its bytes go.
        let _ = std::fs::remove_file(&self.temp);
    }
}

impl Writing {
    fn file(&mut self) -> FsResult<&mut std::fs::File> {
        self.file.as_mut().ok_or(FsError::GeneralFailure)
    }
}

impl DavFile for Writing {
    fn metadata(&'_ mut self) -> FsFuture<'_, Box<dyn DavMetaData>> {
        Box::pin(async move {
            let metadata = self.file()?.metadata()?;
            Ok(Box::new(Meta::File(metadata)) as Box<dyn DavMetaData>)
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
            self.fs
                .commit(self.folder, &self.name, &self.rest, &self.temp)
                .await
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn paths_are_the_top_its_folders_and_items_in_them() {
        let p = |s: &str| place(&DavPath::new(s).unwrap()).unwrap();
        let item = |folder: &'static str, name: &str, rest: &[&str]| Place::Item {
            folder,
            name: name.into(),
            rest: rest.iter().map(|r| r.to_string()).collect(),
        };
        assert_eq!(p("/"), Place::Folder(""));
        assert_eq!(p("/review/"), Place::Folder("review"));
        assert_eq!(p("/a.pdf"), item("", "a.pdf", &[]));
        assert_eq!(p("/auto/a.pdf"), item("auto", "a.pdf", &[]));
        assert_eq!(
            p("/Trip/day 1/a.jpg"),
            item("", "Trip", &["day 1", "a.jpg"])
        );
        assert_eq!(p("/review/Trip/a.jpg"), item("review", "Trip", &["a.jpg"]));
        assert!(ignored("._a.pdf") && ignored(".DS_Store") && !ignored("a.pdf"));
    }
}
