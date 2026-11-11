//! The server's side: the generated client for the JSON endpoints, and,
//! on the same reqwest client, what progenitor cannot describe: downloads
//! into a file, plain uploads (multipart, and `PUT` with `If-Match`), the
//! resumable uploads (`routes/uploads.rs`) and the live events.

use crate::Error;
use crate::api::{self, types};
use crate::local::Kind;
use bytes::Bytes;
use reqwest::header::{self, HeaderMap, HeaderValue};
use std::io;
use std::path::{Path, PathBuf};
use std::pin::Pin;
use std::task::{Context, Poll};
use std::time::Duration;
use tokio::io::{AsyncRead, AsyncSeekExt, AsyncWriteExt, ReadBuf};
use uuid::Uuid;

/// Files under this go in one plain request, larger ones through the
/// resumable uploads (M5_DESIGN.md §3, as the mobile apps do).
pub const PLAIN_MAX: u64 = 4 * 1024 * 1024;

/// What the engine's requests are sent with.
fn http_client(token: Option<&str>) -> Result<reqwest::Client, Error> {
    crate::install_tls_provider();
    let mut headers = HeaderMap::new();
    if let Some(token) = token {
        let mut value = HeaderValue::from_str(&format!("Bearer {token}"))
            .map_err(|_| Error::Invalid("An invalid token".into()))?;
        value.set_sensitive(true);
        headers.insert(header::AUTHORIZATION, value);
    }
    reqwest::Client::builder()
        .default_headers(headers)
        .user_agent(concat!("Organizer-Desktop/", env!("CARGO_PKG_VERSION")))
        .connect_timeout(Duration::from_secs(15))
        // No overall timeout (a large transfer takes what it takes), but a
        // connection that sends nothing for a minute is broken. The live
        // events have a comment every 30 seconds.
        .read_timeout(Duration::from_secs(60))
        .build()
        .map_err(|e| Error::Invalid(e.to_string()))
}

/// The generated client, with the token as `Authorization: Bearer`.
pub fn api_client(server: &str, token: Option<&str>) -> Result<api::Client, Error> {
    Ok(api::Client::new_with_client(server, http_client(token)?))
}

/// A node of the server's tree, as the engine needs it.
#[derive(Clone, Debug)]
pub struct Node {
    pub id: Uuid,
    pub parent_id: Option<Uuid>,
    pub name: String,
    pub kind: Kind,
    pub version: i64,
    /// A file's size and modification time (RFC 3339), when known: the
    /// change feed does not say them.
    pub size: Option<i64>,
    pub modified: Option<String>,
    /// The encrypted folder it is in (its own id for that folder), or None.
    pub e2e_root: Option<Uuid>,
    /// The drive it is in (docs/M7A_DESIGN.md), when told.
    pub drive_id: Option<String>,
    /// What the user may do with it (docs/M7B_DESIGN.md §1), when told.
    pub role: Option<Role>,
    /// In a mirror (docs/M7D_DESIGN.md), the node of the drive mirrored it
    /// copies, when told (not kept in the state: asked when needed).
    pub source_id: Option<Uuid>,
}

/// What the user may do with a node or a drive (docs/M7B_DESIGN.md §1),
/// from the least to the most, as the server orders them.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash, serde::Serialize)]
#[serde(rename_all = "snake_case")]
pub enum Role {
    /// Adds files and folders, and sees only what they added.
    UploadOnly,
    /// Reads and downloads.
    Viewer,
    Editor,
    Manager,
    Owner,
}

impl Role {
    pub fn as_str(self) -> &'static str {
        match self {
            Role::UploadOnly => "upload_only",
            Role::Viewer => "viewer",
            Role::Editor => "editor",
            Role::Manager => "manager",
            Role::Owner => "owner",
        }
    }

    pub fn parse(s: &str) -> Option<Role> {
        Some(match s {
            "upload_only" => Role::UploadOnly,
            "viewer" => Role::Viewer,
            "editor" => Role::Editor,
            "manager" => Role::Manager,
            "owner" => Role::Owner,
            _ => return None,
        })
    }

    /// Whether what is there may be changed: its content replaced, renamed,
    /// moved or deleted (the server's `Need::Change` and `Need::Delete`).
    pub fn may_change(self) -> bool {
        self >= Role::Editor
    }

    /// Whether files and folders may be added in it (`Need::Create`):
    /// upload only may, a viewer may not.
    pub fn may_add(self) -> bool {
        self != Role::Viewer
    }
}

impl From<types::Role> for Role {
    fn from(role: types::Role) -> Self {
        match role {
            types::Role::UploadOnly => Role::UploadOnly,
            types::Role::Viewer => Role::Viewer,
            types::Role::Editor => Role::Editor,
            types::Role::Manager => Role::Manager,
            types::Role::Owner => Role::Owner,
        }
    }
}

impl From<types::Node> for Node {
    fn from(node: types::Node) -> Self {
        Node {
            id: node.id,
            parent_id: node.parent_id,
            name: node.name,
            kind: kind(node.kind),
            version: node.version,
            size: Some(node.size),
            modified: Some(node.updated_at),
            e2e_root: node.e2e_root,
            drive_id: Some(node.drive_id),
            role: node.my_role.map(Role::from),
            source_id: node.source_id,
        }
    }
}

impl From<types::Entry> for Node {
    fn from(node: types::Entry) -> Self {
        Node {
            id: node.id,
            parent_id: node.parent_id,
            name: node.name,
            kind: kind(node.kind),
            version: node.version,
            size: Some(node.size),
            modified: Some(node.updated_at),
            e2e_root: node.e2e_root,
            drive_id: Some(node.drive_id),
            role: node.my_role.map(Role::from),
            source_id: node.source_id,
        }
    }
}

pub fn kind(kind: types::Kind) -> Kind {
    match kind {
        types::Kind::File => Kind::File,
        types::Kind::Folder => Kind::Folder,
    }
}

/// A file downloaded into a temporary file beside its place.
pub struct Downloaded {
    pub temp: PathBuf,
    pub version: i64,
    pub hash: String,
}

/// Where a resumable upload goes.
pub enum Target<'a> {
    New { parent: Uuid, name: &'a str },
    Replace { id: Uuid, if_version: i64 },
    DropBox { name: &'a str },
}

#[derive(Clone)]
pub struct Remote {
    pub api: api::Client,
    http: reqwest::Client,
    base: String,
    /// For the tests: a resumable upload's body breaks off after this many
    /// bytes, as a dropped connection would.
    break_after: std::sync::Arc<std::sync::Mutex<Option<u64>>>,
}

impl Remote {
    pub fn new(server: &str, token: &str) -> Result<Self, Error> {
        let http = http_client(Some(token))?;
        Ok(Remote {
            api: api::Client::new_with_client(server, http.clone()),
            http,
            base: server.trim_end_matches('/').to_string(),
            break_after: Default::default(),
        })
    }

    #[doc(hidden)]
    pub fn break_uploads_after(&self, bytes: Option<u64>) {
        *self.break_after.lock().unwrap() = bytes;
    }

    fn url(&self, path: &str) -> String {
        format!("{}{path}", self.base)
    }

    pub async fn root(&self) -> Result<Node, Error> {
        Ok(self
            .api
            .get_root()
            .await
            .map_err(Error::from_api)?
            .into_inner()
            .into())
    }

    pub async fn node(&self, id: Uuid) -> Result<Node, Error> {
        Ok(self
            .api
            .get_file(&id)
            .await
            .map_err(Error::from_api)?
            .into_inner()
            .into())
    }

    /// The user's drives (docs/M7A_DESIGN.md): My drive first, then the
    /// shared drives they are a member of, by name.
    pub async fn drives(&self) -> Result<Vec<types::Drive>, Error> {
        Ok(self
            .api
            .list_drives()
            .await
            .map_err(Error::from_api)?
            .into_inner())
    }

    /// One of the user's drives (404: not theirs, or gone).
    pub async fn drive(&self, id: &str) -> Result<types::Drive, Error> {
        Ok(self
            .api
            .get_drive(id)
            .await
            .map_err(Error::from_api)?
            .into_inner())
    }

    /// Everything in a folder, all pages.
    pub async fn children(&self, id: Uuid) -> Result<Vec<Node>, Error> {
        let mut out = Vec::new();
        let mut after: Option<String> = None;
        loop {
            let page = self
                .api
                .list_children(&id, after.as_deref(), Some(1000))
                .await
                .map_err(Error::from_api)?
                .into_inner();
            out.extend(page.items.into_iter().map(Node::from));
            match page.next {
                Some(next) => after = Some(next),
                None => return Ok(out),
            }
        }
    }

    /// The changes since a cursor, or (without one) the cursor of now.
    pub async fn changes(&self, cursor: Option<&str>) -> Result<types::Changes, Error> {
        Ok(self
            .api
            .list_changes(cursor, Some(1000))
            .await
            .map_err(Error::from_api)?
            .into_inner())
    }

    pub async fn create_folder(&self, parent: Uuid, name: &str) -> Result<Node, Error> {
        Ok(self
            .api
            .create_folder(
                &parent,
                &types::NewFolder {
                    name: name.to_string(),
                    encrypted: None,
                },
            )
            .await
            .map_err(Error::from_api)?
            .into_inner()
            .into())
    }

    pub async fn move_node(
        &self,
        id: Uuid,
        parent: Option<Uuid>,
        name: Option<&str>,
        if_version: i64,
    ) -> Result<Node, Error> {
        Ok(self
            .api
            .move_file(
                &id,
                &types::MoveFile {
                    if_version,
                    name: name.map(String::from),
                    parent_id: parent,
                },
            )
            .await
            .map_err(Error::from_api)?
            .into_inner()
            .into())
    }

    /// To the server's trash.
    /// The ids of what is in a drive's trash, as the user sees it.
    pub async fn trashed(&self, drive: &str) -> Result<Vec<Uuid>, Error> {
        Ok(self
            .api
            .list_trash(Some(drive))
            .await
            .map_err(Error::from_api)?
            .into_inner()
            .into_iter()
            .map(|item| item.id)
            .collect())
    }

    pub async fn delete(&self, id: Uuid, if_version: i64) -> Result<(), Error> {
        self.api
            .delete_file(&id, if_version)
            .await
            .map_err(Error::from_api)?;
        Ok(())
    }

    /// How many drop-box items wait for the user (the web's `WAITING`).
    pub async fn drop_box_waiting(&self) -> Result<usize, Error> {
        use types::Status as S;
        let items = self
            .api
            .list_drop_box_items(None, Some(500), None)
            .await
            .map_err(Error::from_api)?
            .into_inner();
        Ok(items
            .iter()
            .filter(|item| {
                matches!(
                    item.status,
                    S::Proposed | S::NeedsManual | S::Rejected | S::Failed | S::Waiting | S::Undone
                )
            })
            .count())
    }

    /// A file into a temporary file in `dir` (hidden, ignored by the scans),
    /// hashed on the way: renamed into place by the caller.
    pub async fn download(&self, id: Uuid, dir: &Path) -> Result<Downloaded, Error> {
        let mut response = self
            .http
            .get(self.url(&format!("/api/files/{id}/content")))
            .send()
            .await
            .map_err(Error::from_reqwest)?;
        if !response.status().is_success() {
            return Err(error_of(response).await);
        }
        let version = response
            .headers()
            .get(header::ETAG)
            .and_then(|v| v.to_str().ok())
            .and_then(|v| v.trim_matches('"').parse().ok())
            .ok_or_else(|| Error::Invalid("A download without its version".into()))?;
        tokio::fs::create_dir_all(dir).await?;
        let temp = dir.join(format!(
            "{}download-{}",
            crate::ignore::OWN_PREFIX,
            Uuid::new_v4().simple()
        ));
        let mut file = tokio::fs::File::create(&temp).await?;
        let mut hasher = blake3::Hasher::new();
        let result: Result<(), Error> = async {
            while let Some(chunk) = response.chunk().await.map_err(Error::from_reqwest)? {
                hasher.update(&chunk);
                file.write_all(&chunk).await?;
            }
            file.sync_all().await?;
            Ok(())
        }
        .await;
        if let Err(e) = result {
            let _ = tokio::fs::remove_file(&temp).await;
            return Err(e);
        }
        Ok(Downloaded {
            temp,
            version,
            hash: hasher.finalize().to_hex().to_string(),
        })
    }

    /// A range of a file at a version, for a placeholder's fetch: the
    /// response, read a chunk at a time by the caller. Another version (it
    /// changed since) is a conflict.
    pub async fn download_range(
        &self,
        id: Uuid,
        version: i64,
        offset: u64,
        length: u64,
    ) -> Result<reqwest::Response, Error> {
        let last = offset + length.max(1) - 1;
        let response = self
            .http
            .get(self.url(&format!("/api/files/{id}/content")))
            .header(header::RANGE, format!("bytes={offset}-{last}"))
            .send()
            .await
            .map_err(Error::from_reqwest)?;
        if !response.status().is_success() {
            return Err(error_of(response).await);
        }
        let served: Option<i64> = response
            .headers()
            .get(header::ETAG)
            .and_then(|v| v.to_str().ok())
            .and_then(|v| v.trim_matches('"').parse().ok());
        if served != Some(version) {
            return Err(Error::Conflict(
                "The file changed on the server since".into(),
            ));
        }
        // A whole file instead of the range only does from its start.
        if response.status().as_u16() != 206 && offset != 0 {
            return Err(Error::Invalid("The server did not send the range".into()));
        }
        Ok(response)
    }

    /// A new file under `parent`, plainly (multipart) if small.
    pub async fn upload_new_plain(
        &self,
        parent: Uuid,
        name: &str,
        path: &Path,
    ) -> Result<Node, Error> {
        let bytes = tokio::fs::read(path).await?;
        let part = reqwest::multipart::Part::bytes(bytes).file_name(name.to_string());
        let form = reqwest::multipart::Form::new().part("file", part);
        let response = self
            .http
            .post(self.url(&format!("/api/files/{parent}/files")))
            .multipart(form)
            .send()
            .await
            .map_err(Error::from_reqwest)?;
        node_of(response).await
    }

    /// A file's new content at its version (`If-Match`), plainly if small.
    pub async fn replace_plain(
        &self,
        id: Uuid,
        if_version: i64,
        path: &Path,
    ) -> Result<Node, Error> {
        let bytes = tokio::fs::read(path).await?;
        let response = self
            .http
            .put(self.url(&format!("/api/files/{id}/content")))
            .header(header::IF_MATCH, format!("\"{if_version}\""))
            .header(header::CONTENT_TYPE, "application/octet-stream")
            .body(bytes)
            .send()
            .await
            .map_err(Error::from_reqwest)?;
        node_of(response).await
    }

    /// Start a resumable upload: its URL (`Location`), with no bytes yet.
    pub async fn create_upload(&self, target: &Target<'_>, length: u64) -> Result<String, Error> {
        let query: Vec<(&str, String)> = match target {
            Target::New { parent, name } => {
                vec![
                    ("parent_id", parent.to_string()),
                    ("name", name.to_string()),
                ]
            }
            Target::Replace { id, if_version } => {
                vec![
                    ("replaces", id.to_string()),
                    ("if_version", if_version.to_string()),
                ]
            }
            Target::DropBox { name } => {
                vec![("drop_box", "true".into()), ("name", name.to_string())]
            }
        };
        let response = self
            .http
            .post(self.url("/api/uploads"))
            .query(&query)
            .header("upload-complete", "?0")
            .header("upload-length", length.to_string())
            .header("upload-draft-interop-version", "6")
            .header(header::CONTENT_TYPE, "application/octet-stream")
            .body(Vec::new())
            .send()
            .await
            .map_err(Error::from_reqwest)?;
        if response.status().as_u16() != 201 {
            return Err(error_of(response).await);
        }
        let location = response
            .headers()
            .get(header::LOCATION)
            .and_then(|v| v.to_str().ok())
            .ok_or_else(|| Error::Invalid("An upload without its Location".into()))?;
        Ok(if location.starts_with("http") {
            location.to_string()
        } else {
            self.url(location)
        })
    }

    /// Where an upload is: the bytes the server has, or None when it is
    /// gone (expired, or done with).
    pub async fn upload_offset(&self, url: &str) -> Result<Option<u64>, Error> {
        let response = self
            .http
            .head(url)
            .header("upload-draft-interop-version", "6")
            .send()
            .await
            .map_err(Error::from_reqwest)?;
        match response.status().as_u16() {
            204 | 200 => Ok(response
                .headers()
                .get("upload-offset")
                .and_then(|v| v.to_str().ok())
                .and_then(|v| v.parse().ok())),
            404 => Ok(None),
            _ => Err(error_of(response).await),
        }
    }

    /// The rest of a file from `offset`, as the last bytes of the upload:
    /// what the upload made, as JSON (the file, or the drop-box item). A
    /// server at another offset (it was still taking a broken request's
    /// bytes when asked) says which: the rest is sent from there.
    pub async fn finish_upload(
        &self,
        url: &str,
        path: &Path,
        mut offset: u64,
    ) -> Result<serde_json::Value, Error> {
        for _ in 0..3 {
            match self.send_rest(url, path, offset).await? {
                Ok(made) => return Ok(made),
                Err(at) => offset = at,
            }
        }
        Err(Error::Invalid("The upload's offset keeps moving".into()))
    }

    async fn send_rest(
        &self,
        url: &str,
        path: &Path,
        offset: u64,
    ) -> Result<Result<serde_json::Value, u64>, Error> {
        let mut file = tokio::fs::File::open(path).await?;
        file.seek(io::SeekFrom::Start(offset)).await?;
        let length = file.metadata().await?.len().saturating_sub(offset);
        let stream = FileStream {
            file,
            buffer: vec![0; 256 * 1024],
            left: self.break_after.lock().unwrap().map(|b| b.min(length)),
        };
        let response = self
            .http
            .patch(url)
            .header("upload-complete", "?1")
            .header("upload-offset", offset.to_string())
            .header("upload-draft-interop-version", "6")
            .header(header::CONTENT_TYPE, "application/partial-upload")
            .header(header::CONTENT_LENGTH, length.to_string())
            .body(reqwest::Body::wrap_stream(stream))
            .send()
            .await
            .map_err(Error::from_reqwest)?;
        if response.status().as_u16() == 409
            && let Some(at) = response
                .headers()
                .get("upload-offset")
                .and_then(|v| v.to_str().ok())
                .and_then(|v| v.parse().ok())
        {
            return Ok(Err(at));
        }
        if response.status().as_u16() != 201 {
            return Err(error_of(response).await);
        }
        Ok(Ok(response.json().await.map_err(Error::from_reqwest)?))
    }

    /// Any request of the API (`/api/...`), answered as it comes: its
    /// status, content type and body. The desktop window's SDK goes through
    /// this, so that the token stays out of the web view.
    pub async fn request(
        &self,
        method: &str,
        path: &str,
        content_type: Option<&str>,
        body: Option<Vec<u8>>,
    ) -> Result<(u16, Option<String>, Vec<u8>), Error> {
        if !path.starts_with("/api/") || path.contains("..") {
            return Err(Error::Invalid("Only the API's paths".into()));
        }
        let method = reqwest::Method::from_bytes(method.as_bytes())
            .map_err(|_| Error::Invalid("An invalid method".into()))?;
        let mut request = self.http.request(method, self.url(path));
        if let Some(content_type) = content_type {
            request = request.header(header::CONTENT_TYPE, content_type);
        }
        if let Some(body) = body {
            request = request.body(body);
        }
        let response = request.send().await.map_err(Error::from_reqwest)?;
        let status = response.status().as_u16();
        let content_type = response
            .headers()
            .get(header::CONTENT_TYPE)
            .and_then(|v| v.to_str().ok())
            .map(String::from);
        let body = response.bytes().await.map_err(Error::from_reqwest)?;
        Ok((status, content_type, body.to_vec()))
    }

    pub async fn cancel_upload(&self, url: &str) {
        let _ = self.http.delete(url).send().await;
    }

    /// The live events (`GET /api/events`): the response, read by the
    /// engine a chunk at a time.
    pub async fn events(&self) -> Result<reqwest::Response, Error> {
        let response = self
            .http
            .get(self.url("/api/events"))
            .header(header::ACCEPT, "text/event-stream")
            .send()
            .await
            .map_err(Error::from_reqwest)?;
        if !response.status().is_success() {
            return Err(error_of(response).await);
        }
        Ok(response)
    }
}

/// The error of an answer that is not a success, with the API's message.
async fn error_of(response: reqwest::Response) -> Error {
    let status = response.status().as_u16();
    match response.json::<types::ErrorBody>().await {
        Ok(body) => Error::from_answer(status, body.error, body.code.as_deref()),
        Err(_) => Error::from_status(status, String::new()),
    }
}

async fn node_of(response: reqwest::Response) -> Result<Node, Error> {
    if !response.status().is_success() {
        return Err(error_of(response).await);
    }
    let node: types::Node = response.json().await.map_err(Error::from_reqwest)?;
    Ok(node.into())
}

/// A file as a request body, a chunk at a time. Written out here: tokio
/// has no stream of a file without tokio-util, and this is all of it.
struct FileStream {
    file: tokio::fs::File,
    buffer: Vec<u8>,
    /// For the tests: the bytes left before the body breaks off.
    left: Option<u64>,
}

impl futures_core::Stream for FileStream {
    type Item = Result<Bytes, io::Error>;

    fn poll_next(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Option<Self::Item>> {
        let this = &mut *self;
        if this.left == Some(0) {
            return Poll::Ready(Some(Err(io::Error::new(
                io::ErrorKind::ConnectionReset,
                "the upload broke off (test)",
            ))));
        }
        let max = match this.left {
            Some(left) => (left as usize).min(this.buffer.len()),
            None => this.buffer.len(),
        };
        let mut buf = ReadBuf::new(&mut this.buffer[..max]);
        match Pin::new(&mut this.file).poll_read(cx, &mut buf) {
            Poll::Pending => Poll::Pending,
            Poll::Ready(Err(e)) => Poll::Ready(Some(Err(e))),
            Poll::Ready(Ok(())) => {
                let filled = buf.filled();
                if filled.is_empty() {
                    return Poll::Ready(None);
                }
                if let Some(left) = this.left.as_mut() {
                    *left -= filled.len() as u64;
                }
                Poll::Ready(Some(Ok(Bytes::copy_from_slice(filled))))
            }
        }
    }
}
