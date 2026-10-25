# Organizer — Architecture

Status: **draft for discussion**. No code exists yet; this document records the decisions made so far and the open questions. "Organizer" is a **working name** (see §10).

## 1. What it is

Organizer is a light, self-hosted, source-available (ethical source, Hippocratic License — see §2) file storage server in the spirit of Nextcloud and Seafile, with one differentiator: a **drop box**.

You drop files, folders or links into the drop box (web UI, WebDAV `/Inbox`, mobile share sheet, desktop client). A **local AI** inspects each item — name and metadata, and optionally its contents — and proposes where it belongs in your directory tree, plus an optional better filename. You get a notification, and you confirm the placement or change it.

Besides the drop box, Organizer is an ordinary file server: browse, upload, download, search, and mount over **WebDAV**.

### Components

| Component | Tech | Purpose |
|---|---|---|
| Server | Rust, single binary | REST API, WebDAV, storage, AI pipeline, search, embedded web UI |
| Web UI | Solid + Vite (static build embedded in the server binary) | Browse files, drop box, review placements, search, settings |
| iOS app | Swift / SwiftUI | Browse files, review placements, **share extension** |
| Android app | Kotlin / Jetpack Compose | Browse files, review placements, **share target** |
| Desktop client | Rust sync core + Tauri shell with a Solid UI | Sync folders on macOS, Windows, Linux; tray; placement prompts |

## 2. Product decisions

Nearly every behaviour below is a **user setting**; the value in bold is the default.

### Drop box and AI

| Area | Decision |
|---|---|
| AI runtime | Server calls an **external AI endpoint** (Ollama, llama.cpp server, LM Studio, vLLM, or an OpenAI-compatible API) for chat completions **and embeddings**. Nothing is embedded in the binary. |
| AI freedom | Proposes an existing folder **or a new folder**, plus an **optional rename**. The user confirms both. |
| New-folder limits | Configurable: how freely the AI may create new folders (**balanced**; "consistent" prefers existing folders; "refined" allows more specific ones), plus an optional allow-list of top-level folders. |
| Pending placement | Configurable mode: **(a) move now, mark pending**; (b) stay in drop box until confirmed; (c) move now and auto-accept after a timeout; (d) auto-accept when the AI's confidence ≥ a threshold, otherwise ask. |
| Content scanning | Configurable mode: **global setting only**; per-upload only; global default with per-upload override. |
| Sensitive-file guard | Configurable path / filename / keyword rules. Matching items are **never sent to the model** — placed on name alone, or left for manual placement. |
| Scanned formats (v1) | Plain text / markdown, PDF text layer, office documents (docx, xlsx, pptx, odt, ods, odp). OCR, vision and audio come later. |
| Shared links | Chosen at share/upload time: bookmark; **bookmark + page snapshot**; download the linked file if it is one. |
| Dropped folders | Optional choice at upload time: **keep as a unit** (configurable default) or split into individual files. |
| Folder-tree context | **Retrieval-based**: the prompt gets the top-level tree plus the most relevant subtrees, found by embedding similarity over folder paths and short folder summaries. Scales to large trees. **Without an embeddings endpoint**: a pruned whole-tree prompt (depth/size limits) plus FTS5 keyword matches to pick candidate subtrees; the UI shows that semantic features are off. |
| Undo | Every applied placement is recorded and can be undone. |

### Server

| Area | Decision |
|---|---|
| Storage | **Plain filesystem** (the tree on disk is the user's real tree, browsable and backupable without Organizer) + **SQLite** for metadata. **Disk is the source of truth**; the DB is a rebuildable index (§4.1). |
| Users | **Single user** for the MVP. Data model and paths are designed so that multi-user with sharing can be added later without migration pain. |
| Auth | **Local account** (argon2) + **revocable per-device tokens / app passwords** for WebDAV, mobile and desktop. OIDC later. |
| WebDAV | Full tree exposed; the drop box appears as **`/Inbox`**, with optional `/Inbox/review` (always ask) and `/Inbox/auto` (auto-accept) subfolders that override the pending mode. |
| Sync protocol | **Cursor-based change feed + REST** for our own clients. WebDAV stays for third-party clients. |
| Uploads | **IETF Resumable Uploads for HTTP** (`draft-ietf-httpbis-resumable-upload`) for large files, and **plain uploads**. No tus. **Pinned to the interop version that current iOS URLSession speaks** (`Upload-Draft-Interop-Version`), newer versions accepted as clients adopt them, final RFC once published (draft -14 as of 2026-10-18). |
| Search | **Full-text + semantic**, both inside SQLite: **FTS5** over names, metadata and extracted text, **sqlite-vec** for embeddings. The same embeddings feed the folder-tree retrieval. Without an embeddings endpoint, search is full-text only. |
| Filenames | **Portable names enforced** (§4.6). |
| Trash | Deleted files go to a trash with retention. *(After the AI drop box milestone.)* |
| Versioning & backups | Two layers: (1) **own lightweight per-change versions** for quick "restore previous version" in the UI, with retention rules; (2) **rustic** (restic-compatible, Rust) snapshots — deduplicated, compressed, encrypted — on a user-defined schedule to the local filesystem or S3. *(After the AI drop box milestone.)* |
| Encryption | TLS in transit (via reverse proxy). **Optional end-to-end encrypted folders later**: encrypted client-side, and therefore excluded from AI, search and WebDAV. |
| Notifications | **In-app only** for the MVP: live events (SSE) to web UI, desktop and mobile apps while they are open. Push (UnifiedPush / APNs / FCM) later. |

### Clients and project

| Area | Decision |
|---|---|
| Mobile | Native **Swift / SwiftUI** and **Kotlin / Jetpack Compose**. |
| Desktop sync model | **Both, selectable per folder**: full sync, or on-demand files where the OS supports it (macOS File Provider, Windows Cloud Files API). Linux: full sync only. |
| License | **Hippocratic License 3.0** with **all modules except "US Tariff Act"** (Social Auditing, Workers on Board, Supply Chain Disclosure, Equal Pay, Fossil Fuel Divestment, Ecocide, Extractive Industries, BDS, Taliban, Myanmar, Xinjiang, Mass Surveillance, Military Activities, Law Enforcement, Media). Not OSI-approved: we describe the project as **source-available / ethical source**, not "open source". Consequences accepted: no F-Droid / Debian / Fedora distribution, no GPL/AGPL dependencies (MIT/Apache are fine). |
| Name | "Organizer" is a **working name**; a distinctive name is chosen before the first public release. |

## 3. Backend architecture and libraries

### 3.1 Library choices

| Concern | Choice | Notes |
|---|---|---|
| Async runtime | **tokio** | |
| HTTP | **axum** + tower / tower-http | REST, SSE, uploads, WebDAV and the SPA in one router. |
| Database | **sqlx** (SQLite) | WAL mode; a read pool plus **one dedicated writer** so writes queue instead of failing with `database is locked`. sqlx migrations. |
| Full-text search | **SQLite FTS5** | Same DB file and transaction as the metadata: no index drift, trivial backup. |
| Vector search | **sqlite-vec** | Loaded as an extension into the sqlx connections. Brute-force / quantised KNN; fine to ~1M vectors on a personal server. |
| Content extraction | **xberg** (MIT; successor of kreuzberg) | Only the features we need: `pdf-native`, `office`, `excel`, `html`, `chunking`. Switch to `pdf-pdfium` only if the eval set shows native PDF extraction is too weak. `ocr` later. Requires Rust ≥ 1.92. |
| LLM + embeddings | **genai** | Multi-provider client (OpenAI-compatible, native Ollama, …). Our own code owns prompts, retrieval and **validation** (§5). |
| WebDAV | **dav-server** | Custom `DavFileSystem` adapter over our core, so WebDAV writes get the same journal, IDs, inbox hooks and guard rules as REST. In-memory lock system (single node). |
| Resumable uploads | **Own implementation** of the IETF Resumable Uploads draft | No mature crate. Writes to a staging area, then moves atomically into place. |
| Job queue | **Own small queue in SQLite** | jobs table (type, payload, status, attempts, run_after) + a tokio worker per job type with a semaphore (e.g. 1 concurrent LLM job). |
| File watching | **notify** + periodic scan | §4.2. |
| Hashing | **BLAKE3** | Fast; used for file identity and change detection. |
| Backups *(M3)* | **rustic_core** | Filesystem and S3 targets. |
| Page snapshots | **monolith** (or similar) | For the "bookmark + snapshot" link mode. |
| API contract | **utoipa** | Code-first OpenAPI, generated at build time and committed; CI fails on drift. Clients: openapi-typescript (web), swift-openapi-generator (iOS), openapi-generator (Kotlin); desktop uses `api-types` directly. |
| Sessions | **tower-sessions** with a SQLite store | Web UI cookie sessions + CSRF protection. |
| Passwords | **argon2** | |
| Embedded web UI | **rust-embed** (or include_dir) | Solid build output compiled into the binary. |
| Observability | **tracing** | Structured logs; metrics later. |
| Errors | **thiserror** in libraries, **anyhow** at the binary edge | |

### 3.2 Crate layout — few crates, split later

Start with a small number of crates and split modules out only when they need isolation (heavy dependencies such as xberg for compile times, or reuse by another crate).

```
organizer/
├── Cargo.toml                 # Rust workspace
├── mise.toml                  # toolchains + tasks
├── crates/
│   ├── server/                # binary: axum router, WebDAV adapter, uploads, SSE, embedded web UI, boot config
│   ├── core/                  # domain + storage + pipeline as modules:
│   │                          #   files, journal, reconciler, change log, inbox/pipeline,
│   │                          #   ai, extract, search, jobs, auth, settings
│   ├── api-types/             # request/response types shared with clients (utoipa schemas)
│   └── sync/                  # desktop sync engine (used by apps/desktop)
├── apps/
│   ├── web/                   # Solid + Vite
│   ├── desktop/               # Tauri shell, reuses web components where sensible
│   ├── ios/                   # Xcode project: app + share extension
│   └── android/               # Gradle project: app + share target
└── docs/
```

Likely first splits: `extract` (xberg) and `backup` (rustic).

### 3.3 Configuration

- **Environment variables** (`ORGANIZER_*`) for infrastructure needed before the DB opens: data dir, bind address, log level, trusted proxy settings.
- **Database** for everything user-facing (AI endpoint and models, modes, guard rules, limits), edited in the UI/API.
- No config file.

### 3.4 Deployment

- **Docker first**: official multi-arch image (amd64 + arm64) on a slim glibc base (plus pdfium only if we switch to `pdf-pdfium`).
- **Prebuilt Linux glibc binaries** (amd64, arm64) for bare-metal installs.
- **HTTPS is the reverse proxy's job** (Caddy, Traefik, nginx…). The server speaks plain HTTP and trusts forwarded headers only from configured proxies.
- macOS and Windows servers are not supported at first.

### 3.5 Testing

- Unit and integration tests use a **scripted fake LLM provider**, so they're deterministic.
- An **eval harness** runs separately (manually or nightly, not in PR CI): a fixture tree plus ~200 labelled sample files, run against real local models to report placement accuracy per model and catch prompt regressions.
- WebDAV is checked with litmus plus real clients (macOS Finder, Windows Explorer, DAVx5, rclone).
- Resumable uploads are tested against the clients we ship (URLSession on iOS, our own web and Android code).

## 4. Server internals

### 4.1 Storage, identity and source of truth

- Data dir layout (single user now, multi-user ready):
  ```
  <data>/
  ├── organizer.db             # SQLite (metadata, FTS5, sqlite-vec, jobs, sessions)
  ├── users/<user-id>/files/   # the user's real tree
  ├── users/<user-id>/inbox/   # drop box staging
  ├── users/<user-id>/trash/   # trash (M3)
  ├── uploads/                 # in-progress resumable uploads
  ├── versions/                # per-change versions (M3)
  └── cache/                   # snapshots, extracted text, thumbnails
  ```
- **File identity**: every file and folder gets a stable ID in SQLite. External renames ("vanished + appeared" pairs) are matched by size + BLAKE3 hash, so IDs, undo history and sync state survive moves made outside Organizer.
- **Disk wins**: the filesystem is authoritative and the DB is a rebuildable index plus metadata. Every operation that changes the disk is **journaled**: an intent row is written, then the filesystem operation runs, then the intent is committed. On startup, unfinished intents are replayed or rolled back, followed by a rescan.
- SQLite holds: users, tokens, file index (ID, path, size, mtime, hash, mime), drop box items, placement proposals and undo log, journal, settings, job queue, sync change log, FTS5 and vector tables.

### 4.2 Reconciler

- **notify** watcher for fast reaction where it works, plus a **periodic mtime/size scan** (configurable interval) as the guarantee. A watcher overflow triggers a full rescan.
- Only files whose size or mtime changed are re-hashed.
- Network mounts that deliver no events (SMB, NFS, some Docker bind mounts) are covered by the periodic scan.
- Every detected change is appended to the change log that feeds sync clients.

### 4.3 API

- REST + JSON for everything the apps need; **SSE** stream for live events (new placement proposal, placement confirmed, sync changes).
- **Change feed**: `GET /changes?since=<cursor>` returns an ordered list of creates, updates, moves and deletes, plus a new cursor. Desktop and mobile clients sync from this; content moves over plain REST (download) and resumable or plain uploads.
- OpenAPI generated from `api-types` with utoipa.

### 4.4 Auth

- **Web UI**: HttpOnly, SameSite cookie session (tower-sessions, SQLite store) + CSRF protection.
- **Devices and WebDAV**: random per-device tokens / app passwords, stored hashed, listable ("iPhone, last used 2 h ago") and **revocable immediately**. No JWTs.

### 4.5 WebDAV

- Served under `/dav/` via dav-server with our own `DavFileSystem`, authenticated with app passwords.
- `/dav/Inbox/` maps to the drop box: a completed `PUT` there creates a drop box item. `/dav/Inbox/review/` and `/dav/Inbox/auto/` force "always ask" and "auto-accept" for that item.
- ETags and mtimes come from the file index, so WebDAV clients and the sync client see consistent state.

### 4.6 Filenames

- Names are stored in **Unicode NFC**.
- **Case-only collisions** within a folder are refused, or fixed automatically for AI-generated names.
- AI-generated and newly created names must be **Windows-safe** (no `: * ? " < > |`, no reserved names like `CON`/`NUL`, within length limits).
- Existing non-portable files created outside Organizer are allowed but flagged; the sync client maps them to safe local names.

### 4.7 Search

- **FTS5** over filename, path, metadata and extracted text (when content scanning is on for that file).
- **sqlite-vec** embeddings of file text chunks, folder paths and folder summaries, for semantic queries and for the folder-tree retrieval in the drop box pipeline. Only populated when an embeddings endpoint is configured; otherwise search is full-text only.
- Items under the sensitive-file guard are indexed by name only.

### 4.8 Trash, versions and backups *(M3)*

- **Trash**: deletes (web, WebDAV, sync) move to trash; retention configurable.
- **Per-change versions**: before a file is overwritten, the previous version is kept in `versions/`, with retention rules (e.g. keep all for a day, daily for a month, monthly after that). The UI offers "restore previous version".
- **Backups**: rustic snapshots of the data dir (files + database) on a user-defined schedule, to the local filesystem or S3. Deduplicated, compressed and encrypted; restore a single file or everything.

## 5. The drop box pipeline

```
ingest ─► metadata ─► [content extraction] ─► retrieve context ─► propose ─► validate ─► place ─► notify ─► review
```

1. **Ingest** — item arrives (upload, WebDAV `/Inbox`, share extension, link). Stored in `inbox/`, recorded as a drop box item with any user-supplied prompt / metadata and per-item options (scan content? keep folder as unit? link mode?). The sensitive-file guard is evaluated here.
2. **Metadata** — filename, extension, mime type, size, timestamps, embedded properties (via xberg). Links: fetch title, description, and optionally a readable snapshot or the target file.
3. **Content extraction** *(only if scanning is enabled for this item and it is not guarded)* — text via xberg, chunked and truncated/summarised to fit the model's context.
4. **Retrieve context** — embed the item and pick the most relevant subtrees from sqlite-vec (without embeddings: pruned whole tree + FTS5 keyword matches); combine with the top-level tree, the new-folder limits, and recent user corrections as examples.
5. **Propose** — ask the model (via genai) for **structured output**: target folder, whether it is new, optional new filename, short reason, confidence. Use JSON-schema constrained output where the endpoint supports it.
6. **Validate** — parse with serde and check our own rules: target exists or is an allowed new folder, portable filename, within limits. On failure, retry once with the error fed back to the model; after that, mark the item "needs manual placement".
7. **Place** — **optimistic check**: the proposal recorded the file's ID and version (hash/mtime) and the target folder's ID. At apply time:
   - file changed → re-analyse;
   - file deleted → drop the proposal;
   - target folder moved → follow its ID;
   - target folder deleted → ask the user.

   Then act according to the pending-placement mode (including the confidence threshold and `/Inbox/review` / `/Inbox/auto` overrides), and record an undo entry.
8. **Notify** — emit an event to connected clients.
9. **Review** — user confirms, edits folder/name, rejects, or undoes. Corrections are stored and used as few-shot examples for future proposals, so the AI learns the user's habits without fine-tuning.

AI work runs on the **SQLite job queue** with low concurrency, since a local model is slow and may be shared with other workloads. If the AI endpoint is down, items wait in the queue, and the user can always place them manually.

### Placement states

`queued → analysing → proposed → (confirmed | changed | rejected | auto_accepted)`, plus `failed` (with retry), `needs_manual` and `undone`.

## 6. Clients

### 6.1 Web UI (Solid)

File browser, upload (drag and drop to the drop box or to a folder; resumable for large files), review queue for proposals, search, settings (AI endpoint and models, the modes above, sensitive-file rules, devices and app passwords).

### 6.2 Mobile (Swift / Kotlin)

- File browser, review queue, upload. iOS uses URLSession's native support for resumable uploads (background-capable); Android uses our own small client for the same protocol.
- **Share target**: iOS Share Extension; Android `ACTION_SEND` / `ACTION_SEND_MULTIPLE` intent filters. Accepts files and URLs.
- The share UI offers two paths:
  - **Drop box**: optional prompt / metadata, and per-item options (scan content, link mode, keep folder as unit).
  - **Place manually**: pick a folder in the tree.

### 6.3 Desktop (Rust sync core + Tauri)

- Sync engine in `crates/sync`: local watcher + server change feed, resumable uploads, conflict handling (conflict copies, never silent overwrite).
- Per-folder mode: full sync or on-demand. On-demand uses macOS File Provider and the Windows Cloud Files API; Linux supports full sync only.
- Tray app shows sync status and placement prompts. A local "Drop box" folder feeds the server's inbox.

## 7. Milestones

| # | Milestone | Content |
|---|---|---|
| M0 | Foundations | Workspace, mise tasks, CI, lint/format, OpenAPI pipeline |
| M1 | File server | Auth + device tokens, storage + journal + reconciler, REST API, change feed, resumable + plain uploads, web UI browse/upload, WebDAV |
| M2 | AI drop box | Pipeline, job queue, extraction, LLM + embeddings client, retrieval context, proposals, validation, undo, SSE, review UI, sensitive-file guard, full-text + semantic search, settings, eval harness |
| M3 | Safety net | Trash, per-change versions, rustic backups to filesystem / S3 |
| M4 | Mobile | iOS and Android apps with share targets and review queue |
| M5 | Desktop | Full sync, then on-demand files on macOS and Windows |
| M6 | Beyond MVP | Multi-user + sharing, OIDC, push notifications, OCR / vision / audio, optional E2E-encrypted folders |

## 8. Open questions

None right now. To do before the first public release:

- Generate the exact HL 3.0 license text and identifier with the official builder ([firstdonoharm.dev/build](https://firstdonoharm.dev/build/)) and add `LICENSE.md`.
- Choose the final name (§10).

## 9. Risks to watch

- **Hashing cost**: BLAKE3 over large files on slow NAS disks during reconciliation; mitigated by hashing only on size/mtime change and throttling the scanner.
- **sqlite-vec with sqlx**: loading a C extension into every pooled connection and building it for amd64 + arm64.
- **Small local models**: placement quality depends heavily on the model; the eval harness is how we know.
- **xberg's `pdf-native`** may extract real-world PDFs worse than pdfium; measured on the eval set before M2 ships.
- **IETF upload draft churn**: the draft may change before becoming an RFC; iOS URLSession compatibility is the reference.
- **License reach**: the Hippocratic License keeps the project out of FOSS-only channels (F-Droid, distro repos) and may deter some contributors.

## 10. Name

"Organizer" collides with several existing projects (e.g. tfeldmann/organize, bsnydr/organizer). It stays as the working name; a distinctive name is chosen before the first public release. See [PRIOR_ART.md](PRIOR_ART.md).
