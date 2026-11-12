-- The base schema: every migration up to the first release (0001-0035)
-- squashed into their final result, grouped by area. The per-change files,
-- with their comments and the docs' "migration NNNN" references, are in git
-- history. New schema changes are new numbered files, 0002_*.sql and up;
-- never edit this one once it has shipped (its checksum is recorded in
-- _sqlx_migrations — see packages/backend/AGENTS.md).
--
-- Produced by applying 0001-0035 to a fresh database and dumping the final
-- schema (sqlite3 .schema), then normalized: columns that later migrations
-- added with ALTER TABLE are merged into the CREATE TABLE, one per line.
--
-- This file does not start with `-- no-transaction`, so sqlx runs it in a
-- single transaction, bookkeeping included: it either applies completely or
-- not at all. It only creates the final objects and inserts the seed rows,
-- with the foreign keys on, so no table is ever rebuilt with them off.


-- ---------------------------------------------------------------------------
-- Auth and accounts
-- ---------------------------------------------------------------------------

CREATE TABLE users (
  id BLOB PRIMARY KEY NOT NULL CHECK (length(id) = 16),
  email TEXT NOT NULL UNIQUE,
  -- NULL for a user who has no password (signs in through the provider).
  password_hash TEXT,
  is_superadmin INTEGER NOT NULL DEFAULT 0,
  -- Two-factor authentication: the base32 secret (NULL = off), a setup not
  -- yet confirmed, and the last time step a code was accepted for (a code
  -- works once).
  totp_secret TEXT,
  totp_pending TEXT,
  totp_last_step INTEGER NOT NULL DEFAULT 0,
  -- The user's tree (docs/M1_DESIGN.md §4): NULL is FILES_DIR, or else
  -- <DATA_DIR>/users/<id>/files.
  files_root TEXT,
  created_at TEXT NOT NULL DEFAULT (strftime('%Y-%m-%dT%H:%M:%fZ', 'now')),
  feed_epoch INTEGER NOT NULL DEFAULT 0,
  feed_floor INTEGER NOT NULL DEFAULT 0,
  disabled_at TEXT,
  last_seen_at TEXT,
  owns_files_dir INTEGER NOT NULL DEFAULT 0,
  shared_seen_at TEXT,
  primary_team_id BLOB REFERENCES teams(id) ON DELETE SET NULL
);

CREATE TABLE user_identities (
  issuer TEXT NOT NULL,
  subject TEXT NOT NULL,
  user_id BLOB NOT NULL REFERENCES users(id) ON DELETE CASCADE,
  created_at TEXT NOT NULL DEFAULT (strftime('%Y-%m-%dT%H:%M:%fZ', 'now')),
  PRIMARY KEY (issuer, subject)
);

CREATE INDEX user_identities_user ON user_identities (user_id);

CREATE TABLE recovery_codes (
  user_id BLOB NOT NULL REFERENCES users(id) ON DELETE CASCADE,
  code_hash TEXT NOT NULL,
  PRIMARY KEY (user_id, code_hash)
);

CREATE TABLE api_keys (
  id BLOB PRIMARY KEY NOT NULL CHECK (length(id) = 16),
  user_id BLOB NOT NULL REFERENCES users(id) ON DELETE CASCADE,
  kind TEXT NOT NULL DEFAULT 'key' CHECK (kind IN ('key', 'device', 'app_password')),
  name TEXT NOT NULL,
  prefix TEXT NOT NULL,
  token_hash TEXT NOT NULL UNIQUE,
  scope TEXT NOT NULL CHECK (scope IN ('read', 'write', 'admin')),
  created_at TEXT NOT NULL DEFAULT (strftime('%Y-%m-%dT%H:%M:%fZ', 'now')),
  last_used_at TEXT,
  expires_at TEXT
);

CREATE INDEX api_keys_user ON api_keys (user_id);

CREATE TABLE sessions (
  id_hash TEXT PRIMARY KEY,
  data TEXT NOT NULL,
  expires_at INTEGER NOT NULL
);

CREATE INDEX sessions_expiry ON sessions (expires_at);

CREATE TABLE device_codes (
  code_hash TEXT PRIMARY KEY,
  user_id BLOB NOT NULL REFERENCES users(id) ON DELETE CASCADE,
  -- PKCE S256: base64url(SHA-256(verifier)), from the app.
  code_challenge TEXT NOT NULL,
  redirect_uri TEXT NOT NULL,
  device_name TEXT NOT NULL,
  expires_at TEXT NOT NULL
);

CREATE TABLE passkeys (
  id BLOB PRIMARY KEY NOT NULL CHECK (length(id) = 16),
  user_id BLOB NOT NULL REFERENCES users(id) ON DELETE CASCADE,
  -- The credential's id, as the authenticator gives it.
  credential_id BLOB NOT NULL UNIQUE,
  name TEXT NOT NULL,
  -- webauthn-rs's Passkey, as JSON: the public key and its counter.
  passkey TEXT NOT NULL,
  created_at TEXT NOT NULL DEFAULT (strftime('%Y-%m-%dT%H:%M:%fZ', 'now')),
  last_used_at TEXT
);

CREATE INDEX passkeys_user ON passkeys (user_id);

CREATE TABLE invites (
  id BLOB PRIMARY KEY NOT NULL CHECK (length(id) = 16),
  token_hash TEXT NOT NULL UNIQUE,
  token TEXT,
  -- Only this email may use it; NULL: anyone with the link.
  email TEXT,
  is_superadmin INTEGER NOT NULL DEFAULT 0,
  created_by BLOB REFERENCES users(id) ON DELETE SET NULL,
  created_at TEXT NOT NULL,
  expires_at TEXT NOT NULL,
  used_at TEXT
);

CREATE TABLE passkey_challenges (
  id BLOB PRIMARY KEY NOT NULL CHECK (length(id) = 16),
  -- The user registering or confirming; NULL for a sign-in (the passkey
  -- says who).
  user_id BLOB REFERENCES users(id) ON DELETE CASCADE,
  kind TEXT NOT NULL CHECK (kind IN ('register', 'sign_in', 'step_up')),
  state TEXT NOT NULL,
  expires_at TEXT NOT NULL
);

-- ---------------------------------------------------------------------------
-- Files and drives
-- ---------------------------------------------------------------------------

CREATE TABLE changes (
  seq INTEGER PRIMARY KEY AUTOINCREMENT,
  user_id BLOB NOT NULL REFERENCES users(id) ON DELETE CASCADE,
  node_id BLOB NOT NULL,
  op TEXT NOT NULL CHECK (op IN ('create', 'update', 'move', 'delete')),
  kind TEXT CHECK (kind IN ('file', 'folder')),
  -- After the change; NULL for a deletion.
  parent_id BLOB,
  name TEXT,
  version INTEGER,
  at TEXT NOT NULL DEFAULT (strftime('%Y-%m-%dT%H:%M:%fZ', 'now')),
  drive_id BLOB
);

CREATE INDEX changes_user ON changes (user_id, seq);

CREATE INDEX changes_at ON changes (at);

CREATE VIRTUAL TABLE search USING fts5(
  name, path, text,
  tokenize = 'unicode61 remove_diacritics 2'
);

CREATE TABLE uploads (
  id BLOB PRIMARY KEY NOT NULL CHECK (length(id) = 16),
  user_id BLOB NOT NULL REFERENCES users(id) ON DELETE CASCADE,
  -- A new file: the folder and the name (or, for the drop box, the name).
  parent_id BLOB,
  name TEXT,
  -- Or a file it replaces, at its version then.
  replaces BLOB,
  if_version INTEGER,
  -- The final size, once the client has said it.
  length INTEGER,
  "offset" INTEGER NOT NULL DEFAULT 0,
  -- Received in full, but not placed in the tree (the name was taken in
  -- the meantime, say): appending is refused, and it expires.
  complete INTEGER NOT NULL DEFAULT 0,
  created_at TEXT NOT NULL DEFAULT (strftime('%Y-%m-%dT%H:%M:%fZ', 'now')),
  -- 24 hours after the last byte.
  expires_at TEXT NOT NULL,
  -- What wrote it, for its version.
  source TEXT,
  -- Becomes a drop-box item when complete, instead of a file of the tree.
  drop_box INTEGER NOT NULL DEFAULT 0,
  -- The drop-box item's note and read-contents choice.
  note TEXT,
  read_contents INTEGER,
  CHECK (
    (drop_box = 0 AND note IS NULL AND read_contents IS NULL
      AND ((parent_id IS NOT NULL AND name IS NOT NULL) OR replaces IS NOT NULL))
    OR (drop_box = 1 AND name IS NOT NULL AND parent_id IS NULL AND replaces IS NULL)
  )
);

CREATE INDEX uploads_expiry ON uploads (expires_at);

CREATE TABLE share_mounts (
  id BLOB PRIMARY KEY NOT NULL CHECK (length(id) = 16),
  user_id BLOB NOT NULL REFERENCES users(id) ON DELETE CASCADE,
  node_id BLOB NOT NULL REFERENCES nodes(id) ON DELETE CASCADE,
  parent_id BLOB NOT NULL REFERENCES nodes(id) ON DELETE CASCADE,
  name TEXT NOT NULL,
  created_at TEXT NOT NULL DEFAULT (strftime('%Y-%m-%dT%H:%M:%fZ', 'now')),
  UNIQUE (user_id, node_id)
);

CREATE INDEX share_mounts_parent ON share_mounts (parent_id);

CREATE INDEX share_mounts_node ON share_mounts (node_id);

CREATE TABLE nodes (
  id BLOB PRIMARY KEY NOT NULL CHECK (length(id) = 16),
  drive_id BLOB NOT NULL REFERENCES drives(id) ON DELETE CASCADE,
  -- NULL only for the drive's root; a folder's rows go with it.
  parent_id BLOB REFERENCES nodes(id) ON DELETE CASCADE,
  name TEXT NOT NULL,
  kind TEXT NOT NULL CHECK (kind IN ('file', 'folder')),
  size INTEGER NOT NULL DEFAULT 0,
  -- The modification time on disk, in nanoseconds since 1970.
  mtime_ns INTEGER NOT NULL DEFAULT 0,
  -- BLAKE3 of the content, 32 bytes; files only, NULL until hashed.
  hash BLOB CHECK (hash IS NULL OR length(hash) = 32),
  mime TEXT,
  version INTEGER NOT NULL DEFAULT 1,
  -- 0 for a name that breaks the portable-name rules (made outside
  -- Organizer: kept, but flagged).
  portable INTEGER NOT NULL DEFAULT 1,
  created_at TEXT NOT NULL DEFAULT (strftime('%Y-%m-%dT%H:%M:%fZ', 'now')),
  updated_at TEXT NOT NULL DEFAULT (strftime('%Y-%m-%dT%H:%M:%fZ', 'now')),
  e2e_root BLOB,
  created_by BLOB REFERENCES users(id) ON DELETE SET NULL,
  limited INTEGER NOT NULL DEFAULT 0,
  source_id BLOB,
  UNIQUE (parent_id, name)
);

CREATE UNIQUE INDEX nodes_root ON nodes (drive_id) WHERE parent_id IS NULL;

CREATE INDEX nodes_content ON nodes (drive_id, size, hash);

CREATE INDEX nodes_e2e_root ON nodes (e2e_root) WHERE e2e_root IS NOT NULL;

CREATE TRIGGER nodes_e2e_insert AFTER INSERT ON nodes
WHEN NEW.e2e_root IS NULL AND NEW.parent_id IS NOT NULL
BEGIN
  UPDATE nodes SET e2e_root = (SELECT e2e_root FROM nodes WHERE id = NEW.parent_id)
  WHERE id = NEW.id;
END;

CREATE TRIGGER nodes_e2e_move AFTER UPDATE OF parent_id ON nodes
WHEN NEW.e2e_root IS NOT NEW.id
BEGIN
  UPDATE nodes SET e2e_root = (SELECT e2e_root FROM nodes WHERE id = NEW.parent_id)
  WHERE id = NEW.id;
END;

CREATE TABLE journal (
  id INTEGER PRIMARY KEY AUTOINCREMENT,
  drive_id BLOB NOT NULL REFERENCES drives(id) ON DELETE CASCADE,
  op TEXT NOT NULL CHECK (op IN ('create_folder', 'write', 'move', 'trash', 'restore', 'take_out')),
  node_id BLOB,
  -- Absolute paths, so that recovery needs nothing else.
  from_path TEXT,
  to_path TEXT,
  created_at TEXT NOT NULL DEFAULT (strftime('%Y-%m-%dT%H:%M:%fZ', 'now')),
  target_parent BLOB,
  target_name TEXT
);

CREATE TABLE trash (
  id BLOB PRIMARY KEY NOT NULL CHECK (length(id) = 16),
  drive_id BLOB NOT NULL REFERENCES drives(id) ON DELETE CASCADE,
  name TEXT NOT NULL,
  kind TEXT NOT NULL CHECK (kind IN ('file', 'folder')),
  -- Where it was, for a restore: the folder's id, and its path relative to
  -- the root for when that folder is gone.
  original_parent_id BLOB,
  original_path TEXT NOT NULL,
  size INTEGER NOT NULL DEFAULT 0,
  deleted_at TEXT NOT NULL DEFAULT (strftime('%Y-%m-%dT%H:%M:%fZ', 'now')),
  e2e_root BLOB,
  source_id BLOB,
  parent_source_id BLOB,
  e2e_root_source_id BLOB
);

CREATE INDEX trash_drive ON trash (drive_id, deleted_at);

CREATE TABLE versions (
  id BLOB PRIMARY KEY NOT NULL CHECK (length(id) = 16),
  drive_id BLOB NOT NULL REFERENCES drives(id) ON DELETE CASCADE,
  -- The file: in the tree, or in the trash (then with trash_id).
  node_id BLOB NOT NULL,
  -- The file's version these contents were.
  version INTEGER NOT NULL,
  size INTEGER NOT NULL,
  hash BLOB NOT NULL,
  -- What wrote the contents that replaced these: web, api or webdav.
  source TEXT,
  created_at TEXT NOT NULL DEFAULT (strftime('%Y-%m-%dT%H:%M:%fZ', 'now')),
  -- In the trash: the trash item it went with (the file, or a folder it
  -- was in), and its path inside that folder, to find it again after a
  -- restore (what is in a restored folder is indexed anew).
  trash_id BLOB,
  trash_path TEXT
);

CREATE INDEX versions_node ON versions (node_id, created_at);

CREATE INDEX versions_trash ON versions (trash_id);

CREATE INDEX versions_drive ON versions (drive_id);

CREATE TABLE search_docs (
  rowid INTEGER PRIMARY KEY,
  node_id BLOB NOT NULL UNIQUE,
  drive_id BLOB NOT NULL REFERENCES drives(id) ON DELETE CASCADE,
  kind TEXT NOT NULL,
  name TEXT NOT NULL,
  -- The parent folder's path from the root ('' at the top).
  path TEXT NOT NULL,
  -- The content hash (BLAKE3) whose text was looked at: read, or found to
  -- have none, or not to be read. NULL: not yet. A move or a rename keeps
  -- it; a new content does not.
  text_hash BLOB
);

CREATE INDEX search_docs_drive ON search_docs (drive_id);

CREATE TABLE links (
  id BLOB PRIMARY KEY NOT NULL CHECK (length(id) = 16),
  node_id BLOB NOT NULL REFERENCES nodes(id) ON DELETE CASCADE,
  drive_id BLOB NOT NULL REFERENCES drives(id) ON DELETE CASCADE,
  -- Who made it: the drive's user, a member, or someone with re-share.
  created_by BLOB REFERENCES users(id) ON DELETE CASCADE,
  token_hash TEXT NOT NULL UNIQUE,
  token TEXT NOT NULL,
  -- view: see and download; edit: also upload, new folder, rename and
  -- delete; drop: upload only, nothing shown.
  kind TEXT NOT NULL CHECK (kind IN ('view', 'edit', 'drop')),
  -- argon2 (PHC string); NULL: none.
  password_hash TEXT,
  expires_at TEXT,
  created_at TEXT NOT NULL DEFAULT (strftime('%Y-%m-%dT%H:%M:%fZ', 'now'))
);

CREATE INDEX links_node ON links (node_id);

CREATE INDEX links_drive ON links (drive_id);

CREATE INDEX nodes_limited ON nodes (drive_id) WHERE limited = 1;

CREATE TABLE trash_marks (
  trash_id BLOB NOT NULL REFERENCES trash(id) ON DELETE CASCADE,
  rel_path TEXT NOT NULL,
  limited INTEGER NOT NULL DEFAULT 0,
  created_by BLOB,
  PRIMARY KEY (trash_id, rel_path)
);

CREATE UNIQUE INDEX nodes_source ON nodes (drive_id, source_id) WHERE source_id IS NOT NULL;

CREATE INDEX changes_user_drive ON changes (user_id, drive_id, seq);

CREATE TABLE drives (
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

-- ---------------------------------------------------------------------------
-- Drop box and AI
-- ---------------------------------------------------------------------------

CREATE TABLE guard_rules (
  id BLOB PRIMARY KEY,
  user_id BLOB NOT NULL REFERENCES users(id) ON DELETE CASCADE,
  kind TEXT NOT NULL CHECK (kind IN ('path', 'name', 'keyword')),
  pattern TEXT NOT NULL,
  action TEXT NOT NULL CHECK (action IN ('name_only', 'by_hand')),
  created_at TEXT NOT NULL DEFAULT (strftime('%Y-%m-%dT%H:%M:%fZ', 'now'))
);

CREATE INDEX guard_rules_user ON guard_rules (user_id, created_at);

CREATE TABLE jobs (
  id INTEGER PRIMARY KEY AUTOINCREMENT,
  kind TEXT NOT NULL,
  -- JSON, read by the job's handler.
  payload TEXT NOT NULL,
  status TEXT NOT NULL DEFAULT 'queued' CHECK (status IN ('queued', 'running', 'failed')),
  attempts INTEGER NOT NULL DEFAULT 0,
  -- Not run before this time (the retries' delay, the pending timeout).
  run_after TEXT NOT NULL,
  last_error TEXT
);

CREATE INDEX jobs_next ON jobs (kind, status, run_after);

CREATE TABLE inbox_items (
  id BLOB PRIMARY KEY,
  user_id BLOB NOT NULL REFERENCES users(id) ON DELETE CASCADE,
  kind TEXT NOT NULL CHECK (kind IN ('file', 'folder', 'link')),
  -- The file's or folder's name; for a link, its address until the page
  -- gives a title.
  name TEXT NOT NULL,
  url TEXT,
  -- What the user said about it when dropping it.
  note TEXT,
  size INTEGER,
  mime_type TEXT,
  -- Whether its contents may be read (the user's setting, or their choice
  -- for this item).
  read_contents INTEGER NOT NULL,
  -- For a link: bookmark, snapshot or download.
  link_mode TEXT CHECK (link_mode IN ('bookmark', 'snapshot', 'download')),
  status TEXT NOT NULL CHECK (status IN (
    'queued',        -- waiting for its analysis
    'analysing',
    'waiting',       -- no AI connection: analysed again once there is one
    'proposed',      -- a place is proposed, to confirm
    'confirmed',
    'changed',       -- placed somewhere else than proposed
    'rejected',      -- the proposal was refused: to place by hand
    'auto_accepted',
    'failed',        -- the analysis failed three times: `error` says why
    'needs_manual',  -- to place by hand (a sensitive-file rule, or no proposal)
    'undone'
  )),
  -- The sensitive-file rule that matched: name_only or by_hand.
  guard TEXT CHECK (guard IN ('name_only', 'by_hand')),
  -- The text read from it (at most 32,000 characters): what the model is sent.
  text TEXT,
  error TEXT,
  -- Where it is in the tree, once placed.
  node_id BLOB,
  created_at TEXT NOT NULL,
  updated_at TEXT NOT NULL,
  dav_folder TEXT CHECK (dav_folder IN ('', 'review', 'auto')),
  companions TEXT NOT NULL DEFAULT '[]',
  source_path TEXT
);

CREATE INDEX inbox_items_user ON inbox_items (user_id, created_at);

CREATE INDEX inbox_items_status ON inbox_items (status);

CREATE TABLE proposals (
  item_id BLOB PRIMARY KEY REFERENCES inbox_items(id) ON DELETE CASCADE,
  folder_id BLOB,
  -- The folder's path when proposed, for display (the folder may move).
  folder_path TEXT NOT NULL,
  -- A JSON array of the names of new folders, each under the one before.
  new_folders TEXT NOT NULL DEFAULT '[]',
  -- A new name for the item; NULL keeps its own.
  name TEXT,
  reason TEXT NOT NULL,
  confidence REAL NOT NULL,
  model TEXT NOT NULL,
  created_at TEXT NOT NULL
);

CREATE TABLE placements (
  id BLOB PRIMARY KEY,
  item_id BLOB NOT NULL REFERENCES inbox_items(id) ON DELETE CASCADE,
  node_id BLOB NOT NULL,
  to_parent BLOB NOT NULL,
  to_name TEXT NOT NULL,
  -- A JSON array of the ids of the folders made for it, the deepest last.
  created_folders TEXT NOT NULL DEFAULT '[]',
  applied_at TEXT NOT NULL,
  undone_at TEXT,
  companion_nodes TEXT NOT NULL DEFAULT '[]'
);

CREATE INDEX placements_item ON placements (item_id, applied_at);

CREATE TABLE corrections (
  id BLOB PRIMARY KEY,
  user_id BLOB NOT NULL REFERENCES users(id) ON DELETE CASCADE,
  -- The item as the model saw it, short: its name and note.
  summary TEXT NOT NULL,
  -- The paths proposed and chosen, with the file name.
  proposed TEXT NOT NULL,
  chosen TEXT NOT NULL,
  created_at TEXT NOT NULL
);

CREATE INDEX corrections_user ON corrections (user_id, created_at);

CREATE TABLE ai_models (
  id BLOB PRIMARY KEY,
  -- NULL: the admin's, for everyone. A user's own models are for later.
  owner_id BLOB REFERENCES users(id) ON DELETE CASCADE,
  -- The name the admin gives it; one of each per owner.
  name TEXT NOT NULL,
  -- A genai adapter: openai (any OpenAI-compatible API), ollama, anthropic,
  -- gemini, ...
  provider TEXT NOT NULL,
  -- The base URL; empty for the provider's own.
  endpoint TEXT NOT NULL DEFAULT '',
  -- Never sent back.
  api_key TEXT NOT NULL DEFAULT '',
  -- The provider's name for the model.
  model TEXT NOT NULL,
  -- What it can do.
  chat INTEGER NOT NULL DEFAULT 0,
  vision INTEGER NOT NULL DEFAULT 0,
  speech INTEGER NOT NULL DEFAULT 0,
  embeddings INTEGER NOT NULL DEFAULT 0,
  created_at TEXT NOT NULL
);

CREATE UNIQUE INDEX ai_models_name ON ai_models (ifnull(owner_id, x''), name);

CREATE TABLE model_reads (
  user_id BLOB NOT NULL REFERENCES users(id) ON DELETE CASCADE,
  -- The content's BLAKE3.
  hash BLOB NOT NULL,
  -- `images`, `scans` or `audio`: the work that read it.
  work TEXT NOT NULL CHECK (work IN ('images', 'scans', 'audio')),
  -- Empty: the model found nothing.
  text TEXT NOT NULL,
  -- The registered name of the model that read it.
  model TEXT NOT NULL,
  read_at TEXT NOT NULL,
  PRIMARY KEY (user_id, hash, work)
);

-- ---------------------------------------------------------------------------
-- Teams, grants and settings
-- ---------------------------------------------------------------------------

CREATE TABLE teams (
  id BLOB PRIMARY KEY NOT NULL CHECK (length(id) = 16),
  name TEXT NOT NULL UNIQUE COLLATE NOCASE,
  -- Bytes, for all the team's drives together; NULL: no limit.
  storage_limit INTEGER,
  created_at TEXT NOT NULL DEFAULT (strftime('%Y-%m-%dT%H:%M:%fZ', 'now'))
);

CREATE TABLE team_members (
  team_id BLOB NOT NULL REFERENCES teams(id) ON DELETE CASCADE,
  user_id BLOB NOT NULL REFERENCES users(id) ON DELETE CASCADE,
  role TEXT NOT NULL DEFAULT 'member' CHECK (role IN ('manager', 'member')),
  joined_at TEXT NOT NULL DEFAULT (strftime('%Y-%m-%dT%H:%M:%fZ', 'now')),
  PRIMARY KEY (team_id, user_id)
);

CREATE INDEX team_members_user ON team_members (user_id);

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

CREATE TABLE grant_declines (
  grant_id BLOB NOT NULL REFERENCES grants(id) ON DELETE CASCADE,
  user_id BLOB NOT NULL REFERENCES users(id) ON DELETE CASCADE,
  PRIMARY KEY (grant_id, user_id)
);

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

CREATE TRIGGER settings_of_user AFTER DELETE ON users
BEGIN
  DELETE FROM settings WHERE level = 'user' AND level_id = OLD.id;
END;

CREATE TRIGGER settings_of_team AFTER DELETE ON teams
BEGIN
  DELETE FROM settings WHERE level = 'team' AND level_id = OLD.id;
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

-- ---------------------------------------------------------------------------
-- Activity, backends and mirrors
-- ---------------------------------------------------------------------------

CREATE TABLE activity (
  id BLOB PRIMARY KEY NOT NULL CHECK (length(id) = 16),
  -- Whose account.
  user_id BLOB NOT NULL REFERENCES users(id) ON DELETE CASCADE,
  at TEXT NOT NULL,
  -- 'superadmin' or 'team_manager' (actor_id: who), or 'command_line'.
  actor TEXT NOT NULL CHECK (actor IN ('superadmin', 'command_line', 'team_manager')),
  actor_id BLOB REFERENCES users(id) ON DELETE SET NULL,
  action TEXT NOT NULL,
  -- JSON: what it was about (a team's or drive's name, a role, a limit).
  detail TEXT NOT NULL DEFAULT '{}'
);

CREATE INDEX activity_user ON activity (user_id, at);

CREATE TABLE backends (
  id BLOB PRIMARY KEY NOT NULL CHECK (length(id) = 16),
  -- NULL: a global backend; else the user whose own storage it is.
  owner_id BLOB REFERENCES users(id) ON DELETE CASCADE,
  kind TEXT NOT NULL CHECK (kind IN ('s3')),
  name TEXT NOT NULL,
  -- The S3 endpoint (https://…), its region, the bucket and a prefix in
  -- it (each drive is at `<prefix>/<drive id>/`).
  endpoint TEXT NOT NULL,
  region TEXT NOT NULL DEFAULT '',
  bucket TEXT NOT NULL,
  prefix TEXT NOT NULL DEFAULT '',
  access_key TEXT NOT NULL,
  -- The secret encrypted with the server key (a nonce, then the
  -- ciphertext and its tag, AES-256-GCM); NULL with private credentials.
  secret BLOB,
  -- Private credentials (users' own backends only): the secret sealed to
  -- the owner's encryption key by their device, which the server cannot
  -- open. A session that unlocks it hands the secret over, in memory only.
  sealed_secret TEXT,
  -- Bytes for all its drives together; NULL: no limit.
  storage_limit INTEGER,
  -- The limit each new drive gets; NULL: none.
  drive_limit INTEGER,
  -- Who may make drives on a global backend: the superadmin alone, the
  -- teams' managers (their team's drives), everyone, or the managers of
  -- the teams in `backend_teams`. A user's own: its owner only.
  make_drives TEXT NOT NULL DEFAULT 'superadmin'
    CHECK (make_drives IN ('superadmin', 'team_managers', 'everyone', 'teams')),
  -- How often each drive's prefix is listed for changes made outside
  -- Organizer, in seconds; NULL: never.
  list_every INTEGER DEFAULT 3600 CHECK (list_every IS NULL OR list_every >= 60),
  created_at TEXT NOT NULL DEFAULT (strftime('%Y-%m-%dT%H:%M:%fZ', 'now')),
  CHECK ((secret IS NULL) != (sealed_secret IS NULL)),
  CHECK (sealed_secret IS NULL OR owner_id IS NOT NULL)
);

CREATE UNIQUE INDEX backends_global_name ON backends (name COLLATE NOCASE) WHERE owner_id IS NULL;

CREATE UNIQUE INDEX backends_own_name ON backends (owner_id, name COLLATE NOCASE)
  WHERE owner_id IS NOT NULL;

CREATE TABLE backend_teams (
  backend_id BLOB NOT NULL REFERENCES backends(id) ON DELETE CASCADE,
  team_id BLOB NOT NULL REFERENCES teams(id) ON DELETE CASCADE,
  PRIMARY KEY (backend_id, team_id)
);

CREATE TABLE drive_moves (
  drive_id BLOB PRIMARY KEY NOT NULL REFERENCES drives(id) ON DELETE CASCADE,
  from_backend BLOB,
  to_backend BLOB,
  started_at TEXT NOT NULL DEFAULT (strftime('%Y-%m-%dT%H:%M:%fZ', 'now')),
  -- Why the last try failed (fit to show); it is tried again.
  error TEXT
);

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
  created_at TEXT NOT NULL DEFAULT (strftime('%Y-%m-%dT%H:%M:%fZ', 'now')),
  stopped_at TEXT
);

CREATE INDEX mirrors_user ON mirrors (user_id);

CREATE INDEX mirrors_source ON mirrors (source_id);

CREATE TABLE e2e_copy_keys (
  -- The copy's encrypted folder (its top) in the mirror. Not a foreign key
  -- of `nodes`: a copy in the mirror's trash keeps its keys; they go when
  -- it is purged (`e2e::forget_gone`).
  folder_id BLOB NOT NULL,
  -- The mirror (or the drive ejected from one) the copy is in.
  drive_id BLOB NOT NULL REFERENCES drives(id) ON DELETE CASCADE,
  -- The drive's encrypted folder the copy is of: the id the keys (and the
  -- names under it) are sealed with. It outlives that folder.
  source_id BLOB NOT NULL,
  generation INTEGER NOT NULL,
  -- Whose wrapping it is: the mirror's user, the one it is wrapped for.
  user_id BLOB NOT NULL REFERENCES users(id) ON DELETE CASCADE,
  -- docs/E2E_SPEC.md §4, base64url, as `e2e_folder_keys` had it.
  wrapped TEXT NOT NULL,
  created_at TEXT NOT NULL DEFAULT (strftime('%Y-%m-%dT%H:%M:%fZ', 'now')),
  PRIMARY KEY (folder_id, generation)
);

CREATE INDEX e2e_copy_keys_source ON e2e_copy_keys (user_id, source_id);

CREATE INDEX e2e_copy_keys_drive ON e2e_copy_keys (drive_id);

CREATE TRIGGER mirrors_drive_deleted AFTER UPDATE OF source_id ON mirrors
WHEN OLD.source_id IS NOT NULL AND NEW.source_id IS NULL
BEGIN
  UPDATE mirrors SET stopped_at = strftime('%Y-%m-%dT%H:%M:%fZ', 'now') WHERE id = NEW.id;
END;

-- ---------------------------------------------------------------------------
-- End-to-end encryption
-- ---------------------------------------------------------------------------

CREATE TABLE e2e_keys (
  user_id BLOB PRIMARY KEY REFERENCES users(id) ON DELETE CASCADE,
  public_key TEXT NOT NULL,
  passphrase_salt TEXT NOT NULL,
  passphrase_iterations INTEGER NOT NULL,
  passphrase_wrapped TEXT NOT NULL,
  recovery_wrapped TEXT NOT NULL,
  created_at TEXT NOT NULL DEFAULT (strftime('%Y-%m-%dT%H:%M:%fZ', 'now')),
  updated_at TEXT NOT NULL DEFAULT (strftime('%Y-%m-%dT%H:%M:%fZ', 'now'))
);

CREATE TABLE e2e_folders (
  root_id BLOB PRIMARY KEY,
  generation INTEGER NOT NULL DEFAULT 0,
  needs_new_key INTEGER NOT NULL DEFAULT 1,
  created_at TEXT NOT NULL DEFAULT (strftime('%Y-%m-%dT%H:%M:%fZ', 'now'))
);

CREATE TABLE e2e_folder_keys (
  root_id BLOB NOT NULL REFERENCES e2e_folders(root_id) ON DELETE CASCADE,
  generation INTEGER NOT NULL,
  user_id BLOB NOT NULL REFERENCES users(id) ON DELETE CASCADE,
  wrapped TEXT NOT NULL,
  wrapped_by BLOB REFERENCES users(id) ON DELETE SET NULL,
  created_at TEXT NOT NULL DEFAULT (strftime('%Y-%m-%dT%H:%M:%fZ', 'now')),
  PRIMARY KEY (root_id, generation, user_id)
);

CREATE INDEX e2e_folder_keys_user ON e2e_folder_keys (user_id);

CREATE TABLE e2e_trash_nodes (
  trash_id BLOB NOT NULL REFERENCES trash(id) ON DELETE CASCADE,
  id BLOB NOT NULL,
  rel_path TEXT NOT NULL,
  kind TEXT NOT NULL CHECK (kind IN ('file', 'folder')),
  e2e_root BLOB,
  PRIMARY KEY (trash_id, rel_path)
);

CREATE INDEX e2e_trash_nodes_id ON e2e_trash_nodes (id);

CREATE TABLE e2e_remote_keys (
  root_id BLOB NOT NULL REFERENCES e2e_folders(root_id) ON DELETE CASCADE,
  generation INTEGER NOT NULL,
  share_id BLOB NOT NULL REFERENCES ocm_shares(id) ON DELETE CASCADE,
  wrapped TEXT NOT NULL,
  wrapped_by BLOB REFERENCES users(id) ON DELETE SET NULL,
  created_at TEXT NOT NULL DEFAULT (strftime('%Y-%m-%dT%H:%M:%fZ', 'now')),
  PRIMARY KEY (root_id, generation, share_id)
);

CREATE INDEX e2e_remote_keys_share ON e2e_remote_keys (share_id);

-- ---------------------------------------------------------------------------
-- Federation and OCM
-- ---------------------------------------------------------------------------

CREATE TABLE federation_servers (
  host TEXT PRIMARY KEY NOT NULL COLLATE NOCASE,
  list TEXT NOT NULL CHECK (list IN ('allowed', 'refused')),
  created_at TEXT NOT NULL DEFAULT (strftime('%Y-%m-%dT%H:%M:%fZ', 'now'))
);

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
  provider TEXT,
  organizer INTEGER NOT NULL DEFAULT 0,
  UNIQUE (user_id, address)
);

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
  created_at TEXT NOT NULL DEFAULT (strftime('%Y-%m-%dT%H:%M:%fZ', 'now')),
  public_key TEXT
);

CREATE UNIQUE INDEX ocm_shares_node ON ocm_shares (node_id, address) WHERE node_id IS NOT NULL;

CREATE UNIQUE INDEX ocm_shares_drive ON ocm_shares (drive_id, address) WHERE node_id IS NULL;

CREATE INDEX ocm_shares_drive_id ON ocm_shares (drive_id);

CREATE TABLE ocm_changes (
  seq INTEGER PRIMARY KEY AUTOINCREMENT,
  share_id BLOB NOT NULL REFERENCES ocm_shares(id) ON DELETE CASCADE,
  path TEXT NOT NULL,
  gone INTEGER NOT NULL DEFAULT 0,
  at TEXT NOT NULL DEFAULT (strftime('%Y-%m-%dT%H:%M:%fZ', 'now'))
);

CREATE INDEX ocm_changes_share ON ocm_changes (share_id, seq);

CREATE INDEX ocm_changes_at ON ocm_changes (at);

CREATE TABLE ocm_created (
  node_id BLOB NOT NULL REFERENCES nodes(id) ON DELETE CASCADE,
  share_id BLOB NOT NULL REFERENCES ocm_shares(id) ON DELETE CASCADE,
  PRIMARY KEY (node_id, share_id)
);

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

-- ---------------------------------------------------------------------------
-- Backups, push and notices
-- ---------------------------------------------------------------------------

CREATE TABLE push_keys (
  id INTEGER PRIMARY KEY CHECK (id = 1),
  vapid_private_key TEXT NOT NULL
);

CREATE TABLE push_subscriptions (
  id BLOB PRIMARY KEY,
  user_id BLOB NOT NULL REFERENCES users(id) ON DELETE CASCADE,
  kind TEXT NOT NULL CHECK (kind IN ('web', 'unifiedpush', 'apns')),
  -- What the user sees in the list: "Firefox on Linux", "Pixel 8".
  name TEXT NOT NULL,
  -- The push service's URL (Web Push), or the device token (APNs, hex).
  endpoint TEXT NOT NULL UNIQUE,
  -- Web Push only: the client's P-256 key and auth secret, base64url.
  p256dh TEXT,
  auth TEXT,
  created_at TEXT NOT NULL,
  last_notice_at TEXT
);

CREATE INDEX push_subscriptions_user ON push_subscriptions (user_id);

CREATE TABLE notification_settings (
  user_id BLOB PRIMARY KEY REFERENCES users(id) ON DELETE CASCADE,
  review_notified_at TEXT
);

CREATE TABLE notices (
  id BLOB PRIMARY KEY,
  user_id BLOB NOT NULL REFERENCES users(id) ON DELETE CASCADE,
  kind TEXT NOT NULL CHECK (kind IN ('review', 'shared', 'backup')),
  -- Items to review: how many there were.
  count INTEGER,
  created_at TEXT NOT NULL
);

CREATE INDEX notices_user ON notices (user_id, created_at);

-- ---------------------------------------------------------------------------
-- Server-wide settings
-- ---------------------------------------------------------------------------

-- One JSON document per area: `oidc`, `ai`, `backup`, `backup_status`,
-- `signup`, `federation`. Key-value on purpose: the shapes, defaults and
-- validation are in code (src/server_settings.rs), not in column types.
CREATE TABLE server_settings (
  key TEXT PRIMARY KEY,
  value TEXT NOT NULL CHECK (json_valid(value))
);

-- The seed rows: one document per area, with its defaults.
INSERT INTO server_settings (key, value) VALUES
  ('oidc', '{"issuer":"","client_id":"","client_secret":"","scopes":"","label":"","redirect_url":""}'),
  ('ai', '{"default_model":null,"proposals":null,"embeddings":null,"images":null,"scans":null,"audio":null,"concurrency":1,"timeout_seconds":120}'),
  ('backup', '{"repository":"","s3_endpoint":"","s3_region":"","s3_access_key":"","s3_secret_key":"","password":"","schedule":"daily 03:00","keep_daily":7,"keep_weekly":4,"keep_monthly":12}'),
  ('backup_status', '{"last_started":null,"last_finished":null,"last_error":null,"last_snapshot":null,"last_added":null,"last_total":null,"snapshots":null,"last_check":null}'),
  ('signup', '{"open_signup":false,"oidc_create_users":false}'),
  ('federation', '{"mode":"off"}');
