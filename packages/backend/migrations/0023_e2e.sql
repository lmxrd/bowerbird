-- End-to-end-encrypted folders (docs/M6E_DESIGN.md, docs/E2E_SPEC.md).
-- The server keeps keys it cannot use: each user's public key and private
-- key, the latter only wrapped (under the passphrase and under the
-- recovery key), and each encrypted folder's keys wrapped for each member.

-- A user's identity key pair: the X25519 public key in plain, the private
-- key wrapped under a key derived from the passphrase (PBKDF2 with this
-- salt and iteration count) and under the recovery key. Binary values are
-- base64url, as the API sends them.
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

-- The encrypted folder a node is in: the top encrypted folder's id, its
-- own for that folder, NULL for everything else. Names and contents under
-- it are encrypted; its own name is not (it is what a locked device shows).
ALTER TABLE nodes ADD COLUMN e2e_root BLOB;
CREATE INDEX nodes_e2e_root ON nodes (e2e_root) WHERE e2e_root IS NOT NULL;

-- Whatever adds a node (an operation, the reconciler, a restore) puts it
-- in its parent's encrypted folder; a file moved on disk by hand follows
-- its new parent. Organizer itself never moves anything across.
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

-- An encrypted folder's keys: `generation` is the newest (0 until its
-- first key is posted), what new writes use. `needs_new_key` is set when a
-- member lost access: the next member's device to see it makes a new
-- generation, wrapped for the members left. Not a foreign key of `nodes`:
-- the folder in the trash keeps its keys; they go when it is purged
-- (`e2e::forget_gone`).
CREATE TABLE e2e_folders (
  root_id BLOB PRIMARY KEY,
  generation INTEGER NOT NULL DEFAULT 0,
  needs_new_key INTEGER NOT NULL DEFAULT 1,
  created_at TEXT NOT NULL DEFAULT (strftime('%Y-%m-%dT%H:%M:%fZ', 'now'))
);

-- A folder key of a generation wrapped for a member (docs/E2E_SPEC.md §4),
-- base64url. A member who loses access loses their rows.
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

-- Encrypted files and folders in the trash keep their ids: names inside
-- are sealed with their parent's id, so a restore must give the same ones.
-- The trashed node's encrypted folder, and, when encrypted ones are in or
-- under it, every node under it with its path below it (`rel_path`,
-- '/'-joined) and its encrypted folder.
ALTER TABLE trash ADD COLUMN e2e_root BLOB;
CREATE TABLE e2e_trash_nodes (
  trash_id BLOB NOT NULL REFERENCES trash(id) ON DELETE CASCADE,
  id BLOB NOT NULL,
  rel_path TEXT NOT NULL,
  kind TEXT NOT NULL CHECK (kind IN ('file', 'folder')),
  e2e_root BLOB,
  PRIMARY KEY (trash_id, rel_path)
);
CREATE INDEX e2e_trash_nodes_id ON e2e_trash_nodes (id);
