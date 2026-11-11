-- Where the model proposes an item goes, where it was placed, and what the
-- user chose instead (docs/M2_DESIGN.md §4).

-- The model's proposal for an item: an existing folder (NULL: the top of
-- the tree), maybe new folders under it, maybe a new name.
CREATE TABLE proposals (
  item_id BLOB PRIMARY KEY REFERENCES inbox_items(id) ON DELETE CASCADE,
  folder_id BLOB,
  -- The folder's path when proposed, for display (the folder may move).
  folder_path TEXT NOT NULL,
  -- A JSON array of the names of new folders, each under the one before.
  new_folders TEXT NOT NULL DEFAULT '[]',
  -- A new name for the item; NULL keeps its own.
  name TEXT,
  reason TEXT NOT NULL,
  confidence REAL NOT NULL,
  model TEXT NOT NULL,
  created_at TEXT NOT NULL
);

-- An item placed in the tree. Undone, the file goes back to the drop box,
-- and the folders made for it go too if they are still empty.
CREATE TABLE placements (
  id BLOB PRIMARY KEY,
  item_id BLOB NOT NULL REFERENCES inbox_items(id) ON DELETE CASCADE,
  node_id BLOB NOT NULL,
  to_parent BLOB NOT NULL,
  to_name TEXT NOT NULL,
  -- A JSON array of the ids of the folders made for it, the deepest last.
  created_folders TEXT NOT NULL DEFAULT '[]',
  applied_at TEXT NOT NULL,
  undone_at TEXT
);
CREATE INDEX placements_item ON placements (item_id, applied_at);

-- A place the user chose over the proposal: an example for the next ones.
CREATE TABLE corrections (
  id BLOB PRIMARY KEY,
  user_id BLOB NOT NULL REFERENCES users(id) ON DELETE CASCADE,
  -- The item as the model saw it, short: its name and note.
  summary TEXT NOT NULL,
  -- The paths proposed and chosen, with the file name.
  proposed TEXT NOT NULL,
  chosen TEXT NOT NULL,
  created_at TEXT NOT NULL
);
CREATE INDEX corrections_user ON corrections (user_id, created_at);
