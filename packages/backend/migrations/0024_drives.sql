-- no-transaction
-- Drives (docs/M7A_DESIGN.md §1 and §5): a drive is a tree with its own
-- root, trash, versions and storage limit. Every user's tree becomes their
-- My drive, whose id is the user's own (one per user), so the rows keep
-- their keys; shared drives get ids of their own.
--
-- The tables of a tree (nodes, journal, trash, versions, search_docs) and
-- the shares and links made in one are keyed by the drive from now on, a
-- foreign key of `drives` instead of `users`: SQLite cannot change a
-- foreign key, so they are made again. That needs the foreign keys off (a
-- dropped table would take the rows that refer to it with it), which is
-- why this migration runs outside sqlx's transaction and opens its own.
PRAGMA foreign_keys = OFF;
BEGIN;

CREATE TABLE drives (
  id BLOB PRIMARY KEY NOT NULL CHECK (length(id) = 16),
  kind TEXT NOT NULL CHECK (kind IN ('my', 'shared')),
  -- My drive's user (its id is theirs too); NULL for a shared drive.
  user_id BLOB UNIQUE REFERENCES users(id) ON DELETE CASCADE,
  -- A shared drive's name; My drive has none (the apps say "My drive").
  name TEXT NOT NULL DEFAULT '',
  -- A shared drive's folder when the superadmin picked an existing one;
  -- NULL: <DATA_DIR>/drives/<id>. My drive's is the user's files root.
  root TEXT,
  -- Bytes; NULL: no limit.
  storage_limit INTEGER,
  created_by BLOB REFERENCES users(id) ON DELETE SET NULL,
  created_at TEXT NOT NULL DEFAULT (strftime('%Y-%m-%dT%H:%M:%fZ', 'now')),
  CHECK ((kind = 'my') = (user_id IS NOT NULL))
);
CREATE UNIQUE INDEX drives_shared_name ON drives (name COLLATE NOCASE) WHERE kind = 'shared';

INSERT INTO drives (id, kind, user_id, storage_limit, created_by, created_at)
  SELECT id, 'my', id, storage_limit, id, created_at FROM users;
-- The limit is My drive's now.
ALTER TABLE users DROP COLUMN storage_limit;
-- Every new user gets their My drive, whatever way they came in.
CREATE TRIGGER users_my_drive AFTER INSERT ON users
BEGIN
  INSERT INTO drives (id, kind, user_id, created_by) VALUES (NEW.id, 'my', NEW.id, NEW.id);
END;

-- A shared drive's members. In this step a member may do what an editor
-- may; one who `manages` it also adds and removes members and renames it
-- (docs/M7A_DESIGN.md §1). Roles and teams replace this in M7b.
CREATE TABLE drive_members (
  drive_id BLOB NOT NULL REFERENCES drives(id) ON DELETE CASCADE,
  user_id BLOB NOT NULL REFERENCES users(id) ON DELETE CASCADE,
  manages INTEGER NOT NULL DEFAULT 0,
  added_at TEXT NOT NULL DEFAULT (strftime('%Y-%m-%dT%H:%M:%fZ', 'now')),
  PRIMARY KEY (drive_id, user_id)
);
CREATE INDEX drive_members_user ON drive_members (user_id);

-- The tree: keyed by the drive.
CREATE TABLE nodes_new (
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
  UNIQUE (parent_id, name)
);
INSERT INTO nodes_new (id, drive_id, parent_id, name, kind, size, mtime_ns, hash, mime, version,
                       portable, created_at, updated_at, e2e_root)
  SELECT id, user_id, parent_id, name, kind, size, mtime_ns, hash, mime, version,
         portable, created_at, updated_at, e2e_root FROM nodes;
DROP TABLE nodes;
ALTER TABLE nodes_new RENAME TO nodes;
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

CREATE TABLE journal_new (
  id INTEGER PRIMARY KEY AUTOINCREMENT,
  drive_id BLOB NOT NULL REFERENCES drives(id) ON DELETE CASCADE,
  op TEXT NOT NULL CHECK (op IN ('create_folder', 'write', 'move', 'trash', 'restore', 'take_out')),
  node_id BLOB,
  -- Absolute paths, so that recovery needs nothing else.
  from_path TEXT,
  to_path TEXT,
  created_at TEXT NOT NULL DEFAULT (strftime('%Y-%m-%dT%H:%M:%fZ', 'now'))
);
INSERT INTO journal_new (id, drive_id, op, node_id, from_path, to_path, created_at)
  SELECT id, user_id, op, node_id, from_path, to_path, created_at FROM journal;
DROP TABLE journal;
ALTER TABLE journal_new RENAME TO journal;

-- The trash: its files are in <drive root>/.organizer/trash/<id> now
-- (moved there at the first start, `files::drives::move_into_drives`).
CREATE TABLE trash_new (
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
  e2e_root BLOB
);
INSERT INTO trash_new (id, drive_id, name, kind, original_parent_id, original_path, size,
                       deleted_at, e2e_root)
  SELECT id, user_id, name, kind, original_parent_id, original_path, size, deleted_at, e2e_root
  FROM trash;
DROP TABLE trash;
ALTER TABLE trash_new RENAME TO trash;
CREATE INDEX trash_drive ON trash (drive_id, deleted_at);

-- The versions: their bytes are in <drive root>/.organizer/versions/<id>.
CREATE TABLE versions_new (
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
INSERT INTO versions_new (id, drive_id, node_id, version, size, hash, source, created_at,
                          trash_id, trash_path)
  SELECT id, user_id, node_id, version, size, hash, source, created_at, trash_id, trash_path
  FROM versions;
DROP TABLE versions;
ALTER TABLE versions_new RENAME TO versions;
CREATE INDEX versions_node ON versions (node_id, created_at);
CREATE INDEX versions_trash ON versions (trash_id);
CREATE INDEX versions_drive ON versions (drive_id);

-- The search index's rows, by drive: a user searches every drive they have.
CREATE TABLE search_docs_new (
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
INSERT INTO search_docs_new (rowid, node_id, drive_id, kind, name, path, text_hash)
  SELECT rowid, node_id, user_id, kind, name, path, text_hash FROM search_docs;
DROP TABLE search_docs;
ALTER TABLE search_docs_new RENAME TO search_docs;
CREATE INDEX search_docs_drive ON search_docs (drive_id);

-- A share is of a node in a drive (`drive_id`, was the owner); its maker
-- is `created_by`.
CREATE TABLE shares_new (
  id BLOB PRIMARY KEY NOT NULL CHECK (length(id) = 16),
  node_id BLOB NOT NULL REFERENCES nodes(id) ON DELETE CASCADE,
  drive_id BLOB NOT NULL REFERENCES drives(id) ON DELETE CASCADE,
  user_id BLOB REFERENCES users(id) ON DELETE CASCADE,
  group_id BLOB REFERENCES groups(id) ON DELETE CASCADE,
  can_create INTEGER NOT NULL DEFAULT 0,
  can_change INTEGER NOT NULL DEFAULT 0,
  can_delete INTEGER NOT NULL DEFAULT 0,
  can_share INTEGER NOT NULL DEFAULT 0,
  created_by BLOB REFERENCES users(id) ON DELETE CASCADE,
  created_at TEXT NOT NULL DEFAULT (strftime('%Y-%m-%dT%H:%M:%fZ', 'now')),
  CHECK ((user_id IS NULL) != (group_id IS NULL))
);
INSERT INTO shares_new (id, node_id, drive_id, user_id, group_id, can_create, can_change,
                        can_delete, can_share, created_by, created_at)
  SELECT id, node_id, owner_id, user_id, group_id, can_create, can_change, can_delete,
         can_share, created_by, created_at FROM shares;
DROP TABLE shares;
ALTER TABLE shares_new RENAME TO shares;
CREATE UNIQUE INDEX shares_user ON shares (node_id, user_id) WHERE user_id IS NOT NULL;
CREATE UNIQUE INDEX shares_group ON shares (node_id, group_id) WHERE group_id IS NOT NULL;
CREATE INDEX shares_drive ON shares (drive_id);
CREATE INDEX shares_group_id ON shares (group_id);

-- A public link is of a node in a drive; its maker is `created_by`.
CREATE TABLE links_new (
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
INSERT INTO links_new (id, node_id, drive_id, created_by, token_hash, token, kind,
                       password_hash, expires_at, created_at)
  SELECT id, node_id, owner_id, created_by, token_hash, token, kind, password_hash, expires_at,
         created_at FROM links;
DROP TABLE links;
ALTER TABLE links_new RENAME TO links;
CREATE INDEX links_node ON links (node_id);
CREATE INDEX links_drive ON links (drive_id);

-- The change feed stays each user's (the apps follow one feed), each row
-- with the drive it is in: My drive for everything until now (a placed
-- share is in the recipient's My drive too). Not a foreign key: a row
-- outlives its drive.
ALTER TABLE changes ADD COLUMN drive_id BLOB;
UPDATE changes SET drive_id = user_id;

COMMIT;
PRAGMA foreign_keys = ON;
