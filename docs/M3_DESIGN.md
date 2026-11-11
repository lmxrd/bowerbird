# M3 design proposal — the safety net

Status: **accepted 2026-11-07** (§7) and built: the trash's rules, versions, backups with rustic (local and S3; S3 not yet tried against a real bucket), restores by users and from the command line, and the pages. It covers
milestone M3 of [ARCHITECTURE.md](ARCHITECTURE.md) (§4.8 there): the
trash's retention, per-change versions, and backups with rustic. The
questions that need a decision are in §7.

Already there from M1: every delete (web, REST, WebDAV, sync, the drop box)
moves the file or folder to `<DATA_DIR>/users/<id>/trash/<id>` through the
journal, and the trash page restores it or deletes it for good. Nothing is
ever removed without that last step.

## 1. The trash

- **Retention.** Items are deleted for good a number of days after they
  were deleted (default **30**; `0` keeps them until the trash is
  emptied). Each user's own setting (§1.1); `TRASH_DAYS` locks it for everyone. A daily job does the
  purging, through the same `purge` as the button.
- **Empty the trash**: a button and `DELETE /trash` (everything of the
  signed-in user), with a confirmation.
- **The page** shows, for each item, when it goes ("in 23 days"), and the
  rule above the table.
- A file's versions (§2) go to the trash with it, come back with a
  restore, and are deleted with a purge.

### 1.1 Who sets the rules

Each user sets their own trash and version rules (§7, question 3). The
admin sets the **defaults** a user has until they change them, on the
same form; a `TRASH_DAYS` or `VERSIONS_*` variable locks a rule for
everyone, as the `AI_*` variables lock their fields. A `retention` row per
user holds only what they changed (NULL: the default).

## 2. Versions

### 2.1 When one is kept

When a write **through Organizer** replaces a file: a REST upload over a
file, a WebDAV `PUT` over one, the sync client (M5), a restore of an
older version. Before the new bytes move in, the old file moves to
`<DATA_DIR>/users/<id>/versions/<file id>/<version>`, in the same journal
intent as the write (the journal's `write` gets the version's path, so a
stop half way is put right at start as the other operations are).

Not kept:
- **A write of the same bytes** (same BLAKE3 hash): nothing changed.
- **A change made directly on disk** (on the NAS share, by another
  program). The reconciler sees it only after the old bytes are gone.
  The backups (§3) are the safety net for those; the settings page says so.

### 2.2 How long

Thinned as Time Machine does, by the version's age:
- **every version** for the first **24 hours**;
- then **one a day** (the last of each day) for **30 days**;
- then **one a month** for **12 months**;
- then none.

Three settings, each user's own (§1.1); `VERSIONS_HOURS`, `VERSIONS_DAYS`
and `VERSIONS_MONTHS` lock them for everyone. All `0` turns versions off. The thinning
runs in the same daily job as the trash. The storage page shows how much
the versions and the trash take.

### 2.3 Using them

- **A file's menu → Versions**: a dialog listing the versions (when,
  size, by which way: web, WebDAV, device name), each with **Download**
  and **Restore**.
- **Restore** makes that version the current file. It is itself a write,
  so the file that was current becomes a version: a restore can be undone.
- **Delete** a version for good, from the same list.

### 2.4 Table and endpoints

```sql
CREATE TABLE versions (
  id BLOB PRIMARY KEY NOT NULL CHECK (length(id) = 16),
  user_id BLOB NOT NULL REFERENCES users(id) ON DELETE CASCADE,
  node_id BLOB NOT NULL,          -- the file (in the tree or in the trash)
  version INTEGER NOT NULL,       -- the file's version it was
  size INTEGER NOT NULL,
  hash BLOB NOT NULL,
  mtime_ns INTEGER NOT NULL,
  source TEXT,                    -- 'web', 'webdav', a device's name, ...
  created_at TEXT NOT NULL DEFAULT (strftime('%Y-%m-%dT%H:%M:%fZ', 'now')),
  UNIQUE (node_id, version)
);
```

| Method and path | What |
| --- | --- |
| `GET /files/{id}/versions` | The versions of a file, newest first |
| `GET /files/{id}/versions/{version}/content` | Download one |
| `POST /files/{id}/versions/{version}/restore?if_version=` | Make it the current file |
| `DELETE /files/{id}/versions/{version}` | Delete it for good |
| `DELETE /trash` | Empty the trash |
| `GET`/`PUT /retention` | The signed-in user's trash and version rules, with the defaults and locks |
| `GET`/`PUT /admin/retention` | The defaults (admin) |

## 3. Backups

### 3.1 What, and where

A backup is of **the whole server**, made by the admin's settings: the
database, every user's files folder, and the data dir's trash, versions
and drop box. Not uploads in progress, caches, or the vectors
(`vectors.db` is made again from the text).

- **The database** is copied first with `VACUUM INTO` to a staging file
  in the data dir: a consistent copy at one moment, taken while the
  server runs. The files are read after it. A file changed during the run
  may be newer than the database says; after a restore the reconciler's
  rescan settles that, as it does after any change on disk.
- **The repository** is a [rustic](https://rustic.cli.rs) (restic-format)
  repository: deduplicated, compressed and encrypted. It can also be read
  with the `restic` or `rustic` command, without Organizer.
- **Targets:** a local path (another disk, a mounted share), or **S3**
  and S3-compatible storage (AWS, Backblaze B2, Scaleway, MinIO, ...)
  through rustic's opendal backend. Others it supports (SFTP, a REST
  server) are another scheme later.

### 3.2 Settings (admin)

One `backup_settings` row with `BACKUP_*` variables on top that lock their
field, as the `AI_*` and `OIDC_*` ones do:

| Field | Variable | Meaning |
| --- | --- | --- |
| `repository` | `BACKUP_REPOSITORY` | `/mnt/backup/organizer` or `s3://bucket/path`; empty: no backups |
| `s3_endpoint`, `s3_region` | `BACKUP_S3_ENDPOINT`, `BACKUP_S3_REGION` | For S3 that is not AWS |
| `s3_access_key`, `s3_secret_key` | `BACKUP_S3_ACCESS_KEY`, `BACKUP_S3_SECRET_KEY` | The secret is never sent back |
| `password` | `BACKUP_PASSWORD` | The repository's key. Never sent back |
| `schedule` | `BACKUP_SCHEDULE` | `daily 03:00` (the default), or `every 6h` |
| `keep_daily`, `keep_weekly`, `keep_monthly` | `BACKUP_KEEP` (`7,4,12`) | Which snapshots `forget` keeps; `prune` follows |

**The password** encrypts everything. It is kept in the database (the
scheduled runs need it), but a backup cannot be read without it, so the
copy in the backed-up database is no help after losing the server. The
page says so plainly, shows the password once when it is generated, and
asks the admin to keep it somewhere else.

A **Test** button, as for the AI connection: opens (or creates, after a
confirmation) the repository and answers its snapshots.

### 3.3 Running

- A **`backup` job** on the existing job queue, added by the schedule and
  by **Back up now**; one at a time. rustic is synchronous: it runs in
  `spawn_blocking`.
- After a backup: `forget` by the keep rules, then `prune`. A `check` of
  the repository's structure once a week.
- **Status** on the backups page: the last run (when, how long, how much
  was new, the total size), the next one, and the last error. A failed or
  missed backup also shows a warning on the admin's settings page and in
  the log.

### 3.4 Restoring

- **One file or folder, by the user** (§7, question 2): a folder's menu →
  **From a backup…** lists the snapshots and shows the folder as it was
  in one; a file or folder is restored **next to** what is there now, as
  `Report (2026-11-05).pdf`, never over it. Each user sees only their own
  files folder in the snapshots.
- **Everything, by the admin**, with the server stopped:
  `organizer backup restore [--snapshot latest]` puts the database and the
  folders back where the settings say, then the server starts and
  rescans. `organizer backup snapshots` lists them. Documented in the
  README, with how to do the same with `restic` alone.

### 3.5 Endpoints

| Method and path | What |
| --- | --- |
| `GET`/`PUT /admin/backup` | The settings and the status (admin) |
| `POST /admin/backup/test` | Open the repository, answer its snapshots |
| `POST /admin/backup/run` | Back up now |
| `GET /backups` | The snapshots (when, id), for the signed-in user's view |
| `GET /backups/{snapshot}/files?path=` | A folder of the user's files in a snapshot |
| `POST /backups/{snapshot}/restore` | Restore a path of it next to the current one |

## 4. Crates

- **rustic_core 0.13.0** and **rustic_backend 0.7.0** (Apache-2.0 or MIT,
  August 2026), the library under the `rustic` command. rustic_backend
  without its default features: `opendal` (local and S3) only. It brings
  its own reqwest 0.12 (we are on 0.13), as genai does.

No other new crate: the schedule is two forms we parse ourselves
(`daily HH:MM`, `every Nh`), not a cron expression.

## 5. The web UI (mockups first)

1. **Trash**: "in 23 days" per item, the rule, **Empty trash**.
2. **Versions dialog**: from a file's menu.
3. **Settings → Backups** (admin): the repository, the password, the
   schedule and keep rules, Test, Back up now, the status.
4. **Settings → Trash and versions**: the user's rules and the space
   they take; the admin's defaults on the same form (one component).
5. **From a backup…**: a dialog from a folder's menu: a snapshot picker
   and the folder's contents then, each with Restore.

## 6. Order of work

1. The trash's retention and Empty trash; the daily job.
2. Versions: the write path, the table, the endpoints, the thinning.
3. Backups: settings, the job, forget/prune/check, status.
4. Restoring: one file or folder; then the CLI for everything.
5. The UI (mockups first).

Each step committed and tested on its own. Backups are tested against a
local repository in a temp dir (S3 only by hand, against MinIO).

## 7. Decisions (2026-11-07)

1. **rustic_core 0.13.0 and rustic_backend 0.7.0**: added (§4).
2. **Restoring from backups**: each user their own files, next to the
   current ones; the admin everything, from the command line.
3. **Retention rules**: each user's own, with the admin's defaults (§1.1).
4. **The pages**: approved as mocked up. The frontend is built from
   reusable components (and the missing small ones), with no duplication.
