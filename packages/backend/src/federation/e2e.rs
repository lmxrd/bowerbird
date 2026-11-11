//! The key exchange of `organizer-e2e` (docs/M7E_DESIGN.md §6, §10,
//! docs/E2E_SPEC.md §12): an encrypted folder shared with someone on
//! another Organizer server.
//!
//! - **On the owner's server**, the member is the share with them (an
//!   `ocm_shares` row with `e2e`; its id stands for them where a user's id
//!   would, in `/encryption/folders/{id}` and `/encryption/pending`). Their
//!   public key is asked of their server ([`public_key`], signed, kept once
//!   answered); the owner's devices wrap the folder's keys for them as for
//!   a local member, kept here (`e2e_remote_keys`); a share that ends
//!   takes them and the folder needs a new key ([`remote_gone`]).
//! - **Their server** asks for them, signed, whenever its user's device
//!   does ([`fetch_keys`], answered by [`answer_keys`]), and hands them out
//!   through its own `/encryption/folders/{id}`, with the owner's folder id
//!   to unwrap with. Names are sealed with the owner's ids: our WebDAV's
//!   sync-collection carries each node's id (`o:id`) and the top's
//!   (`o:root-id`) to a share marked `e2e`, and the member's server keeps
//!   them as `source_id` of its index ([`sync_ids`]), so its clients open
//!   the folder as they open a mirror's copy.
//!
//! Both endpoints are POSTs under `/ocm/organizer-e2e/`, signed as every
//! request between servers is (`Federation::check_signed`), and answered
//! only to a server that advertises `organizer-e2e`.

use super::address::host_of;
use super::incoming::{Answer, Received, ShareToken, answer};
use super::outgoing::{OcmShare, ShareState, role_in, share_row};
use super::{Trust, dav};
use crate::error::HttpError;
use crate::files::{FileError, Files};
use axum::http::StatusCode;
use serde::Deserialize;
use std::sync::Arc;
use std::time::{Duration, Instant};
use uuid::Uuid;

/// Where the two endpoints are, under the OCM `endPoint`.
pub const PUBLIC_KEY_PATH: &str = "/organizer-e2e/public-key";
pub const KEYS_PATH: &str = "/organizer-e2e/keys";

/// The XML namespace of our WebDAV properties (`o:id`, `o:root-id`).
pub const DAV_NS: &str = "urn:x-organizer:dav";

/// How long a member's server that did not give a public key is not asked
/// again.
const ASK_AGAIN: Duration = Duration::from_secs(60);

/// A member of an encrypted folder on another server: the share with them.
pub struct RemoteMember {
    pub share_id: Uuid,
    pub address: String,
    /// Null while their server has none to give (not set up there yet).
    pub public_key: Option<String>,
}

/// The members on other servers of an encrypted folder (`root`): its
/// shares marked `e2e` that are not declined and still reach it (the
/// giver's access, limited access, federation allowed on the drive).
pub async fn remote_members(files: &Files, root: Uuid) -> Result<Vec<RemoteMember>, FileError> {
    let ids: Vec<Uuid> = sqlx::query_scalar(
        "SELECT id FROM ocm_shares WHERE node_id = ?1 AND e2e = 1 AND state != 'declined'
         ORDER BY created_at, id",
    )
    .bind(root)
    .fetch_all(files.db().reader())
    .await?;
    let mut members = Vec::new();
    for id in ids {
        let Some(share) = share_row(files.db().reader(), id).await? else {
            continue;
        };
        if role_in(files, &share, root).await?.is_none()
            || !super::outgoing::drive_allows(files, share.drive_id).await?
        {
            continue;
        }
        let public_key: Option<String> =
            sqlx::query_scalar("SELECT public_key FROM ocm_shares WHERE id = ?1")
                .bind(id)
                .fetch_optional(files.db().reader())
                .await?
                .flatten();
        members.push(RemoteMember {
            share_id: id,
            address: share.address,
            public_key,
        });
    }
    Ok(members)
}

/// Ask the members on other servers who have no public key here yet for
/// theirs (at most once a minute each): before their keys are listed.
pub async fn ask_public_keys(files: &Files, members: &mut [RemoteMember]) {
    for member in members.iter_mut().filter(|m| m.public_key.is_none()) {
        if let Ok(Some(share)) = share_row(files.db().reader(), member.share_id).await {
            member.public_key = public_key(files, &share).await;
        }
    }
}

/// A member's public key, asked of their server (signed) unless kept:
/// the share's id and their address, answered with the key of the user
/// it was shared with there, or null.
pub async fn public_key(files: &Files, share: &OcmShare) -> Option<String> {
    let kept: Option<String> =
        sqlx::query_scalar("SELECT public_key FROM ocm_shares WHERE id = ?1")
            .bind(share.id)
            .fetch_optional(files.db().reader())
            .await
            .ok()
            .flatten()
            .flatten();
    if kept.is_some() {
        return kept;
    }
    let federation = files.federation();
    {
        let mut asked = federation.keys_asked.lock().expect("not poisoned");
        if asked
            .get(&share.id)
            .is_some_and(|at| at.elapsed() < ASK_AGAIN)
        {
            return None;
        }
        asked.insert(share.id, Instant::now());
    }
    let host = federation.host()?;
    let discovery = federation.discover(&share.host).await.ok()?;
    if !discovery.extension(super::E2E) {
        return None;
    }
    let body = serde_json::json!({
        "senderDomain": host,
        "providerId": share.id.to_string(),
        "shareWith": share.address,
    });
    let (status, answer) = match federation
        .post_json(&discovery, PUBLIC_KEY_PATH, &body)
        .await
    {
        Ok(answered) => answered,
        Err(_) => {
            tracing::info!("a member's server cannot be reached for their public key");
            return None;
        }
    };
    if status != 200 {
        tracing::info!("a member's server answered {status} for their public key");
        return None;
    }
    let value: serde_json::Value = serde_json::from_slice(&answer).ok()?;
    let key = value["publicKey"].as_str()?.to_string();
    if crate::util::base64url_decode(&key).map(|b| b.len()) != Some(32) {
        tracing::info!("a member's server answered a public key that is not one");
        return None;
    }
    sqlx::query("UPDATE ocm_shares SET public_key = ?1 WHERE id = ?2 AND public_key IS NULL")
        .bind(&key)
        .bind(share.id)
        .execute(files.db().writer())
        .await
        .ok()?;
    federation
        .keys_asked
        .lock()
        .expect("not poisoned")
        .remove(&share.id);
    Some(key)
}

/// A share with another server ends (or no longer reaches its encrypted
/// folder): the keys wrapped for its member go, and the folder needs a
/// new key, which is not wrapped for them.
pub async fn remote_gone(conn: &mut sqlx::SqliteConnection, share: Uuid) -> sqlx::Result<()> {
    sqlx::query(
        "UPDATE e2e_folders SET needs_new_key = 1
         WHERE root_id IN (SELECT root_id FROM e2e_remote_keys WHERE share_id = ?1)",
    )
    .bind(share)
    .execute(&mut *conn)
    .await?;
    sqlx::query("DELETE FROM e2e_remote_keys WHERE share_id = ?1")
        .bind(share)
        .execute(&mut *conn)
        .await?;
    Ok(())
}

/// [`remote_gone`] in a transaction of its own.
pub async fn remote_gone_now(files: &Files, share: Uuid) -> sqlx::Result<()> {
    let mut tx = files.db().writer().begin().await?;
    remote_gone(&mut tx, share).await?;
    tx.commit().await
}

/// The body of both requests.
#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct Asked {
    sender_domain: String,
    provider_id: serde_json::Value,
    #[serde(default)]
    share_with: Option<String>,
}

/// The checks both endpoints make: federation on, the asking server not
/// refused, the request signed by it, `organizer-e2e` in its discovery.
async fn checked(
    files: &Files,
    parts: &axum::http::request::Parts,
    body: &[u8],
) -> Result<Result<(Asked, String), Answer>, HttpError> {
    let federation = files.federation();
    if !federation.on().await? {
        return Ok(Err(answer(
            StatusCode::NOT_IMPLEMENTED,
            "This server does not take part in federation",
        )));
    }
    let Ok(asked) = serde_json::from_slice::<Asked>(body) else {
        return Ok(Err(answer(
            StatusCode::BAD_REQUEST,
            "The request cannot be read",
        )));
    };
    let Some(host) = host_of(&asked.sender_domain) else {
        return Ok(Err(answer(
            StatusCode::BAD_REQUEST,
            "senderDomain is not valid",
        )));
    };
    if federation.trust(&host).await? == Trust::Refused {
        return Ok(Err(answer(
            StatusCode::FORBIDDEN,
            "This server does not take requests from yours",
        )));
    }
    if federation.check_signed(parts, body, &host).await.is_err() {
        return Ok(Err(answer(
            StatusCode::UNAUTHORIZED,
            "The request's signature does not verify",
        )));
    }
    if !federation
        .discover(&host)
        .await
        .is_ok_and(|d| d.extension(super::E2E))
    {
        return Ok(Err(answer(
            StatusCode::FORBIDDEN,
            "Only for servers with organizer-e2e",
        )));
    }
    Ok(Ok((asked, host)))
}

/// `POST /ocm/organizer-e2e/public-key`, on the member's server: the
/// public key of the user an encrypted folder was shared with, asked by
/// the server it came from (`providerId` the share's id there).
pub async fn answer_public_key(
    files: &Arc<Files>,
    parts: &axum::http::request::Parts,
    body: &[u8],
) -> Result<Answer, HttpError> {
    let (asked, host) = match checked(files, parts, body).await? {
        Ok(checked) => checked,
        Err(refused) => return Ok(refused),
    };
    let Some(provider_id) = super::incoming::id_string(&asked.provider_id) else {
        return Ok(answer(StatusCode::BAD_REQUEST, "providerId is missing"));
    };
    let users: Vec<(Uuid, String)> = sqlx::query_as(
        "SELECT r.user_id, u.email FROM ocm_received r JOIN users u ON u.id = r.user_id
         WHERE r.host = ?1 AND r.provider_id = ?2 AND r.e2e = 1",
    )
    .bind(&host)
    .bind(&provider_id)
    .fetch_all(files.db().reader())
    .await?;
    let ours = files.federation().host().unwrap_or_default();
    let user = users
        .into_iter()
        .find(|(_, email)| match &asked.share_with {
            Some(address) => super::address::Address::parse(address)
                .is_some_and(|a| a.to_string() == super::address_of(email, &ours)),
            None => true,
        });
    let Some((user, _)) = user else {
        return Ok(answer(StatusCode::NOT_FOUND, "No such share"));
    };
    let key: Option<String> =
        sqlx::query_scalar("SELECT public_key FROM e2e_keys WHERE user_id = ?1")
            .bind(user)
            .fetch_optional(files.db().reader())
            .await?;
    Ok((StatusCode::OK, serde_json::json!({ "publicKey": key })))
}

/// `POST /ocm/organizer-e2e/keys`, on the owner's server: the keys wrapped
/// for the member of a share (`providerId` our share's id), asked by their
/// server: the encrypted folder's id (what they are wrapped and its names
/// sealed with), its newest generation, and the member's wrapped keys of
/// every generation they have.
pub async fn answer_keys(
    files: &Arc<Files>,
    parts: &axum::http::request::Parts,
    body: &[u8],
) -> Result<Answer, HttpError> {
    let (asked, host) = match checked(files, parts, body).await? {
        Ok(checked) => checked,
        Err(refused) => return Ok(refused),
    };
    let share = match super::incoming::id_string(&asked.provider_id)
        .and_then(|p| Uuid::parse_str(&p).ok())
    {
        Some(id) => share_row(files.db().reader(), id).await?,
        None => return Ok(answer(StatusCode::BAD_REQUEST, "providerId is missing")),
    };
    let no_share = || Ok(answer(StatusCode::NOT_FOUND, "No such share"));
    let Some(share) = share.filter(|s| s.host == host && s.e2e && s.state != ShareState::Declined)
    else {
        return no_share();
    };
    let Some(root) = share.node_id else {
        return no_share();
    };
    if !super::outgoing::drive_allows(files, share.drive_id).await?
        || role_in(files, &share, root).await?.is_none()
    {
        return no_share();
    }
    let generation: Option<i64> =
        sqlx::query_scalar("SELECT generation FROM e2e_folders WHERE root_id = ?1")
            .bind(root)
            .fetch_optional(files.db().reader())
            .await?;
    let keys: Vec<(i64, String)> = sqlx::query_as(
        "SELECT generation, wrapped FROM e2e_remote_keys
         WHERE root_id = ?1 AND share_id = ?2 ORDER BY generation",
    )
    .bind(root)
    .bind(share.id)
    .fetch_all(files.db().reader())
    .await?;
    let keys: Vec<serde_json::Value> = keys
        .into_iter()
        .map(|(generation, wrapped)| {
            serde_json::json!({ "generation": generation, "wrapped": wrapped })
        })
        .collect();
    Ok((
        StatusCode::OK,
        serde_json::json!({
            "folderId": root.to_string(),
            "generation": generation.unwrap_or(0),
            "keys": keys,
        }),
    ))
}

/// The keys of an encrypted folder shared from another server, as its
/// server answered them for our user.
pub struct RemoteKeys {
    /// The owner's encrypted folder: the id the keys are wrapped with.
    pub folder_id: Uuid,
    pub generation: i64,
    pub keys: Vec<(i64, String)>,
}

/// Ask the owner's server for our user's keys of an encrypted folder it
/// shared (signed); the drive's root takes the folder's id as its
/// `source_id`.
pub async fn fetch_keys(files: &Files, received: &Received) -> Result<RemoteKeys, FileError> {
    let federation = files.federation();
    let host = federation
        .host()
        .ok_or_else(|| FileError::Down("Federation is off".into()))?;
    let discovery = federation
        .discover(&received.host)
        .await
        .map_err(FileError::Down)?;
    if !discovery.extension(super::E2E) {
        return Err(FileError::Down(
            "The server it comes from no longer takes encrypted folders".into(),
        ));
    }
    let body = serde_json::json!({
        "senderDomain": host,
        "providerId": received.provider_id,
    });
    let (status, answer) = federation
        .post_json(&discovery, KEYS_PATH, &body)
        .await
        .map_err(FileError::Down)?;
    match status {
        200 => {}
        404 => return Err(FileError::NotFound),
        status => {
            return Err(FileError::Down(format!(
                "The server it comes from answered {status}"
            )));
        }
    }
    #[derive(Deserialize)]
    #[serde(rename_all = "camelCase")]
    struct Answered {
        folder_id: Uuid,
        generation: i64,
        keys: Vec<Key>,
    }
    #[derive(Deserialize)]
    struct Key {
        generation: i64,
        wrapped: String,
    }
    let answered: Answered = serde_json::from_slice(&answer)
        .map_err(|_| FileError::Down("The keys' answer is not valid".into()))?;
    let wrapping = |k: &Key| {
        k.generation >= 1
            && crate::util::base64url_decode(&k.wrapped).is_some_and(|b| b.len() == 93 && b[0] == 1)
    };
    if !answered.keys.iter().all(wrapping) {
        return Err(FileError::Down("The keys' answer is not valid".into()));
    }
    sqlx::query(
        "UPDATE nodes SET source_id = ?1
         WHERE drive_id = ?2 AND parent_id IS NULL AND source_id IS NOT ?1",
    )
    .bind(answered.folder_id)
    .bind(received.drive_id)
    .execute(files.db().writer())
    .await?;
    Ok(RemoteKeys {
        folder_id: answered.folder_id,
        generation: answered.generation,
        keys: answered
            .keys
            .into_iter()
            .map(|k| (k.generation, k.wrapped))
            .collect(),
    })
}

/// The share from another server an encrypted folder of ours stands for,
/// as `user` asks for its keys by `id`: a node of its drive, or the
/// owner's id of one (`source_id`). None: not one.
pub async fn received_for(
    files: &Files,
    user: Uuid,
    id: Uuid,
) -> sqlx::Result<Option<(Uuid, Received)>> {
    let node: Option<(Uuid, Uuid)> = sqlx::query_as(
        "SELECT n.id, n.drive_id FROM nodes n JOIN ocm_received r ON r.drive_id = n.drive_id
         WHERE (n.id = ?1 OR n.source_id = ?1) AND r.user_id = ?2 AND r.e2e = 1
         LIMIT 1",
    )
    .bind(id)
    .bind(user)
    .fetch_optional(files.db().reader())
    .await?;
    let Some((node, drive)) = node else {
        return Ok(None);
    };
    Ok(
        super::incoming::received_of_drive(files.db().reader(), drive)
            .await?
            .map(|r| (node, r)),
    )
}

/// Keep the owner's ids of an encrypted folder shared from another server
/// as `source_id` of our index of it: a sync-collection of it whole, whose
/// members carry `o:id` (and the multistatus the top's, `o:root-id`),
/// matched by path. After a scan of its drive.
pub async fn sync_ids(files: &Arc<Files>, received: &Received) -> Result<(), String> {
    let federation = files.federation();
    let url = federation.allowed_url(&received.uri)?;
    let base = url.path().trim_end_matches('/').to_string();
    let mut url = url;
    url.set_path(&format!("{base}/"));
    let token = ShareToken::new(files, received.id);
    let body = r#"<?xml version="1.0" encoding="utf-8"?><d:sync-collection xmlns:d="DAV:"><d:sync-token/><d:sync-level>infinite</d:sync-level><d:prop><d:getetag/></d:prop></d:sync-collection>"#;
    let report = axum::http::Method::from_bytes(b"REPORT").map_err(|e| e.to_string())?;
    let mut fresh = false;
    let text = loop {
        let bearer = token.get(fresh).await?;
        let response = federation
            .client()
            .request(report.clone(), url.clone())
            .bearer_auth(bearer)
            .header("content-type", "application/xml; charset=utf-8")
            .header("depth", "0")
            .body(body)
            .send()
            .await
            .map_err(|_| "the server cannot be reached".to_string())?;
        let status = response.status().as_u16();
        if status == 401 && !fresh {
            fresh = true;
            continue;
        }
        if status != 207 {
            return Err(format!("the server answered {status} to the listing"));
        }
        break response
            .text()
            .await
            .map_err(|_| "the listing broke off".to_string())?;
    };
    let mut ids: Vec<(String, Uuid)> = Vec::new();
    for response in dav::elements_text(&text, "response") {
        let (Some(href), Some(id)) = (
            dav::element_text(&response, "href"),
            dav::element_text(&response, "id").and_then(|i| Uuid::parse_str(&i).ok()),
        ) else {
            continue;
        };
        let Some(rel) = href.strip_prefix(&base) else {
            continue;
        };
        let rel: Vec<String> = rel
            .split('/')
            .filter(|s| !s.is_empty())
            .map(super::incoming::percent_decode)
            .collect();
        ids.push((rel.join("/"), id));
    }
    let root = dav::element_text(&text, "root-id").and_then(|i| Uuid::parse_str(&i).ok());
    let ours: Vec<(Uuid, String)> = sqlx::query_as(
        "WITH RECURSIVE down (id, rel) AS (
           SELECT id, '' FROM nodes WHERE drive_id = ?1 AND parent_id IS NULL
           UNION ALL
           SELECT n.id, CASE down.rel WHEN '' THEN n.name ELSE down.rel || '/' || n.name END
           FROM nodes n JOIN down ON n.parent_id = down.id
         )
         SELECT id, rel FROM down",
    )
    .bind(received.drive_id)
    .fetch_all(files.db().reader())
    .await
    .map_err(|e| e.to_string())?;
    let theirs: std::collections::HashMap<String, Uuid> = ids
        .into_iter()
        .chain(root.map(|r| (String::new(), r)))
        .collect();
    let mut tx = files
        .db()
        .writer()
        .begin()
        .await
        .map_err(|e| e.to_string())?;
    for (node, rel) in ours {
        let Some(source) = theirs.get(&rel) else {
            continue;
        };
        sqlx::query("UPDATE nodes SET source_id = ?1 WHERE id = ?2 AND source_id IS NOT ?1")
            .bind(source)
            .bind(node)
            .execute(&mut *tx)
            .await
            .map_err(|e| e.to_string())?;
    }
    tx.commit().await.map_err(|e| e.to_string())
}

/// A folder just made in an encrypted folder shared from another server,
/// with its id there (`source_id`) read at once: its children's names are
/// sealed with it. Unchanged when that server cannot say yet (the next
/// follow brings it).
pub async fn with_source(files: &Arc<Files>, mut node: crate::files::Node) -> crate::files::Node {
    if node.e2e_root.is_none() || node.source_id.is_some() {
        return node;
    }
    let Ok(Some(received)) =
        super::incoming::received_of_drive(files.db().reader(), node.drive_id).await
    else {
        return node;
    };
    if !received.e2e {
        return node;
    }
    if let Err(e) = sync_ids(files, &received).await {
        tracing::info!("the ids of an encrypted folder from another server cannot be read: {e}");
        return node;
    }
    node.source_id = sqlx::query_scalar("SELECT source_id FROM nodes WHERE id = ?1")
        .bind(node.id)
        .fetch_optional(files.db().reader())
        .await
        .ok()
        .flatten()
        .flatten();
    node
}

/// Whether some node of a drive has no `source_id` yet.
pub async fn ids_missing(files: &Files, drive: Uuid) -> sqlx::Result<bool> {
    let missing: Option<i64> =
        sqlx::query_scalar("SELECT 1 FROM nodes WHERE drive_id = ?1 AND source_id IS NULL LIMIT 1")
            .bind(drive)
            .fetch_optional(files.db().reader())
            .await?;
    Ok(missing.is_some())
}
