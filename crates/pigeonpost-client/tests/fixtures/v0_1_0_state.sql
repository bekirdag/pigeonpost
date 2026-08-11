-- Exact schema emitted by pigeonpost-client 0.1.0. The release did not set user_version.
CREATE TABLE meta (
    key TEXT PRIMARY KEY,
    value TEXT NOT NULL
);
CREATE TABLE lofts (
    url TEXT PRIMARY KEY,
    pubkey BLOB,
    added_at INTEGER NOT NULL,
    state TEXT NOT NULL DEFAULT 'active'
);
CREATE TABLE outbox (
    row INTEGER PRIMARY KEY AUTOINCREMENT,
    message_id TEXT NOT NULL,
    to_addr TEXT NOT NULL,
    loft_url TEXT NOT NULL,
    wrap BLOB NOT NULL,
    attempts INTEGER NOT NULL DEFAULT 0,
    last_error TEXT,
    created_at INTEGER NOT NULL,
    sent_at INTEGER,
    UNIQUE (message_id, loft_url)
);
CREATE INDEX outbox_pending ON outbox (sent_at);
CREATE TABLE cursors (
    loft_url TEXT PRIMARY KEY,
    cursor INTEGER NOT NULL
);
CREATE TABLE messages (
    id TEXT PRIMARY KEY,
    from_pubkey BLOB NOT NULL,
    from_address TEXT NOT NULL,
    received_at INTEGER NOT NULL,
    read INTEGER NOT NULL DEFAULT 0,
    body TEXT NOT NULL,
    state TEXT NOT NULL DEFAULT 'accepted'
);
CREATE INDEX messages_unread ON messages (read, received_at);
CREATE TABLE resolutions (
    addr TEXT PRIMARY KEY,
    pubkey BLOB NOT NULL,
    successor_hash BLOB NOT NULL,
    seq INTEGER NOT NULL,
    lofts TEXT NOT NULL,
    fetched_at INTEGER NOT NULL,
    pow_min INTEGER NOT NULL DEFAULT 0
);
CREATE TABLE allowlist (
    pubkey BLOB PRIMARY KEY,
    added_at INTEGER NOT NULL,
    reason TEXT
);
CREATE TABLE scores (
    pubkey BLOB PRIMARY KEY,
    score INTEGER NOT NULL,
    updated_at INTEGER NOT NULL
);
