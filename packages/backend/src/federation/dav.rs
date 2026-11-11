//! Our WebDAV for other servers (docs/M7E_DESIGN.md §4): a share at
//! `/ocm/dav/<share>/`, with a bearer token of our code flow (or, for a
//! share made without `must-exchange-token`, its shared secret). The
//! share's top is the root; what its recipient may do anywhere in it is
//! `outgoing::role_in` (the role, limited access, the re-share rule,
//! upload only, encrypted folders), and the drive's storage answers as it
//! does for a local user (a backend that fails is a 503).
//!
//! Reads come from the index and the storage layer; every change goes
//! through `files::ops` as the API's do (a PUT to the uploads folder,
//! placed whole; a DELETE to the drive's trash; a MOVE keeping the id).
//! A REPORT `sync-collection` (RFC 6578) answers what changed since a
//! sync-token, from the share's change log (`ocm_changes`); to a share of
//! an encrypted folder, with each member's id (`o:id`, `organizer-e2e`).

use super::outgoing::{OcmShare, role_in, share_row};
use crate::dav::{
    Entries, IndexMeta, MountEntry, RemoteFile, Target, Upload, Writer, encode, fs_error, nfc,
    parts, split,
};
use crate::files::shares::Need;
use crate::files::{Files, Kind, NODE_COLUMNS, Node};
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
use std::io::{Seek, SeekFrom};
use std::sync::Arc;
use uuid::Uuid;

/// A request to a share's WebDAV.
pub async fn handle(State(state): State<AppState>, request: Request) -> Response {
    let path = request.uri().path().to_string();
    let Some(rest) = path.strip_prefix(&format!("{}/", super::DAV_PREFIX)) else {
        return StatusCode::NOT_FOUND.into_response();
    };
    let segment = rest.split('/').next().unwrap_or_default();
    let Ok(share_id) = Uuid::parse_str(segment) else {
        return StatusCode::NOT_FOUND.into_response();
    };
    let Some(share) = authenticate(&state.files, share_id, request.headers()).await else {
        return (
            StatusCode::UNAUTHORIZED,
            [(
                header::WWW_AUTHENTICATE,
                HeaderValue::from_static("Bearer realm=\"Organizer\""),
            )],
            "A token of this server's code flow is required",
        )
            .into_response();
    };
    let top = match super::outgoing::top_of(&state.files, &share).await {
        Ok(top) => top,
        Err(e) => return crate::error::HttpError::from(e).into_response(),
    };
    // The top itself may have gone out of reach (limited, the giver's
    // access lost, an encrypted folder): nothing is there then.
    match role_in(&state.files, &share, top.id).await {
        Ok(Some(_)) => {}
        Ok(None) => return StatusCode::NOT_FOUND.into_response(),
        Err(e) => return crate::error::HttpError::from(e).into_response(),
    }
    let prefix = format!("{}/{segment}", super::DAV_PREFIX);
    let fs = ShareFs {
        files: state.files.clone(),
        share: Arc::new(share),
        top,
    };
    if request.method().as_str() == "REPORT" {
        return report(&fs, &prefix, request).await;
    }
    // A MOVE or COPY stays in the share.
    if let Some(destination) = request
        .headers()
        .get("destination")
        .and_then(|v| v.to_str().ok())
        .and_then(crate::dav::destination_path)
        && !destination.starts_with(&format!("{prefix}/"))
        && destination != prefix
    {
        return StatusCode::FORBIDDEN.into_response();
    }
    let handler = dav_server::DavHandler::builder()
        .filesystem(Box::new(fs))
        .locksystem(crate::dav::locks())
        .strip_prefix(prefix)
        .hide_symlinks(true)
        .build_handler();
    handler.handle(request).await.map(Body::new)
}

/// The share a request's bearer token opens: a token of our code flow
/// for this share, or the share's own secret when it was made without
/// `must-exchange-token`; while federation and the drive allow it and the
/// recipient's server is not refused.
async fn authenticate(
    files: &Arc<Files>,
    share_id: Uuid,
    headers: &axum::http::HeaderMap,
) -> Option<OcmShare> {
    let value = headers.get(header::AUTHORIZATION)?.to_str().ok()?;
    let (scheme, token) = value.split_once(' ')?;
    if !scheme.eq_ignore_ascii_case("bearer") {
        return None;
    }
    let token = token.trim();
    let federation = files.federation();
    let share = match federation.redeem(token) {
        Some(id) if id == share_id => share_row(files.db().reader(), id).await.ok()??,
        Some(_) => return None,
        None => {
            let share = share_row(files.db().reader(), share_id).await.ok()??;
            let hash = crate::util::sha256_hex(token);
            let secret: Option<String> =
                sqlx::query_scalar("SELECT secret_hash FROM ocm_shares WHERE id = ?1")
                    .bind(share_id)
                    .fetch_optional(files.db().reader())
                    .await
                    .ok()?;
            if share.strict || secret.as_deref() != Some(hash.as_str()) {
                return None;
            }
            share
        }
    };
    if share.state == super::outgoing::ShareState::Declined {
        return None;
    }
    if !matches!(
        federation.trust(&share.host).await.ok()?,
        super::Trust::Allowed { .. }
    ) {
        return None;
    }
    if !super::outgoing::drive_allows(files, share.drive_id)
        .await
        .ok()?
    {
        return None;
    }
    Some(share)
}

/// A share as dav-server's file system: its top is the root.
#[derive(Clone)]
struct ShareFs {
    files: Arc<Files>,
    share: Arc<OcmShare>,
    top: Node,
}

impl ShareFs {
    /// The node at a path, if the share lets its recipient see it, with
    /// the role they have there.
    async fn node(&self, path: &DavPath) -> FsResult<(Node, crate::files::roles::Role)> {
        let names = parts(&nfc(path));
        let mut node = self.top.clone();
        for name in names {
            if node.kind != Kind::Folder {
                return Err(FsError::NotFound);
            }
            node = child_named(&self.files, node.id, &name)
                .await
                .map_err(fs_error)?
                .ok_or(FsError::NotFound)?;
        }
        let role = role_in(&self.files, &self.share, node.id)
            .await
            .map_err(fs_error)?
            .ok_or(FsError::NotFound)?;
        Ok((node, role))
    }

    /// The node at a path, refused unless the role there allows `need`.
    async fn node_for(&self, path: &DavPath, need: Need) -> FsResult<Node> {
        let (node, role) = self.node(path).await?;
        if !role.allows(need) {
            return Err(FsError::Forbidden);
        }
        Ok(node)
    }

    /// The visible children of a folder.
    async fn children(&self, folder: &Node) -> FsResult<Vec<Node>> {
        let children: Vec<Node> = sqlx::query_as(sqlx::AssertSqlSafe(format!(
            "SELECT {NODE_COLUMNS} FROM nodes WHERE parent_id = ?1 ORDER BY name"
        )))
        .bind(folder.id)
        .fetch_all(self.files.db().reader())
        .await
        .map_err(|e| fs_error(e.into()))?;
        let mut seen = Vec::with_capacity(children.len());
        for child in children {
            if role_in(&self.files, &self.share, child.id)
                .await
                .map_err(fs_error)?
                .is_some()
            {
                seen.push(child);
            }
        }
        Ok(seen)
    }

    async fn remove(&self, path: &DavPath) -> FsResult<()> {
        let node = self.node_for(path, Need::Delete).await?;
        if node.id == self.top.id {
            return Err(FsError::Forbidden);
        }
        self.files
            .trash(self.share.drive_id, node.id, node.version)
            .await
            .map(|_| ())
            .map_err(fs_error)
    }
}

/// A child of a folder by name (as stored, or in NFC).
async fn child_named(
    files: &Files,
    parent: Uuid,
    name: &str,
) -> Result<Option<Node>, crate::files::FileError> {
    Ok(sqlx::query_as(sqlx::AssertSqlSafe(format!(
        "SELECT {NODE_COLUMNS} FROM nodes WHERE parent_id = ?1 AND name IN (?2, ?3)"
    )))
    .bind(parent)
    .bind(name)
    .bind(crate::files::names::nfc(name))
    .fetch_optional(files.db().reader())
    .await?)
}

impl DavFileSystem for ShareFs {
    fn open<'a>(
        &'a self,
        path: &'a DavPath,
        options: OpenOptions,
    ) -> FsFuture<'a, Box<dyn DavFile>> {
        Box::pin(async move {
            if !(options.write || options.append || options.truncate || options.create) {
                let (node, _) = self.node(path).await?;
                if node.kind == Kind::Folder {
                    return Err(FsError::Forbidden);
                }
                let storage = self
                    .files
                    .storage(self.share.drive_id)
                    .await
                    .map_err(fs_error)?;
                let rel = self.files.rel_path(node.id).await.map_err(fs_error)?;
                let (reader, size) = storage.reader(&rel).await.map_err(|e| fs_error(e.into()))?;
                return Ok(Box::new(RemoteFile {
                    reader,
                    size,
                    at: 0,
                    node,
                }) as Box<dyn DavFile>);
            }
            let existing = match self.node(path).await {
                Ok((node, _)) if node.kind == Kind::Folder => return Err(FsError::Forbidden),
                Ok((node, _)) => Some(node),
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
                    self.node_for(path, Need::Change).await?;
                    Target::Replace(node.id)
                }
                None => {
                    let (parent, name) = split(&nfc(path))?;
                    let parent = DavPath::new(&encode(&format!("/{}", parent.to_string_lossy())))
                        .map_err(|_| FsError::NotFound)?;
                    let parent = self.node_for(&parent, Need::Create).await?;
                    if parent.kind != Kind::Folder {
                        return Err(FsError::Forbidden);
                    }
                    Target::New(parent.id, name)
                }
            };
            std::fs::create_dir_all(self.files.uploads_dir())?;
            let temp = self
                .files
                .uploads_dir()
                .join(format!("ocm-{}", Uuid::now_v7()));
            let keeps = match &existing {
                Some(node) if !options.truncate => {
                    let storage = self
                        .files
                        .storage(self.share.drive_id)
                        .await
                        .map_err(fs_error)?;
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
                writer: Writer::Share(Box::new((*self.share).clone())),
                target,
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
            let (folder, _) = self.node(path).await?;
            if folder.kind != Kind::Folder {
                return Err(FsError::Forbidden);
            }
            let entries = self
                .children(&folder)
                .await?
                .into_iter()
                .map(|node| {
                    Ok(Box::new(MountEntry {
                        name: node.name.clone().into_bytes(),
                        meta: Box::new(IndexMeta(node)),
                    }) as Box<dyn DavDirEntry>)
                })
                .collect();
            Ok(Box::pin(Entries(entries)) as FsStream<Box<dyn DavDirEntry>>)
        })
    }

    fn metadata<'a>(&'a self, path: &'a DavPath) -> FsFuture<'a, Box<dyn DavMetaData>> {
        Box::pin(async move {
            let (node, _) = self.node(path).await?;
            Ok(Box::new(IndexMeta(node)) as Box<dyn DavMetaData>)
        })
    }

    fn create_dir<'a>(&'a self, path: &'a DavPath) -> FsFuture<'a, ()> {
        Box::pin(async move {
            let (parent, name) = split(&nfc(path))?;
            let parent = DavPath::new(&encode(&format!("/{}", parent.to_string_lossy())))
                .map_err(|_| FsError::NotFound)?;
            let parent = self.node_for(&parent, Need::Create).await?;
            if parent.kind != Kind::Folder {
                return Err(FsError::Forbidden);
            }
            let folder = self
                .files
                .create_folder(self.share.drive_id, parent.id, &name)
                .await
                .map_err(fs_error)?;
            super::outgoing::mark_created(&self.files, folder.id, self.share.id)
                .await
                .map_err(|e| fs_error(e.into()))
        })
    }

    fn remove_dir<'a>(&'a self, path: &'a DavPath) -> FsFuture<'a, ()> {
        Box::pin(async move { self.remove(path).await })
    }

    fn remove_file<'a>(&'a self, path: &'a DavPath) -> FsFuture<'a, ()> {
        Box::pin(async move { self.remove(path).await })
    }

    fn rename<'a>(&'a self, from: &'a DavPath, to: &'a DavPath) -> FsFuture<'a, ()> {
        Box::pin(async move {
            let node = self.node_for(from, Need::Change).await?;
            if node.id == self.top.id {
                return Err(FsError::Forbidden);
            }
            let (parent, name) = split(&nfc(to))?;
            let parent = DavPath::new(&encode(&format!("/{}", parent.to_string_lossy())))
                .map_err(|_| FsError::NotFound)?;
            let parent = self.node_for(&parent, Need::Create).await?;
            self.files
                .move_node(
                    self.share.drive_id,
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
}

/// A REPORT: `sync-collection` (RFC 6578) on a shared folder, anything
/// else is not supported.
async fn report(fs: &ShareFs, prefix: &str, request: Request) -> Response {
    let rel = request
        .uri()
        .path()
        .strip_prefix(prefix)
        .unwrap_or_default()
        .to_string();
    let body = match axum::body::to_bytes(request.into_body(), 64 * 1024).await {
        Ok(body) => String::from_utf8_lossy(&body).into_owned(),
        Err(_) => return StatusCode::PAYLOAD_TOO_LARGE.into_response(),
    };
    if !body.contains("sync-collection") {
        return dav_error(StatusCode::FORBIDDEN, "supported-report");
    }
    // Only on the share's top folder.
    if !(rel.is_empty() || rel == "/") || fs.top.kind != Kind::Folder {
        return dav_error(StatusCode::FORBIDDEN, "supported-report");
    }
    let token = element_text(&body, "sync-token").unwrap_or_default();
    let infinite = element_text(&body, "sync-level")
        .is_some_and(|l| l.trim().eq_ignore_ascii_case("infinite"));
    let Some(base) = fs.files.federation().base() else {
        return StatusCode::NOT_FOUND.into_response();
    };
    let token_prefix = format!("{base}/ocm/sync/{}/", fs.share.id);
    let since = if token.trim().is_empty() {
        None
    } else {
        match token
            .trim()
            .strip_prefix(&token_prefix)
            .and_then(|s| s.parse::<i64>().ok())
        {
            Some(seq) if seq >= fs.share.sync_floor => Some(seq),
            _ => return dav_error(StatusCode::FORBIDDEN, "valid-sync-token"),
        }
    };
    let top_seq: i64 = match sqlx::query_scalar::<_, Option<i64>>(
        "SELECT max(seq) FROM ocm_changes WHERE share_id = ?1",
    )
    .bind(fs.share.id)
    .fetch_one(fs.files.db().reader())
    .await
    {
        Ok(seq) => seq.unwrap_or(0).max(fs.share.sync_floor),
        Err(_) => return StatusCode::INTERNAL_SERVER_ERROR.into_response(),
    };
    // An encrypted folder (`organizer-e2e`): its names are sealed with our
    // ids, so the member's server is given them (`o:id`, the top's
    // `o:root-id`) for its index (`e2e::sync_ids`).
    let ids = fs.share.e2e;
    let mut out = if ids {
        format!(
            r#"<?xml version="1.0" encoding="utf-8"?><d:multistatus xmlns:d="DAV:" xmlns:o="{}"><o:root-id>{}</o:root-id>"#,
            super::e2e::DAV_NS,
            fs.top.id
        )
    } else {
        String::from(r#"<?xml version="1.0" encoding="utf-8"?><d:multistatus xmlns:d="DAV:">"#)
    };
    match since {
        // Everything (the first sync).
        None => {
            let mut folders = vec![(fs.top.clone(), String::new())];
            while let Some((folder, at)) = folders.pop() {
                let Ok(children) = fs.children(&folder).await else {
                    return StatusCode::INTERNAL_SERVER_ERROR.into_response();
                };
                for child in children {
                    let path = crate::storage::join(&at, &child.name);
                    out.push_str(&found(prefix, &path, &child, ids));
                    if infinite && child.kind == Kind::Folder {
                        folders.push((child, path));
                    }
                }
            }
        }
        Some(since) => {
            let rows: Vec<(String, bool)> = match sqlx::query_as(
                "SELECT path, gone FROM ocm_changes WHERE share_id = ?1 AND seq > ?2 ORDER BY seq",
            )
            .bind(fs.share.id)
            .bind(since)
            .fetch_all(fs.files.db().reader())
            .await
            {
                Ok(rows) => rows,
                Err(_) => return StatusCode::INTERNAL_SERVER_ERROR.into_response(),
            };
            let mut paths: Vec<String> = Vec::new();
            for (path, _) in rows {
                if path.is_empty() || (!infinite && path.contains('/')) {
                    continue;
                }
                if !paths.contains(&path) {
                    paths.push(path);
                }
            }
            for path in paths {
                let at = match DavPath::new(&encode(&format!("/{path}"))) {
                    Ok(at) => at,
                    Err(_) => continue,
                };
                match fs.node(&at).await {
                    Ok((node, _)) => out.push_str(&found(prefix, &path, &node, ids)),
                    Err(_) => out.push_str(&format!(
                        "<d:response><d:href>{}</d:href><d:status>HTTP/1.1 404 Not Found</d:status></d:response>",
                        href(prefix, &path, false)
                    )),
                }
            }
        }
    }
    out.push_str(&format!(
        "<d:sync-token>{}</d:sync-token></d:multistatus>",
        xml_escape(&format!("{token_prefix}{top_seq}"))
    ));
    (
        StatusCode::MULTI_STATUS,
        [(header::CONTENT_TYPE, "application/xml; charset=utf-8")],
        out,
    )
        .into_response()
}

/// A member's href under the share.
fn href(prefix: &str, path: &str, folder: bool) -> String {
    let encoded: Vec<String> = path.split('/').map(crate::util::form_encode).collect();
    let mut href = format!("{prefix}/{}", encoded.join("/"));
    if folder {
        href.push('/');
    }
    xml_escape(&href)
}

/// A member that is there, with its properties (and its id, `ids`).
fn found(prefix: &str, path: &str, node: &Node, ids: bool) -> String {
    let folder = node.kind == Kind::Folder;
    let modified = node
        .updated_at
        .parse::<jiff::Timestamp>()
        .ok()
        .and_then(|t| {
            jiff::fmt::rfc2822::DateTimePrinter::new()
                .timestamp_to_rfc9110_string(&t)
                .ok()
        })
        .unwrap_or_default();
    let mut props = format!(
        "<d:getetag>\"{}-{}\"</d:getetag><d:getlastmodified>{}</d:getlastmodified>",
        node.id,
        node.version,
        xml_escape(&modified)
    );
    if ids {
        props.push_str(&format!("<o:id>{}</o:id>", node.id));
    }
    if folder {
        props.push_str("<d:resourcetype><d:collection/></d:resourcetype>");
    } else {
        props.push_str(&format!(
            "<d:resourcetype/><d:getcontentlength>{}</d:getcontentlength>",
            node.size
        ));
    }
    format!(
        "<d:response><d:href>{}</d:href><d:propstat><d:prop>{props}</d:prop><d:status>HTTP/1.1 200 OK</d:status></d:propstat></d:response>",
        href(prefix, path, folder)
    )
}

/// A WebDAV precondition error (RFC 4918 §16).
fn dav_error(status: StatusCode, condition: &str) -> Response {
    (
        status,
        [(header::CONTENT_TYPE, "application/xml; charset=utf-8")],
        format!(
            r#"<?xml version="1.0" encoding="utf-8"?><d:error xmlns:d="DAV:"><d:{condition}/></d:error>"#
        ),
    )
        .into_response()
}

pub(crate) fn xml_escape(text: &str) -> String {
    text.replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
        .replace('"', "&quot;")
}

fn xml_unescape(text: &str) -> String {
    text.replace("&lt;", "<")
        .replace("&gt;", ">")
        .replace("&quot;", "\"")
        .replace("&apos;", "'")
        .replace("&amp;", "&")
}

/// The text of the first element of that local name, whatever its
/// namespace prefix (`<d:sync-token>…</d:sync-token>`); an empty element
/// is an empty text. Enough of XML for RFC 6578's few elements: written
/// here rather than taking an XML crate as one more dependency.
pub(crate) fn element_text(xml: &str, local: &str) -> Option<String> {
    element_from(xml, local, 0).map(|(text, _)| text)
}

/// [`element_text`] from an offset: the text and where the element ends.
fn element_from(xml: &str, local: &str, from: usize) -> Option<(String, usize)> {
    let mut from = from;
    while let Some(at) = xml[from..].find('<') {
        let start = from + at;
        let end = start + xml[start..].find('>')?;
        let tag = &xml[start + 1..end];
        from = end + 1;
        if tag.starts_with('/') || tag.starts_with('?') || tag.starts_with('!') {
            continue;
        }
        let name = tag
            .split(|c: char| c.is_whitespace() || c == '/')
            .next()
            .unwrap_or_default();
        let name_local = name.rsplit(':').next().unwrap_or_default();
        if name_local != local {
            continue;
        }
        if tag.ends_with('/') {
            return Some((String::new(), from));
        }
        let close = xml[from..].find(&format!("</{name}"))?;
        return Some((xml_unescape(xml[from..from + close].trim()), from + close));
    }
    None
}

/// Every element of that local name's text (the `href`s of a
/// multistatus).
pub(crate) fn elements_text(xml: &str, local: &str) -> Vec<String> {
    let mut out = Vec::new();
    let mut from = 0;
    while let Some((text, end)) = element_from(xml, local, from) {
        out.push(text);
        from = end;
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn elements_are_found_by_their_local_name() {
        let xml = r#"<?xml version="1.0"?><D:sync-collection xmlns:D="DAV:"><D:sync-token>https://a/ocm/sync/x/3</D:sync-token><D:sync-level>1</D:sync-level><D:prop><D:getetag/></D:prop></D:sync-collection>"#;
        assert_eq!(
            element_text(xml, "sync-token").as_deref(),
            Some("https://a/ocm/sync/x/3")
        );
        assert_eq!(element_text(xml, "sync-level").as_deref(), Some("1"));
        assert_eq!(element_text(xml, "getetag").as_deref(), Some(""));
        assert_eq!(element_text(xml, "nothing"), None);
        let empty = r#"<d:sync-collection xmlns:d="DAV:"><d:sync-token/></d:sync-collection>"#;
        assert_eq!(element_text(empty, "sync-token").as_deref(), Some(""));
        let multi = "<d:multistatus><d:response><d:href>/a</d:href></d:response><d:response><d:href>/b&amp;c</d:href></d:response></d:multistatus>";
        assert_eq!(elements_text(multi, "href"), ["/a", "/b&c"]);
    }
}
