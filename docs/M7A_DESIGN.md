# M7a design proposal — drives on local storage, and the superadmin

Status: **accepted 2026-11-09** (§7), **built** (§8). Step 1 of
[M7_DESIGN.md](M7_DESIGN.md) (§9). Roles, teams and settings levels are
step 2; storage backends other than the server's own disk are step 3.

## 1. Drives

- **A drive** is a tree with its own root, trash, versions and storage
  limit. Two kinds now:
  - **My drive**: one per user, the tree they have today (their files
    folder), nothing moved on disk.
  - **Shared drives**: trees of their own under `<DATA_DIR>/drives/<id>`
    (or an existing folder the superadmin picks), outside every user's
    tree, seen only by their members. In this step the superadmin makes
    them and names their members; a member may do what an editor may, and
    the one who made it, or whom they name, manages it (adds and removes
    members, renames it). The five roles and teams replace this in step 2.
- **Trash and versions move into each drive**: `<drive root>/.organizer/`
  holds them (moved there once, at the first start, from
  `<DATA_DIR>/users/<id>/`). That folder is hidden everywhere: listings,
  scans, search, WebDAV, sync, the apps. Storage counts per drive.
- **What a user has**: My drive, the shared drives they are a member of,
  and Shared with me (M6b's shares) — the sidebar lists them so.
- **Shares (M6b) work inside any drive**; placing a share (a mount) goes
  into My drive as now. A share of something in a shared drive is made by
  a member who may manage it.
- **The drop box** places into My drive and into shared drives the user
  can write in.

## 2. The API

- **`GET /api/drives`**: the user's drives (id, kind, name, root node id,
  storage used and limit, whether they manage it). **`/api/drives/{id}`**
  for one; the superadmin's `POST /api/superadmin/drives`, members and
  rename under it.
- **Node ids stay** (UUIDs) and every `/api/files/{id}…` endpoint keeps
  working for every drive: the apps and the desktop keep their calls. A
  node answers its `drive_id`. `/api/files/root` stays My drive's root.
- **Ids that allow remote ones**: a drive's id in the API is a **string**
  — a UUID for this server's drives — so that `drive@host` (step 5) fits
  without changing the type.
- **The change feed** covers all of a user's drives, each change with its
  `drive_id`; the live events too.

## 3. The clients

- **Web**: the sidebar lists the drives (My drive, each shared drive,
  Shared with me); a drive's root is its own page; breadcrumbs start at
  the drive. The superadmin's Settings → Drives makes and manages shared
  drives.
- **Desktop**: Settings → Synced folders lists each drive; the user picks
  which drives (or folders in them) to sync, each at its own local place
  (`~/Organizer/<drive>`).
- **iOS and Android**: the Files tab starts with the list of drives; the
  share sheet's folder picker too.

## 4. The superadmin

- **"Admin" becomes "superadmin"** everywhere: the UI, the docs, the API
  (`/api/admin/…` becomes `/api/superadmin/…`), the role in the database
  (`users.is_admin` becomes `is_superadmin`).
- **Step-up**: the superadmin's pages and endpoints ask for the passkey or
  the two-factor code again when the last one was more than 15 minutes
  ago (a dialog, then the page). A superadmin with neither is asked to
  add one first. The superadmin's endpoints take a session only, never an
  API key (an API key cannot step up).
- **Recovery on the command line**, run on the server:
  `organizer superadmin grant <email>` (and `revoke`), and
  `organizer two-factor reset <email>` (removes the TOTP secret and the
  passkeys, so the user signs in with the password or single sign-on and
  sets them up again).

## 5. Migration

At the first start: a My drive per user over their files folder, nodes
keyed by drive, trash and versions moved into `.organizer/`, `is_admin`
renamed, `/api/admin` answered as `/api/superadmin` for one release (then
removed). Backups (M3) back up each drive on the server's storage, as they
back up each files folder now.

## 6. Pages (mockups)

1. The web sidebar with drives; a shared drive's page.
2. The step-up dialog.
3. Settings → Drives (superadmin): make a shared drive, its members.
4. The desktop's drives in Settings → Synced folders.
5. The apps' Files tab starting with the drives.

## 7. Decided (2026-11-09)

1. **The design** (§1–§5) as proposed.
2. **The pages** as mocked up.

## 8. Built

- **Backend**: migration 0024 (drives, members, the tree's tables keyed by
  drive; My drive's id is its user's), 0025 (superadmin); `files/drives.rs`,
  `routes/drives.rs`, the step-up (`routes/step_up.rs`) and the recovery
  commands (`recovery.rs`).
- **Clients**: the web's sidebar of drives, a shared drive's page, the
  drive's trash, Settings → Drives and the step-up dialog; the desktop's
  Synced folders per drive and the engine's drives; the iOS and Android
  Files tabs and pickers starting with the drives; the macOS File
  Provider's domains in drives.
- **Left for later**: WebDAV shows My drive only (shared drives are not
  mounted there yet); a shared drive's search text is read under the
  default drop-box settings, and photos, scans and recordings in it are
  not read by a model nor embedded (model reads and embeddings are per
  user); restoring a shared drive's files from a backup is through the
  command line (`organizer backup restore`), not the web.
