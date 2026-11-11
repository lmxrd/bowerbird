-- The safety net, its first part (docs/M3_DESIGN.md §1 and §2): how long
-- the trash and the versions are kept, and the versions themselves.

-- The admin's defaults: what a user has until they change it. A TRASH_DAYS
-- or VERSIONS_* variable wins over both (src/retention.rs).
CREATE TABLE retention_defaults (
  id INTEGER PRIMARY KEY CHECK (id = 1),
  -- 0: kept until the trash is emptied.
  trash_days INTEGER NOT NULL DEFAULT 30,
  -- Every version for this many hours, then one a day for this many days,
  -- then one a month for this many months. All 0: no versions.
  versions_hours INTEGER NOT NULL DEFAULT 24,
  versions_days INTEGER NOT NULL DEFAULT 30,
  versions_months INTEGER NOT NULL DEFAULT 12
);
INSERT INTO retention_defaults (id) VALUES (1);

-- A user's own rules; NULL is the default.
CREATE TABLE retention (
  user_id BLOB PRIMARY KEY REFERENCES users(id) ON DELETE CASCADE,
  trash_days INTEGER,
  versions_hours INTEGER,
  versions_days INTEGER,
  versions_months INTEGER
);

-- The earlier contents of a file, kept when a write through Organizer
-- replaced them. The bytes are in <DATA_DIR>/users/<id>/versions/<id>.
CREATE TABLE versions (
  id BLOB PRIMARY KEY NOT NULL CHECK (length(id) = 16),
  user_id BLOB NOT NULL REFERENCES users(id) ON DELETE CASCADE,
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
CREATE INDEX trash_deleted ON trash (user_id, deleted_at);

-- What wrote a resumable upload, for its version.
ALTER TABLE uploads ADD COLUMN source TEXT;
