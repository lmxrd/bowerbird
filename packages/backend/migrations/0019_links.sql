-- Public links (docs/M6B_DESIGN.md §3, §6): a file or folder reached
-- without signing in, at /s/<token>.

-- Found by the SHA-256 of the token. The token itself is kept too: the
-- Share dialog shows each link's URL to copy again, as the pending invites
-- do. A link goes with its node (deleted, to the trash too) and its owner;
-- it does nothing while the owner is disabled or after it expires.
CREATE TABLE links (
  id BLOB PRIMARY KEY NOT NULL CHECK (length(id) = 16),
  node_id BLOB NOT NULL REFERENCES nodes(id) ON DELETE CASCADE,
  owner_id BLOB NOT NULL REFERENCES users(id) ON DELETE CASCADE,
  -- The owner, or someone with re-share on it.
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
CREATE INDEX links_owner ON links (owner_id);
