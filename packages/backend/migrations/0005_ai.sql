-- The AI drop box, its first part (docs/M2_DESIGN.md §2 to §5): the
-- connection to the model, each user's drop box settings and sensitive-file
-- rules, and the queue of background jobs.

-- The AI connection, set by an admin; an AI_* environment variable wins
-- over its field (src/ai.rs). The API key is never sent back.
CREATE TABLE ai_settings (
  id INTEGER PRIMARY KEY CHECK (id = 1),
  -- A genai adapter: openai (any OpenAI-compatible API), ollama, anthropic,
  -- gemini, ... Empty: the drop box is off.
  provider TEXT NOT NULL DEFAULT '',
  -- The base URL; empty for the provider's own.
  endpoint TEXT NOT NULL DEFAULT '',
  api_key TEXT NOT NULL DEFAULT '',
  model TEXT NOT NULL DEFAULT '',
  -- Empty: no embeddings (no semantic retrieval or search).
  embeddings_model TEXT NOT NULL DEFAULT '',
  -- Empty: the chat endpoint.
  embeddings_endpoint TEXT NOT NULL DEFAULT '',
  concurrency INTEGER NOT NULL DEFAULT 1,
  timeout_seconds INTEGER NOT NULL DEFAULT 120
);
INSERT INTO ai_settings (id) VALUES (1);

-- A user's drop box settings; no row is the defaults.
CREATE TABLE drop_box_settings (
  user_id BLOB PRIMARY KEY REFERENCES users(id) ON DELETE CASCADE,
  pending_mode TEXT NOT NULL DEFAULT 'move_pending'
    CHECK (pending_mode IN ('move_pending', 'stay', 'timeout', 'confidence')),
  -- With `timeout`: accepted after this many hours.
  accept_after_hours INTEGER,
  -- With `confidence`: accepted at once from this confidence (0 to 1).
  confidence_threshold REAL,
  read_contents TEXT NOT NULL DEFAULT 'always'
    CHECK (read_contents IN ('always', 'per_item', 'default_on')),
  folder_freedom TEXT NOT NULL DEFAULT 'balanced'
    CHECK (folder_freedom IN ('consistent', 'balanced', 'refined')),
  -- A JSON array of top-level folder names; NULL for any.
  allowed_top_folders TEXT,
  folders TEXT NOT NULL DEFAULT 'whole' CHECK (folders IN ('whole', 'split')),
  links TEXT NOT NULL DEFAULT 'snapshot' CHECK (links IN ('bookmark', 'snapshot', 'download'))
);

-- The sensitive-file rules: an item that matches is placed by its name
-- only (its contents never read), or left for the user to place by hand
-- (nothing sent at all).
CREATE TABLE guard_rules (
  id BLOB PRIMARY KEY,
  user_id BLOB NOT NULL REFERENCES users(id) ON DELETE CASCADE,
  kind TEXT NOT NULL CHECK (kind IN ('path', 'name', 'keyword')),
  pattern TEXT NOT NULL,
  action TEXT NOT NULL CHECK (action IN ('name_only', 'by_hand')),
  created_at TEXT NOT NULL
);
CREATE INDEX guard_rules_user ON guard_rules (user_id, created_at);

-- The background jobs (src/jobs.rs). A job that fails is tried again later,
-- three times in all, then left `failed` with its error.
CREATE TABLE jobs (
  id INTEGER PRIMARY KEY AUTOINCREMENT,
  kind TEXT NOT NULL,
  -- JSON, read by the job's handler.
  payload TEXT NOT NULL,
  status TEXT NOT NULL DEFAULT 'queued' CHECK (status IN ('queued', 'running', 'failed')),
  attempts INTEGER NOT NULL DEFAULT 0,
  -- Not run before this time (the retries' delay, the pending timeout).
  run_after TEXT NOT NULL,
  last_error TEXT
);
CREATE INDEX jobs_next ON jobs (kind, status, run_after);
