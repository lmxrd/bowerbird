-- no-transaction
-- Mirrors (docs/M7D_DESIGN.md): a one-way, read-only copy of a drive in
-- one of the user's own storage backends. A mirror is a drive of its own
-- (`kind = 'mirror'`, its id the mirror's): its tree, trash and versions at
-- `<prefix>/<mirror id>/` of the backend, its user a viewer of it. So a
-- mirror ejected in place is that same drive, made a shared one.
--
-- `drives.kind` gains 'mirror': SQLite cannot change a CHECK, so the table
-- is made again, with the foreign keys off (a dropped table would take the
-- rows that refer to it with it), which is why this migration runs outside
-- sqlx's transaction and opens its own. The view and the trigger that name
-- the table are made again with it.
PRAGMA foreign_keys = OFF;
BEGIN;

DROP VIEW drive_users;
DROP TRIGGER users_my_drive;
DROP TRIGGER settings_of_drive;

CREATE TABLE drives_new (
  id BLOB PRIMARY KEY NOT NULL CHECK (length(id) = 16),
  kind TEXT NOT NULL CHECK (kind IN ('my', 'shared', 'mirror')),
  -- My drive's user (its id is theirs too); NULL for the others.
  user_id BLOB UNIQUE REFERENCES users(id) ON DELETE CASCADE,
  -- A shared drive's name; a mirror's is its drive's (as last copied); My
  -- drive has none (the apps say "My drive").
  name TEXT NOT NULL DEFAULT '',
  root TEXT,
  storage_limit INTEGER,
  created_by BLOB REFERENCES users(id) ON DELETE SET NULL,
  created_at TEXT NOT NULL DEFAULT (strftime('%Y-%m-%dT%H:%M:%fZ', 'now')),
  team_id BLOB REFERENCES teams(id) ON DELETE SET NULL,
  backend_id BLOB REFERENCES backends(id),
  CHECK ((kind = 'my') = (user_id IS NOT NULL)),
  -- A mirror is always in a backend (a user's own).
  CHECK (kind != 'mirror' OR backend_id IS NOT NULL)
);
INSERT INTO drives_new (id, kind, user_id, name, root, storage_limit, created_by, created_at,
                        team_id, backend_id)
  SELECT id, kind, user_id, name, root, storage_limit, created_by, created_at, team_id, backend_id
  FROM drives;
DROP TABLE drives;
ALTER TABLE drives_new RENAME TO drives;
CREATE UNIQUE INDEX drives_shared_name ON drives (name COLLATE NOCASE) WHERE kind = 'shared';
CREATE INDEX drives_team ON drives (team_id) WHERE team_id IS NOT NULL;
CREATE INDEX drives_backend ON drives (backend_id);

CREATE TRIGGER users_my_drive AFTER INSERT ON users
BEGIN
  INSERT INTO drives (id, kind, user_id, created_by) VALUES (NEW.id, 'my', NEW.id, NEW.id);
END;
CREATE TRIGGER settings_of_drive AFTER DELETE ON drives
BEGIN
  DELETE FROM settings WHERE level = 'drive' AND level_id = OLD.id;
END;
CREATE VIEW drive_users (drive_id, user_id, role) AS
  SELECT id, user_id, 'owner' FROM drives WHERE user_id IS NOT NULL
  UNION ALL
  SELECT drive_id, user_id, role FROM grants WHERE node_id IS NULL AND user_id IS NOT NULL
  UNION ALL
  SELECT g.drive_id, m.user_id, g.role FROM grants g JOIN team_members m ON m.team_id = g.team_id
  WHERE g.node_id IS NULL
  UNION ALL
  SELECT d.id, m.user_id, 'manager' FROM drives d
  JOIN team_members m ON m.team_id = d.team_id AND m.role = 'manager';

-- A mirror: its drive (the copy), whose it is, and the drive it copies.
CREATE TABLE mirrors (
  id BLOB PRIMARY KEY NOT NULL REFERENCES drives(id) ON DELETE CASCADE,
  user_id BLOB NOT NULL REFERENCES users(id) ON DELETE CASCADE,
  -- The drive it copies; NULL once that drive is deleted (the mirror
  -- stays, stopped).
  source_id BLOB REFERENCES drives(id) ON DELETE SET NULL,
  -- When it copies: within about a minute of a change, hourly, daily, or
  -- only when asked.
  schedule TEXT NOT NULL DEFAULT 'continuous'
    CHECK (schedule IN ('continuous', 'hourly', 'daily', 'manual')),
  -- The last row of the user's change feed about the drive that the last
  -- complete copy took in: a later one means something changed since.
  seen_seq INTEGER NOT NULL DEFAULT 0,
  -- The last copy left something for the next (a file not yet hashed, one
  -- that changed while it was copied).
  again INTEGER NOT NULL DEFAULT 0,
  -- "Mirror now": copied at the next chance, whatever the schedule.
  requested INTEGER NOT NULL DEFAULT 0,
  -- A copy under way: one a stop left is finished at the next start.
  running_since TEXT,
  last_try_at TEXT,
  -- The end of the last complete copy.
  last_copy_at TEXT,
  -- Why the last try failed, fit to show; NULL when it went through.
  error TEXT,
  created_at TEXT NOT NULL DEFAULT (strftime('%Y-%m-%dT%H:%M:%fZ', 'now'))
);
CREATE INDEX mirrors_user ON mirrors (user_id);
CREATE INDEX mirrors_source ON mirrors (source_id);

-- In a mirror (and a drive made from one): the node of the drive mirrored
-- that this one is a copy of. Not a foreign key: it outlives that node.
ALTER TABLE nodes ADD COLUMN source_id BLOB;
CREATE UNIQUE INDEX nodes_source ON nodes (drive_id, source_id) WHERE source_id IS NOT NULL;

-- Whether a user's feed has rows about a drive since a point.
CREATE INDEX changes_user_drive ON changes (user_id, drive_id, seq);

COMMIT;
PRAGMA foreign_keys = ON;
