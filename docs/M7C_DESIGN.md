# M7c design proposal — storage backends

Status: **accepted 2026-11-09** (§8). Step 3 of [M7_DESIGN.md](M7_DESIGN.md) (§5, §9), on
top of M7a's drives and M7b's roles and settings levels. Mirrors are
step 4.

## 1. Backends and drives

- **A storage backend** is configured storage that holds drives. Two kinds
  now:
  - **The server's disk** (what every drive uses today): one built-in
    backend, the files root and `<DATA_DIR>/drives`.
  - **S3-compatible** (AWS, Backblaze B2, Cloudflare R2, MinIO, Garage,
    Scaleway, …): endpoint, region, bucket, an optional prefix, access key
    and secret. WebDAV and SFTP come later through the same layer.
- **A drive lives in one backend**, at its own folder or prefix
  (`<prefix>/<drive id>/`), with its tree as plain files at their paths,
  and its trash and versions under `.organizer/` there (as on disk). Users
  see drives, never backends, except for one line on the drive's page:
  where it is stored.
- **My drive stays on the server's disk.** Backends hold shared drives
  (and team drives); a user's own backend holds drives they make there.

## 2. Who adds backends, who makes drives on them

- **Global backends** are the superadmin's (Settings → Storage). Per
  backend: who may make drives on it (superadmin only, team managers,
  everyone, or chosen teams), a **storage limit** for all its drives, and
  a default limit for each new drive.
- **Users' own backends** (Settings → Storage → Your storage), when the
  superadmin allows it (a setting, off by default, at superadmin and team
  level): S3-compatible only. Their endpoints must be public addresses
  unless `BACKENDS_ALLOW_PRIVATE` is set (as `LINKS_ALLOW_PRIVATE`), so a
  user cannot point the server at the server's own network.
- **A drive on a user's own backend** is made by that user, who is its
  manager; it can get members like any shared drive. **Its members see
  whose storage it is on, with a warning**: "Stored on Alice's own
  storage. If she removes it, this drive goes." The superadmin can make no
  drive there.
- **Moving a drive to another backend**: a job that copies it and switches
  over when done (the drive is read-only meanwhile).

## 3. Credentials

- **By default** a backend's secret is encrypted with a **server key**:
  `<DATA_DIR>/secret.key`, made at the first start (or `SECRET_KEY_FILE`),
  AES-256-GCM through the crypto crate already in the workspace. The
  secret is never sent back by the API (only "set").
- **Private credentials** (opt-in, users' own backends only): the secret is
  sealed to the user's M6e encryption key, so the server cannot read it at
  rest. When the user unlocks encryption in a session, the client opens it
  and hands it to the server for as long as that session lives (in memory
  only). **Without an unlocked session the drive is unavailable**: its
  members see "Locked until <user> signs in", sync waits, and the drop box
  skips it. The page says so before the user chooses it.

## 4. How a drive works on S3

- **The index is the truth for what Organizer writes**; the bytes go
  through one storage layer (opendal) for every backend, the server's disk
  included, so every feature works on every backend: listing, upload and
  download (streamed, multipart), trash, versions, search and AI reading,
  WebDAV, the change feed, sync, links, encrypted folders.
- **Changes made outside Organizer** (another tool writing to the bucket)
  are found by a **periodic listing** of the drive's prefix (hourly by
  default, per backend; or off). The server's disk keeps its watcher.
- **Moving or renaming a folder** on S3 copies each object and deletes the
  old one (S3 has no rename): a journaled job, so a crash part-way is
  finished at the next start. Large folders take a while; the folder shows
  "Moving…" meanwhile.
- **Limits**: the drive's limit and the backend's limit both count; an
  upload over either is refused (507).
- **A backend that fails** (unreachable, credentials refused) marks its
  drives unavailable with the reason, for their members; the superadmin or
  the owner sees it on the backend's row. Nothing is deleted.

## 5. Backups (M3)

**Server backups cover drives on the server's disk.** Drives on global S3
backends are not backed up by Organizer: the pages say the provider's
versioning or replication is the way. Drives on users' own
backends are never backed up by the server, and the app says so where the
drive is made and on its page.

## 6. The API

- `/api/superadmin/backends` (list, add, change, remove; `POST …/test`
  checks the connection), `/api/backends` (mine: list, add, change,
  remove, test), each with kind, name, endpoint, bucket, prefix, region,
  `secret_set`, private, limit, used, who may make drives, status.
- `POST /api/drives` and `/api/teams/{id}/drives` take `backend_id`;
  `Drive` gains `backend` (`id`, `name`, `kind`, `owner` when it is a
  user's), `available` and its reason.
- `PUT /api/backends/{id}/unlock` (the opened secret, session only).
- `POST /api/drives/{id}/move` with `backend_id`.

## 7. Pages (mockups)

1. Settings → Storage (superadmin): the server's disk and the global
   backends, Add S3 storage (with Test connection), who may make drives,
   limits.
2. Settings → Storage → Your storage (users, when allowed): their own
   backends, with "private credentials".
3. New shared drive: choosing where it is stored.
4. A drive's page stored on someone's own backend: the warning; and a
   locked drive.

## 8. Decided (2026-11-09)

1. **The design** and **the pages** as proposed and mocked up.
2. **opendal 0.58.2** as a direct dependency of the backend, with
   `services-fs` and `services-s3` (the version already built in through
   `rustic_backend`).
3. **Global S3 backends are not in the server's backups**; the provider's
   versioning is the way, and the pages say so.
4. **Moving a drive between backends** is in this step.
5. **aes-gcm 0.11.1** as a direct dependency of the backend (the server
   key's encryption of backend secrets; the version the crypto crate
   uses), approved after it was added.

## 9. Built (backend)

Migration 0029 (`backends`, `backend_teams`, `drives.backend_id`,
`drive_moves`, the journal's `target_parent` and `target_name`).
`storage.rs` (the storage layer), `backends.rs` (operators, health,
secrets), `files/moves.rs` (moving a drive), `routes/backends.rs` (the
API). Choices made while building:

- **opendal 0.58.2** with `services-fs` and `services-s3` and its default
  features: rustic_backend builds them anyway, so turning them off builds
  nothing less. **aes-gcm 0.11.1**, the crate (and version) the end-to-end
  crypto crate uses, for the server key; no other new crate.
- **One storage layer for every drive** (`Storage`, `Tree.storage`): paths
  are relative to the drive's root; downloads (one range, `If-Range` on the
  ETag; tower-http's `ServeFile` is gone), links and their zips, versions,
  the trash, search's and the AI's reading, WebDAV's shares and backups'
  restores go through it. On the server's disk four things use the
  filesystem itself, as opendal offers nothing for them: the listing
  (symlinks skipped, never followed), the watcher, a version as a hard
  link, and an upload moved in from the uploads folder by a rename (a copy
  through opendal would write every upload twice). WebDAV still reads My
  drive (always on the disk) through `LocalFs`; a share placed from a
  drive in a bucket is read through the index and the storage layer.
- **Key layout**: a drive is at `<prefix>/<drive id>/` of its bucket, its
  tree as plain objects, its trash and versions under `.organizer/`. An
  empty folder is a marker (`name/`, opendal's `create_dir`); a listing
  makes a folder of each marker and of each prefix an object is under; a
  key that is both a file and a folder (`x` and `x/y`) is the folder;
  empty names and `.`/`..` are left out.
- **The server's disk** has no row: `backend_id` NULL in `drives`, the nil
  UUID (`00000000-…`) in the API (`Backend`, `BackendRef`, `backend_id` in
  a request). My drive always stays there.
- **The server key**: `<DATA_DIR>/secret.key`, 32 random bytes, made at
  the first start (mode 0600), or the file `SECRET_KEY_FILE` names, which
  must exist (a wrong size stops the start). A secret is a random nonce
  then the AES-256-GCM ciphertext and tag, with the associated data
  `organizer/backend-secret/1`. It is not in the server's backups (they
  take `users/`, `drives/` and the database's copy): the README says to
  keep a copy.
- **S3 connections**: path-style requests, region `us-east-1` when none is
  given, S3's own ways to credentials (environment, profile files, the
  cloud's metadata service) off for every backend. A connection is tested
  before it is stored and when it changes: an object
  `<prefix>/.organizer-test-<uuid>` written, read back and deleted.
- **Users' own backends' addresses**: https and a public address (the
  links' `is_public`, and our reqwest with the `PublicOnly` resolver and no
  redirects as opendal's HTTP transport, `Guarded`), unless
  `BACKENDS_ALLOW_PRIVATE`. The transport is ours (about forty lines) rather
  than opendal's reqwest transport crate as a new direct dependency.
- **Who makes drives where** (`check_maker`): the superadmin on any global
  backend (`POST /superadmin/drives`); `make_drives` for the others:
  `team_managers` and `teams` (the chosen teams' managers) for a team's
  drive (`POST /teams/{id}/drives`), `everyone` also for `POST /drives`
  (which still needs `drives.create` to be `everyone`). A user's own
  backend: its owner only, through `POST /drives` (whatever
  `drives.create` says; the `storage.own_backends` setting must still
  allow it), not for a team's drive; never the superadmin. A backend's
  `drive_limit` is each new drive's limit. `GET /backends/choices
  [?team_id=]` lists where the user may put a new drive.
- **The setting** is `storage.own_backends` (bool, off, superadmin and team
  levels, the loosest of one's teams as for `encryption.allowed`).
- **Private credentials**: the client sends the plain `secret` (for the
  test, and to unlock the adding session) and `sealed_secret`, base64 of
  at most 4 KB that the server stores and never opens; the user must have
  set up encryption (M6e). The format is the clients': we suggest the
  folder keys' wrapping of docs/E2E_SPEC.md (an ephemeral X25519 key, HKDF,
  AES-256-GCM) with the secret's UTF-8 bytes. The secret given to
  `PUT /backends/{id}/unlock` is tested, then kept in memory with the hash
  of the session (as the `sessions` table keys it); `DELETE
  /backends/{id}/unlock` forgets it for that session. Every minute
  (`Files::close_locked`) the secrets of ended sessions are dropped and
  the drives of a backend left without one are closed. An unlock of a
  locked backend settles what a stop left in its drives' journal.
- **Unavailable drives**: a backend's health is in memory; a failed call
  (not "not found" or "exists") marks it with a reason fit to show ("The
  storage cannot be reached", "The storage refused its credentials", …),
  any call that goes through clears it, as does a test. The listing is a
  probe too. `Drive` says `available`, `unavailable` (`locked` or `failing`,
  for a client to act on, never the words) and `unavailable_reason` (also
  "Locked until <email> signs in"); a file operation answers 503 with
  `code` `storage_unavailable` and the reason, and a write to a drive being
  moved 409 with `code` `read_only`. Nothing is deleted: a
  listing that fails or finds the prefix empty changes nothing.
- **The listing** of a bucket's drive is the reconciler of the disk on
  `Storage::walk`, every `list_every` (per backend, minutes in the API,
  hourly by default, 0 for never). A file changed outside Organizer is
  downloaded to be hashed (what Organizer writes is hashed as it goes).
- **Moves on S3** (a rename, a delete to the trash, a restore): each object
  copied, then the original deleted, under the tree's lock, in a task of
  its own (a dropped request does not cut it short); the intent carries
  the node's new parent and name. A failure puts back what was copied and
  drops the intent; if even that fails, the intent stays and the next
  start (or unlock) finishes the copy and moves the node there, keeping its
  id. **"Moving…"** is the client's while its request is pending: the node
  has no flag of its own.
- **Moving a drive** (`POST /drives/{id}/move` for its managers, to a
  backend they may make drives on; `POST /superadmin/drives/{id}/move` for
  the superadmin; 202 with the drive, whose `moving_to` says where): the
  `drive_moves` row is the job, carried on at start, tried again five
  minutes after a failure (`error` on the row). Writes answer 409 meanwhile;
  reads go on. The copy skips what is there with the same size, runs once
  more under the lock, then the drive switches over, the index takes the
  new storage's times (so the next listing sees nothing changed) and the
  old copy goes: the folder in the data dir or the bucket's prefix; a
  folder on the server that the superadmin chose keeps its files (only
  `.organizer` goes) and the drive's `root` is cleared.
- **Removing a backend**: a global one with drives (or a move to or from
  it) is refused (409); a user's own takes its drives out of Organizer
  (`forget_drive`: the rows go, the objects stay in the bucket, which is
  theirs), and so does removing the user. Deleting a shared drive in a
  bucket deletes its prefix there when the backend answers.
- **Backups** take only the drives on the disk (`backup::run::sources`,
  `backend_id IS NULL`). The drop box offers no folder of a drive that is
  failing, locked or being moved.
- **The API** (all in the spec): `/superadmin/backends` (list with the
  disk first, add, `/test` an unsaved one, `/{id}` change and remove,
  `/{id}/test`), `/backends` (the user's own: the same, plus `/choices`,
  `PUT`/`DELETE /{id}/unlock`; changes take a session, never a key), the
  tag `Storage`. `Backend`: `kind`, `name`, `endpoint`, `region`,
  `bucket`, `prefix`, `access_key`, `secret_set`, `private`, `limit`,
  `drive_limit`, `used`, `drives`, `make_drives`, `team_ids`,
  `list_every_minutes`, `owner`, `status` (`ok`, `failing`, `locked`),
  `error`, `backed_up`. `Drive` gains `backend` (`id`, `name`, `kind`,
  `owner`'s email for a user's own, `backed_up`), `available`,
  `unavailable`, `unavailable_reason` and `moving_to`; `NewDrive`, `NewTeamDrive` and
  `NewSharedDrive` take `backend_id`; `SharedDrive.folder` is
  `s3://bucket/<prefix>/<drive id>/` for a drive in a bucket.
- **Tests without S3**: opendal's in-memory service (built in through
  rustic_backend's features, none added) stands in for an endpoint
  (`Backends::stand_in`). It has no rename and keeps folders as markers,
  like S3, and no copy, so the read-and-write copy is what runs. Not
  exercised against real S3: the S3 service's requests themselves (signing,
  multipart uploads, server-side `CopyObject`, `ListObjectsV2` pages and
  real markers, how a real 403 or a timeout is classified), the `Guarded`
  transport (a stand-in replaces the whole operator) and its refusal of a
  name that resolves to a private address. Unit tests cover the key
  layout, the listing's tree, the object-by-object move carried on after a
  stop, a drive's copy, the health and the server key; integration tests
  (`tests/api.rs`) the backends' API, who makes drives, users' own
  backends and the address rules, private credentials locked and
  unlocked, both limits, a failing backend, the listing's reconcile, a
  drive's move (with a stop part-way), an interrupted folder move on S3,
  and backups leaving out drives in buckets.
- **The load test** is worth a run: downloads, uploads, versions and every
  tree operation now go through opendal on the disk too.
