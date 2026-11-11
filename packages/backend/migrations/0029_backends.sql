-- Storage backends (docs/M7C_DESIGN.md §1–§3): configured storage that
-- holds drives. The server's disk is the built-in one and has no row (a
-- drive whose `backend_id` is NULL is on it); the rows are S3-compatible
-- backends, the superadmin's (global, `owner_id` NULL) or a user's own.
CREATE TABLE backends (
  id BLOB PRIMARY KEY NOT NULL CHECK (length(id) = 16),
  -- NULL: a global backend; else the user whose own storage it is.
  owner_id BLOB REFERENCES users(id) ON DELETE CASCADE,
  kind TEXT NOT NULL CHECK (kind IN ('s3')),
  name TEXT NOT NULL,
  -- The S3 endpoint (https://…), its region, the bucket and a prefix in
  -- it (each drive is at `<prefix>/<drive id>/`).
  endpoint TEXT NOT NULL,
  region TEXT NOT NULL DEFAULT '',
  bucket TEXT NOT NULL,
  prefix TEXT NOT NULL DEFAULT '',
  access_key TEXT NOT NULL,
  -- The secret encrypted with the server key (a nonce, then the
  -- ciphertext and its tag, AES-256-GCM); NULL with private credentials.
  secret BLOB,
  -- Private credentials (users' own backends only): the secret sealed to
  -- the owner's encryption key by their device, which the server cannot
  -- open. A session that unlocks it hands the secret over, in memory only.
  sealed_secret TEXT,
  -- Bytes for all its drives together; NULL: no limit.
  storage_limit INTEGER,
  -- The limit each new drive gets; NULL: none.
  drive_limit INTEGER,
  -- Who may make drives on a global backend: the superadmin alone, the
  -- teams' managers (their team's drives), everyone, or the managers of
  -- the teams in `backend_teams`. A user's own: its owner only.
  make_drives TEXT NOT NULL DEFAULT 'superadmin'
    CHECK (make_drives IN ('superadmin', 'team_managers', 'everyone', 'teams')),
  -- How often each drive's prefix is listed for changes made outside
  -- Organizer, in seconds; NULL: never.
  list_every INTEGER DEFAULT 3600 CHECK (list_every IS NULL OR list_every >= 60),
  created_at TEXT NOT NULL DEFAULT (strftime('%Y-%m-%dT%H:%M:%fZ', 'now')),
  CHECK ((secret IS NULL) != (sealed_secret IS NULL)),
  CHECK (sealed_secret IS NULL OR owner_id IS NOT NULL)
);
CREATE UNIQUE INDEX backends_global_name ON backends (name COLLATE NOCASE) WHERE owner_id IS NULL;
CREATE UNIQUE INDEX backends_own_name ON backends (owner_id, name COLLATE NOCASE)
  WHERE owner_id IS NOT NULL;

-- The teams whose managers may make drives on a backend (`make_drives`
-- 'teams').
CREATE TABLE backend_teams (
  backend_id BLOB NOT NULL REFERENCES backends(id) ON DELETE CASCADE,
  team_id BLOB NOT NULL REFERENCES teams(id) ON DELETE CASCADE,
  PRIMARY KEY (backend_id, team_id)
);

-- The backend a drive is in; NULL: the server's disk. A backend with
-- drives is not removed (its drives are moved or deleted first).
ALTER TABLE drives ADD COLUMN backend_id BLOB REFERENCES backends(id);
CREATE INDEX drives_backend ON drives (backend_id);

-- A drive being moved to another backend (§2): copied, then switched over;
-- read-only meanwhile. The row is the job: one left at a stop is carried on
-- at the next start. A backend's NULL is the server's disk.
CREATE TABLE drive_moves (
  drive_id BLOB PRIMARY KEY NOT NULL REFERENCES drives(id) ON DELETE CASCADE,
  from_backend BLOB,
  to_backend BLOB,
  started_at TEXT NOT NULL DEFAULT (strftime('%Y-%m-%dT%H:%M:%fZ', 'now')),
  -- Why the last try failed (fit to show); it is tried again.
  error TEXT
);

-- A move or rename on storage without one (S3) copies every object and
-- deletes the old one: a crash part-way is finished at the next start, and
-- the node then goes where it was going.
ALTER TABLE journal ADD COLUMN target_parent BLOB;
ALTER TABLE journal ADD COLUMN target_name TEXT;
