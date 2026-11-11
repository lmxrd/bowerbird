-- Roles and teams (docs/M7B_DESIGN.md §1 to §3, §8): one model for every
-- drive and every item in it. A grant gives a user or a team a role on an
-- item or on a whole (shared) drive; M6b's shares and M7a's drive members
-- become grants, M6b's groups become teams.

-- Teams, with their members (the groups' ids are kept, so the shares made
-- to a group are the grants of its team).
CREATE TABLE teams (
  id BLOB PRIMARY KEY NOT NULL CHECK (length(id) = 16),
  name TEXT NOT NULL UNIQUE COLLATE NOCASE,
  -- Bytes, for all the team's drives together; NULL: no limit.
  storage_limit INTEGER,
  created_at TEXT NOT NULL DEFAULT (strftime('%Y-%m-%dT%H:%M:%fZ', 'now'))
);
INSERT INTO teams (id, name, created_at) SELECT id, name, created_at FROM groups;

-- A team's members; a manager adds and removes members and managers, makes
-- the team's shared drives and sets the team's settings.
CREATE TABLE team_members (
  team_id BLOB NOT NULL REFERENCES teams(id) ON DELETE CASCADE,
  user_id BLOB NOT NULL REFERENCES users(id) ON DELETE CASCADE,
  role TEXT NOT NULL DEFAULT 'member' CHECK (role IN ('manager', 'member')),
  joined_at TEXT NOT NULL DEFAULT (strftime('%Y-%m-%dT%H:%M:%fZ', 'now')),
  PRIMARY KEY (team_id, user_id)
);
CREATE INDEX team_members_user ON team_members (user_id);
-- Until the superadmin names one, a team has no manager of its own.
INSERT INTO team_members (team_id, user_id, role) SELECT group_id, user_id, 'member' FROM group_members;

-- The team whose defaults a user gets (§3): the first they joined until
-- they choose another of theirs. NULL: the first they joined.
ALTER TABLE users ADD COLUMN primary_team_id BLOB REFERENCES teams(id) ON DELETE SET NULL;

-- A shared drive may belong to a team: the team's managers manage it, and
-- the team's members get the role of the team's grant on it.
ALTER TABLE drives ADD COLUMN team_id BLOB REFERENCES teams(id) ON DELETE SET NULL;
CREATE INDEX drives_team ON drives (team_id) WHERE team_id IS NOT NULL;

-- Who added a node through Organizer: an upload-only user sees only theirs.
-- What exists is its drive's user's (none for a shared drive's).
ALTER TABLE nodes ADD COLUMN created_by BLOB REFERENCES users(id) ON DELETE SET NULL;
UPDATE nodes SET created_by = (SELECT user_id FROM drives WHERE drives.id = nodes.drive_id);
-- Limited access: from this folder down only the grants made on it or
-- below count (the owner and the drive's managers keep theirs).
ALTER TABLE nodes ADD COLUMN limited INTEGER NOT NULL DEFAULT 0;
-- Whether a drive hides anything is asked with every change of it.
CREATE INDEX nodes_limited ON nodes (drive_id) WHERE limited = 1;

-- The grants: (item or drive, user or team, role). `node_id` NULL is a
-- grant on the whole drive (shared drives only). An `owner` grant is on an
-- item of a My drive whose owner was changed (§1), to a user.
-- `created_by` is who gave it: a grant given by someone who is neither the
-- owner nor a manager of the drive gives no more than its giver still has.
CREATE TABLE grants (
  id BLOB PRIMARY KEY NOT NULL CHECK (length(id) = 16),
  drive_id BLOB NOT NULL REFERENCES drives(id) ON DELETE CASCADE,
  node_id BLOB REFERENCES nodes(id) ON DELETE CASCADE,
  user_id BLOB REFERENCES users(id) ON DELETE CASCADE,
  team_id BLOB REFERENCES teams(id) ON DELETE CASCADE,
  role TEXT NOT NULL CHECK (role IN ('owner', 'manager', 'editor', 'viewer', 'upload_only')),
  created_by BLOB REFERENCES users(id) ON DELETE SET NULL,
  created_at TEXT NOT NULL DEFAULT (strftime('%Y-%m-%dT%H:%M:%fZ', 'now')),
  CHECK ((user_id IS NULL) != (team_id IS NULL)),
  CHECK (role != 'owner' OR (node_id IS NOT NULL AND user_id IS NOT NULL))
);
CREATE UNIQUE INDEX grants_node_user ON grants (node_id, user_id)
  WHERE node_id IS NOT NULL AND user_id IS NOT NULL;
CREATE UNIQUE INDEX grants_node_team ON grants (node_id, team_id)
  WHERE node_id IS NOT NULL AND team_id IS NOT NULL;
CREATE UNIQUE INDEX grants_drive_user ON grants (drive_id, user_id)
  WHERE node_id IS NULL AND user_id IS NOT NULL;
CREATE UNIQUE INDEX grants_drive_team ON grants (drive_id, team_id)
  WHERE node_id IS NULL AND team_id IS NOT NULL;
CREATE INDEX grants_drive ON grants (drive_id);
CREATE INDEX grants_user ON grants (user_id) WHERE user_id IS NOT NULL;
CREATE INDEX grants_team ON grants (team_id) WHERE team_id IS NOT NULL;

-- Shares (§2): read only → viewer; with create, change or delete →
-- editor; with re-share → manager.
INSERT INTO grants (id, drive_id, node_id, user_id, team_id, role, created_by, created_at)
  SELECT id, drive_id, node_id, user_id, group_id,
         CASE WHEN can_share THEN 'manager'
              WHEN can_create OR can_change OR can_delete THEN 'editor'
              ELSE 'viewer' END,
         created_by, created_at
  FROM shares;
-- Drive members: who manage it → manager, the others → editor.
INSERT INTO grants (id, drive_id, node_id, user_id, role, created_by, created_at)
  SELECT randomblob(16), m.drive_id, NULL, m.user_id,
         CASE WHEN m.manages THEN 'manager' ELSE 'editor' END,
         d.created_by, m.added_at
  FROM drive_members m JOIN drives d ON d.id = m.drive_id;

-- A recipient who declined a grant: gone for them (for a team's, for them
-- alone).
CREATE TABLE grant_declines (
  grant_id BLOB NOT NULL REFERENCES grants(id) ON DELETE CASCADE,
  user_id BLOB NOT NULL REFERENCES users(id) ON DELETE CASCADE,
  PRIMARY KEY (grant_id, user_id)
);
INSERT INTO grant_declines (grant_id, user_id) SELECT share_id, user_id FROM share_declines;

DROP TABLE share_declines;
DROP TABLE shares;
DROP TABLE drive_members;
DROP TABLE group_members;
DROP TABLE groups;

-- Who has a drive, and with what role from the drive itself: My drive's
-- user (owner), the drive's grants to users and to teams (each member),
-- and the managers of the drive's team. A user may appear more than once:
-- the highest role counts.
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

-- A trashed item keeps what hides it (§1): whether it and what was under
-- it were limited, and who added them, by their path under it ('' for
-- itself), so a restore gives them back.
CREATE TABLE trash_marks (
  trash_id BLOB NOT NULL REFERENCES trash(id) ON DELETE CASCADE,
  rel_path TEXT NOT NULL,
  limited INTEGER NOT NULL DEFAULT 0,
  created_by BLOB,
  PRIMARY KEY (trash_id, rel_path)
);
