//! Nests Phase A (docs/nests/phase-a-plan.md): the postbox's record of each hosted agent and its
//! one-time enrollment codes. The worker host (nestd) keeps the runtime, run and usage ledger; the
//! postbox stays the authority for who owns a Nest and which mailbox it is.

pub(super) const SCHEMA: &str = "
CREATE TABLE IF NOT EXISTS nests (
    id         TEXT PRIMARY KEY,
    account_id TEXT NOT NULL,
    -- NULL only while the row reserves the slot and the mailbox is being minted.
    address    TEXT UNIQUE,
    label      TEXT NOT NULL,
    state      TEXT NOT NULL,
    created_at INTEGER NOT NULL,
    updated_at INTEGER NOT NULL,
    deleted_at INTEGER
);
CREATE INDEX IF NOT EXISTS nests_by_account ON nests(account_id, deleted_at);

CREATE TABLE IF NOT EXISTS nest_enrollments (
    id                TEXT PRIMARY KEY,
    nest_id           TEXT NOT NULL REFERENCES nests(id),
    code_hash         BLOB NOT NULL UNIQUE,
    created_at        INTEGER NOT NULL,
    expires_at        INTEGER NOT NULL,
    redeemed_at       INTEGER,
    device_public_key BLOB,
    redeemed_ip_hash  TEXT,
    revoked_at        INTEGER
);
CREATE INDEX IF NOT EXISTS nest_enrollments_by_nest ON nest_enrollments(nest_id);
";
