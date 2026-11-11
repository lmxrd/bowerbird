-- A resumable upload may go to the drop box: `drop_box` with `name`, and
-- the per-item options of a multipart drop (`note`, `read_contents`: NULL
-- for the user's setting). SQLite cannot change a CHECK, so the table is
-- made again with the new one; the uploads in progress are kept.
CREATE TABLE uploads_new (
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
INSERT INTO uploads_new (id, user_id, parent_id, name, replaces, if_version, length,
  "offset", complete, created_at, expires_at, source)
SELECT id, user_id, parent_id, name, replaces, if_version, length,
  "offset", complete, created_at, expires_at, source
FROM uploads;
DROP TABLE uploads;
ALTER TABLE uploads_new RENAME TO uploads;
CREATE INDEX uploads_expiry ON uploads (expires_at);
