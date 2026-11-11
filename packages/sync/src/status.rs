//! What the engine tells the UI: its status (a `watch` channel, always the
//! latest) and its events (a `broadcast`, for the notifications).

use serde::Serialize;

/// How many recent transfers the status keeps.
pub const RECENT: usize = 50;
/// How many conflicts it keeps.
pub const CONFLICTS: usize = 20;

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum Phase {
    /// Nothing waits.
    UpToDate,
    /// Changes are being applied: `pending` of them left.
    Syncing,
    Paused,
    /// The server cannot be reached: changes wait for it.
    Offline,
    /// Something went wrong: `error` says what.
    Error,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum ActivityKind {
    Upload,
    Download,
    Move,
    Delete,
    Conflict,
    DropBox,
    /// Deleted here where the user may not delete it (view only, upload
    /// only): the server's copy is brought back.
    Restore,
}

/// Why a local change is not sent (docs/M7B_DESIGN.md §7).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum Held {
    /// In a folder the user may only view: nothing made or changed here is
    /// sent.
    ViewOnly,
    /// In a folder the user may only upload to: new files go, but what
    /// was added is not changed, renamed or moved on the server.
    UploadOnly,
    /// No longer seen on the server (limited access, a grant removed): its
    /// files stay here, not synced, until it is seen again.
    Hidden,
    /// Its drive's storage cannot be used now (docs/M7C_DESIGN.md §4): its
    /// backend fails, or its owner's private credentials are locked. Sent
    /// once it is back.
    Unavailable,
    /// Its drive is being moved to other storage, read-only meanwhile:
    /// sent once the move is done.
    Moving,
}

/// A local change or place that is not synced, and why: kept as it is
/// here, never sent nor deleted.
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct NotSynced {
    /// The local path, from the synced folder's name.
    pub path: String,
    pub why: Held,
    /// The server's words for it, when it said some: why the drive's
    /// storage is unavailable ("Locked until alice@example.com signs in"),
    /// or where it is being moved.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub reason: Option<String>,
}

/// A synced folder that waits for its drive's storage (docs/M7C_DESIGN.md
/// §4): nothing is synced in it and nothing deleted here; what is changed
/// here is kept, listed as not synced, and sent once the storage is back
/// or the move done.
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct Waiting {
    /// The synced folder's id.
    pub folder: i64,
    /// `unavailable` or `moving`.
    pub why: Held,
    /// Fit to show: the server's reason, or where the drive is going.
    pub reason: String,
}

/// A change applied, for the list of recent activity.
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct Activity {
    pub kind: ActivityKind,
    /// The local path, from the synced folder's name (`Documents/Taxes/a.pdf`).
    pub path: String,
    /// RFC 3339.
    pub at: String,
}

/// Both versions kept: the server's at the path, the local one beside it.
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct Conflict {
    pub path: String,
    pub copy: String,
    pub at: String,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct Status {
    pub phase: Phase,
    /// The changes left in this round, and those done in it.
    pub pending: usize,
    pub done: usize,
    pub folders: usize,
    /// The newest first.
    pub recent: Vec<Activity>,
    /// The newest first.
    pub conflicts: Vec<Conflict>,
    pub error: Option<String>,
    /// When the engine was last up to date (RFC 3339).
    pub synced_at: Option<String>,
    pub encryption: Encryption,
    /// The synced folders (their ids) that are no longer on the server, or
    /// whose drive the user left or was deleted: not synced, their local
    /// files kept, until they are back or the user removes them.
    pub unavailable: Vec<i64>,
    /// What the user's role keeps from being sent, and what is no longer
    /// seen on the server, as the last pass found it (at most
    /// `NOT_SYNCED` of them, the shallowest of a place only).
    pub not_synced: Vec<NotSynced>,
    /// The synced folders whose drive's storage is unavailable, or which is
    /// being moved, as the last pass found them.
    pub waiting: Vec<Waiting>,
}

/// How many not-synced places the status lists.
pub const NOT_SYNCED: usize = 100;

/// The end-to-end-encrypted folders, as this computer sees them.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize)]
pub struct Encryption {
    /// The account has set up encryption.
    pub set_up: bool,
    /// The private key is here: encrypted folders are synced.
    pub unlocked: bool,
    /// Encrypted folders left alone in the last pass: locked here, or
    /// without a key for this user yet.
    pub skipped: usize,
}

impl Default for Status {
    fn default() -> Self {
        Status {
            phase: Phase::UpToDate,
            pending: 0,
            done: 0,
            folders: 0,
            recent: Vec::new(),
            conflicts: Vec::new(),
            error: None,
            synced_at: None,
            encryption: Encryption::default(),
            unavailable: Vec::new(),
            not_synced: Vec::new(),
            waiting: Vec::new(),
        }
    }
}

/// What the app may want to tell the user about.
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum Event {
    /// A conflict copy was made.
    Conflict { path: String, copy: String },
    /// Something in the drop box changed: ask for its items again.
    DropBox,
    /// The server told of changes (its live events): what keeps the
    /// on-demand folders outside the engine (macOS' File Provider
    /// extension) is told to look.
    Changes,
    /// The token was refused: the app signs out.
    SignedOut,
    /// The private key here is not the account's any more: forgotten (the
    /// app forgets its copy too).
    Locked,
    /// A change could not be applied.
    Error { message: String },
}

pub fn now() -> String {
    jiff::Timestamp::now().to_string()
}
