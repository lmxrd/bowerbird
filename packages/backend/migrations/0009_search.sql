-- Full-text search over the tree (docs/M2_DESIGN.md §4): names, paths and
-- the text read from files, in FTS5. `search_docs` says what each row of
-- `search` holds (its rowid is the FTS row's), so that the index is brought
-- up to date by comparing it with the tree (src/search.rs).
CREATE TABLE search_docs (
  rowid INTEGER PRIMARY KEY,
  node_id BLOB NOT NULL UNIQUE,
  user_id BLOB NOT NULL REFERENCES users(id) ON DELETE CASCADE,
  kind TEXT NOT NULL,
  name TEXT NOT NULL,
  -- The parent folder's path from the root ('' at the top).
  path TEXT NOT NULL,
  -- The content hash (BLAKE3) whose text was looked at: read, or found to
  -- have none, or not to be read. NULL: not yet. A move or a rename keeps
  -- it; a new content does not.
  text_hash BLOB
);
CREATE INDEX search_docs_user ON search_docs (user_id);

CREATE VIRTUAL TABLE search USING fts5(
  name, path, text,
  tokenize = 'unicode61 remove_diacritics 2'
);
