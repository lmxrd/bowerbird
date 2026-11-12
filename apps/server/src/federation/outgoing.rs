//! Sharing out (docs/M7E_DESIGN.md §4): a file, a folder or (our
//! extension `organizer-drives`) a whole shared drive given to someone on
//! another server with a role, as a grant would give it to a local user.
//!
//! The share goes to their server as OCM's Share Creation Notification
//! (§7: `POST /shares`, signed), protocol `multi` with `webdav`: a relative
//! URI (the share's id under our `/ocm/dav/`), a shared secret (kept here
//! as its SHA-256), the permissions of the role (viewer → read; editor →
//! read, write; manager → read, write, share) and `must-exchange-token`
//! whenever their server can do the code flow. Their server then reaches
//! the files through `dav.rs` with a token of our code flow; what it may
//! do there is [`role_in`]: the role, cut as a grant is (limited access,
//! the re-share rule, upload only), and nothing encrypted unless both
//! servers have `organizer-e2e`.
//!
//! A change of role or the end of a share is told to their server
//! (`SHARE_CHANGE_PERMISSION`, `SHARE_UNSHARED`, §8.2). Every change in a
//! shared tree is logged per share (`ocm_changes`, [`log_change`]) for the
//! sync-collection of our WebDAV and, to servers with `organizer-live`, a
//! notice at once ([`send_live`]).

use super::address::Address;
use super::{Discovery, Trust};
use crate::error::HttpError;
use crate::files::roles::Role;
use crate::files::{Change, FileError, Files, Kind};
use axum::http::StatusCode;
use serde::Serialize;
use std::sync::Arc;
use utoipa::ToSchema;
use uuid::Uuid;

/// The `code`s of the refusals a client acts on.
pub const FEDERATION_OFF: &str = "federation_off";
pub const SERVER_REFUSED: &str = "server_refused";
pub const CONTACT_REQUIRED: &str = "contact_required";
pub const FEDERATION_FORBIDDEN: &str = "federation_forbidden";
pub const E2E_UNSUPPORTED: &str = "e2e_unsupported";
pub const DRIVES_UNSUPPORTED: &str = "drives_unsupported";
pub const REMOTE_UNREACHABLE: &str = "remote_unreachable";
pub const REMOTE_REFUSED: &str = "remote_refused";

pub(crate) fn refusal(
    status: StatusCode,
    code: &'static str,
    message: impl Into<String>,
) -> HttpError {
    HttpError::Status {
        status,
        message: message.into(),
        code: Some(code),
    }
}

/// Where a share with another server stands.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, ToSchema, sqlx::Type)]
#[serde(rename_all = "snake_case")]
#[sqlx(type_name = "TEXT", rename_all = "snake_case")]
#[schema(as = RemoteShareState)]
pub enum ShareState {
    /// Sent; the recipient has not said yet.
    Pending,
    Accepted,
    /// The recipient declined it (nothing reaches it any more).
    Declined,
}

/// A share with someone on another server, as stored.
#[derive(Clone, Debug, sqlx::FromRow)]
pub struct OcmShare {
    pub id: Uuid,
    pub drive_id: Uuid,
    /// None: the whole drive.
    pub node_id: Option<Uuid>,
    pub address: String,
    pub host: String,
    pub recipient_name: String,
    pub role: Role,
    pub created_by: Option<Uuid>,
    pub strict: bool,
    pub state: ShareState,
    pub live: bool,
    pub e2e: bool,
    pub sync_floor: i64,
    pub created_at: String,
}

pub const SHARE_COLUMNS: &str = "id, drive_id, node_id, address, host, recipient_name, role, \
     created_by, strict, state, live, e2e, sync_floor, created_at";

pub async fn share_row<'e>(
    db: impl sqlx::SqliteExecutor<'e>,
    id: Uuid,
) -> sqlx::Result<Option<OcmShare>> {
    sqlx::query_as(sqlx::AssertSqlSafe(format!(
        "SELECT {SHARE_COLUMNS} FROM ocm_shares WHERE id = ?1"
    )))
    .bind(id)
    .fetch_optional(db)
    .await
}

/// The shares with other servers of an item (`node`), or of a whole drive
/// (`node` None), the oldest first.
pub async fn shares_of<'e>(
    db: impl sqlx::SqliteExecutor<'e>,
    drive: Uuid,
    node: Option<Uuid>,
) -> sqlx::Result<Vec<OcmShare>> {
    sqlx::query_as(sqlx::AssertSqlSafe(format!(
        "SELECT {SHARE_COLUMNS} FROM ocm_shares WHERE drive_id = ?1 AND node_id IS ?2
         ORDER BY created_at, id"
    )))
    .bind(drive)
    .bind(node)
    .fetch_all(db)
    .await
}

/// The shares with other servers of a node and of what is under it, with
/// the kind of what each is of (they end when it goes to the trash).
pub async fn shares_under<'e>(
    db: impl sqlx::SqliteExecutor<'e>,
    node: Uuid,
) -> sqlx::Result<Vec<(OcmShare, Kind)>> {
    #[derive(sqlx::FromRow)]
    struct Row {
        #[sqlx(flatten)]
        share: OcmShare,
        kind: Kind,
    }
    let columns = SHARE_COLUMNS
        .split(", ")
        .map(|c| format!("s.{}", c.trim()))
        .collect::<Vec<_>>()
        .join(", ");
    let rows: Vec<Row> = sqlx::query_as(sqlx::AssertSqlSafe(format!(
        "WITH RECURSIVE down (id) AS (
           SELECT ?1
           UNION ALL
           SELECT n.id FROM nodes n JOIN down ON n.parent_id = down.id
         )
         SELECT {columns}, n.kind FROM ocm_shares s JOIN nodes n ON n.id = s.node_id
         WHERE s.node_id IN (SELECT id FROM down) AND s.state != 'declined'"
    )))
    .bind(node)
    .fetch_all(db)
    .await?;
    Ok(rows.into_iter().map(|r| (r.share, r.kind)).collect())
}

/// Tell the servers of shares that ended with their node (trashed) that
/// they did, in the background.
pub fn tell_ended(files: &Arc<Files>, ended: Vec<(OcmShare, Kind)>) {
    for (share, kind) in ended {
        let files = files.clone();
        files.federation().revoke_tokens(share.id);
        tokio::spawn(async move {
            if let Err(e) = super::e2e::remote_gone_now(&files, share.id).await {
                tracing::warn!("cannot drop the keys of a share that ended: {e}");
            }
            notify_as(&files, &share, "SHARE_UNSHARED", kind.as_str()).await;
        });
    }
}

/// Whether sharing with this server's people is allowed at all, and how
/// much we trust it: federation on, the server not refused (or listed).
pub async fn trust_of(files: &Files, host: &str) -> Result<Trust, HttpError> {
    let federation = files.federation();
    if !federation.on().await? {
        return Err(refusal(
            StatusCode::FORBIDDEN,
            FEDERATION_OFF,
            "Sharing with other servers is off on this server",
        ));
    }
    match federation.trust(host).await? {
        Trust::Refused => Err(refusal(
            StatusCode::FORBIDDEN,
            SERVER_REFUSED,
            format!("This server does not share with {host}"),
        )),
        allowed => Ok(allowed),
    }
}

/// Whether a drive may be shared with other servers
/// (`federation.allowed`: its managers, its team, the superadmin).
pub async fn drive_allows(files: &Files, drive: Uuid) -> sqlx::Result<bool> {
    let env = crate::settings::Env::of_files(files);
    Ok(crate::settings::resolve(
        files.db(),
        &env,
        "federation.allowed",
        crate::settings::For::drive(drive),
    )
    .await?
    .bool())
}

/// Contacts first (§3): a user shares with a contact, with anyone on a
/// listed server, or with any address when `federation.share_with_any`
/// allows it.
pub async fn may_share_with(
    files: &Files,
    user: Uuid,
    address: &Address,
    trust: Trust,
) -> Result<(), HttpError> {
    if trust == (Trust::Allowed { listed: true }) {
        return Ok(());
    }
    let contact: Option<i64> =
        sqlx::query_scalar("SELECT 1 FROM contacts WHERE user_id = ?1 AND address = ?2")
            .bind(user)
            .bind(address.to_string())
            .fetch_optional(files.db().reader())
            .await?;
    if contact.is_some() {
        return Ok(());
    }
    let env = crate::settings::Env::of_files(files);
    let any = crate::settings::resolve(
        files.db(),
        &env,
        "federation.share_with_any",
        crate::settings::For::user(user),
    )
    .await?
    .bool();
    if any {
        return Ok(());
    }
    Err(refusal(
        StatusCode::FORBIDDEN,
        CONTACT_REQUIRED,
        "Invite them first: you share with your contacts on other servers",
    ))
}

/// Another server's discovery, as an answer to a client.
pub(crate) async fn discover(files: &Files, host: &str) -> Result<Arc<Discovery>, HttpError> {
    files
        .federation()
        .discover(host)
        .await
        .map_err(|e| refusal(StatusCode::BAD_GATEWAY, REMOTE_UNREACHABLE, e))
}

/// A user's OCM address here.
pub(crate) async fn address_of_user(files: &Files, user: Uuid) -> Result<String, HttpError> {
    let host = files.federation().host().ok_or_else(|| {
        refusal(
            StatusCode::FORBIDDEN,
            FEDERATION_OFF,
            "PUBLIC_URL is not set",
        )
    })?;
    let email: String = sqlx::query_scalar("SELECT email FROM users WHERE id = ?1")
        .bind(user)
        .fetch_optional(files.db().reader())
        .await?
        .ok_or_else(HttpError::not_found)?;
    Ok(super::address_of(&email, &host))
}

/// Share an item (`node`) or a whole shared drive (`node` None) with
/// someone on another server, as `by` (who may share it, checked by the
/// caller, as are the role's bounds). The other server is asked first;
/// the share exists once it took it.
pub async fn share_out(
    files: &Arc<Files>,
    by: Uuid,
    drive: Uuid,
    node: Option<Uuid>,
    address: &Address,
    role: Role,
) -> Result<OcmShare, HttpError> {
    if role == Role::Owner || (node.is_some() && role == Role::UploadOnly) {
        return Err(HttpError::bad_request(
            "People on other servers can be viewers, editors or managers",
        ));
    }
    let trust = trust_of(files, &address.host).await?;
    if !drive_allows(files, drive).await? {
        return Err(refusal(
            StatusCode::FORBIDDEN,
            FEDERATION_FORBIDDEN,
            "This drive is not shared with other servers",
        ));
    }
    may_share_with(files, by, address, trust).await?;
    let drive_row = crate::files::drives::DriveRow::get(files.db().reader(), drive)
        .await?
        .ok_or_else(HttpError::not_found)?;
    if drive_row.kind == crate::files::drives::DriveKind::Remote {
        // OCM: a receiving server never reshares itself (§8.3).
        return Err(HttpError::bad_request(
            "This is on another server: only its owner's server can share it",
        ));
    }
    let (name, kind, encrypted) = match node {
        Some(id) => {
            let node = files.node(drive, id).await?;
            if node.parent_id.is_none() {
                return Err(HttpError::bad_request(
                    "Share the files and folders in it, or the whole drive",
                ));
            }
            if node.e2e_root.is_some_and(|root| root != id) {
                return Err(HttpError::bad_request(
                    "Share the encrypted folder itself, not what is in it",
                ));
            }
            (node.name.clone(), node.kind, node.e2e_root.is_some())
        }
        None => (drive_row.name.clone(), Kind::Folder, false),
    };
    let discovery = discover(files, &address.host).await?;
    let resource = match kind {
        Kind::File => "file",
        Kind::Folder => "folder",
    };
    if !discovery.receives(resource) {
        return Err(refusal(
            StatusCode::BAD_GATEWAY,
            REMOTE_REFUSED,
            format!("{} does not take shared {resource}s", address.host),
        ));
    }
    // Our extensions only between Organizer servers that have them.
    if node.is_none() && !discovery.extension(super::DRIVES) {
        return Err(refusal(
            StatusCode::BAD_REQUEST,
            DRIVES_UNSUPPORTED,
            format!(
                "{} cannot take a whole drive: share its folders instead",
                address.host
            ),
        ));
    }
    if encrypted && !discovery.extension(super::E2E) {
        return Err(refusal(
            StatusCode::BAD_REQUEST,
            E2E_UNSUPPORTED,
            format!(
                "{} cannot take encrypted folders: they are never shared with it",
                address.host
            ),
        ));
    }
    // §7, §10.4: the code flow whenever their server can do it (always
    // when it requires it); the legacy secret only for one that cannot.
    let strict = discovery.has("exchange-token") || discovery.requires("must-exchange-token");
    let id = Uuid::now_v7();
    let secret = format!("ocms_{}", crate::util::random_hex(32));
    let sender = address_of_user(files, by).await?;
    let owner = match node {
        Some(id) => match files.owner_user(id).await? {
            Some(owner) => address_of_user(files, owner).await?,
            None => sender.clone(),
        },
        None => sender.clone(),
    };
    let display = |a: &str| a.rsplit_once('@').map_or(a, |(id, _)| id).to_string();
    let mut webdav = serde_json::json!({
        "uri": id.to_string(),
        "sharedSecret": secret,
        "permissions": super::permissions_of(role),
    });
    if strict {
        webdav["requirements"] = serde_json::json!(["must-exchange-token"]);
    }
    let mut body = serde_json::json!({
        "shareWith": address.to_string(),
        "name": name,
        "providerId": id.to_string(),
        "owner": owner,
        "sender": sender,
        "ownerDisplayName": display(&owner),
        "senderDisplayName": display(&sender),
        "shareType": "user",
        "resourceType": resource,
        "protocol": { "name": "multi", "webdav": webdav },
    });
    let organizer = discovery.extension(super::DRIVES)
        || discovery.extension(super::E2E)
        || discovery.extension(super::LIVE);
    if organizer {
        body["organizer"] = serde_json::json!({
            "role": role,
            "drive": node.is_none(),
            "e2e": encrypted,
        });
    }
    sqlx::query(
        "INSERT INTO ocm_shares (id, drive_id, node_id, address, host, role, created_by,
           secret_hash, strict, live, e2e)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11)",
    )
    .bind(id)
    .bind(drive)
    .bind(node)
    .bind(address.to_string())
    .bind(&address.host)
    .bind(role)
    .bind(by)
    .bind(crate::util::sha256_hex(&secret))
    .bind(strict)
    .bind(discovery.extension(super::LIVE))
    .bind(encrypted)
    .execute(files.db().writer())
    .await
    .map_err(|e| match e {
        sqlx::Error::Database(e) if e.is_unique_violation() => {
            HttpError::conflict("They have access here already: change their role")
        }
        e => e.into(),
    })?;
    let sent = files
        .federation()
        .post_json(&discovery, "/shares", &body)
        .await;
    let taken = match sent {
        Ok((201 | 200, answer)) => Ok(answer),
        Ok((status, answer)) => Err(refusal(
            StatusCode::BAD_GATEWAY,
            REMOTE_REFUSED,
            format!(
                "{} refused the share ({status}){}",
                address.host,
                remote_reason(&answer)
            ),
        )),
        Err(e) => Err(refusal(StatusCode::BAD_GATEWAY, REMOTE_UNREACHABLE, e)),
    };
    let answer = match taken {
        Ok(answer) => answer,
        Err(e) => {
            sqlx::query("DELETE FROM ocm_shares WHERE id = ?1")
                .bind(id)
                .execute(files.db().writer())
                .await?;
            return Err(e);
        }
    };
    let name: String = serde_json::from_slice::<serde_json::Value>(&answer)
        .ok()
        .and_then(|v| v["recipientDisplayName"].as_str().map(str::to_string))
        .unwrap_or_default();
    sqlx::query("UPDATE ocm_shares SET recipient_name = ?1 WHERE id = ?2")
        .bind(name.chars().take(200).collect::<String>())
        .bind(id)
        .execute(files.db().writer())
        .await?;
    let share = share_row(files.db().reader(), id)
        .await?
        .ok_or_else(HttpError::not_found)?;
    // An encrypted folder: their public key now, so that the owner's
    // devices can wrap its keys for them at once (asked again later while
    // they have none).
    if encrypted {
        super::e2e::public_key(files, &share).await;
    }
    Ok(share)
}

/// A short reason out of another server's error answer, fit to show.
fn remote_reason(answer: &[u8]) -> String {
    let value: serde_json::Value = serde_json::from_slice(answer).unwrap_or_default();
    let said = ["message", "error"]
        .iter()
        .find_map(|k| value[*k].as_str())
        .map(|m| m.chars().take(200).collect::<String>());
    match said {
        Some(m) if !m.is_empty() => format!(": {m}"),
        _ => String::new(),
    }
}

/// Change the role of a share with another server, and tell its server
/// (`SHARE_CHANGE_PERMISSION`).
pub async fn change_role(
    files: &Arc<Files>,
    share: &OcmShare,
    role: Role,
) -> Result<OcmShare, HttpError> {
    if role == Role::Owner || (share.node_id.is_some() && role == Role::UploadOnly) {
        return Err(HttpError::bad_request(
            "People on other servers can be viewers, editors or managers",
        ));
    }
    sqlx::query("UPDATE ocm_shares SET role = ?1 WHERE id = ?2")
        .bind(role)
        .bind(share.id)
        .execute(files.db().writer())
        .await?;
    let changed = share_row(files.db().reader(), share.id)
        .await?
        .ok_or_else(HttpError::not_found)?;
    notify(files, &changed, "SHARE_CHANGE_PERMISSION").await;
    Ok(changed)
}

/// End a share with another server, and tell its server
/// (`SHARE_UNSHARED`); its tokens stop at once.
pub async fn unshare(files: &Arc<Files>, share: &OcmShare) -> Result<(), HttpError> {
    // An encrypted folder's member on another server: their keys go, and
    // the next generation is not for them.
    let mut tx = files.db().writer().begin().await?;
    super::e2e::remote_gone(&mut tx, share.id).await?;
    sqlx::query("DELETE FROM ocm_shares WHERE id = ?1")
        .bind(share.id)
        .execute(&mut *tx)
        .await?;
    tx.commit().await?;
    files.federation().revoke_tokens(share.id);
    if share.state != ShareState::Declined {
        notify(files, share, "SHARE_UNSHARED").await;
    }
    Ok(())
}

/// A user about to be removed: their OCM address and the servers to tell
/// (`USER_REMOVED`, §8.2): their contacts', and those of the shares they
/// made, that are of their drive, or that they received. None while
/// federation has no address of ours.
pub async fn removed_user(
    files: &Files,
    user: Uuid,
) -> Result<Option<(String, Vec<String>)>, HttpError> {
    let Some(host) = files.federation().host() else {
        return Ok(None);
    };
    let email: Option<String> = sqlx::query_scalar("SELECT email FROM users WHERE id = ?1")
        .bind(user)
        .fetch_optional(files.db().reader())
        .await?;
    let Some(email) = email else {
        return Ok(None);
    };
    let hosts: Vec<String> = sqlx::query_scalar(
        "SELECT host FROM contacts WHERE user_id = ?1
         UNION SELECT host FROM ocm_shares WHERE created_by = ?1 OR drive_id = ?1
         UNION SELECT host FROM ocm_received WHERE user_id = ?1
         ORDER BY 1",
    )
    .bind(user)
    .fetch_all(files.db().reader())
    .await?;
    Ok(Some((super::address_of(&email, &host), hosts)))
}

/// Tell those servers that the user was removed (`USER_REMOVED`), in the
/// background, once each; not retried (as every notification).
pub fn tell_removed(files: &Arc<Files>, removed: Option<(String, Vec<String>)>) {
    let Some((address, hosts)) = removed else {
        return;
    };
    let files = files.clone();
    tokio::spawn(async move {
        let federation = files.federation();
        let Some(ours) = federation.host() else {
            return;
        };
        let body = serde_json::json!({
            "notificationType": "USER_REMOVED",
            "senderDomain": ours,
            "resourceType": "user",
            "notification": { "user": { "userId": address } },
        });
        for host in hosts {
            let Ok(discovery) = federation.discover(&host).await else {
                tracing::info!("a server cannot be reached to tell it a user was removed");
                continue;
            };
            match federation
                .post_json(&discovery, "/notifications", &body)
                .await
            {
                Ok((status, _)) if (200..300).contains(&status) => {}
                Ok((status, _)) => {
                    tracing::info!("a server answered {status} to a user's removal")
                }
                Err(_) => {
                    tracing::info!("a server cannot be reached to tell it a user was removed")
                }
            }
        }
    });
}

/// The kind of what a share is of, as OCM names it.
async fn resource_of(files: &Files, share: &OcmShare) -> &'static str {
    let Some(node) = share.node_id else {
        return "folder";
    };
    match files.node(share.drive_id, node).await.map(|n| n.kind) {
        Ok(Kind::File) => "file",
        _ => "folder",
    }
}

/// Tell a share's server of a change to it (§8.2). A server that does not
/// answer is not asked again: OCM lets a sender revoke without telling,
/// and its tokens stop working anyway. Nothing of it is logged but that it
/// failed.
pub(crate) async fn notify(files: &Files, share: &OcmShare, kind: &str) {
    let resource = resource_of(files, share).await;
    notify_as(files, share, kind, resource).await
}

/// [`notify`], the resource's type given (its node may be gone).
pub(crate) async fn notify_as(files: &Files, share: &OcmShare, kind: &str, resource: &str) {
    let federation = files.federation();
    let Some(host) = federation.host() else {
        return;
    };
    let Ok(discovery) = federation.discover(&share.host).await else {
        tracing::info!("a share's server cannot be reached to tell it of a change");
        return;
    };
    let mut file = serde_json::json!({ "providerId": share.id.to_string() });
    if kind == "SHARE_CHANGE_PERMISSION" {
        file["permissions"] = serde_json::json!(super::permissions_of(share.role));
    }
    let mut notification = serde_json::json!({ "file": file });
    if discovery.extension(super::DRIVES) || discovery.extension(super::LIVE) {
        notification["organizer"] = serde_json::json!({ "role": share.role });
    }
    let body = serde_json::json!({
        "notificationType": kind,
        "senderDomain": host,
        "resourceType": resource,
        "shareType": "user",
        "notification": notification,
    });
    match federation
        .post_json(&discovery, "/notifications", &body)
        .await
    {
        Ok((status, _)) if (200..300).contains(&status) => {}
        Ok((status, _)) => tracing::info!("a share's server answered {status} to a notification"),
        Err(_) => tracing::info!("a share's server cannot be reached to tell it of a change"),
    }
}

/// What a share's recipient may do with a node (its top or anything
/// under it): the share's role, as a grant gives it (§4): a limited folder
/// under the top hides what is under it; a share given by someone who is
/// not a keeper gives no more than they still have; upload only sees the
/// top and what it added; nothing encrypted reaches a share without
/// `organizer-e2e`. None: not seen (not found).
pub async fn role_in(
    files: &Files,
    share: &OcmShare,
    node: Uuid,
) -> Result<Option<Role>, FileError> {
    if share.state == ShareState::Declined {
        return Ok(None);
    }
    let mut conn = files.db().reader().acquire().await?;
    let chain = crate::files::roles::chain(&mut conn, node).await?;
    let Some(first) = chain.first() else {
        return Ok(None);
    };
    if first.drive_id != share.drive_id {
        return Ok(None);
    }
    let role = match share.node_id {
        Some(top) => {
            let Some(at) = chain.iter().position(|u| u.id == top) else {
                return Ok(None);
            };
            // Limited access below the top: only the grants made there
            // count, and this share is above.
            if chain[..at].iter().any(|u| u.limited) {
                return Ok(None);
            }
            let role = share.role.min(Role::Manager);
            match share.created_by {
                Some(giver) => match crate::files::roles::resolve(&mut conn, giver, top).await? {
                    Some(theirs) => role.min(theirs.role),
                    None => return Ok(None),
                },
                None => return Ok(None),
            }
        }
        // A whole drive: as a drive grant, cut by any limited folder.
        None => {
            if chain.iter().any(|u| u.limited) {
                return Ok(None);
            }
            share.role.min(Role::Manager)
        }
    };
    let encrypted: Option<Option<Uuid>> =
        sqlx::query_scalar("SELECT e2e_root FROM nodes WHERE id = ?1")
            .bind(node)
            .fetch_optional(&mut *conn)
            .await?;
    if encrypted.flatten().is_some() && !share.e2e {
        return Ok(None);
    }
    if role == Role::UploadOnly {
        let top = match share.node_id {
            Some(top) => Some(top),
            None => chain.last().map(|u| u.id),
        };
        let added: Option<i64> =
            sqlx::query_scalar("SELECT 1 FROM ocm_created WHERE node_id = ?1 AND share_id = ?2")
                .bind(node)
                .bind(share.id)
                .fetch_optional(&mut *conn)
                .await?;
        if Some(node) != top && added.is_none() {
            return Ok(None);
        }
    }
    Ok(Some(role))
}

/// Note that a share's recipient added a node (upload only sees it).
pub async fn mark_created(files: &Files, node: Uuid, share: Uuid) -> sqlx::Result<()> {
    sqlx::query("INSERT OR IGNORE INTO ocm_created (node_id, share_id) VALUES (?1, ?2)")
        .bind(node)
        .bind(share)
        .execute(files.db().writer())
        .await?;
    Ok(())
}

/// The top of a share: its node, or its drive's root.
pub async fn top_of(files: &Arc<Files>, share: &OcmShare) -> Result<crate::files::Node, FileError> {
    match share.node_id {
        Some(id) => files.node(share.drive_id, id).await,
        None => files.root_node(share.drive_id).await,
    }
}

/// Log a change of a drive in the shares with other servers it is in
/// (`ocm_changes`, inside the change's transaction): the path from each
/// share's top, and whether it went (deleted, or moved out of the share).
pub async fn log_change(
    tx: &mut sqlx::SqliteConnection,
    drive_id: Uuid,
    change: &Change<'_>,
) -> sqlx::Result<()> {
    let shares: Vec<(Uuid, Option<Uuid>)> = sqlx::query_as(
        "SELECT id, node_id FROM ocm_shares WHERE drive_id = ?1 AND state != 'declined'",
    )
    .bind(drive_id)
    .fetch_all(&mut *tx)
    .await?;
    if shares.is_empty() {
        return Ok(());
    }
    let now = crate::files::path_of(&mut *tx, change.node_id).await?;
    let before: Option<Vec<String>> = change.from_path.map(|p| {
        p.split('/')
            .filter(|n| !n.is_empty())
            .map(str::to_string)
            .collect()
    });
    for (share, top) in shares {
        let top = match top {
            Some(top) => crate::files::path_of(&mut *tx, top).await?,
            None => Vec::new(),
        };
        let under = |path: &[String]| path.starts_with(&top).then(|| path[top.len()..].join("/"));
        let mut rows: Vec<(String, bool)> = Vec::new();
        if let Some(before) = &before
            && let Some(rel) = under(before)
        {
            rows.push((rel, true));
        }
        if let Some(rel) = under(&now) {
            rows.push((rel, change.op == "delete"));
        }
        for (path, gone) in rows {
            sqlx::query("INSERT INTO ocm_changes (share_id, path, gone) VALUES (?1, ?2, ?3)")
                .bind(share)
                .bind(path)
                .bind(gone)
                .execute(&mut *tx)
                .await?;
        }
    }
    Ok(())
}

/// Log that a folder's limited access changed (it appears or goes for
/// the shares above it): its path in each share it is in.
pub async fn log_access(files: &Files, node: Uuid) -> sqlx::Result<()> {
    let mut tx = files.db().writer().begin().await?;
    let drive: Option<Uuid> = sqlx::query_scalar("SELECT drive_id FROM nodes WHERE id = ?1")
        .bind(node)
        .fetch_optional(&mut *tx)
        .await?;
    if let Some(drive) = drive {
        log_change(
            &mut tx,
            drive,
            &Change {
                node_id: node,
                op: "update",
                kind: None,
                parent_id: None,
                name: None,
                version: None,
                from_parent: None,
                from_path: None,
            },
        )
        .await?;
    }
    tx.commit().await
}

/// Drop the change log past what the change feed keeps, raising each
/// share's floor: an older sync-token is invalid then.
pub async fn prune(db: &crate::db::Db) -> sqlx::Result<()> {
    let cutoff = crate::auth::iso(std::time::SystemTime::now() - crate::files::CHANGES_KEPT);
    let mut tx = db.writer().begin().await?;
    sqlx::query(
        "UPDATE ocm_shares SET sync_floor = max(sync_floor, (
           SELECT max(seq) FROM ocm_changes WHERE share_id = ocm_shares.id AND at < ?1))
         WHERE EXISTS (SELECT 1 FROM ocm_changes WHERE share_id = ocm_shares.id AND at < ?1)",
    )
    .bind(&cutoff)
    .execute(&mut *tx)
    .await?;
    sqlx::query("DELETE FROM ocm_changes WHERE at < ?1")
        .bind(&cutoff)
        .execute(&mut *tx)
        .await?;
    tx.commit().await
}

/// Send the live notices (`organizer-live`, §6) of the changes logged
/// since the last round: one per share that changed, to servers that
/// have the extension and accepted the share.
pub async fn send_live(files: &Files) -> sqlx::Result<()> {
    use std::sync::atomic::Ordering;
    let federation = files.federation();
    let mark = federation.live_mark.load(Ordering::SeqCst);
    let changed: Vec<(Uuid, i64)> = sqlx::query_as(
        "SELECT c.share_id, max(c.seq) FROM ocm_changes c JOIN ocm_shares s ON s.id = c.share_id
         WHERE c.seq > ?1 AND s.live = 1 AND s.state = 'accepted'
         GROUP BY c.share_id",
    )
    .bind(mark)
    .fetch_all(files.db().reader())
    .await?;
    let top: Option<i64> = sqlx::query_scalar("SELECT max(seq) FROM ocm_changes")
        .fetch_one(files.db().reader())
        .await?;
    federation
        .live_mark
        .store(top.unwrap_or(mark).max(mark), Ordering::SeqCst);
    for (id, _) in changed {
        if let Some(share) = share_row(files.db().reader(), id).await? {
            notify(files, &share, super::CHANGED).await;
        }
    }
    Ok(())
}

/// Follow the change feed's notices and send the live notices half a
/// second after a burst of changes; prune the log hourly.
pub async fn run_live(files: Arc<Files>) {
    let mut events = files.subscribe();
    if let Ok(Some(top)) = sqlx::query_scalar::<_, Option<i64>>("SELECT max(seq) FROM ocm_changes")
        .fetch_one(files.db().reader())
        .await
    {
        files
            .federation()
            .live_mark
            .store(top, std::sync::atomic::Ordering::SeqCst);
    }
    let mut pruned = std::time::Instant::now();
    loop {
        match events.recv().await {
            Ok(_) | Err(tokio::sync::broadcast::error::RecvError::Lagged(_)) => {}
            Err(tokio::sync::broadcast::error::RecvError::Closed) => return,
        }
        tokio::time::sleep(std::time::Duration::from_millis(500)).await;
        while events.try_recv().is_ok() {}
        if let Err(e) = send_live(&files).await {
            tracing::warn!("cannot send the live notices: {e}");
        }
        if pruned.elapsed() > std::time::Duration::from_secs(3_600) {
            pruned = std::time::Instant::now();
            if let Err(e) = prune(files.db()).await {
                tracing::warn!("cannot prune the shares' change log: {e}");
            }
        }
    }
}
