//! How long the trash and the versions are kept (docs/M3_DESIGN.md §1.1):
//! the `retention.*` settings (docs/M7B_DESIGN.md §4), with the levels
//! superadmin, team, user and drive, and the `TRASH_DAYS` and `VERSIONS_*`
//! variables over all, which lock a rule for everyone. [`sweep`] applies
//! them, hourly: My drive by its user's rules, a shared drive by its own
//! (and its team's).

use crate::db::Db;
use crate::files::Files;
use crate::state::AppState;
use serde::{Deserialize, Serialize};
use std::time::Duration;
use utoipa::ToSchema;
use uuid::Uuid;

/// The rules in effect.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize, ToSchema, sqlx::FromRow)]
#[schema(as = RetentionRules)]
pub struct Rules {
    /// Trashed items are deleted for good this many days after; 0 keeps
    /// them until the trash is emptied.
    pub trash_days: i64,
    /// Every version is kept for this many hours.
    pub versions_hours: i64,
    /// Then the last of each day, for this many days.
    pub versions_days: i64,
    /// Then the last of each month, for this many months. All three 0: no
    /// versions are kept.
    pub versions_months: i64,
}

impl Default for Rules {
    fn default() -> Self {
        Rules {
            trash_days: 30,
            versions_hours: 24,
            versions_days: 30,
            versions_months: 12,
        }
    }
}

impl Rules {
    /// Whether versions are kept at all.
    pub fn versions(&self) -> bool {
        self.versions_hours > 0 || self.versions_days > 0 || self.versions_months > 0
    }

    /// A rule that cannot be: negative, or past ten years.
    pub fn validate(&self) -> Result<(), String> {
        let limits = [
            ("trash_days", self.trash_days, 3_650),
            ("versions_hours", self.versions_hours, 87_600),
            ("versions_days", self.versions_days, 3_650),
            ("versions_months", self.versions_months, 120),
        ];
        for (field, value, max) in limits {
            if !(0..=max).contains(&value) {
                return Err(format!("{field} must be between 0 and {max}"));
            }
        }
        Ok(())
    }
}

/// A partial set of rules: a user's changes (NULL is the default), or the
/// variables.
#[derive(
    Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize, ToSchema, sqlx::FromRow,
)]
#[schema(as = RetentionChanges)]
pub struct Partial {
    pub trash_days: Option<i64>,
    pub versions_hours: Option<i64>,
    pub versions_days: Option<i64>,
    pub versions_months: Option<i64>,
}

impl Partial {
    /// These over `rules`.
    pub fn over(&self, rules: Rules) -> Rules {
        Rules {
            trash_days: self.trash_days.unwrap_or(rules.trash_days),
            versions_hours: self.versions_hours.unwrap_or(rules.versions_hours),
            versions_days: self.versions_days.unwrap_or(rules.versions_days),
            versions_months: self.versions_months.unwrap_or(rules.versions_months),
        }
    }

    /// The fields that are set.
    pub fn fields(&self) -> Vec<&'static str> {
        [
            ("trash_days", self.trash_days),
            ("versions_hours", self.versions_hours),
            ("versions_days", self.versions_days),
            ("versions_months", self.versions_months),
        ]
        .into_iter()
        .filter_map(|(f, v)| v.map(|_| f))
        .collect()
    }

    /// The `TRASH_DAYS` and `VERSIONS_*` variables. A value that is not a
    /// number in range stops the start.
    pub fn from_env() -> Result<Self, String> {
        let number = |name: &str| -> Result<Option<i64>, String> {
            crate::config::var(name)
                .map(|v| {
                    v.parse()
                        .map_err(|_| format!("{name} must be a whole number, not `{v}`"))
                })
                .transpose()
        };
        let env = Partial {
            trash_days: number("TRASH_DAYS")?,
            versions_hours: number("VERSIONS_HOURS")?,
            versions_days: number("VERSIONS_DAYS")?,
            versions_months: number("VERSIONS_MONTHS")?,
        };
        env.over(Rules::default())
            .validate()
            .map_err(|e| format!("{}: {e}", variable(env.fields().first().unwrap_or(&""))))?;
        Ok(env)
    }
}

/// The variable that locks a field.
pub fn variable(field: &str) -> String {
    field.to_uppercase()
}

/// The settings of the four rules.
pub const KEYS: [&str; 4] = [
    "retention.trash_days",
    "retention.versions_hours",
    "retention.versions_days",
    "retention.versions_months",
];

/// The rules as the settings resolve them (`upto`: only the levels above
/// that one).
async fn resolved(
    files: &Files,
    to: crate::settings::For,
    upto: Option<crate::settings::Level>,
) -> Result<Rules, sqlx::Error> {
    let env = crate::settings::Env::of_files(files);
    let mut n = [0i64; 4];
    for (i, key) in KEYS.iter().enumerate() {
        let def = crate::settings::def(key).expect("a retention setting");
        n[i] = crate::settings::resolve_def(files.db(), &env, def, to, upto)
            .await?
            .int();
    }
    Ok(Rules {
        trash_days: n[0],
        versions_hours: n[1],
        versions_days: n[2],
        versions_months: n[3],
    })
}

/// The superadmin's defaults, under the variables.
pub async fn defaults(files: &Files) -> Result<Rules, sqlx::Error> {
    resolved(
        files,
        crate::settings::For::default(),
        Some(crate::settings::Level::Team),
    )
    .await
}

/// What a user has under their own rules: the variables, the superadmin's
/// and their teams'.
pub async fn above_user(files: &Files, user: Uuid) -> Result<Rules, sqlx::Error> {
    resolved(
        files,
        crate::settings::For::user(user),
        Some(crate::settings::Level::User),
    )
    .await
}

/// A user's own rules (their level; None: not set).
pub async fn changes(db: &Db, user: Uuid) -> Result<Partial, sqlx::Error> {
    let mut out = [None; 4];
    for (i, key) in KEYS.iter().enumerate() {
        out[i] = crate::settings::row(db, crate::settings::Level::User, Some(user), key)
            .await?
            .and_then(|r| r.value)
            .and_then(|v| v.parse().ok());
    }
    Ok(Partial {
        trash_days: out[0],
        versions_hours: out[1],
        versions_days: out[2],
        versions_months: out[3],
    })
}

/// The rules in effect for a drive: My drive by its user's (the drive's id
/// is theirs), a shared drive by its own and its team's, a mirror by its
/// user's (docs/M7D_DESIGN.md §1).
pub async fn rules(files: &Files, drive: Uuid) -> Result<Rules, sqlx::Error> {
    let mirrored_by: Option<Uuid> = sqlx::query_scalar("SELECT user_id FROM mirrors WHERE id = ?1")
        .bind(drive)
        .fetch_optional(files.db().reader())
        .await?;
    let to = match mirrored_by {
        Some(user) => crate::settings::For::user(user),
        None => crate::settings::For::drive(drive),
    };
    resolved(files, to, None).await
}

/// Which of a file's versions go, by their age: every version younger
/// than `versions_hours`, then the newest of each day younger than
/// `versions_days`, then the newest of each month younger than
/// `versions_months`, stay. `versions` is (id, created_at); `now` is when
/// the rules are applied.
pub fn thin(
    rules: &Rules,
    versions: &[(Uuid, jiff::Timestamp)],
    now: jiff::Timestamp,
) -> Vec<Uuid> {
    let hours = Duration::from_secs(rules.versions_hours.max(0) as u64 * 3_600);
    let days = Duration::from_secs(rules.versions_days.max(0) as u64 * 86_400);
    // A month is 30 days here: the age limit only, the buckets are
    // calendar months.
    let months = Duration::from_secs(rules.versions_months.max(0) as u64 * 30 * 86_400);
    let mut sorted: Vec<_> = versions.to_vec();
    sorted.sort_by_key(|v| std::cmp::Reverse(v.1));
    let mut days_seen = std::collections::HashSet::new();
    let mut months_seen = std::collections::HashSet::new();
    let mut gone = Vec::new();
    for (id, at) in sorted {
        let age = Duration::try_from(now.duration_since(at)).unwrap_or(Duration::ZERO);
        let day = at.strftime("%Y-%m-%d").to_string();
        let month = at.strftime("%Y-%m").to_string();
        let keep = if age < hours {
            true
        } else if age < days {
            !days_seen.contains(&day)
        } else if age < months {
            !months_seen.contains(&month)
        } else {
            false
        };
        // A day or month counts as kept once a version of it is kept, by
        // whichever rule kept it.
        if keep {
            days_seen.insert(day);
            months_seen.insert(month);
        } else {
            gone.push(id);
        }
    }
    gone
}

/// Apply every user's rules once: trash past its days goes for good,
/// versions are thinned, and what nothing refers to any more goes.
pub async fn sweep(state: &AppState) -> anyhow::Result<()> {
    // Per drive: My drive by its user's rules (the drive's id is theirs),
    // a shared drive by the defaults.
    // Not another server's shares: they keep their own trash and versions.
    let drives: Vec<Uuid> = sqlx::query_scalar("SELECT id FROM drives WHERE kind != 'remote'")
        .fetch_all(state.db.reader())
        .await?;
    let now = jiff::Timestamp::now();
    for user in drives {
        let rules = rules(&state.files, user).await?;
        if rules.trash_days > 0 {
            let before = crate::auth::iso(
                (now - jiff::SignedDuration::from_hours(24 * rules.trash_days)).into(),
            );
            let old: Vec<Uuid> =
                sqlx::query_scalar("SELECT id FROM trash WHERE drive_id = ?1 AND deleted_at < ?2")
                    .bind(user)
                    .bind(&before)
                    .fetch_all(state.db.reader())
                    .await?;
            for id in old {
                if let Err(e) = state.files.purge(user, id).await {
                    tracing::warn!("cannot delete trash item {id} for good: {e}");
                }
            }
        }
        state.files.thin_versions(user, &rules, now).await?;
    }
    // The keys of encrypted folders gone for good (a removed user's too).
    crate::files::e2e::forget_gone(&state.db).await?;
    Ok(())
}

/// [`sweep`] every hour, from start.
pub async fn run(state: AppState) {
    loop {
        if let Err(e) = sweep(&state).await {
            tracing::warn!("cannot apply the trash and version rules: {e}");
        }
        tokio::time::sleep(Duration::from_secs(3_600)).await;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn at(s: &str) -> jiff::Timestamp {
        s.parse().unwrap()
    }

    #[test]
    fn versions_thin_out_with_age() {
        let now = at("2026-11-07T12:00:00Z");
        let ids: Vec<Uuid> = (0..8).map(|_| Uuid::now_v7()).collect();
        let versions = vec![
            (ids[0], at("2026-11-07T11:00:00Z")), // an hour: kept
            (ids[1], at("2026-11-07T01:00:00Z")), // 11 hours: kept
            (ids[2], at("2026-11-05T18:00:00Z")), // the newest of Nov 5: kept
            (ids[3], at("2026-11-05T09:00:00Z")), // same day: goes
            (ids[4], at("2026-10-20T09:00:00Z")), // the newest of Oct 20: kept
            (ids[5], at("2026-09-02T09:00:00Z")), // the newest of September: kept
            (ids[6], at("2026-09-01T09:00:00Z")), // same month: goes
            (ids[7], at("2025-01-01T09:00:00Z")), // past 12 months: goes
        ];
        let mut gone = thin(&Rules::default(), &versions, now);
        gone.sort();
        let mut want = vec![ids[3], ids[6], ids[7]];
        want.sort();
        assert_eq!(gone, want);

        let off = Rules {
            versions_hours: 0,
            versions_days: 0,
            versions_months: 0,
            ..Rules::default()
        };
        assert!(!off.versions());
        assert_eq!(thin(&off, &versions, now).len(), versions.len());
    }

    #[test]
    fn a_day_kept_by_the_hours_rule_is_not_kept_twice() {
        let now = at("2026-11-07T12:00:00Z");
        let rules = Rules {
            versions_hours: 6,
            ..Rules::default()
        };
        let (a, b) = (Uuid::now_v7(), Uuid::now_v7());
        // a is within the hours; b is older the same day: a is that day's.
        let versions = [
            (a, at("2026-11-07T10:00:00Z")),
            (b, at("2026-11-07T01:00:00Z")),
        ];
        assert_eq!(thin(&rules, &versions, now), [b]);
    }

    #[test]
    fn variables_lock_and_are_checked() {
        let env = Partial {
            trash_days: Some(7),
            ..Partial::default()
        };
        assert_eq!(env.fields(), ["trash_days"]);
        assert_eq!(env.over(Rules::default()).trash_days, 7);
        assert_eq!(variable("versions_days"), "VERSIONS_DAYS");
        assert!(
            Rules {
                trash_days: -1,
                ..Rules::default()
            }
            .validate()
            .is_err()
        );
    }
}
