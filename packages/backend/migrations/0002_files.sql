-- The file index, the journal, the change feed and the trash
-- (docs/M1_DESIGN.md §1.2). The tree on disk is the source of truth: these
-- rows describe it, and the reconciler (src/files/scan.rs) keeps them so.

-- One row per file or folder of a user's tree. A path is resolved one name
-- at a time through UNIQUE (parent_id, name), so moving a folder changes one
-- row. `version` is the ETag and the optimistic check of every write.
CREATE TABLE nodes (
  id BLOB PRIMARY KEY NOT NULL CHECK (length(id) = 16),
  user_id BLOB NOT NULL REFERENCES users(id) ON DELETE CASCADE,
  -- NULL only for the user's root; a folder's rows go with it.
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
  UNIQUE (parent_id, name)
);
CREATE UNIQUE INDEX nodes_root ON nodes (user_id) WHERE parent_id IS NULL;
-- Moves made outside Organizer are matched by size and hash.
CREATE INDEX nodes_content ON nodes (user_id, size, hash);

-- What Organizer is changing on disk right now: written before the
-- filesystem operation, deleted once it is done. At start, what is left is
-- settled and the tree rescanned (src/files/journal.rs).
CREATE TABLE journal (
  id INTEGER PRIMARY KEY AUTOINCREMENT,
  user_id BLOB NOT NULL REFERENCES users(id) ON DELETE CASCADE,
  op TEXT NOT NULL CHECK (op IN ('create_folder', 'write', 'move', 'trash', 'restore')),
  node_id BLOB,
  -- Absolute paths, so that recovery needs nothing else.
  from_path TEXT,
  to_path TEXT,
  created_at TEXT NOT NULL DEFAULT (strftime('%Y-%m-%dT%H:%M:%fZ', 'now'))
);

-- The change feed of the sync clients: `seq` is the cursor. `node_id` is no
-- foreign key, so deletions stay listed. Rows older than 30 days are
-- pruned; a cursor older than a user's oldest row is answered with 410.
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
  at TEXT NOT NULL DEFAULT (strftime('%Y-%m-%dT%H:%M:%fZ', 'now'))
);
CREATE INDEX changes_user ON changes (user_id, seq);
CREATE INDEX changes_at ON changes (at);

-- A cursor is `<epoch>.<seq>`. The epoch changes when the tree is indexed
-- from scratch (another files folder), the floor rises when old rows are
-- pruned: a cursor of another epoch, or below the floor, is answered with
-- 410, and the client lists everything again.
ALTER TABLE users ADD COLUMN feed_epoch INTEGER NOT NULL DEFAULT 0;
ALTER TABLE users ADD COLUMN feed_floor INTEGER NOT NULL DEFAULT 0;

-- Deleted files and folders, out of the tree in
-- <DATA_DIR>/users/<user id>/trash/<id>. Deletes are never permanent until
-- someone empties the trash.
CREATE TABLE trash (
  id BLOB PRIMARY KEY NOT NULL CHECK (length(id) = 16),
  user_id BLOB NOT NULL REFERENCES users(id) ON DELETE CASCADE,
  name TEXT NOT NULL,
  kind TEXT NOT NULL CHECK (kind IN ('file', 'folder')),
  -- Where it was, for a restore: the folder's id, and its path relative to
  -- the root for when that folder is gone.
  original_parent_id BLOB,
  original_path TEXT NOT NULL,
  size INTEGER NOT NULL DEFAULT 0,
  deleted_at TEXT NOT NULL DEFAULT (strftime('%Y-%m-%dT%H:%M:%fZ', 'now'))
);
CREATE INDEX trash_user ON trash (user_id, deleted_at);
