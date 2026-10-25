-- Rebuildable cache. Everything here can be recomputed from the files on disk.
CREATE TABLE IF NOT EXISTS roots (
    id           INTEGER PRIMARY KEY,
    path         TEXT NOT NULL UNIQUE,
    is_reference INTEGER NOT NULL DEFAULT 0
);

CREATE TABLE IF NOT EXISTS files (
    id               INTEGER PRIMARY KEY,
    root_id          INTEGER NOT NULL REFERENCES roots(id),
    rel_path         TEXT NOT NULL,
    ext              TEXT NOT NULL,
    size             INTEGER NOT NULL,
    mtime            INTEGER NOT NULL,
    -- Hashes are filled by duplicate analysis; browsing only needs metadata.
    blake3           BLOB,
    image_data_hash  BLOB,
    width            INTEGER,
    height           INTEGER,
    analyzer_version INTEGER,
    meta_version     INTEGER,
    analyze_error    TEXT,
    luma_stddev      REAL,
    changed_at       INTEGER,
    change_source    TEXT,
    seen_at          INTEGER,
    UNIQUE (root_id, rel_path)
);

CREATE INDEX IF NOT EXISTS files_blake3 ON files(blake3);

CREATE TABLE IF NOT EXISTS exif (
    file_id       INTEGER PRIMARY KEY REFERENCES files(id) ON DELETE CASCADE,
    capture_time  TEXT,
    subsec        TEXT,
    offset        TEXT,
    make          TEXT,
    model         TEXT,
    serial        TEXT,
    lens          TEXT,
    focal         REAL,
    fnumber       REAL,
    exposure      TEXT,
    iso           INTEGER,
    width         INTEGER,
    height        INTEGER,
    orientation   INTEGER,
    shutter_count INTEGER,
    gps_lat       REAL,
    gps_lon       REAL,
    -- Nikon's NEFCompression tag value (13, 14: High efficiency).
    nef_compression INTEGER,
    -- A movie's length in seconds.
    duration      REAL
);

-- Effective culling marks (sidecar > embedded XMP > EXIF), and a signature
-- of the sidecars they were read from, so a changed sidecar is re-read.
CREATE TABLE IF NOT EXISTS marks (
    file_id     INTEGER PRIMARY KEY REFERENCES files(id) ON DELETE CASCADE,
    rating      INTEGER NOT NULL,
    pick        TEXT,
    label       TEXT,
    keywords    TEXT NOT NULL,
    event       TEXT,
    gps_lat_e7  INTEGER,
    gps_lon_e7  INTEGER,
    sidecar     TEXT,
    sidecar_sig TEXT NOT NULL,
    -- The sidecars' date (XMP's form), shown in place of the file's own.
    date        TEXT
);

-- One row per rotation (0, 90, 180, 270) of the upright image.
CREATE TABLE IF NOT EXISTS phash (
    file_id  INTEGER NOT NULL REFERENCES files(id) ON DELETE CASCADE,
    rotation INTEGER NOT NULL,
    dhash    INTEGER NOT NULL,
    phash    INTEGER NOT NULL,
    PRIMARY KEY (file_id, rotation)
);

CREATE TABLE IF NOT EXISTS dupe_ignores (
    hash_a BLOB NOT NULL,
    hash_b BLOB NOT NULL,
    PRIMARY KEY (hash_a, hash_b)
);

-- Working copies of photo edits. The durable copy is the root's
-- `.shoebox/edits/<rel>.json`, written a moment later (`saved_at`); a newer
-- file wins over this row. Keyed by paths, not file ids, and not in the
-- rebuildable set: a cache rebuild must not lose an unsaved edit.
CREATE TABLE IF NOT EXISTS edits (
    root       TEXT NOT NULL,
    rel_path   TEXT NOT NULL,
    copy_id    TEXT NOT NULL,
    doc_json   TEXT NOT NULL,
    doc_hash   TEXT NOT NULL,
    updated_at INTEGER NOT NULL,
    saved_at   INTEGER,
    PRIMARY KEY (root, rel_path, copy_id)
);

-- Files exported from photos (Develop's export), with the preset used and
-- the edit they were made from (to tell a stale export). Not rebuildable.
CREATE TABLE IF NOT EXISTS exports (
    photo       TEXT NOT NULL,
    copy_id     TEXT NOT NULL,
    path        TEXT NOT NULL,
    preset_name TEXT NOT NULL,
    preset_json TEXT NOT NULL,
    edit_key    TEXT,
    created_at  INTEGER NOT NULL
);
CREATE INDEX IF NOT EXISTS exports_photo ON exports(photo);
