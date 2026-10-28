//! Agent SSH (SDS §11, `docs/agent-ssh.md`).
//!
//! What the postbox keeps is metadata and public keys: which agents offer a shell (as their own
//! machine last reported it), each agent's pinned SSH host key, the org's per-agent switch, and
//! one row per session — who asked, through which grant, when it opened and closed, and how many
//! bytes the tunnel carried. Never terminal content, never a private key: the CA's private key is
//! a file on the host, and the session's own keys exist only on the person's device and the agent's
//! machine.

pub(super) const SCHEMA: &str = "
-- The agent machine's own report of its local opt-in, plus its pinned host key. Written only with
-- the agent's mailbox credential; a different key never replaces the pinned one on its own — it
-- waits in `pending_*` until a person with authority over the agent approves the rotation.
CREATE TABLE IF NOT EXISTS agent_shell_hosts (
    identity       TEXT PRIMARY KEY,
    host_key       TEXT NOT NULL,
    host_fp        TEXT NOT NULL,
    pending_key    TEXT,
    pending_fp     TEXT,
    pending_at     INTEGER,
    enabled        INTEGER NOT NULL,
    mode           TEXT NOT NULL CHECK (mode IN ('container','allowlist','unconfined')),
    max_sessions   INTEGER NOT NULL CHECK (max_sessions BETWEEN 1 AND 8),
    agent_version  TEXT,
    registered_at  INTEGER NOT NULL,
    updated_at     INTEGER NOT NULL,
    rotated_at     INTEGER,
    rotated_by     TEXT
);

-- The org's switch for one org agent (SDS §6.5: the `ssh` delegation also needs the agent admin to
-- enable SSH on the agent). Absent means off.
CREATE TABLE IF NOT EXISTS agent_shell_policy (
    identity    TEXT PRIMARY KEY,
    org_id      TEXT NOT NULL,
    enabled     INTEGER NOT NULL,
    updated_by  TEXT,
    updated_at  INTEGER NOT NULL
);

CREATE TABLE IF NOT EXISTS ssh_sessions (
    id                  TEXT PRIMARY KEY,
    agent               TEXT NOT NULL,
    org_id              TEXT,
    requester_account   TEXT NOT NULL,
    requester_member    TEXT,
    via                 TEXT NOT NULL CHECK (via IN ('owner','org_owner','delegation')),
    delegation_id       TEXT,
    delegation_revision INTEGER,
    member_revision     INTEGER,
    mode                TEXT NOT NULL,
    state               TEXT NOT NULL CHECK (state IN ('awaiting_approval','approved','issued','connected','closed')),
    client_key          TEXT NOT NULL,
    client_fp           TEXT NOT NULL,
    host_fp             TEXT NOT NULL,
    cert_serial         TEXT,
    client_leg_hash     BLOB,
    agent_leg_hash      BLOB,
    client_leg_at       INTEGER,
    agent_leg_at        INTEGER,
    cols                INTEGER NOT NULL,
    rows                INTEGER NOT NULL,
    created_at          INTEGER NOT NULL,
    approved_by         TEXT,
    approved_at         INTEGER,
    issued_at           INTEGER,
    connect_by          INTEGER,
    connected_at        INTEGER,
    last_seen_at        INTEGER,
    ends_by             INTEGER NOT NULL,
    closed_at           INTEGER,
    close_reason        TEXT,
    closed_by           TEXT,
    bytes_client        INTEGER NOT NULL DEFAULT 0,
    bytes_agent         INTEGER NOT NULL DEFAULT 0
);
CREATE INDEX IF NOT EXISTS ssh_sessions_agent ON ssh_sessions(agent, state);
CREATE INDEX IF NOT EXISTS ssh_sessions_state ON ssh_sessions(state, ends_by);
CREATE INDEX IF NOT EXISTS ssh_sessions_requester ON ssh_sessions(requester_account, created_at);
";
