-- The items dropped into the drop box (docs/M2_DESIGN.md §1, §4). A file is
-- staged in <DATA_DIR>/users/<user id>/inbox/<item id>/ until it is placed.
CREATE TABLE inbox_items (
  id BLOB PRIMARY KEY,
  user_id BLOB NOT NULL REFERENCES users(id) ON DELETE CASCADE,
  kind TEXT NOT NULL CHECK (kind IN ('file', 'folder', 'link')),
  -- The file's or folder's name; for a link, its address until the page
  -- gives a title.
  name TEXT NOT NULL,
  url TEXT,
  -- What the user said about it when dropping it.
  note TEXT,
  size INTEGER,
  mime_type TEXT,
  -- Whether its contents may be read (the user's setting, or their choice
  -- for this item).
  read_contents INTEGER NOT NULL,
  -- For a link: bookmark, snapshot or download.
  link_mode TEXT CHECK (link_mode IN ('bookmark', 'snapshot', 'download')),
  status TEXT NOT NULL CHECK (status IN (
    'queued',        -- waiting for its analysis
    'analysing',
    'waiting',       -- no AI connection: analysed again once there is one
    'proposed',      -- a place is proposed, to confirm
    'confirmed',
    'changed',       -- placed somewhere else than proposed
    'rejected',      -- the proposal was refused: to place by hand
    'auto_accepted',
    'failed',        -- the analysis failed three times: `error` says why
    'needs_manual',  -- to place by hand (a sensitive-file rule, or no proposal)
    'undone'
  )),
  -- The sensitive-file rule that matched: name_only or by_hand.
  guard TEXT CHECK (guard IN ('name_only', 'by_hand')),
  -- The text read from it (at most 32,000 characters): what the model is sent.
  text TEXT,
  error TEXT,
  -- Where it is in the tree, once placed.
  node_id BLOB,
  created_at TEXT NOT NULL,
  updated_at TEXT NOT NULL
);
CREATE INDEX inbox_items_user ON inbox_items (user_id, created_at);
CREATE INDEX inbox_items_status ON inbox_items (status);
