//! Enterprise API tables (SDS §18; contract `docs/org-api.md`): org API keys, their idempotency
//! records, webhooks, the webhook event log and its durable delivery outbox.
//!
//! Webhook events are recorded by triggers on the org tables, so every path that changes an org —
//! the dashboard, the org API, the billing bridge, the lifecycle worker, offboarding — produces the
//! event in the same transaction as the change, and none can forget to. A trigger fires only for an
//! org that has a live webhook, so orgs without webhooks accumulate nothing. Event payloads are ids
//! and handles only (SDS §18.5: never message content).
//!
//! Additive only: an older binary ignores these tables, and the triggers only ever insert into
//! tables defined here.

use rusqlite::Connection;

pub(super) const SCHEMA: &str = "
CREATE TABLE IF NOT EXISTS org_api_keys (
    id                 TEXT PRIMARY KEY,
    org_id             TEXT NOT NULL,
    name               TEXT NOT NULL,
    key_hash           BLOB NOT NULL UNIQUE,
    last4              TEXT NOT NULL,
    scopes             TEXT NOT NULL,
    send_as            TEXT,
    ip_allowlist       TEXT,
    created_by_member  TEXT NOT NULL,
    created_by_account TEXT NOT NULL,
    created_at         INTEGER NOT NULL,
    expires_at         INTEGER,
    last_used_at       INTEGER,
    revoked_at         INTEGER,
    revoked_by         TEXT
);
CREATE INDEX IF NOT EXISTS org_api_keys_by_org ON org_api_keys(org_id, created_at);

CREATE TABLE IF NOT EXISTS org_api_idempotency (
    key_id       TEXT NOT NULL,
    route        TEXT NOT NULL,
    idem_key     TEXT NOT NULL,
    request_hash TEXT NOT NULL,
    status       INTEGER NOT NULL,
    response     TEXT NOT NULL,
    created_at   INTEGER NOT NULL,
    PRIMARY KEY (key_id, route, idem_key)
);
CREATE INDEX IF NOT EXISTS org_api_idempotency_age ON org_api_idempotency(created_at);

CREATE TABLE IF NOT EXISTS webhooks (
    id                     TEXT PRIMARY KEY,
    org_id                 TEXT NOT NULL,
    url                    TEXT NOT NULL,
    events                 TEXT NOT NULL,
    description            TEXT,
    secret_sealed          BLOB NOT NULL,
    previous_secret_sealed BLOB,
    previous_expires_at    INTEGER,
    created_by             TEXT,
    created_at             INTEGER NOT NULL,
    rotated_at             INTEGER,
    deleted_at             INTEGER
);
CREATE INDEX IF NOT EXISTS webhooks_by_org ON webhooks(org_id) WHERE deleted_at IS NULL;

CREATE TABLE IF NOT EXISTS webhook_events (
    id         TEXT PRIMARY KEY,
    org_id     TEXT NOT NULL,
    type       TEXT NOT NULL,
    data       TEXT NOT NULL,
    created_at INTEGER NOT NULL,
    fanned_out INTEGER NOT NULL DEFAULT 0
);
CREATE INDEX IF NOT EXISTS webhook_events_pending ON webhook_events(fanned_out, created_at);

CREATE TABLE IF NOT EXISTS webhook_outbox (
    id              INTEGER PRIMARY KEY AUTOINCREMENT,
    webhook_id      TEXT NOT NULL,
    event_id        TEXT NOT NULL,
    state           TEXT NOT NULL CHECK (state IN ('pending','delivered','failed','cancelled')),
    attempts        INTEGER NOT NULL DEFAULT 0,
    next_attempt_at INTEGER NOT NULL,
    lease_until     INTEGER NOT NULL DEFAULT 0,
    created_at      INTEGER NOT NULL,
    finished_at     INTEGER,
    UNIQUE (webhook_id, event_id)
);
CREATE INDEX IF NOT EXISTS webhook_outbox_due ON webhook_outbox(state, next_attempt_at);

CREATE TABLE IF NOT EXISTS webhook_deliveries (
    id              TEXT PRIMARY KEY,
    webhook_id      TEXT NOT NULL,
    event_id        TEXT NOT NULL,
    event_type      TEXT NOT NULL,
    attempt         INTEGER NOT NULL,
    status          TEXT NOT NULL CHECK (status IN ('delivered','retrying','failed','blocked')),
    response_status INTEGER,
    error           TEXT,
    duration_ms     INTEGER,
    at              INTEGER NOT NULL
);
CREATE INDEX IF NOT EXISTS webhook_deliveries_by_hook ON webhook_deliveries(webhook_id, at);
";

/// Triggers that record webhook events. Created after the org, group and device tables exist.
const TRIGGERS: &str = "
CREATE TRIGGER IF NOT EXISTS whv1_member_invited AFTER INSERT ON org_members
WHEN NEW.status = 'invited'
 AND EXISTS (SELECT 1 FROM webhooks WHERE org_id = NEW.org_id AND deleted_at IS NULL)
BEGIN
    INSERT INTO webhook_events (id, org_id, type, data, created_at)
    VALUES ('evt_' || lower(hex(randomblob(16))), NEW.org_id, 'member.invited',
            json_object('member_id', NEW.id, 'handle', NEW.handle),
            CAST(strftime('%s','now') AS INTEGER));
END;

CREATE TRIGGER IF NOT EXISTS whv1_member_status AFTER UPDATE OF status ON org_members
WHEN OLD.status <> NEW.status
 AND EXISTS (SELECT 1 FROM webhooks WHERE org_id = NEW.org_id AND deleted_at IS NULL)
BEGIN
    INSERT INTO webhook_events (id, org_id, type, data, created_at)
    SELECT 'evt_' || lower(hex(randomblob(16))), NEW.org_id, t,
           json_object('member_id', NEW.id, 'handle', COALESCE(NEW.handle, OLD.handle)),
           CAST(strftime('%s','now') AS INTEGER)
      FROM (SELECT CASE
                WHEN OLD.status = 'invited' AND NEW.status = 'active' THEN 'member.joined'
                WHEN NEW.status = 'suspended' THEN 'member.suspended'
                WHEN NEW.status = 'removed' AND OLD.status IN ('active','suspended') THEN 'member.removed'
            END AS t)
     WHERE t IS NOT NULL;
END;

CREATE TRIGGER IF NOT EXISTS whv1_agent_created AFTER INSERT ON org_agents
WHEN EXISTS (SELECT 1 FROM webhooks WHERE org_id = NEW.org_id AND deleted_at IS NULL)
BEGIN
    INSERT INTO webhook_events (id, org_id, type, data, created_at)
    VALUES ('evt_' || lower(hex(randomblob(16))), NEW.org_id, 'agent.created',
            json_object('agent', NEW.identity, 'handle', NEW.handle),
            CAST(strftime('%s','now') AS INTEGER));
END;

CREATE TRIGGER IF NOT EXISTS whv1_agent_connected AFTER INSERT ON device_credentials
WHEN NEW.org_id IS NOT NULL
 AND EXISTS (SELECT 1 FROM webhooks WHERE org_id = NEW.org_id AND deleted_at IS NULL)
BEGIN
    INSERT INTO webhook_events (id, org_id, type, data, created_at)
    VALUES ('evt_' || lower(hex(randomblob(16))), NEW.org_id, 'agent.connected',
            json_object('agent', NEW.identity, 'device_id', NEW.id),
            CAST(strftime('%s','now') AS INTEGER));
END;

CREATE TRIGGER IF NOT EXISTS whv1_agent_disconnected AFTER UPDATE OF revoked_at ON device_credentials
WHEN OLD.revoked_at IS NULL AND NEW.revoked_at IS NOT NULL AND NEW.org_id IS NOT NULL
 AND EXISTS (SELECT 1 FROM webhooks WHERE org_id = NEW.org_id AND deleted_at IS NULL)
BEGIN
    INSERT INTO webhook_events (id, org_id, type, data, created_at)
    VALUES ('evt_' || lower(hex(randomblob(16))), NEW.org_id, 'agent.disconnected',
            json_object('agent', NEW.identity, 'device_id', NEW.id),
            CAST(strftime('%s','now') AS INTEGER));
END;

CREATE TRIGGER IF NOT EXISTS whv1_room_created AFTER INSERT ON groups
WHEN NEW.org_id IS NOT NULL
 AND EXISTS (SELECT 1 FROM webhooks WHERE org_id = NEW.org_id AND deleted_at IS NULL)
BEGIN
    INSERT INTO webhook_events (id, org_id, type, data, created_at)
    VALUES ('evt_' || lower(hex(randomblob(16))), NEW.org_id, 'room.created',
            json_object('room_id', NEW.id), CAST(strftime('%s','now') AS INTEGER));
END;

CREATE TRIGGER IF NOT EXISTS whv1_room_deleted AFTER UPDATE OF deleted_at ON groups
WHEN OLD.deleted_at IS NULL AND NEW.deleted_at IS NOT NULL AND NEW.org_id IS NOT NULL
 AND EXISTS (SELECT 1 FROM webhooks WHERE org_id = NEW.org_id AND deleted_at IS NULL)
BEGIN
    INSERT INTO webhook_events (id, org_id, type, data, created_at)
    VALUES ('evt_' || lower(hex(randomblob(16))), NEW.org_id, 'room.deleted',
            json_object('room_id', NEW.id), CAST(strftime('%s','now') AS INTEGER));
END;

CREATE TRIGGER IF NOT EXISTS whv1_billing_status AFTER UPDATE OF status ON orgs
WHEN OLD.status <> NEW.status
 AND EXISTS (SELECT 1 FROM webhooks WHERE org_id = NEW.id AND deleted_at IS NULL)
BEGIN
    INSERT INTO webhook_events (id, org_id, type, data, created_at)
    VALUES ('evt_' || lower(hex(randomblob(16))), NEW.id, 'billing.status_changed',
            json_object('status', NEW.status, 'previous_status', OLD.status),
            CAST(strftime('%s','now') AS INTEGER));
END;
";

/// Create the tables and the event triggers. Idempotent.
pub(super) fn migrate(c: &Connection) -> rusqlite::Result<()> {
    c.execute_batch(SCHEMA)?;
    c.execute_batch(TRIGGERS)?;
    Ok(())
}
