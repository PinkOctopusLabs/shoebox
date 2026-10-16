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
    shutter_count INTEGER
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
    sidecar     TEXT,
    sidecar_sig TEXT NOT NULL
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
