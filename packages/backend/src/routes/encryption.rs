//! End-to-end encryption's keys (docs/M6E_DESIGN.md §2, docs/E2E_SPEC.md):
//! each user's key pair (the private key only wrapped, under their
//! passphrase and under their recovery key), and each encrypted folder's
//! keys wrapped for each member. The server checks shapes, members and
//! generations; it can open none of them.
//!
//! The devices do the rest: the one that makes an encrypted folder posts
//! its first key; a member's device wraps the folder's keys for members who
//! have none yet (someone shared with later, or who set up encryption
//! since: `GET /encryption/pending`), and makes a new generation when a
//! member lost access (`needs_new_key`).

use crate::error::{ErrorBody, HttpError};
use crate::extract::ApiJson;
use crate::files::e2e;
use crate::files::shares::Need;
use crate::routes::auth::{AuthUser, SessionUser};
use crate::state::AppState;
use axum::Json;
use axum::extract::{Path, State};
use axum::http::StatusCode;
use serde::{Deserialize, Serialize};
use utoipa::ToSchema;
use utoipa_axum::router::OpenApiRouter;
use utoipa_axum::routes;
use uuid::Uuid;

pub fn router() -> OpenApiRouter<AppState> {
    OpenApiRouter::new()
        .routes(routes!(get_encryption, set_up_encryption))
        .routes(routes!(change_passphrase))
        .routes(routes!(list_encryption_tasks))
        .routes(routes!(get_folder_keys))
        .routes(routes!(add_folder_keys))
}

/// The fewest PBKDF2 iterations accepted (docs/E2E_SPEC.md §2).
const MIN_ITERATIONS: i64 = 100_000;
/// The most: a count a device can still derive in reasonable time.
const MAX_ITERATIONS: i64 = 10_000_000;

/// The private key wrapped under a key derived from the passphrase.
#[derive(Clone, Debug, Serialize, Deserialize, ToSchema, sqlx::FromRow)]
pub struct PassphraseWrap {
    /// The PBKDF2 salt (16 to 64 bytes), base64url.
    pub salt: String,
    /// PBKDF2-HMAC-SHA256 iterations: 600,000 for a new passphrase.
    pub iterations: i64,
    /// The wrapped private key (61 bytes), base64url.
    pub wrapped: String,
}

/// A user's keys as the server keeps them.
#[derive(Serialize, ToSchema)]
pub struct EncryptionKeys {
    /// Encryption is set up: the rest is present.
    pub set_up: bool,
    /// The X25519 public key (32 bytes), base64url.
    #[schema(required = true)]
    pub public_key: Option<String>,
    #[schema(required = true)]
    pub passphrase: Option<PassphraseWrap>,
    /// The private key wrapped under the recovery key (61 bytes), base64url.
    #[schema(required = true)]
    pub recovery_wrapped: Option<String>,
}

#[derive(sqlx::FromRow)]
struct KeysRow {
    public_key: String,
    passphrase_salt: String,
    passphrase_iterations: i64,
    passphrase_wrapped: String,
    recovery_wrapped: String,
}

/// A user's keys, if they set up encryption.
pub async fn keys_of(state: &AppState, user: Uuid) -> Result<Option<EncryptionKeys>, HttpError> {
    let row: Option<KeysRow> = sqlx::query_as(
        "SELECT public_key, passphrase_salt, passphrase_iterations, passphrase_wrapped,
           recovery_wrapped FROM e2e_keys WHERE user_id = ?1",
    )
    .bind(user)
    .fetch_optional(state.db.reader())
    .await?;
    Ok(row.map(|row| EncryptionKeys {
        set_up: true,
        public_key: Some(row.public_key),
        passphrase: Some(PassphraseWrap {
            salt: row.passphrase_salt,
            iterations: row.passphrase_iterations,
            wrapped: row.passphrase_wrapped,
        }),
        recovery_wrapped: Some(row.recovery_wrapped),
    }))
}

/// Unpadded base64url of `min..=max` bytes, else a 400 naming `what`.
/// `versioned`: the first byte is the format's version, 1.
fn check_b64(
    value: &str,
    min: usize,
    max: usize,
    versioned: bool,
    what: &str,
) -> Result<(), HttpError> {
    let bytes = crate::util::base64url_decode(value)
        .ok_or_else(|| HttpError::bad_request(format!("{what}: not unpadded base64url")))?;
    if bytes.len() < min || bytes.len() > max {
        return Err(HttpError::bad_request(format!("{what}: the wrong length")));
    }
    if versioned && bytes.first() != Some(&1) {
        return Err(HttpError::bad_request(format!(
            "{what}: an unknown format version"
        )));
    }
    Ok(())
}

fn check_passphrase(wrap: &PassphraseWrap) -> Result<(), HttpError> {
    check_b64(&wrap.salt, 16, 64, false, "salt")?;
    check_b64(&wrap.wrapped, 61, 61, true, "wrapped")?;
    if !(MIN_ITERATIONS..=MAX_ITERATIONS).contains(&wrap.iterations) {
        return Err(HttpError::bad_request(
            "iterations: from 100,000 to 10,000,000",
        ));
    }
    Ok(())
}

#[utoipa::path(
    get,
    path = "",
    tag = "Encryption",
    operation_id = "getEncryption",
    summary = "Your encryption keys",
    description = "The public key and the private key wrapped under the passphrase and under the recovery key (docs/E2E_SPEC.md): a device unlocks with either. `set_up` is false until encryption is set up.",
    responses(
        (status = 200, description = "The keys", body = EncryptionKeys),
        (status = 401, description = "Not signed in", body = ErrorBody),
    ),
    security(("session" = []), ("api_key" = [])),
)]
async fn get_encryption(
    State(state): State<AppState>,
    user: AuthUser,
) -> Result<Json<EncryptionKeys>, HttpError> {
    Ok(Json(keys_of(&state, user.0.id).await?.unwrap_or(
        EncryptionKeys {
            set_up: false,
            public_key: None,
            passphrase: None,
            recovery_wrapped: None,
        },
    )))
}

/// A new key pair, its private key wrapped twice by the device.
#[derive(Deserialize, ToSchema)]
pub struct SetUpEncryption {
    /// The X25519 public key (32 bytes), base64url.
    pub public_key: String,
    pub passphrase: PassphraseWrap,
    /// The private key wrapped under the recovery key (61 bytes), base64url.
    pub recovery_wrapped: String,
}

#[utoipa::path(
    post,
    path = "",
    tag = "Encryption",
    operation_id = "setUpEncryption",
    summary = "Set up encryption",
    description = "Once: the keys of a user who has none. They cannot be replaced (the folders' keys are wrapped for them); a forgotten passphrase is changed with the recovery key.",
    request_body(content = SetUpEncryption, content_type = "application/json"),
    responses(
        (status = 201, description = "Set up", body = EncryptionKeys),
        (status = 400, description = "A malformed key", body = ErrorBody),
        (status = 401, description = "Not signed in", body = ErrorBody),
        (status = 403, description = "Not with an API key", body = ErrorBody),
        (status = 409, description = "Set up already", body = ErrorBody),
        (status = 415, description = "The body must be JSON (Content-Type: application/json)", body = ErrorBody),
    ),
    security(("session" = [])),
)]
async fn set_up_encryption(
    State(state): State<AppState>,
    user: SessionUser,
    ApiJson(body): ApiJson<SetUpEncryption>,
) -> Result<(StatusCode, Json<EncryptionKeys>), HttpError> {
    check_b64(&body.public_key, 32, 32, false, "public_key")?;
    check_passphrase(&body.passphrase)?;
    check_b64(&body.recovery_wrapped, 61, 61, true, "recovery_wrapped")?;
    let inserted = sqlx::query(
        "INSERT INTO e2e_keys (user_id, public_key, passphrase_salt, passphrase_iterations,
           passphrase_wrapped, recovery_wrapped)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6) ON CONFLICT (user_id) DO NOTHING",
    )
    .bind(user.0.id)
    .bind(&body.public_key)
    .bind(&body.passphrase.salt)
    .bind(body.passphrase.iterations)
    .bind(&body.passphrase.wrapped)
    .bind(&body.recovery_wrapped)
    .execute(state.db.writer())
    .await?;
    if inserted.rows_affected() == 0 {
        return Err(HttpError::conflict("Encryption is set up already"));
    }
    let keys = keys_of(&state, user.0.id)
        .await?
        .ok_or_else(|| HttpError::internal("the keys just stored are gone"))?;
    Ok((StatusCode::CREATED, Json(keys)))
}

#[utoipa::path(
    put,
    path = "/passphrase",
    tag = "Encryption",
    operation_id = "changePassphrase",
    summary = "Change the encryption passphrase",
    description = "The same private key wrapped under a new passphrase (and a new salt), by a device that unlocked it with the passphrase or the recovery key.",
    request_body(content = PassphraseWrap, content_type = "application/json"),
    responses(
        (status = 200, description = "Changed", body = EncryptionKeys),
        (status = 400, description = "A malformed key, or not set up", body = ErrorBody),
        (status = 401, description = "Not signed in", body = ErrorBody),
        (status = 403, description = "Not with an API key", body = ErrorBody),
        (status = 415, description = "The body must be JSON (Content-Type: application/json)", body = ErrorBody),
    ),
    security(("session" = [])),
)]
async fn change_passphrase(
    State(state): State<AppState>,
    user: SessionUser,
    ApiJson(body): ApiJson<PassphraseWrap>,
) -> Result<Json<EncryptionKeys>, HttpError> {
    check_passphrase(&body)?;
    let changed = sqlx::query(
        "UPDATE e2e_keys SET passphrase_salt = ?2, passphrase_iterations = ?3,
           passphrase_wrapped = ?4, updated_at = strftime('%Y-%m-%dT%H:%M:%fZ', 'now')
         WHERE user_id = ?1",
    )
    .bind(user.0.id)
    .bind(&body.salt)
    .bind(body.iterations)
    .bind(&body.wrapped)
    .execute(state.db.writer())
    .await?;
    if changed.rows_affected() == 0 {
        return Err(HttpError::bad_request("Set up encryption first"));
    }
    Ok(Json(keys_of(&state, user.0.id).await?.ok_or_else(
        || HttpError::internal("the keys just stored are gone"),
    )?))
}

/// A folder key of a generation, wrapped for the user.
#[derive(Serialize, Deserialize, ToSchema, sqlx::FromRow)]
pub struct WrappedKey {
    pub generation: i64,
    /// docs/E2E_SPEC.md §4 (93 bytes), base64url.
    pub wrapped: String,
}

/// A member of an encrypted folder.
#[derive(Serialize, ToSchema)]
pub struct Member {
    /// The user; for a member on another server (`remote`), the id of the
    /// share with them (their `Grant.id`), which their keys are posted for
    /// as `MemberKey.user_id`.
    pub user_id: Uuid,
    /// Their email; for a member on another server, their address.
    pub email: String,
    /// Their public key, base64url; null while they have not set up
    /// encryption ("waiting for them to set up encryption").
    #[schema(required = true)]
    pub public_key: Option<String>,
    /// The generations they have no key of yet: a member's device wraps
    /// them (only when they have a public key).
    pub missing: Vec<i64>,
    /// On another server (docs/M7E_DESIGN.md §10, `organizer-e2e`): their
    /// address (`user@host`). Their public key comes from their server;
    /// the keys wrapped for them stay here and are served to it. Null for a
    /// user of this server.
    #[schema(required = true)]
    pub remote: Option<String>,
}

/// An encrypted folder's keys, as one of its members sees them.
#[derive(Serialize, ToSchema)]
pub struct FolderKeys {
    /// The encrypted folder (the top one): the id the keys are wrapped
    /// with. For a mirror's copy, the drive's folder it copies; for a
    /// folder shared from another server, the owner's folder there.
    pub folder_id: Uuid,
    /// The newest generation, which new names and contents use; 0 until
    /// the first key is posted.
    pub generation: i64,
    /// A member lost access (or there is no key yet): the next device
    /// makes a new generation for the members left.
    pub needs_new_key: bool,
    /// The user's own wrapped keys, every generation they have.
    pub keys: Vec<WrappedKey>,
    /// The owner first.
    pub members: Vec<Member>,
}

/// The encrypted folder (the top one) a node is in, which the user can
/// read; a node that is not encrypted is a 400.
async fn folder_of(state: &AppState, user: &AuthUser, id: Uuid) -> Result<Uuid, HttpError> {
    let access = state.files.access_for(user.0.id, id, Need::Read).await?;
    access
        .node
        .e2e_root
        .ok_or_else(|| HttpError::bad_request("Not an encrypted folder"))
}

async fn folder_keys(state: &AppState, user: Uuid, root: Uuid) -> Result<FolderKeys, HttpError> {
    let (generation, needs_new_key): (i64, bool) =
        sqlx::query_as("SELECT generation, needs_new_key FROM e2e_folders WHERE root_id = ?1")
            .bind(root)
            .fetch_optional(state.db.reader())
            .await?
            .ok_or_else(HttpError::not_found)?;
    let keys: Vec<WrappedKey> = sqlx::query_as(
        "SELECT generation, wrapped FROM e2e_folder_keys
         WHERE root_id = ?1 AND user_id = ?2 ORDER BY generation",
    )
    .bind(root)
    .bind(user)
    .fetch_all(state.db.reader())
    .await?;
    let mut members = Vec::new();
    for member in e2e::members(&state.db, root).await? {
        let (email, public_key): (String, Option<String>) = sqlx::query_as(
            "SELECT u.email, k.public_key FROM users u
             LEFT JOIN e2e_keys k ON k.user_id = u.id WHERE u.id = ?1",
        )
        .bind(member)
        .fetch_one(state.db.reader())
        .await?;
        let held: Vec<i64> = sqlx::query_scalar(
            "SELECT generation FROM e2e_folder_keys WHERE root_id = ?1 AND user_id = ?2",
        )
        .bind(root)
        .bind(member)
        .fetch_all(state.db.reader())
        .await?;
        let missing = if public_key.is_some() {
            (1..=generation).filter(|g| !held.contains(g)).collect()
        } else {
            Vec::new()
        };
        members.push(Member {
            user_id: member,
            email,
            public_key,
            missing,
            remote: None,
        });
    }
    // Members on other servers: their public keys asked of their servers
    // when not here yet, their keys as kept here.
    let mut remote = crate::federation::e2e::remote_members(&state.files, root).await?;
    crate::federation::e2e::ask_public_keys(&state.files, &mut remote).await;
    for member in remote {
        let held: Vec<i64> = sqlx::query_scalar(
            "SELECT generation FROM e2e_remote_keys WHERE root_id = ?1 AND share_id = ?2",
        )
        .bind(root)
        .bind(member.share_id)
        .fetch_all(state.db.reader())
        .await?;
        let missing = if member.public_key.is_some() {
            (1..=generation).filter(|g| !held.contains(g)).collect()
        } else {
            Vec::new()
        };
        members.push(Member {
            user_id: member.share_id,
            email: member.address.clone(),
            public_key: member.public_key,
            missing,
            remote: Some(member.address),
        });
    }
    Ok(FolderKeys {
        folder_id: root,
        generation,
        needs_new_key,
        keys,
        members,
    })
}

#[utoipa::path(
    get,
    path = "/folders/{id}",
    tag = "Encryption",
    operation_id = "getFolderKeys",
    summary = "An encrypted folder's keys",
    description = "For the encrypted folder a file or folder is in: the user's wrapped keys of each generation, and the members with their public keys and the generations they lack. A member on another server (`remote`, docs/M7E_DESIGN.md §10) is the share with them: `user_id` is its id, their public key comes from their server.\n\nShared from another server (a remote drive's encrypted folder, `organizer-e2e`): the keys stay on the owner's server, which this server asks for the user's own; `folder_id` is the owner's folder there, the id they are wrapped with, and every node's `source_id` is its id there, the one its children's names are sealed with (as in a mirror). They are answered for a node of it or by its `source_id`; `members` is empty and `needs_new_key` false (the owner's devices see to the keys). 502 with `code` `remote_unreachable` when that server does not answer.\n\nIn a mirror (and a drive ejected from one), an encrypted folder is a copy sealed with the drive's ids (`Node.source_id`), opened with the keys the mirror kept: the user's own wrappings of the drive's folder, every generation they had, kept while it copied. They are answered for a node of the copy, or for the drive's folder or anything in it (by `source_id`) once the user can no longer read it there (the drive deleted, access lost): then `folder_id` is the drive's folder, the id the keys and the names are sealed with (not the copy's), `members` is empty and `needs_new_key` false (nothing to post: the keys are only kept).",
    params(("id" = Uuid, Path, description = "The encrypted folder, or anything in it; in a mirror, the copy or the drive's folder it copies (`source_id`)")),
    responses(
        (status = 200, description = "The keys", body = FolderKeys),
        (status = 400, description = "Not an encrypted folder", body = ErrorBody),
        (status = 401, description = "Not signed in", body = ErrorBody),
        (status = 404, description = "Not found", body = ErrorBody),
        (status = 502, description = "Shared from another server that does not answer (`code` `remote_unreachable`)", body = ErrorBody),
    ),
    security(("session" = []), ("api_key" = [])),
)]
async fn get_folder_keys(
    State(state): State<AppState>,
    user: AuthUser,
    Path(id): Path<Uuid>,
) -> Result<Json<FolderKeys>, HttpError> {
    // Shared from another server (`organizer-e2e`): the keys are there,
    // asked of it for this user.
    if let Some((node, received)) =
        crate::federation::e2e::received_for(&state.files, user.0.id, id).await?
    {
        folder_of(&state, &user, node).await?;
        let remote = crate::federation::e2e::fetch_keys(&state.files, &received)
            .await
            .map_err(|e| match e {
                crate::files::FileError::Down(reason) => crate::federation::outgoing::refusal(
                    StatusCode::BAD_GATEWAY,
                    crate::federation::outgoing::REMOTE_UNREACHABLE,
                    reason,
                ),
                e => e.into(),
            })?;
        return Ok(Json(FolderKeys {
            folder_id: remote.folder_id,
            generation: remote.generation,
            needs_new_key: false,
            keys: remote
                .keys
                .into_iter()
                .map(|(generation, wrapped)| WrappedKey {
                    generation,
                    wrapped,
                })
                .collect(),
            members: Vec::new(),
        }));
    }
    let refused = match folder_of(&state, &user, id).await {
        Ok(root) => {
            let held: Option<i64> =
                sqlx::query_scalar("SELECT 1 FROM e2e_folders WHERE root_id = ?1")
                    .bind(root)
                    .fetch_optional(state.db.reader())
                    .await?;
            if held.is_some() {
                return Ok(Json(folder_keys(&state, user.0.id, root).await?));
            }
            // A mirror's copy: it has no keys of its own.
            HttpError::not_found()
        }
        Err(e) => e,
    };
    // A mirror's copy, or the drive's folder it copies, out of reach.
    match e2e::kept_keys(&state.db, user.0.id, id).await? {
        Some((source, kept)) => Ok(Json(FolderKeys {
            folder_id: source,
            generation: kept.iter().map(|(g, _)| *g).max().unwrap_or(0),
            needs_new_key: false,
            keys: kept
                .into_iter()
                .map(|(generation, wrapped)| WrappedKey {
                    generation,
                    wrapped,
                })
                .collect(),
            members: Vec::new(),
        })),
        None => Err(refused),
    }
}

/// A folder key wrapped for a member.
#[derive(Deserialize, ToSchema)]
pub struct MemberKey {
    pub user_id: Uuid,
    pub generation: i64,
    /// docs/E2E_SPEC.md §4 (93 bytes), base64url.
    pub wrapped: String,
}

/// Folder keys for members: of generations that exist, for members who
/// lack them; or a new generation (the newest + 1), which must come with
/// a key for every member who has a public key.
#[derive(Deserialize, ToSchema)]
pub struct NewFolderKeys {
    pub keys: Vec<MemberKey>,
}

#[utoipa::path(
    post,
    path = "/folders/{id}/keys",
    tag = "Encryption",
    operation_id = "addFolderKeys",
    summary = "Post an encrypted folder's keys for its members",
    description = "Wrapped by a member's device. A key a member has already is kept as it is. A member on another server (`Member.remote`) is named by the share's id, once their public key is known here. A new generation (the newest + 1, or the first) needs a key for every member with a public key, the poster's included, and clears `needs_new_key`.",
    params(("id" = Uuid, Path, description = "The encrypted folder, or anything in it")),
    request_body(content = NewFolderKeys, content_type = "application/json"),
    responses(
        (status = 200, description = "Stored", body = FolderKeys),
        (status = 400, description = "Not an encrypted folder, not a member, a malformed key, or a generation that is not the next", body = ErrorBody),
        (status = 401, description = "Not signed in", body = ErrorBody),
        (status = 403, description = "A read-only API key", body = ErrorBody),
        (status = 404, description = "Not found", body = ErrorBody),
        (status = 409, description = "Another device made that generation first", body = ErrorBody),
        (status = 415, description = "The body must be JSON (Content-Type: application/json)", body = ErrorBody),
    ),
    security(("session" = []), ("api_key" = [])),
)]
async fn add_folder_keys(
    State(state): State<AppState>,
    user: AuthUser,
    Path(id): Path<Uuid>,
    ApiJson(body): ApiJson<NewFolderKeys>,
) -> Result<Json<FolderKeys>, HttpError> {
    let root = folder_of(&state, &user, id).await?;
    let members = e2e::members(&state.db, root).await?;
    if !members.contains(&user.0.id) {
        return Err(HttpError::not_found());
    }
    // Members on other servers, by the share with them: a key for one
    // goes with their public key known here.
    let remote: Vec<Uuid> = crate::federation::e2e::remote_members(&state.files, root)
        .await?
        .into_iter()
        .filter(|m| m.public_key.is_some())
        .map(|m| m.share_id)
        .collect();
    // The members who set up encryption: those a key can be for.
    let mut with_keys = remote.clone();
    for member in &members {
        let set_up: Option<i64> = sqlx::query_scalar("SELECT 1 FROM e2e_keys WHERE user_id = ?1")
            .bind(member)
            .fetch_optional(state.db.reader())
            .await?;
        if set_up.is_some() {
            with_keys.push(*member);
        }
    }
    for key in &body.keys {
        check_b64(&key.wrapped, 93, 93, true, "wrapped")?;
        if !with_keys.contains(&key.user_id) {
            return Err(HttpError::bad_request(
                "A key for someone who is not a member, or has not set up encryption",
            ));
        }
    }
    let mut tx = state.db.writer().begin().await?;
    // None for a mirror's copy: its keys are the drive's folder's, kept.
    let (current,): (i64,) =
        sqlx::query_as("SELECT generation FROM e2e_folders WHERE root_id = ?1")
            .bind(root)
            .fetch_optional(&mut *tx)
            .await?
            .ok_or_else(HttpError::not_found)?;
    let newest = body.keys.iter().map(|k| k.generation).max().unwrap_or(0);
    if body.keys.iter().any(|k| k.generation < 1) || newest > current + 1 {
        return Err(HttpError::bad_request(
            "A generation must exist already, or be the next",
        ));
    }
    if newest == current + 1 {
        // A new generation: for everyone who can have it, at once.
        let covered = with_keys.iter().all(|member| {
            body.keys
                .iter()
                .any(|k| k.generation == newest && k.user_id == *member)
        });
        if !covered || !with_keys.contains(&user.0.id) {
            return Err(HttpError::bad_request(
                "A new key must be wrapped for every member who set up encryption, you included",
            ));
        }
        let moved = sqlx::query(
            "UPDATE e2e_folders SET generation = ?2, needs_new_key = 0
             WHERE root_id = ?1 AND generation = ?3",
        )
        .bind(root)
        .bind(newest)
        .bind(current)
        .execute(&mut *tx)
        .await?;
        if moved.rows_affected() == 0 {
            return Err(HttpError::conflict(
                "Another device made a new key first: read the keys again",
            ));
        }
    }
    for key in &body.keys {
        let table = if remote.contains(&key.user_id) {
            "INSERT INTO e2e_remote_keys (root_id, generation, share_id, wrapped, wrapped_by)
             VALUES (?1, ?2, ?3, ?4, ?5) ON CONFLICT DO NOTHING"
        } else {
            "INSERT INTO e2e_folder_keys (root_id, generation, user_id, wrapped, wrapped_by)
             VALUES (?1, ?2, ?3, ?4, ?5) ON CONFLICT DO NOTHING"
        };
        sqlx::query(table)
            .bind(root)
            .bind(key.generation)
            .bind(key.user_id)
            .bind(&key.wrapped)
            .bind(user.0.id)
            .execute(&mut *tx)
            .await?;
    }
    tx.commit().await?;
    // A new generation, or one given to someone who mirrors the drive:
    // their mirror keeps it at once, not only at its next copy.
    e2e::keep_for_mirrors(&state.db, None, Some(root)).await?;
    Ok(Json(folder_keys(&state, user.0.id, root).await?))
}

/// An encrypted folder whose keys a member's device should see to.
#[derive(Serialize, ToSchema)]
pub struct EncryptionTask {
    pub folder_id: Uuid,
    /// No key yet, or a member lost access: make a new generation.
    pub needs_new_key: bool,
    /// Members with a public key who lack a generation: wrap it for them.
    pub waiting: Vec<Uuid>,
}

#[utoipa::path(
    get,
    path = "/pending",
    tag = "Encryption",
    operation_id = "listEncryptionTasks",
    summary = "Encrypted folders whose keys need a device",
    description = "The encrypted folders the user is a member of that need a new key, or have members waiting for one, which the user's unlocked device can wrap: those it holds the newest key of, and (for a new key) any it can reach.",
    responses(
        (status = 200, description = "What to do", body = Vec<EncryptionTask>),
        (status = 401, description = "Not signed in", body = ErrorBody),
    ),
    security(("session" = []), ("api_key" = [])),
)]
async fn list_encryption_tasks(
    State(state): State<AppState>,
    user: AuthUser,
) -> Result<Json<Vec<EncryptionTask>>, HttpError> {
    if keys_of(&state, user.0.id).await?.is_none() {
        return Ok(Json(Vec::new()));
    }
    // The encrypted folders of the user's drives, and those they hold a
    // key of.
    let roots: Vec<Uuid> = sqlx::query_scalar(
        "SELECT f.root_id FROM e2e_folders f JOIN nodes n ON n.id = f.root_id
         WHERE n.drive_id IN (SELECT drive_id FROM drive_users WHERE user_id = ?1)
         UNION
         SELECT root_id FROM e2e_folder_keys WHERE user_id = ?1
           AND root_id IN (SELECT id FROM nodes)",
    )
    .bind(user.0.id)
    .fetch_all(state.db.reader())
    .await?;
    let mut tasks = Vec::new();
    for root in roots {
        if state.files.access(user.0.id, root).await.is_err() {
            continue;
        }
        let keys = folder_keys(&state, user.0.id, root).await?;
        let holds_newest = keys.keys.iter().any(|k| k.generation == keys.generation);
        let waiting: Vec<Uuid> = if holds_newest {
            keys.members
                .iter()
                .filter(|m| !m.missing.is_empty())
                .map(|m| m.user_id)
                .collect()
        } else {
            Vec::new()
        };
        if keys.needs_new_key || !waiting.is_empty() {
            tasks.push(EncryptionTask {
                folder_id: root,
                needs_new_key: keys.needs_new_key,
                waiting,
            });
        }
    }
    Ok(Json(tasks))
}
