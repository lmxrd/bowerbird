-- The sync state of one account (docs/M5_DESIGN.md §3).

-- Small values: the change feed's cursor.
CREATE TABLE meta (
    key TEXT PRIMARY KEY,
    value TEXT NOT NULL
);

-- The synced folders: a folder of the server's tree and its local place.
CREATE TABLE folders (
    id INTEGER PRIMARY KEY,
    remote_id TEXT NOT NULL UNIQUE,
    local_path TEXT NOT NULL UNIQUE,
    -- `sync` (full sync); `on_demand` comes with M5b.
    mode TEXT NOT NULL DEFAULT 'sync'
);

-- The server's tree under the synced folders, as the listings and the
-- change feed tell it.
CREATE TABLE remote (
    node_id TEXT PRIMARY KEY,
    parent_id TEXT,
    name TEXT NOT NULL,
    kind TEXT NOT NULL,
    version INTEGER NOT NULL
);
CREATE INDEX remote_parent ON remote (parent_id);

-- What both sides agreed on when they were last in sync: per synced node,
-- its place on each side, the server's version and the local file's hash,
-- size, modification time and inode then.
CREATE TABLE entries (
    folder INTEGER NOT NULL REFERENCES folders (id) ON DELETE CASCADE,
    node_id TEXT NOT NULL,
    -- Local, relative to the folder, `/` between the names.
    path TEXT NOT NULL,
    parent_id TEXT NOT NULL,
    -- The name on the server, which the local one may differ from (§4.6).
    name TEXT NOT NULL,
    kind TEXT NOT NULL,
    version INTEGER NOT NULL,
    hash TEXT,
    size INTEGER,
    mtime_ns INTEGER,
    inode INTEGER,
    PRIMARY KEY (folder, node_id),
    UNIQUE (folder, path)
);

-- Resumable uploads under way, carried on after a break (folder 0: the
-- drop box folder). Only while the file is as it was when it started.
CREATE TABLE uploads (
    folder INTEGER NOT NULL,
    path TEXT NOT NULL,
    url TEXT NOT NULL,
    size INTEGER NOT NULL,
    mtime_ns INTEGER NOT NULL,
    PRIMARY KEY (folder, path)
);
