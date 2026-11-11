-- The admin is the superadmin (docs/M7A_DESIGN.md §4): the role is renamed,
-- in users and in the invites that give it.
ALTER TABLE users RENAME COLUMN is_admin TO is_superadmin;
ALTER TABLE invites RENAME COLUMN is_admin TO is_superadmin;

-- The superadmin's endpoints take a session with a second factor confirmed
-- lately, never an API key: an `admin` key is a `write` key now (the scope's
-- CHECK still allows the old value, which is read as `write` too).
UPDATE api_keys SET scope = 'write' WHERE scope = 'admin';

-- Passkey ceremonies of a new kind: a signed-in user confirming one of their
-- passkeys again (step-up). Made again to change the CHECK; what is in it
-- lives five minutes at most, so nothing of worth is lost.
DROP TABLE passkey_challenges;
CREATE TABLE passkey_challenges (
  id BLOB PRIMARY KEY NOT NULL CHECK (length(id) = 16),
  -- The user registering or confirming; NULL for a sign-in (the passkey
  -- says who).
  user_id BLOB REFERENCES users(id) ON DELETE CASCADE,
  kind TEXT NOT NULL CHECK (kind IN ('register', 'sign_in', 'step_up')),
  state TEXT NOT NULL,
  expires_at TEXT NOT NULL
);
