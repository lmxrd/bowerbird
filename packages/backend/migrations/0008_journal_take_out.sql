-- The journal gains `take_out`: a file moved out of the tree into the drop
-- box (an undo or a rejection). SQLite cannot change a CHECK, so the table
-- is made again; a row still in it is kept.
CREATE TABLE journal_new (
  id INTEGER PRIMARY KEY AUTOINCREMENT,
  user_id BLOB NOT NULL REFERENCES users(id) ON DELETE CASCADE,
  op TEXT NOT NULL CHECK (op IN ('create_folder', 'write', 'move', 'trash', 'restore', 'take_out')),
  node_id BLOB,
  -- Absolute paths, so that recovery needs nothing else.
  from_path TEXT,
  to_path TEXT,
  created_at TEXT NOT NULL DEFAULT (strftime('%Y-%m-%dT%H:%M:%fZ', 'now'))
);
INSERT INTO journal_new SELECT * FROM journal;
DROP TABLE journal;
ALTER TABLE journal_new RENAME TO journal;
