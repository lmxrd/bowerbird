-- Encrypted folders shared with someone on another server (`organizer-e2e`,
-- docs/M7E_DESIGN.md §10, docs/E2E_SPEC.md §12). The member is not a user
-- here: they are the share with them (`ocm_shares`), and their keys are
-- wrapped by the owner's devices as for a local member and kept here, on
-- the owner's server, served signed to the member's server.

-- Their public key, as their server answered it (base64url, 32 bytes);
-- NULL until they set up encryption there. A key once set up is never
-- replaced (the folders' keys are wrapped for it), so it is asked once.
ALTER TABLE ocm_shares ADD COLUMN public_key TEXT;

-- A folder key of a generation wrapped for a member on another server
-- (docs/E2E_SPEC.md §4), base64url. A share that ends takes its rows, and
-- the folder then needs a new key (`e2e::remote_gone`).
CREATE TABLE e2e_remote_keys (
  root_id BLOB NOT NULL REFERENCES e2e_folders(root_id) ON DELETE CASCADE,
  generation INTEGER NOT NULL,
  share_id BLOB NOT NULL REFERENCES ocm_shares(id) ON DELETE CASCADE,
  wrapped TEXT NOT NULL,
  wrapped_by BLOB REFERENCES users(id) ON DELETE SET NULL,
  created_at TEXT NOT NULL DEFAULT (strftime('%Y-%m-%dT%H:%M:%fZ', 'now')),
  PRIMARY KEY (root_id, generation, share_id)
);
CREATE INDEX e2e_remote_keys_share ON e2e_remote_keys (share_id);
