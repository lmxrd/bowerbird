-- What a vision or speech model read from a file (docs/M6C_DESIGN.md §3 to
-- §5): a photo's or a scan's text and description, a recording's words.
-- By the content's hash, so a file is read once, however it is moved,
-- renamed or dropped again (src/reading.rs).
CREATE TABLE model_reads (
  user_id BLOB NOT NULL REFERENCES users(id) ON DELETE CASCADE,
  -- The content's BLAKE3.
  hash BLOB NOT NULL,
  -- `images`, `scans` or `audio`: the work that read it.
  work TEXT NOT NULL CHECK (work IN ('images', 'scans', 'audio')),
  -- Empty: the model found nothing.
  text TEXT NOT NULL,
  -- The registered name of the model that read it.
  model TEXT NOT NULL,
  read_at TEXT NOT NULL,
  PRIMARY KEY (user_id, hash, work)
);
