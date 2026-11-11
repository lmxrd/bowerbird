//! The drop box of each user: what is dropped into it and its items
//! (`drop_box/`), its settings and its sensitive-file rules
//! (docs/M2_DESIGN.md §3, §6).

use crate::ai::Recipient;
use crate::auth::now_iso;
use crate::drop_box::review::{self, Choice, ItemView};
use crate::drop_box::{self as drop_box, ITEM_COLUMNS, InboxItem, Status};
use crate::error::{ErrorBody, HttpError};
use crate::extract::ApiJson;
use crate::routes::auth::AuthUser;
use crate::state::AppState;
use axum::Json;
use axum::extract::{Multipart, Path, Query, State};
use axum::http::StatusCode;
use serde::{Deserialize, Serialize};
use utoipa::{IntoParams, ToSchema};
use utoipa_axum::router::OpenApiRouter;
use utoipa_axum::routes;
use uuid::Uuid;

/// The most sensitive-file rules a user may have.
pub const MAX_RULES: i64 = 200;
/// The longest pattern of a rule, and of a top-level folder's name.
const MAX_PATTERN: usize = 200;
/// The most top-level folders the drop box may be limited to.
const MAX_TOP_FOLDERS: usize = 100;
/// The longest a pending item waits, in hours, before it is accepted
/// (pending mode `timeout`): 30 days.
pub const MAX_ACCEPT_AFTER_HOURS: i64 = 720;

/// The drop box routes, nested under /drop-box.
pub fn router() -> OpenApiRouter<AppState> {
    OpenApiRouter::new()
        .routes(routes!(get_settings, update_settings))
        .routes(routes!(list_rules, create_rule))
        .routes(routes!(delete_rule))
        .routes(routes!(add_files))
        .routes(routes!(add_link))
        .routes(routes!(list_items))
        .routes(routes!(get_item))
        .routes(routes!(confirm_item))
        .routes(routes!(reject_item))
        .routes(routes!(place_item))
        .routes(routes!(retry_item))
}

/// The placements, nested under /placements.
pub fn placements_router() -> OpenApiRouter<AppState> {
    OpenApiRouter::new().routes(routes!(undo_placement))
}

/// What happens to an item once the model proposed a place for it.
#[derive(Clone, Copy, Debug, PartialEq, Serialize, Deserialize, ToSchema, sqlx::Type)]
#[serde(rename_all = "snake_case")]
#[sqlx(rename_all = "snake_case")]
pub enum PendingMode {
    /// Moved there at once, marked pending until it is confirmed.
    MovePending,
    /// Left in the drop box until it is confirmed.
    Stay,
    /// Moved at once, and accepted after `accept_after_hours`.
    Timeout,
    /// Accepted at once from `confidence_threshold`, else left to confirm.
    Confidence,
}

/// When the contents of a file (or the page of a link) are read and sent.
#[derive(Clone, Copy, Debug, PartialEq, Serialize, Deserialize, ToSchema, sqlx::Type)]
#[serde(rename_all = "snake_case")]
#[sqlx(rename_all = "snake_case")]
pub enum ReadContents {
    /// For every item.
    Always,
    /// Chosen per item, off unless asked for.
    PerItem,
    /// Per item, on unless turned off.
    DefaultOn,
}

/// How readily the model makes new folders.
#[derive(Clone, Copy, Debug, PartialEq, Serialize, Deserialize, ToSchema, sqlx::Type)]
#[serde(rename_all = "snake_case")]
#[sqlx(rename_all = "snake_case")]
pub enum FolderFreedom {
    /// Prefers the existing folders.
    Consistent,
    Balanced,
    /// Makes more specific folders.
    Refined,
}

/// What a dropped folder becomes.
#[derive(Clone, Copy, Debug, PartialEq, Serialize, Deserialize, ToSchema, sqlx::Type)]
#[serde(rename_all = "snake_case")]
#[sqlx(rename_all = "snake_case")]
pub enum DroppedFolders {
    /// Placed as one item.
    Whole,
    /// Each of its files placed on its own.
    Split,
}

/// What a shared link becomes.
#[derive(Clone, Copy, Debug, PartialEq, Serialize, Deserialize, ToSchema, sqlx::Type)]
#[serde(rename_all = "snake_case")]
#[sqlx(rename_all = "snake_case")]
pub enum Links {
    /// A bookmark file.
    Bookmark,
    /// A bookmark and a snapshot of the page.
    Snapshot,
    /// The file the link points to.
    Download,
}

/// A user's drop box settings, as stored (no row: the defaults).
#[derive(Clone, Debug, PartialEq, sqlx::FromRow)]
pub struct Stored {
    pub pending_mode: PendingMode,
    pub accept_after_hours: Option<i64>,
    pub confidence_threshold: Option<f64>,
    pub read_contents: ReadContents,
    pub folder_freedom: FolderFreedom,
    /// A JSON array; `None` for any.
    pub allowed_top_folders: Option<String>,
    pub folders: DroppedFolders,
    pub links: Links,
}

impl Default for Stored {
    fn default() -> Self {
        Stored {
            pending_mode: PendingMode::MovePending,
            accept_after_hours: None,
            confidence_threshold: None,
            read_contents: ReadContents::Always,
            folder_freedom: FolderFreedom::Balanced,
            allowed_top_folders: None,
            folders: DroppedFolders::Whole,
            links: Links::Snapshot,
        }
    }
}

/// The settings of the drop box (docs/M7B_DESIGN.md §4), by field.
const KEYS: [(&str, &str); 8] = [
    ("pending_mode", "drop_box.pending_mode"),
    ("accept_after_hours", "drop_box.accept_after_hours"),
    ("confidence_threshold", "drop_box.confidence_threshold"),
    ("read_contents", "drop_box.read_contents"),
    ("folder_freedom", "drop_box.folder_freedom"),
    ("allowed_top_folders", "drop_box.allowed_top_folders"),
    ("folders", "drop_box.folders"),
    ("links", "drop_box.links"),
];

/// A user's settings, as the levels resolve them (theirs, their teams',
/// the superadmin's, the defaults).
pub async fn settings_of(state: &AppState, user: Uuid) -> Result<Stored, sqlx::Error> {
    let env = crate::settings::Env::of_state(state);
    let mut values = serde_json::Map::new();
    for (field, key) in KEYS {
        let resolved =
            crate::settings::resolve(&state.db, &env, key, crate::settings::For::user(user))
                .await?;
        values.insert(field.into(), resolved.value);
    }
    let settings: DropBoxSettings =
        serde_json::from_value(values.into()).map_err(|e| sqlx::Error::Decode(e.into()))?;
    let pending = settings.pending_mode;
    Ok(Stored {
        pending_mode: pending,
        accept_after_hours: settings
            .accept_after_hours
            .filter(|_| pending == PendingMode::Timeout),
        confidence_threshold: settings
            .confidence_threshold
            .filter(|_| pending == PendingMode::Confidence),
        read_contents: settings.read_contents,
        folder_freedom: settings.folder_freedom,
        allowed_top_folders: settings
            .allowed_top_folders
            .map(|names| serde_json::to_string(&names).expect("names serialize")),
        folders: settings.folders,
        links: settings.links,
    })
}

/// The drop box settings of the signed-in user.
#[derive(Serialize, Deserialize, ToSchema)]
pub struct DropBoxSettings {
    pub pending_mode: PendingMode,
    /// With `timeout`: hours until a pending item is accepted (1 to 720;
    /// default 24).
    pub accept_after_hours: Option<i64>,
    /// With `confidence`: the model's confidence (above 0, up to 1) from
    /// which an item is accepted at once (default 0.8).
    pub confidence_threshold: Option<f64>,
    pub read_contents: ReadContents,
    pub folder_freedom: FolderFreedom,
    /// The top-level folders the drop box may place items in; absent (or
    /// null) for any.
    pub allowed_top_folders: Option<Vec<String>>,
    pub folders: DroppedFolders,
    pub links: Links,
}

/// The settings with what the settings page shows next to them.
#[derive(Serialize, ToSchema)]
pub struct DropBoxSettingsView {
    #[serde(flatten)]
    pub settings: DropBoxSettings,
    /// Where the AI requests go, for the description of what is sent
    /// (`enabled` false: the AI connection is not set up, and the drop box
    /// cannot place anything).
    pub recipient: Recipient,
}

impl From<Stored> for DropBoxSettings {
    fn from(s: Stored) -> Self {
        DropBoxSettings {
            pending_mode: s.pending_mode,
            accept_after_hours: s.accept_after_hours,
            confidence_threshold: s.confidence_threshold,
            read_contents: s.read_contents,
            folder_freedom: s.folder_freedom,
            allowed_top_folders: s
                .allowed_top_folders
                .and_then(|json| serde_json::from_str(&json).ok()),
            folders: s.folders,
            links: s.links,
        }
    }
}

/// The settings as they are stored, once they are checked; the hours and
/// the threshold are kept only with the mode that uses them, with their
/// defaults when left out.
fn checked(s: DropBoxSettings) -> Result<Stored, HttpError> {
    let accept_after_hours = match s.pending_mode {
        PendingMode::Timeout => {
            let hours = s.accept_after_hours.unwrap_or(24);
            if !(1..=MAX_ACCEPT_AFTER_HOURS).contains(&hours) {
                return Err(HttpError::bad_request(format!(
                    "accept_after_hours must be from 1 to {MAX_ACCEPT_AFTER_HOURS}"
                )));
            }
            Some(hours)
        }
        _ => None,
    };
    let confidence_threshold = match s.pending_mode {
        PendingMode::Confidence => {
            let threshold = s.confidence_threshold.unwrap_or(0.8);
            if !(threshold > 0.0 && threshold <= 1.0) {
                return Err(HttpError::bad_request(
                    "confidence_threshold must be above 0 and at most 1",
                ));
            }
            Some(threshold)
        }
        _ => None,
    };
    let allowed_top_folders = match s.allowed_top_folders {
        None => None,
        Some(names) => {
            let mut clean: Vec<String> = Vec::new();
            for name in names {
                let name = name.trim().to_string();
                if name.is_empty() || name.len() > MAX_PATTERN || name.contains('/') {
                    return Err(HttpError::bad_request(
                        "allowed_top_folders must be folder names (not empty, no /)",
                    ));
                }
                if !clean.contains(&name) {
                    clean.push(name);
                }
            }
            if clean.is_empty() || clean.len() > MAX_TOP_FOLDERS {
                return Err(HttpError::bad_request(format!(
                    "allowed_top_folders must name 1 to {MAX_TOP_FOLDERS} folders, or be null for any"
                )));
            }
            Some(serde_json::to_string(&clean).expect("names serialize"))
        }
    };
    Ok(Stored {
        pending_mode: s.pending_mode,
        accept_after_hours,
        confidence_threshold,
        read_contents: s.read_contents,
        folder_freedom: s.folder_freedom,
        allowed_top_folders,
        folders: s.folders,
        links: s.links,
    })
}

#[utoipa::path(
    get,
    path = "/settings",
    tag = "Drop box",
    operation_id = "getDropBoxSettings",
    summary = "The drop box settings",
    responses(
        (status = 200, description = "The settings, and where the AI requests go", body = DropBoxSettingsView),
        (status = 401, description = "Not signed in", body = ErrorBody),
    ),
    security(("session" = []), ("api_key" = [])),
)]
async fn get_settings(
    State(state): State<AppState>,
    AuthUser(user, _): AuthUser,
) -> Result<Json<DropBoxSettingsView>, HttpError> {
    Ok(Json(DropBoxSettingsView {
        settings: settings_of(&state, user.id).await?.into(),
        recipient: state.ai.recipient().await,
    }))
}

#[utoipa::path(
    put,
    path = "/settings",
    tag = "Drop box",
    operation_id = "updateDropBoxSettings",
    summary = "Change the drop box settings",
    description = "Replaces them all. `accept_after_hours` is kept with the pending mode `timeout` only (default 24), `confidence_threshold` with `confidence` only (default 0.8).",
    request_body(content = DropBoxSettings, content_type = "application/json"),
    responses(
        (status = 200, description = "The settings now in effect", body = DropBoxSettingsView),
        (status = 400, description = "The settings are invalid", body = ErrorBody),
        (status = 401, description = "Not signed in", body = ErrorBody),
        (status = 403, description = "A read-only API key, or a setting a level above locked (`GET /settings` says which)", body = ErrorBody),
        (status = 415, description = "The body must be JSON (Content-Type: application/json)", body = ErrorBody),
    ),
    security(("session" = []), ("api_key" = [])),
)]
async fn update_settings(
    State(state): State<AppState>,
    AuthUser(user, _): AuthUser,
    ApiJson(body): ApiJson<DropBoxSettings>,
) -> Result<Json<DropBoxSettingsView>, HttpError> {
    let s = checked(body)?;
    let now = serde_json::to_value(DropBoxSettings::from(settings_of(&state, user.id).await?))
        .expect("settings serialize");
    let wanted = serde_json::to_value(DropBoxSettings::from(s)).expect("settings serialize");
    // Each setting that changed is the user's own now; one a level above
    // locked is refused (the hours and the threshold only with their
    // mode).
    for (field, key) in KEYS {
        let value = wanted[field].clone();
        if value.is_null() && matches!(field, "accept_after_hours" | "confidence_threshold") {
            continue;
        }
        if value != now[field] {
            crate::settings::set_own(&state, user.id, key, value).await?;
        }
    }
    Ok(Json(DropBoxSettingsView {
        settings: settings_of(&state, user.id).await?.into(),
        recipient: state.ai.recipient().await,
    }))
}

/// What a sensitive-file rule looks at.
#[derive(Clone, Copy, Debug, PartialEq, Serialize, Deserialize, ToSchema, sqlx::Type)]
#[serde(rename_all = "snake_case")]
#[sqlx(rename_all = "snake_case")]
pub enum RuleKind {
    /// A glob on the item's path inside a dropped folder, e.g. `*/tax/*`:
    /// `*` any characters (`/` included), `?` one, `[abc]` one of these,
    /// `{a,b}` either, `\` before a character makes it plain.
    Path,
    /// A glob on the item's name, e.g. `*.kdbx` or `passport*`.
    Name,
    /// A word in the item's name, its note, or the text read from it
    /// (read here, before anything is sent).
    Keyword,
}

/// What happens to an item that matches a rule.
#[derive(Clone, Copy, Debug, PartialEq, Serialize, Deserialize, ToSchema, sqlx::Type)]
#[serde(rename_all = "snake_case")]
#[sqlx(rename_all = "snake_case")]
pub enum RuleAction {
    /// Placed by its name only; its contents are never read or sent.
    NameOnly,
    /// Nothing is sent: it stays in the drop box, to be placed by hand.
    ByHand,
}

/// A sensitive-file rule. Patterns and keywords ignore case.
#[derive(Serialize, ToSchema, sqlx::FromRow)]
pub struct GuardRule {
    pub id: Uuid,
    pub kind: RuleKind,
    pub pattern: String,
    pub action: RuleAction,
    pub created_at: String,
}

/// A new sensitive-file rule.
#[derive(Deserialize, ToSchema)]
pub struct NewGuardRule {
    pub kind: RuleKind,
    pub pattern: String,
    pub action: RuleAction,
}

/// A user's rules, the oldest first.
pub async fn rules_of(state: &AppState, user: Uuid) -> Result<Vec<GuardRule>, sqlx::Error> {
    sqlx::query_as(
        "SELECT id, kind, pattern, action, created_at FROM guard_rules
         WHERE user_id = ?1 ORDER BY created_at, id",
    )
    .bind(user)
    .fetch_all(state.db.reader())
    .await
}

#[utoipa::path(
    get,
    path = "/guard-rules",
    tag = "Drop box",
    operation_id = "listGuardRules",
    summary = "The sensitive-file rules",
    responses(
        (status = 200, description = "The rules, the oldest first", body = Vec<GuardRule>),
        (status = 401, description = "Not signed in", body = ErrorBody),
    ),
    security(("session" = []), ("api_key" = [])),
)]
async fn list_rules(
    State(state): State<AppState>,
    AuthUser(user, _): AuthUser,
) -> Result<Json<Vec<GuardRule>>, HttpError> {
    Ok(Json(rules_of(&state, user.id).await?))
}

#[utoipa::path(
    post,
    path = "/guard-rules",
    tag = "Drop box",
    operation_id = "createGuardRule",
    summary = "Add a sensitive-file rule",
    description = "At most 200 rules; a pattern is 1 to 200 characters. A name or path pattern is a glob, case ignored: `*` any characters (`/` included), `?` one, `[abc]` one of these, `{a,b}` either, and `\\` before a character makes it plain (`\\*` is a star). One that is not valid is refused (400).",
    request_body(content = NewGuardRule, content_type = "application/json"),
    responses(
        (status = 201, description = "The rule", body = GuardRule),
        (status = 400, description = "The rule is invalid, or there are too many", body = ErrorBody),
        (status = 401, description = "Not signed in", body = ErrorBody),
        (status = 403, description = "A read-only API key", body = ErrorBody),
        (status = 415, description = "The body must be JSON (Content-Type: application/json)", body = ErrorBody),
    ),
    security(("session" = []), ("api_key" = [])),
)]
async fn create_rule(
    State(state): State<AppState>,
    AuthUser(user, _): AuthUser,
    ApiJson(body): ApiJson<NewGuardRule>,
) -> Result<(StatusCode, Json<GuardRule>), HttpError> {
    let pattern = body.pattern.trim().to_string();
    if pattern.is_empty() || pattern.chars().count() > MAX_PATTERN {
        return Err(HttpError::bad_request(
            "The pattern must be 1 to 200 characters",
        ));
    }
    if body.kind != RuleKind::Keyword {
        crate::drop_box::guard::compile(&pattern)
            .map_err(|e| HttpError::bad_request(format!("The pattern is not valid: {e}")))?;
    }
    let rule = GuardRule {
        id: Uuid::now_v7(),
        kind: body.kind,
        pattern,
        action: body.action,
        created_at: now_iso(),
    };
    // Counted and added in one statement: two requests at once cannot both
    // pass the limit.
    let added = sqlx::query(
        "INSERT INTO guard_rules (id, user_id, kind, pattern, action, created_at)
         SELECT ?1, ?2, ?3, ?4, ?5, ?6
         WHERE (SELECT COUNT(*) FROM guard_rules WHERE user_id = ?2) < ?7",
    )
    .bind(rule.id)
    .bind(user.id)
    .bind(rule.kind)
    .bind(&rule.pattern)
    .bind(rule.action)
    .bind(&rule.created_at)
    .bind(MAX_RULES)
    .execute(state.db.writer())
    .await?;
    if added.rows_affected() == 0 {
        return Err(HttpError::bad_request(format!(
            "There can be at most {MAX_RULES} rules"
        )));
    }
    Ok((StatusCode::CREATED, Json(rule)))
}

#[utoipa::path(
    delete,
    path = "/guard-rules/{id}",
    tag = "Drop box",
    operation_id = "deleteGuardRule",
    summary = "Remove a sensitive-file rule",
    params(("id" = Uuid, Path, description = "The rule")),
    responses(
        (status = 204, description = "Removed"),
        (status = 401, description = "Not signed in", body = ErrorBody),
        (status = 403, description = "A read-only API key", body = ErrorBody),
        (status = 404, description = "No such rule of this user", body = ErrorBody),
    ),
    security(("session" = []), ("api_key" = [])),
)]
async fn delete_rule(
    State(state): State<AppState>,
    AuthUser(user, _): AuthUser,
    Path(id): Path<Uuid>,
) -> Result<StatusCode, HttpError> {
    let deleted = sqlx::query("DELETE FROM guard_rules WHERE id = ?1 AND user_id = ?2")
        .bind(id)
        .bind(user.id)
        .execute(state.db.writer())
        .await?;
    if deleted.rows_affected() == 0 {
        return Err(HttpError::not_found());
    }
    Ok(StatusCode::NO_CONTENT)
}

/// The most files one upload to the drop box may hold.
const MAX_FILES: usize = 1_000;

/// Files for the drop box, as a multipart form: one or more parts `file`
/// (each named by its filename; a file of a dropped folder by its path in
/// it, `Trip/2025/ticket.pdf`), and optionally a `note`, `read_contents`
/// (`true` or `false`) and `folders` (`whole` or `split`) for all of them,
/// before the files.
#[derive(ToSchema)]
#[allow(dead_code)] // Documents the multipart form; read by `Multipart`.
pub struct DropForm {
    #[schema(format = Binary, content_media_type = "application/octet-stream")]
    file: Vec<String>,
    /// What to tell the model about them, at most 2,000 characters.
    note: Option<String>,
    /// Whether their contents may be read, where the settings leave it to
    /// each item (`per_item`, `default_on`).
    read_contents: Option<bool>,
    /// A dropped folder kept as one item, or split into its files; absent
    /// for the user's setting.
    folders: Option<DroppedFolders>,
}

#[utoipa::path(
    post,
    path = "/files",
    tag = "Drop box",
    operation_id = "dropFiles",
    summary = "Drop files",
    description = "Each file becomes an item, staged in the drop box and analysed in the background (the `drop-box` live event tells when it changes). Files named by a path (`Trip/2025/ticket.pdf`) are a dropped folder: one item for the folder, or one per file, by `folders` or the user's setting. At most 1,000 files of 100 MB each: a larger file goes through the resumable uploads (`POST /uploads?drop_box=true&name=...`).",
    request_body(content = DropForm, content_type = "multipart/form-data"),
    responses(
        (status = 201, description = "The new items", body = Vec<InboxItem>),
        (status = 400, description = "No file part, an invalid name, or a note too long", body = ErrorBody),
        (status = 401, description = "Not signed in", body = ErrorBody),
        (status = 403, description = "A read-only API key", body = ErrorBody),
        (status = 413, description = "A file over 100 MB", body = ErrorBody),
    ),
    security(("session" = []), ("api_key" = [])),
)]
async fn add_files(
    State(state): State<AppState>,
    AuthUser(user, _): AuthUser,
    mut form: Multipart,
) -> Result<(StatusCode, Json<Vec<InboxItem>>), HttpError> {
    let invalid = || HttpError::bad_request("Expected a multipart form with the files in `file`");
    let mut note = None;
    let mut read_contents = None;
    let mut folders = None;
    // Received first, all of them, then made items: a form that breaks off
    // half way adds nothing.
    let mut received: Vec<(String, std::path::PathBuf)> = Vec::new();
    let result: Result<(), HttpError> = async {
        while let Some(mut field) = form.next_field().await.map_err(|_| invalid())? {
            match field.name() {
                Some("note") => note = Some(field.text().await.map_err(|_| invalid())?),
                Some("folders") => {
                    let value = field.text().await.map_err(|_| invalid())?;
                    folders = Some(match value.trim() {
                        "whole" => DroppedFolders::Whole,
                        "split" => DroppedFolders::Split,
                        _ => {
                            return Err(HttpError::bad_request("folders must be whole or split"));
                        }
                    });
                }
                Some("read_contents") => {
                    let value = field.text().await.map_err(|_| invalid())?;
                    read_contents = crate::config::parse_flag("read_contents", &value)
                        .map_err(HttpError::bad_request)?;
                }
                Some("file") => {
                    if received.len() == MAX_FILES {
                        return Err(HttpError::bad_request(format!(
                            "At most {MAX_FILES} files at once"
                        )));
                    }
                    let name = field.file_name().map(String::from).ok_or_else(invalid)?;
                    let temp = crate::routes::files::temp_upload(&state)?;
                    // Pushed before it is written: removed with the rest on
                    // an error.
                    received.push((name, temp.clone()));
                    crate::routes::files::write_field(&mut field, &temp).await?;
                }
                _ => {}
            }
        }
        if received.is_empty() {
            return Err(invalid());
        }
        drop_box::clean_note(note.clone())?;
        Ok(())
    }
    .await;
    if let Err(e) = result {
        for (_, temp) in &received {
            let _ = std::fs::remove_file(temp);
        }
        return Err(e);
    }
    let temps: Vec<std::path::PathBuf> = received.iter().map(|(_, t)| t.clone()).collect();
    let made = make_items(&state, user.id, received, note, read_contents, folders).await;
    // What was not made into an item goes (a moved file is already gone).
    for temp in temps {
        let _ = std::fs::remove_file(temp);
    }
    Ok((StatusCode::CREATED, Json(made?)))
}

/// A file of a dropped folder: its path in the folder, and where it was
/// received.
type FolderFile = (Vec<String>, std::path::PathBuf);

/// The received files as items: each loose file one; a dropped folder (its
/// files named by their path in it) one item kept whole, or one per file.
async fn make_items(
    state: &AppState,
    user: Uuid,
    received: Vec<(String, std::path::PathBuf)>,
    note: Option<String>,
    read_contents: Option<bool>,
    folders: Option<DroppedFolders>,
) -> Result<Vec<InboxItem>, HttpError> {
    let mode = match folders {
        Some(mode) => mode,
        None => settings_of(state, user).await?.folders,
    };
    let mut loose = Vec::new();
    let mut grouped: Vec<(String, Vec<FolderFile>)> = Vec::new();
    for (name, temp) in received {
        let parts: Vec<String> = name
            .split(['/', '\\'])
            .filter(|p| !p.is_empty())
            .map(String::from)
            .collect();
        match parts.as_slice() {
            [] => return Err(HttpError::bad_request("A file without a name")),
            // A client's own files (Finder's `.DS_Store`, `._*`) go.
            [.., last] if crate::dav_inbox::ignored(last) => {}
            [single] => loose.push((single.clone(), temp)),
            [top, rest @ ..] => match grouped.iter_mut().find(|(t, _)| t == top) {
                Some((_, files)) => files.push((rest.to_vec(), temp)),
                None => grouped.push((top.clone(), vec![(rest.to_vec(), temp)])),
            },
        }
    }
    let mut items = Vec::new();
    for (name, temp) in loose {
        items.push(
            drop_box::add_file(state, user, &name, &temp, note.clone(), read_contents).await?,
        );
    }
    for (top, files) in grouped {
        match mode {
            DroppedFolders::Whole => items.push(
                drop_box::add_folder(state, user, &top, files, note.clone(), read_contents).await?,
            ),
            DroppedFolders::Split => {
                for (parts, temp) in files {
                    let name = parts.last().cloned().unwrap_or_default();
                    let path = format!("{top}/{}", parts.join("/"));
                    items.push(
                        drop_box::add_file_from(
                            state,
                            user,
                            &name,
                            &temp,
                            drop_box::Origin {
                                note: note.clone(),
                                read_contents,
                                source_path: Some(path),
                                ..drop_box::Origin::default()
                            },
                        )
                        .await?,
                    );
                }
            }
        }
    }
    Ok(items)
}

/// A link for the drop box.
#[derive(Deserialize, ToSchema)]
pub struct DropLink {
    /// An http:// or https:// URL, at most 2,048 characters.
    pub url: String,
    /// What to tell the model about it, at most 2,000 characters.
    pub note: Option<String>,
    /// What it becomes; absent for the user's setting.
    pub mode: Option<Links>,
    /// Whether the page may be read, where the settings leave it to each
    /// item.
    pub read_contents: Option<bool>,
}

#[utoipa::path(
    post,
    path = "/links",
    tag = "Drop box",
    operation_id = "dropLink",
    summary = "Drop a link",
    request_body(content = DropLink, content_type = "application/json"),
    responses(
        (status = 201, description = "The new item", body = InboxItem),
        (status = 400, description = "Not an http(s) URL, or too long", body = ErrorBody),
        (status = 401, description = "Not signed in", body = ErrorBody),
        (status = 403, description = "A read-only API key", body = ErrorBody),
        (status = 415, description = "The body must be JSON (Content-Type: application/json)", body = ErrorBody),
    ),
    security(("session" = []), ("api_key" = [])),
)]
async fn add_link(
    State(state): State<AppState>,
    AuthUser(user, _): AuthUser,
    ApiJson(body): ApiJson<DropLink>,
) -> Result<(StatusCode, Json<InboxItem>), HttpError> {
    let item = drop_box::add_link(
        &state,
        user.id,
        &body.url,
        body.note,
        body.mode,
        body.read_contents,
    )
    .await?;
    Ok((StatusCode::CREATED, Json(item)))
}

/// Which items to list.
#[derive(Deserialize, IntoParams)]
#[into_params(parameter_in = Query)]
pub struct ItemsParams {
    /// Only the items in this state.
    pub status: Option<Status>,
    /// Only the items older than this one: the next page.
    pub before: Option<Uuid>,
    /// At most this many (default 100, at most 500).
    pub limit: Option<i64>,
}

#[utoipa::path(
    get,
    path = "/items",
    tag = "Drop box",
    operation_id = "listDropBoxItems",
    summary = "The drop box's items",
    description = "The newest first. For the next page, `before` the last one.",
    params(ItemsParams),
    responses(
        (status = 200, description = "The items, with their proposals and placements", body = Vec<ItemView>),
        (status = 400, description = "An invalid parameter", body = ErrorBody),
        (status = 401, description = "Not signed in", body = ErrorBody),
    ),
    security(("session" = []), ("api_key" = [])),
)]
async fn list_items(
    State(state): State<AppState>,
    AuthUser(user, _): AuthUser,
    Query(params): Query<ItemsParams>,
) -> Result<Json<Vec<ItemView>>, HttpError> {
    let limit = params.limit.unwrap_or(100).clamp(1, 500);
    // Ids are v7: their order is their creation's.
    let items: Vec<InboxItem> = sqlx::query_as(sqlx::AssertSqlSafe(format!(
        "SELECT {ITEM_COLUMNS} FROM inbox_items
         WHERE user_id = ?1 AND (?2 IS NULL OR status = ?2) AND (?3 IS NULL OR id < ?3)
         ORDER BY id DESC LIMIT ?4"
    )))
    .bind(user.id)
    .bind(params.status)
    .bind(params.before)
    .bind(limit)
    .fetch_all(state.db.reader())
    .await?;
    let mut views = Vec::with_capacity(items.len());
    for item in items {
        views.push(review::view(&state, item).await?);
    }
    Ok(Json(views))
}

#[utoipa::path(
    get,
    path = "/items/{id}",
    tag = "Drop box",
    operation_id = "getDropBoxItem",
    summary = "An item of the drop box",
    params(("id" = Uuid, Path, description = "The item")),
    responses(
        (status = 200, description = "The item, with its proposal and placement", body = ItemView),
        (status = 401, description = "Not signed in", body = ErrorBody),
        (status = 404, description = "No such item of this user", body = ErrorBody),
    ),
    security(("session" = []), ("api_key" = [])),
)]
async fn get_item(
    State(state): State<AppState>,
    AuthUser(user, _): AuthUser,
    Path(id): Path<Uuid>,
) -> Result<Json<ItemView>, HttpError> {
    Ok(Json(review::item_view(&state, user.id, id).await?))
}

#[utoipa::path(
    post,
    path = "/items/{id}/confirm",
    tag = "Drop box",
    operation_id = "confirmDropBoxItem",
    summary = "Confirm or change a proposal",
    description = "Without a body, the proposal is accepted (and the item placed by it, if it was not yet). With a place (`folder_id`, `new_folders`, `name`), the item goes there instead, and the choice is kept as an example for the next proposals (status `changed`).",
    params(("id" = Uuid, Path, description = "The item")),
    request_body(content = Option<Choice>, content_type = "application/json"),
    responses(
        (status = 200, description = "The item", body = ItemView),
        (status = 400, description = "An invalid place", body = ErrorBody),
        (status = 401, description = "Not signed in", body = ErrorBody),
        (status = 403, description = "A read-only API key", body = ErrorBody),
        (status = 404, description = "No such item, or no such folder", body = ErrorBody),
        (status = 409, description = "The item is not proposed", body = ErrorBody),
        (status = 415, description = "A body that is not JSON", body = ErrorBody),
    ),
    security(("session" = []), ("api_key" = [])),
)]
async fn confirm_item(
    State(state): State<AppState>,
    AuthUser(user, _): AuthUser,
    Path(id): Path<Uuid>,
    headers: axum::http::HeaderMap,
    body: axum::body::Bytes,
) -> Result<Json<ItemView>, HttpError> {
    // No body: the proposal as it is. A body: JSON, as everywhere.
    let choice = if body.iter().all(u8::is_ascii_whitespace) {
        None
    } else {
        let json = headers
            .get(axum::http::header::CONTENT_TYPE)
            .and_then(|v| v.to_str().ok())
            .is_some_and(|v| v.starts_with("application/json"));
        if !json {
            return Err(HttpError::new(
                StatusCode::UNSUPPORTED_MEDIA_TYPE,
                "The body must be JSON (Content-Type: application/json)",
            ));
        }
        // `null` is no place too.
        serde_json::from_slice::<Option<Choice>>(&body)
            .map_err(|e| HttpError::bad_request(format!("Invalid JSON: {e}")))?
    };
    Ok(Json(review::confirm(&state, user.id, id, choice).await?))
}

#[utoipa::path(
    post,
    path = "/items/{id}/reject",
    tag = "Drop box",
    operation_id = "rejectDropBoxItem",
    summary = "Reject a proposal",
    description = "The item goes back to the drop box (taken out of the tree if it was placed), to place by hand.",
    params(("id" = Uuid, Path, description = "The item")),
    responses(
        (status = 200, description = "The item", body = ItemView),
        (status = 401, description = "Not signed in", body = ErrorBody),
        (status = 403, description = "A read-only API key", body = ErrorBody),
        (status = 404, description = "No such item", body = ErrorBody),
        (status = 409, description = "The item is not proposed", body = ErrorBody),
    ),
    security(("session" = []), ("api_key" = [])),
)]
async fn reject_item(
    State(state): State<AppState>,
    AuthUser(user, _): AuthUser,
    Path(id): Path<Uuid>,
) -> Result<Json<ItemView>, HttpError> {
    Ok(Json(review::reject(&state, user.id, id).await?))
}

#[utoipa::path(
    post,
    path = "/items/{id}/place",
    tag = "Drop box",
    operation_id = "placeDropBoxItem",
    summary = "Place an item by hand",
    description = "For an item without a usable proposal, rejected, waiting, failed or undone. New folders are made under the folder (or used, if they exist by now); a taken name gets a number.",
    params(("id" = Uuid, Path, description = "The item")),
    request_body(content = Choice, content_type = "application/json"),
    responses(
        (status = 200, description = "The item", body = ItemView),
        (status = 400, description = "An invalid place", body = ErrorBody),
        (status = 401, description = "Not signed in", body = ErrorBody),
        (status = 403, description = "A read-only API key", body = ErrorBody),
        (status = 404, description = "No such item, or no such folder", body = ErrorBody),
        (status = 409, description = "The item is being analysed, or is placed", body = ErrorBody),
    ),
    security(("session" = []), ("api_key" = [])),
)]
async fn place_item(
    State(state): State<AppState>,
    AuthUser(user, _): AuthUser,
    Path(id): Path<Uuid>,
    ApiJson(choice): ApiJson<Choice>,
) -> Result<Json<ItemView>, HttpError> {
    Ok(Json(
        review::place_by_hand(&state, user.id, id, choice).await?,
    ))
}

#[utoipa::path(
    post,
    path = "/items/{id}/retry",
    tag = "Drop box",
    operation_id = "retryDropBoxItem",
    summary = "Analyse an item again",
    params(("id" = Uuid, Path, description = "The item")),
    responses(
        (status = 200, description = "The item, queued", body = ItemView),
        (status = 401, description = "Not signed in", body = ErrorBody),
        (status = 403, description = "A read-only API key", body = ErrorBody),
        (status = 404, description = "No such item", body = ErrorBody),
        (status = 409, description = "The item is being analysed, or is placed", body = ErrorBody),
    ),
    security(("session" = []), ("api_key" = [])),
)]
async fn retry_item(
    State(state): State<AppState>,
    AuthUser(user, _): AuthUser,
    Path(id): Path<Uuid>,
) -> Result<Json<ItemView>, HttpError> {
    Ok(Json(review::retry(&state, user.id, id).await?))
}

#[utoipa::path(
    post,
    path = "/{id}/undo",
    tag = "Drop box",
    operation_id = "undoPlacement",
    summary = "Undo a placement",
    description = "The file goes back to the drop box from wherever it is in the tree now, and the folders made for it go if nothing else is in them. The item can then be placed by hand or analysed again.",
    params(("id" = Uuid, Path, description = "The placement")),
    responses(
        (status = 200, description = "The item", body = ItemView),
        (status = 401, description = "Not signed in", body = ErrorBody),
        (status = 403, description = "A read-only API key", body = ErrorBody),
        (status = 404, description = "No such placement, already undone, or its file is gone", body = ErrorBody),
    ),
    security(("session" = []), ("api_key" = [])),
)]
async fn undo_placement(
    State(state): State<AppState>,
    AuthUser(user, _): AuthUser,
    Path(id): Path<Uuid>,
) -> Result<Json<ItemView>, HttpError> {
    Ok(Json(review::undo(&state, user.id, id).await?))
}
