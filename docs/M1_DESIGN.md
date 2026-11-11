# M1 design proposal — schema and endpoints

Status: **accepted 2026-11-06** (§6 records the answers), implemented: §5 steps 1 to 7 are done (the web UI's pages were approved as mockups first). It covers
milestone M1 (the file server) of [ARCHITECTURE.md](ARCHITECTURE.md): auth,
storage with the journal and the reconciler, the REST API, the change feed,
uploads, the web UI's needs and WebDAV. The AI drop box, search, jobs and
the trash come later and are left out on purpose, except where M1 must leave
room for them. The questions that need a decision are in §6.

Conventions taken from Potoo and Woodmouse:
- ids are UUIDv7, stored as 16-byte `BLOB`s, sent as strings;
- times are UTC `TEXT` (`strftime('%Y-%m-%dT%H:%M:%fZ')`), handled with jiff;
- flags are booleans in the API (`INTEGER` 0/1 in SQLite);
- every table that belongs to a user has `user_id` with `ON DELETE CASCADE`.

## 1. Tables

### 1.1 Auth (the same shapes as Potoo)

```sql
CREATE TABLE users (
  id BLOB PRIMARY KEY NOT NULL CHECK (length(id) = 16),
  email TEXT NOT NULL UNIQUE,
  password_hash TEXT,                       -- NULL: single sign-on only
  is_admin INTEGER NOT NULL DEFAULT 0,
  totp_secret TEXT,                         -- NULL: two-factor off
  totp_pending TEXT,                        -- a setup not yet confirmed
  totp_last_step INTEGER NOT NULL DEFAULT 0,
  files_root TEXT,                          -- NULL: FILES_DIR, else <DATA_DIR>/users/<id>/files (§4)
  created_at TEXT NOT NULL DEFAULT (strftime('%Y-%m-%dT%H:%M:%fZ', 'now'))
);

CREATE TABLE user_identities (              -- single sign-on links
  issuer TEXT NOT NULL,
  subject TEXT NOT NULL,
  user_id BLOB NOT NULL REFERENCES users(id) ON DELETE CASCADE,
  created_at TEXT NOT NULL DEFAULT (strftime('%Y-%m-%dT%H:%M:%fZ', 'now')),
  PRIMARY KEY (issuer, subject)
);

CREATE TABLE oidc_settings (                -- one row; OIDC_* variables lock fields
  id INTEGER PRIMARY KEY CHECK (id = 1),
  issuer TEXT NOT NULL DEFAULT '',
  client_id TEXT NOT NULL DEFAULT '',
  client_secret TEXT NOT NULL DEFAULT '',
  scopes TEXT NOT NULL DEFAULT '',
  label TEXT NOT NULL DEFAULT '',
  redirect_url TEXT NOT NULL DEFAULT '',
  enabled INTEGER NOT NULL DEFAULT 1
);

CREATE TABLE recovery_codes (
  user_id BLOB NOT NULL REFERENCES users(id) ON DELETE CASCADE,
  code_hash TEXT NOT NULL,                  -- SHA-256
  PRIMARY KEY (user_id, code_hash)
);

CREATE TABLE api_keys (                     -- API keys, device tokens and app passwords
  id BLOB PRIMARY KEY NOT NULL CHECK (length(id) = 16),
  user_id BLOB NOT NULL REFERENCES users(id) ON DELETE CASCADE,
  kind TEXT NOT NULL CHECK (kind IN ('key', 'device', 'app_password')),
  name TEXT NOT NULL,                       -- "iPhone", "rclone on the NAS"
  prefix TEXT NOT NULL,                     -- shown in the list: "org_3f9a…"
  token_hash TEXT NOT NULL UNIQUE,          -- SHA-256 of the token
  scope TEXT NOT NULL CHECK (scope IN ('read', 'write', 'admin')),
  created_at TEXT NOT NULL DEFAULT (strftime('%Y-%m-%dT%H:%M:%fZ', 'now')),
  last_used_at TEXT,                        -- written at most once a minute
  expires_at TEXT
);

CREATE TABLE device_codes (                 -- the one-time codes of the device login (§3.2)
  code_hash TEXT PRIMARY KEY,               -- SHA-256 of the code
  user_id BLOB NOT NULL REFERENCES users(id) ON DELETE CASCADE,
  code_challenge TEXT NOT NULL,             -- PKCE S256, from the app
  redirect_uri TEXT NOT NULL,
  device_name TEXT NOT NULL,
  expires_at TEXT NOT NULL                  -- one minute; deleted when used
);

CREATE TABLE sessions (                     -- tower-sessions, our store
  id_hash TEXT PRIMARY KEY,
  data TEXT NOT NULL,
  expires_at INTEGER NOT NULL
);
```

`kind` is the one addition to Potoo's `api_keys`. One table for all three
is what Nextcloud does too (its `authtoken` table holds sessions, devices
and app passwords with a `type`): the same hashing, listing, last-used and
revocation code serves every kind. All three kinds are the
same token underneath: `kind` decides where they are listed and what
they may do.
- An `app_password` is accepted only as a WebDAV Basic-auth password.
- A `device` is made only by the device login.
- A `key` is made by hand in the settings.

### 1.2 Files

```sql
CREATE TABLE nodes (                        -- the file index: one row per file or folder
  id BLOB PRIMARY KEY NOT NULL CHECK (length(id) = 16),
  user_id BLOB NOT NULL REFERENCES users(id) ON DELETE CASCADE,
  parent_id BLOB REFERENCES nodes(id),      -- NULL only for the user's root
  name TEXT NOT NULL,                       -- as on disk (NFC when Organizer made it)
  kind TEXT NOT NULL CHECK (kind IN ('file', 'folder')),
  size INTEGER NOT NULL DEFAULT 0,
  mtime_ns INTEGER NOT NULL,                -- from the filesystem
  hash BLOB,                                -- BLAKE3, 32 bytes; files only, NULL until hashed
  mime TEXT,
  version INTEGER NOT NULL DEFAULT 1,       -- bumped on every content or name/place change
  portable INTEGER NOT NULL DEFAULT 1,      -- 0: a name that breaks the rules of §4.6
  created_at TEXT NOT NULL DEFAULT (strftime('%Y-%m-%dT%H:%M:%fZ', 'now')),
  updated_at TEXT NOT NULL DEFAULT (strftime('%Y-%m-%dT%H:%M:%fZ', 'now')),
  UNIQUE (parent_id, name)
);
CREATE UNIQUE INDEX nodes_root ON nodes (user_id) WHERE parent_id IS NULL;
CREATE INDEX nodes_hash ON nodes (user_id, size, hash);  -- matching external moves

CREATE TABLE journal (                      -- intent → filesystem operation → commit
  id INTEGER PRIMARY KEY AUTOINCREMENT,
  user_id BLOB NOT NULL REFERENCES users(id) ON DELETE CASCADE,
  op TEXT NOT NULL CHECK (op IN ('create_folder', 'write', 'move', 'delete')),
  node_id BLOB,
  from_path TEXT,                           -- relative to the files root
  to_path TEXT,
  created_at TEXT NOT NULL DEFAULT (strftime('%Y-%m-%dT%H:%M:%fZ', 'now'))
);                                          -- a row is deleted when its operation is committed

CREATE TABLE changes (                      -- the change feed (§2.3)
  seq INTEGER PRIMARY KEY AUTOINCREMENT,    -- the cursor
  user_id BLOB NOT NULL REFERENCES users(id) ON DELETE CASCADE,
  node_id BLOB NOT NULL,                    -- not a foreign key: deletions stay listed
  op TEXT NOT NULL CHECK (op IN ('create', 'update', 'move', 'delete')),
  parent_id BLOB,                           -- after the change
  name TEXT,                                -- after the change
  version INTEGER,
  at TEXT NOT NULL DEFAULT (strftime('%Y-%m-%dT%H:%M:%fZ', 'now'))
);
CREATE INDEX changes_user ON changes (user_id, seq);

CREATE TABLE uploads (                      -- resumable uploads in progress
  id BLOB PRIMARY KEY NOT NULL CHECK (length(id) = 16),
  user_id BLOB NOT NULL REFERENCES users(id) ON DELETE CASCADE,
  parent_id BLOB NOT NULL,                  -- the target folder
  name TEXT NOT NULL,
  replaces BLOB,                            -- the file it overwrites, if any
  if_version INTEGER,                       -- ... at this version
  length INTEGER,                           -- NULL until the client says it
  offset INTEGER NOT NULL DEFAULT 0,
  created_at TEXT NOT NULL DEFAULT (strftime('%Y-%m-%dT%H:%M:%fZ', 'now')),
  expires_at TEXT NOT NULL                  -- 24 hours after the last byte
);                                          -- the bytes are in <DATA_DIR>/uploads/<id>
CREATE TABLE trash (                        -- deleted files and folders (§6, question 1)
  id BLOB PRIMARY KEY NOT NULL CHECK (length(id) = 16),  -- the node's id, kept
  user_id BLOB NOT NULL REFERENCES users(id) ON DELETE CASCADE,
  name TEXT NOT NULL,
  kind TEXT NOT NULL CHECK (kind IN ('file', 'folder')),
  original_parent_id BLOB,                  -- where it was, for a restore
  original_path TEXT NOT NULL,              -- relative to the files root, if the parent is gone
  size INTEGER NOT NULL,
  deleted_at TEXT NOT NULL DEFAULT (strftime('%Y-%m-%dT%H:%M:%fZ', 'now'))
);                                          -- the bytes are in <DATA_DIR>/users/<id>/trash/<id>
```

Why it looks like this:
- **No path column.** A path is resolved one name at a time through
  `UNIQUE (parent_id, name)`, so a folder move is one row, not one row per
  descendant. WebDAV and `by-path` lookups cost one indexed query per level.
- **No case-insensitive unique index.** Folders made outside Organizer may
  already hold `Report.pdf` and `report.pdf`. The rule "refuse case-only
  collisions" is checked in code for names Organizer makes, where Rust can
  do Unicode case folding (SQLite's `lower` is ASCII only).
- **`version`** is the ETag and the optimistic check of every write
  (`if_version`, `If-Match`), and what a placement proposal will record in M2.
- **The journal holds only what is in flight.** A row is written before the
  filesystem operation and deleted once it is done. At start, leftover rows
  are replayed or rolled back, then a rescan follows.

**The trash in M1** has no UI yet: a delete (REST, WebDAV, sync) moves
the file or folder out of the tree into `<DATA_DIR>/users/<id>/trash/<id>`
through the journal and records it in `trash`. Restore and emptying get
endpoints now and a UI in M3, with the retention rules.

Left for later milestones, with room made now: `inbox_items`,
`placements`, `jobs` and `settings` (M2, after the AI configuration is
decided); FTS5 and sqlite-vec tables (M2); the trash's UI and retention, and versions (M3);
sharing (M6, through `user_id` on every row).

## 2. Endpoints

Under `/api` like the other two apps, each with a `#[utoipa::path]`.
WebDAV is the one exception and is mounted at `/dav`.

### 2.1 Auth and account (Potoo's paths, plus the device login)

| Method and path | What |
| --- | --- |
| `GET /auth/session` | `needsSetup`, the signed-in user, and the sign-in methods on offer (`oidc` with its label, `password_login`, `totp_required`). The web login page and the apps both read it |
| `POST /auth/setup` | The first admin (email, password; the files root comes with M1.2) |
| `POST /auth/login` · `POST /auth/logout` | Password sign-in (answers `LoginStep` when the TOTP step follows) |
| `POST /auth/login/totp` · `…/login/totp/setup` · `…/login/totp/enable` | The TOTP step, and the enrolment at sign-in with `TOTP_REQUIRED` |
| `GET /auth/oidc/login` · `GET /auth/oidc/callback` | Single sign-on. `login` takes `next` (only the device approval page, `/settings/devices/approve?…`), which the callback goes on to |
| `POST /auth/device/password` | Device login with a password, in one call: email, password, device name, and the TOTP code once asked for → a token of kind `device` (scope `write`) |
| `GET /auth/device/authorize` | An app's sign-in in the browser: to the approval page `/settings/devices/approve`, through the web login first |
| `POST /auth/device/authorize` | The approval page allows the device (session only) → the app's redirect with a one-time code |
| `POST /auth/device/token` | The one-time code + PKCE verifier → a token of kind `device` |
| `POST /auth/email` · `POST /auth/password` | The signed-in user's email and password |
| `POST /auth/totp/setup` · `…/enable` · `…/disable` · `…/replace` · `…/replace/confirm` · `…/recovery-codes` | Two-factor settings |
| `GET/POST /auth/api-keys` · `DELETE /auth/api-keys/{id}` | API keys and app passwords: list, make, revoke (devices too, once made by the apps) |
| `GET/PUT /admin/oidc` | Single sign-on settings |
| `GET/PATCH /admin/storage` | The files root (§4) |

### 2.2 Files

| Method and path | What |
| --- | --- |
| `GET /files/root` | The user's root folder |
| `GET /files/by-path?path=/a/b` | A node by its path (for web URLs and WebDAV clients) |
| `GET /files/{id}` | A node: name, kind, size, mime, version, times, path |
| `GET /files/{id}/children?after=&limit=` | A folder's children, folders first, by name, paged |
| `POST /files/{id}/folders` | A new folder in it (`{name}`) |
| `PATCH /files/{id}` | Rename or move (`{name?, parent_id?, if_version}`); 409 on a stale version or a name clash |
| `DELETE /files/{id}?if_version=` | Delete: moves it to the trash |
| `GET /trash` · `POST /trash/{id}/restore` · `DELETE /trash/{id}` | The trash: list, restore (to where it was, or to the root if that is gone), delete for good |
| `GET /files/{id}/content` | Download: `Range`, `ETag` (= version), `If-None-Match`; `?inline` for previews |
| `PUT /files/{id}/content` | Overwrite with a small body, `If-Match` required |
| `POST /files/{id}/files` | A new file in a folder, multipart, for small files (the web UI uses resumable uploads above a size limit) |

`GET /files/{id}` gives the path too, worked out on the way up, so the web
UI can show breadcrumbs.

### 2.3 Change feed and live events

`GET /changes?cursor=&limit=` answers
`{ changes: [...], cursor: "…", has_more: bool }`. Each change has its
`op`, the node's id, and its `parent_id`, `name` and `version` after the
change.
- **Cursor:** an opaque string (`<epoch>.<seq>`), read with no cursor at
  all for "from now on". A client starts with a full listing, keeps the
  cursor, and asks again. The epoch changes when the tree is indexed from
  scratch (another files folder), so no cursor of the old tree passes.
- **Coalescing:** several changes to one node between two reads come back
  as its last one.
- **Retention:** rows older than the retention (§6, question 3) are
  pruned. A cursor older than the oldest row answers `410 Gone`, and the
  client lists everything again.

`GET /events` is a Server-Sent Events stream for the open apps. In M1 it
only says "changes are waiting" (with the newest cursor), so clients go to
`/changes` instead of polling. M2 adds the drop box events.

### 2.4 Resumable uploads (IETF draft)

These follow `draft-ietf-httpbis-resumable-upload`:

| Method and path | What |
| --- | --- |
| `POST /uploads?parent_id=&name=` (or `?replaces=&if_version=`) | Start, optionally with the first bytes and `Upload-Complete`. Answers `201` with `Location: /api/uploads/{id}` (or the file, when complete). No `104`: hyper cannot send one |
| `HEAD /uploads/{id}` | `Upload-Offset`, `Upload-Complete` |
| `PATCH /uploads/{id}` | Append at `Upload-Offset` (`application/partial-upload`) |
| `DELETE /uploads/{id}` | Cancel |

The bytes go to `<DATA_DIR>/uploads/<id>`.
- **When complete:** the file is hashed and moved into the tree through
  the journal: new, or replacing `replaces` at `if_version`.
- **When abandoned:** an upload is removed 24 hours after its last byte.

The interop version is pinned to what the current iOS `URLSession`
speaks. Newer versions are accepted as clients adopt them.

### 2.5 WebDAV (`/dav`)

The user's tree, at `/dav/`, through dav-server and our own `DavFileSystem`.
- **Auth:** Basic, with an app password as the password.
- **Writes:** go through the same journal and change log as REST.
- **ETags:** dav-server's own (modification time and size), not the
  index's `version`: WebDAV clients and the sync clients never compare
  each other's. Revisit if a client needs them to match.
- **Inbox:** the drop box over WebDAV arrives in M2. Where it is mounted
  is §6, question 4.

## 3. Flows

### 3.1 First run
`/setup` makes the first admin and sets the files root (§4). Nobody can
sign in through the provider before that, as in the other two apps.

### 3.2 Device login
1. The app asks for the server URL, then calls `GET /auth/session` for the
   methods on offer.
2. **Password**: `POST /auth/device/password` with the email, the password
   and the device's name. A user with two-factor authentication on gets
   `totp_required`; the app asks for the code and sends everything again
   with it. The answer is the device's token (kind `device`, scope
   `write`). With `TOTP_REQUIRED`, someone who has not set it up does so in
   the web app first; with `OIDC_ONLY`, the browser is the way.
3. **Through the browser** (single sign-on, or any way the web login
   offers), as RFC 8252 has it:
   1. The app opens the system browser at
      `/api/auth/device/authorize?redirect_uri=organizer://auth&code_challenge=…&device_name=…`.
   2. Someone not signed in goes through the web login first (password,
      single sign-on and the TOTP step all work as usual; `next` brings
      them back), then to the approval page `/settings/devices/approve`.
   3. The page asks to allow the device and posts the approval: the server
      answers the app's redirect with a one-time code (a minute), and the
      page sends the browser there.
   4. The app posts the code and its PKCE verifier to
      `POST /auth/device/token` and gets the device's token.

   Redirects go only to `organizer://auth` or this machine's loopback
   address (`http://127.0.0.1:<port>/…`, a desktop app). The approval page
   is UI: it is presented for approval before it is built.

## 4. The files root (existing folders)

The **files dir** is the user's own tree: their real files, possibly a
folder that already exists, such as a NAS share. It is not the **data dir**
(`DATA_DIR`), which holds Organizer's own things: the database, the drop
box, uploads in progress, the trash, versions and caches. They are kept
apart so that the user's tree never fills with Organizer's files, and so
that the tree can live on a share while the database stays on a local disk.

`users.files_root` is the absolute path of a user's tree. Where it comes
from, first match wins:
1. **`FILES_DIR`**, an environment variable. It is the files dir of the
   first admin and locks the field, as the `OIDC_*` variables do. This is
   for compose files and other configuration kept in code.
2. **The setup page**, when the first admin is made, and the admin
   settings later (`PATCH /admin/storage`).
3. **The default**, `<DATA_DIR>/users/<id>/files`, when neither says
   otherwise (NULL in the column). Checks when it is set:
- the path exists, and the server can read, write and make folders in it;
- it is not inside the data dir, and the data dir is not inside it;
- `DATABASE_URL`'s file is not inside it.

Setting it starts a full index of that tree in the background, with
hashing throttled. Until the index is done, the tree is browsable from disk
but changes are not yet in the feed. Changing the root of a user who
already has files is an index from scratch: the old ids, the change feed
and (later) the undo history of the old tree are dropped, and connected
clients get `410` and list everything again.

## 5. What is built in which order in M1

1. Auth: the tables of §1.1, sessions, password, OIDC, TOTP, API keys,
   limits, cross-site protection (ported from Potoo, adapted to organizer).
2. The files root, the index (`nodes`), the reconciler (watcher plus
   periodic scan), the journal.
3. The REST files API, downloads, small uploads, the change feed, SSE.
4. Resumable uploads.
5. The device login.
6. WebDAV.
7. The web UI: login, setup, the file browser, upload, settings. **Each
   page is presented for approval before it is built**, on shadcn's
   `sidebar-07`.

## 6. Decisions (2026-11-06)

1. **A minimal trash comes into M1** (§1.2): deletes are never permanent.
   It has endpoints, but no UI until M3.
2. **`FILES_DIR`** (§4): an optional environment variable for the first
   admin's files dir, which locks the field. Otherwise the folder is set on
   the setup page and in the admin settings.
3. **The change feed keeps 30 days.**
4. **Where the WebDAV drop box goes** is still open. It is M2, so nothing
   in M1 depends on it.
5. **One table** for API keys, device tokens and app passwords (§1.1), as
   Nextcloud does.
