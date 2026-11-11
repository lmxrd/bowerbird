# M5 design proposal — the desktop app

Status: **accepted 2026-11-07** (§9). **M5a built** (2026-11-08):
`packages/sync` (the engine, §3) and `packages/desktop` (the app, §4);
a CI job checks, tests and bundles it on macOS, Windows and Linux,
unsigned (§8). The bundles are not attached to releases yet. M5b (on-demand files)
is next, with its own short design. It covers
milestone M5 of [ARCHITECTURE.md](ARCHITECTURE.md) (§6.3 there): a desktop
app for macOS, Windows and Linux that keeps folders in sync with the
server, lives in the tray, and shows the drop box's proposals. The
questions are in §9.

Decided already (ARCHITECTURE.md): a Rust sync core with a Tauri shell; the
change feed (`GET /changes?since=`) plus REST and the resumable uploads as
the protocol (not WebDAV); conflict copies, never a silent overwrite; full
sync everywhere, on-demand files where the OS has an API for it (macOS
File Provider, Windows Cloud Files), Linux full sync only; sign-in through
the system browser with a loopback redirect (RFC 8252), which the device
login (M1) already allows.

## 1. Two steps

**M5a — full sync** (all three systems): sign in, choose what to sync,
two-way sync with conflict copies, the tray, the drop box folder and the
review window. Everything a user needs to work with local folders.

**M5b — on-demand files** (macOS, then Windows): files show in the system's
file manager with their size and icon but download only when opened, and
can be freed again. On macOS this is a File Provider extension: an app
extension that must be written in Swift and signed with a Developer ID, so
it is a native module beside the Rust core rather than Rust itself. On
Windows it is the Cloud Files API, from Rust through the `windows` crate.

M5b depends on M5a's engine and on signing (§8), so M5a is built first;
M5b gets its own short design once M5a works.

## 2. What syncs

- The user picks **folders** of their tree, each with a **local place**
  (by default `~/Organizer/<folder>`) and a **mode**: sync, or (M5b)
  on-demand. The root may be picked as a whole.
- **Both ways.** A change on either side is applied on the other: create,
  change, rename, move, delete.
- **Deletes go to the trash** on the server (as every delete does), and to
  the system's trash locally, never removed for good by the app.
- **Names** that are not portable on this system (`:` on Windows, case-only
  twins on macOS and Windows) are mapped to safe local names, as
  ARCHITECTURE.md §4.6 says; the mapping is kept, so renames round-trip.
- **Ignored**: `.DS_Store`, `Thumbs.db`, `desktop.ini`, `~$*` and `*.tmp`
  office lock files, `.~lock.*#`, and partial downloads of browsers; the
  list is shown in the settings.

## 3. The engine (`packages/sync`)

A library crate with no UI, tested on its own:

- **State**: a small SQLite database per account in the app's data folder:
  for each synced node, its id, local path, the server version and hash
  last seen, the local size, mtime and inode/file id last seen. With the
  change feed's cursor.
- **Remote changes**: the change feed from the saved cursor, woken by the
  live events (`/api/events`) and polled every few minutes as a fallback.
  A cursor too old for the feed (`410`) means a full listing to compare.
- **Local changes**: the `notify` watcher, debounced (a file still being
  written is waited for), and a full local scan at start and every hour, so
  a missed event is found.
- **Reconcile**: each change is compared with the state: only one side
  changed → apply it; both changed → **conflict copy**
  `Report (conflict, MacBook, 2026-11-07).docx` beside the server's, both
  kept and the user told. Moves are recognised by id remotely and by
  inode (or hash) locally, so a moved folder is moved, not re-sent.
- **Transfers**: downloads to a temporary file then renamed into place
  (nobody sees half a file); uploads plain under 4 MB, resumable above
  (the protocol the apps use), at the file's version (`if_version`), so a
  file changed on the server in the meantime becomes a conflict, not an
  overwrite. Two transfers at once, more on request.
- **Pause and resume**, offline handled (queued until the server is back),
  and a status the UI shows: up to date, syncing N, paused, offline, error.

## 4. The app (`packages/desktop`)

Tauri 2. The Rust side runs the engine; the window is React with the web
app's components (shadcn/ui, the same look), in a small Vite app of its own
that imports from `packages/frontend`, so nothing is written twice.

- **Tray** (menu bar on macOS): the status line, "Open Organizer folder",
  "Review the drop box (3)", "Pause syncing", "Settings…", "Quit".
- **Window**: the drop box's review (the web's page and its components:
  confirm, elsewhere, reject, undo), the sync status with recent activity
  and conflicts, and the settings.
- **Settings**: the account (server, signed in as, sign out), the synced
  folders (add, remove, change the local place, the mode), start at login,
  the ignore list, and the drop box folder on or off.
- **The drop box folder**: `~/Organizer/Drop box` (localised name later).
  What is put there is sent to the drop box (resumable) and removed
  locally once the server has it; the item then shows in the review.
- **Notifications**: the system's own, for proposals waiting, conflicts and
  errors (while the app runs; push is M6).
- **Sign in**: the server's address, then the system browser with a
  loopback redirect (`http://127.0.0.1:<port>/…`); the token is kept in
  the system's credential store (Keychain, Credential Manager, Secret
  Service). Sign out revokes it (`DELETE /api/auth/device`).

## 5. The API client

The apps' clients are generated from the spec; the desktop's too, for the
same reason (one source of truth, nothing written by hand): see §9,
question 2, for the two ways. The engine needs few endpoints: the change
feed, listings, downloads, uploads, moves, deletes and the events.

## 6. Repository and tools

- A **Cargo workspace at the root** with `packages/backend`,
  `packages/sync` and `packages/desktop/src-tauri`: one lockfile, one
  target directory, shared versions of tokio and reqwest. The backend's
  tasks and Dockerfile build stays as it is (`-p organizer`).
- mise tasks: `dev:desktop`, `build:desktop` (this system's app bundle),
  `test:sync`, `check:desktop`; CI builds and tests on macOS, Windows and
  Linux runners.
- Versions exact, waiting a day, as everywhere.

## 7. Testing

- The engine against a real server in-process (the backend crate's test
  helpers, as `tests/api.rs` does) and temp folders: every pair of changes
  (local/remote × create/change/rename/move/delete), conflicts, offline and
  resume, a cursor too old, non-portable names, interrupted transfers.
- The Tauri commands with the engine stubbed; the React views with Vitest
  where they hold logic; a smoke run of the built app on macOS by hand.

## 8. Signing and distribution

Unsigned builds work for development; to install without warnings, and for
M5b at all, they need signing: an Apple Developer ID (and notarisation) for
macOS, a code-signing certificate for Windows. Those are your accounts and
keys. M5a ships unsigned build artifacts from CI (`.dmg`, `.msi`,
`.AppImage`/`.deb`) attached to releases; signing and auto-update are
added when you have the accounts.

## 9. Decisions (2026-11-07)

1. **M5a first** (full sync on macOS, Windows and Linux), M5b (on-demand)
   after, with its own short design.
2. **The API client is generated with progenitor** from the spec, like the
   apps' clients; no shared `api-types` crate.
3. **Crates**: tauri 2 with its tray and the official plugins
   single-instance, autostart, notification and opener; progenitor;
   keyring; trash; notify and sqlx as in the backend; and a Cargo workspace
   at the root (§6).
4. **The screens**: approved as mocked up.
