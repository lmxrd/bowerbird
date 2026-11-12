# M2 design proposal — the AI drop box

Status: **accepted 2026-11-06** (§9 as recommended), being implemented. Not built yet: dropped folders, and the UI (being built; see §8b for the decisions). It covers
milestone M2 of [ARCHITECTURE.md](ARCHITECTURE.md): the drop box pipeline
(§5 there), the AI connection, extraction, retrieval, proposals, undo, the
review UI, the sensitive-file guard, search, and the eval harness. The
questions that need a decision are in §9.

Decided already (ARCHITECTURE.md §2, 2026-11-06):
- **The connection** is the admin's: a settings page and `AI_*` variables
  that lock their fields, as single sign-on works.
- **Any endpoint** may be used, local or cloud, with no warning. The
  settings say exactly what is sent (§2.3).
- **Several providers** through genai.
- **The drop box's behaviour** is each user's own.

## 1. What happens to a dropped item

```
ingest → (guard) → metadata → (extract) → retrieve → propose → validate → place → notify → review
```

1. **Ingest.** The item arrives by an upload to the drop box, a link, the
   WebDAV drop box (§9, question 1), or later the apps' share sheets. It is
   staged in `<DATA_DIR>/users/<id>/inbox/<item id>/` and recorded as an
   `inbox_items` row with its options (read contents? keep a dropped
   folder whole? what to do with a link?). The sensitive-file guard is
   checked here.
2. **Analyse** (a job, §4). Metadata, then the extracted text when reading
   contents is on and the guard allows it, then the retrieved context of
   the tree, then the model's proposal, then our own validation.
3. **Place** by the user's pending mode:
   - (a) **move now**, marked pending (the default);
   - (b) **stay in the drop box** until confirmed;
   - (c) **move now, accept after a timeout**;
   - (d) **accept when the confidence is high enough**, otherwise ask.
4. **Review.** Confirm, change (another folder or name), reject (back to
   the drop box), or undo. A change is kept as a correction: a few-shot
   example for the next proposals.

Without an AI connection, or when it is down, items wait in the drop box;
they can always be placed by hand.

## 2. The AI connection (admin)

> **Since M6c** the one connection is a registry of models with a default and
> a model per kind of work, and the `AI_*` variables are one locked model:
> see [M6C_DESIGN.md](M6C_DESIGN.md) §1 and `apps/server/AGENTS.md`.
> What follows is the connection as M2 built it.

### 2.1 Settings

One `ai_settings` row, edited by an admin, with `AI_*` variables on top
(a variable locks its field):

| Field | Variable | Meaning |
| --- | --- | --- |
| `provider` | `AI_PROVIDER` | `openai` (any OpenAI-compatible API: Ollama, llama.cpp, LM Studio, vLLM, OpenAI, …), `ollama` (native), `anthropic`, `gemini`, … as genai supports |
| `endpoint` | `AI_ENDPOINT` | The base URL; empty for the provider's own |
| `api_key` | `AI_API_KEY` | Stored like the OIDC client secret: never sent back |
| `model` | `AI_MODEL` | The chat model of the proposals |
| `embeddings_model` | `AI_EMBEDDINGS_MODEL` | Optional: semantic retrieval and search. Empty turns them off |
| `embeddings_endpoint` | `AI_EMBEDDINGS_ENDPOINT` | Optional: when embeddings come from another server |
| `concurrency` | `AI_CONCURRENCY` | How many requests at once (default 1: a local model is slow and shared) |
| `timeout_seconds` | `AI_TIMEOUT` | Per request (default 120) |

A save asks the provider first, as single sign-on does: it lists the
models, or sends a tiny prompt, and the embeddings model if set. A failure
is a 400 with the provider's reason, and nothing is stored.

### 2.2 Endpoints

| Method and path | What |
| --- | --- |
| `GET /admin/ai` | The settings, without the key (`api_key_set`), the locked fields, and whether it is working |
| `PUT /admin/ai` | Save (tested first) |
| `POST /admin/ai/test` | Test without saving |

### 2.3 What is sent: the description shown in the settings

Generated from the settings, on the AI connection page and the user's
drop box page:

> **What Organizer sends to the AI**
> - For every item: its name, type, size and dates, the note you add when
>   you drop it, and the names of folders in your tree (the top level and
>   the ones most like the item).
> - When reading contents is on: the text taken from the file (PDF, office
>   documents, plain text), up to the first 32,000 characters, and for a
>   link the page's title, description and text.
> - With embeddings on: your folder names, and the text read from your
>   files for search (when reading contents is on), to compare them by
>   meaning.
> - Never: files that match your sensitive-file rules (only their name,
>   or nothing, as the rule says), the file's bytes themselves, your
>   account or other users' files.
>
> They go to: `<endpoint>` (`<provider>`, model `<model>`).

## 3. The drop box (each user)

### 3.1 Settings

One `drop_box_settings` row per user:

| Setting | Values (**default**) |
| --- | --- |
| Pending mode | **move now, mark pending**; stay in the drop box; move now and accept after N hours; accept at confidence ≥ X, else ask |
| Reading contents | **on for every item**; chosen per item; on by default, per item to turn off |
| New folders | **balanced**; consistent (prefers existing folders); refined (more specific ones) |
| Top-level folders it may use | **any**; a list |
| A dropped folder | **kept whole**; split into its files |
| A shared link | bookmark; **bookmark and page snapshot**; download the file it points to |

Plus `guard_rules`: path, filename or keyword patterns, each saying
**name only** (placed by its name, its content never read) or **by hand**
(nothing sent, left in the drop box).

### 3.2 Endpoints

| Method and path | What |
| --- | --- |
| `GET/PUT /drop-box/settings` | The user's settings |
| `GET/POST /drop-box/guard-rules` · `DELETE /drop-box/guard-rules/{id}` | The sensitive-file rules |

## 4. Tables

```sql
CREATE TABLE ai_settings (id INTEGER PRIMARY KEY CHECK (id = 1),
  provider TEXT NOT NULL DEFAULT '', endpoint TEXT NOT NULL DEFAULT '',
  api_key TEXT NOT NULL DEFAULT '', model TEXT NOT NULL DEFAULT '',
  embeddings_model TEXT NOT NULL DEFAULT '', embeddings_endpoint TEXT NOT NULL DEFAULT '',
  concurrency INTEGER NOT NULL DEFAULT 1, timeout_seconds INTEGER NOT NULL DEFAULT 120);

CREATE TABLE drop_box_settings (user_id BLOB PRIMARY KEY REFERENCES users(id) ON DELETE CASCADE,
  pending_mode TEXT NOT NULL DEFAULT 'move_pending',   -- move_pending | stay | timeout | confidence
  accept_after_hours INTEGER, confidence_threshold REAL,
  read_contents TEXT NOT NULL DEFAULT 'always',        -- always | per_item | default_on
  folder_freedom TEXT NOT NULL DEFAULT 'balanced',     -- consistent | balanced | refined
  allowed_top_folders TEXT,                            -- JSON array of names, NULL for any
  folders TEXT NOT NULL DEFAULT 'whole',               -- whole | split
  links TEXT NOT NULL DEFAULT 'snapshot');             -- bookmark | snapshot | download

CREATE TABLE guard_rules (id BLOB PRIMARY KEY, user_id BLOB NOT NULL REFERENCES users(id) ON DELETE CASCADE,
  kind TEXT NOT NULL CHECK (kind IN ('path', 'name', 'keyword')), pattern TEXT NOT NULL,
  action TEXT NOT NULL CHECK (action IN ('name_only', 'by_hand')));

CREATE TABLE inbox_items (id BLOB PRIMARY KEY, user_id BLOB NOT NULL REFERENCES users(id) ON DELETE CASCADE,
  kind TEXT NOT NULL CHECK (kind IN ('file', 'folder', 'link')),
  name TEXT NOT NULL, url TEXT, note TEXT,
  options TEXT NOT NULL,                               -- JSON: read_contents, folders, links
  status TEXT NOT NULL,                                -- queued | analysing | proposed | confirmed | changed |
                                                       -- rejected | auto_accepted | failed | needs_manual | undone
  error TEXT, node_id BLOB,                            -- where it is in the tree, once placed
  created_at TEXT NOT NULL, updated_at TEXT NOT NULL);

CREATE TABLE proposals (item_id BLOB PRIMARY KEY REFERENCES inbox_items(id) ON DELETE CASCADE,
  folder_id BLOB, new_folders TEXT,                    -- an existing folder, or a path of new ones under it
  name TEXT, reason TEXT NOT NULL, confidence REAL NOT NULL,
  model TEXT NOT NULL, node_version INTEGER, created_at TEXT NOT NULL);

CREATE TABLE placements (id BLOB PRIMARY KEY, item_id BLOB NOT NULL REFERENCES inbox_items(id) ON DELETE CASCADE,
  node_id BLOB NOT NULL, from_parent BLOB, from_name TEXT NOT NULL,
  to_parent BLOB NOT NULL, to_name TEXT NOT NULL,
  created_folders TEXT,                                -- JSON ids, removed by an undo when empty
  applied_at TEXT NOT NULL, undone_at TEXT);

CREATE TABLE corrections (id BLOB PRIMARY KEY, user_id BLOB NOT NULL REFERENCES users(id) ON DELETE CASCADE,
  summary TEXT NOT NULL,                               -- the item as the model saw it, short
  proposed TEXT NOT NULL, chosen TEXT NOT NULL, created_at TEXT NOT NULL);

CREATE TABLE jobs (id INTEGER PRIMARY KEY AUTOINCREMENT,
  kind TEXT NOT NULL,                                  -- analyse | extract | embed | snapshot | accept
  payload TEXT NOT NULL, status TEXT NOT NULL DEFAULT 'queued',
  attempts INTEGER NOT NULL DEFAULT 0, run_after TEXT NOT NULL, last_error TEXT);

CREATE TABLE contents (node_id BLOB PRIMARY KEY REFERENCES nodes(id) ON DELETE CASCADE,
  version INTEGER NOT NULL, text TEXT NOT NULL);       -- the extracted text, for search and proposals

CREATE VIRTUAL TABLE search USING fts5(name, path, text, content='', tokenize='unicode61 remove_diacritics 2');
-- sqlite-vec, only with an embeddings model: chunks of contents, and folder paths with short summaries.
CREATE VIRTUAL TABLE vec_chunks USING vec0(embedding float[<dims>]);
CREATE VIRTUAL TABLE vec_folders USING vec0(embedding float[<dims>]);
```

The vector tables are made when an embeddings model is set, with its
dimensions, and rebuilt when it changes.

## 5. Jobs

A small queue in SQLite (`jobs`), with one tokio worker per kind and
`AI_CONCURRENCY` for the ones that call the model. A failed job is retried
with a growing delay (three times), then the item is `failed` with the
reason, and can be retried by hand. A job survives a restart. Kinds:

- **`analyse`**: metadata, extraction, retrieval, proposal, validation,
  placement by the pending mode.
- **`extract`**: the text of tree files for search (in the background,
  throttled, when reading contents is on).
- **`embed`**: chunks and folders, when embeddings are on.
- **`snapshot`**: a link's page saved with monolith.
- **`accept`**: the timeout of pending mode (c).

## 6. Endpoints of the drop box

| Method and path | What |
| --- | --- |
| `POST /drop-box/files` | Upload into the drop box (multipart), with a note and options. Large files: the resumable uploads with `?drop_box=true` |
| `POST /drop-box/links` | A link, with a note and what to do with it |
| `GET /drop-box/items?status=` | The items, the newest first, with their proposals |
| `POST /drop-box/items/{id}/confirm` | Accept the proposal, or another folder or name (a correction) |
| `POST /drop-box/items/{id}/reject` | Back to the drop box, to place by hand |
| `POST /drop-box/items/{id}/place` | Place by hand: a folder and a name |
| `POST /drop-box/items/{id}/retry` | Analyse again |
| `POST /placements/{id}/undo` | Put it back where it came from |
| `GET /search?q=&semantic=` | Names, paths and contents (FTS5); with embeddings, semantic matches too |

The live events (`/api/events`) gain a `drop-box` event when an item
changes state.

## 7. The web UI

Pages to design as mockups, for your approval before they are built:
- **Drop box**: a drop zone with the note and per-item options, and the
  items, each with its proposal (folder, name, why, confidence) and
  Confirm, Change, Reject, Undo. A badge in the sidebar counts the items
  waiting.
- **Search**: a field in the header and a results page.
- **Settings → AI connection** (admin), with the "what is sent" text.
- **Settings → Drop box** (each user), with the "what is sent" text, and
  the sensitive-file rules.

## 8. The eval harness

`tests/eval/`: a fixture tree (a household's folders) and about 200
labelled samples (name, optional text, expected folder). I would generate
them synthetically, as no real data may be used. `mise run eval` runs them
against the configured `AI_*` model and reports placement accuracy, per
folder-freedom setting. Manual, never in CI.

## 8b. Decided during the build (2026-11-06)

- **The pages** (§7) are approved as mocked up.
- **Vectors** live in **Turso** (its `vector32` and `vector_distance_cos`),
  behind a small `VectorStore` interface, so that another engine
  (pgvector, ...) can take its place later.
- **Snapshots** are our own: the page fetched through the guarded client,
  saved as a **self-contained HTML page** (its images and styles inlined,
  scripts dropped) and as **Markdown**, next to the bookmark.
- **Sensitive-file patterns** are globs (globset), case ignored; the
  settings page explains the format and the escaping.

## 9. Questions for you

1. **The WebDAV drop box.** Recommended: a separate address,
   `/dav-inbox/`, mounted as its own small drive ("Organizer drop box"), so
   no folder name of yours is reserved. Its `review/` and `auto/`
   subfolders force "always ask" and "accept at once".
2. **Order of work.**
   1. Jobs, the AI connection and its settings.
   2. The drop box's ingest and extraction.
   3. Retrieval.
   4. Proposals, validation, placement and undo.
   5. Search.
   6. Links and snapshots.
   7. The UI (mockups first).
   8. The eval harness.

   Each step is committed and tested on its own. Proposals in step 4 are
   tested against a scripted fake model; a real one only in the eval.
3. **Anything in §2.3's description** you want said differently, or
   anything that should never be sent.
