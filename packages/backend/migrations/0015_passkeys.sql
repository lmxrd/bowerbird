-- Passkeys (WebAuthn): a passkey signs in on its own, in place of the
-- password and the code (src/passkeys.rs).
CREATE TABLE passkeys (
  id BLOB PRIMARY KEY NOT NULL CHECK (length(id) = 16),
  user_id BLOB NOT NULL REFERENCES users(id) ON DELETE CASCADE,
  -- The credential's id, as the authenticator gives it.
  credential_id BLOB NOT NULL UNIQUE,
  name TEXT NOT NULL,
  -- webauthn-rs's Passkey, as JSON: the public key and its counter.
  passkey TEXT NOT NULL,
  created_at TEXT NOT NULL DEFAULT (strftime('%Y-%m-%dT%H:%M:%fZ', 'now')),
  last_used_at TEXT
);
CREATE INDEX passkeys_user ON passkeys (user_id);

-- A registration or sign-in in progress: its challenge, used once, for five
-- minutes.
CREATE TABLE passkey_challenges (
  id BLOB PRIMARY KEY NOT NULL CHECK (length(id) = 16),
  -- The user registering; NULL for a sign-in (the passkey says who).
  user_id BLOB REFERENCES users(id) ON DELETE CASCADE,
  kind TEXT NOT NULL CHECK (kind IN ('register', 'sign_in')),
  state TEXT NOT NULL,
  expires_at TEXT NOT NULL
);
