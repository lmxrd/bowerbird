//! Sharing in (docs/M7E_DESIGN.md §5): what someone on another server
//! shares with one of our users.
//!
//! A Share Creation Notification (§7) that passes (signed by the sender's
//! server, from a server we take, to a user of ours who has the sender as
//! a contact or may take shares from anyone, over WebDAV with
//! `must-exchange-token`) becomes a **drive of its own**, `kind` `remote`,
//! named as the share: its root folder stands for the shared folder (or
//! holds the shared file), and its recipient has a grant on that root with
//! the role of OCM's permissions (or, between Organizer servers, the
//! role it was given), or on the whole drive for a whole drive
//! (`organizer-drives`). So it is in "Shared with me" (or among the
//! drives), placed in My drive as any share, browsed, read and written
//! through our API with `my_role`, and its changes reach the change feed
//! through the same reconciler as every drive.
//!
//! **Bytes always go through our server**: the drive's storage is the
//! other server's WebDAV (opendal's), through our guarded client, with a
//! short-lived bearer token of that server's code flow ([`ShareToken`]),
//! asked again before it ends or when it is refused. A delete there is
//! that server's (it keeps its own trash); we keep no versions of it.
//!
//! **Following changes** ([`follow`]): sync-collection (RFC 6578) when the
//! other server answers it, else its ETag; a change found is a scan of
//! the drive. Every few minutes, sooner while someone has it open, and at
//! once on a live notice (`organizer-live`).

use super::address::{Address, host_of};
use super::{Discovery, Trust};
use crate::error::HttpError;
use crate::files::roles::Role;
use crate::files::{FileError, Files};
use axum::http::StatusCode;
use serde::{Deserialize, Serialize};
use std::sync::{Arc, Weak};
use std::time::Duration;
use utoipa::ToSchema;
use uuid::Uuid;

/// Where a share from another server stands.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, ToSchema, sqlx::Type)]
#[serde(rename_all = "snake_case")]
#[sqlx(type_name = "TEXT", rename_all = "snake_case")]
#[schema(as = IncomingShareState)]
pub enum ReceivedState {
    /// Not accepted yet: listed, not reachable.
    Pending,
    Accepted,
}

/// A share from another server, as stored.
#[derive(Clone, Debug, sqlx::FromRow)]
pub struct Received {
    pub id: Uuid,
    pub user_id: Uuid,
    pub drive_id: Uuid,
    pub host: String,
    pub provider_id: String,
    pub name: String,
    pub resource_type: String,
    pub owner: String,
    pub owner_name: String,
    pub sender: String,
    pub sender_name: String,
    pub permissions: String,
    pub role: Role,
    pub uri: String,
    pub secret: Vec<u8>,
    pub must_exchange: bool,
    pub token_endpoint: Option<String>,
    pub whole_drive: bool,
    pub e2e: bool,
    pub state: ReceivedState,
    pub sync_token: Option<String>,
    pub etag: Option<String>,
    pub sync_collection: Option<bool>,
    pub checked_at: Option<String>,
    pub error: Option<String>,
    pub created_at: String,
}

const RECEIVED_COLUMNS: &str = "id, user_id, drive_id, host, provider_id, name, resource_type, \
     owner, owner_name, sender, sender_name, permissions, role, uri, secret, must_exchange, \
     token_endpoint, whole_drive, e2e, state, sync_token, etag, sync_collection, checked_at, \
     error, created_at";

pub async fn received_row<'e>(
    db: impl sqlx::SqliteExecutor<'e>,
    id: Uuid,
) -> sqlx::Result<Option<Received>> {
    sqlx::query_as(sqlx::AssertSqlSafe(format!(
        "SELECT {RECEIVED_COLUMNS} FROM ocm_received WHERE id = ?1"
    )))
    .bind(id)
    .fetch_optional(db)
    .await
}

/// The share from another server a drive stands for.
pub async fn received_of_drive<'e>(
    db: impl sqlx::SqliteExecutor<'e>,
    drive: Uuid,
) -> sqlx::Result<Option<Received>> {
    sqlx::query_as(sqlx::AssertSqlSafe(format!(
        "SELECT {RECEIVED_COLUMNS} FROM ocm_received WHERE drive_id = ?1"
    )))
    .bind(drive)
    .fetch_optional(db)
    .await
}

impl Received {
    /// OCM's permissions it came with.
    pub fn permissions(&self) -> Vec<String> {
        serde_json::from_str(&self.permissions).unwrap_or_default()
    }
}

/// An answer to another server: a status and a JSON body.
pub type Answer = (StatusCode, serde_json::Value);

pub(crate) fn answer(status: StatusCode, message: &str) -> Answer {
    (status, serde_json::json!({ "message": message }))
}

/// A Share Creation Notification's body (§7.1), as much as we read.
#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct ShareRequest {
    share_with: String,
    name: String,
    provider_id: serde_json::Value,
    owner: String,
    sender: String,
    #[serde(default)]
    owner_display_name: Option<String>,
    #[serde(default)]
    sender_display_name: Option<String>,
    share_type: String,
    resource_type: String,
    protocol: serde_json::Value,
    /// Our extension, between Organizer servers.
    #[serde(default)]
    organizer: Option<Extension>,
}

#[derive(Deserialize, Default)]
struct Extension {
    #[serde(default)]
    role: Option<Role>,
    #[serde(default)]
    drive: bool,
    #[serde(default)]
    e2e: bool,
}

/// An id another server sent, a string or a number.
pub(crate) fn id_string(value: &serde_json::Value) -> Option<String> {
    match value {
        serde_json::Value::String(s) if !s.is_empty() && s.len() <= 200 => Some(s.clone()),
        serde_json::Value::Number(n) => Some(n.to_string()),
        _ => None,
    }
}

/// The user an address of ours stands for (their email at our host).
async fn local_user(files: &Files, address: &Address) -> sqlx::Result<Option<(Uuid, String)>> {
    if files.federation().host().as_deref() != Some(address.host.as_str()) {
        return Ok(None);
    }
    sqlx::query_as(
        "SELECT id, email FROM users WHERE email = ?1 COLLATE NOCASE AND disabled_at IS NULL",
    )
    .bind(&address.identifier)
    .fetch_optional(files.db().reader())
    .await
}

/// Whether a user takes shares from someone: a contact, anyone on a
/// listed server, or anyone when `federation.share_with_any` lets them
/// (contacts first, as for sharing out).
async fn takes_from(
    files: &Files,
    user: Uuid,
    from: &[&Address],
    trust: Trust,
) -> sqlx::Result<bool> {
    if trust == (Trust::Allowed { listed: true }) {
        return Ok(true);
    }
    for address in from {
        let contact: Option<i64> =
            sqlx::query_scalar("SELECT 1 FROM contacts WHERE user_id = ?1 AND address = ?2")
                .bind(user)
                .bind(address.to_string())
                .fetch_optional(files.db().reader())
                .await?;
        if contact.is_some() {
            return Ok(true);
        }
    }
    let env = crate::settings::Env::of_files(files);
    Ok(crate::settings::resolve(
        files.db(),
        &env,
        "federation.share_with_any",
        crate::settings::For::user(user),
    )
    .await?
    .bool())
}

/// A share's name as a drive's (and a file's) name here.
fn clean_name(name: &str) -> String {
    let cleaned: String = name
        .chars()
        .map(|c| {
            if c == '/' || c == '\\' || c.is_control() {
                '_'
            } else {
                c
            }
        })
        .take(200)
        .collect();
    match crate::files::names::new_name(cleaned.trim()) {
        Ok(name) => name,
        Err(_) => "Shared".into(),
    }
}

/// Take a Share Creation Notification (`POST /ocm/shares`, §7): checked,
/// then a remote drive for its recipient, pending until they accept it.
/// The answer, and the recipient of a share new here (to be told).
pub async fn receive(
    files: &Arc<Files>,
    parts: &axum::http::request::Parts,
    body: &[u8],
) -> Result<(Answer, Option<Uuid>), HttpError> {
    let mut recipient = None;
    let answer = take(files, parts, body, &mut recipient).await?;
    Ok((answer, recipient))
}

async fn take(
    files: &Arc<Files>,
    parts: &axum::http::request::Parts,
    body: &[u8],
    recipient: &mut Option<Uuid>,
) -> Result<Answer, HttpError> {
    let federation = files.federation();
    if !federation.on().await? {
        return Ok(answer(
            StatusCode::NOT_IMPLEMENTED,
            "This server does not take shares from other servers",
        ));
    }
    let Ok(request) = serde_json::from_slice::<ShareRequest>(body) else {
        return Ok(answer(StatusCode::BAD_REQUEST, "The share cannot be read"));
    };
    let (Some(sender), Some(owner), Some(share_with)) = (
        Address::parse(&request.sender),
        Address::parse(&request.owner),
        Address::parse(&request.share_with),
    ) else {
        return Ok(answer(StatusCode::BAD_REQUEST, "An address is not valid"));
    };
    // The owner is an account of the server the share comes from (§16.4).
    if owner.host != sender.host {
        return Ok(answer(
            StatusCode::BAD_REQUEST,
            "The owner must be at the sender's server",
        ));
    }
    let trust = federation.trust(&sender.host).await?;
    if trust == Trust::Refused {
        return Ok(answer(
            StatusCode::FORBIDDEN,
            "This server does not take shares from yours",
        ));
    }
    if federation
        .check_signed(parts, body, &sender.host)
        .await
        .is_err()
    {
        return Ok(answer(
            StatusCode::UNAUTHORIZED,
            "The request's signature does not verify",
        ));
    }
    if request.share_type != "user" {
        return Ok(answer(
            StatusCode::NOT_IMPLEMENTED,
            "Only shares with a user are taken",
        ));
    }
    let resource = request.resource_type.as_str();
    if !matches!(resource, "file" | "folder") {
        return Ok(answer(
            StatusCode::NOT_IMPLEMENTED,
            "Only files and folders are taken",
        ));
    }
    let Some(provider_id) = id_string(&request.provider_id) else {
        return Ok(answer(StatusCode::BAD_REQUEST, "providerId is missing"));
    };
    let Some((user, email)) = local_user(files, &share_with).await? else {
        return Ok(answer(StatusCode::BAD_REQUEST, "No such user here"));
    };
    if !takes_from(files, user, &[&sender, &owner], trust).await? {
        return Ok(answer(
            StatusCode::FORBIDDEN,
            "The recipient takes shares from their contacts only",
        ));
    }
    let Ok(discovery) = federation.discover(&sender.host).await else {
        return Ok(answer(
            StatusCode::BAD_REQUEST,
            "The sender's server cannot be discovered",
        ));
    };
    // The protocol: `multi`, or `webdav` with its details named so; the
    // deprecated `options` (v1.0, Basic authentication) is not taken.
    let webdav = &request.protocol["webdav"];
    if !webdav.is_object() {
        return Ok(answer(
            StatusCode::NOT_IMPLEMENTED,
            "Only WebDAV access with its details is taken",
        ));
    }
    let (Some(uri), Some(secret)) = (webdav["uri"].as_str(), webdav["sharedSecret"].as_str())
    else {
        return Ok(answer(
            StatusCode::BAD_REQUEST,
            "The WebDAV uri and sharedSecret are required",
        ));
    };
    let list = |key: &str| -> Vec<String> {
        webdav[key]
            .as_array()
            .map(|a| {
                a.iter()
                    .filter_map(|v| v.as_str().map(str::to_string))
                    .collect()
            })
            .unwrap_or_default()
    };
    let requirements = list("requirements");
    let permissions = list("permissions");
    // We advertise must-exchange-token (§5.3): every share needs it.
    if !requirements.iter().any(|r| r == "must-exchange-token") {
        return Ok(answer(
            StatusCode::BAD_REQUEST,
            "This server requires must-exchange-token",
        ));
    }
    if requirements.iter().any(|r| r == "must-use-mfa") {
        return Ok(answer(
            StatusCode::BAD_REQUEST,
            "This server does not enforce MFA for shares",
        ));
    }
    let Some(token_endpoint) = discovery
        .token_end_point
        .clone()
        .filter(|_| discovery.has("exchange-token"))
    else {
        return Ok(answer(
            StatusCode::BAD_REQUEST,
            "The sender's server has no token endpoint",
        ));
    };
    let Some(uri) = resource_url(&discovery, &sender.host, uri) else {
        return Ok(answer(
            StatusCode::BAD_REQUEST,
            "The WebDAV uri is not at the sender's server",
        ));
    };
    // Our extensions, only from a server that has them.
    let extension = request.organizer.unwrap_or_default();
    let role = match extension.role {
        Some(role) if discovery.has(super::DRIVES) || discovery.has(super::LIVE) => {
            role.min(Role::Manager)
        }
        _ => super::role_of(&permissions),
    };
    let whole_drive = extension.drive && discovery.has(super::DRIVES) && resource == "folder";
    let e2e = extension.e2e && discovery.has(super::E2E);
    let existing: Option<Uuid> = sqlx::query_scalar(
        "SELECT id FROM ocm_received WHERE host = ?1 AND provider_id = ?2 AND user_id = ?3",
    )
    .bind(&sender.host)
    .bind(&provider_id)
    .bind(user)
    .fetch_optional(files.db().reader())
    .await?;
    let created = serde_json::json!({ "recipientDisplayName": email });
    if existing.is_some() {
        return Ok((StatusCode::CREATED, created));
    }
    let sealed = files.backends().seal(secret).map_err(anyhow::Error::from)?;
    let name = clean_name(&request.name);
    let (id, drive, root) = (Uuid::now_v7(), Uuid::now_v7(), Uuid::now_v7());
    let mut tx = files.db().writer().begin().await?;
    sqlx::query("INSERT INTO drives (id, kind, name) VALUES (?1, 'remote', ?2)")
        .bind(drive)
        .bind(&name)
        .execute(&mut *tx)
        .await?;
    // An encrypted folder (`organizer-e2e`) is one here too: its names
    // and contents stay sealed, opened only by its members' devices.
    sqlx::query(
        "INSERT INTO nodes (id, drive_id, parent_id, name, kind, e2e_root)
         VALUES (?1, ?2, NULL, '', 'folder', CASE WHEN ?3 THEN ?1 END)",
    )
    .bind(root)
    .bind(drive)
    .bind(e2e)
    .execute(&mut *tx)
    .await?;
    sqlx::query(
        "INSERT INTO ocm_received (id, user_id, drive_id, host, provider_id, name, resource_type,
           owner, owner_name, sender, sender_name, permissions, role, uri, secret, must_exchange,
           token_endpoint, whole_drive, e2e)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?14, ?15, 1, ?16, ?17, ?18)",
    )
    .bind(id)
    .bind(user)
    .bind(drive)
    .bind(&sender.host)
    .bind(&provider_id)
    .bind(&name)
    .bind(resource)
    .bind(owner.to_string())
    .bind(display(request.owner_display_name.as_deref()))
    .bind(sender.to_string())
    .bind(display(request.sender_display_name.as_deref()))
    .bind(serde_json::to_string(&permissions).unwrap_or_default())
    .bind(role)
    .bind(uri.as_str())
    .bind(sealed)
    .bind(&token_endpoint)
    .bind(whole_drive)
    .bind(e2e)
    .execute(&mut *tx)
    .await?;
    // The recipient's grant: on the whole drive, or on its root.
    sqlx::query(
        "INSERT INTO grants (id, drive_id, node_id, user_id, role) VALUES (?1, ?2, ?3, ?4, ?5)",
    )
    .bind(Uuid::now_v7())
    .bind(drive)
    .bind((!whole_drive).then_some(root))
    .bind(user)
    .bind(role)
    .execute(&mut *tx)
    .await?;
    tx.commit().await?;
    files.announce(user, crate::files::Topic::Changes);
    *recipient = Some(user);
    Ok((StatusCode::CREATED, created))
}

/// A display name another server sent, fit to keep.
fn display(name: Option<&str>) -> String {
    name.unwrap_or_default()
        .chars()
        .filter(|c| !c.is_control())
        .take(200)
        .collect()
}

/// A share's WebDAV URL: an absolute one at the sender's server, or a
/// relative one under its WebDAV prefix (§9 step 4).
fn resource_url(discovery: &Discovery, host: &str, uri: &str) -> Option<reqwest::Url> {
    let end = reqwest::Url::parse(&discovery.end_point).ok()?;
    let at = |url: &reqwest::Url| {
        let name = url.host_str().unwrap_or_default();
        let found = match url.port() {
            Some(port) => format!("{name}:{port}"),
            None => name.to_string(),
        };
        host_of(&found).as_deref() == Some(host)
    };
    if !at(&end) {
        return None;
    }
    if let Ok(url) = reqwest::Url::parse(uri) {
        return (matches!(url.scheme(), "http" | "https") && at(&url)).then_some(url);
    }
    let prefix = discovery.webdav_path()?;
    let mut url = end.clone();
    let path = format!(
        "{}/{}",
        prefix.trim_end_matches('/'),
        uri.trim_start_matches('/')
    );
    if reqwest::Url::parse(prefix).is_ok() {
        // A prefix given as a URL: at the sender's server too.
        let prefixed = reqwest::Url::parse(&format!(
            "{}/{}",
            prefix.trim_end_matches('/'),
            uri.trim_start_matches('/')
        ))
        .ok()?;
        return at(&prefixed).then_some(prefixed);
    }
    url.set_path(&path);
    url.set_query(None);
    Some(url)
}

/// Tell the sender's server of what the recipient did (§8.2).
async fn tell(files: &Files, received: &Received, kind: &str) {
    let federation = files.federation();
    let Some(host) = federation.host() else {
        return;
    };
    let Ok(discovery) = federation.discover(&received.host).await else {
        tracing::info!("a share's server cannot be reached to tell it of an answer");
        return;
    };
    let body = serde_json::json!({
        "notificationType": kind,
        "senderDomain": host,
        "resourceType": received.resource_type,
        "shareType": "user",
        "notification": { "file": { "providerId": received.provider_id } },
    });
    match federation
        .post_json(&discovery, "/notifications", &body)
        .await
    {
        Ok((status, _)) if (200..300).contains(&status) => {}
        Ok((status, _)) => tracing::info!("a share's server answered {status} to a notification"),
        Err(_) => tracing::info!("a share's server cannot be reached to tell it of an answer"),
    }
}

/// Accept a share from another server: its server is told, and its files
/// are listed.
pub async fn accept(files: &Arc<Files>, received: &Received) -> Result<(), HttpError> {
    if received.state == ReceivedState::Accepted {
        return Ok(());
    }
    sqlx::query("UPDATE ocm_received SET state = 'accepted' WHERE id = ?1")
        .bind(received.id)
        .execute(files.db().writer())
        .await?;
    tell(files, received, "SHARE_ACCEPTED").await;
    // A whole drive comes into the user's drives (their feed says so).
    if received.whole_drive {
        let root = files.root_node(received.drive_id).await?;
        let mut tx = files.db().writer().begin().await?;
        crate::files::drives::root_row(
            &mut tx,
            received.user_id,
            received.drive_id,
            root.id,
            "create",
            Some(root.version),
            Some(&received.name),
        )
        .await?;
        tx.commit().await?;
        files.announce(received.user_id, crate::files::Topic::Changes);
    }
    if let Err(e) = follow(files, received.id).await {
        tracing::info!("a share from another server cannot be listed yet: {e}");
    }
    Ok(())
}

/// Decline a share from another server (or leave one accepted): its
/// server is told, and it goes from here.
pub async fn decline(files: &Arc<Files>, received: &Received) -> Result<(), HttpError> {
    tell(files, received, "SHARE_DECLINED").await;
    forget(files, received).await?;
    Ok(())
}

/// Decline every share from other servers of a user (who is removed).
pub async fn decline_all(files: &Arc<Files>, user: Uuid) -> Result<(), HttpError> {
    let ids: Vec<Uuid> = sqlx::query_scalar("SELECT id FROM ocm_received WHERE user_id = ?1")
        .bind(user)
        .fetch_all(files.db().reader())
        .await?;
    for id in ids {
        if let Some(row) = received_row(files.db().reader(), id).await? {
            decline(files, &row).await?;
        }
    }
    Ok(())
}

/// A share from another server goes: its drive (mounts, feed rows, the
/// index) and its token.
async fn forget(files: &Arc<Files>, received: &Received) -> Result<(), FileError> {
    files.federation().drop_token(received.id);
    files.forget_drive(received.drive_id).await
}

/// A notification's body (§8.1).
#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct NotificationRequest {
    notification_type: String,
    sender_domain: String,
    #[serde(default)]
    provider_id: Option<serde_json::Value>,
    #[serde(default)]
    notification: Option<serde_json::Value>,
}

/// Take a notification (`POST /ocm/notifications`, §8): about a share we
/// sent (accepted, declined) or one we received (unshared, its
/// permissions changed, our live notice), or a user removed.
pub async fn notification(
    files: &Arc<Files>,
    parts: &axum::http::request::Parts,
    body: &[u8],
) -> Result<Answer, HttpError> {
    let federation = files.federation();
    if !federation.on().await? {
        return Ok(answer(
            StatusCode::NOT_IMPLEMENTED,
            "This server does not take part in federation",
        ));
    }
    let Ok(request) = serde_json::from_slice::<NotificationRequest>(body) else {
        return Ok(answer(
            StatusCode::BAD_REQUEST,
            "The notification cannot be read",
        ));
    };
    let Some(host) = host_of(&request.sender_domain) else {
        return Ok(answer(StatusCode::BAD_REQUEST, "senderDomain is not valid"));
    };
    if federation.trust(&host).await? == Trust::Refused {
        return Ok(answer(
            StatusCode::FORBIDDEN,
            "This server does not take notifications from yours",
        ));
    }
    if federation.check_signed(parts, body, &host).await.is_err() {
        return Ok(answer(
            StatusCode::UNAUTHORIZED,
            "The request's signature does not verify",
        ));
    }
    let details = request.notification.unwrap_or_default();
    let provider_id = id_string(&details["file"]["providerId"])
        .or_else(|| request.provider_id.as_ref().and_then(id_string));
    let created = (StatusCode::CREATED, serde_json::json!({}));
    match request.notification_type.as_str() {
        // About a share we sent: its id is ours.
        kind @ ("SHARE_ACCEPTED" | "SHARE_DECLINED") => {
            let Some(share) = provider_id
                .and_then(|p| Uuid::parse_str(&p).ok())
                .map(|id| super::outgoing::share_row(files.db().reader(), id))
            else {
                return Ok(answer(StatusCode::BAD_REQUEST, "providerId is missing"));
            };
            let Some(share) = share.await?.filter(|s| s.host == host) else {
                return Ok(answer(StatusCode::NOT_FOUND, "No such share"));
            };
            let state = if kind == "SHARE_ACCEPTED" {
                "accepted"
            } else {
                federation.revoke_tokens(share.id);
                super::e2e::remote_gone_now(files, share.id).await?;
                "declined"
            };
            sqlx::query("UPDATE ocm_shares SET state = ?1 WHERE id = ?2")
                .bind(state)
                .bind(share.id)
                .execute(files.db().writer())
                .await?;
            Ok(created)
        }
        // About a share we received: its id is theirs.
        kind @ ("SHARE_UNSHARED" | "SHARE_CHANGE_PERMISSION" | super::CHANGED) => {
            let Some(provider_id) = provider_id else {
                return Ok(answer(StatusCode::BAD_REQUEST, "providerId is missing"));
            };
            let received: Vec<Uuid> = sqlx::query_scalar(
                "SELECT id FROM ocm_received WHERE host = ?1 AND provider_id = ?2",
            )
            .bind(&host)
            .bind(&provider_id)
            .fetch_all(files.db().reader())
            .await?;
            if received.is_empty() {
                return Ok(answer(StatusCode::NOT_FOUND, "No such share"));
            }
            for id in received {
                let Some(row) = received_row(files.db().reader(), id).await? else {
                    continue;
                };
                match kind {
                    "SHARE_UNSHARED" => forget(files, &row).await?,
                    "SHARE_CHANGE_PERMISSION" => {
                        let permissions: Vec<String> = details["file"]["permissions"]
                            .as_array()
                            .map(|a| {
                                a.iter()
                                    .filter_map(|v| v.as_str().map(str::to_string))
                                    .collect()
                            })
                            .unwrap_or_else(|| row.permissions());
                        let ours = details["organizer"]["role"].clone();
                        let role = match serde_json::from_value::<Role>(ours) {
                            Ok(role)
                                if federation
                                    .discover(&host)
                                    .await
                                    .is_ok_and(|d| d.has(super::DRIVES) || d.has(super::LIVE)) =>
                            {
                                role.min(Role::Manager)
                            }
                            _ => super::role_of(&permissions),
                        };
                        set_role(files, &row, role, &permissions).await?;
                    }
                    _ => {
                        let files = files.clone();
                        tokio::spawn(async move {
                            if let Err(e) = follow(&files, id).await {
                                tracing::info!("a share from another server cannot be listed: {e}");
                            }
                        });
                    }
                }
            }
            Ok(created)
        }
        // A user of theirs is gone (§8.2): no longer a contact, their
        // shares here go, and so do ours with them.
        "USER_REMOVED" => {
            if let Some(address) = details["user"]["userId"].as_str().and_then(Address::parse)
                && address.host == host
            {
                user_removed(files, &address).await?;
            }
            Ok(created)
        }
        _ => Ok(answer(
            StatusCode::NOT_IMPLEMENTED,
            "This notification type is not taken",
        )),
    }
}

/// Someone on another server was removed there (`USER_REMOVED`): they are
/// no longer anyone's contact, what they shared with our users goes (as
/// when unshared), and what our users shared with them ends (their
/// encrypted folders then need a new key). Nothing is told back.
async fn user_removed(files: &Arc<Files>, address: &Address) -> Result<(), HttpError> {
    let address = address.to_string();
    sqlx::query("DELETE FROM contacts WHERE address = ?1")
        .bind(&address)
        .execute(files.db().writer())
        .await?;
    let received: Vec<Uuid> =
        sqlx::query_scalar("SELECT id FROM ocm_received WHERE owner = ?1 OR sender = ?1")
            .bind(&address)
            .fetch_all(files.db().reader())
            .await?;
    for id in received {
        if let Some(row) = received_row(files.db().reader(), id).await? {
            forget(files, &row).await?;
        }
    }
    let shares: Vec<Uuid> = sqlx::query_scalar("SELECT id FROM ocm_shares WHERE address = ?1")
        .bind(&address)
        .fetch_all(files.db().reader())
        .await?;
    for share in shares {
        files.federation().revoke_tokens(share);
        let mut tx = files.db().writer().begin().await?;
        super::e2e::remote_gone(&mut tx, share).await?;
        sqlx::query("DELETE FROM ocm_shares WHERE id = ?1")
            .bind(share)
            .execute(&mut *tx)
            .await?;
        tx.commit().await?;
    }
    Ok(())
}

/// A share from another server got other permissions: its recipient's
/// role follows (through the grants, so the feed and the mounts do too).
async fn set_role(
    files: &Arc<Files>,
    received: &Received,
    role: Role,
    permissions: &[String],
) -> Result<(), HttpError> {
    sqlx::query("UPDATE ocm_received SET role = ?1, permissions = ?2 WHERE id = ?3")
        .bind(role)
        .bind(serde_json::to_string(permissions).unwrap_or_default())
        .bind(received.id)
        .execute(files.db().writer())
        .await?;
    let grant: Option<Uuid> =
        sqlx::query_scalar("SELECT id FROM grants WHERE drive_id = ?1 AND user_id = ?2")
            .bind(received.drive_id)
            .bind(received.user_id)
            .fetch_optional(files.db().reader())
            .await?;
    if let Some(grant) = grant {
        files.change_grant(grant, role).await?;
    }
    Ok(())
}

/// The token of an incoming share's code flow (§10): asked of the sender's
/// token endpoint, signed, kept until shortly before it ends.
pub(crate) struct ShareToken {
    files: Weak<Files>,
    received: Uuid,
}

impl ShareToken {
    pub(crate) fn new(files: &Arc<Files>, received: Uuid) -> ShareToken {
        ShareToken {
            files: Arc::downgrade(files),
            received,
        }
    }

    pub(crate) async fn get(&self, fresh: bool) -> Result<String, String> {
        let files = self.files.upgrade().ok_or("the server is stopping")?;
        let federation = files.federation();
        if !fresh && let Some(token) = federation.token_for(self.received) {
            return Ok(token);
        }
        federation.drop_token(self.received);
        let row = received_row(files.db().reader(), self.received)
            .await
            .map_err(|e| e.to_string())?
            .ok_or("the share is gone")?;
        let secret = files
            .backends()
            .open(&row.secret)
            .map_err(|_| "the share's secret cannot be opened".to_string())?;
        let endpoint = row
            .token_endpoint
            .clone()
            .ok_or("the sender has no token endpoint")?;
        let host = federation.host().ok_or("federation is off")?;
        let body = format!(
            "grant_type=authorization_code&client_id={}&code={}",
            crate::util::form_encode(&host),
            crate::util::form_encode(&secret)
        );
        let (status, answer) = federation
            .post_signed(
                &endpoint,
                "application/x-www-form-urlencoded",
                body.into_bytes(),
            )
            .await?;
        if status != 200 {
            return Err(format!("the token endpoint answered {status}"));
        }
        #[derive(Deserialize)]
        struct TokenAnswer {
            access_token: String,
            #[serde(default)]
            expires_in: Option<u64>,
        }
        let token: TokenAnswer =
            serde_json::from_slice(&answer).map_err(|_| "the token answer is not valid")?;
        federation.keep_token(
            self.received,
            token.access_token.clone(),
            token.expires_in.unwrap_or(60),
        );
        Ok(token.access_token)
    }
}

impl crate::backends::Bearer for ShareToken {
    fn token(&self, fresh: bool) -> crate::backends::TokenFuture<'_> {
        Box::pin(async move {
            self.get(fresh)
                .await
                .map_err(|e| opendal::Error::new(opendal::ErrorKind::PermissionDenied, e))
        })
    }
}

/// The storage of a drive that is a share from another server: its
/// WebDAV, rooted at the shared folder (or at a shared file's folder,
/// the file being the drive's one file), through our guarded client with
/// the share's token.
pub async fn storage(
    files: &Arc<Files>,
    drive: Uuid,
) -> Result<crate::storage::Storage, FileError> {
    let row = received_of_drive(files.db().reader(), drive)
        .await?
        .ok_or(FileError::NotFound)?;
    let health = files.federation().health(drive);
    if row.state != ReceivedState::Accepted {
        return Err(FileError::Down("Not accepted yet".into()));
    }
    let url = files
        .federation()
        .allowed_url(&row.uri)
        .map_err(FileError::Down)?;
    let origin = match url.port() {
        Some(port) => format!(
            "{}://{}:{port}",
            url.scheme(),
            url.host_str().unwrap_or_default()
        ),
        None => format!("{}://{}", url.scheme(), url.host_str().unwrap_or_default()),
    };
    let path = url.path().to_string();
    let (root, single) = if row.resource_type == "file" {
        let (parent, key) = path
            .trim_end_matches('/')
            .rsplit_once('/')
            .unwrap_or(("", ""));
        let key = percent_decode(key);
        (format!("{parent}/"), Some((row.name.clone(), key)))
    } else {
        (format!("{}/", path.trim_end_matches('/')), None)
    };
    let builder = opendal::services::Webdav::default()
        .endpoint(&origin)
        .root(&root);
    let op = opendal::Operator::new(builder)
        .map_err(|e| FileError::Down(format!("The share cannot be reached: {e}")))?;
    let transport = crate::backends::Guarded::with_bearer(
        files.federation().client(),
        Arc::new(ShareToken {
            files: Arc::downgrade(files),
            received: row.id,
        }),
    );
    let context = op
        .context()
        .with_http_transport(opendal::HttpTransporter::new(transport));
    Ok(crate::storage::Storage::webdav(
        op.with_context(context),
        crate::storage::Dav { single },
        health,
    ))
}

pub(crate) fn percent_decode(text: &str) -> String {
    let bytes = text.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] == b'%'
            && i + 2 < bytes.len()
            && let Ok(byte) = u8::from_str_radix(&text[i + 1..i + 3], 16)
        {
            out.push(byte);
            i += 3;
            continue;
        }
        out.push(bytes[i]);
        i += 1;
    }
    String::from_utf8_lossy(&out).into_owned()
}

/// Look at an accepted share from another server and list it again if
/// anything changed: by sync-collection when its server answers it, else
/// by its ETag. The first look lists it whole.
pub async fn follow(files: &Arc<Files>, id: Uuid) -> Result<(), FileError> {
    let Some(row) = received_row(files.db().reader(), id).await? else {
        return Ok(());
    };
    if row.state != ReceivedState::Accepted {
        return Ok(());
    }
    // The listing that follows a change counts too: a server that stopped
    // answering may only show there (`remote.error`).
    let looked = match look(files, &row).await {
        Ok(true) => files.scan(row.drive_id).await.map(|_| true),
        other => other,
    };
    sqlx::query("UPDATE ocm_received SET checked_at = ?1, error = ?2 WHERE id = ?3")
        .bind(crate::auth::now_iso())
        .bind(looked.as_ref().err().map(|e| e.to_string()))
        .bind(id)
        .execute(files.db().writer())
        .await?;
    let changed = looked?;
    // An encrypted folder: the owner's ids, which its names are sealed
    // with, for what the scan found (`organizer-e2e`).
    if row.e2e
        && (changed || super::e2e::ids_missing(files, row.drive_id).await?)
        && let Err(e) = super::e2e::sync_ids(files, &row).await
    {
        tracing::info!("the ids of an encrypted folder from another server cannot be read: {e}");
    }
    Ok(())
}

/// Whether a share changed since the last look (and keep what says so).
async fn look(files: &Arc<Files>, row: &Received) -> Result<bool, FileError> {
    let tree = files.tree(row.drive_id).await?;
    let never = row.checked_at.is_none() || (row.sync_token.is_none() && row.etag.is_none());
    if row.resource_type == "folder" && row.sync_collection != Some(false) {
        match sync_collection(files, row).await {
            Ok(Some((token, any))) => {
                sqlx::query(
                    "UPDATE ocm_received SET sync_token = ?1, sync_collection = 1 WHERE id = ?2",
                )
                .bind(&token)
                .bind(row.id)
                .execute(files.db().writer())
                .await?;
                return Ok(never || any);
            }
            // The token is no longer valid: a full listing.
            Ok(None) => {
                sqlx::query("UPDATE ocm_received SET sync_token = NULL WHERE id = ?1")
                    .bind(row.id)
                    .execute(files.db().writer())
                    .await?;
                return Ok(true);
            }
            // Not asked this time: the share's error, and sync-collection
            // is asked again next time.
            Err(NoSync::Failed(reason)) => return Err(FileError::Down(reason)),
            Err(NoSync::Unsupported) => {
                sqlx::query("UPDATE ocm_received SET sync_collection = 0 WHERE id = ?1")
                    .bind(row.id)
                    .execute(files.db().writer())
                    .await?;
            }
        }
    }
    let rel = match row.resource_type.as_str() {
        "file" => row.name.clone(),
        _ => String::new(),
    };
    let etag = tree.storage.etag(&rel).await?;
    sqlx::query("UPDATE ocm_received SET etag = ?1 WHERE id = ?2")
        .bind(&etag)
        .bind(row.id)
        .execute(files.db().writer())
        .await?;
    Ok(never || etag.is_none() || etag != row.etag)
}

/// Why a sync-collection REPORT gave nothing to follow by.
enum NoSync {
    /// The other server does not answer sync-collection (or not usefully):
    /// its ETag from now on.
    Unsupported,
    /// It could not be asked this time, or refused the share (unreachable,
    /// the token refused, the share gone): the share's error, fit to show;
    /// sync-collection is asked again next time.
    Failed(String),
}

/// A sync-collection REPORT on the share (RFC 6578): the new token and
/// whether anything changed; None when the old token is no longer valid.
async fn sync_collection(
    files: &Arc<Files>,
    row: &Received,
) -> Result<Option<(String, bool)>, NoSync> {
    let token = ShareToken {
        files: Arc::downgrade(files),
        received: row.id,
    };
    let federation = files.federation();
    let url = federation
        .allowed_url(&row.uri)
        .map_err(|_| NoSync::Unsupported)?;
    let url = if url.path().ends_with('/') {
        url
    } else {
        let mut url = url;
        let path = format!("{}/", url.path());
        url.set_path(&path);
        url
    };
    let body = format!(
        r#"<?xml version="1.0" encoding="utf-8"?><d:sync-collection xmlns:d="DAV:"><d:sync-token>{}</d:sync-token><d:sync-level>infinite</d:sync-level><d:prop><d:getetag/></d:prop></d:sync-collection>"#,
        super::dav::xml_escape(row.sync_token.as_deref().unwrap_or_default())
    );
    let report = axum::http::Method::from_bytes(b"REPORT").map_err(|_| NoSync::Unsupported)?;
    let unreachable = || NoSync::Failed("it does not answer".into());
    let mut fresh = false;
    loop {
        let bearer = token.get(fresh).await.map_err(NoSync::Failed)?;
        let response = federation
            .client()
            .request(report.clone(), url.clone())
            .bearer_auth(bearer)
            .header("content-type", "application/xml; charset=utf-8")
            .header("depth", "0")
            .body(body.clone())
            .send()
            .await
            .map_err(|_| unreachable())?;
        let status = response.status().as_u16();
        if status == 401 && !fresh {
            fresh = true;
            continue;
        }
        let text = response.text().await.map_err(|_| unreachable())?;
        return match status {
            207 => {
                let token = super::dav::element_text(&text, "sync-token")
                    .filter(|t| !t.is_empty())
                    .ok_or(NoSync::Unsupported)?;
                let any = !super::dav::elements_text(&text, "href").is_empty();
                Ok(Some((token, any)))
            }
            403 | 409 if text.contains("valid-sync-token") => Ok(None),
            // The REPORT is not taken there (RFC 3253 §3.6, RFC 4918).
            400 | 405 | 415 | 501 => Err(NoSync::Unsupported),
            403 if text.contains("supported-report") => Err(NoSync::Unsupported),
            401 | 403 => Err(NoSync::Failed("it refuses the share".into())),
            404 | 410 => Err(NoSync::Failed("it no longer has the share".into())),
            _ => Err(NoSync::Failed(format!("it answered {status}"))),
        };
    }
}

/// How often an accepted share is looked at; sooner while someone has it
/// open.
const FOLLOW_EVERY: Duration = Duration::from_secs(300);
const FOLLOW_OPEN: Duration = Duration::from_secs(30);
const OPEN_FOR: Duration = Duration::from_secs(600);

/// The shares due a look.
pub async fn due(files: &Files) -> sqlx::Result<Vec<Uuid>> {
    let rows: Vec<(Uuid, Uuid, Option<String>)> = sqlx::query_as(
        "SELECT id, drive_id, checked_at FROM ocm_received WHERE state = 'accepted'",
    )
    .fetch_all(files.db().reader())
    .await?;
    let opened = files
        .federation()
        .opened
        .lock()
        .expect("not poisoned")
        .clone();
    let now = jiff::Timestamp::now();
    Ok(rows
        .into_iter()
        .filter(|(_, drive, checked)| {
            let since = checked
                .as_deref()
                .and_then(|c| c.parse::<jiff::Timestamp>().ok())
                .map(|c| now.duration_since(c).unsigned_abs())
                .unwrap_or(Duration::MAX);
            let open = opened.get(drive).is_some_and(|at| at.elapsed() < OPEN_FOR);
            since >= FOLLOW_EVERY || (open && since >= FOLLOW_OPEN)
        })
        .map(|(id, _, _)| id)
        .collect())
}

/// Follow the accepted shares from other servers: a look every few
/// minutes (sooner while someone has one open).
pub async fn run_follow(files: Arc<Files>) {
    loop {
        tokio::time::sleep(Duration::from_secs(15)).await;
        if !files.federation().on().await.unwrap_or(false) {
            continue;
        }
        let ids = match due(&files).await {
            Ok(ids) => ids,
            Err(e) => {
                tracing::warn!("cannot list the shares from other servers: {e}");
                continue;
            }
        };
        for id in ids {
            if let Err(e) = follow(&files, id).await {
                tracing::info!("a share from another server cannot be followed: {e}");
            }
        }
    }
}

/// Whether search and the AI may read shares from other servers here
/// (`federation.read_remote`).
pub async fn read_remote(files: &Files) -> bool {
    let env = crate::settings::Env::of_files(files);
    crate::settings::resolve(
        files.db(),
        &env,
        "federation.read_remote",
        crate::settings::For {
            user: None,
            drive: None,
        },
    )
    .await
    .map(|r| r.bool())
    .unwrap_or(true)
}
