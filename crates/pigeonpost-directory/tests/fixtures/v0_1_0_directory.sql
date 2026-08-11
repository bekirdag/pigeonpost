-- Exact SQLite schema emitted by pigeonpost-directory v0.1.0. The release did not set
-- PRAGMA user_version. Tests add release-shaped signed JSON rows after loading this fixture.
CREATE TABLE lofts (
    endpoint      TEXT PRIMARY KEY,
    entry         BLOB NOT NULL,
    state         TEXT NOT NULL,
    first_seen    INTEGER NOT NULL,
    last_probe    INTEGER NOT NULL DEFAULT 0,
    fail_streak   INTEGER NOT NULL DEFAULT 0,
    probes_ok     INTEGER NOT NULL DEFAULT 0,
    probes_total  INTEGER NOT NULL DEFAULT 0,
    degraded_at   INTEGER,
    drain_after   INTEGER
);

CREATE TABLE probes (
    id        INTEGER PRIMARY KEY AUTOINCREMENT,
    endpoint  TEXT NOT NULL,
    at        INTEGER NOT NULL,
    result    BLOB NOT NULL
);
CREATE INDEX probes_by_endpoint ON probes (endpoint, at);
