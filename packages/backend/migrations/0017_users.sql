-- More than one user (docs/M6A_DESIGN.md §3): an admin disables a user
-- (NULL: enabled), sets a storage limit (bytes; NULL: none), and sees when
-- each was last active (at most a minute late).
ALTER TABLE users ADD COLUMN disabled_at TEXT;
ALTER TABLE users ADD COLUMN storage_limit INTEGER;
ALTER TABLE users ADD COLUMN last_seen_at TEXT;
-- FILES_DIR is the files folder of the first admin only, the one made at
-- setup; every other user has their own folder. Kept on the row, so that
-- removing that admin never hands FILES_DIR to someone else.
ALTER TABLE users ADD COLUMN owns_files_dir INTEGER NOT NULL DEFAULT 0;
UPDATE users SET owns_files_dir = 1
  WHERE id = (SELECT id FROM users ORDER BY created_at, id LIMIT 1);

-- Invite links: made by an admin, good for 7 days, used once. Found by the
-- SHA-256 of the token; the token itself is kept while the invite is
-- pending, so that the admin can copy its link again, and cleared when it
-- is used.
CREATE TABLE invites (
  id BLOB PRIMARY KEY NOT NULL CHECK (length(id) = 16),
  token_hash TEXT NOT NULL UNIQUE,
  token TEXT,
  -- Only this email may use it; NULL: anyone with the link.
  email TEXT,
  is_admin INTEGER NOT NULL DEFAULT 0,
  created_by BLOB REFERENCES users(id) ON DELETE SET NULL,
  created_at TEXT NOT NULL,
  expires_at TEXT NOT NULL,
  used_at TEXT
);

-- How people may join besides invites: open sign-up, and single sign-on
-- making an account on a first sign-in. SIGNUP_OPEN and OIDC_CREATE_USERS
-- win over these and lock them.
CREATE TABLE signup_settings (
  id INTEGER PRIMARY KEY CHECK (id = 1),
  open_signup INTEGER NOT NULL DEFAULT 0,
  oidc_create_users INTEGER NOT NULL DEFAULT 0
);
INSERT INTO signup_settings (id) VALUES (1);
