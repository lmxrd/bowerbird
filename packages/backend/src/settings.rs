//! Settings levels (docs/M7B_DESIGN.md §4): the settings a superadmin, a
//! team's managers, a user or a drive's managers set, in one table
//! (`settings`), read through one resolver that walks the levels:
//!
//! environment > superadmin > team > user > drive
//!
//! At each level a setting is **locked** (fixed for everyone below, who see
//! who set it), a **default** (given, those below may change it), a
//! **range** (a number those below choose within: at least, at most; a
//! default may carry one too) or **not set** (no row: those below choose,
//! the built-in default applies until they do). An environment variable
//! locks what it sets.
//!
//! A user in several teams (§3): the drive settings of a shared drive
//! follow the drive's team; sign-in settings take the **strictest** of
//! their teams; their own settings (My drive, preferences) the
//! **loosest** of their teams' locks and ranges; defaults come from their
//! **primary team** (the first they joined, or the one they chose).
//!
//! The settings no level has (backups, single sign-on, sign-up, AI
//! concurrency) stay the superadmin's own, as JSON documents in
//! `server_settings` (src/server_settings.rs).

use crate::db::Db;
use crate::error::HttpError;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use utoipa::ToSchema;
use uuid::Uuid;

/// A level of the settings.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize, ToSchema, sqlx::Type)]
#[serde(rename_all = "snake_case")]
#[sqlx(type_name = "TEXT", rename_all = "snake_case")]
#[schema(as = SettingLevel)]
pub enum Level {
    Superadmin,
    Team,
    User,
    Drive,
}

impl Level {
    fn as_str(self) -> &'static str {
        match self {
            Level::Superadmin => "superadmin",
            Level::Team => "team",
            Level::User => "user",
            Level::Drive => "drive",
        }
    }
}

/// What a level does with a setting (no row: not set).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize, ToSchema, sqlx::Type)]
#[serde(rename_all = "snake_case")]
#[sqlx(type_name = "TEXT", rename_all = "snake_case")]
#[schema(as = SettingMode)]
pub enum Mode {
    /// The value is fixed for every level below.
    Locked,
    /// The value is given; the levels below may change it (within `min`
    /// and `max`, when set).
    Default,
    /// Only a range for the levels below (a number): `min`, `max`.
    Range,
}

/// What kind of value a setting takes.
#[derive(Clone, Copy, Debug)]
pub enum Kind {
    Bool,
    /// A whole number in a range.
    Int(i64, i64),
    /// A number above `0` up to `1`.
    Fraction,
    /// One of these strings.
    Choice(&'static [&'static str]),
    /// A model's id, one of the superadmin's AI models that can do the
    /// work; null: the AI page's choice.
    Model(crate::ai::Work),
    /// A list of top-level folder names, or null for any.
    Folders,
}

/// How a user in several teams gets a setting from them (§3).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Scope {
    /// Their own settings: the loosest of their teams' locks and ranges.
    Own,
    /// Sign-in: the strictest of their teams.
    SignIn,
    /// A drive's: a shared drive's team; for My drive, as `Own`.
    Drive,
    /// The superadmin's alone.
    Instance,
}

/// A setting with levels.
pub struct Def {
    pub key: &'static str,
    pub kind: Kind,
    /// The built-in default.
    pub default: fn() -> Value,
    pub levels: &'static [Level],
    pub scope: Scope,
    pub description: &'static str,
}

use Level::{Drive, Superadmin, Team, User};

const PERSONAL: &[Level] = &[Superadmin, Team, User];

/// Every setting with levels (the table of §4).
pub const DEFS: &[Def] = &[
    Def {
        key: "drop_box.pending_mode",
        kind: Kind::Choice(&["move_pending", "stay", "timeout", "confidence"]),
        default: || Value::from("move_pending"),
        levels: PERSONAL,
        scope: Scope::Own,
        description: "What happens to a drop box item once a place is proposed",
    },
    Def {
        key: "drop_box.accept_after_hours",
        kind: Kind::Int(1, 720),
        default: || Value::from(24),
        levels: PERSONAL,
        scope: Scope::Own,
        description: "With the pending mode timeout: hours until an item is accepted",
    },
    Def {
        key: "drop_box.confidence_threshold",
        kind: Kind::Fraction,
        default: || Value::from(0.8),
        levels: PERSONAL,
        scope: Scope::Own,
        description: "With the pending mode confidence: from which confidence an item is accepted",
    },
    Def {
        key: "drop_box.read_contents",
        kind: Kind::Choice(&["always", "per_item", "default_on"]),
        default: || Value::from("always"),
        levels: PERSONAL,
        scope: Scope::Own,
        description: "When the drop box reads and sends the contents of a file",
    },
    Def {
        key: "drop_box.folder_freedom",
        kind: Kind::Choice(&["consistent", "balanced", "refined"]),
        default: || Value::from("balanced"),
        levels: PERSONAL,
        scope: Scope::Own,
        description: "How readily the drop box makes new folders",
    },
    Def {
        key: "drop_box.allowed_top_folders",
        kind: Kind::Folders,
        default: || Value::Null,
        levels: &[User],
        scope: Scope::Own,
        description: "The top-level folders the drop box may place items in (null: any)",
    },
    Def {
        key: "drop_box.folders",
        kind: Kind::Choice(&["whole", "split"]),
        default: || Value::from("whole"),
        levels: &[User],
        scope: Scope::Own,
        description: "What a dropped folder becomes",
    },
    Def {
        key: "drop_box.links",
        kind: Kind::Choice(&["bookmark", "snapshot", "download"]),
        default: || Value::from("snapshot"),
        levels: &[User],
        scope: Scope::Own,
        description: "What a dropped link becomes",
    },
    Def {
        key: "retention.trash_days",
        kind: Kind::Int(0, 3_650),
        default: || Value::from(30),
        levels: &[Superadmin, Team, User, Drive],
        scope: Scope::Drive,
        description: "Days the trash keeps what is deleted (0: until it is emptied)",
    },
    Def {
        key: "retention.versions_hours",
        kind: Kind::Int(0, 87_600),
        default: || Value::from(24),
        levels: &[Superadmin, Team, User, Drive],
        scope: Scope::Drive,
        description: "Hours every version is kept",
    },
    Def {
        key: "retention.versions_days",
        kind: Kind::Int(0, 3_650),
        default: || Value::from(30),
        levels: &[Superadmin, Team, User, Drive],
        scope: Scope::Drive,
        description: "Then days the last version of each day is kept",
    },
    Def {
        key: "retention.versions_months",
        kind: Kind::Int(0, 120),
        default: || Value::from(12),
        levels: &[Superadmin, Team, User, Drive],
        scope: Scope::Drive,
        description: "Then months the last version of each month is kept",
    },
    Def {
        key: "notifications.review",
        kind: Kind::Bool,
        default: || Value::Bool(true),
        levels: PERSONAL,
        scope: Scope::Own,
        description: "A notice when drop box items wait for review",
    },
    Def {
        key: "notifications.shared",
        kind: Kind::Bool,
        default: || Value::Bool(true),
        levels: PERSONAL,
        scope: Scope::Own,
        description: "A notice when something is shared with you",
    },
    Def {
        key: "notifications.backup",
        kind: Kind::Bool,
        default: || Value::Bool(true),
        levels: PERSONAL,
        scope: Scope::Own,
        description: "A notice when a backup failed (superadmins)",
    },
    Def {
        key: "ai.model.proposals",
        kind: Kind::Model(crate::ai::Work::Proposals),
        default: || Value::Null,
        levels: PERSONAL,
        scope: Scope::Own,
        description: "The model that proposes places in the drop box (null: the AI page's choice)",
    },
    Def {
        key: "ai.model.images",
        kind: Kind::Model(crate::ai::Work::Images),
        default: || Value::Null,
        levels: PERSONAL,
        scope: Scope::Own,
        description: "The model that reads photos (null: the AI page's choice)",
    },
    Def {
        key: "ai.model.scans",
        kind: Kind::Model(crate::ai::Work::Scans),
        default: || Value::Null,
        levels: PERSONAL,
        scope: Scope::Own,
        description: "The model that reads scans (null: the AI page's choice)",
    },
    Def {
        key: "ai.model.audio",
        kind: Kind::Model(crate::ai::Work::Audio),
        default: || Value::Null,
        levels: PERSONAL,
        scope: Scope::Own,
        description: "The model that transcribes recordings (null: the AI page's choice)",
    },
    Def {
        key: "ai.own_models",
        kind: Kind::Bool,
        default: || Value::Bool(false),
        levels: &[Superadmin],
        scope: Scope::Instance,
        description: "Users may add their own AI models",
    },
    Def {
        key: "links.allowed",
        kind: Kind::Bool,
        default: || Value::Bool(true),
        levels: &[Superadmin, Team, Drive],
        scope: Scope::Drive,
        description: "Public links may be made",
    },
    Def {
        key: "links.password_required",
        kind: Kind::Bool,
        default: || Value::Bool(false),
        levels: &[Superadmin, Team, Drive],
        scope: Scope::Drive,
        description: "Every public link has a password",
    },
    Def {
        key: "links.max_days",
        kind: Kind::Int(0, 3_650),
        default: || Value::from(0),
        levels: &[Superadmin, Team, Drive],
        scope: Scope::Drive,
        description: "The longest a public link lives, in days (0: no limit)",
    },
    Def {
        key: "encryption.allowed",
        kind: Kind::Bool,
        default: || Value::Bool(true),
        levels: &[Superadmin, Team],
        scope: Scope::Own,
        description: "Users may make end-to-end-encrypted folders",
    },
    Def {
        key: "sign_in.two_factor_required",
        kind: Kind::Bool,
        default: || Value::Bool(false),
        levels: &[Superadmin, Team],
        scope: Scope::SignIn,
        description: "Two-factor authentication is required to sign in",
    },
    Def {
        key: "drives.create",
        kind: Kind::Choice(&["superadmin", "team_managers", "everyone"]),
        default: || Value::from("team_managers"),
        levels: &[Superadmin],
        scope: Scope::Instance,
        description: "Who may make shared drives: the superadmin only, also team managers (their team's), or everyone",
    },
    Def {
        key: "storage.own_backends",
        kind: Kind::Bool,
        default: || Value::Bool(false),
        levels: &[Superadmin, Team],
        scope: Scope::Own,
        description: "Users may add their own S3-compatible storage and make shared drives on it",
    },
    Def {
        key: "mirrors.shared_drives",
        kind: Kind::Bool,
        default: || Value::Bool(false),
        levels: &[Superadmin, Team],
        scope: Scope::Drive,
        description: "Members may mirror shared drives to their own storage (a team's drives by the team's setting)",
    },
    Def {
        key: "mirrors.allowed",
        kind: Kind::Bool,
        default: || Value::Bool(true),
        levels: &[Drive],
        scope: Scope::Drive,
        description: "This drive may be mirrored (its managers may forbid it; that stops the mirrors of it)",
    },
    Def {
        key: "federation.share_with_any",
        kind: Kind::Bool,
        default: || Value::Bool(false),
        levels: &[Superadmin, Team],
        scope: Scope::Own,
        description: "Users may share with any address on another server (and take shares from anyone), not only their contacts and the servers listed",
    },
    Def {
        key: "federation.allowed",
        kind: Kind::Bool,
        default: || Value::Bool(true),
        levels: &[Superadmin, Team, Drive],
        scope: Scope::Drive,
        description: "This drive may be shared with people on other servers (forbidding it stops its shares with them)",
    },
    Def {
        key: "federation.read_remote",
        kind: Kind::Bool,
        default: || Value::Bool(true),
        levels: &[Superadmin],
        scope: Scope::Instance,
        description: "Search and the AI may read what people on other servers share with users here",
    },
];

/// A setting's definition, by key.
pub fn def(key: &str) -> Option<&'static Def> {
    DEFS.iter().find(|d| d.key == key)
}

/// What the environment sets: each locks its setting.
#[derive(Clone, Debug, Default)]
pub struct Env {
    pub values: Vec<(&'static str, Value)>,
}

impl Env {
    /// The `TRASH_DAYS` and `VERSIONS_*` variables (all a drive's tree
    /// needs).
    pub fn of_files(files: &crate::files::Files) -> Env {
        let r = files.retention_env();
        let mut values = Vec::new();
        for (key, v) in [
            ("retention.trash_days", r.trash_days),
            ("retention.versions_hours", r.versions_hours),
            ("retention.versions_days", r.versions_days),
            ("retention.versions_months", r.versions_months),
        ] {
            if let Some(v) = v {
                values.push((key, Value::from(v)));
            }
        }
        Env { values }
    }

    /// Every variable that locks a setting: the retention ones,
    /// `TOTP_REQUIRED`, and `AI_MODEL_*` (a work's model).
    pub fn of_state(state: &crate::state::AppState) -> Env {
        let mut env = Env::of_files(&state.files);
        if state.totp_required {
            env.values
                .push(("sign_in.two_factor_required", Value::Bool(true)));
        }
        for def in DEFS {
            if let Kind::Model(work) = def.kind
                && let Some(id) = state.ai.locked_model(work)
            {
                env.values.push((def.key, Value::from(id)));
            }
        }
        env
    }

    fn get(&self, key: &str) -> Option<&Value> {
        self.values.iter().find(|(k, _)| *k == key).map(|(_, v)| v)
    }
}

/// Where a setting's value comes from, or who locked it.
#[derive(Clone, Debug, PartialEq, Serialize, ToSchema)]
#[schema(as = SettingSource)]
pub struct Source {
    /// `env` (an environment variable), `builtin`, or a level:
    /// `superadmin`, `team`, `user`, `drive`.
    pub level: String,
    /// A team's: its id and name.
    #[schema(required = true)]
    pub team_id: Option<Uuid>,
    #[schema(required = true)]
    pub team_name: Option<String>,
}

impl Source {
    fn of(level: &str) -> Source {
        Source {
            level: level.into(),
            team_id: None,
            team_name: None,
        }
    }
}

/// A setting as it applies.
#[derive(Clone, Debug, Serialize, ToSchema)]
#[schema(as = ResolvedSetting)]
pub struct Resolved {
    pub key: String,
    /// The value in effect (JSON: a boolean, number, string, list or null).
    pub value: Value,
    /// Where it comes from.
    pub source: Source,
    /// Who locked it, when a level above locked it (the clients show it
    /// disabled: "Set by your administrator", "Set by <team>").
    #[schema(required = true)]
    pub locked_by: Option<Source>,
    /// A number's range from the levels above.
    #[schema(required = true)]
    pub min: Option<f64>,
    #[schema(required = true)]
    pub max: Option<f64>,
}

impl Resolved {
    pub fn locked(&self) -> bool {
        self.locked_by.is_some()
    }

    pub fn bool(&self) -> bool {
        self.value.as_bool().unwrap_or(false)
    }

    pub fn int(&self) -> i64 {
        self.value.as_i64().unwrap_or(0)
    }

    pub fn str(&self) -> Option<&str> {
        self.value.as_str()
    }
}

/// A row of `settings`.
#[derive(Clone, Debug, sqlx::FromRow)]
pub struct Row {
    pub level: Level,
    pub level_id: Option<Uuid>,
    pub key: String,
    pub value: Option<String>,
    pub mode: Mode,
    pub min: Option<f64>,
    pub max: Option<f64>,
}

impl Row {
    fn json(&self) -> Option<Value> {
        self.value
            .as_deref()
            .and_then(|v| serde_json::from_str(v).ok())
    }
}

/// Who a setting is resolved for.
#[derive(Clone, Copy, Debug, Default)]
pub struct For {
    pub user: Option<Uuid>,
    /// The drive, for the drive settings.
    pub drive: Option<Uuid>,
}

impl For {
    pub fn user(user: Uuid) -> For {
        For {
            user: Some(user),
            drive: None,
        }
    }

    pub fn drive(drive: Uuid) -> For {
        For {
            user: None,
            drive: Some(drive),
        }
    }
}

/// A level's row of a key.
pub async fn row(
    db: &Db,
    level: Level,
    level_id: Option<Uuid>,
    key: &str,
) -> sqlx::Result<Option<Row>> {
    sqlx::query_as(
        "SELECT level, level_id, key, value, mode, min, max FROM settings
         WHERE level = ?1 AND level_id IS ?2 AND key = ?3",
    )
    .bind(level)
    .bind(level_id)
    .bind(key)
    .fetch_optional(db.reader())
    .await
}

/// The levels that apply, top-down, below the environment: each with its
/// row (or a row made of a user's several teams), and its source.
async fn chain(
    db: &Db,
    def: &Def,
    to: For,
    upto: Option<Level>,
) -> sqlx::Result<Vec<(Option<Row>, Source)>> {
    let mut out = Vec::new();
    let has = |l: Level| def.levels.contains(&l) && upto != Some(l);
    let stop = |l: Level| upto.is_some_and(|u| level_rank(l) >= level_rank(u));
    if stop(Superadmin) {
        return Ok(out);
    }
    if def.levels.contains(&Superadmin) {
        out.push((
            row(db, Superadmin, None, def.key).await?,
            Source::of("superadmin"),
        ));
    }
    // The drive, when the setting is a drive's: a shared drive and its
    // team, or My drive and its user.
    let drive: Option<(Uuid, Option<Uuid>, Option<Uuid>)> = match to.drive {
        Some(drive) if def.scope == Scope::Drive => {
            sqlx::query_as("SELECT id, user_id, team_id FROM drives WHERE id = ?1")
                .bind(drive)
                .fetch_optional(db.reader())
                .await?
        }
        _ => None,
    };
    let user = match drive {
        Some((_, my_user, _)) => my_user,
        None => to.user,
    };
    if stop(Team) {
        return Ok(out);
    }
    if has(Team) {
        match drive {
            // A shared drive: its team's, whoever works there.
            Some((_, None, team)) => {
                if let Some(team) = team {
                    let name: Option<String> =
                        sqlx::query_scalar("SELECT name FROM teams WHERE id = ?1")
                            .bind(team)
                            .fetch_optional(db.reader())
                            .await?;
                    out.push((
                        row(db, Team, Some(team), def.key).await?,
                        Source {
                            level: "team".into(),
                            team_id: Some(team),
                            team_name: name,
                        },
                    ));
                }
            }
            _ => {
                if let Some(user) = user
                    && let Some(found) = teams_row(db, def, user).await?
                {
                    out.push(found);
                }
            }
        }
    }
    if stop(User) {
        return Ok(out);
    }
    let my_drive = matches!(drive, Some((_, Some(_), _)));
    if has(User)
        && let Some(user) = user
        && (drive.is_none() || my_drive)
    {
        out.push((
            row(db, User, Some(user), def.key).await?,
            Source::of("user"),
        ));
    }
    if stop(Drive) {
        return Ok(out);
    }
    // My drive's own settings are its user's, unless the setting has no
    // user level (links): then My drive's drive row, which its user sets.
    if has(Drive)
        && let Some((drive, _, _)) = drive
        && !(my_drive && def.levels.contains(&User))
    {
        out.push((
            row(db, Drive, Some(drive), def.key).await?,
            Source::of("drive"),
        ));
    }
    Ok(out)
}

fn level_rank(level: Level) -> u8 {
    match level {
        Level::Superadmin => 0,
        Level::Team => 1,
        Level::User => 2,
        Level::Drive => 3,
    }
}

/// A user's teams as one level (§3): sign-in settings the strictest of
/// them, their own the loosest; values from their primary team.
async fn teams_row(db: &Db, def: &Def, user: Uuid) -> sqlx::Result<Option<(Option<Row>, Source)>> {
    let teams: Vec<(Uuid, String)> = sqlx::query_as(
        "SELECT t.id, t.name FROM team_members m JOIN teams t ON t.id = m.team_id
         WHERE m.user_id = ?1
         ORDER BY t.id = (SELECT primary_team_id FROM users WHERE id = ?1) DESC, m.joined_at",
    )
    .bind(user)
    .fetch_all(db.reader())
    .await?;
    if teams.is_empty() {
        return Ok(None);
    }
    let mut rows = Vec::with_capacity(teams.len());
    for (team, _) in &teams {
        rows.push(row(db, Team, Some(*team), def.key).await?);
    }
    let source = |i: usize| Source {
        level: "team".into(),
        team_id: Some(teams[i].0),
        team_name: Some(teams[i].1.clone()),
    };
    let primary = rows[0].clone();
    if teams.len() == 1 {
        return Ok(Some((primary, source(0))));
    }
    let merged = match def.scope {
        Scope::SignIn => strictest(&rows),
        _ => loosest(&rows),
    };
    // Who it comes from: the team that locks it, else the primary.
    let from = merged
        .as_ref()
        .filter(|r| r.mode == Mode::Locked)
        .and_then(|r| r.level_id)
        .and_then(|id| teams.iter().position(|(t, _)| *t == id))
        .unwrap_or(0);
    Ok(Some((merged, source(from))))
}

/// The strictest of several teams' rows: locked when any locks it (true
/// over false for a switch), ranges intersected, the primary's default.
fn strictest(rows: &[Option<Row>]) -> Option<Row> {
    let locks: Vec<&Row> = rows
        .iter()
        .flatten()
        .filter(|r| r.mode == Mode::Locked)
        .collect();
    if let Some(lock) = locks
        .iter()
        .find(|r| r.json() == Some(Value::Bool(true)))
        .or(locks.first())
    {
        return Some((*lock).clone());
    }
    let mut out = rows[0]
        .clone()
        .or_else(|| rows.iter().flatten().next().cloned())?;
    out.min = rows.iter().flatten().filter_map(|r| r.min).reduce(f64::max);
    out.max = rows.iter().flatten().filter_map(|r| r.max).reduce(f64::min);
    Some(out)
}

/// The loosest of several teams' rows: locked only when every team locks
/// it (the primary's lock, else the first), a range only as wide as the
/// widest (no bound where a team has none), the primary's default.
fn loosest(rows: &[Option<Row>]) -> Option<Row> {
    let all_locked = rows
        .iter()
        .all(|r| r.as_ref().is_some_and(|r| r.mode == Mode::Locked));
    if all_locked {
        return rows[0].clone();
    }
    let bound = |pick: fn(&Row) -> Option<f64>, wider: fn(f64, f64) -> f64| -> Option<f64> {
        let mut out: Option<f64> = None;
        for r in rows {
            // A team without a bound leaves that side open.
            let b = r.as_ref().and_then(pick)?;
            out = Some(out.map_or(b, |o| wider(o, b)));
        }
        out
    };
    let min = bound(|r| r.min, f64::min);
    let max = bound(|r| r.max, f64::max);
    let primary = rows[0].clone();
    let value = primary
        .as_ref()
        .filter(|r| r.mode != Mode::Range)
        .and_then(|r| r.value.clone());
    if value.is_none() && min.is_none() && max.is_none() {
        return None;
    }
    let base = primary.or_else(|| rows.iter().flatten().next().cloned())?;
    Some(Row {
        mode: if value.is_some() {
            Mode::Default
        } else {
            Mode::Range
        },
        value,
        min,
        max,
        ..base
    })
}

/// A number held to a range.
fn clamp(value: Value, min: Option<f64>, max: Option<f64>) -> Value {
    let Some(n) = value.as_f64() else {
        return value;
    };
    let mut c = n;
    if let Some(min) = min {
        c = c.max(min);
    }
    if let Some(max) = max {
        c = c.min(max);
    }
    if c == n {
        return value;
    }
    if value.is_i64() || value.is_u64() {
        Value::from(c.round() as i64)
    } else {
        Value::from(c)
    }
}

/// A setting as it applies to a user or a drive.
pub async fn resolve(db: &Db, env: &Env, key: &str, to: For) -> sqlx::Result<Resolved> {
    let def = def(key).expect("a known setting");
    resolve_def(db, env, def, to, None).await
}

/// A setting as the levels above `upto` leave it (what that level sees
/// above it); `upto` None: all of them.
pub async fn resolve_def(
    db: &Db,
    env: &Env,
    def: &Def,
    to: For,
    upto: Option<Level>,
) -> sqlx::Result<Resolved> {
    let mut out = Resolved {
        key: def.key.into(),
        value: (def.default)(),
        source: Source::of("builtin"),
        locked_by: None,
        min: None,
        max: None,
    };
    if let Some(value) = env.get(def.key) {
        out.value = value.clone();
        out.source = Source::of("env");
        out.locked_by = Some(Source::of("env"));
        return Ok(out);
    }
    for (row, source) in chain(db, def, to, upto).await? {
        let Some(row) = row else { continue };
        match row.mode {
            Mode::Locked => {
                if let Some(value) = row.json() {
                    out.value = clamp(value, out.min, out.max);
                }
                out.source = source.clone();
                out.locked_by = Some(source);
                break;
            }
            Mode::Default | Mode::Range => {
                if let Some(min) = row.min {
                    out.min = Some(out.min.map_or(min, |m| m.max(min)));
                }
                if let Some(max) = row.max {
                    out.max = Some(out.max.map_or(max, |m| m.min(max)));
                }
                if row.mode == Mode::Default
                    && let Some(value) = row.json()
                {
                    out.value = value;
                    out.source = source;
                }
                out.value = clamp(out.value.clone(), out.min, out.max);
            }
        }
    }
    Ok(out)
}

/// Whether a value fits a setting's kind (a 400 otherwise, fit to show).
pub fn check_value(
    state: Option<&crate::state::AppState>,
    def: &Def,
    value: &Value,
) -> Result<(), HttpError> {
    let bad = |what: &str| HttpError::bad_request(format!("{}: expected {what}", def.key));
    match def.kind {
        Kind::Bool => {
            value.as_bool().ok_or_else(|| bad("true or false"))?;
        }
        Kind::Int(min, max) => {
            let n = value
                .as_i64()
                .ok_or_else(|| bad(&format!("a whole number from {min} to {max}")))?;
            if !(min..=max).contains(&n) {
                return Err(bad(&format!("a whole number from {min} to {max}")));
            }
        }
        Kind::Fraction => {
            let n = value
                .as_f64()
                .ok_or_else(|| bad("a number above 0, at most 1"))?;
            if !(n > 0.0 && n <= 1.0) {
                return Err(bad("a number above 0, at most 1"));
            }
        }
        Kind::Choice(choices) => {
            let s = value
                .as_str()
                .ok_or_else(|| bad(&format!("one of {}", choices.join(", "))))?;
            if !choices.contains(&s) {
                return Err(bad(&format!("one of {}", choices.join(", "))));
            }
        }
        Kind::Model(work) => {
            if value.is_null() {
                return Ok(());
            }
            let id = value.as_str().ok_or_else(|| bad("a model's id, or null"))?;
            if let Some(state) = state
                && !state.ai.can_do(id, work)
            {
                return Err(bad("one of the superadmin's models that can do this"));
            }
        }
        Kind::Folders => {
            if value.is_null() {
                return Ok(());
            }
            let names = value
                .as_array()
                .ok_or_else(|| bad("a list of folder names, or null"))?;
            if names.is_empty() || names.len() > 100 {
                return Err(bad("1 to 100 folder names, or null for any"));
            }
            for name in names {
                let name = name.as_str().unwrap_or_default().trim();
                if name.is_empty() || name.len() > 200 || name.contains('/') {
                    return Err(bad("folder names (not empty, no /)"));
                }
            }
        }
    }
    Ok(())
}

/// What a level writes: its mode, value and range.
#[derive(Clone, Debug, Deserialize, ToSchema)]
pub struct LevelValue {
    pub mode: Mode,
    /// The value (for `locked` and `default`): JSON of the setting's kind.
    #[serde(default)]
    pub value: Option<Value>,
    /// A number's range for the levels below: at least.
    pub min: Option<f64>,
    /// At most.
    pub max: Option<f64>,
}

/// Store a level's setting, once it is checked against its kind and what
/// the levels above leave it: a lock above refuses it (403), a value
/// outside the range above is refused (400).
pub async fn set(
    state: &crate::state::AppState,
    level: Level,
    level_id: Option<Uuid>,
    key: &str,
    body: &LevelValue,
    to: For,
) -> Result<Resolved, HttpError> {
    let def = def(key).ok_or_else(HttpError::not_found)?;
    if !def.levels.contains(&level) {
        return Err(HttpError::bad_request(format!(
            "{key} cannot be set at the {} level",
            level.as_str()
        )));
    }
    let env = Env::of_state(state);
    let above = resolve_def(&state.db, &env, def, to, Some(level)).await?;
    if let Some(by) = &above.locked_by {
        return Err(locked_error(by));
    }
    let numeric = matches!(def.kind, Kind::Int(..) | Kind::Fraction);
    if (body.min.is_some() || body.max.is_some()) && !numeric {
        return Err(HttpError::bad_request(format!(
            "{key} is not a number: it has no range"
        )));
    }
    let value = match body.mode {
        Mode::Range => {
            if body.min.is_none() && body.max.is_none() {
                return Err(HttpError::bad_request("A range needs min or max"));
            }
            None
        }
        _ => {
            let value = body
                .value
                .clone()
                .ok_or_else(|| HttpError::bad_request("A value is needed"))?;
            check_value(Some(state), def, &value)?;
            Some(value)
        }
    };
    let within = |n: f64| above.min.is_none_or(|m| n >= m) && above.max.is_none_or(|m| n <= m);
    for n in value
        .iter()
        .filter_map(Value::as_f64)
        .chain(body.min)
        .chain(body.max)
    {
        if !within(n) {
            return Err(HttpError::bad_request(format!(
                "{key} must stay within {}",
                range_text(above.min, above.max)
            )));
        }
    }
    if let (Some(min), Some(max)) = (body.min, body.max)
        && min > max
    {
        return Err(HttpError::bad_request("min is above max"));
    }
    sqlx::query(
        "INSERT INTO settings (level, level_id, key, value, mode, min, max, updated_at)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8)
         ON CONFLICT (level, ifnull(level_id, x''), key) DO UPDATE SET
           value = ?4, mode = ?5, min = ?6, max = ?7, updated_at = ?8",
    )
    .bind(level)
    .bind(level_id)
    .bind(key)
    .bind(value.map(|v| v.to_string()))
    .bind(body.mode)
    .bind(body.min)
    .bind(body.max)
    .bind(crate::auth::now_iso())
    .execute(state.db.writer())
    .await?;
    Ok(resolve_def(&state.db, &env, def, to, None).await?)
}

/// Remove a level's setting: not set there any more.
pub async fn unset(
    db: &Db,
    level: Level,
    level_id: Option<Uuid>,
    key: &str,
) -> Result<(), HttpError> {
    def(key).ok_or_else(HttpError::not_found)?;
    sqlx::query("DELETE FROM settings WHERE level = ?1 AND level_id IS ?2 AND key = ?3")
        .bind(level)
        .bind(level_id)
        .bind(key)
        .execute(db.writer())
        .await?;
    Ok(())
}

fn range_text(min: Option<f64>, max: Option<f64>) -> String {
    match (min, max) {
        (Some(min), Some(max)) => format!("{min} to {max}"),
        (Some(min), None) => format!("at least {min}"),
        (None, Some(max)) => format!("at most {max}"),
        (None, None) => "its range".into(),
    }
}

/// The refusal of a change a level above locked.
pub fn locked_error(by: &Source) -> HttpError {
    let who = match (by.level.as_str(), &by.team_name) {
        ("env", _) => "an environment variable".to_string(),
        ("team", Some(name)) => name.clone(),
        _ => "your administrator".to_string(),
    };
    HttpError::new(
        axum::http::StatusCode::FORBIDDEN,
        format!("Set by {who}: it cannot be changed here"),
    )
}

/// A user's own value of a setting (their level): checked against the
/// levels above, stored with mode `default`.
pub async fn set_own(
    state: &crate::state::AppState,
    user: Uuid,
    key: &str,
    value: Value,
) -> Result<Resolved, HttpError> {
    set(
        state,
        Level::User,
        Some(user),
        key,
        &LevelValue {
            mode: Mode::Default,
            value: Some(value),
            min: None,
            max: None,
        },
        For::user(user),
    )
    .await
}

/// A setting as a level's page shows it.
#[derive(Serialize, ToSchema)]
pub struct LevelSetting {
    pub key: String,
    pub description: String,
    /// The kind of value: `bool`, `integer`, `fraction`, `choice`, `model`
    /// or `folders`.
    pub kind: String,
    /// For `choice`: the values; for `integer`: none.
    pub choices: Vec<String>,
    /// For `integer`: its bounds.
    #[schema(required = true)]
    pub kind_min: Option<f64>,
    #[schema(required = true)]
    pub kind_max: Option<f64>,
    /// The levels it has.
    pub levels: Vec<Level>,
    /// This level's: null when not set here.
    #[schema(required = true)]
    pub mode: Option<Mode>,
    pub value: Value,
    #[schema(required = true)]
    pub min: Option<f64>,
    #[schema(required = true)]
    pub max: Option<f64>,
    /// What the levels above leave it (with `locked_by` when they lock it:
    /// it cannot be set here then).
    pub above: Resolved,
}

/// The settings of a level (those it has), each with what is above it.
pub async fn level_view(
    state: &crate::state::AppState,
    level: Level,
    level_id: Option<Uuid>,
    to: For,
) -> Result<Vec<LevelSetting>, HttpError> {
    let env = Env::of_state(state);
    let mut out = Vec::new();
    for def in DEFS.iter().filter(|d| d.levels.contains(&level)) {
        let above = resolve_def(&state.db, &env, def, to, Some(level)).await?;
        let here = row(&state.db, level, level_id, def.key).await?;
        out.push(level_setting(def, here, above));
    }
    Ok(out)
}

pub fn level_setting(def: &Def, here: Option<Row>, above: Resolved) -> LevelSetting {
    let (kind, choices, kind_min, kind_max) = describe(def);
    LevelSetting {
        key: def.key.into(),
        description: def.description.into(),
        kind,
        choices,
        kind_min,
        kind_max,
        levels: def.levels.to_vec(),
        mode: here.as_ref().map(|r| r.mode),
        value: here.as_ref().and_then(Row::json).unwrap_or(Value::Null),
        min: here.as_ref().and_then(|r| r.min),
        max: here.as_ref().and_then(|r| r.max),
        above,
    }
}

fn describe(def: &Def) -> (String, Vec<String>, Option<f64>, Option<f64>) {
    match def.kind {
        Kind::Bool => ("bool".into(), Vec::new(), None, None),
        Kind::Int(min, max) => (
            "integer".into(),
            Vec::new(),
            Some(min as f64),
            Some(max as f64),
        ),
        Kind::Fraction => ("fraction".into(), Vec::new(), Some(0.0), Some(1.0)),
        Kind::Choice(c) => (
            "choice".into(),
            c.iter().map(|s| s.to_string()).collect(),
            None,
            None,
        ),
        Kind::Model(_) => ("model".into(), Vec::new(), None, None),
        Kind::Folders => ("folders".into(), Vec::new(), None, None),
    }
}

/// A setting as a user sees it: the value in effect, where it comes from,
/// and whether they may change it.
#[derive(Serialize, ToSchema)]
pub struct UserSetting {
    #[serde(flatten)]
    pub resolved: Resolved,
    pub description: String,
    /// See `LevelSetting.kind`.
    pub kind: String,
    pub choices: Vec<String>,
    #[schema(required = true)]
    pub kind_min: Option<f64>,
    #[schema(required = true)]
    pub kind_max: Option<f64>,
    /// The user may change it (it has a user level, and nothing above
    /// locks it).
    pub may_change: bool,
}

/// Every setting a user has a level of, as it applies to them.
pub async fn user_view(
    state: &crate::state::AppState,
    user: Uuid,
) -> Result<Vec<UserSetting>, HttpError> {
    let env = Env::of_state(state);
    let mut out = Vec::new();
    for def in DEFS {
        let resolved = resolve_def(&state.db, &env, def, For::user(user), None).await?;
        let (kind, choices, kind_min, kind_max) = describe(def);
        out.push(UserSetting {
            may_change: def.levels.contains(&Level::User) && !resolved.locked(),
            resolved,
            description: def.description.into(),
            kind,
            choices,
            kind_min,
            kind_max,
        });
    }
    Ok(out)
}

/// The model a user chose for a work (`ai.model.*`, theirs or their
/// team's or the superadmin's); None: the AI page's choice.
pub async fn ai_model(
    state: &crate::state::AppState,
    user: Uuid,
    work: crate::ai::Work,
) -> sqlx::Result<Option<String>> {
    let key = format!("ai.model.{}", work.as_str());
    let Some(def) = def(&key) else {
        return Ok(None);
    };
    let env = Env::of_state(state);
    let resolved = resolve_def(&state.db, &env, def, For::user(user), None).await?;
    Ok(resolved.str().map(String::from))
}

#[cfg(test)]
mod tests {
    use super::*;

    async fn db() -> Db {
        crate::db::open(":memory:").await.unwrap()
    }

    #[allow(clippy::too_many_arguments)]
    async fn put(
        db: &Db,
        level: &str,
        id: Option<Uuid>,
        key: &str,
        value: Option<&str>,
        mode: &str,
        min: Option<f64>,
        max: Option<f64>,
    ) {
        sqlx::query(
            "INSERT INTO settings (level, level_id, key, value, mode, min, max)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)",
        )
        .bind(level)
        .bind(id)
        .bind(key)
        .bind(value)
        .bind(mode)
        .bind(min)
        .bind(max)
        .execute(db.writer())
        .await
        .unwrap();
    }

    async fn user(db: &Db, email: &str) -> Uuid {
        let id = Uuid::now_v7();
        sqlx::query("INSERT INTO users (id, email) VALUES (?1, ?2)")
            .bind(id)
            .bind(email)
            .execute(db.writer())
            .await
            .unwrap();
        id
    }

    async fn team(db: &Db, name: &str, members: &[Uuid]) -> Uuid {
        let id = Uuid::now_v7();
        sqlx::query("INSERT INTO teams (id, name) VALUES (?1, ?2)")
            .bind(id)
            .bind(name)
            .execute(db.writer())
            .await
            .unwrap();
        for (i, m) in members.iter().enumerate() {
            sqlx::query(
                "INSERT INTO team_members (team_id, user_id, joined_at) VALUES (?1, ?2, ?3)",
            )
            .bind(id)
            .bind(m)
            .bind(format!("2026-01-0{}T00:00:00.000Z", i + 1))
            .execute(db.writer())
            .await
            .unwrap();
        }
        id
    }

    const NONE: Env = Env { values: Vec::new() };

    /// Each mode: not set (the built-in default), a default the user
    /// changes, a lock nobody below changes, a range that holds the values
    /// below, and the environment over everything.
    #[tokio::test]
    async fn each_mode_and_the_environment() {
        let db = db().await;
        let alice = user(&db, "alice@example.com").await;
        let key = "retention.trash_days";
        let get = |db: Db| async move { resolve(&db, &NONE, key, For::user(alice)).await.unwrap() };
        let r = get(db.clone()).await;
        assert_eq!(
            (r.value.clone(), r.source.level.as_str()),
            (Value::from(30), "builtin")
        );
        put(
            &db,
            "superadmin",
            None,
            key,
            Some("10"),
            "default",
            None,
            None,
        )
        .await;
        assert_eq!(get(db.clone()).await.value, Value::from(10));
        put(
            &db,
            "user",
            Some(alice),
            key,
            Some("3"),
            "default",
            None,
            None,
        )
        .await;
        let r = get(db.clone()).await;
        assert_eq!(
            (r.value.clone(), r.source.level.as_str()),
            (Value::from(3), "user")
        );
        assert!(!r.locked());
        // A floor above: the user's 3 is held to 7.
        sqlx::query("UPDATE settings SET min = 7 WHERE level = 'superadmin'")
            .execute(db.writer())
            .await
            .unwrap();
        let r = get(db.clone()).await;
        assert_eq!((r.value.clone(), r.min), (Value::from(7), Some(7.0)));
        // Locked: the user's value no longer counts.
        sqlx::query("UPDATE settings SET mode = 'locked' WHERE level = 'superadmin'")
            .execute(db.writer())
            .await
            .unwrap();
        let r = get(db.clone()).await;
        assert_eq!(r.value, Value::from(10));
        assert_eq!(r.locked_by.unwrap().level, "superadmin");
        // A range alone: the built-in default within it.
        sqlx::query("DELETE FROM settings")
            .execute(db.writer())
            .await
            .unwrap();
        put(
            &db,
            "superadmin",
            None,
            key,
            None,
            "range",
            Some(40.0),
            Some(90.0),
        )
        .await;
        assert_eq!(get(db.clone()).await.value, Value::from(40));
        // The environment locks it over everything.
        let env = Env {
            values: vec![(key, Value::from(5))],
        };
        let r = resolve(&db, &env, key, For::user(alice)).await.unwrap();
        assert_eq!(
            (r.value, r.locked_by.unwrap().level),
            (Value::from(5), "env".to_string())
        );
    }

    /// Several teams: a drive's settings by the drive's team, sign-in the
    /// strictest, one's own the loosest, defaults from the primary team.
    #[tokio::test]
    async fn several_teams() {
        let db = db().await;
        let alice = user(&db, "alice@example.com").await;
        let strict = team(&db, "Strict", &[alice]).await;
        let lax = team(&db, "Lax", &[alice]).await;
        // Sign-in: the strict team's lock holds, whatever the lax one says.
        put(
            &db,
            "team",
            Some(strict),
            "sign_in.two_factor_required",
            Some("true"),
            "locked",
            None,
            None,
        )
        .await;
        put(
            &db,
            "team",
            Some(lax),
            "sign_in.two_factor_required",
            Some("false"),
            "default",
            None,
            None,
        )
        .await;
        let r = resolve(&db, &NONE, "sign_in.two_factor_required", For::user(alice))
            .await
            .unwrap();
        assert!(r.bool());
        assert_eq!(r.locked_by.unwrap().team_name.as_deref(), Some("Strict"));
        // Their own: locked only when both lock it.
        put(
            &db,
            "team",
            Some(strict),
            "notifications.shared",
            Some("false"),
            "locked",
            None,
            None,
        )
        .await;
        let r = resolve(&db, &NONE, "notifications.shared", For::user(alice))
            .await
            .unwrap();
        assert!(!r.locked());
        put(
            &db,
            "team",
            Some(lax),
            "notifications.shared",
            Some("true"),
            "locked",
            None,
            None,
        )
        .await;
        let r = resolve(&db, &NONE, "notifications.shared", For::user(alice))
            .await
            .unwrap();
        assert!(r.locked());
        // Ranges: as wide as the widest.
        put(
            &db,
            "team",
            Some(strict),
            "retention.trash_days",
            None,
            "range",
            Some(30.0),
            None,
        )
        .await;
        put(
            &db,
            "team",
            Some(lax),
            "retention.trash_days",
            None,
            "range",
            Some(7.0),
            None,
        )
        .await;
        let r = resolve(&db, &NONE, "retention.trash_days", For::user(alice))
            .await
            .unwrap();
        assert_eq!(r.min, Some(7.0));
        // Defaults from the primary team: the first joined, or the chosen.
        put(
            &db,
            "team",
            Some(strict),
            "drop_box.read_contents",
            Some("\"per_item\""),
            "default",
            None,
            None,
        )
        .await;
        put(
            &db,
            "team",
            Some(lax),
            "drop_box.read_contents",
            Some("\"default_on\""),
            "default",
            None,
            None,
        )
        .await;
        let r = resolve(&db, &NONE, "drop_box.read_contents", For::user(alice))
            .await
            .unwrap();
        assert_eq!(r.str(), Some("per_item"));
        sqlx::query("UPDATE users SET primary_team_id = ?1 WHERE id = ?2")
            .bind(lax)
            .bind(alice)
            .execute(db.writer())
            .await
            .unwrap();
        let r = resolve(&db, &NONE, "drop_box.read_contents", For::user(alice))
            .await
            .unwrap();
        assert_eq!(r.str(), Some("default_on"));
        assert_eq!(r.source.team_name.as_deref(), Some("Lax"));
        // A shared drive's settings: its team's, whoever works there.
        let drive = Uuid::now_v7();
        sqlx::query("INSERT INTO drives (id, kind, name, team_id) VALUES (?1, 'shared', 'D', ?2)")
            .bind(drive)
            .bind(strict)
            .execute(db.writer())
            .await
            .unwrap();
        put(
            &db,
            "team",
            Some(strict),
            "links.allowed",
            Some("false"),
            "locked",
            None,
            None,
        )
        .await;
        let r = resolve(
            &db,
            &NONE,
            "links.allowed",
            For {
                user: Some(alice),
                drive: Some(drive),
            },
        )
        .await
        .unwrap();
        assert!(!r.bool());
        // Her My drive follows her teams: the lax one leaves links open.
        let r = resolve(
            &db,
            &NONE,
            "links.allowed",
            For {
                user: Some(alice),
                drive: Some(alice),
            },
        )
        .await
        .unwrap();
        assert!(r.bool() && !r.locked());
    }
}
