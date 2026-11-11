-- Users, sessions, single sign-on, two-factor authentication and API keys:
-- the same shapes as Potoo's (docs/M1_DESIGN.md §1.1). Ids are UUIDv7 as
-- 16-byte blobs; times are UTC text.

CREATE TABLE users (
  id BLOB PRIMARY KEY NOT NULL CHECK (length(id) = 16),
  email TEXT NOT NULL UNIQUE,
  -- NULL for a user who has no password (signs in through the provider).
  password_hash TEXT,
  is_admin INTEGER NOT NULL DEFAULT 0,
  -- Two-factor authentication: the base32 secret (NULL = off), a setup not
  -- yet confirmed, and the last time step a code was accepted for (a code
  -- works once).
  totp_secret TEXT,
  totp_pending TEXT,
  totp_last_step INTEGER NOT NULL DEFAULT 0,
  -- The user's tree (docs/M1_DESIGN.md §4): NULL is FILES_DIR, or else
  -- <DATA_DIR>/users/<id>/files.
  files_root TEXT,
  created_at TEXT NOT NULL DEFAULT (strftime('%Y-%m-%dT%H:%M:%fZ', 'now'))
);

-- A provider account linked to a user: the issuer and the subject (the
-- provider's own id for the account), never the email.
CREATE TABLE user_identities (
  issuer TEXT NOT NULL,
  subject TEXT NOT NULL,
  user_id BLOB NOT NULL REFERENCES users(id) ON DELETE CASCADE,
  created_at TEXT NOT NULL DEFAULT (strftime('%Y-%m-%dT%H:%M:%fZ', 'now')),
  PRIMARY KEY (issuer, subject)
);
CREATE INDEX user_identities_user ON user_identities (user_id);

-- The single sign-on settings: one row, edited by an admin. Each OIDC_*
-- environment variable that is set wins over its field.
CREATE TABLE oidc_settings (
  id INTEGER PRIMARY KEY CHECK (id = 1),
  issuer TEXT NOT NULL DEFAULT '',
  client_id TEXT NOT NULL DEFAULT '',
  client_secret TEXT NOT NULL DEFAULT '',
  scopes TEXT NOT NULL DEFAULT '',
  label TEXT NOT NULL DEFAULT '',
  redirect_url TEXT NOT NULL DEFAULT ''
);
INSERT INTO oidc_settings (id) VALUES (1);

-- Two-factor recovery codes: the SHA-256 of each, deleted when used.
CREATE TABLE recovery_codes (
  user_id BLOB NOT NULL REFERENCES users(id) ON DELETE CASCADE,
  code_hash TEXT NOT NULL,
  PRIMARY KEY (user_id, code_hash)
);

-- API keys, device tokens and WebDAV app passwords: one kind of token
-- (`org_` + random), told apart by `kind`. Only the SHA-256 is stored.
-- `scope` caps what it may do: `read` (GET only), `write` (also changes),
-- `admin` (also the admin endpoints, for an admin).
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

-- Sessions (tower-sessions, our store): keyed by the SHA-256 of the
-- session id, never the id. `expires_at` is a Unix time.
CREATE TABLE sessions (
  id_hash TEXT PRIMARY KEY,
  data TEXT NOT NULL,
  expires_at INTEGER NOT NULL
);
CREATE INDEX sessions_expiry ON sessions (expires_at);
