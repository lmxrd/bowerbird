# Organizer — Architecture

Status: **M0 and M1 done**; M2 (the AI drop box) starts by asking how the use of AI is configured. This document records the decisions made so far and the open questions (§8). "Organizer" is a **working name** (see §10).

The `M*` documents in this directory are per-milestone records: paths, task names and migration numbers in them are as of that milestone. The current layout is §3.2; the per-change migration files from before the squash are in git history.

## 1. What it is

Organizer is a light, self-hosted, source-available (ethical source, Hippocratic License — see §2) file storage server in the spirit of Nextcloud and Seafile, with one differentiator: a **drop box**.

You drop files, folders or links into the drop box (web UI, WebDAV `/Inbox`, mobile share sheet, desktop client). A **local AI** inspects each item — name and metadata, and optionally its contents — and proposes where it belongs in your directory tree, plus an optional better filename. You get a notification, and you confirm the placement or change it.

Besides the drop box, Organizer is an ordinary file server: browse, upload, download, search, and mount over **WebDAV**.

### Components

| Component | Tech | Purpose |
|---|---|---|
| Server | Rust, single binary | REST API, WebDAV, storage, AI pipeline, search, embedded web UI |
| Web UI | React 19, TanStack Router + Query, Tailwind 4, **stock shadcn/ui** (Base UI, Nova style; `sidebar-07` layout), Vite (static build embedded in the server binary) | Browse files, drop box, review placements, search, settings |
| iOS app | Swift / SwiftUI | Browse files, review placements, **share extension** |
| Android app | Kotlin / Jetpack Compose | Browse files, review placements, **share target** |
| Desktop client | Rust sync core + Tauri shell with a React UI | Sync folders on macOS, Windows, Linux; tray; placement prompts |

## 2. Product decisions

Nearly every behaviour below is a **user setting**; the value in bold is the default.

### Drop box and AI

| Area | Decision |
|---|---|
| AI runtime | Server calls an **external AI endpoint** (Ollama, llama.cpp server, LM Studio, vLLM, or an OpenAI-compatible API) for chat completions **and embeddings**. Nothing is embedded in the binary. |
| AI configuration *(decided 2026-11-06)* | **The connection** (provider, endpoint, models, API key) is the admin's: a settings page that tests the connection before it saves, with `AI_*` environment variables that lock their fields, as single sign-on works. **Any endpoint** may be used, local or cloud, with no warning; the settings say **exactly what is sent** to the model. **Several providers** through genai (OpenAI-compatible, native Ollama, Anthropic, Gemini, …), picked in the settings. **How the drop box behaves** (pending mode, reading contents, new-folder freedom, sensitive-file rules) is **each user's** own settings. See [M2_DESIGN.md](M2_DESIGN.md). |
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
| Storage | **Plain filesystem** (the tree on disk is the user's real tree, browsable and backupable without Organizer; it can be an **existing folder**, §4.1) + **SQLite** for metadata. **Disk is the source of truth** for files; the DB's file index can be rebuilt from it, the rest of the DB is data (§4.1). |
| Users | **Single user** for the MVP. Data model and paths are designed so that multi-user with sharing can be added later without migration pain. |
| Auth | **Local account** (argon2), **single sign-on through OpenID Connect**, **two-factor (TOTP)** and **API keys** — all as Potoo and Woodmouse do them (§4.4). Devices and WebDAV use revocable API keys (device tokens / app passwords). |
| WebDAV | Full tree exposed; the drop box appears as **`/Inbox`**, with optional `/Inbox/review` (always ask) and `/Inbox/auto` (auto-accept) subfolders that override the pending mode. |
| Sync protocol | **Cursor-based change feed + REST** for our own clients. WebDAV stays for third-party clients. |
| Uploads | **IETF Resumable Uploads for HTTP** (`draft-ietf-httpbis-resumable-upload`) for large files, and **plain uploads**. No tus. **Pinned to the interop version that current iOS URLSession speaks** (`Upload-Draft-Interop-Version`), newer versions accepted as clients adopt them, final RFC once published (draft -15 as of 2026-11-02; URLSession speaks interop version 6). **No 104 is sent** (decided 2026-11-06): hyper cannot send 1xx responses, and the draft makes it optional. Our clients know the server supports resumption; the iOS app runs the protocol itself rather than URLSession's automatic resumption, which waits for a 104. Add it when hyper can. |
| Search | **Full-text + semantic**, both inside SQLite: **FTS5** over names, metadata and extracted text, **sqlite-vec** for embeddings. The same embeddings feed the folder-tree retrieval. Without an embeddings endpoint, search is full-text only. |
| Filenames | **Portable names enforced** (§4.6). |
| Trash | Deleted files go to a trash with retention. Deletes are never permanent from M1 on; the trash's UI and retention come in M3. |
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
| Database | **sqlx** (SQLite) | WAL mode; a read pool plus **one dedicated writer** so writes queue instead of failing with `database is locked`. sqlx migrations: one squashed base file (`apps/server/migrations/0001_base.sql`, applied in a single transaction) plus one numbered file per change (`0002` up). |
| Full-text search | **SQLite FTS5** | Same DB file and transaction as the metadata: no index drift, trivial backup. |
| Vector search | **sqlite-vec** | Loaded as an extension into the sqlx connections. Brute-force / quantised KNN; fine to ~1M vectors on a personal server. |
| Content extraction | **xberg** (MIT; successor of kreuzberg) | Only the features we need: `pdf` (which is the native PDF extractor, `pdf-native`: that feature alone turns nothing on), `office`, `excel`, `html`; `chunking` when embeddings come. Only ever handed bytes (a URL it would fetch itself), with its cache off. Switch to `pdf-pdfium` only if the eval set shows native PDF extraction is too weak. `ocr` later. Requires Rust ≥ 1.92. |
| LLM + embeddings | **genai** | Multi-provider client (OpenAI-compatible, native Ollama, …). Our own code owns prompts, retrieval and **validation** (§5). |
| WebDAV | **dav-server** | Custom `DavFileSystem` adapter over our core, so WebDAV writes get the same journal, IDs, inbox hooks and guard rules as REST. In-memory lock system (single node). |
| Resumable uploads | **Own implementation** of the IETF Resumable Uploads draft | No mature crate. Writes to a staging area, then moves atomically into place. |
| Job queue | **Own small queue in SQLite** | jobs table (type, payload, status, attempts, run_after) + a tokio worker per job type with a semaphore (e.g. 1 concurrent LLM job). |
| File watching | **notify** + periodic scan | §4.2. |
| Hashing | **BLAKE3** | Fast; used for file identity and change detection. |
| Backups *(M3)* | **rustic_core** | Filesystem and S3 targets. |
| Page snapshots | **monolith** (or similar) | For the "bookmark + snapshot" link mode. |
| API contract | **utoipa** + **utoipa-axum** | Code-first OpenAPI, as in Potoo and Woodmouse: every handler has a `#[utoipa::path]` and is registered with `routes!` on an `OpenApiRouter`, so the router is the one list of endpoints. The `openapi` binary (`--features spec`) writes the spec to `apps/server/dist/openapi.json` (`mise run gen:spec`); it is **not committed**. The web client is **generated from it by hey-api** (fetch client, TanStack Query, zod validation; `mise run gen:sdk`) and not committed either. Mobile clients are generated from the same spec: swift-openapi-generator (iOS), openapi-generator (Kotlin); the desktop window uses the same SDK through the app's `api` command. The spec and Swagger UI are **served only when `SWAGGER_UI` is set**. |
| Single sign-on | **openidconnect** (without its default features) | Our own reqwest is handed to it, redirects off. Same design as Potoo and Woodmouse (§4.4). |
| Sessions | **axum-login** on **tower-sessions**, our own SQLite store | Web UI cookie sessions (§4.4). |
| Two-factor | **totp-rs** | TOTP + recovery codes (§4.4). |
| Rate limits | **governor** | Sign-in, code and password attempts (§4.4). |
| Cross-site protection | Own Fetch Metadata layer | As in Potoo and Woodmouse (§4.4). |
| Passwords | **argon2** | |
| Dates and times | **jiff** | No chrono or time, except where a dependency's API requires one. |
| Embedded web UI | **rust-embed** (or include_dir) | React build output compiled into the binary. |
| Observability | **tracing** | Structured logs; metrics later. |
| Errors | **thiserror** in libraries, **anyhow** at the binary edge | |

### 3.2 Repository layout — apps, packages, tooling

One directory per deployable under `apps/`, per shared library under `packages/`, per tool under `tooling/`; one pnpm workspace and lockfile and one Cargo workspace and lockfile at the root.

```
organizer/
├── package.json               # Node (LTS) and pnpm versions, read by mise
├── pnpm-workspace.yaml        # the JavaScript packages, minimumReleaseAge
├── rust-toolchain.toml        # the Rust version, read by rustup and mise
├── mise.toml                  # the other tools + every task
├── apps/
│   ├── server/                # the server crate: axum router, WebDAV, uploads, SSE, embedded web UI, and as modules
│   │                          #   files, journal, reconciler, change log, inbox/pipeline,
│   │                          #   ai, extract, search, jobs, auth, settings
│   ├── frontend/              # React + TanStack Router/Query + Tailwind + Vite, embedded into the server
│   ├── desktop/               # Tauri shell reusing the frontend's components; macos/ is the File Provider extension
│   ├── ios/                   # Xcode project: app + share extension
│   └── android/               # Gradle project: app + share target
├── packages/
│   ├── sdk/                   # TypeScript client generated from the spec (not committed)
│   ├── sync/                  # the desktop sync engine (a library, no UI)
│   ├── cloudfiles/            # Windows on-demand folders (the Cloud Files API)
│   └── e2e-crypto/            # the E2E format's reference implementation and test vectors
├── tooling/
│   ├── e2e/                   # Playwright smoke tests against the built image
│   └── decrypt/               # the organizer-decrypt CLI
├── scripts/                   # the Linux build, the end-to-end run
└── docs/
```

The server is one crate (`organizer`: the binary, its library, and the `openapi` spec writer behind the `spec` feature); the sync engine and the Cloud Files API are their own crates in `packages/`, the decrypt CLI one in `tooling/decrypt`.

### 3.3 Configuration

- **Environment variables**, unprefixed like Potoo's and Woodmouse's (`PORT`, `PUBLIC_URL`, `DATABASE_URL`, `DATA_DIR`, `TRUST_PROXY`, `SWAGGER_UI`, `TOTP_REQUIRED`, `OIDC_*` including `OIDC_ONLY`, `FILES_DIR`, `RUST_LOG`, …), for infrastructure needed before the DB opens: database, data dir, public URL, bind address, log level, trusted proxy settings.
- `DATABASE_URL` is an sqlx SQLite URL. It **defaults to `<DATA_DIR>/organizer.db`**, and self-hosters can override it.
  - The database must be on a local disk, even when the files are on a network share: SQLite's locking and WAL are unsafe on NFS/SMB. A data dir on a share is the main reason to override it.
  - A database outside the data dir must be backed up with it (§4.1).
- **Database** for everything user-facing (AI endpoint and models, modes, guard rules, limits), edited in the UI/API.
- No config file.

### 3.4 Deployment

- **A static musl binary**, as Potoo and Woodmouse ship: built outside the image with cargo-zigbuild (zig as the C toolchain, so SQLite, sqlite-vec and other C code build for musl on a Mac or Linux; `scripts/build-linux.sh`), for amd64 and arm64. Released as prebuilt Linux binaries for bare-metal installs.
- **Docker first**: the official multi-arch image is an exact Alpine release (`alpine:3.x.y` + `apk upgrade`) that only copies that binary; nothing is compiled in it. Its healthcheck is `organizer healthcheck`.
- musl holds **as long as it does not limit the libraries and programs we need**. Known points: musl's allocator is slow under many threads (switch to mimalloc if profiling shows it), and pdfium (only if we leave xberg's `pdf-native`) must come as a musl build. A dependency that only exists for glibc is a reason to revisit this, not to work around it.
- **HTTPS is the reverse proxy's job** (Caddy, Traefik, nginx…). The server speaks plain HTTP and trusts forwarded headers only from configured proxies.
- macOS and Windows servers are not supported at first.

### 3.5 Toolchain and supply chain

The same setup as Potoo and Woodmouse:

- **mise** pins every tool. Node is the current **LTS** (26 today), pinned to an exact version as the other two do.
- **Dependabot** updates cargo, npm, GitHub Actions and the image base weekly, grouped by ecosystem. Routine updates wait **5 days** after their release (`cooldown`), security updates do not.
- **Auto-merge** (`dependabot-auto-merge.yml`): Dependabot's minor, patch and security updates merge on their own once the required checks pass; majors wait for a person.
- **pnpm `minimumReleaseAge`** (one day, shorter than the cooldown so it never blocks Dependabot): a version published less than a day ago is not installed.
- **Vulnerability scans** (`security.yml`, daily and on lockfile changes): `mise run audit` checks the lockfiles with osv-scanner (accepted findings in `osv-scanner.toml`, with the reason), and `mise run audit:image` checks the published image with Trivy. CI scans the image it builds.

### 3.6 Releases

As in Potoo and Woodmouse:

- Commit messages and **pull request titles are Conventional Commits** (`feat:`, `fix:`, `ci:`, …). Pull requests are squash merged, so the commit on main takes the title. A `PR title` workflow fails a title that does not parse.
- **release-please** reads them to bump the version and write `CHANGELOG.md` (never edited by hand). Nobody tags by hand.
- Dependabot's pull requests are `fix(deps)` for what ships (so merging one releases a patch), `chore(deps-dev)` for devDependencies, and `ci` for actions. A merge made by the auto-merge workflow starts no workflow, so `release.yml` also runs daily to pick it up.

### 3.7 Testing

- The layers Potoo and Woodmouse use:
  - unit tests next to the code;
  - integration tests of the API on **axum-test**;
  - **Vitest** for the frontend's pure logic in `src/lib` (no component or visual tests);
  - a few **Playwright** end-to-end smoke tests (`tooling/e2e`, `scripts/e2e.sh`, `mise run test:e2e`) that drive a real browser against the built image.
- Unit and integration tests use a **scripted fake LLM provider**, so they're deterministic.
- An **eval harness** runs separately (manually or nightly, not in PR CI): a fixture tree plus ~200 labelled sample files, run against real local models to report placement accuracy per model and catch prompt regressions.
- WebDAV is checked with litmus plus real clients (macOS Finder, Windows Explorer, DAVx5, rclone).
- Resumable uploads are tested against the clients we ship (URLSession on iOS, our own web and Android code).

## 4. Server internals

### 4.1 Storage, identity and source of truth

- Data dir layout (single user now, multi-user ready):
  ```
  <data>/
  ├── organizer.db             # SQLite (metadata, FTS5, sqlite-vec, jobs, sessions); DATABASE_URL can move it
  ├── users/<user-id>/files/   # the user's real tree, by default (see "Existing folders")
  ├── users/<user-id>/inbox/   # drop box staging
  ├── users/<user-id>/trash/   # trash (M3)
  ├── uploads/                 # in-progress resumable uploads
  ├── versions/                # per-change versions (M3)
  └── cache/                   # snapshots, extracted text, thumbnails
  ```
- **File identity**: every file and folder gets a stable ID in SQLite. External renames ("vanished + appeared" pairs) are matched by size + BLAKE3 hash, so IDs, undo history and sync state survive moves made outside Organizer.
- **Disk wins**: the filesystem is authoritative for files; the DB's file index can be rebuilt from it, and the rest of the DB is data (see Backups below). Every operation that changes the disk is **journaled**: an intent row is written, then the filesystem operation runs, then the intent is committed. On startup, unfinished intents are replayed or rolled back, followed by a rescan.
- SQLite holds: users, tokens, file index (ID, path, size, mtime, hash, mime), drop box items, placement proposals and undo log, journal, settings, job queue, sync change log, FTS5 and vector tables.
- **Existing folders**: a user's tree can be a directory that already exists, such as a NAS share mounted into the container. Organizer indexes it in place and does not copy it.
  - `users/<user-id>/files/` is only the default.
  - Organizer's own folders (inbox, trash, versions, uploads, cache) stay in the data dir and are never created inside the user's tree.
  - The first index of a large tree runs in the background, hashing throttled as in §4.2.
- **Backups contain data only**:
  - The files (tree, versions, trash) and the database: users, credentials, API keys, settings, drop box items, proposals, the undo log and corrections.
  - Sessions, the job queue, in-progress uploads and the cache are not data. They are left out or dropped on restore.
  - The file index, FTS5 and embeddings could be rebuilt from disk, but they are kept: rebuilding re-hashes and re-embeds everything and gives files new IDs, which breaks undo and sync state.
  - Until rustic backups (M3), an operator copies the data dir (and the database, if `DATABASE_URL` puts it elsewhere) while the server is stopped.

### 4.2 Reconciler

- **notify** watcher for fast reaction where it works, plus a **periodic mtime/size scan** (configurable interval) as the guarantee. A watcher overflow triggers a full rescan.
- Only files whose size or mtime changed are re-hashed.
- Network mounts that deliver no events (SMB, NFS, some Docker bind mounts) are covered by the periodic scan.
- Every detected change is appended to the change log that feeds sync clients.

### 4.3 API

- REST + JSON for everything the apps need; **SSE** stream for live events (new placement proposal, placement confirmed, sync changes).
- **Change feed**: `GET /changes?since=<cursor>` returns an ordered list of creates, updates, moves and deletes, plus a new cursor. Desktop and mobile clients sync from this; content moves over plain REST (download) and resumable or plain uploads.
- The OpenAPI spec is generated from the handlers with utoipa (`mise run gen:spec`); the TypeScript SDK from the spec (`mise run gen:sdk`), and the mobile clients from the spec at their build time.

### 4.4 Auth

Sessions, two-factor, API keys, limits and cross-site protection work as in Potoo and Woodmouse:

- **Sessions** (web UI): axum-login on tower-sessions.
  - The store is ours, a `sessions` table keyed by the SHA-256 of the session id. The community's sqlx store needs sqlx 0.8.
  - Every sign-in ends in one `sign_in` function (a new session id, 30 days).
  - axum-login loads the user on each request and checks the session against the user's `auth_hash` (password hash + TOTP secret), so changing either signs the other sessions out.
  - Cookie: `SameSite=Strict`, `HttpOnly`, and `__Host-` + `Secure` when `PUBLIC_URL` is https.
- **Two-factor** (TOTP, `totp-rs`):
  - Secret on the user, `totp_last_step` so a code works once, ten single-use recovery codes stored as SHA-256.
  - A sign-in of a user with it on gets a signed five-minute challenge cookie, not a session.
  - `TOTP_REQUIRED` makes everyone enrol. `OIDC_ONLY` turns password sign-in off.
- **API keys**: `<prefix>_<random>`, stored as SHA-256, with a name, a scope (`read` / `write` / `admin`) and `last_used_at`. Listable and **revocable immediately**; no JWTs.
  - A device token is an API key made by the device login (below).
  - An app password for WebDAV is an API key used as the Basic-auth password.
  - Account endpoints (password, TOTP, keys) refuse keys and need a session.
- **Limits** (governor): every attempt counts, over the quota is a 429 with `Retry-After`, never a lockout.
- **Cross-site requests** (Fetch Metadata, a layer outside the sessions). A POST/PUT/PATCH/DELETE passes when one of these holds:
  - `Sec-Fetch-Site` is `same-origin` or `none`;
  - without that header, `Origin` matches `PUBLIC_URL` (or `Host`);
  - neither header is present (not a browser).

  `same-site` is refused. Bearer requests (API keys, devices) skip it. Basic authentication does not, because a browser resends remembered Basic credentials by itself; WebDAV clients pass anyway, as they send neither header.
- **Device login** (mobile, desktop):
  1. The user enters the server URL.
  2. The app asks the server which sign-in methods it allows (password, single sign-on with its label; the same answer the web login page uses).
  3. The user picks one and signs in:
     - **Password**: the app posts the email and password, then the TOTP step when asked, and gets an API key for the device.
     - **Single sign-on**: the app opens the system browser (ASWebAuthenticationSession on iOS, Custom Tabs on Android, the default browser on desktop) at the server's login URL, with a PKCE challenge and its redirect (`organizer://…` or a loopback port on desktop, per RFC 8252).
  4. After the provider (and the TOTP step), the server redirects to the app with a one-time code valid for a minute.
  5. The app exchanges the code and its PKCE verifier for the device's API key.

  The app redirects are a fixed allow-list. The web callback still goes only to `/` or `/login?error=<code>`.
- **Single sign-on** (OpenID Connect), implemented as in Potoo and Woodmouse:
  - Settings are the `oidc` document of the `server_settings` key-value table (the shapes and validation are in code, `src/server_settings.rs`). The `OIDC_*` environment variables override it field by field and lock the fields they set. An admin's save takes effect only after the provider answers.
  - The login in progress (state, nonce, PKCE verifier, callback URL) is not stored on the server. It lives in a signed `__Host-` cookie, valid for ten minutes, signed with a key made at start.
  - The provider's metadata is cached per issuer. Its endpoints must be https, and its answers are size-limited.
  - Provider accounts are linked to users in `user_identities` (issuer + subject). An unlinked account is matched only by an email the provider says is verified.
  - `password_hash` is NULL for a user without a password.
  - The callback only redirects, to `/` or `/login?error=<code>`. The backend and frontend keep the same list of error codes.
  - Nobody signs in through the provider before the first admin exists.
  - While Organizer is single-user, single sign-on only links a provider account to that one user. Creating users on first sign-in (`OIDC_AUTO_CREATE`) comes with multi-user.
  - Tests run a stand-in provider that signs real ID tokens.

### 4.5 WebDAV

- Served under `/dav/` via dav-server with our own `DavFileSystem`, authenticated with app passwords (API keys as the Basic-auth password, §4.4).
- `/dav/Inbox/` maps to the drop box: a completed `PUT` there creates a drop box item. `/dav/Inbox/review/` and `/dav/Inbox/auto/` force "always ask" and "auto-accept" for that item.
- Reads (listings, metadata, downloads) go through dav-server's `LocalFs`; every change goes through the journaled operations, like the REST API's (a DELETE goes to the trash, a MOVE keeps the id). ETags are dav-server's own (modification time and size): WebDAV clients and the sync clients never compare each other's.

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
- **Backups**: rustic snapshots of the data dir (files + database) on a user-defined schedule, to the local filesystem or S3. Deduplicated, compressed and encrypted; restore a single file or everything. Built as [M3_DESIGN.md](M3_DESIGN.md) describes.

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

### 6.1 Web UI (React)

Stock shadcn/ui on Base UI with the Nova style, as `shadcn init` sets it up, and its `sidebar-07` block (a sidebar that collapses to icons) as the layout. Each page is presented for approval before it is built.

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
| M0 | Foundations | Workspace, mise tasks, CI, lint/format, OpenAPI pipeline, Dependabot + auto-merge, vulnerability scans |
| M1 | File server | Auth (local accounts, OIDC single sign-on, two-factor, API keys / device tokens, device login, limits, cross-site protection), storage + journal + reconciler, REST API, change feed, resumable + plain uploads, a minimal trash (no UI), web UI browse/upload, WebDAV |
| M2 | AI drop box | Pipeline, job queue, extraction, LLM + embeddings client, retrieval context, proposals, validation, undo, SSE, review UI, sensitive-file guard, full-text + semantic search, settings, eval harness |
| M3 | Safety net | Trash, per-change versions, rustic backups to filesystem / S3 |
| M4 | Mobile | iOS and Android apps with share targets and review queue |
| M5 | Desktop | Full sync, then on-demand files on macOS and Windows |
| M6 | Beyond MVP | Multi-user + sharing, push notifications, OCR / vision / audio, optional E2E-encrypted folders |
| M7 | Drives and policy | Drives, roles and teams, settings levels with a superadmin, storage backends, mirrors, federation ([M7_DESIGN.md](M7_DESIGN.md)) |

## 8. Open questions

Decided on 2026-11-06 and no longer open: a static musl binary (§3.4); `DATABASE_URL`, so that another database (PostgreSQL) can be supported later (§3.3); the `packages/` layout (§3.2); versions in `package.json`, `rust-toolchain.toml` and `mise.toml`; and the conventions Potoo and Woodmouse share (shadcn/ui, an exact Alpine release with a `healthcheck` subcommand, typed API errors that keep internal details out, "use the community's crates"), which are in `AGENTS.md`. The IBM patent US11748305B2 was looked at and is not a concern: many tools suggest folders, and its claim is a specific tree walk we do not use. "Organizer" stays the name for now.

### Before M1

- **M1 schema and endpoints.** Proposed in [M1_DESIGN.md](M1_DESIGN.md), awaiting review (its §6 has the questions). It covers:
  - tables for the file index, journal, change log, API keys, settings, sessions and the files root of a user;
  - the REST resources;
  - the change feed's cursor, and how long deletions stay in it;
  - how an existing folder is configured as a user's tree.
- **Mobile builds need the spec.** It is not committed, so the iOS and Android builds (and their CI) must run `gen:spec` (Rust) before generating their clients.

### Before M2

- **How the use of AI is configured:** decided (§2, "AI configuration"). The M2 design is proposed in [M2_DESIGN.md](M2_DESIGN.md), for review before it is built.
- **Eval set**: the fixture tree and ~200 labelled sample files do not exist yet.

### Later

- **Licence check for dependencies.** The Hippocratic License rules out GPL/AGPL dependencies, but nothing enforces it yet (cargo-deny for crates, and an npm licence check). Neither Potoo nor Woodmouse has one either.

### Before the first public release

- Choose the final name (§10).
- PRIOR_ART.md still mentions a pending "MIT vs Hippocratic vs GPL" decision.

## 9. Risks to watch

- **Hashing cost**: BLAKE3 over large files on slow NAS disks during reconciliation; mitigated by hashing only on size/mtime change and throttling the scanner.
- **sqlite-vec with sqlx**: loading a C extension into every pooled connection and building it for amd64 + arm64.
- **Small local models**: placement quality depends heavily on the model; the eval harness is how we know.
- **xberg's `pdf-native`** may extract real-world PDFs worse than pdfium; measured on the eval set before M2 ships.
- **IETF upload draft churn**: the draft may change before becoming an RFC; iOS URLSession compatibility is the reference.
- **No 104 from hyper**: until hyper can send 1xx responses, URLSession's automatic resumption is out of reach (the iOS app implements the protocol itself).
- **License reach**: the Hippocratic License keeps the project out of FOSS-only channels (F-Droid, distro repos) and may deter some contributors.

## 10. Name

"Organizer" collides with several existing projects (e.g. tfeldmann/organize, bsnydr/organizer). It stays as the working name; a distinctive name is chosen before the first public release. See [PRIOR_ART.md](PRIOR_ART.md).
