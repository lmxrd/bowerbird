-- Backups (docs/M3_DESIGN.md §3): where and when, set by an admin with
-- BACKUP_* variables on top (src/backup/mod.rs), and how the last ones went.

CREATE TABLE backup_settings (
  id INTEGER PRIMARY KEY CHECK (id = 1),
  -- A path (another disk, a mounted share) or s3://bucket/path. Empty: no
  -- backups.
  repository TEXT NOT NULL DEFAULT '',
  s3_endpoint TEXT NOT NULL DEFAULT '',
  s3_region TEXT NOT NULL DEFAULT '',
  s3_access_key TEXT NOT NULL DEFAULT '',
  -- Never sent back, as the password.
  s3_secret_key TEXT NOT NULL DEFAULT '',
  -- The repository's key: without it the backups cannot be read.
  password TEXT NOT NULL DEFAULT '',
  -- `daily HH:MM` or `every Nh`.
  schedule TEXT NOT NULL DEFAULT 'daily 03:00',
  keep_daily INTEGER NOT NULL DEFAULT 7,
  keep_weekly INTEGER NOT NULL DEFAULT 4,
  keep_monthly INTEGER NOT NULL DEFAULT 12
);
INSERT INTO backup_settings (id) VALUES (1);

CREATE TABLE backup_status (
  id INTEGER PRIMARY KEY CHECK (id = 1),
  last_started TEXT,
  last_finished TEXT,
  -- Set when the last run failed; cleared by one that did not.
  last_error TEXT,
  -- The last snapshot's id, how much was new in it, and in all.
  last_snapshot TEXT,
  last_added INTEGER,
  last_total INTEGER,
  snapshots INTEGER,
  -- The last check of the repository's structure (weekly).
  last_check TEXT
);
INSERT INTO backup_status (id) VALUES (1);
