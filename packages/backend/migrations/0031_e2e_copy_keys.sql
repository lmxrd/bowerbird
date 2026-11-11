-- The keys a mirror keeps of the encrypted folders it copied
-- (docs/M7D_DESIGN.md §9): a copy is ciphertext sealed with the drive's
-- ids, opened with the mirroring user's wrapped keys of the drive's folder
-- (`e2e_folder_keys`), which go when the drive is deleted or the user
-- loses access. So the mirror keeps that user's own wrappings, every
-- generation they had, and only theirs: still wrapped, nothing in plain.
-- A drive ejected from a mirror keeps them too.
CREATE TABLE e2e_copy_keys (
  -- The copy's encrypted folder (its top) in the mirror. Not a foreign key
  -- of `nodes`: a copy in the mirror's trash keeps its keys; they go when
  -- it is purged (`e2e::forget_gone`).
  folder_id BLOB NOT NULL,
  -- The mirror (or the drive ejected from one) the copy is in.
  drive_id BLOB NOT NULL REFERENCES drives(id) ON DELETE CASCADE,
  -- The drive's encrypted folder the copy is of: the id the keys (and the
  -- names under it) are sealed with. It outlives that folder.
  source_id BLOB NOT NULL,
  generation INTEGER NOT NULL,
  -- Whose wrapping it is: the mirror's user, the one it is wrapped for.
  user_id BLOB NOT NULL REFERENCES users(id) ON DELETE CASCADE,
  -- docs/E2E_SPEC.md §4, base64url, as `e2e_folder_keys` had it.
  wrapped TEXT NOT NULL,
  created_at TEXT NOT NULL DEFAULT (strftime('%Y-%m-%dT%H:%M:%fZ', 'now')),
  PRIMARY KEY (folder_id, generation)
);
CREATE INDEX e2e_copy_keys_source ON e2e_copy_keys (user_id, source_id);
CREATE INDEX e2e_copy_keys_drive ON e2e_copy_keys (drive_id);
