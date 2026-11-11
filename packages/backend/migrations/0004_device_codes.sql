-- The one-time codes of an app's sign-in (docs/M1_DESIGN.md §3.2): made
-- when the user approves the device in the browser, exchanged by the app,
-- with its PKCE verifier, for a device token. A minute to live; deleted
-- when used.
CREATE TABLE device_codes (
  code_hash TEXT PRIMARY KEY,
  user_id BLOB NOT NULL REFERENCES users(id) ON DELETE CASCADE,
  -- PKCE S256: base64url(SHA-256(verifier)), from the app.
  code_challenge TEXT NOT NULL,
  redirect_uri TEXT NOT NULL,
  device_name TEXT NOT NULL,
  expires_at TEXT NOT NULL
);
