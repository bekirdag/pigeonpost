-- Exact SQLite schema emitted by pigeonpost-registry v0.1.0, plus two deterministic released
-- handle leaves. The release did not set PRAGMA user_version, so it intentionally remains zero.
CREATE TABLE entries (
    idx        INTEGER PRIMARY KEY,
    kind       TEXT NOT NULL,
    handle     TEXT NOT NULL,
    pubkey     TEXT NOT NULL,
    subject    TEXT NOT NULL,
    timestamp  INTEGER NOT NULL
);
CREATE INDEX entries_by_handle ON entries (handle, idx);

INSERT INTO entries (idx, kind, handle, pubkey, subject, timestamp) VALUES
    (0, 'handle_bind', '/gh/alice',
     '1111111111111111111111111111111111111111111111111111111111111111',
     'gh:alice', 1786105721),
    (1, 'handle_rotate', '/gh/alice',
     '2222222222222222222222222222222222222222222222222222222222222222',
     'gh:alice', 1786105722);
