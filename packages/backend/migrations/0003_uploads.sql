-- Resumable uploads in progress (draft-ietf-httpbis-resumable-upload,
-- interop version 6; docs/M1_DESIGN.md §2.4). The bytes received so far are
-- in <DATA_DIR>/uploads/<id>; `offset` is how many. A complete upload is
-- moved into the tree and its row deleted.
CREATE TABLE uploads (
  id BLOB PRIMARY KEY NOT NULL CHECK (length(id) = 16),
  user_id BLOB NOT NULL REFERENCES users(id) ON DELETE CASCADE,
  -- A new file: the folder and the name.
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
  CHECK ((parent_id IS NOT NULL AND name IS NOT NULL) OR replaces IS NOT NULL)
);
CREATE INDEX uploads_expiry ON uploads (expires_at);
