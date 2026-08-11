-- Exact schema emitted by pigeonpost-loft 0.1.0. The release did not set user_version.
CREATE TABLE events (
    cursor INTEGER PRIMARY KEY AUTOINCREMENT,
    id BLOB NOT NULL UNIQUE,
    recipient BLOB NOT NULL,
    stored_at INTEGER NOT NULL,
    expires_at INTEGER NOT NULL,
    size INTEGER NOT NULL,
    blob BLOB NOT NULL
);
CREATE INDEX events_by_recipient ON events (recipient, cursor);
CREATE INDEX events_by_expiry ON events (expires_at);
CREATE TABLE recipient_policy (
    pubkey BLOB PRIMARY KEY,
    seq INTEGER NOT NULL,
    policy BLOB NOT NULL
);
CREATE TABLE agent_records (
    address TEXT PRIMARY KEY,
    seq INTEGER NOT NULL,
    record BLOB NOT NULL
);
