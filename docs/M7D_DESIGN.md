# M7d design proposal — mirrors

Status: **accepted 2026-11-10** (§8). Step 4 of [M7_DESIGN.md](M7_DESIGN.md) (§6, §9), on
top of M7c's storage backends. Federation is step 5.

## 1. What a mirror is

- **A mirror is a one-way, read-only copy of a drive in one of the user's
  own storage backends** (never a global one): plain files at their paths
  under `<prefix>/<mirror id>/`, readable with any S3 tool without
  Organizer.
- **Deletes and changes are copied exactly**, but the mirror keeps **its
  own trash and versions** (in its `.organizer/`): a file deleted or
  overwritten in the drive goes to the mirror's trash or versions, kept by
  the mirroring user's retention settings. So a mirror also protects
  against a delete upstream, for as long as that retention.
- **What is copied is what the mirroring user can see** (M7b): limited
  folders they are not in, and other people's uploads in an upload-only
  folder, are never copied. Viewer and above may mirror; upload only may
  not.
- **Encrypted folders stay sealed**: their files and names are copied as
  the server holds them (ciphertext), and open with the user's key as
  anywhere else.
- **A mirror stays when its drive is deleted**, or when the user loses
  access: mirroring stops and the mirror says why.

## 2. When it copies

The user's choice per mirror: **continuous** (the default: within about a
minute of a change, from the drive's journal), hourly, daily, or only when
asked ("Mirror now"). A mirror on private credentials (M7c §3) copies only
while a session has them unlocked, and catches up when one does.

## 3. Who may mirror what

- **My drive**: its owner, to their own storage, whenever they have one
  (`storage.own_backends`). **Shares placed in My drive are not copied**
  (they are other people's drives); they can be mirrored on their own
  only as the shared drive rules allow.
- **Shared drives**: **off until the superadmin enables it** (a setting,
  `mirrors.shared_drives`, superadmin and team level). A drive's managers
  can **forbid it for their drive** (a drive-level setting,
  `mirrors.allowed`); forbidding stops existing mirrors of it (they stay,
  stopped).
- **Members see who mirrors**: the drive's access list shows "Mirrored by
  Alice (her own storage)".

## 4. Eject

- **Make it a drive (in place)**: the mirror becomes a drive on the same
  backend at the same place, with the user as its manager; mirroring stops.
  Nothing is copied.
- **Make a copy**: a new drive on that backend with the mirror's current
  contents; the mirror goes on mirroring.

## 5. Where users see mirrors

- **The sidebar's "Mirrors" section** lists the user's mirrors as
  read-only drives (`kind: mirror`, `my_role: viewer`): browse, download,
  and restore from the mirror's trash or versions (a restore copies the
  file back into the source drive when the user may write there, else
  downloads it). The desktop and the apps show them read-only through the
  same drives list, with no extra screens.
- **Settings → Mirrors**: every mirror with its drive, backend, schedule,
  last copy, status (up to date, copying, waiting for unlock, stopped and
  why), and Mirror now, Change, Eject, Remove (removing a mirror deletes
  its copy only after a confirmation that names the bucket).

## 6. The API (outline)

`GET/POST /api/mirrors` (`drive_id`, `backend_id`, `schedule`),
`GET/PATCH/DELETE /api/mirrors/{id}`, `POST /api/mirrors/{id}/run`,
`POST /api/mirrors/{id}/eject` (`in_place` or `copy`);
`Drive.kind` gains `mirror` with `mirror_of`; a drive's access answers
`mirrored_by`.

## 7. Pages (mockups)

1. "Mirror…" from a drive's page: choose the backend and the schedule.
2. Settings → Mirrors.
3. A mirror's page in the sidebar: read-only, with its status and Eject.
4. A shared drive's access list with "Mirrored by".

## 8. Decided (2026-11-10)

1. **The design** and **the pages** as proposed and mocked up.
2. **Mirrors are read-only drives** in the sidebar and the apps.
3. **Shares placed in My drive are not in its mirror.**
4. **A mirror's trash and versions follow the mirroring user's
   retention.**

## 9. Built (backend)

Migration 0030 (`drives.kind` gains `mirror`, the table made again with
its view and triggers; `mirrors`; `nodes.source_id`; an index of the
change feed by user and drive). `files/mirrors.rs` (the copy, the
schedule, eject, restores), `routes/mirrors.rs` (the API, tag `Mirrors`).
No new crate. Choices made while building:

- **A mirror is a drive.** Its `drives` row (`kind = 'mirror'`, in a
  backend of its user's own, the name of the drive it copies) and its
  `mirrors` row share one id, so its tree, trash and versions are a
  drive's at `<prefix>/<mirror id>/`, and ejecting it in place only turns
  the row into a shared drive's. Its user has a `viewer` grant on it: the
  files API browses it and downloads from it, versions are listed and
  downloaded there, and the roles refuse every write (`my_role` `viewer`
  on every node). Only the copy writes there, through the operations of
  every drive (journaled; a replaced file's old content a version, a
  deleted one in the trash), so a crash part-way is the journal's as
  anywhere. The trash and versions follow the mirroring user's own
  settings (`retention::rules` resolves a mirror for its user).
- **The copy** compares, by node, what the user sees of the drive (the
  listings' rules from the root down: a limited folder they are not in,
  others' uploads where they may only upload; a folder hidden from them
  hides what is under it, even an item granted to them below) with the
  mirror's index: `nodes.source_id` is the node each copy is of. New is
  copied (through the uploads folder, as every write; the limits of the
  mirror's backend count), a different hash is written over, a move or
  rename is a move (the copy keeps its id), and what the user no longer
  sees goes to the trash, the topmost of each part. A file of the drive
  not hashed yet is copied when new or of another size, else left for
  the next copy. A name taken by a copy still to be moved elsewhere
  steps aside (`<name>~<8 hex>`) until it is; one taken by what goes is
  trashed first. Names are kept as the drive has them (made outside
  Organizer, not portable, differing only in case): a copy clashes only
  with the same name. Placed shares are not nodes of My drive, so they
  are never copied.
- **"From the journal"**: the drive's changes reach the mirroring user's
  change feed already (every change of what they see, and access gained or
  lost). A mirror keeps the last row its last complete copy took in
  (`seen_seq`); a later row is a change to copy. The loop looks every 20
  seconds (and at once when woken: a new mirror, "Mirror now", a new
  schedule), so a continuous mirror copies within about a minute. Hourly
  and daily copy when something changed and that long passed since the
  last complete copy; manual only when asked. A failed copy is tried
  again after five minutes.
- **A stop part-way**: `running_since` stays set; the next start's first
  look finishes that copy whatever the schedule. A file written to the
  bucket but not indexed is found by the start's scan without a source;
  the copy takes it for the drive's node of the same name and kind in the
  same folder (not copied again, nothing trashed or versioned).
- **Encrypted folders** are copied as the server holds them: opaque names
  and contents; a copied encrypted folder's top has `e2e_root` its own id
  (so search, links and WebDAV leave it out as anywhere), with no keys of
  its own (no `e2e_folders` row). Its keys and names stay sealed with the
  drive's ids: `Node.source_id` gives them (`GET
  /encryption/folders/{source_id}` for the keys, a parent's `source_id` to
  open a name). A drive ejected as a copy keeps the drive's ids there too.
- **A mirror keeps its user's keys** of the encrypted folders it copied
  (migration 0031, `e2e_copy_keys`; fixed after M7d was first built: the
  copies could no longer be opened once the drive's keys went). The
  user's wrapped keys of a drive's encrypted folder are dropped when the
  drive is deleted, when they lose access, or when they leave the
  folder's members; a mirror must stay usable then (§1). So the mirror
  keeps a copy of **its own user's wrappings only** (never another
  member's; still wrapped, nothing in plain), every generation they had:
  taken after each copy (`e2e::keep_for_mirrors`) and at once when keys
  are posted for the folder (a new generation made while mirroring,
  without waiting for a copy). A generation kept is never changed. They
  go with the mirror's drive, or when the copy is purged from its trash
  (`forget_gone`); a drive ejected as a copy takes them along for its
  own copies. **The API is unchanged but for what it answers** (the spec
  says it): `GET /encryption/folders/{id}` answers the kept keys
  (`e2e::kept_keys`) for a node of the copy, and for the drive's folder
  or anything in it (by `source_id`) once the user can no longer read it
  there; while they can, the drive's own answer as before. The kept
  answer's `folder_id` is the drive's folder (the id the keys and names
  are sealed with, so the same whether asked by the copy's id or the
  drive's), with `members` empty and `needs_new_key` false; posting keys
  for a copy is a 404. **Clients**: nothing changes for one that asks by
  `source_id`, as above; one that asks by a copy's id must unwrap with
  the answer's `folder_id` (as the associated data), not the id it asked
  with. Tests: the keys still answered after access is lost and after
  the drive is deleted (by the copy's ids and the drive's, a generation
  made while mirroring included, and from a drive ejected as a copy);
  nobody else (the drive's other members, its manager, the superadmin)
  gets them, and only the mirror's user's wrappings are stored.
- **Who may mirror** (`mirror_refusal`, asked when made and before every
  copy, never stored): My drive by its user; a shared drive by a viewer or
  above, while `mirrors.shared_drives` (bool, off; superadmin and team
  levels, a drive's by its team, as the drive settings) is on and
  `mirrors.allowed` (bool, on; drive level, its managers; not listed for
  My drive) is not off. Either reason, access lost and the drive deleted
  stop a mirror; it keeps what it has, and goes on when the reason goes
  (but a deleted drive). The target must be one of the user's own
  backends while `storage.own_backends` allows them; a mirror is not
  mirrored.
- **Locked**: a mirror whose storage (or whose drive's) is on private
  credentials no session holds is not copied (`waiting_for_unlock`); "Mirror
  now" then waits for the unlock.
- **Status** (`MirrorStatus`): `up_to_date`, `pending` (changes, or a
  copy asked for, wait for the schedule or "Mirror now"), `copying` (with
  `progress`: files and bytes to copy and done), `waiting_for_unlock`,
  `failing` (`error`, fit to show), `stopped` (`stopped`: `drive_deleted`,
  `access_lost`, `forbidden`, `shared_drives_off`, with `stopped_reason`
  in words). The two extra ones, `pending` and `failing`, say what the
  four of §5 left out.
- **Eject**: in place keeps the id, the objects and everything indexed,
  the grant becomes `manager`, the row goes, and the name is the mirror's
  (its drive's) unless one is given; it must be free among the shared
  drives (409). As a copy, a new shared drive on the same backend made by
  the copy itself from the mirror (its current tree; not its trash or
  versions), answered once done; refused for both while a copy runs (409).
  Both need `storage.own_backends` to allow the user's storage.
- **Restores into the drive mirrored**: from the mirror's trash, the item
  (a folder with what was in it) is written, as a new item, into its
  folder there now (by the copy's source), else the folder at its path,
  else the root; from its versions, over the file the copy is of (whose
  content becomes a version there), else as a new file where it was. Each
  as the user, where they may add or change (`access_for`); refused with
  `code` `drive_read_only` (download instead: a trashed file has its own
  download, `GET /mirrors/{id}/trash/{item_id}/content`; a version is
  downloaded as any), `drive_deleted` or `access_lost`.
- **Removing a mirror** deletes its objects (its drive's deletion); the
  confirmation naming the bucket is the client's (`Mirror.bucket`,
  `location`). Removing the user's backend, or the user, takes their
  mirrors out of Organizer with their other drives there (the objects
  stay).
- **Not searched**: a mirror's drive is (search leaves mirrors out). Not
  in the drop box's folders, not among the superadmin's shared drives,
  never backed up by the server (it is in a user's own storage).

**The API** (all in the spec):

- `GET /mirrors`, `POST /mirrors` (`drive_id`, `backend_id`, `schedule`:
  `continuous` by default, `hourly`, `daily`, `manual`; 201 `Mirror`; 403
  with `code` `access_lost`, `shared_drives_off` or `mirroring_forbidden`;
  400 for a storage not the user's own), `GET/PATCH/DELETE /mirrors/{id}`
  (PATCH: `schedule`), `POST /mirrors/{id}/run` (202; 409 `code`
  `mirror_stopped`), `POST /mirrors/{id}/eject` (`mode` `in_place` → 200
  `Drive`, or `copy` → 201 `Drive`; `name`), `POST
  /mirrors/{id}/trash/{item_id}/restore` and `POST
  /mirrors/{id}/versions/{version_id}/restore` (200 `Node` of the drive
  mirrored), `GET /mirrors/{id}/trash/{item_id}/content`.
- `Mirror`: `id` (its drive's too), `root_id`, `drive_id` (null once
  deleted), `drive_name`, `drive_kind`, `backend`, `bucket`, `location`,
  `schedule`, `status`, `stopped`, `stopped_reason`, `error`, `progress`,
  `last_copy_at`, `restores_into_drive` (the user may write to the drive:
  a restore copies back; else download), `used`, `created_at`.
- `Drive.kind` gains `mirror`; `Drive.mirror_of` (null for other drives):
  `drive_id`, `drive_name`, `schedule`, `status`, `stopped`,
  `restores_into_drive`. `GET /drives` lists mirrors after the shared
  drives, by name; a mirror's `my_role` is `viewer`.
- `DriveAccess.mirrored_by`: the users (`user`, `since`) whose mirror of
  the shared drive copies (stopped ones left out).
- `Node.source_id` (and `Entry`): in a mirror, the node it copies.

**Tests**: integration tests (`tests/api.rs`) of each schedule; changes,
deletes and moves into the mirror's versions and trash, kept and swept by
the user's own rules; what the user sees (limited folders, upload only,
placed shares, an encrypted folder, upload only refused); the shared-drive
setting, the drive's forbidding and `mirrored_by`; private credentials
waiting and catching up; access lost and the drive deleted; eject in
place and as a copy; restores into the drive (a file, a folder, a version;
refused for a viewer, downloaded instead); a copy cut short by a stop
finished at start. Unit tests of the copy's index and of what stops a
copy. Not exercised: real S3 (the stand-in as in M7c), the loop's own
timing (the tests look when they want).

**The load test** is not worth a run for this step: nothing on the hit
path of uploads, downloads or listings changed but one more column in
the listings (`source_id`); a mirror's copies are background work the
load test does not make.

**Finishing M7d (2026-11-10)**, after the first build was tried:

- **When it stopped**: `Mirror.stopped_at` and `Drive.mirror_of.stopped_at`
  (null while it goes on). Migration 0032 (`mirrors.stopped_at`): the
  first look that finds a mirror stopped notes the time
  (`Files::mirror_stop`, from the loop every 20 s and from every answer
  about the mirror), a trigger sets it when the drive is deleted (the
  foreign key's SET NULL), and it is cleared when the mirror goes on, so
  each stop has its own. The web's banner: "Mirroring stopped: the drive
  was deleted on 2 Nov. This copy stays until you remove it." ("today"
  for today).
- **Eject's and removal's 409s have codes**: `name_taken` (a shared drive
  has the name) and `copy_running` (a copy runs; an eject as a copy now
  claims the mirror too, so the loop does not copy into it meanwhile).
  The web's Eject dialog asks for another name on `name_taken` and words
  both by their code, not the message.
- **A mirror's trash opens encrypted names**: a trashed node keeps the
  drive's ids it had (migration 0032: `trash.source_id`,
  `parent_source_id`, `e2e_root_source_id`), answered as
  `TrashItem.source_id`, `original_parent_source_id` and
  `e2e_root_source_id` (null elsewhere); the web's trash opens a name with
  them (`trashSealedAs`). A restore in a drive ejected from a mirror gives
  the node its `source_id` back.
- **The sync engine's tests make a real mirror**: `Backends::stand_in_own`
  (doc-hidden, beside `stand_in`) adds a user's own backend whose S3 the
  in-memory stand-in is, and the tests call `Files::create_mirror`, as
  `POST /mirrors` does, instead of forcing SQL past the CHECK. That
  found a bug in the engine: a mirror's kept keys, answered by the copy's
  id, were unwrapped with that id rather than the answer's `folder_id`
  (the drive's folder), so an encrypted folder in a mirror stayed locked
  once the mirror kept keys; `e2e.rs` now unwraps with `folder_id`.
- **The settings switch that showed the opposite** (`levels.tsx`): not
  reproduced in a normal browser session. Tried on the superadmin's page
  (`mirrors.shared_drives`, `links.password_required`, also with the
  step-up expiring mid-write) and on a shared drive's page
  (`mirrors.allowed`, `links.password_required`, with a mirror of the
  drive), on the built app and on Vite's dev server, with direct loads
  and in-app navigation, a slow server (3 s), the label and the switch:
  each time the PUT, then the list's refetch, then the switch (its
  `aria-checked` and its hidden checkbox) and the server agreed. What was
  seen is the hidden Browser pane: a page there paints no frames, so the
  Switch's CSS transition does not run, and a screenshot (or the computed
  style) still shows the old position for seconds after `data-checked`
  changed. A second click then rightly flips the value that was saved. No
  code change.
- **Tried in a browser** (the dev server against a local server and a
  standard-library S3 stand-in): Mirror… of a shared drive; a stopped
  mirror's banner with its date; Eject as a copy, and in place refused
  with `name_taken` (the dialog asked for another name); an encrypted
  folder's item deleted in the drive, its name read in the mirror's
  trash.
