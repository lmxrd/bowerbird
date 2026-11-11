# M7 design — settings, access, storage, mirrors and federation

Status: **decided 2026-11-09** in a side conversation (every point below
is the user's decision; nothing is open there), recorded here before
anything is built. §8 lists what these decisions change in what is built
already (to confirm), and §9 the build order. Each step gets its own short
implementation design and mockups before it is built.

## 1. Principle

**Settings govern only their own instance; nothing is promised about data
once it leaves it.**

## 2. The superadmin and settings

- **"Superadmin"** is the instance's administrator; "admin" is no longer
  used for it. It is a role on a normal account. Opening the superadmin's
  pages asks for the passkey or two-factor code again (**step-up**, valid
  15 minutes). (The user may revisit this later; see §10.)
- **A command-line tool on the server** for recovery when nobody can sign
  in: make a user superadmin, reset a user's two-factor.
- **Every setting** is, per level, **locked**, a **default** users may
  change, or **open**; the levels are **superadmin → team → user**, and
  environment variables override them all.
- **A privacy floor**: the superadmin's minimums are locked settings.
- **AI models**: users choose among the superadmin's models; the
  superadmin can let users bring their own (off by default).
- **Public links**: allowed by default; the superadmin can turn them off,
  or require a password and a maximum lifetime. **A link gives view, edit
  or upload-only access** (corrected 2026-11-09: the side conversation had
  recorded view and upload only; the user keeps edit links).
- **The superadmin sees only what they own or what is shared with them**,
  never other users' data. **Each user sees a log** of the superadmin's
  actions that affected their account.

## 3. Access (one model, as Google Drive)

- **Roles**:
  | Role | May |
  | --- | --- |
  | owner | everything, including delete and handing over |
  | manager | members and settings, and all an editor may |
  | editor | read, write, rename, move, delete (to the trash) |
  | viewer | read and download |
  | upload only | add files, and see only their own uploads |
  No commenter role (see §10).
- **Shared drives have no owner**, and always at least one manager.
- **Teams** use the same role names: team managers manage the team's
  drives; members get the role each drive gives the team.
- **Inheritance**: roles apply downward. A subfolder can **widen** them, or
  **narrow** them with **limited access**: only the people listed, and
  everyone else does not see it at all — search, trash, versions, mirrors
  and sync all obey it. Managers always keep access.

## 4. Teams

As Nextcloud's or Google Workspace's: their own shared drives, their own
setting defaults (the team level of §2), a storage limit, their own
managers. No separate tenant spaces.

## 5. Storage backends and drives

- **A storage backend** is configured storage (local disk, S3; later
  WebDAV, SFTP; through opendal) and holds many drives. **A drive** is a
  root (a folder or prefix) in a backend with its own tree, trash,
  versions, limit and members. Users see drives, not backends.
- **Users can add their own backends** (S3-compatible), when the
  superadmin allows it, under network rules like `LINKS_ALLOW_PRIVATE`.
  The superadmin adds **global backends** and restricts who may create
  drives on them, by role or group; **limits per backend and per drive**.
- **Credentials** are encrypted with a server key by default; opt-in
  **private credentials** are wrapped with the user's M6e key (the backend
  then works only while a session has unlocked it).
- **The sidebar lists drives**; the sync clients show each drive as its
  own, and the user picks which to enable.
- **Shared drives** sit outside users' trees, for their members only;
  **shared folders** stay in a user's tree. Shared drives are made by the
  superadmin, or by users when permitted. A shared drive may live on a
  user's own backend; its members see whose storage it is on, with a
  warning.
- **Trash and versions live in each drive's own backend** (a hidden
  `.organizer/` prefix).
- **Server backups cover only drives on the server's own storage**; users
  are responsible for their own backends, and the app says so.

## 6. Mirrors

- A user can **mirror a drive to their own backends only** (never a global
  one): read-only, one-way, plain files at their paths, deletes copied
  exactly; the mirror has its own trash and versions; encrypted folders
  stay sealed; a mirror stays when its drive is deleted.
- **Eject**: the user turns the mirror into a drive in place (mirroring
  stops), or makes a copy (mirroring goes on).
- **Mirroring shared drives** is off until the superadmin enables it; a
  drive's managers can forbid it for their drive; members see who mirrors.
- **When**: the user's choice; continuous by default.

## 7. Federation

- **Standards**: OCM for sharing, discovery and invitations; WebDAV for the
  bytes; WebDAV sync-collection (RFC 6578); HTTP Message Signatures
  (RFC 9421); WebFinger (RFC 7033). The current OCM specification is read
  when this step is designed; our own extensions (whole drives with roles,
  E2E keys, live events) only where OCM has nothing. **Ids allow remote
  ones (`drive@host`) from the start.**
- **Bytes go through each user's home server**; clients talk only to their
  home server.
- **The superadmin chooses off, an allowed list, or open; off by
  default.**
- Drives and shared folders can be federated.
- This server's search and AI may use remote drives; the superadmin can
  restrict that on this server. No flags that try to restrict the other
  server.

## 8. What this changes in what is built (to confirm)

Confirmed 2026-11-09: §8.2 (shares map to the nearest role), §8.3
(links unchanged) and §8.5 (each tree becomes My drive, nothing moved).

1. **Admin → superadmin**, everywhere (UI, docs, API names like
   `/api/admin/…`, `is_admin`), plus step-up on the superadmin's pages and
   the recovery tool.
2. **M6b's fine-grained permissions become roles.** Today a share has
   read/create/change/delete/re-share toggles; the roles replace them.
   Existing shares map to the nearest role: read only → viewer; read with
   create, change or delete → editor; with re-share → manager (re-sharing
   becomes the manager's member management).
3. **Links keep their three kinds** (view, edit, file drop), as M6b built
   them; nothing changes for existing links.
4. **M6b's groups become teams** (with drives, defaults, limits,
   managers).
5. **Each user's tree becomes their "My drive"** on the server's own
   storage backend (the files folder they have today), so nothing moves
   on disk; trash and versions move into the drive's `.organizer/` prefix.
6. **Settings that exist** (drop box, retention, notifications, AI work,
   sharing, encryption, sign-in policy) move onto the levels of §2; the
   retention rules' default-plus-override already works this way.
7. **Backups' scope** narrows to drives on the server's own storage.

## 9. Build order

1. **Drives on local storage**: the drive as the unit (My drive, shared
   drives), the sidebar of drives, sync and apps per drive, trash and
   versions per drive; admin → superadmin with step-up and the recovery
   tool; ids that allow `drive@host`.
2. **Roles and teams**: the five roles, inheritance and limited access,
   teams with their drives, defaults, limits and managers; M6b's shares
   and groups migrated (§8.2–8.4); the settings levels (§2) with locks
   and the superadmin's audit log for users.
3. **Storage backends**: global and users' own (S3 through opendal),
   credentials (server key, or private with the M6e key), limits, backups'
   scope.
4. **Mirrors**: to users' own backends, eject, the shared-drive rules.
5. **Federation**: OCM, after reading its current specification.

## 10. Later

- Comments, with a commenter role.
- Superadmin sign-in variants: accounts that hold no files, or a separate
  sign-in (as Vaultwarden's admin page).
