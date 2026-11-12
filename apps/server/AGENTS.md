# AGENTS.md

The server: Rust (edition 2024), axum, SQLite through sqlx, one binary with
the frontend embedded. The root AGENTS.md has the rules of the whole
repository; these are the backend's. Run the tasks through mise from the
repository root (`mise run check:backend`, `test:backend`, `dev:backend`).

## Architecture you must not break

**The database** (`db.rs`, sqlx, SQLite only for now; `DATABASE_URL`):

- `db.writer()` is a pool of **one** connection; `db.reader()` is a pool of
  read-only connections. Use the reader for reads that need no atomicity.
- Never run a query on the writer pool while you hold a transaction or
  connection from it: with one connection that waits for itself. Inside a
  transaction, use the transaction (`&mut *tx`).
- A read-then-write belongs in a transaction on the writer. Do not hold a
  transaction during slow work (hashing, network calls).
- Queries are run-time `sqlx::query` / `query_as` with `FromRow`; never the
  `query!` macros (they need a database at compile time). SQL is
  SQLite-flavoured (`?1` placeholders). Dynamic SQL is built only from
  fixed fragments and placeholders; every value is bound.
- Schema changes are **new numbered files** in `migrations/`, from `0002`
  up: `0001_base.sql` is the whole pre-release schema, squashed into one
  file that sqlx applies in a single transaction (all or nothing). Never
  edit a file that has shipped (its checksum is recorded in
  `_sqlx_migrations`); until the first release, `0001_base.sql` itself may
  still be changed. The "migration NNNN" references in this file and in
  `docs/` name the per-change files from before the squash — they are in
  git history.
- A clean stop (SIGTERM/SIGINT) finishes the requests in flight and closes
  the pools, which checkpoints the WAL.

**Single sign-on** (`oidc.rs`, `routes/oidc.rs`): OpenID Connect through
the `openidconnect` crate, without its default features (it would bring a
second reqwest); our reqwest is handed to it (`Http` in `oidc.rs`, redirects
off). The settings are the `oidc` document of the `server_settings` table
with the `OIDC_*` environment variables on top (a variable locks its
field); they live in
memory in `state.oidc` and are replaced when a superadmin saves, after the
provider answered. A login in progress (state, nonce, PKCE verifier,
callback URL) is not kept on the server: it is the `organizer_oidc` cookie
(`__Host-organizer_oidc` over https), signed with a key made at start
(`oidc.key()`), good for ten minutes, so a flood of logins fills nothing.
The provider's metadata is cached by issuer (the last two) for an hour, and
fetched at once for a token signed with a key it does not have; a provider
that failed to answer is not asked again for 30 seconds, and only one fetch
runs at a time (logins and saves alike). Its endpoints must be https (or on
this machine), and its answers at most 1 MB. A save of the settings runs as
its own task holding `oidc.lock_saves()` from reading the row to putting it
in effect, so a dropped request cannot store it without applying it; a new
issuer drops the stored secret unless one is sent with it. Nobody is signed
in through the provider before the first superadmin exists (`/setup`). A
provider account is linked to a user in `user_identities` (issuer +
subject); an unlinked one is matched only by an email the provider says is
verified, and not to a user linked to another account at that provider.
A verified email without an account gets one with the invite the login was
started from (`/auth/oidc/login?invite=`, kept in the login cookie), or
when the superadmin's "create users" switch is on (`users::oidc_creates_users`;
with `OIDC_ONLY`, open sign-up means this too). A disabled user is refused
(`disabled`).
`OIDC_ONLY` (`oidc.passwords_allowed()`) refuses every password path with
403 (`HttpError::passwords_off`: login, its second step, setting a
password) but `/setup`; the session's
`password_login` tells the frontend. The callback URL is `OIDC_REDIRECT_URL`, else under `PUBLIC_URL`, else plain http on the request's host; forwarded headers never count. The
callback never redirects anywhere but `/`, `/login?step=<step>`,
`/login?error=<code>` and the `next` the login was started with: a path
of this server only (`device::allowed_next`: `/` then not `/` or `\`,
printable ASCII; an app's device approval page, OCM's invite accept
dialog), carried on to `/login?step=<step>&next=…`, and
always clears the login cookie (even one it did not sign), with the
attributes it was set with: a browser ignores a `__Host-` removal without
`Secure`. The tests in `tests/api.rs` run a stand-in provider
that signs real ID tokens with the keys in `tests/fixtures/` (test only).

**Users** (`users.rs`, `routes/users.rs`, docs/M6A_DESIGN.md): the ways in
are setup (the first superadmin like any user: their tree is the default under `STORAGE_URL`),
invites (`invites`, 7 days, once, found by the token's SHA-256; the token is
kept while pending for "Copy link"), open sign-up and single sign-on. The
switches are the `signup` document of the `server_settings` table with
`SIGNUP_OPEN` and
`OIDC_CREATE_USERS` on top (`state.joining`, a variable locks its switch);
`/auth/session` says `signup`. Invites and sign-up go through the sign-in
limits (the invite page per address: `Limits::address`). A disabled user
(`disabled_at`) is filtered out of `Backend::get_user` and `key_user`, so
sessions, keys, device tokens and app passwords stop at once; password
sign-ins answer 403 only after the right password. The last superadmin who is
not disabled cannot be demoted, disabled or removed
(`users::other_superadmins`). Removing a user deletes their row (everything
cascades), their FTS rows, upload parts, embeddings and
`<DATA_DIR>/users/<id>` (the default files folder with it); an existing
folder is outside the data dir and never touched. `last_seen_at` is written
by `auth::note_seen` at most once a minute (requests and sign-ins).
**The superadmin** (docs/M7A_DESIGN.md §4, migration 0025; it was "admin"
until M7a): `users.is_superadmin`. Their endpoints are under
`/api/superadmin` (`routes::superadmin_router`), also answered at
`/api/admin`, outside the spec, for one release (`app` in `lib.rs`: remove
it then). Every one takes the `Superadmin` extractor (`routes/auth.rs`):
a session only (any token: 403), a superadmin, and a **step-up**, a
second factor confirmed within `session::STEP_UP_WINDOW` (15 minutes),
kept in the session (`session::step_up`, with the user's id, so a session
signed in again as someone else has none; `sign_in` drops it). A refusal
the web acts on is a 403 with `ErrorBody.code`: `step_up_required`, or
`second_factor_required` when the user has neither the app nor a usable
passkey (`second_factors`). The confirmations (`routes/step_up.rs`,
through `limits.user`): a code of the app (`totp::use_app_code`; a
recovery code does not count: it is for a lost app) or one of the user's
passkeys (non-discoverable, `Kind::StepUp`); a sign-in with a passkey or
an app code is one too (`session::sign_in_confirmed`). `/auth/session`
says `step_up` (`until`, `totp`, `passkey`). No API key scope reaches them:
`admin` keys became `write` (`Scope`'s serde alias keeps the old name
accepted). The tests step up through the stored session (`stepped_up` in
`tests/api.rs`). Recovery on the command line (`recovery.rs`, dispatched
by `main` as `backup` is; each noted in the account's activity): `organizer superadmin grant|revoke <email>`
(never the last enabled one) and `organizer two-factor reset <email>`
(the app's secret, pending setup and recovery codes, the passkeys, and the
user's sessions, found by axum-login's `user_id` in their record).
**Drives** (`files/drives.rs`, `routes/drives.rs`, docs/M7A_DESIGN.md,
migration 0024): a drive is a tree with its own root, trash, versions and
limit (`drives`). My drive is a user's tree; **its id is the user's**
(`drives::my`), so every table of a tree kept its keys, and a trigger
makes it with every new user. Shared drives (`kind = 'shared'`) are at
`drives.root` or `<DATA_DIR>/drives/<id>`, apart from every other drive
(`check_apart`), seen by those their grants reach (below). Who has a
drive is the `drive_users` view (My drive's user as owner, the drive's
grants to users and to teams' members, the managers of `drives.team_id`):
use it, never the tables underneath. The tables of a tree (`nodes`,
`journal`, `trash`, `versions`, `search_docs`) and `grants`/`links` are
keyed by `drive_id`; `Access.owner` is the node's drive and `own` means "a
drive of the user's" (a role on the drive itself). The change feed stays
per user: `record_change` writes a row for each user of the drive who sees
the node, with `drive_id` (a placed share's rows are in the recipient's My
drive); getting or losing a shared drive writes its root's
`create`/`delete`. In the API a drive's id is a **string** (room for
`drive@host`). Each drive's trash and versions are in `<root>/.organizer/`
(`ORGANIZER_DIR`, `Tree::trash_dir`/`versions_dir`), moved there once from
`<DATA_DIR>/users/<id>/` at start (`move_into_drives`) and along with a new
files root (`move_organizer_dir`): the scan skips it, the watcher ignores
it, `check_free` refuses the name at a drive's root, WebDAV hides it and
404s any path into it, backups' browsing hides it, and a drive's root
cannot be shared. Search indexes per drive (a user searches all theirs);
a shared drive's text is read under the default settings, and only My
drives get model reads and embeddings (they are per user). Retention runs
per drive (My drive by its user's rules, a shared drive by the defaults).
**Storage limits** (`files/quota.rs`): per drive (`drives.storage_limit`;
a user's limit is their My drive's): used is the drive's files, trash,
versions and, for My drive, its user's unplaced drop-box items; `Files::check_room` refuses with
`FileError::Full` (507, WebDAV's InsufficientStorage). Every way new bytes
come in checks it: `Files::write_file` (plain uploads, replaces, WebDAV,
version restores), resumable uploads (at creation with a length, and on
completion), the drop box's staging (multipart, folders, WebDAV drop box),
backup restores. `place_file` itself does not: a drop-box placement only
moves bytes already counted. A drive in a storage backend counts against
the backend's limit too (`backend_usage`).

**The storage layer** (`storage.rs`, docs/M7C_DESIGN.md §4): every drive's
bytes go through a `Storage` (an opendal operator over the drive's folder,
or its prefix `<prefix>/<drive id>/` in a bucket), the server's disk
included: `Tree.storage`. Paths are relative to the drive's root (`a/b`,
"" for the root; `Files::rel_path`); the trash and versions are
`Tree::trash_path`/`version_path` (`.organizer/…`). Never reach a drive's
files through `std::fs` outside `storage.rs`. On the disk a few things go
to the filesystem itself (opendal has nothing for them): the listing
(`walk`, symlinks skipped), the watcher, a version as a hard link, and an
upload moved in from the uploads folder by a rename. Downloads are
`routes::files::serve_file` over a reader (one range; `If-Range` on the
ETag); a link's zip reads on its thread through the runtime's handle;
search and the AI's reading take `(storage, path)`; a staged drop-box file
is `Storage::local_file`. S3 has no rename: `rename` there is
`move_objects` (each object copied, then deleted; run again, it carries on),
so a move, a trash and a restore run as a task of their own (`detached`)
and, on failure, put back what they copied (`undo_copies`) or leave the
intent, which `journal::recover` finishes at the next start (or when a
locked backend is unlocked), moving the node to the journal's
`target_parent`/`target_name`. A backend's failure is an `io::Error`
carrying `storage::Down` → `FileError::Down` (503, `code`
`storage_unavailable`, the reason fit to show; `Drive.unavailable` says
`locked` or `failing`, for the clients to act on) and marks the backend's
`Health`; any call that goes through clears it. A drive in a bucket is
listed by its backend's `list_every` (hourly by default, or never) instead
of being watched (`watch.rs`): the same reconciler (`scan.rs`), on
`Storage::walk` (folders from markers and the prefixes objects are under;
`entries_of_objects`). Hashing a file changed outside Organizer downloads
it.
**Storage backends** (`backends.rs`, `routes/backends.rs`, migration
0029): the server's disk has no row (`backends::DISK`, the nil UUID, in the
API; `drives.backend_id` NULL); S3 rows are global (`owner_id` NULL) or a
user's own (the `storage.own_backends` setting, superadmin and team level).
`Backends` (in `Files`) holds the server key (`<DATA_DIR>/secret.key` made
at start, 0600, or `SECRET_KEY_FILE`, which must exist; AES-256-GCM, a
nonce then the ciphertext, AD `organizer/backend-secret/1`), each backend's
`Health`, and the secrets of private credentials by session (the SHA-256
of the session id, as `sessions` keys them): `PUT /backends/{id}/unlock`
keeps one for as long as that session lives; `Files::close_locked` (each
minute) drops those of ended sessions and closes their drives' trees. A
secret is never sent back (`secret_set`). Every connection is tested before
it is stored (an object written, read and deleted in the prefix). S3's own
ways to credentials (environment, files, the cloud's metadata service) are
off. A user's own backend must be https at a public address unless
`BACKENDS_ALLOW_PRIVATE`: checked when set, and its requests go through our
reqwest (`Guarded`: `PublicOnly` resolver, no redirects) as opendal's HTTP
transport. Who makes drives on a backend is `routes::backends::check_maker`
(the superadmin on any global one; its `make_drives` for others; a user's
own by its owner only, as their shared drive, whatever `drives.create`).
A global backend with drives is not removed (409); a user's own takes its
drives out of Organizer (`Files::forget_drive`, the objects stay), and so
does removing the user. A changed backend closes its drives' trees
(`forget_backend`). The tests use opendal's in-memory service as a
stand-in for an endpoint (`Backends::stand_in`): there is no S3 in them.
**Moving a drive** (`files/moves.rs`): a `drive_moves` row is the job
(resumed at start); the drive is read-only meanwhile (`Tree::write_lock`
refuses with `FileError::ReadOnly`: 409, `code` `read_only`), the copy (`Storage::copy_all_to`, skipping
what is there with the same size) runs, then under the lock it is copied
again, switched (`drives.backend_id`), the index takes the new times
(`scan::adopt_times`) and the old copy goes (a folder chosen on the server
keeps its files). The drop box offers no folder of a drive that is not
`Files::writable` (failing, locked or moving). Backups take only the
drives on the disk (`backup::run::sources`).
**Mirrors** (`files/mirrors.rs`, `routes/mirrors.rs`, docs/M7D_DESIGN.md,
migration 0030): a mirror **is a drive** (`drives.kind = 'mirror'`, always
in a backend of its user's own) with a `mirrors` row of the same id (its
user, `source_id` the drive it copies, NULL once deleted: ON DELETE SET
NULL, the schedule, the copy's state). Its user has a `viewer` grant on it,
so the files API browses and downloads it and the roles refuse every write;
only the copy writes, through `ops.rs` (`copy_folder`, `WriteTarget::Copy`,
`move_copy`: names kept as the drive has them, clashing only when equal,
`check_free_exactly`; `trash`; `Replace`, which keeps a version), so its
trash and versions are a drive's, swept by its user's rules
(`retention::rules`). `nodes.source_id` is the node a copy is of (in the
API `Node.source_id`: an encrypted folder's ids). A mirror keeps its
user's own wrapped keys of the encrypted folders it copied, never anyone
else's (`e2e_copy_keys`, migration 0031: `e2e::keep_for_mirrors` after
each copy and when keys are posted), which `GET /encryption/folders/{id}`
answers (`e2e::kept_keys`) for a copy's node, or for the drive's once the
user can no longer read it (deleted, access lost). `Files::copy_tree` diffs
what the user sees of the drive (`seen_nodes`: the listings' rules, from
the root down, a hidden folder hiding what is under it; mounts are not
nodes, so placed shares are left out) with the mirror by source id: new is
copied (through the uploads folder), a new hash written over, a move
moved, the rest trashed (the topmost); an unmapped node of the same name
and kind (written before a stop, found by the scan) is taken over. A
failure of one item (`NotFound`, `Conflict`, `Invalid`) leaves it for the
next copy (`again`); any other stops the copy (`error`, tried again after
five minutes). Whether a copy is due: the user's change feed about the
drive past `seen_seq`, `again`, `requested` ("Mirror now"), `running_since`
(a copy a stop cut short: finished at the next look, whatever the
schedule), and the schedule against `last_copy_at`; never while stopped
(`mirror_refusal`: the drive deleted, access lost or upload only,
`mirrors.allowed` false, `mirrors.shared_drives` off; computed each time,
so a reason that goes lets it go on) or locked (its storage or the drive's,
`mirror_locked`). `Files::run_mirrors` copies the due ones (each a task,
claimed in `Mirroring`, which holds the progress); `spawn_mirrors` runs it
every `TICK` (20 s) and on `wake_mirrors`, from `Files::start` (not in the
tests: they call `run_mirrors`). Eject in place turns the drive `shared`
and the grant `manager` and drops the row; eject as a copy is a new shared
drive on the same backend filled by `copy_tree` from the mirror, its
`source_id`s then the drive's. Restores from a mirror's trash or versions
write into the drive mirrored (`restore_into_source`,
`restore_version_into_source`), as the user, where `access_for` allows.
Mirrors are not searched (`search::all_drives`, `drives_named`), not in
the drop box's folders, not the superadmin's shared drives. When a mirror
stopped (`mirrors.stopped_at`, migration 0032) is noted by the first look
that finds it stopped (`Files::mirror_stop`: the loop's, a client's) and
cleared when it goes on; a trigger sets it when the drive is deleted. A
removal or an eject refused answers a `code` (`copy_running`,
`name_taken`: `conflict_code` in `routes/mirrors.rs`; an eject as a copy
claims the mirror too). A trashed node keeps its `source_id`, its
folder's and its encrypted folder's (`trash.source_id`,
`parent_source_id`, `e2e_root_source_id`; `TrashItem`), and a restore
gives the node its `source_id` back. Other crates' tests make a mirror
through `Backends::stand_in_own` (a user's own backend on the in-memory
stand-in) and `Files::create_mirror`.

**Roles** (`files/roles.rs`, `files/grants.rs`, `routes/access.rs`,
docs/M7B_DESIGN.md §1, migration 0026): `grants` give a user or a team a
`Role` (owner, manager, editor, viewer, upload_only) on a node (and what is
under it) or on a whole shared drive (`node_id` NULL); `grant_declines`
per user. `roles::resolve(conn, user, node)` is the one answer to "what
may this user do here": the owner (the nearest `owner` grant, else My
drive's user) is owner; My drive's user (when someone else owns the item)
and a shared drive's managers (manager grants on it, its team's managers)
are **keepers**, at least manager everywhere; else the highest of the
grants from the node up, stopping after a node with `limited = 1`, and the
drive's grants when no limited node cut the walk. A grant on a node given
by someone who is not a keeper gives no more than its giver still has there
(M6b's re-share rule; cycles give nothing); drive grants are never cut.
Upload only sees the node it was given and what it added
(`nodes.created_by`, set by `Files::mark_created` after every op a user
makes a node with: the files API, uploads, WebDAV, the drop box, backups'
restores). None means not seen: 404, as if it did not exist. `Role::allows(Need)` is
§1's table (`Need::Own`, deleting for good and handing over, is a shared
drive's managers' too: `Access::allows`). The listings leave out what is
hidden with `roles::hidden` (a SQL fragment) and give each child its role
with `roles::child_role`; search, the trash (`trash_items_for`, with
`trash_marks` keeping what was limited and who added what for a restore),
versions, WebDAV (`TreeFs::hidden`, `hidden_children`), the drop box's
folders and the change feed (`record_change`, `fan_out`: a move into or
out of what a user sees is the subtree appearing or going for them) all
follow it. Every `Node` the API returns to a user carries `my_role`
(`routes::files::seen` after an op). Every change of access goes through
`Files::grant`/`change_grant`/`revoke`/`set_limited`/`change_owner`/
`set_drive_team`/`set_team_member` (`grants::changing`): one transaction,
the feed rows of whoever got or lost something, a shared drive that had a
manager keeps one (409), then `settle_mounts`. A shared drive's grants
are `/drives/{id}/access` (its managers; a user's own grant also by that
user: leaving) and `/superadmin/drives/{id}/access` (the superadmin, for
any shared drive, member or not; a user's grant noted in their activity).
M7a's member endpoints are gone.
**Sharing** (`files/shares.rs`, `routes/shares.rs`, `routes/access.rs`,
docs/M6B_DESIGN.md): a grant on a node of a drive that is not the user's is
"shared with me". The M6b endpoints (`/files/{id}/shares`,
`/superadmin/groups`) answer for one release over grants and teams, a
share's permissions read and written as the role they stand for
(`Role::of_perms`, `Role::perms`). **Every handler that takes a node id
asks `Files::access` / `access_for(user, id, Need)`**: the user's own node,
or one shared with them (else 404, as if it did not exist), then runs the
operation in `access.owner`'s tree. Never call an op with the request's
user id on a node you did not check. Renaming, moving or deleting needs
change (and delete, out of its folder); inside a share it is refused on
the shared item itself (`inside_share`); moves between trees are refused
(a copy is the way). Deleting a version or a trash item for good is
`Need::Own`.
**Teams** (`routes/teams.rs`, §3): `teams` (name, `storage_limit` for all
its drives together: `Files::check_room` checks it with the drive's own),
`team_members` (`manager` or `member`, `joined_at`), `users.primary_team_id`
(None: the first joined). The superadmin makes them and names managers
(`/superadmin/teams`); a team's managers add members and make the team's
drives (`drives.team_id`, a team grant on the drive, editor by default).
**Settings levels** (`settings.rs`, `routes/settings.rs`, §4, migration
0027): `settings (level, level_id, key, value, mode, min, max)`, one row
per level that sets a key (no row: not set); `settings::DEFS` lists every
key with its kind, built-in default, levels and how several teams merge
(`Scope`: a drive's by the drive's team, sign-in the strictest, one's own
the loosest, values from the primary team). `settings::resolve` walks
environment (`Env::of_state`/`of_files`: `TRASH_DAYS`, `VERSIONS_*`,
`TOTP_REQUIRED`, `AI_MODEL_*`) > superadmin > team > user / drive; `set`
checks a write against what is above (a lock: 403, out of range: 400).
The drop box's, the trash's and versions', the notifications' settings and
two-factor's requirement are read through it (`drop_box_settings`,
`retention`, `retention_defaults` and the notification switches moved into
it); a user's choice of AI model (`ai.model.<work>`, null: the AI page's
choice) goes to `Ai::chat`/`look`/`transcribe` as `chosen`. What one may
choose there is `GET /ai/models` (`routes/ai.rs`, any user): per work, the
models that can do it, by name and id only (never an endpoint or a key).
**Account activity** (`activity.rs`, `routes/me.rs`, §5): `activity::log`
for what a superadmin, a team's manager (`Actor::TeamManager`: adding to
the team, taking out, changing the team role; not what one does oneself)
or the command line (`recovery.rs`) did to an account; `GET /me/activity`
(a session only).
**Mounts** (`share_mounts`, keyed by user and node): a placed share, a
virtual entry under a folder of the recipient's *own* tree (never inside
another mount), with the recipient's name; a user's mounts never nest. A
mount's top has the owner's node id, so ids inside are the owner's.
Listing a folder (`routes/files.rs` `CHILDREN`), `child_named`/
`resolve_for`, `check_free` and the drop box's `free_name` include the
user's mounts there; the owner's own mounts are never listed to others (a
share of a folder holding a mount shares only the owner's files). Renaming
or moving a mount's top, or deleting it, changes only the mount
(`move_as`, `delete_as`: one place for the REST API and WebDAV).
**The change feed fans out** (`shares::fan_out`, called by
`record_change`): each owner row is copied to every user with a mount over
the node (the mount's top as their entry: their parent and name), a move
into a mount is the node and its subtree appearing (`create` rows, parents
first), a move out is a `delete`. So a deletion is recorded **before** its
node row goes (trash, take out, the scan), and a move carries
`from_parent`. Placing writes `create` rows for the whole subtree;
unplacing, declining, losing access (`settle_mounts`, after any grant,
team or limited-access change), a removed owner or a new files root
(`end_mounts_on`) write a `delete`. `Files::notify` also tells every other
user whose feed got rows since the last notice (`feed_mark`).
**WebDAV** walks into mounts (`dav.rs` `locate`: the owner's disk under the
mount, through a `LocalFs` there); a folder's DELETE is handled whole
(`delete_folder`), never by dav-server's walk, which would delete the
owner's files inside a mount one by one.
**Public links** (`routes/links.rs`, `links`, `/api/s/{token}/…`): found by
the token's SHA-256 (the token is kept for "Copy"), dead when expired,
while the owner is disabled, or with its node (cascade). A password is
argon2; the right one goes in the signed session cookie `organizer_links`
(`challenge_key`; `<link id>:<tag of the hash>`, so a new password asks
again), and every try goes through `limits.address`. Everything stays
inside the link's node (`inside`: `is_within`), runs in the owner's tree
on the owner's limit; files are served by `serve_file` (`nosniff`,
`sandbox`); a folder's zip is written by the `zip` crate on a thread into
the response (nothing on disk). A link never gives more than its maker
has (edit: create, change and delete; drop: create; both folders only).

**Federation** (`federation/`, `routes/ocm.rs`, `routes/federation.rs`,
docs/M7E_DESIGN.md, migration 0033): Open Cloud Mesh,
draft-ietf-ocm-open-cloud-mesh-07. `Federation` (in `Files`,
`files.federation()`) holds the Ed25519 signing key (`<DATA_DIR>/ocm-key`
made at start, 0600, or `OCM_KEY_FILE`, which must exist; `kid` is
`<host>#<fingerprint>`), `PUBLIC_URL` (our host; without it the mode is
`off`), the client to other servers (redirects off, `PublicOnly` unless
`FEDERATION_ALLOW_PRIVATE`, which also allows http; answers at most 1 MB),
other servers' discovery and JWK Sets (kept 5 minutes, a failure 30 s,
a JWK Set fetched again once for an unknown `kid`), the code flow's tokens
we issued (in memory, by SHA-256, 300 s, `issue`/`redeem`) and those we
were given (`ShareToken`, renewed before they end or on a 401). The mode
and lists are the `federation` document of `server_settings` and the
`federation_servers` table (`trust`: off,
listed, open but refused; ourselves never). The OCM endpoints are routed
outside `/api` and the spec, as WebDAV (their API is OCM's):
`/.well-known/ocm` (`discovery`), `/.well-known/webfinger` (our users while
on, through `limits.address`), `/ocm/jwks`, `/ocm/shares`,
`/ocm/notifications`, `/ocm/invite-accepted`, `/ocm/token`, `/ocm/dav/…`.
**Every POST between servers is signed and checked** (`sign.rs` on
httpsig: one signature with `tag="ocm"`, `@method`, `@target-uri` (our
`PUBLIC_URL` + path), `content-digest` (sha-256, every known algorithm
must match), `content-length`, `created` within 5 minutes (60 s of skew),
`keyid` the JWK's `kid`, the algorithm from the JWK (`Ed25519`, or `EdDSA`
on Ed25519), an `alg` parameter only `ed25519`): `Federation::check_signed`
with the host the payload names (`sender`'s, `senderDomain`,
`recipientProvider`, `client_id`); we advertise `must-use-http-sig`, so an
unsigned one is refused. **Out** (`outgoing.rs`): a grant's `address`
(`routes/access.rs`) is `share_out`: federation on and the server allowed
(`server_refused`), `federation.allowed` of the drive
(`federation_forbidden`), contacts first (`may_share_with`:
a contact, a listed server, or `federation.share_with_any`;
`contact_required`), their discovery (`remote_unreachable`), our
extensions only to servers that have them (`drives_unsupported`,
`e2e_unsupported`); then the `ocm_shares` row (its id is `providerId`, the
secret only as SHA-256) and the signed `POST /shares` (refused: the row
goes, `remote_refused`). `strict` (`must-exchange-token`) whenever their
server can do the code flow; only a share made without it takes its secret
as a bearer. What a share's recipient may do anywhere in it is `role_in`
(the role as a grant gives it: a limited folder under the top cuts it, the
giver's own role caps it, a whole drive is cut by any limited folder,
upload only sees the top and what it added (`ocm_created`), nothing
encrypted without the share's `e2e`). `federation/dav.rs` is that WebDAV:
the share's top as root, reads from the index and the storage layer,
writes through `files::ops` (`dav::Upload` with `Writer::Share`), MOVE and
COPY only inside the share, and a REPORT `sync-collection` from
`ocm_changes`, which `record_change` fills (`log_change`, in the change's
transaction: each share's path, `gone` for a delete or a move out, using
`Change.from_path`; `log_access` after limited access changes), pruned
with the feed (`sync_floor`: an older token is `valid-sync-token`).
`change_role`/`unshare` tell the other server (`SHARE_CHANGE_PERMISSION`,
`SHARE_UNSHARED`), so does trashing what a share is of (`tell_ended`); a
notification that fails is logged, never retried. `run_live` (spawned by
`main`, and by the federation tests) sends `ORGANIZER_CHANGED` to servers
with `organizer-live` half a second after a burst of changes. **In**
(`incoming.rs`): `receive` checks the payload (sender and owner at the
signing host, `user` and `file`/`folder`, `webdav` with `uri` at the
sender's host and `must-exchange-token`, which we require: our criterion),
then makes a drive of `kind` `remote` (its root inserted, `e2e_root` its
own when encrypted), the `ocm_received` row (secret sealed with the
server key, `Backends::seal`) and the recipient's grant (on the root, or
the drive for a whole drive) with the role (OCM's permissions, or ours
between Organizer servers). Pending until accepted (`accept`: told,
`follow`ed; placing accepts too; declining tells and `forget_drive`s it,
as does SHARE_UNSHARED and removing the user, `decline_all`). Its tree's
storage is `incoming::storage`: opendal's WebDAV through `Guarded` with
the share's bearer (`backends::Bearer`), `Storage::webdav` (a MOVE is a
rename; a shared file is the drive's one file, `Dav.single`); on such a
storage a trash deletes there (`Files::trash_now`), no version is kept,
nothing is hashed, nothing counts against a limit (`check_room`), and its
health is `Federation::health` (`Drive.unavailable` `failing`). `follow`
asks sync-collection (else, or for a file, the ETag) and scans when
something changed; a REPORT is "unsupported" (ETag from then on) only for
400, 405, 415, 501 or `supported-report`: unreachable, refused or gone is
the share's `error` (`NoSync::Failed`), as is a scan that fails, and
sync-collection is asked again next time. `run_follow` looks every 5 minutes, every 30 s while
someone opened it (`Files::access` notes it), and a live notice looks at
once. `Node.remote` and `Drive.remote`, `SharedItem.remote` say where;
search reads remote drives only with `federation.read_remote`. A share
that arrives pushes `Kind::Shared` to its recipient (`routes/ocm.rs`).
A contact keeps what their server is (`Contact.provider`, discovery's
`provider`; `Contact.organizer`, it advertises our extensions; migration
0035): set when it is made (`add_contact` with the discovery), refreshed
by every fresh fetch of that server's discovery (`note_server`, in a task
of its own: a caller of `discover` may hold the writer).
Removing a user sends `USER_REMOVED` to the servers of their contacts and
shares (`outgoing::removed_user`, read before the rows go, then
`tell_removed` after the commit); one received drops the contact, forgets
what that person shared here and ends our shares with them.
**`organizer-e2e`** (`federation/e2e.rs`, migration 0034, docs/E2E_SPEC.md
§12): on the owner's server a member on another server is the share
(`remote_members`: `e2e`, not declined, `role_in` reaching the folder),
listed in `/encryption/folders/{id}` with `Member.remote` and the share's
id as `user_id`, its keys in `e2e_remote_keys`, its public key asked of
its server (`public_key`, `ocm_shares.public_key`, at most once a minute
while it has none); a share that ends or stops reaching the folder calls
`remote_gone` (keys out, `needs_new_key`), from `unshare`, a decline,
`tell_ended`, `USER_REMOVED` and `settle_e2e`. The two signed endpoints
are `/ocm/organizer-e2e/public-key` (`answer_public_key`, the member's
server) and `/ocm/organizer-e2e/keys` (`answer_keys`, the owner's). Our
WebDAV's sync-collection to an `e2e` share carries `o:id` and `o:root-id`;
the member's server keeps them as `source_id` (`sync_ids` after a scan in
`follow`, `with_source` after a folder is made) and answers
`/encryption/folders/{id}` (by a node or its `source_id`) with the keys
fetched from the owner (`fetch_keys`). Uploads into a remote share stay
one PUT read whole: opendal's WebDAV writer is one-shot (`Storage::upload`).
The tests are `tests/federation.rs`: two servers in one process over real
sockets.

**End-to-end-encrypted folders** (`files/e2e.rs`, `routes/encryption.rs`,
docs/E2E_SPEC.md, migration 0023): the server keeps keys it cannot use
(`e2e_keys`: public key, private key wrapped under the passphrase and the
recovery key; `e2e_folder_keys`: each folder key wrapped per member and
generation) and opaque names and contents. A node's `e2e_root` is its
encrypted folder's top (set from the parent by a trigger, so every insert
path agrees); it is in `Node`, the change feed and `TrashItem`. Nothing
moves across an encrypted folder's edge (`e2e::check_move`), none nests,
shares are of the top only, mounts never go inside one. Search, the drop
box (retrieval, placement, `writable_mounts`), links (refused on one,
left out of a plain folder's listing and zip) and WebDAV (hidden in
listings, any path into one is a 404) leave them out. The trash keeps
their ids (`e2e_trash_nodes`, restored by `restore_kept`), since names are
sealed with the parent's id; something inside goes back only into its own
folder. Members are the owner and whoever a share gives access
(`e2e::members`: who `roles::sees` it); `settle_e2e` (after every grant, team or limited-access
change, in `settle_mounts`) drops the keys of who lost access and sets
`needs_new_key`; `forget_gone` drops the keys of folders purged for good.

**Device sign-in** (`routes/device.rs`): an app gets a token of kind
`device` (scope `write`), with a password in one call (the TOTP code sent
again with the credentials once asked for), or through the browser as RFC
8252 has it: `GET /auth/device/authorize` → the web login if needed → the
approval page → `POST /auth/device/authorize` (session only) → a one-time
code (`device_codes`, a minute, deleted when tried) to the app's redirect
→ `POST /auth/device/token` with the PKCE verifier. Redirects only to
`organizer://auth` or the loopback address (`allowed_redirect`).

**Two-factor authentication** (`totp.rs` on top of `totp-rs`,
`routes/totp.rs`): `users.totp_secret` (base32, NULL = off),
`totp_pending` (a setup not yet confirmed) and `totp_last_step` (a code
works once: a step is accepted only when it is later, set with a
conditional UPDATE); recovery codes are SHA-256 hashes in `recovery_codes`,
deleted when used. A sign-in (password, or the single sign-on callback)
of a user with it on gets no session but the signed `organizer_2fa` cookie
(`__Host-` over https, `<user id>.<expiry>.<step>`, five minutes, the
`state.challenge_key`, its own): the password answers 200 `LoginStep`, the callback redirects
to `/login?step=totp`; `/login/totp` finishes it. With `TOTP_REQUIRED`
(`state.totp_required`), or the `sign_in.two_factor_required` setting of
the superadmin or one of the user's teams (`totp::two_factor_required`), a
user without it gets the `enroll` step instead
(`/login?step=totp-setup`, `/login/totp/setup`, `/login/totp/enable`), and
`/totp/disable` is refused. `next_step` decides, for both paths.

**The AI models** (`ai.rs`, `routes/ai.rs`, docs/M6C_DESIGN.md): a
registry, not one connection. The superadmin's models are `ai_models` rows
(`owner_id` NULL; users' own models are for later): a name (one per owner),
the provider (a genai adapter), endpoint, API key (never sent back), the
provider's model name and the capabilities chat, vision, speech,
embeddings. The `ai` document of `server_settings` holds the choices: the
default model (a
chat model) and the model of each `Work` (proposals, the default unless
another is chosen; embeddings, images, scans, audio, off while NULL), the
concurrency and the timeout. The `AI_*` variables are one more model, `env`
("From the environment", the locked default), `AI_EMBEDDINGS_MODEL` the
locked `env-embeddings`; `AI_MODEL_IMAGES`/`_SCANS`/`_AUDIO`/`_EMBEDDINGS`
lock a work to a model by its registered name (`Env::locked`); a model a
variable names cannot be removed. `Registry::model_for(work)` is the one
answer to "which model does this", filtered by capability. A model is tested
for each thing it can do before it is stored (a prompt, a 1×1 PNG, a tenth
of a second of WAV, a text to embed); a new provider or endpoint drops the
stored key unless one comes with it. Every model gets its own genai client
on our reqwest (redirects off, `AI_TIMEOUT`), with an auth resolver and a
service-target resolver that always give the model's key and endpoint, so
genai never reads a key from its own variables (`OPENAI_API_KEY`, ...).
Speech is an OpenAI-compatible `/audio/transcriptions` (reqwest multipart,
`verbose_json`; the segments that start in the first hour are kept). In
memory in `state.ai`, read again (`Ai::reload`) after every save, which
runs as a task holding `lock_saves`. Every request, whatever its model,
goes through `Ai::chat` / `look` / `transcribe` / `embed`, which wait for one
of the `AI_CONCURRENCY` permits (shared by all the models).
`Ai::recipient` is where requests go, shown on the settings pages under
"What Organizer sends to the AI" (change both together). Migration 0020
turned the old single connection into registered models.

**Reading photos, scans and recordings** (`reading.rs`): a JPEG, PNG, WebP
or GIF goes to the images' vision model as it is (base64, at most 20 MB;
no image library), the page images xberg finds in a PDF without text
(`extract::page_images`, 20 at most) to the scans' model, a recording
(`audio/*`, 100 MB at most) to the audio's speech model; only once a model
is chosen for the work, only where text is read now and as the user's
`read_contents` and the sensitive-file rules allow (a rule on the name or
path: never read; a keyword rule can only look at what the model gave
back). What a model read is kept in `model_reads` by user and content hash
(BLAKE3), so a file is read once (`READING` makes the copies of a content
wait for the first read). The drop box reads in its `analyse` job before
the proposal (the text marked as read by a model); the search index queues
a `read` job per file (`reading::Read`), which puts the text in the index
if the file still has that content and announces `Topic::Reindex`.
`reading::read_again` ("Read existing files again") clears `text_hash` of
the photos, PDFs and recordings no model read, so the index looks at them
again.

**Jobs** (`jobs.rs`): the `jobs` table is the queue, so a job survives a
restart (one that was `running` is queued again at start). A `Handler` per
kind, a worker per handler running up to `parallel()` at once, woken by
`Jobs::add` or the next `run_after`. A failure is retried after 30 s, then
2 min; the third is `failed`, kept with its error, and `give_up` is called.

**The drop box** (`drop_box/`, `routes/drop_box.rs`): an item (an upload,
a link) is an `inbox_items` row, its file staged in
`<DATA_DIR>/users/<id>/inbox/<item id>/`, and an `analyse` job
(`drop_box/analyse.rs`): the sensitive-file rules (`guard.rs`) on the name
and note, then nothing more without a model for the proposals (status
`waiting`, queued again when a superadmin sets one up), then the text
(`extract.rs`,
xberg on the bytes only, its cache off, 50 MB and 60 s at most, 32,000
characters kept; a photo, scan or recording read by its model,
`reading.rs`), then the keyword rules on that text. A `by_hand` rule
means nothing is read or sent; `name_only` means the text is dropped.
Then the proposal (`propose.rs`): the folders shown are those of
`retrieve.rs` (the whole tree up to 150 folders, else the top level and the
30 most alike by rare shared words, with their parents, and five file
names each), the prompt holds exactly what the settings pages list under
"What Organizer sends to the AI" (change both together), and the answer is
checked against the shown folders, the folder-freedom limit, the allowed
top-level folders and the name rules (a new name keeps the extension); a
wrong answer is asked again once, then the item `needs_manual`. The
pending mode decides what follows (`place.rs`: placed at once, or not; an
`accept` job for `timeout`). The user's actions are `review.rs`; they and
the analysis's own placing hold `review::LOCK`, so nothing is placed twice.
Another place than proposed is a `corrections` row, shown to the model as
an example. An undo or a rejection moves the file back to the inbox
(`Files::take_out`, journal op `take_out`) and removes the folders made
for it when they are empty. Every change of an item announces
`Topic::DropBox` (the `drop-box` live event).

**The WebDAV drop box** (`dav_inbox.rs`, `/dav-inbox/`): a drive of its
own over the waiting items (`dav_folder` '' / `review` / `auto`, which
override the pending mode). A PUT becomes an item whose analysis waits
`DELAY` (5 s); a PUT of the same name while it is still `queued` replaces
its file (Finder writes empty, then the content), and `queue_after` keeps
one queued analysis per item. Clients' `._*`, `.DS_Store` are discarded.
A MKCOL at a drive folder's top is a folder item (`add_empty_folder`);
what is written into it while it is `queued` is staged inside it, each
write pushing its analysis back (`WRITING` while a file is open, then
`DELAY`); once analysed it takes nothing more.

**Search** (`search.rs`, `routes/search.rs`): FTS5 (`search`) with
`search_docs` saying what each row holds. `search::run` (spawned in main
and the tests) follows the `changes` events, a second of quiet first, and
`refresh` compares the index with the nodes; text is read for files whose
content hash differs from `text_hash` (a move keeps the text), 50 a round,
only when the user's `read_contents` is `always` or `default_on` and no
sensitive-file rule matches; photos, scans and recordings by `read` jobs
when a model is chosen for them (`reading.rs`). A query is the words as quoted prefixes,
ANDed; bm25 weights name 10, path 3, text 1.

**Snapshots** (`drop_box/snapshot.rs`, lol_html): a link in `snapshot`
mode is the page made self-contained (its images, styles and fonts fetched
through the guarded client of `link.rs` and inlined as `data:` URIs, at
most 80 and 30 MB; scripts, frames, embeds, `on*` handlers dropped; links
absolute; a CSP that loads nothing) as `<title>.html`, with `<title>.md`
(xberg's Markdown) as its companion: `inbox_items.companions`, placed
beside it under its name, moved and taken back with it
(`placements.companion_nodes`).

**Embeddings** (`vectors.rs`, `embed.rs`): a `VectorStore` trait, so the
engine can be swapped; `TursoStore` (Turso, `vector32` and
`vector_distance_cos`, a scan per user) at `VECTOR_DATABASE_URL`
(`turso:<path>`; another engine is another scheme in `vectors::open`).
`embed::sync` runs after each search refresh: folders (path and examples)
and 16 chunks at most per file, keyed by fingerprint; the store is reset
when the model changes, and a save that changes the embeddings model
announces `Topic::Reindex` (internal, never sent to the apps). The analysis adds the
15 nearest folders to the lexical ones (`retrieve::context_with`);
`/search?semantic=true` adds the nearest chunks after the word matches.

**Push notifications** (`push.rs`, `routes/notifications.rs`,
docs/M6D_DESIGN.md): every push is a **content-free wake-up** (`PAYLOAD`,
an encrypted `{}` for Web Push; one fixed alert for APNs, `apns_payload`):
the device then asks `GET /api/notifications/news`, which words the
notices (`push::words`). Never put anything about a notice in a push.
Web Push (browsers, the Android app through UnifiedPush) is built by
web-push-native and sent with our reqwest (no redirects; the endpoint goes
through the links' guard, `link::allowed` and `PublicOnly`: https and a
public address unless `LINKS_ALLOW_PRIVATE`); the VAPID key is the
`push_keys` row, made at first start, unless `VAPID_PRIVATE_KEY`. APNs
(the iOS app) is HTTP/2 (`http2_prior_knowledge`) with an ES256 provider
token (only `iss` and `iat`, `kid` in the header, renewed after 40
minutes) signed with jwt-simple, which web-push-native re-exports (no
direct dependency); `APNS_*` set it up, and without them an `apns`
subscription is kept but skipped (`available: false`). 404/410 (APNs: 410,
`BadDeviceToken`, `DeviceTokenNotForTopic`) removes a subscription. What
notifies: `push::notify(state, user, kind)` (spawns; `notify_now` waits),
per the user's switches (the `notifications.*` settings; `notification_settings`
keeps only `review_notified_at`), a
notice kept in `notices` a week (news shows the last day). Items to review
(the badge's statuses, `WAITING`) go through `check_review`: `push::run`
follows the `drop-box` events and looks 5 s after a change; a notice only
when an item changed since the last one, and at most one per
`REVIEW_EVERY` (15 min; sooner, the look is scheduled for the end of the
quarter hour, once per user). A backup job's `give_up` tells the superadmins
(`notify_superadmins`). The tests run a stand-in push service and APNs (axum,
h2c) and decrypt the pushes with web-push-native's `decrypt`.

**Passkeys** (`passkeys.rs`, `routes/passkeys.rs`, webauthn-rs): a
passkey signs in on its own (no TOTP step), discoverable (no email), with
the password kept as a fallback. The relying party is `PUBLIC_URL`'s host;
without it, or with `OIDC_ONLY`, passkeys are off (403) and
`/auth/session` says `passkeys: false`. Ceremony state lives in
`passkey_challenges` (five minutes, taken once); finish uses
`finish_discoverable_authentication_v2` with the one passkey the credential
names, then stores the updated counter. The tests drive webauthn-authenticator-rs's
`SoftPasskey`, adding the user handle a discoverable authenticator would.
The apps get passkeys through the browser sign-in.

**Trash and versions** (`retention.rs`, `files/versions.rs`): the
`retention.*` settings (superadmin, team, user, drive), under `TRASH_DAYS`
and `VERSIONS_*` (`Files::retention_env`, which lock a rule): My drive by
its user's, a shared drive by its own and its team's. A replace through
`place_file` keeps the old bytes first (`keep_version`: a hard link, a
copy across filesystems, skipped for the same hash) in
`<drive root>/.organizer/versions/<version id>`, its row written in the
write's transaction; `WriteTarget::Replace` carries the `Source` (web,
api, webdav). Versions go to the trash with their file or folder
(`trash_id`, `trash_path`: a restored folder's contents get new ids, and
the versions are found again by path) and are purged with it.
`retention::run` sweeps hourly: trash past its days, `thin` (all within
the hours, then the newest per day, then per month), orphans, and bytes
without a row (under the tree's lock, so no write is half made).

**Backups** (`backup/`): rustic (`rustic_core`, `rustic_backend` with
opendal only) into a restic-format repository, a path or `s3://`.
Settings are the `backup` document of `server_settings`, with the
`BACKUP_*` variables on top that lock fields
(`BACKUP_KEEP` locks the three keep rules). `backup::schedule` queues a
`backup` job when due (`Schedule`: `daily HH:MM` or `every Nh`); the job
copies the database with `VACUUM INTO` into `<DATA_DIR>/backup/staging`,
backs up that, `<DATA_DIR>/users`, `<DATA_DIR>/drives` and each drive's
root outside them (the
paths canonicalised, as rustic records them), then forgets by the keep
rules (the newest always), prunes, and checks weekly. rustic is
synchronous and its S3 backend starts its own runtime: every call runs on
a thread of its own (`run::off_thread`), never in `spawn_blocking`. Each
snapshot's description is a `Manifest` (each user's files root, the
database copy), so a user browses and restores their own files
(`/backups`, restored next to the current one with the date in the name)
even after their root moved. `organizer backup snapshots|restore` is the
command line (`backup/cli.rs`): the variables alone when the database is
gone. The crypto crates are optimised in dev builds (`[profile.dev.package.*]`):
scrypt takes minutes unoptimised.

**Limits** (`limits.rs`, governor): every attempt counts, right or wrong,
and over the quota is a 429 with `Retry-After` (`HttpError::TooManyRequests`),
never a lockout. `state.limits.sign_in(ip, email)` for a password sign-in
(5 per email, 30 per address), `state.limits.user(id, ip)` for a code or a
current password (5 per user). Any new endpoint that checks a secret goes
through one of them. The tests get large quotas (`Options`), but for
`attempts_are_limited_per_email_address_and_user`; the quotas are
Potoo's and Woodmouse's too: change all three.

**Sessions** (`session.rs`): tower-sessions keeps them and axum-login
signs users in and out (`AuthSession`). The store is ours (the community's
sqlx store needs sqlx 0.8): the `sessions` table, keyed by the SHA-256 of
the session id, never the id; it writes only when a session changes
(sign-in, sign-out, a step-up), and expired rows are deleted hourly by `main`. Every
sign-in ends in `session::sign_in` (a new session id, 30 days); never
create a session another way. axum-login loads the user on each request
(`Backend::get_user`, so a deleted user is signed out at once) and checks
the session against the user's `auth_hash` (the password hash and the TOTP
secret): changing either signs the other sessions out, so after the user
changes their own, call `session::refresh` to keep the current one. The
cookie is `organizer_session`, `SameSite=Strict` (`__Host-organizer_session`,
`Secure` when `PUBLIC_URL` is https: `state.secure_cookies`, which decides
for every cookie). The 2FA challenge and the single sign-on login cookies
are separate signed cookies, not sessions.

**Cross-site requests** (`csrf.rs`, a layer outside the sessions): a
POST, PUT, PATCH or DELETE passes with `Sec-Fetch-Site: same-origin` (or
`none`), or, without that header, an `Origin` that is `PUBLIC_URL`'s (or,
without it, matches `Host`; forwarded headers never count), or neither
header (not a browser).
`same-site` is refused on purpose: a sibling subdomain is the same site.
Bearer requests skip it; Basic authentication (WebDAV) does not, as a
browser resends remembered Basic credentials by itself (WebDAV clients pass
anyway: they send neither header). A new endpoint that must take
cross-origin writes (none should) goes in `allowed`, with the reason.

**API keys** (`routes/api_keys.rs`, `key_user` in `auth.rs`): `api_keys`
holds the SHA-256 of `org_<64 hex>`, its scope (`read`/`write`;
an old `admin` is read as `write`) and its kind (`key`, `device` made by the apps' sign-in, or
`app_password`, which is WebDAV's Basic password and refused as a Bearer
token). One table for the three, as Nextcloud does. `AuthUser(user, Access)` takes the session cookie or
`Authorization: Bearer`; a read key gets 403 on anything but GET/HEAD there,
and the superadmin's endpoints take a `Superadmin` (a session, never a key).
Account endpoints (email, password, TOTP, API keys) take
`SessionUser`, which refuses keys: use it for any new endpoint about the
account itself. A handler that takes `AuthUser` lists `("api_key" = [])` in
its `security`. `last_used_at` is written at most once a minute.

**The drives' trees** (`files/`): the drive's storage (the disk, or a
bucket: above) is the source of truth, and the index (`nodes`) describes
it; for what Organizer writes, the index is the truth (it is written with
it). A tree is a drive's (`Files::tree(drive_id)`, `drive_root` for one on
the disk).
- My drive's tree is at its user's files root: `users.files_root` when the
  superadmin chose one, else the default `<STORAGE_URL>/users/<id>/files`
  (`Files::root_for` with a `StoredRoot`; `STORAGE_URL` is a path or an
  `s3://bucket/prefix` whose drives are backend drives). Drives' roots are
  apart (`check_apart`). Organizer's own folders (uploads, the drop box) are
  scratch under the local base (`STORAGE_URL` when a path, else `DATA_DIR`),
  never in the tree; the backup work dir is in the data dir,
  but for each drive's `.organizer/` (trash and versions);
  `Files::check_root` keeps a chosen folder and the data dir apart.
- Everything that touches a tree holds its lock: a scan (`files/scan.rs`,
  `Tree::read_lock`) and every operation (`files/ops.rs`,
  `Tree::write_lock`, which refuses while the drive is being moved). An
  operation is journaled (`files/journal.rs`): the intent is written, then
  the storage is changed, then the index, the change feed (`record_change`)
  and the end of the intent are committed in one transaction. A storage
  step that fails abandons its intent (on S3 after putting back what a
  move copied). Never change a tree without going through `ops.rs`.
- The reconciler adds what appeared, updates what changed (size or mtime),
  removes what vanished (only the topmost of a vanished subtree, which takes
  its rows with it), and matches a vanished file to an appeared one by size
  and hash: a move made outside Organizer keeps its id. Folders are not
  matched. Hashing (BLAKE3) of files without a hash is a pass of its own,
  after the scan and without the lock. An empty or missing root while the
  index has files is `Unavailable` (an unmounted share), never a mass delete.
- Watching (`files/watch.rs`): notify's events start a scan two seconds
  after they stop, and every tree is scanned every ten minutes anyway.
- Deletes go to the trash (`<drive root>/.organizer/trash/<id>`, the `trash`
  table) and keep their id for a restore. `move_path` copies across
  filesystems (a share and the local data dir).
- Names Organizer makes go through `names::new_name` (NFC, portable) and
  `check_free` (no case-only clash). Names made outside are kept as they
  are, with `portable` false when other systems refuse them.
- The change feed keeps 30 days (`prune_changes`, hourly). A cursor is
  `<epoch>.<seq>`: `users.feed_epoch` changes with a new files root,
  `users.feed_floor` rises with pruning; either makes an old cursor a 410.
- After every commit that changes a tree, `Files::notify` tells the open
  apps (`GET /api/events`, SSE) that the feed has new rows.
- The files API (`routes/files.rs`): the ETag is the version, and every
  write takes the current one (`if_version`, or `If-Match` on `PUT
  .../content`): a stale one is a 409. Plain uploads (multipart, `PUT`) are
  streamed to `<DATA_DIR>/uploads` and capped at `PLAIN_UPLOAD_MAX`, then
  moved into the tree whole by `Files::write_file`. Downloads go through
  tower-http's `ServeFile` (ranges) with `Content-Security-Policy: sandbox`
  and `nosniff`: a file is never a page of this origin.
- WebDAV (`dav.rs`, at `/dav/`, outside the OpenAPI document): Basic
  authentication with an app password only. Reads go through dav-server's
  `LocalFs`; every change through `files::ops` (MKCOL, DELETE to the trash,
  MOVE keeping the id, PUT to the uploads folder then placed whole on
  `flush`; COPY by `LocalFs`, then a scan). Paths are normalized to NFC.
- Resumable uploads (`routes/uploads.rs`): draft-ietf-httpbis-resumable-
  upload at interop version 6 (URLSession's), without the 104 (hyper cannot
  send 1xx). One request at a time per upload (`Files::upload_locks`); the
  bytes in `<DATA_DIR>/uploads/<id>.part`, the offset in the `uploads` row,
  updated to what is on disk even when a request breaks off. A complete
  upload goes into the tree through `Files::place_file`, which keeps the
  bytes if placing fails. Uploads expire 24 hours after their last byte.
  An upload created with `drop_box=true` (a name, and the per-item `note`
  and `read_contents` of a multipart drop, checked at creation and kept in
  the row) becomes a drop-box item when complete, through the same
  `drop_box::add_file` as `POST /drop-box/files`; its answer is the item
  (`Uploaded`: the file, or the item). This is how a drop-box file over
  `PLAIN_UPLOAD_MAX` (100 MB) gets in.

**Tests run on an in-memory database**, where the reader and the writer
pools are one and the same single connection. So a write made through
`db.reader()` (read-only on a real file) passes the tests but fails in
production, and a read on `db.reader()` while a writer transaction is open
works in production but deadlocks in the tests. The test
`readers_see_the_writes_and_do_not_block_the_writer` in `db.rs` uses a real
file; add one like it when you change how the pools are used.

**The API is described by OpenAPI, and the frontend's SDK is generated from
it** (nothing is committed; served only with `SWAGGER_UI`):

- Every handler has a `#[utoipa::path]` (path, method, `operation_id`, tag,
  summary, request body, every response including the errors, `security`)
  and is registered with `.routes(routes!(handler))` on the `OpenApiRouter`
  of its module's `router()` (utoipa-axum): the router is the one and only
  list, so a routed endpoint is documented and a documented one is routed.
  The path in the annotation is relative to where the router is nested
  (`/api`, so `/health`, not `/api/health`; handlers with the same path go
  in one `routes!(a, b)`). Request and response bodies are named structs
  with `ToSchema` (never `json!` or a struct declared inside a handler);
  query parameters are `IntoParams`. `openapi.rs` only holds the title and
  the tags; its tests check that every operation has a unique id, a tag and
  a summary.
- Errors are `HttpError` (`error.rs`, thiserror), answered as
  `{"error": "..."}`. A failure on our side is logged with its causes and
  answered with a plain 500: no internal detail reaches the client.
- The spec is written by the `openapi` binary, built with `--features spec`,
  the only build allowed to compile without `apps/frontend/dist`
  (`allow_missing` in `frontend.rs`). Never ship a `spec` build. The order
  is always spec → SDK → frontend build → backend build with the frontend
  embedded (the mise tasks keep it).

**rust-embed** needs `apps/frontend/dist` to exist to compile, even in
debug. The mise tasks that only compile Rust (dev, checks, tests) satisfy
that with an empty folder (`ensure:dist`, which is all rust-embed needs);
the release builds (`build`, `build:linux`) depend on the real
`build:frontend`. On a fresh checkout, run tasks through mise rather than
raw `cargo`.

**Endpoints:** everything the app serves itself lives under `/api/`; every
other path is the embedded frontend (with the SPA fallback to
`index.html`), so an unknown `/api/` path is a JSON 404, never the app.

## Conventions

- **No unsafe code**: `unsafe_code = "forbid"` in `Cargo.toml`'s `[lints.rust]`; nothing can lift it. A need for unsafe is a reason to find a crate that hides it, or to ask.

- Settings from the environment go through `config.rs` (`var`, `flag`:
  a yes/no value that is not understood stops the start).
- Integration tests are in `tests/api.rs` (axum-test, an in-memory
  database), federation's in `tests/federation.rs` (two servers on real
  sockets); unit tests next to the code.
- The static Linux binary is musl (`scripts/build-linux.sh`, zig as the C
  toolchain). A dependency with C code must build that way; a library that
  only exists for glibc needs a decision first (`docs/ARCHITECTURE.md` §3.4).
