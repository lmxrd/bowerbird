-- Sharing with people and groups (docs/M6B_DESIGN.md §1, §2, §4, §6).

-- Groups, made by an admin, to share with.
CREATE TABLE groups (
  id BLOB PRIMARY KEY NOT NULL CHECK (length(id) = 16),
  name TEXT NOT NULL UNIQUE COLLATE NOCASE,
  created_at TEXT NOT NULL DEFAULT (strftime('%Y-%m-%dT%H:%M:%fZ', 'now'))
);

CREATE TABLE group_members (
  group_id BLOB NOT NULL REFERENCES groups(id) ON DELETE CASCADE,
  user_id BLOB NOT NULL REFERENCES users(id) ON DELETE CASCADE,
  PRIMARY KEY (group_id, user_id)
);
CREATE INDEX group_members_user ON group_members (user_id);

-- A file or folder of the owner's tree shared with a user or a group.
-- Read is always given; the rest is each on or off. A share goes with its
-- node (the node deleted, to the trash too, ends it) and with its owner.
-- `created_by` is the owner, or someone who had re-share: what they give
-- is never more than what they have, checked when the share is used.
CREATE TABLE shares (
  id BLOB PRIMARY KEY NOT NULL CHECK (length(id) = 16),
  node_id BLOB NOT NULL REFERENCES nodes(id) ON DELETE CASCADE,
  owner_id BLOB NOT NULL REFERENCES users(id) ON DELETE CASCADE,
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
CREATE UNIQUE INDEX shares_user ON shares (node_id, user_id) WHERE user_id IS NOT NULL;
CREATE UNIQUE INDEX shares_group ON shares (node_id, group_id) WHERE group_id IS NOT NULL;
CREATE INDEX shares_owner ON shares (owner_id);
CREATE INDEX shares_group_id ON shares (group_id);

-- A recipient who declined a share: it is gone for them (for a group's
-- share, for them alone).
CREATE TABLE share_declines (
  share_id BLOB NOT NULL REFERENCES shares(id) ON DELETE CASCADE,
  user_id BLOB NOT NULL REFERENCES users(id) ON DELETE CASCADE,
  PRIMARY KEY (share_id, user_id)
);

-- A share placed in the recipient's tree: a mount, standing for the
-- owner's node under `parent_id` (a folder of the recipient's own tree,
-- never inside another mount) with the recipient's `name`. Keyed by the
-- node rather than the share: a node shared with someone twice (directly
-- and through a group) is placed once, with what both give.
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

-- When the user last looked at "Shared with me": newer shares are the
-- sidebar's badge.
ALTER TABLE users ADD COLUMN shared_seen_at TEXT;
