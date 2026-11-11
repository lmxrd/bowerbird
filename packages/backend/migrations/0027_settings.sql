-- Settings levels (docs/M7B_DESIGN.md §4): one table for every setting
-- with levels, read through one resolver (src/settings.rs) that walks the
-- environment > superadmin > team > user / drive. At each level a setting
-- is locked (fixed for everyone below), a default (those below may change
-- it), a range (those below choose within it), or not set (no row).
CREATE TABLE settings (
  level TEXT NOT NULL CHECK (level IN ('superadmin', 'team', 'user', 'drive')),
  -- The team's, user's or drive's id; NULL for the superadmin's.
  level_id BLOB,
  key TEXT NOT NULL,
  -- JSON; NULL for a range alone.
  value TEXT,
  mode TEXT NOT NULL CHECK (mode IN ('locked', 'default', 'range')),
  -- A number's range for the levels below (at least, at most).
  min REAL,
  max REAL,
  updated_at TEXT NOT NULL DEFAULT (strftime('%Y-%m-%dT%H:%M:%fZ', 'now')),
  CHECK ((level = 'superadmin') = (level_id IS NULL))
);
CREATE UNIQUE INDEX settings_at ON settings (level, ifnull(level_id, x''), key);

-- A level's rows go with it (level_id is no foreign key: it names one of
-- three tables).
CREATE TRIGGER settings_of_user AFTER DELETE ON users
BEGIN
  DELETE FROM settings WHERE level = 'user' AND level_id = OLD.id;
END;
CREATE TRIGGER settings_of_team AFTER DELETE ON teams
BEGIN
  DELETE FROM settings WHERE level = 'team' AND level_id = OLD.id;
END;
CREATE TRIGGER settings_of_drive AFTER DELETE ON drives
BEGIN
  DELETE FROM settings WHERE level = 'drive' AND level_id = OLD.id;
END;

-- Each user's drop box settings, as user rows: what differs from the
-- built-in defaults.
INSERT INTO settings (level, level_id, key, value, mode)
  SELECT 'user', user_id, 'drop_box.pending_mode', json_quote(pending_mode), 'default'
  FROM drop_box_settings WHERE pending_mode != 'move_pending';
INSERT INTO settings (level, level_id, key, value, mode)
  SELECT 'user', user_id, 'drop_box.accept_after_hours', CAST(accept_after_hours AS TEXT), 'default'
  FROM drop_box_settings WHERE accept_after_hours IS NOT NULL AND accept_after_hours != 24;
INSERT INTO settings (level, level_id, key, value, mode)
  SELECT 'user', user_id, 'drop_box.confidence_threshold', CAST(confidence_threshold AS TEXT), 'default'
  FROM drop_box_settings WHERE confidence_threshold IS NOT NULL AND confidence_threshold != 0.8;
INSERT INTO settings (level, level_id, key, value, mode)
  SELECT 'user', user_id, 'drop_box.read_contents', json_quote(read_contents), 'default'
  FROM drop_box_settings WHERE read_contents != 'always';
INSERT INTO settings (level, level_id, key, value, mode)
  SELECT 'user', user_id, 'drop_box.folder_freedom', json_quote(folder_freedom), 'default'
  FROM drop_box_settings WHERE folder_freedom != 'balanced';
INSERT INTO settings (level, level_id, key, value, mode)
  SELECT 'user', user_id, 'drop_box.allowed_top_folders', allowed_top_folders, 'default'
  FROM drop_box_settings WHERE allowed_top_folders IS NOT NULL;
INSERT INTO settings (level, level_id, key, value, mode)
  SELECT 'user', user_id, 'drop_box.folders', json_quote(folders), 'default'
  FROM drop_box_settings WHERE folders != 'whole';
INSERT INTO settings (level, level_id, key, value, mode)
  SELECT 'user', user_id, 'drop_box.links', json_quote(links), 'default'
  FROM drop_box_settings WHERE links != 'snapshot';
DROP TABLE drop_box_settings;

-- The trash and version rules: the superadmin's defaults (mode default),
-- each user's own.
INSERT INTO settings (level, key, value, mode)
  SELECT 'superadmin', 'retention.trash_days', CAST(trash_days AS TEXT), 'default'
  FROM retention_defaults WHERE trash_days != 30;
INSERT INTO settings (level, key, value, mode)
  SELECT 'superadmin', 'retention.versions_hours', CAST(versions_hours AS TEXT), 'default'
  FROM retention_defaults WHERE versions_hours != 24;
INSERT INTO settings (level, key, value, mode)
  SELECT 'superadmin', 'retention.versions_days', CAST(versions_days AS TEXT), 'default'
  FROM retention_defaults WHERE versions_days != 30;
INSERT INTO settings (level, key, value, mode)
  SELECT 'superadmin', 'retention.versions_months', CAST(versions_months AS TEXT), 'default'
  FROM retention_defaults WHERE versions_months != 12;
INSERT INTO settings (level, level_id, key, value, mode)
  SELECT 'user', user_id, 'retention.trash_days', CAST(trash_days AS TEXT), 'default'
  FROM retention WHERE trash_days IS NOT NULL;
INSERT INTO settings (level, level_id, key, value, mode)
  SELECT 'user', user_id, 'retention.versions_hours', CAST(versions_hours AS TEXT), 'default'
  FROM retention WHERE versions_hours IS NOT NULL;
INSERT INTO settings (level, level_id, key, value, mode)
  SELECT 'user', user_id, 'retention.versions_days', CAST(versions_days AS TEXT), 'default'
  FROM retention WHERE versions_days IS NOT NULL;
INSERT INTO settings (level, level_id, key, value, mode)
  SELECT 'user', user_id, 'retention.versions_months', CAST(versions_months AS TEXT), 'default'
  FROM retention WHERE versions_months IS NOT NULL;
DROP TABLE retention;
DROP TABLE retention_defaults;

-- What each user is told about: the switches they turned off. The table
-- keeps only when the last notice of items to review went.
INSERT INTO settings (level, level_id, key, value, mode)
  SELECT 'user', user_id, 'notifications.review', 'false', 'default'
  FROM notification_settings WHERE review = 0;
INSERT INTO settings (level, level_id, key, value, mode)
  SELECT 'user', user_id, 'notifications.shared', 'false', 'default'
  FROM notification_settings WHERE shared = 0;
INSERT INTO settings (level, level_id, key, value, mode)
  SELECT 'user', user_id, 'notifications.backup', 'false', 'default'
  FROM notification_settings WHERE backup = 0;
ALTER TABLE notification_settings DROP COLUMN review;
ALTER TABLE notification_settings DROP COLUMN shared;
ALTER TABLE notification_settings DROP COLUMN backup;

-- What the superadmin (or the server's command line) did to an account
-- (§5), which its user sees in Settings → Security → Account activity.
CREATE TABLE activity (
  id BLOB PRIMARY KEY NOT NULL CHECK (length(id) = 16),
  -- Whose account.
  user_id BLOB NOT NULL REFERENCES users(id) ON DELETE CASCADE,
  at TEXT NOT NULL,
  -- 'superadmin' (actor_id: who) or 'command_line'.
  actor TEXT NOT NULL CHECK (actor IN ('superadmin', 'command_line')),
  actor_id BLOB REFERENCES users(id) ON DELETE SET NULL,
  action TEXT NOT NULL,
  -- JSON: what it was about (a team's or drive's name, a role, a limit).
  detail TEXT NOT NULL DEFAULT '{}'
);
CREATE INDEX activity_user ON activity (user_id, at);
