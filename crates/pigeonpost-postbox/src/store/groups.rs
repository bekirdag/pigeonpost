//! Persistent end-to-end-encrypted groups (SDS §4.3, `docs/group-e2ee.md` §6–§7).
//!
//! The postbox stores **ciphertext and routing metadata only**: group metadata, membership,
//! MLS device credentials and key packages, encrypted commits and Welcomes, and one encrypted copy
//! of each application message. It never holds group secrets, device private keys or attachment
//! keys.
//!
//! The business rules live in `crate::groups` and `crate::mls`; they run inside one SQLite
//! transaction each through [`Store::group_tx`], so every permission check is re-made inside the
//! transaction that commits the change (SDS §3.3 step 6).
//!
//! # One sequence per group
//!
//! Application messages, commits and server notices share `groups.last_seq`, allocated inside the
//! writing transaction. Clients process items strictly in that order.

use super::{events, Store, StoreError};
use rusqlite::{Connection, Transaction};

pub(super) const SCHEMA: &str = "
CREATE TABLE IF NOT EXISTS groups (
    id                TEXT PRIMARY KEY,
    org_id            TEXT,
    name              TEXT NOT NULL CHECK (length(name) BETWEEN 1 AND 80),
    description       TEXT CHECK (description IS NULL OR length(description) <= 500),
    avatar_id         TEXT,
    handle            TEXT UNIQUE,
    join_policy       TEXT NOT NULL DEFAULT 'private' CHECK (join_policy IN ('public','private')),
    post_policy       TEXT NOT NULL DEFAULT 'everyone' CHECK (post_policy IN ('everyone','admins')),
    visibility        TEXT NOT NULL DEFAULT 'unlisted' CHECK (visibility IN ('listed','unlisted')),
    members_can_invite INTEGER NOT NULL DEFAULT 0,
    auto_join         INTEGER NOT NULL DEFAULT 0,
    history_visible_to_new_members INTEGER NOT NULL DEFAULT 0,
    members_can_change_timer INTEGER NOT NULL DEFAULT 0,
    stream_policy     TEXT NOT NULL DEFAULT 'admins' CHECK (stream_policy IN ('admins','everyone')),
    revision          INTEGER NOT NULL DEFAULT 1,
    activation_state  TEXT NOT NULL DEFAULT 'provisioning' CHECK (activation_state IN ('provisioning','active')),
    link_token_hash   BLOB,
    ephemeral_secs    INTEGER,
    max_members       INTEGER NOT NULL DEFAULT 1000,
    epoch             INTEGER NOT NULL DEFAULT 0,
    last_seq          INTEGER NOT NULL DEFAULT 0,
    pruned_through_seq INTEGER NOT NULL DEFAULT 0,
    created_by        TEXT NOT NULL,
    created_by_account TEXT,
    created_at        INTEGER NOT NULL,
    last_at           INTEGER NOT NULL,
    deleted_at        INTEGER
);
CREATE INDEX IF NOT EXISTS groups_org ON groups(org_id, visibility) WHERE deleted_at IS NULL;
CREATE INDEX IF NOT EXISTS groups_listed ON groups(visibility, join_policy) WHERE deleted_at IS NULL;
CREATE INDEX IF NOT EXISTS groups_creator ON groups(created_by_account, created_at);

CREATE TABLE IF NOT EXISTS group_members (
    group_id    TEXT NOT NULL REFERENCES groups(id),
    member      TEXT NOT NULL,
    role        TEXT NOT NULL CHECK (role IN ('owner','admin','member','restricted')),
    state       TEXT NOT NULL CHECK (state IN ('active','invited','keying','left','removed','banned')),
    invited_by  TEXT,
    invited_at  INTEGER,
    filtered    INTEGER NOT NULL DEFAULT 0,
    joined_at   INTEGER,
    join_seq    INTEGER,
    membership_version INTEGER NOT NULL DEFAULT 0,
    revision    INTEGER NOT NULL DEFAULT 1,
    read_cursor INTEGER NOT NULL DEFAULT 0,
    muted_until INTEGER,
    notify      TEXT NOT NULL DEFAULT 'all' CHECK (notify IN ('all','mentions','none')),
    updated_at  INTEGER NOT NULL,
    PRIMARY KEY (group_id, member)
);
CREATE INDEX IF NOT EXISTS group_members_member ON group_members(member, state);
CREATE UNIQUE INDEX IF NOT EXISTS group_one_owner ON group_members(group_id)
    WHERE role = 'owner' AND state IN ('active','keying');

CREATE TABLE IF NOT EXISTS group_messages (
    group_id    TEXT NOT NULL REFERENCES groups(id),
    seq         INTEGER NOT NULL,
    id          TEXT NOT NULL UNIQUE,
    client_message_id TEXT NOT NULL,
    sender      TEXT NOT NULL,
    sender_device TEXT NOT NULL,
    kind        TEXT NOT NULL CHECK (kind IN ('text')),
    body_blob   BLOB,
    bytes       INTEGER NOT NULL DEFAULT 0,
    attachments TEXT,
    mentions    TEXT,
    reply_to    INTEGER,
    edit_of     INTEGER,
    replaced_by INTEGER,
    body_hash   TEXT NOT NULL,
    created_at  INTEGER NOT NULL,
    expires_at  INTEGER,
    edited_at   INTEGER,
    revision    INTEGER NOT NULL DEFAULT 1,
    deleted_at  INTEGER,
    epoch       INTEGER NOT NULL,
    enc_scheme  TEXT NOT NULL,
    PRIMARY KEY (group_id, seq),
    UNIQUE (group_id, sender, client_message_id)
);
CREATE INDEX IF NOT EXISTS group_messages_expiry ON group_messages(expires_at) WHERE expires_at IS NOT NULL;

CREATE TABLE IF NOT EXISTS group_notices (
    group_id   TEXT NOT NULL REFERENCES groups(id),
    seq        INTEGER NOT NULL,
    event_id   TEXT NOT NULL UNIQUE,
    kind       TEXT NOT NULL,
    actor_identity TEXT,
    metadata   TEXT NOT NULL,
    created_at INTEGER NOT NULL,
    expires_at INTEGER,
    PRIMARY KEY (group_id, seq)
);

CREATE TABLE IF NOT EXISTS group_handshakes (
    group_id   TEXT NOT NULL REFERENCES groups(id),
    seq        INTEGER NOT NULL,
    epoch      INTEGER NOT NULL,
    commit_id  TEXT NOT NULL,
    sender     TEXT NOT NULL,
    sender_device TEXT NOT NULL,
    commit_blob BLOB NOT NULL,
    welcome_blob BLOB,
    adds       TEXT NOT NULL,
    removes    TEXT NOT NULL,
    created_at INTEGER NOT NULL,
    PRIMARY KEY (group_id, seq),
    UNIQUE (group_id, epoch)
);

CREATE TABLE IF NOT EXISTS group_bans (
    group_id TEXT NOT NULL, member TEXT NOT NULL, by_member TEXT, reason TEXT, at INTEGER NOT NULL,
    PRIMARY KEY (group_id, member)
);

CREATE TABLE IF NOT EXISTS group_membership_intervals (
    group_id TEXT NOT NULL REFERENCES groups(id),
    member TEXT NOT NULL,
    membership_version INTEGER NOT NULL,
    first_seq INTEGER NOT NULL,
    last_seq INTEGER,
    join_epoch INTEGER NOT NULL,
    leave_epoch INTEGER,
    PRIMARY KEY (group_id, member, membership_version)
);

-- Scoped idempotency records (SDS §4.3): one per (principal mailbox, group, action, key).
CREATE TABLE IF NOT EXISTS group_idempotency (
    scope      TEXT NOT NULL,
    group_id   TEXT NOT NULL,
    action     TEXT NOT NULL,
    request_key TEXT NOT NULL,
    body_hash  TEXT NOT NULL,
    status     INTEGER NOT NULL,
    response   TEXT NOT NULL,
    created_at INTEGER NOT NULL,
    PRIMARY KEY (scope, group_id, action, request_key)
);
CREATE INDEX IF NOT EXISTS group_idempotency_age ON group_idempotency(created_at);

-- Sealed attachment blobs (content-addressed in the `groups` blob sub-store). `message_seq` is the
-- claim; an unclaimed upload is visible to nobody and swept after a day.
CREATE TABLE IF NOT EXISTS group_attachments (
    id          TEXT PRIMARY KEY,
    group_id    TEXT NOT NULL,
    sha256      TEXT NOT NULL,
    bytes       INTEGER NOT NULL,
    uploader    TEXT NOT NULL,
    message_seq INTEGER,
    created_at  INTEGER NOT NULL,
    expires_at  INTEGER
);
CREATE INDEX IF NOT EXISTS group_attachments_group ON group_attachments(group_id, message_seq);
CREATE INDEX IF NOT EXISTS group_attachments_sha ON group_attachments(sha256);

-- MLS device credentials (P-2 §6). The server holds public keys only.
CREATE TABLE IF NOT EXISTS mls_devices (
    mailbox       TEXT NOT NULL,
    device_id     TEXT NOT NULL,
    signature_key BLOB NOT NULL,
    fingerprint   BLOB NOT NULL,
    attestation   BLOB NOT NULL,
    created_at    INTEGER NOT NULL,
    revoked_at    INTEGER,
    PRIMARY KEY (mailbox, device_id)
);

CREATE TABLE IF NOT EXISTS mls_key_packages (
    ref          TEXT PRIMARY KEY,
    mailbox      TEXT NOT NULL,
    device_id    TEXT NOT NULL,
    bytes        BLOB NOT NULL,
    last_resort  INTEGER NOT NULL DEFAULT 0,
    not_after    INTEGER NOT NULL,
    created_at   INTEGER NOT NULL,
    claimed_at   INTEGER
);
CREATE INDEX IF NOT EXISTS mls_key_packages_device ON mls_key_packages(mailbox, device_id, claimed_at);

CREATE TABLE IF NOT EXISTS mls_kp_claims (
    id            INTEGER PRIMARY KEY AUTOINCREMENT,
    ref           TEXT NOT NULL,
    group_id      TEXT NOT NULL,
    mailbox       TEXT NOT NULL,
    device_id     TEXT NOT NULL,
    claimed_by_mailbox TEXT NOT NULL,
    claimed_by_device  TEXT NOT NULL,
    claimed_at    INTEGER NOT NULL,
    used_at       INTEGER
);
CREATE INDEX IF NOT EXISTS mls_kp_claims_ref ON mls_kp_claims(group_id, ref);

-- The server's view of each group's MLS leaves, from the declared adds/removes of accepted commits.
CREATE TABLE IF NOT EXISTS group_leaves (
    group_id    TEXT NOT NULL,
    mailbox     TEXT NOT NULL,
    device_id   TEXT NOT NULL,
    added_epoch INTEGER NOT NULL,
    added_seq   INTEGER NOT NULL,
    stale       INTEGER NOT NULL DEFAULT 0,
    acked_epoch INTEGER NOT NULL DEFAULT 0,
    acked_at    INTEGER,
    PRIMARY KEY (group_id, mailbox, device_id)
);
CREATE INDEX IF NOT EXISTS group_leaves_device ON group_leaves(mailbox, device_id);

CREATE TABLE IF NOT EXISTS mls_welcomes (
    id          TEXT PRIMARY KEY,
    group_id    TEXT NOT NULL,
    seq         INTEGER NOT NULL,
    epoch       INTEGER NOT NULL,
    key_package_ref TEXT NOT NULL,
    mailbox     TEXT NOT NULL,
    device_id   TEXT NOT NULL,
    created_at  INTEGER NOT NULL,
    acked_at    INTEGER,
    joined      INTEGER
);
CREATE INDEX IF NOT EXISTS mls_welcomes_device ON mls_welcomes(mailbox, device_id, acked_at);
";

impl Store {
    /// Run `f` inside one IMMEDIATE transaction, committing when it returns `Ok`.
    ///
    /// `E` lets the groups layer carry its own refusals out of the transaction (rolling it back)
    /// without stringly-typed store errors.
    pub(crate) async fn group_tx<T, E, F>(&self, f: F) -> Result<T, E>
    where
        T: Send + 'static,
        E: From<rusqlite::Error> + From<StoreError> + Send + 'static,
        F: FnOnce(&Transaction<'_>) -> Result<T, E> + Send + 'static,
    {
        let conn = self.conn.clone();
        tokio::task::spawn_blocking(move || -> Result<T, E> {
            let mut c = conn.lock().expect("store lock");
            let tx = c.transaction_with_behavior(rusqlite::TransactionBehavior::Immediate)?;
            let out = f(&tx)?;
            tx.commit()?;
            Ok(out)
        })
        .await
        .map_err(|_| E::from(StoreError::Join))?
    }

    /// Run a read-only `f` against the connection.
    pub(crate) async fn group_read<T, E, F>(&self, f: F) -> Result<T, E>
    where
        T: Send + 'static,
        E: From<rusqlite::Error> + From<StoreError> + Send + 'static,
        F: FnOnce(&Connection) -> Result<T, E> + Send + 'static,
    {
        let conn = self.conn.clone();
        tokio::task::spawn_blocking(move || -> Result<T, E> {
            let c = conn.lock().expect("store lock");
            f(&c)
        })
        .await
        .map_err(|_| E::from(StoreError::Join))?
    }
}

/// Append a typed account event inside the caller's transaction (see `store::events`).
pub(crate) fn append_event(
    c: &Connection,
    account: &str,
    kind: &str,
    mailbox: Option<&str>,
    data: &serde_json::Value,
    now: u64,
) -> Result<(), StoreError> {
    events::append_in(c, account, kind, mailbox, data, now).map(|_| ())
}
