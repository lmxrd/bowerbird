-- no-transaction
-- Federation through Open Cloud Mesh (docs/M7E_DESIGN.md): the superadmin's
-- choice and lists of servers, people on other servers (contacts and the
-- invites that make them), what is shared with them (outgoing shares) and
-- what they share with our users (incoming shares, each a drive of its own
-- whose files are on the other server).
--
-- `drives.kind` gains 'remote': SQLite cannot change a CHECK, so the table
-- is made again with the foreign keys off, outside sqlx's transaction, as
-- 0030 did; the view and the triggers that name it are made again with it.
PRAGMA foreign_keys = OFF;
BEGIN;

DROP VIEW drive_users;
DROP TRIGGER users_my_drive;
DROP TRIGGER settings_of_drive;

CREATE TABLE drives_new (
  id BLOB PRIMARY KEY NOT NULL CHECK (length(id) = 16),
  kind TEXT NOT NULL CHECK (kind IN ('my', 'shared', 'mirror', 'remote')),
  -- My drive's user (its id is theirs too); NULL for the others.
  user_id BLOB UNIQUE REFERENCES users(id) ON DELETE CASCADE,
  -- A shared drive's name; a mirror's is its drive's (as last copied); a
  -- remote drive's is the share's; My drive has none (the apps say "My
  -- drive").
  name TEXT NOT NULL DEFAULT '',
  root TEXT,
  storage_limit INTEGER,
  created_by BLOB REFERENCES users(id) ON DELETE SET NULL,
  created_at TEXT NOT NULL DEFAULT (strftime('%Y-%m-%dT%H:%M:%fZ', 'now')),
  team_id BLOB REFERENCES teams(id) ON DELETE SET NULL,
  backend_id BLOB REFERENCES backends(id),
  CHECK ((kind = 'my') = (user_id IS NOT NULL)),
  -- A mirror is always in a backend (a user's own).
  CHECK (kind != 'mirror' OR backend_id IS NOT NULL),
  -- A remote drive's files are on the other server, in no backend of ours.
  CHECK (kind != 'remote' OR (backend_id IS NULL AND root IS NULL))
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

-- The superadmin's choice (§2): off (the default), only the servers
-- listed, or open but for the servers refused. One row.
CREATE TABLE federation_settings (
  id INTEGER PRIMARY KEY NOT NULL CHECK (id = 1),
  mode TEXT NOT NULL DEFAULT 'off' CHECK (mode IN ('off', 'listed', 'open'))
);
INSERT INTO federation_settings (id) VALUES (1);

-- The servers listed (`allowed`: the only ones, and trusted, in `listed`)
-- or refused (in `open`), by their host (`cloud.example.org`, with
-- `:port` when not the default).
CREATE TABLE federation_servers (
  host TEXT PRIMARY KEY NOT NULL COLLATE NOCASE,
  list TEXT NOT NULL CHECK (list IN ('allowed', 'refused')),
  created_at TEXT NOT NULL DEFAULT (strftime('%Y-%m-%dT%H:%M:%fZ', 'now'))
);

-- A user's contacts on other servers (§3): made by an invite accepted, in
-- either direction.
CREATE TABLE contacts (
  id BLOB PRIMARY KEY NOT NULL CHECK (length(id) = 16),
  user_id BLOB NOT NULL REFERENCES users(id) ON DELETE CASCADE,
  -- Their OCM address, `identifier@host`.
  address TEXT NOT NULL,
  -- Their server, `host[:port]`, lowercase.
  host TEXT NOT NULL,
  name TEXT NOT NULL DEFAULT '',
  email TEXT NOT NULL DEFAULT '',
  created_at TEXT NOT NULL DEFAULT (strftime('%Y-%m-%dT%H:%M:%fZ', 'now')),
  UNIQUE (user_id, address)
);

-- Invites a user made (§3): found by the token's SHA-256; the token is
-- kept while pending, for "Copy". Accepted once.
CREATE TABLE federated_invites (
  id BLOB PRIMARY KEY NOT NULL CHECK (length(id) = 16),
  user_id BLOB NOT NULL REFERENCES users(id) ON DELETE CASCADE,
  token_hash TEXT NOT NULL UNIQUE,
  token TEXT,
  expires_at TEXT NOT NULL,
  accepted_at TEXT,
  -- Who accepted it: their address.
  accepted_by TEXT,
  created_at TEXT NOT NULL DEFAULT (strftime('%Y-%m-%dT%H:%M:%fZ', 'now'))
);
CREATE INDEX federated_invites_user ON federated_invites (user_id);

-- What is shared with someone on another server (§4): a file or folder
-- (`node_id`), or a whole shared drive (NULL, our extension
-- `organizer-drives`), with a role, as a grant would. Its id is OCM's
-- `providerId`. The shared secret is kept as its SHA-256 only.
CREATE TABLE ocm_shares (
  id BLOB PRIMARY KEY NOT NULL CHECK (length(id) = 16),
  drive_id BLOB NOT NULL REFERENCES drives(id) ON DELETE CASCADE,
  node_id BLOB REFERENCES nodes(id) ON DELETE CASCADE,
  -- The recipient's OCM address and server.
  address TEXT NOT NULL,
  host TEXT NOT NULL,
  -- Their name, as their server answered.
  recipient_name TEXT NOT NULL DEFAULT '',
  role TEXT NOT NULL CHECK (role IN ('manager', 'editor', 'viewer', 'upload_only')),
  created_by BLOB REFERENCES users(id) ON DELETE SET NULL,
  secret_hash TEXT NOT NULL UNIQUE,
  -- `must-exchange-token`: only a token of the code flow opens it.
  strict INTEGER NOT NULL DEFAULT 1,
  state TEXT NOT NULL DEFAULT 'pending' CHECK (state IN ('pending', 'accepted', 'declined')),
  -- What their server has of our extensions.
  live INTEGER NOT NULL DEFAULT 0,
  e2e INTEGER NOT NULL DEFAULT 0,
  -- The last row of `ocm_changes` pruned: an older sync-token is no
  -- longer valid (the other server lists everything again).
  sync_floor INTEGER NOT NULL DEFAULT 0,
  created_at TEXT NOT NULL DEFAULT (strftime('%Y-%m-%dT%H:%M:%fZ', 'now'))
);
CREATE UNIQUE INDEX ocm_shares_node ON ocm_shares (node_id, address) WHERE node_id IS NOT NULL;
CREATE UNIQUE INDEX ocm_shares_drive ON ocm_shares (drive_id, address) WHERE node_id IS NULL;
CREATE INDEX ocm_shares_drive_id ON ocm_shares (drive_id);

-- What changed in a share, for its WebDAV's sync-collection (RFC 6578) and
-- the live notices: the path from the share's top, gone or not.
CREATE TABLE ocm_changes (
  seq INTEGER PRIMARY KEY AUTOINCREMENT,
  share_id BLOB NOT NULL REFERENCES ocm_shares(id) ON DELETE CASCADE,
  path TEXT NOT NULL,
  gone INTEGER NOT NULL DEFAULT 0,
  at TEXT NOT NULL DEFAULT (strftime('%Y-%m-%dT%H:%M:%fZ', 'now'))
);
CREATE INDEX ocm_changes_share ON ocm_changes (share_id, seq);
CREATE INDEX ocm_changes_at ON ocm_changes (at);

-- What a remote member added through a share (upload only sees only
-- that, as `nodes.created_by` does for a local user).
CREATE TABLE ocm_created (
  node_id BLOB NOT NULL REFERENCES nodes(id) ON DELETE CASCADE,
  share_id BLOB NOT NULL REFERENCES ocm_shares(id) ON DELETE CASCADE,
  PRIMARY KEY (node_id, share_id)
);

-- What someone on another server shared with one of our users (§5): a
-- drive of kind 'remote' whose storage is the other server's WebDAV, its
-- recipient a grant on its root (or on the drive, for a whole drive).
-- The shared secret is sealed with the server key (as the backends').
CREATE TABLE ocm_received (
  id BLOB PRIMARY KEY NOT NULL CHECK (length(id) = 16),
  user_id BLOB NOT NULL REFERENCES users(id) ON DELETE CASCADE,
  drive_id BLOB NOT NULL UNIQUE REFERENCES drives(id) ON DELETE CASCADE,
  host TEXT NOT NULL,
  provider_id TEXT NOT NULL,
  name TEXT NOT NULL,
  resource_type TEXT NOT NULL CHECK (resource_type IN ('file', 'folder')),
  owner TEXT NOT NULL,
  owner_name TEXT NOT NULL DEFAULT '',
  sender TEXT NOT NULL,
  sender_name TEXT NOT NULL DEFAULT '',
  -- OCM's webdav permissions, as a JSON array.
  permissions TEXT NOT NULL,
  role TEXT NOT NULL CHECK (role IN ('manager', 'editor', 'viewer', 'upload_only')),
  -- The resource's absolute WebDAV URL.
  uri TEXT NOT NULL,
  secret BLOB NOT NULL,
  must_exchange INTEGER NOT NULL DEFAULT 0,
  token_endpoint TEXT,
  -- A whole drive (`organizer-drives`): in the drives, not "Shared with me".
  whole_drive INTEGER NOT NULL DEFAULT 0,
  -- Encrypted (`organizer-e2e`): the keys stay on the other server.
  e2e INTEGER NOT NULL DEFAULT 0,
  state TEXT NOT NULL DEFAULT 'pending' CHECK (state IN ('pending', 'accepted')),
  -- Following its changes: the last sync-token (RFC 6578) or ETag seen,
  -- whether the other server answers sync-collection, and when it was
  -- last looked at.
  sync_token TEXT,
  etag TEXT,
  sync_collection INTEGER,
  checked_at TEXT,
  error TEXT,
  created_at TEXT NOT NULL DEFAULT (strftime('%Y-%m-%dT%H:%M:%fZ', 'now')),
  UNIQUE (host, provider_id, user_id)
);
CREATE INDEX ocm_received_user ON ocm_received (user_id);

COMMIT;
PRAGMA foreign_keys = ON;
