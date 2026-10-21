//! Organization tables for the enterprise beta (SDS §6, §7, §9, §10; client contract
//! `docs/orgs.md`).
//!
//! The business rules live in `crate::org` and its route modules; every mutation runs inside one
//! IMMEDIATE transaction through [`Store::group_tx`](super::Store::group_tx), so seat counts,
//! handle reservations and authority checks are decided against the rows the transaction then
//! changes (SDS §6.3 "reserve and consume capacity in one serialized transaction").
//!
//! Additive only: an older binary ignores every table here.

use rusqlite::Connection;

pub(super) const SCHEMA: &str = "
CREATE TABLE IF NOT EXISTS org_members (
    id                 TEXT PRIMARY KEY,
    org_id             TEXT NOT NULL REFERENCES orgs(id),
    account_id         TEXT,
    identity           TEXT,
    handle             TEXT,
    first_name         TEXT NOT NULL,
    last_name          TEXT NOT NULL,
    contact_hash       TEXT,
    contact_sealed     BLOB,
    contact_masked     TEXT,
    status             TEXT NOT NULL CHECK (status IN ('invited','active','suspended','removed')),
    authority_revision INTEGER NOT NULL DEFAULT 1,
    invited_by         TEXT,
    invited_at         INTEGER,
    joined_at          INTEGER,
    last_active_at     INTEGER,
    removed_at         INTEGER,
    UNIQUE (org_id, id)
);
CREATE UNIQUE INDEX IF NOT EXISTS org_member_account ON org_members(org_id, account_id)
    WHERE account_id IS NOT NULL AND status <> 'removed';
CREATE UNIQUE INDEX IF NOT EXISTS org_member_identity ON org_members(identity)
    WHERE identity IS NOT NULL AND status <> 'removed';
CREATE UNIQUE INDEX IF NOT EXISTS org_member_handle ON org_members(org_id, handle)
    WHERE status <> 'removed';
CREATE INDEX IF NOT EXISTS org_members_by_account ON org_members(account_id, status);

CREATE TABLE IF NOT EXISTS org_roles (
    org_id     TEXT NOT NULL,
    member_id  TEXT NOT NULL,
    role       TEXT NOT NULL CHECK (role IN ('owner','admin','people_admin','agent_admin','billing_admin')),
    granted_by TEXT,
    granted_at INTEGER,
    PRIMARY KEY (org_id, member_id, role),
    FOREIGN KEY (org_id, member_id) REFERENCES org_members(org_id, id)
);
CREATE UNIQUE INDEX IF NOT EXISTS org_one_owner ON org_roles(org_id) WHERE role = 'owner';

CREATE TABLE IF NOT EXISTS org_agents (
    org_id       TEXT NOT NULL,
    identity     TEXT NOT NULL,
    handle       TEXT NOT NULL,
    label        TEXT,
    created_by   TEXT,
    created_at   INTEGER NOT NULL,
    last_seen_at INTEGER,
    status       TEXT NOT NULL DEFAULT 'active' CHECK (status IN ('active','suspended','deleted')),
    deleted_at   INTEGER,
    PRIMARY KEY (org_id, identity)
);
CREATE UNIQUE INDEX IF NOT EXISTS org_agent_identity ON org_agents(identity);
CREATE UNIQUE INDEX IF NOT EXISTS org_agent_handle ON org_agents(org_id, handle) WHERE status <> 'deleted';

-- An address kept for a new person after its previous holder left (SDS §6.7 option 1).
CREATE TABLE IF NOT EXISTS org_handle_holds (
    org_id     TEXT NOT NULL,
    handle     TEXT NOT NULL,
    reason     TEXT NOT NULL,
    created_at INTEGER NOT NULL,
    PRIMARY KEY (org_id, handle)
);

CREATE TABLE IF NOT EXISTS org_delegations (
    id                TEXT PRIMARY KEY,
    org_id            TEXT NOT NULL,
    grantee_member_id TEXT NOT NULL,
    target_identity   TEXT NOT NULL,
    capabilities      TEXT NOT NULL,
    granted_by        TEXT,
    granted_at        INTEGER NOT NULL,
    expires_at        INTEGER,
    revoked_at        INTEGER,
    revision          INTEGER NOT NULL DEFAULT 1,
    FOREIGN KEY (org_id, grantee_member_id) REFERENCES org_members(org_id, id),
    FOREIGN KEY (org_id, target_identity) REFERENCES org_agents(org_id, identity)
);
CREATE UNIQUE INDEX IF NOT EXISTS org_delegation_live
    ON org_delegations(org_id, grantee_member_id, target_identity) WHERE revoked_at IS NULL;
CREATE INDEX IF NOT EXISTS org_delegations_target ON org_delegations(target_identity, revoked_at);

CREATE TABLE IF NOT EXISTS org_invites (
    id             TEXT PRIMARY KEY,
    org_id         TEXT NOT NULL,
    member_id      TEXT NOT NULL,
    handle         TEXT NOT NULL,
    token_hash     BLOB NOT NULL UNIQUE,
    contact_hash   TEXT NOT NULL,
    roles          TEXT NOT NULL DEFAULT '[]',
    created_by     TEXT NOT NULL,
    created_at     INTEGER NOT NULL,
    expires_at     INTEGER NOT NULL,
    used_at        INTEGER,
    used_by        TEXT,
    revoked_at     INTEGER,
    attempts       INTEGER NOT NULL DEFAULT 0
);
CREATE INDEX IF NOT EXISTS org_invites_member ON org_invites(member_id, created_at);

-- Durable invitation delivery (SDS §7.3): one row per message to send, retried by the worker.
CREATE TABLE IF NOT EXISTS org_invite_deliveries (
    id          TEXT PRIMARY KEY,
    invite_id   TEXT NOT NULL,
    org_id      TEXT NOT NULL,
    link_sealed BLOB NOT NULL,
    state       TEXT NOT NULL CHECK (state IN ('pending','sent','failed','cancelled')),
    attempts    INTEGER NOT NULL DEFAULT 0,
    next_at     INTEGER NOT NULL,
    last_error  TEXT,
    created_at  INTEGER NOT NULL,
    sent_at     INTEGER
);
CREATE INDEX IF NOT EXISTS org_invite_deliveries_due ON org_invite_deliveries(state, next_at);

CREATE TABLE IF NOT EXISTS org_domains (
    org_id          TEXT NOT NULL,
    domain          TEXT NOT NULL,
    token           TEXT NOT NULL,
    verified_at     INTEGER,
    last_checked_at INTEGER,
    failures        INTEGER NOT NULL DEFAULT 0,
    created_at      INTEGER NOT NULL,
    PRIMARY KEY (org_id, domain)
);

CREATE TABLE IF NOT EXISTS org_rules (
    id          TEXT PRIMARY KEY,
    org_id      TEXT NOT NULL,
    owners      TEXT NOT NULL,
    peers       TEXT NOT NULL,
    admission   TEXT CHECK (admission IN ('allow','block')),
    autonomy    TEXT CHECK (autonomy IN ('review','auto')),
    verbs       TEXT,
    known       INTEGER NOT NULL DEFAULT 0,
    locked      INTEGER NOT NULL DEFAULT 0,
    priority    INTEGER NOT NULL DEFAULT 100,
    description TEXT,
    created_by  TEXT,
    created_at  INTEGER NOT NULL,
    updated_at  INTEGER NOT NULL
);
CREATE INDEX IF NOT EXISTS org_rules_by_org ON org_rules(org_id);

CREATE TABLE IF NOT EXISTS device_grants (
    id                 TEXT PRIMARY KEY,
    org_id             TEXT NOT NULL,
    approver_member_id TEXT NOT NULL,
    identity           TEXT NOT NULL,
    capabilities       TEXT NOT NULL,
    authority_kind     TEXT NOT NULL CHECK (authority_kind IN ('role','delegation')),
    authority_role     TEXT,
    delegation_id      TEXT REFERENCES org_delegations(id),
    authority_revision INTEGER NOT NULL,
    created_at         INTEGER NOT NULL,
    expires_at         INTEGER NOT NULL,
    revoked_at         INTEGER,
    FOREIGN KEY (org_id, approver_member_id) REFERENCES org_members(org_id, id),
    FOREIGN KEY (org_id, identity) REFERENCES org_agents(org_id, identity)
);

CREATE TABLE IF NOT EXISTS device_pairings (
    id                TEXT PRIMARY KEY,
    user_code         TEXT NOT NULL UNIQUE,
    claim_challenge   BLOB NOT NULL,
    device_public_key BLOB NOT NULL,
    requested_handle  TEXT NOT NULL,
    target_identity   TEXT,
    org_id            TEXT,
    capabilities      TEXT NOT NULL,
    label             TEXT,
    ip_country        TEXT,
    created_at        INTEGER NOT NULL,
    expires_at        INTEGER NOT NULL,
    approved_by       TEXT,
    approved_grant_id TEXT REFERENCES device_grants(id),
    approved_at       INTEGER,
    denied_at         INTEGER,
    claimed_at        INTEGER,
    credential_id     TEXT,
    revoked_at        INTEGER,
    last_poll_at      INTEGER
);

CREATE TABLE IF NOT EXISTS enrollments (
    id               TEXT PRIMARY KEY,
    code_hash        BLOB NOT NULL UNIQUE,
    org_id           TEXT,
    identity         TEXT NOT NULL,
    capabilities     TEXT NOT NULL,
    grant_id         TEXT NOT NULL REFERENCES device_grants(id),
    created_by       TEXT NOT NULL,
    created_at       INTEGER NOT NULL,
    expires_at       INTEGER NOT NULL,
    redeemed_at      INTEGER,
    redeemed_label   TEXT,
    redeemed_ip_hash TEXT,
    revoked_at       INTEGER
);

CREATE TABLE IF NOT EXISTS device_credentials (
    id                TEXT PRIMARY KEY,
    org_id            TEXT,
    identity          TEXT NOT NULL,
    device_public_key BLOB NOT NULL,
    token_hash        BLOB NOT NULL UNIQUE,
    capabilities      TEXT NOT NULL,
    parent_grant_id   TEXT NOT NULL REFERENCES device_grants(id),
    label             TEXT,
    source            TEXT NOT NULL CHECK (source IN ('pairing','enrollment')),
    created_at        INTEGER NOT NULL,
    expires_at        INTEGER NOT NULL,
    last_used_at      INTEGER,
    revoked_at        INTEGER
);
CREATE INDEX IF NOT EXISTS device_credentials_identity ON device_credentials(identity, revoked_at);
";

/// Columns added to existing tables. Each is applied once; "duplicate column" means it is there.
const ALTERS: &[&str] = &[
    "ALTER TABLE orgs ADD COLUMN revision INTEGER NOT NULL DEFAULT 1",
    "ALTER TABLE orgs ADD COLUMN rules_revision INTEGER NOT NULL DEFAULT 1",
    "ALTER TABLE orgs ADD COLUMN status_since INTEGER",
];

pub(super) fn migrate(c: &Connection) -> rusqlite::Result<()> {
    for sql in ALTERS {
        match c.execute(sql, []) {
            Ok(_) => {}
            Err(rusqlite::Error::SqliteFailure(_, Some(msg)))
                if msg.contains("duplicate column") => {}
            Err(e) => return Err(e),
        }
    }
    c.execute_batch(SCHEMA)
}
