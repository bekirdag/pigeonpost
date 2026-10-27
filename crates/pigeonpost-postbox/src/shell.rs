//! SSH access to an agent's workspace (SDS §11; contract and threat model in `docs/agent-ssh.md`).
//!
//! # What the postbox does, and what it never does
//!
//! The postbox **authorizes** sessions and **issues their credentials**; it never carries or sees
//! terminal traffic. A session is SSH end to end between the person's client and the SSH server
//! embedded in the agent's own `agentd`, spliced through the tunnel broker
//! (`crates/pigeonpost-tunnel`), which sees ciphertext only. For each session the postbox issues:
//!
//! * a **short-lived OpenSSH user certificate** over the client's ephemeral public key, signed by
//!   the SSH CA whose private key is a root-only file on this host (`POSTBOX_SSH_CA_KEY`), bound
//!   to the session id (its only principal), the agent's immutable address, the org, the person,
//!   the grant (and its revision) and the agent's pinned host key, valid for five minutes —
//!   enough to authenticate once;
//! * **two one-time leg tokens**, one per side of the tunnel, stored here only as hashes;
//! * the agent's **pinned host key**, which the client requires before it authenticates.
//!
//! # Who may open one (SDS §11.5, §6.4)
//!
//! All of these, decided again in the transaction that issues credentials and re-checked every
//! [`SWEEP_SECS`] (and immediately after a revocation) for every live session:
//!
//! 1. a **person** (`HumanStepUp`): a validated interactive sign-in with a recent strong `acr`.
//!    Account and org API keys, connectors and mailbox credentials are refused `human_required`;
//! 2. the person **owns** the agent (a personal mailbox on their account), or — for an org agent —
//!    is the org **owner**, or holds a live delegation carrying `ssh` for exactly this agent. Org
//!    admins and agent admins get no shell implicitly;
//! 3. for an org agent, the org's `ssh_allowed` setting **and** the agent admin's per-agent switch;
//! 4. the agent machine's **local opt-in** (reported by agentd over its own credential; nothing here
//!    can turn it on), its control stream connected, and an accepted host key;
//! 5. optionally (`ssh_requires_approval`), a second person's **approval** (org owner, admin or
//!    agent admin, with step-up; never the requester).
//!
//! The verbs model is untouched: automated senders cannot reach any of this.

// Outcomes are built up step by step inside transactions; that reads better than one literal.
#![allow(clippy::field_reassign_with_default)]
// Row tuples read straight from SQL are clearer inline than as one-use type aliases.
#![allow(clippy::type_complexity)]

use crate::audit::ActorKind;
use crate::filtering::feature_on;
use crate::groups::{finish, parse, rate_limit, GErr, GResult, Outcome};
use crate::org::{self, auth_denial, Perm, Role};
use crate::policy::{authorize, Action, Actor, DenyReason, Facts, Resource};
use crate::{capabilities, now_unix, store, AccountCredential, AppState, ClientIp, Principal};
use axum::extract::rejection::JsonRejection;
use axum::extract::{Path, Query, State};
use axum::http::{header, HeaderMap, HeaderValue, StatusCode};
use axum::response::sse::{Event, KeepAlive, Sse};
use axum::response::{IntoResponse, Response};
use axum::{Extension, Json};
use rusqlite::{params, Connection, OptionalExtension};
use serde::Deserialize;
use serde_json::{json, Value};
use std::collections::HashMap;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Mutex;

// ---- limits ------------------------------------------------------------------------------------

/// The longest a session may last (SDS §11.3: at most 8 h).
pub(crate) const MAX_SESSION_SECS: u64 = 8 * 3_600;
/// No traffic in either direction for this long ends a session (SDS §11.3: 15 minutes). Enforced
/// by the broker and again by agentd; the postbox only tells them.
pub(crate) const IDLE_SECS: u64 = 15 * 60;
/// Both legs must reach the broker this soon after credentials are issued.
pub(crate) const CONNECT_SECS: u64 = 120;
/// The user certificate is for authenticating once, not for the session's lifetime.
pub(crate) const CERT_SECS: u64 = 300;
/// A request waits this long for approval, and an approval this long to be collected.
pub(crate) const APPROVAL_SECS: u64 = 600;
/// Closed sessions' rows are deleted after this; the audit log keeps the record (2 years).
pub(crate) const RETAIN_CLOSED_SECS: u64 = 30 * 86_400;
/// How often live sessions are re-authorized when nothing else asks.
pub(crate) const SWEEP_SECS: u64 = 15;
/// Session requests one person may make per hour.
const REQUESTS_PER_HOUR: usize = 30;
/// Sessions one agent may hold at once, whatever its machine reports.
const MAX_LIVE_PER_AGENT: i64 = 8;
/// The SSH user name every session uses; who is connecting is in the certificate.
pub(crate) const SSH_USER: &str = "pigeonpost";
/// Modes an agent machine may report (SDS §11.4, D-20). `user` (bubblewrap/dedicated uid) is not
/// implemented in agentd yet and is not accepted rather than mislabelled.
const MODES: &[&str] = &["container", "allowlist", "unconfined"];

// ---- runtime -----------------------------------------------------------------------------------

/// The CA, the broker's public URL, and the connected agents' control streams.
#[derive(Default)]
pub(crate) struct Runtime {
    ca: Option<Ca>,
    broker_url: Option<String>,
    agents: Mutex<HashMap<String, AgentLink>>,
    next_conn: AtomicU64,
    sweeping: tokio::sync::Mutex<()>,
}

struct Ca {
    key: ssh_key::PrivateKey,
    public: String,
    fp: String,
}

struct AgentLink {
    conn: u64,
    tx: tokio::sync::mpsc::Sender<(&'static str, Value)>,
}

impl Runtime {
    /// `POSTBOX_SSH_CA_KEY` (an unencrypted OpenSSH Ed25519 private key file, owner-only) and
    /// `POSTBOX_SHELL_BROKER_URL` (`wss://tunnel.pigeonpost.dev`). Without both, agent SSH is
    /// unavailable and says so; it never falls back to anything weaker.
    pub(crate) fn from_env() -> Self {
        let broker_url = std::env::var("POSTBOX_SHELL_BROKER_URL")
            .ok()
            .map(|u| u.trim().trim_end_matches('/').to_string())
            .filter(|u| valid_broker_url(u));
        let ca = std::env::var("POSTBOX_SSH_CA_KEY")
            .ok()
            .filter(|p| !p.trim().is_empty())
            .and_then(|path| match load_ca(path.trim()) {
                Ok(ca) => Some(ca),
                Err(e) => {
                    tracing::error!(error = e, "SSH CA key not loaded; agent SSH unavailable");
                    None
                }
            });
        if let Some(ca) = &ca {
            tracing::info!(ca = %ca.fp, broker = ?broker_url, "agent SSH CA loaded");
        }
        Runtime {
            ca,
            broker_url,
            ..Runtime::default()
        }
    }

    /// A runtime with a CA made from `seed` (tests only).
    #[cfg(test)]
    pub(crate) fn for_test(seed: [u8; 32], broker_url: &str) -> Self {
        let key = ssh_key::PrivateKey::from(ssh_key::private::Ed25519Keypair::from_seed(&seed));
        Runtime {
            ca: Some(ca_of(key)),
            broker_url: Some(broker_url.to_string()),
            ..Runtime::default()
        }
    }

    fn available(&self) -> GResult<(&Ca, &str)> {
        match (&self.ca, &self.broker_url) {
            (Some(ca), Some(url)) => Ok((ca, url.as_str())),
            _ => Err(GErr::new(
                StatusCode::SERVICE_UNAVAILABLE,
                "shell_unavailable",
                "agent SSH is not configured on this server",
            )),
        }
    }

    pub(crate) fn ca_public(&self) -> Option<(&str, &str)> {
        self.ca.as_ref().map(|c| (c.public.as_str(), c.fp.as_str()))
    }

    fn connected(&self, agent: &str) -> bool {
        self.agents
            .lock()
            .expect("shell agents lock")
            .get(agent)
            .is_some_and(|l| !l.tx.is_closed())
    }

    /// Deliver one control event to the agent's machine, if it is connected. Best effort: a
    /// machine that is not listening cannot be holding a session the broker still carries — and
    /// the broker ends those itself.
    fn send(&self, agent: &str, kind: &'static str, data: Value) -> bool {
        let tx = self
            .agents
            .lock()
            .expect("shell agents lock")
            .get(agent)
            .map(|l| l.tx.clone());
        match tx {
            Some(tx) => tx.try_send((kind, data)).is_ok(),
            None => false,
        }
    }
}

fn ca_of(key: ssh_key::PrivateKey) -> Ca {
    let public = key
        .public_key()
        .to_openssh()
        .unwrap_or_default()
        .trim()
        .to_string();
    let fp = key
        .public_key()
        .fingerprint(ssh_key::HashAlg::Sha256)
        .to_string();
    Ca { key, public, fp }
}

fn load_ca(path: &str) -> Result<Ca, &'static str> {
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let meta = std::fs::metadata(path).map_err(|_| "unreadable")?;
        // The CA key signs access to people's machines: refuse a file anyone else can read.
        if meta.permissions().mode() & 0o077 != 0 {
            return Err("permissions too open (must be owner-only)");
        }
    }
    let raw = std::fs::read_to_string(path).map_err(|_| "unreadable")?;
    let key =
        ssh_key::PrivateKey::from_openssh(raw.as_bytes()).map_err(|_| "not an OpenSSH key")?;
    if key.is_encrypted() {
        return Err("encrypted keys are not supported");
    }
    if key.algorithm() != ssh_key::Algorithm::Ed25519 {
        return Err("the CA must be an Ed25519 key");
    }
    Ok(ca_of(key))
}

/// `wss://host[:port]`, or `ws://` to loopback for local tests. No path, query or credentials:
/// the session path is appended per session.
pub(crate) fn valid_broker_url(u: &str) -> bool {
    let Ok(url) = url_parse(u) else {
        return false;
    };
    url.0 == "wss"
        || (url.0 == "ws" && matches!(url.1.as_str(), "127.0.0.1" | "localhost" | "[::1]"))
}

/// (scheme, host) of a bare `scheme://host[:port]` URL, or an error for anything with a path,
/// query, fragment or userinfo.
fn url_parse(u: &str) -> Result<(String, String), ()> {
    let (scheme, rest) = u.split_once("://").ok_or(())?;
    if rest.is_empty()
        || rest
            .chars()
            .any(|c| matches!(c, '/' | '?' | '#' | '@' | ' ') || c.is_control())
    {
        return Err(());
    }
    let host = if rest.starts_with('[') {
        rest.split_inclusive(']').next().unwrap_or("").to_string()
    } else {
        rest.split(':').next().unwrap_or("").to_string()
    };
    if host.is_empty() {
        return Err(());
    }
    Ok((scheme.to_ascii_lowercase(), host.to_ascii_lowercase()))
}

/// Periodic re-authorization of every live session (and expiry of pending ones).
pub(crate) fn start_worker(state: AppState) {
    tokio::spawn(async move {
        let mut tick = tokio::time::interval(std::time::Duration::from_secs(SWEEP_SECS));
        loop {
            tick.tick().await;
            sweep(&state).await;
        }
    });
}

/// Re-check soon, after something that may have revoked authority (a delegation, a membership, an
/// org setting, an agent switch).
pub(crate) fn sweep_soon(state: &AppState) {
    let state = state.clone();
    tokio::spawn(async move { sweep(&state).await });
}

// ---- keys and identifiers ----------------------------------------------------------------------

/// An `ssh-ed25519` public key in OpenSSH form. Other algorithms are refused: one well-understood
/// algorithm for an ephemeral key nobody chooses by hand.
pub(crate) fn parse_ed25519(raw: &str) -> GResult<ssh_key::PublicKey> {
    let raw = raw.trim();
    if raw.len() > 400 {
        return Err(GErr::bad("bad_key", "public key too long"));
    }
    let key = ssh_key::PublicKey::from_openssh(raw)
        .map_err(|_| GErr::bad("bad_key", "not an OpenSSH public key"))?;
    if key.algorithm() != ssh_key::Algorithm::Ed25519 {
        return Err(GErr::bad("bad_key", "only ssh-ed25519 keys are accepted"));
    }
    Ok(key)
}

pub(crate) fn fingerprint(key: &ssh_key::PublicKey) -> String {
    key.fingerprint(ssh_key::HashAlg::Sha256).to_string()
}

/// The public key without its comment, the form the postbox stores and returns.
fn openssh(key: &ssh_key::PublicKey) -> String {
    let mut k = key.clone();
    k.set_comment("");
    k.to_openssh().unwrap_or_default().trim().to_string()
}

fn valid_session_id(id: &str) -> bool {
    id.len() == 30
        && id.starts_with("ssh_")
        && id[4..]
            .bytes()
            .all(|b| b.is_ascii_lowercase() || (b'2'..=b'7').contains(&b))
}

fn new_session_id() -> String {
    format!("ssh_{}", crate::groups::rand_b32(16))
}

/// A one-time leg token and its stored hash.
fn new_leg() -> (String, Vec<u8>) {
    let mut b = [0u8; 32];
    rand_core::RngCore::fill_bytes(&mut rand_core::OsRng, &mut b);
    let token = format!("lt_{}", org::b64url(&b));
    let hash = crate::sha256(token.as_bytes()).to_vec();
    (token, hash)
}

/// The certificate's only principal: a session id, never a readable handle.
pub(crate) fn principal_for(sid: &str) -> String {
    format!("pp-ssh-{sid}")
}

/// Sign the session's user certificate. `grant` becomes the key id: signed by the CA along with
/// everything else, so agentd can check every binding without asking anyone.
fn sign_cert(
    ca: &Ca,
    client: &ssh_key::PublicKey,
    sid: &str,
    grant: &Value,
    now: u64,
) -> GResult<(String, String)> {
    let mut nonce = [0u8; 32];
    rand_core::RngCore::fill_bytes(&mut rand_core::OsRng, &mut nonce);
    let mut serial = [0u8; 8];
    rand_core::RngCore::fill_bytes(&mut rand_core::OsRng, &mut serial);
    let serial = u64::from_be_bytes(serial);
    let fail = |_| {
        GErr::new(
            StatusCode::INTERNAL_SERVER_ERROR,
            "cert_failed",
            "could not sign the session certificate",
        )
    };
    let mut b = ssh_key::certificate::Builder::new(
        nonce.to_vec(),
        client.key_data().clone(),
        now.saturating_sub(60),
        now + CERT_SECS,
    )
    .map_err(fail)?;
    b.serial(serial).map_err(fail)?;
    b.cert_type(ssh_key::certificate::CertType::User)
        .map_err(fail)?;
    b.key_id(grant.to_string()).map_err(fail)?;
    b.valid_principal(principal_for(sid)).map_err(fail)?;
    // An interactive terminal and nothing else: no forwarding of ports, agents or X11, no
    // user rc. agentd refuses those channels whatever the certificate says.
    b.extension("permit-pty", "").map_err(fail)?;
    b.comment(sid).map_err(fail)?;
    let cert = b.sign(&ca.key).map_err(fail)?;
    let line = cert.to_openssh().map_err(fail)?;
    Ok((line.trim().to_string(), format!("{serial:016x}")))
}

// ---- rows --------------------------------------------------------------------------------------

#[derive(Debug, Clone)]
pub(crate) struct HostRow {
    pub host_key: String,
    pub host_fp: String,
    pub pending_fp: Option<String>,
    pub enabled: bool,
    pub mode: String,
    pub max_sessions: i64,
    pub updated_at: i64,
}

fn load_host(c: &Connection, agent: &str) -> GResult<Option<HostRow>> {
    Ok(c.query_row(
        "SELECT host_key, host_fp, pending_fp, enabled, mode, max_sessions, updated_at
           FROM agent_shell_hosts WHERE identity = ?1",
        [agent],
        |r| {
            Ok(HostRow {
                host_key: r.get(0)?,
                host_fp: r.get(1)?,
                pending_fp: r.get(2)?,
                enabled: r.get::<_, i64>(3)? != 0,
                mode: r.get(4)?,
                max_sessions: r.get(5)?,
                updated_at: r.get(6)?,
            })
        },
    )
    .optional()?)
}

#[derive(Debug, Clone)]
pub(crate) struct SessionRow {
    pub id: String,
    pub agent: String,
    pub org_id: Option<String>,
    pub requester: String,
    pub member: Option<String>,
    pub via: String,
    pub delegation: Option<String>,
    pub delegation_rev: Option<i64>,
    pub member_rev: Option<i64>,
    pub mode: String,
    pub state: String,
    pub client_key: String,
    pub client_fp: String,
    pub host_fp: String,
    pub cert_serial: Option<String>,
    pub client_leg_hash: Option<Vec<u8>>,
    pub agent_leg_hash: Option<Vec<u8>>,
    pub client_leg_at: Option<i64>,
    pub agent_leg_at: Option<i64>,
    pub cols: i64,
    pub rows: i64,
    pub created_at: i64,
    pub approved_by: Option<String>,
    pub approved_at: Option<i64>,
    pub issued_at: Option<i64>,
    pub connect_by: Option<i64>,
    pub connected_at: Option<i64>,
    pub ends_by: i64,
    pub closed_at: Option<i64>,
    pub close_reason: Option<String>,
    pub bytes_client: i64,
    pub bytes_agent: i64,
}

const SESSION_COLS: &str = "id, agent, org_id, requester_account, requester_member, via, delegation_id,
    delegation_revision, member_revision, mode, state, client_key, client_fp, host_fp, cert_serial,
    client_leg_hash, agent_leg_hash, client_leg_at, agent_leg_at, cols, rows, created_at, approved_by,
    approved_at, issued_at, connect_by, connected_at, ends_by, closed_at, close_reason, bytes_client,
    bytes_agent";

fn map_session(r: &rusqlite::Row<'_>) -> rusqlite::Result<SessionRow> {
    Ok(SessionRow {
        id: r.get(0)?,
        agent: r.get(1)?,
        org_id: r.get(2)?,
        requester: r.get(3)?,
        member: r.get(4)?,
        via: r.get(5)?,
        delegation: r.get(6)?,
        delegation_rev: r.get(7)?,
        member_rev: r.get(8)?,
        mode: r.get(9)?,
        state: r.get(10)?,
        client_key: r.get(11)?,
        client_fp: r.get(12)?,
        host_fp: r.get(13)?,
        cert_serial: r.get(14)?,
        client_leg_hash: r.get(15)?,
        agent_leg_hash: r.get(16)?,
        client_leg_at: r.get(17)?,
        agent_leg_at: r.get(18)?,
        cols: r.get(19)?,
        rows: r.get(20)?,
        created_at: r.get(21)?,
        approved_by: r.get(22)?,
        approved_at: r.get(23)?,
        issued_at: r.get(24)?,
        connect_by: r.get(25)?,
        connected_at: r.get(26)?,
        ends_by: r.get(27)?,
        closed_at: r.get(28)?,
        close_reason: r.get(29)?,
        bytes_client: r.get(30)?,
        bytes_agent: r.get(31)?,
    })
}

fn load_session(c: &Connection, id: &str) -> GResult<Option<SessionRow>> {
    if !valid_session_id(id) {
        return Ok(None);
    }
    Ok(c.query_row(
        &format!("SELECT {SESSION_COLS} FROM ssh_sessions WHERE id = ?1"),
        [id],
        map_session,
    )
    .optional()?)
}

fn no_session() -> GErr {
    GErr::new(StatusCode::NOT_FOUND, "no_session", "no such session")
}

fn session_json(c: &Connection, s: &SessionRow) -> GResult<Value> {
    let agent_handle = handle_of(c, &s.agent)?;
    let requester_handle = match (&s.org_id, &s.member) {
        (Some(org), Some(m)) => org::load_member(c, org, m)?.and_then(|m| m.handle),
        _ => None,
    };
    let duration = match (s.connected_at, s.closed_at) {
        (Some(a), Some(b)) => Some((b - a).max(0)),
        _ => None,
    };
    Ok(json!({
        "id": s.id, "agent": s.agent, "agent_handle": agent_handle, "org_id": s.org_id,
        "requester": { "account": s.requester, "handle": requester_handle }, "via": s.via,
        "mode": s.mode, "state": s.state, "client_key_fp": s.client_fp, "host_key_fp": s.host_fp,
        "created_at": s.created_at, "approved_by": s.approved_by, "approved_at": s.approved_at,
        "issued_at": s.issued_at, "cert_serial": s.cert_serial, "connected_at": s.connected_at, "ends_by": s.ends_by,
        "closed_at": s.closed_at, "close_reason": s.close_reason, "duration_secs": duration,
        "bytes": { "client": s.bytes_client, "agent": s.bytes_agent },
    }))
}

fn handle_of(c: &Connection, address: &str) -> GResult<Option<String>> {
    Ok(c.query_row(
        "SELECT handle FROM identities WHERE address = ?1",
        [address],
        |r| r.get::<_, Option<String>>(0),
    )
    .optional()?
    .flatten())
}

/// An agent by immutable address or by handle: (address, owning account).
fn resolve_agent(c: &Connection, raw: &str) -> GResult<(String, Option<String>)> {
    let raw = raw.trim();
    if raw.is_empty() || raw.len() > 200 || !raw.starts_with('/') {
        return Err(not_an_agent());
    }
    c.query_row(
        "SELECT address, account_id FROM identities WHERE address = ?1 OR handle = ?1
          ORDER BY address = ?1 DESC LIMIT 1",
        [raw],
        |r| Ok((r.get(0)?, r.get(1)?)),
    )
    .optional()?
    .ok_or_else(not_an_agent)
}

fn not_an_agent() -> GErr {
    GErr::new(StatusCode::NOT_FOUND, "no_agent", "no such agent")
}

// ---- authority ---------------------------------------------------------------------------------

/// How a person may open a shell on one agent.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct Grant {
    pub via: &'static str,
    pub org_id: Option<String>,
    pub member: Option<String>,
    pub member_rev: Option<i64>,
    pub delegation: Option<String>,
    pub delegation_rev: Option<i64>,
    pub requires_approval: bool,
}

/// The org agent row's org, when `agent` is a live org agent.
fn org_of_agent(c: &Connection, agent: &str) -> GResult<Option<(String, String)>> {
    Ok(c.query_row(
        "SELECT org_id, status FROM org_agents WHERE identity = ?1 AND status <> 'deleted'",
        [agent],
        |r| Ok((r.get(0)?, r.get(1)?)),
    )
    .optional()?)
}

fn agent_switch_on(c: &Connection, agent: &str) -> GResult<bool> {
    Ok(c.query_row(
        "SELECT enabled FROM agent_shell_policy WHERE identity = ?1",
        [agent],
        |r| r.get::<_, i64>(0),
    )
    .optional()?
    .is_some_and(|v| v != 0))
}

/// Whether `account` may open a shell on `agent` now (SDS §11.5 items 1–3; step-up is the policy's
/// part). `Err` carries the stable refusal code.
pub(crate) fn grant_in(
    c: &Connection,
    account: &str,
    agent: &str,
    agent_account: Option<&str>,
    now: u64,
) -> GResult<Grant> {
    if let Some((org_id, agent_status)) = org_of_agent(c, agent)? {
        let o = org::load_org(c, &org_id)?.ok_or_else(not_an_agent)?;
        let member = org::member_for_account(c, &org_id, account)?;
        let Some(member) = member.filter(|m| m.status == "active") else {
            return Err(GErr::forbidden(
                "not_org_member",
                "only active members of the agent's organization may open a shell on it",
            ));
        };
        if !matches!(o.status.as_str(), "active" | "past_due") || agent_status != "active" {
            return Err(GErr::forbidden(
                "org_inactive",
                "the organization or the agent is not active",
            ));
        }
        if !o.settings.ssh_allowed {
            return Err(GErr::forbidden(
                "ssh_not_allowed",
                "this organization does not allow SSH to its agents",
            ));
        }
        if !agent_switch_on(c, agent)? {
            return Err(GErr::forbidden(
                "ssh_not_enabled",
                "an agent admin has not enabled SSH on this agent",
            ));
        }
        let roles = org::roles_of(c, &org_id, &member.id)?;
        let requires_approval = o.settings.ssh_requires_approval;
        if roles.has(Role::Owner) {
            return Ok(Grant {
                via: "org_owner",
                org_id: Some(org_id),
                member: Some(member.id),
                member_rev: Some(member.authority_revision),
                delegation: None,
                delegation_rev: None,
                requires_approval,
            });
        }
        let dlg: Option<(String, i64)> = c
            .query_row(
                "SELECT id, revision FROM org_delegations
                  WHERE org_id = ?1 AND grantee_member_id = ?2 AND target_identity = ?3
                    AND revoked_at IS NULL AND (expires_at IS NULL OR expires_at > ?4)
                    AND EXISTS (SELECT 1 FROM json_each(capabilities) WHERE value = 'ssh')",
                params![org_id, member.id, agent, now as i64],
                |r| Ok((r.get(0)?, r.get(1)?)),
            )
            .optional()?;
        return match dlg {
            Some((id, rev)) => Ok(Grant {
                via: "delegation",
                org_id: Some(org_id),
                member: Some(member.id),
                member_rev: Some(member.authority_revision),
                delegation: Some(id),
                delegation_rev: Some(rev),
                requires_approval,
            }),
            None => Err(GErr::forbidden(
                "delegation_required",
                "opening a shell on this agent needs an `ssh` delegation for it (org admins get none implicitly)",
            )),
        };
    }
    if agent_account == Some(account) {
        return Ok(Grant {
            via: "owner",
            org_id: None,
            member: None,
            member_rev: None,
            delegation: None,
            delegation_rev: None,
            requires_approval: false,
        });
    }
    Err(GErr::forbidden(
        "forbidden",
        "only the agent's owner or someone it is delegated to may open a shell on it",
    ))
}

/// Whether a live session's grant still holds exactly as issued.
fn still_granted(c: &Connection, s: &SessionRow, now: u64) -> GResult<Result<(), &'static str>> {
    let agent_account: Option<String> = c
        .query_row(
            "SELECT account_id FROM identities WHERE address = ?1",
            [&s.agent],
            |r| r.get(0),
        )
        .optional()?
        .flatten();
    let g = match grant_in(c, &s.requester, &s.agent, agent_account.as_deref(), now) {
        Ok(g) => g,
        Err(e) => return Ok(Err(e.code)),
    };
    if g.via != s.via
        || g.org_id != s.org_id
        || g.member != s.member
        || g.member_rev != s.member_rev
        || g.delegation != s.delegation
        || g.delegation_rev != s.delegation_rev
    {
        return Ok(Err("authority_changed"));
    }
    match load_host(c, &s.agent)? {
        Some(h) if h.enabled && h.host_fp == s.host_fp && h.pending_fp.is_none() => Ok(Ok(())),
        Some(h) if !h.enabled => Ok(Err("agent_disabled")),
        _ => Ok(Err("host_key_changed")),
    }
}

/// Who may approve, deny or end sessions and approve host-key rotations: the personal owner, or an
/// active org owner, admin or agent admin (SDS §6.4 "Agent admin"). This is not the right to a
/// shell; it is the right to supervise one.
fn supervisor_in(
    c: &Connection,
    account: &str,
    agent: &str,
    agent_account: Option<&str>,
) -> GResult<bool> {
    if let Some((org_id, _)) = org_of_agent(c, agent)? {
        let Some(m) =
            org::member_for_account(c, &org_id, account)?.filter(|m| m.status == "active")
        else {
            return Ok(false);
        };
        let roles = org::roles_of(c, &org_id, &m.id)?;
        return Ok(org::permits(&roles, Perm::ManageAgents, false));
    }
    Ok(agent_account == Some(account))
}

// ---- request context ---------------------------------------------------------------------------

/// The signed-in person, with the `agent_ssh` flag, and the policy decision for `action`.
struct Person {
    principal: Principal,
    account: String,
}

async fn person(state: &AppState, headers: &HeaderMap) -> GResult<Person> {
    let principal = crate::principal_for_token(state, crate::bearer(headers)).await?;
    // Every credential that is not a person is refused the same way, whatever it could otherwise
    // reach: API keys, org keys, connectors and mailbox credentials (SDS §11.5).
    let account = match &principal {
        Principal::Account(a, AccountCredential::Member(_)) => a.clone(),
        _ => return Err(auth_denial(crate::principal::AuthDenial::HumanRequired)),
    };
    if !feature_on(state, Some(&account), capabilities::AGENT_SSH).await {
        return Err(GErr::new(
            StatusCode::NOT_FOUND,
            "feature_disabled",
            "agent SSH is not enabled for this account",
        ));
    }
    Ok(Person { principal, account })
}

fn decide(
    state: &AppState,
    p: &Person,
    action: Action,
    agent: &str,
    permitted: bool,
) -> GResult<()> {
    let facts = Facts {
        now: now_unix(),
        step_up: (*state.step_up).clone(),
        shell_permitted: permitted,
        ..Facts::default()
    };
    authorize(
        &Actor::Principal(&p.principal),
        action,
        &Resource::AgentShell { agent },
        &facts,
    )
    .map_err(deny_to_gerr)
}

fn deny_to_gerr(deny: DenyReason) -> GErr {
    match deny {
        DenyReason::Auth(d) => auth_denial(d),
        DenyReason::NotPermitted => GErr::forbidden("forbidden", "not permitted"),
        DenyReason::CredentialNotAccepted => {
            auth_denial(crate::principal::AuthDenial::HumanRequired)
        }
        other => GErr::forbidden(other.code(), "this action is not allowed"),
    }
}

/// The step-up check alone, run before anything is resolved, so the challenge comes first and a
/// person without a recent strong sign-in learns nothing else about the agent.
fn require_step_up(state: &AppState, p: &Person) -> GResult<()> {
    crate::principal::require_step_up(
        &p.principal,
        state.step_up.max_age,
        &state.step_up.acr_values,
        now_unix(),
    )
    .map(|_| ())
    .map_err(auth_denial)
}

#[allow(clippy::too_many_arguments)]
fn audit_in(
    c: &Connection,
    state: &AppState,
    org_id: Option<&str>,
    kind: ActorKind,
    actor: Option<&str>,
    action: &str,
    target: &str,
    details: Value,
    ip: Option<&str>,
    now: u64,
) -> GResult<()> {
    let ip = ip.and_then(|ip| ip.parse::<std::net::IpAddr>().ok());
    let mut row = state.auditor.row(
        kind,
        actor.map(str::to_string),
        action,
        Some(target.to_string()),
        details,
        ip,
        None,
        now,
    );
    row.org_id = org_id.map(str::to_string);
    store::audit::append_in(c, &row)?;
    Ok(())
}

/// Close one session inside the caller's transaction. Returns the agent to tell, when it changed.
#[allow(clippy::too_many_arguments)]
fn close_in(
    c: &Connection,
    state: &AppState,
    s: &SessionRow,
    reason: &str,
    by: Option<&str>,
    by_kind: ActorKind,
    bytes: Option<(i64, i64)>,
    now: u64,
) -> GResult<bool> {
    if s.state == "closed" {
        return Ok(false);
    }
    let (bc, ba) = bytes.unwrap_or((s.bytes_client, s.bytes_agent));
    let (bc, ba) = (bc.max(s.bytes_client), ba.max(s.bytes_agent));
    c.execute(
        "UPDATE ssh_sessions SET state = 'closed', closed_at = ?2, close_reason = ?3, closed_by = ?4,
                bytes_client = ?5, bytes_agent = ?6 WHERE id = ?1 AND state <> 'closed'",
        params![s.id, now as i64, reason, by, bc, ba],
    )?;
    let duration = s.connected_at.map(|t| (now as i64 - t).max(0)).unwrap_or(0);
    // Metadata only: who, which agent, how long, how much — never what was typed or shown.
    audit_in(
        c,
        state,
        s.org_id.as_deref(),
        by_kind,
        by,
        "ssh.session.close",
        &s.agent,
        json!({ "session_id": s.id, "reason": reason, "mode": s.mode, "via": s.via,
                "requester": s.requester, "connected": s.connected_at.is_some(),
                "duration_secs": duration, "bytes_client": bc, "bytes_agent": ba }),
        None,
        now,
    )?;
    Ok(s.issued_at.is_some())
}

fn notify_revoked(state: &AppState, ended: &[(String, String, String)]) {
    for (sid, agent, reason) in ended {
        state.shell.send(
            agent,
            "shell.revoke",
            json!({ "session_id": sid, "reason": reason }),
        );
    }
}

/// Re-authorize every open session; close what no longer holds or has timed out.
pub(crate) async fn sweep(state: &AppState) {
    let _guard = state.shell.sweeping.lock().await;
    let st = state.clone();
    let now = now_unix();
    let ended = state
        .store
        .group_tx(move |tx| -> GResult<Vec<(String, String, String)>> {
            let mut stmt = tx.prepare(&format!(
                "SELECT {SESSION_COLS} FROM ssh_sessions WHERE state <> 'closed'"
            ))?;
            let open: Vec<SessionRow> =
                stmt.query_map([], map_session)?.collect::<Result<_, _>>()?;
            drop(stmt);
            let mut ended = Vec::new();
            for s in open {
                if let Some(reason) = due_reason(tx, &s, now)? {
                    if close_in(tx, &st, &s, reason, None, ActorKind::System, None, now)? {
                        ended.push((s.id.clone(), s.agent.clone(), reason.to_string()));
                    }
                }
            }
            tx.execute(
                "DELETE FROM ssh_sessions WHERE state = 'closed' AND closed_at < ?1",
                [now as i64 - RETAIN_CLOSED_SECS as i64],
            )?;
            Ok(ended)
        })
        .await;
    match ended {
        Ok(ended) => notify_revoked(state, &ended),
        Err(e) => tracing::error!(code = e.code, "shell sweep failed"),
    }
}

/// Why an open session must end now, if it must.
fn due_reason(c: &Connection, s: &SessionRow, now: u64) -> GResult<Option<&'static str>> {
    let n = now as i64;
    let timed_out = match s.state.as_str() {
        "awaiting_approval" => s.created_at + APPROVAL_SECS as i64 <= n,
        "approved" => s.approved_at.unwrap_or(s.created_at) + APPROVAL_SECS as i64 <= n,
        "issued" => s.connect_by.is_some_and(|t| t <= n),
        _ => false,
    };
    if timed_out {
        return Ok(Some(match s.state.as_str() {
            "issued" => "connect_timeout",
            _ => "approval_expired",
        }));
    }
    if s.ends_by <= n {
        return Ok(Some("max_duration"));
    }
    if let Err(code) = still_granted(c, s, now)? {
        return Ok(Some(code));
    }
    Ok(None)
}

// ---- the agent's side --------------------------------------------------------------------------

/// The agent mailbox behind a mailbox credential, after the policy and the flag.
async fn agent_ctx(state: &AppState, headers: &HeaderMap) -> GResult<store::StoredIdentity> {
    let principal = crate::principal_for_token(state, crate::bearer(headers)).await?;
    let Principal::Identity(identity) = &principal else {
        return Err(GErr::forbidden(
            "mailbox_credential_required",
            "the agent's side of SSH uses the agent mailbox's own credential",
        ));
    };
    authorize(
        &Actor::Principal(&principal),
        Action::ShellAgent,
        &Resource::AgentShell {
            agent: &identity.address,
        },
        &Facts {
            now: now_unix(),
            ..Facts::default()
        },
    )
    .map_err(deny_to_gerr)?;
    if !feature_on(
        state,
        identity.account_id.as_deref(),
        capabilities::AGENT_SSH,
    )
    .await
    {
        return Err(GErr::new(
            StatusCode::NOT_FOUND,
            "feature_disabled",
            "agent SSH is not enabled for this account",
        ));
    }
    Ok(identity.clone())
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct HostReport {
    enabled: bool,
    mode: String,
    max_sessions: i64,
    host_key: String,
    #[serde(default)]
    version: Option<String>,
}

/// `PUT /v1/shell/agent` — the machine reports its local opt-in and its host key (SDS §11.3).
///
/// The first key is pinned. A different key later never replaces it: it waits as `pending` until
/// a person with authority over the agent approves the rotation, and sessions are refused until
/// then — a stolen mailbox credential cannot silently swap in a machine of its own.
pub(crate) async fn put_host(
    State(state): State<AppState>,
    headers: HeaderMap,
    body: Result<Json<Value>, JsonRejection>,
) -> Response {
    let result = async {
        let me = agent_ctx(&state, &headers).await?;
        state.shell.available()?;
        let (req, _): (HostReport, String) = parse(body)?;
        if !MODES.contains(&req.mode.as_str()) {
            return Err(GErr::bad("bad_mode", "mode is container, allowlist or unconfined"));
        }
        if !(1..=8).contains(&req.max_sessions) {
            return Err(GErr::bad("bad_request", "max_sessions is 1-8"));
        }
        let key = parse_ed25519(&req.host_key)?;
        let (line, fp) = (openssh(&key), fingerprint(&key));
        let version: Option<String> = req
            .version
            .map(|v| v.chars().filter(|c| c.is_ascii_graphic()).take(40).collect());
        let st = state.clone();
        let now = now_unix();
        let agent = me.address.clone();
        let out = state
            .store
            .group_tx(move |tx| -> GResult<(Outcome, Vec<(String, String, String)>)> {
                let n = now as i64;
                let prior = load_host(tx, &agent)?;
                let mut ended = Vec::new();
                let status = match &prior {
                    None => {
                        tx.execute(
                            "INSERT INTO agent_shell_hosts (identity, host_key, host_fp, enabled, mode, max_sessions,
                                    agent_version, registered_at, updated_at)
                             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?8)",
                            params![agent, line, fp, req.enabled as i64, req.mode, req.max_sessions, version, n],
                        )?;
                        audit_in(tx, &st, org_of_agent(tx, &agent)?.map(|o| o.0).as_deref(), ActorKind::Agent,
                            None, "ssh.host_key.register", &agent, json!({ "fingerprint": fp }), None, now)?;
                        "ok"
                    }
                    Some(h) => {
                        let rotating = h.host_fp != fp;
                        if rotating && h.pending_fp.as_deref() != Some(fp.as_str()) {
                            tx.execute(
                                "UPDATE agent_shell_hosts SET pending_key = ?2, pending_fp = ?3, pending_at = ?4 WHERE identity = ?1",
                                params![agent, line, fp, n],
                            )?;
                            audit_in(tx, &st, org_of_agent(tx, &agent)?.map(|o| o.0).as_deref(), ActorKind::Agent,
                                None, "ssh.host_key.rotation_requested", &agent,
                                json!({ "pinned": h.host_fp, "offered": fp }), None, now)?;
                        } else if !rotating && h.pending_fp.is_some() {
                            // The machine went back to the pinned key: drop the pending one.
                            tx.execute(
                                "UPDATE agent_shell_hosts SET pending_key = NULL, pending_fp = NULL, pending_at = NULL WHERE identity = ?1",
                                [&agent],
                            )?;
                        }
                        tx.execute(
                            "UPDATE agent_shell_hosts SET enabled = ?2, mode = ?3, max_sessions = ?4, agent_version = ?5,
                                    updated_at = ?6 WHERE identity = ?1",
                            params![agent, req.enabled as i64, req.mode, req.max_sessions, version, n],
                        )?;
                        if !req.enabled || h.mode != req.mode {
                            // Switched off (or to a different boundary) locally: end what is open.
                            let reason = if req.enabled { "mode_changed" } else { "agent_disabled" };
                            let mut stmt = tx.prepare(&format!(
                                "SELECT {SESSION_COLS} FROM ssh_sessions WHERE agent = ?1 AND state <> 'closed'"
                            ))?;
                            let open: Vec<SessionRow> =
                                stmt.query_map([&agent], map_session)?.collect::<Result<_, _>>()?;
                            drop(stmt);
                            for s in open {
                                if close_in(tx, &st, &s, reason, None, ActorKind::Agent, None, now)? {
                                    ended.push((s.id.clone(), s.agent.clone(), reason.to_string()));
                                }
                            }
                        }
                        if rotating { "host_key_pending" } else { "ok" }
                    }
                };
                let h = load_host(tx, &agent)?.ok_or_else(not_an_agent)?;
                let (ca, ca_fp) = st.shell.ca_public().unwrap_or(("", ""));
                Ok((
                    Outcome::ok(json!({
                        "status": status, "host_key_fp": h.host_fp, "pending_host_key_fp": h.pending_fp,
                        "enabled": h.enabled, "mode": h.mode, "ca_public_key": ca, "ca_fp": ca_fp,
                    })),
                    ended,
                ))
            })
            .await?;
        notify_revoked(&state, &out.1);
        Ok(out.0)
    }
    .await;
    no_store(finish(&state, result))
}

/// `GET /v1/shell/agent/events` — the agent machine's control stream (SDS §11.6 `shell.request`).
///
/// Held by agentd with the agent mailbox's own credential, so a machine enrolled with a device
/// credential (which has no account stream) is reachable, and a request for one agent reaches
/// only the machine holding that agent's credential. Holding it is what "connected" means. A newer
/// stream for the same agent replaces an older one.
pub(crate) async fn agent_events(State(state): State<AppState>, headers: HeaderMap) -> Response {
    let me = match agent_ctx(&state, &headers).await {
        Ok(me) => me,
        Err(e) => return e.into_response(),
    };
    let Ok((ca, broker)) = state.shell.available() else {
        return GErr::new(
            StatusCode::SERVICE_UNAVAILABLE,
            "shell_unavailable",
            "agent SSH is not configured on this server",
        )
        .into_response();
    };
    let hello = json!({ "agent": me.address, "ca_public_key": ca.public, "ca_fp": ca.fp,
                        "broker_url": broker, "idle_secs": IDLE_SECS, "max_secs": MAX_SESSION_SECS });
    let (tx, mut rx) = tokio::sync::mpsc::channel::<(&'static str, Value)>(64);
    let conn = state.shell.next_conn.fetch_add(1, Ordering::Relaxed) + 1;
    state
        .shell
        .agents
        .lock()
        .expect("shell agents lock")
        .insert(me.address.clone(), AgentLink { conn, tx });
    tracing::info!(conn, "agent shell control stream opened");
    struct Unregister {
        state: AppState,
        agent: String,
        conn: u64,
    }
    impl Drop for Unregister {
        fn drop(&mut self) {
            let mut agents = self.state.shell.agents.lock().expect("shell agents lock");
            if agents.get(&self.agent).is_some_and(|l| l.conn == self.conn) {
                agents.remove(&self.agent);
            }
        }
    }
    let guard = Unregister {
        state: state.clone(),
        agent: me.address.clone(),
        conn,
    };
    let stream = async_stream::stream! {
        let _guard = guard;
        yield Ok::<Event, std::convert::Infallible>(Event::default().event("shell.hello").data(hello.to_string()));
        while let Some((kind, data)) = rx.recv().await {
            yield Ok(Event::default().event(kind).data(data.to_string()));
        }
    };
    let mut resp = Sse::new(stream)
        .keep_alive(KeepAlive::new().interval(std::time::Duration::from_secs(15)))
        .into_response();
    resp.headers_mut()
        .insert(header::CACHE_CONTROL, HeaderValue::from_static("no-store"));
    resp
}

// ---- the person's side -------------------------------------------------------------------------

/// `GET /v1/shell/ca` — the CA's public key, for anyone who wants to pin or check it.
pub(crate) async fn get_ca(State(state): State<AppState>) -> Response {
    match state.shell.ca_public() {
        Some((public, fp)) => {
            Json(json!({ "public_key": public, "fingerprint": fp })).into_response()
        }
        None => GErr::new(
            StatusCode::SERVICE_UNAVAILABLE,
            "shell_unavailable",
            "agent SSH is not configured on this server",
        )
        .into_response(),
    }
}

/// `GET /v1/agents/{address}/shell` — status for a person related to the agent.
pub(crate) async fn agent_status(
    State(state): State<AppState>,
    headers: HeaderMap,
    Path(address): Path<String>,
) -> Response {
    let result = async {
        let p = person(&state, &headers).await?;
        let account = p.account.clone();
        let now = now_unix();
        let (agent, view) = state
            .store
            .group_read(move |c| -> GResult<(String, Value)> {
                let (agent, agent_account) = resolve_agent(c, &address)?;
                let grant = grant_in(c, &account, &agent, agent_account.as_deref(), now);
                let supervisor = supervisor_in(c, &account, &agent, agent_account.as_deref())?;
                // Someone with neither relation learns nothing, not even that the agent exists.
                let delegated_but_off = matches!(&grant, Err(e) if matches!(e.code, "ssh_not_allowed" | "ssh_not_enabled"));
                if grant.is_err() && !supervisor && !delegated_but_off {
                    return Err(not_an_agent());
                }
                let host = load_host(c, &agent)?;
                let org = org_of_agent(c, &agent)?;
                let (org_json, requires_approval) = match &org {
                    Some((org_id, _)) => {
                        let o = org::load_org(c, org_id)?.ok_or_else(not_an_agent)?;
                        (json!({ "id": org_id, "ssh_allowed": o.settings.ssh_allowed,
                                 "ssh_requires_approval": o.settings.ssh_requires_approval,
                                 "agent_enabled": agent_switch_on(c, &agent)? }),
                         o.settings.ssh_requires_approval)
                    }
                    None => (Value::Null, false),
                };
                let live: i64 = c.query_row(
                    "SELECT COUNT(*) FROM ssh_sessions WHERE agent = ?1 AND state IN ('issued','connected')",
                    [&agent],
                    |r| r.get(0),
                )?;
                Ok((agent.clone(), json!({
                    "agent": agent, "handle": handle_of(c, &agent)?,
                    "enabled": host.as_ref().is_some_and(|h| h.enabled),
                    "mode": host.as_ref().map(|h| h.mode.clone()),
                    "max_sessions": host.as_ref().map(|h| h.max_sessions),
                    "host_key": host.as_ref().map(|h| h.host_key.clone()),
                    "host_key_fp": host.as_ref().map(|h| h.host_fp.clone()),
                    "pending_host_key_fp": host.as_ref().and_then(|h| h.pending_fp.clone()),
                    "reported_at": host.as_ref().map(|h| h.updated_at),
                    "org": org_json, "requires_approval": requires_approval, "live_sessions": live,
                    "can_open": grant.as_ref().is_ok(),
                    "via": grant.as_ref().ok().map(|g| g.via),
                    "refusal": grant.as_ref().err().map(|e| e.code),
                    "can_supervise": supervisor,
                })))
            })
            .await?;
        decide(&state, &p, Action::ShellView, &agent, true)?;
        let mut view = view;
        view["connected"] = json!(state.shell.connected(&agent));
        Ok(Outcome::ok(view))
    }
    .await;
    no_store(finish(&state, result))
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct OpenReq {
    agent: String,
    client_public_key: String,
    #[serde(default = "default_cols")]
    cols: i64,
    #[serde(default = "default_rows")]
    rows: i64,
}

fn default_cols() -> i64 {
    80
}

fn default_rows() -> i64 {
    24
}

/// `POST /v1/shell/sessions` — request a session (SDS §11.6).
pub(crate) async fn open(
    State(state): State<AppState>,
    Extension(cip): Extension<ClientIp>,
    headers: HeaderMap,
    body: Result<Json<Value>, JsonRejection>,
) -> Response {
    let result = async {
        let p = person(&state, &headers).await?;
        require_step_up(&state, &p)?;
        state.shell.available()?;
        let (req, _): (OpenReq, String) = parse(body)?;
        let client = parse_ed25519(&req.client_public_key)?;
        if !(20..=500).contains(&req.cols) || !(5..=300).contains(&req.rows) {
            return Err(GErr::bad("bad_request", "cols is 20-500 and rows 5-300"));
        }
        rate_limit(&format!("shell:{}", p.account), REQUESTS_PER_HOUR, 3_600)?;
        let account = p.account.clone();
        let raw = req.agent.clone();
        let now = now_unix();
        // Decide first, from server state, then issue in a second transaction that decides again.
        let (agent, grant) = state
            .store
            .group_read(move |c| -> GResult<(String, GResult<Grant>)> {
                let (agent, agent_account) = resolve_agent(c, &raw)?;
                let g = grant_in(c, &account, &agent, agent_account.as_deref(), now);
                Ok((agent, g))
            })
            .await
            .map_err(|e| if e.code == "no_agent" { not_an_agent() } else { e })?;
        let grant = match grant {
            Ok(g) => g,
            // Refusals for org agents carry their reason (the person is a member); for anything
            // else the agent is indistinguishable from one that does not exist.
            Err(e) if matches!(e.code, "forbidden" | "not_org_member") => return Err(not_an_agent()),
            Err(e) => return Err(e),
        };
        decide(&state, &p, Action::ShellOpen, &agent, true)?;
        if !state.shell.connected(&agent) {
            return Err(GErr::conflict("agent_offline", "the agent's machine is not connected"));
        }
        let st = state.clone();
        let ip = Some(cip.as_str().to_string());
        let account = p.account.clone();
        let client_line = openssh(&client);
        let client_fp = fingerprint(&client);
        let issued = state
            .store
            .group_tx(move |tx| -> GResult<(Outcome, Option<Value>)> {
                let agent_account: Option<String> = tx
                    .query_row("SELECT account_id FROM identities WHERE address = ?1", [&agent], |r| r.get(0))
                    .optional()?
                    .flatten();
                let again = grant_in(tx, &account, &agent, agent_account.as_deref(), now)?;
                if again != grant {
                    return Err(GErr::conflict("authority_changed", "your access changed; retry"));
                }
                let host = load_host(tx, &agent)?
                    .filter(|h| h.enabled)
                    .ok_or_else(|| GErr::conflict("shell_not_offered", "the agent's machine has not enabled SSH"))?;
                if host.pending_fp.is_some() {
                    return Err(GErr::conflict(
                        "host_key_pending",
                        "the agent's machine presented a new host key; someone with authority over the agent must approve it first",
                    )
                    .with("host_key_fp", host.host_fp.clone())
                    .with("pending_host_key_fp", host.pending_fp.clone()));
                }
                let live: i64 = tx.query_row(
                    "SELECT COUNT(*) FROM ssh_sessions WHERE agent = ?1 AND state IN ('approved','issued','connected')",
                    [&agent],
                    |r| r.get(0),
                )?;
                if live >= host.max_sessions.min(MAX_LIVE_PER_AGENT) {
                    return Err(GErr::conflict("agent_busy", "the agent already has as many sessions as its machine allows"));
                }
                let sid = new_session_id();
                let n = now as i64;
                let state_name = if grant.requires_approval { "awaiting_approval" } else { "approved" };
                tx.execute(
                    "INSERT INTO ssh_sessions (id, agent, org_id, requester_account, requester_member, via, delegation_id,
                            delegation_revision, member_revision, mode, state, client_key, client_fp, host_fp, cols, rows,
                            created_at, approved_at, ends_by)
                     VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?14, ?15, ?16, ?17, ?18, ?19)",
                    params![sid, agent, grant.org_id, account, grant.member, grant.via, grant.delegation,
                            grant.delegation_rev, grant.member_rev, host.mode, state_name, client_line, client_fp,
                            host.host_fp, req.cols, req.rows, n,
                            (!grant.requires_approval).then_some(n), n + MAX_SESSION_SECS as i64],
                )?;
                audit_in(tx, &st, grant.org_id.as_deref(), ActorKind::Human, Some(&account), "ssh.session.request",
                    &agent, json!({ "session_id": sid, "via": grant.via, "mode": host.mode,
                                    "approval": grant.requires_approval }), ip.as_deref(), now)?;
                let s = load_session(tx, &sid)?.ok_or_else(no_session)?;
                if grant.requires_approval {
                    let mut out = Outcome::default();
                    out.status = 202;
                    out.body = json!({ "session_id": sid, "state": "awaiting_approval",
                                       "session": session_json(tx, &s)? });
                    return Ok((out, None));
                }
                let (body, event) = issue_in(tx, &st, &s, &account, ip.as_deref(), now)?;
                let mut out = Outcome::default();
                out.status = 201;
                out.body = body;
                Ok((out, Some(event)))
            })
            .await?;
        if let Some(event) = issued.1 {
            deliver_request(&state, event);
        }
        Ok(issued.0)
    }
    .await;
    no_store(finish(&state, result))
}

/// Issue credentials for an approved session: the certificate, the two leg tokens, and the
/// `shell.request` for the agent's machine (returned for delivery after commit).
fn issue_in(
    tx: &Connection,
    state: &AppState,
    s: &SessionRow,
    account: &str,
    ip: Option<&str>,
    now: u64,
) -> GResult<(Value, Value)> {
    let (ca, broker) = state.shell.available()?;
    let host = load_host(tx, &s.agent)?
        .filter(|h| h.enabled && h.host_fp == s.host_fp && h.pending_fp.is_none())
        .ok_or_else(|| {
            GErr::conflict(
                "host_key_changed",
                "the agent's host key changed; request again",
            )
        })?;
    let client = parse_ed25519(&s.client_key)?;
    let grant = json!({
        "v": 1, "sid": s.id, "agent": s.agent, "org": s.org_id, "account": s.requester,
        "member": s.member, "via": s.via, "dlg": s.delegation, "dlg_rev": s.delegation_rev,
        "host_fp": s.host_fp, "client_fp": s.client_fp, "mode": s.mode, "ends_by": s.ends_by,
        "channels": ["session"],
    });
    let (cert, serial) = sign_cert(ca, &client, &s.id, &grant, now)?;
    let (client_leg, client_hash) = new_leg();
    let (agent_leg, agent_hash) = new_leg();
    let n = now as i64;
    let connect_by = n + CONNECT_SECS as i64;
    tx.execute(
        "UPDATE ssh_sessions SET state = 'issued', cert_serial = ?2, client_leg_hash = ?3, agent_leg_hash = ?4,
                issued_at = ?5, connect_by = ?6 WHERE id = ?1 AND state = 'approved'",
        params![s.id, serial, client_hash, agent_hash, n, connect_by],
    )?;
    audit_in(
        tx,
        state,
        s.org_id.as_deref(),
        ActorKind::Human,
        Some(account),
        "ssh.session.open",
        &s.agent,
        json!({ "session_id": s.id, "via": s.via, "mode": s.mode, "delegation": s.delegation,
                "client_key_fp": s.client_fp, "host_key_fp": s.host_fp, "cert_serial": serial }),
        ip,
        now,
    )?;
    let s2 = load_session(tx, &s.id)?.ok_or_else(no_session)?;
    let url = format!("{broker}/t/{}", s.id);
    let requester_handle = match (&s.org_id, &s.member) {
        (Some(org), Some(m)) => org::load_member(tx, org, m)?.and_then(|m| m.handle),
        _ => None,
    };
    let body = json!({
        "session_id": s.id, "state": "issued", "broker_url": url, "leg_token": client_leg,
        "host_key": host.host_key, "host_key_fp": host.host_fp, "user_cert": cert,
        "ssh_user": SSH_USER, "principal": principal_for(&s.id), "ca_fp": ca.fp,
        "connect_by": connect_by, "idle_secs": IDLE_SECS, "ends_by": s.ends_by,
        "session": session_json(tx, &s2)?,
    });
    let event = json!({
        "session_id": s.id, "agent": s.agent, "leg_token": agent_leg, "broker_url": url,
        "requester": { "account": s.requester, "handle": requester_handle }, "via": s.via,
        "mode": s.mode, "client_key_fp": s.client_fp, "cert_serial": serial, "ca_fp": ca.fp,
        "principal": principal_for(&s.id), "connect_by": connect_by, "ends_by": s.ends_by,
        "idle_secs": IDLE_SECS, "cols": s.cols, "rows": s.rows,
    });
    Ok((body, event))
}

fn deliver_request(state: &AppState, event: Value) {
    let agent = event["agent"].as_str().unwrap_or_default().to_string();
    if !state.shell.send(&agent, "shell.request", event) {
        // The machine dropped between the check and now; the session expires at connect_by.
        tracing::warn!("shell.request not delivered; the agent's control stream is gone");
    }
}

/// `GET /v1/shell/sessions/{id}` — for the requester and the agent's supervisors.
pub(crate) async fn get_session(
    State(state): State<AppState>,
    headers: HeaderMap,
    Path(id): Path<String>,
) -> Response {
    let result = async {
        let p = person(&state, &headers).await?;
        let account = p.account.clone();
        let (agent, body) = state
            .store
            .group_read(move |c| -> GResult<(String, Value)> {
                let s = load_session(c, &id)?.ok_or_else(no_session)?;
                if s.requester != account && !supervisor_of_session(c, &account, &s)? {
                    return Err(no_session());
                }
                Ok((s.agent.clone(), json!({ "session": session_json(c, &s)? })))
            })
            .await?;
        decide(&state, &p, Action::ShellView, &agent, true)?;
        Ok(Outcome::ok(body))
    }
    .await;
    no_store(finish(&state, result))
}

fn supervisor_of_session(c: &Connection, account: &str, s: &SessionRow) -> GResult<bool> {
    let agent_account: Option<String> = c
        .query_row(
            "SELECT account_id FROM identities WHERE address = ?1",
            [&s.agent],
            |r| r.get(0),
        )
        .optional()?
        .flatten();
    supervisor_in(c, account, &s.agent, agent_account.as_deref())
}

#[derive(Deserialize)]
pub(crate) struct ListQuery {
    #[serde(default)]
    state: Option<String>,
}

/// `GET /v1/shell/sessions` — my requests, and sessions on agents I supervise.
pub(crate) async fn list_sessions(
    State(state): State<AppState>,
    headers: HeaderMap,
    Query(q): Query<ListQuery>,
) -> Response {
    let result = async {
        let p = person(&state, &headers).await?;
        let account = p.account.clone();
        let filter = q.state.filter(|s| {
            matches!(
                s.as_str(),
                "awaiting_approval" | "approved" | "issued" | "connected" | "closed"
            )
        });
        let sessions = state
            .store
            .group_read(move |c| -> GResult<Vec<Value>> {
                let mut stmt = c.prepare(&format!(
                    "SELECT {SESSION_COLS} FROM ssh_sessions s
                      WHERE (?2 IS NULL OR s.state = ?2) AND (
                            s.requester_account = ?1
                         OR (s.org_id IS NULL AND s.agent IN (SELECT address FROM identities WHERE account_id = ?1))
                         OR (s.org_id IS NOT NULL AND EXISTS (
                                SELECT 1 FROM org_members m JOIN org_roles r ON r.org_id = m.org_id AND r.member_id = m.id
                                 WHERE m.org_id = s.org_id AND m.account_id = ?1 AND m.status = 'active'
                                   AND r.role IN ('owner','admin','agent_admin'))))
                      ORDER BY s.created_at DESC LIMIT 100"
                ))?;
                let rows: Vec<SessionRow> = stmt
                    .query_map(params![account, filter], map_session)?
                    .collect::<Result<_, _>>()?;
                rows.iter().map(|s| session_json(c, s)).collect()
            })
            .await?;
        Ok(Outcome::ok(json!({ "sessions": sessions })))
    }
    .await;
    no_store(finish(&state, result))
}

/// `POST /v1/shell/sessions/{id}/approve` and `/deny` — a supervisor, with step-up, never the
/// requester (SDS §11.5 item 5).
pub(crate) async fn approve(
    state: State<AppState>,
    cip: Extension<ClientIp>,
    headers: HeaderMap,
    id: Path<String>,
) -> Response {
    review(state, cip, headers, id, true).await
}

pub(crate) async fn deny(
    state: State<AppState>,
    cip: Extension<ClientIp>,
    headers: HeaderMap,
    id: Path<String>,
) -> Response {
    review(state, cip, headers, id, false).await
}

async fn review(
    State(state): State<AppState>,
    Extension(cip): Extension<ClientIp>,
    headers: HeaderMap,
    Path(id): Path<String>,
    approve: bool,
) -> Response {
    let result = async {
        let p = person(&state, &headers).await?;
        require_step_up(&state, &p)?;
        let account = p.account.clone();
        let sid = id.clone();
        let (agent, may) = state
            .store
            .group_read(move |c| -> GResult<(String, bool)> {
                let s = load_session(c, &sid)?.ok_or_else(no_session)?;
                let sup = supervisor_of_session(c, &account, &s)?;
                if !sup && s.requester != account {
                    return Err(no_session());
                }
                Ok((s.agent.clone(), sup && s.requester != account))
            })
            .await?;
        decide(&state, &p, Action::ShellApprove, &agent, may)?;
        let st = state.clone();
        let account = p.account.clone();
        let ip = Some(cip.as_str().to_string());
        let now = now_unix();
        state
            .store
            .group_tx(move |tx| -> GResult<Outcome> {
                let s = load_session(tx, &id)?.ok_or_else(no_session)?;
                if s.state != "awaiting_approval" {
                    return Err(GErr::conflict("not_pending", "this session is not waiting for approval"));
                }
                if approve {
                    tx.execute(
                        "UPDATE ssh_sessions SET state = 'approved', approved_by = ?2, approved_at = ?3 WHERE id = ?1",
                        params![s.id, account, now as i64],
                    )?;
                    audit_in(tx, &st, s.org_id.as_deref(), ActorKind::Human, Some(&account), "ssh.session.approve",
                        &s.agent, json!({ "session_id": s.id, "requester": s.requester }), ip.as_deref(), now)?;
                } else {
                    close_in(tx, &st, &s, "denied", Some(&account), ActorKind::Human, None, now)?;
                }
                let s = load_session(tx, &s.id)?.ok_or_else(no_session)?;
                Ok(Outcome::ok(json!({ "session": session_json(tx, &s)? })))
            })
            .await
    }
    .await;
    no_store(finish(&state, result))
}

/// `POST /v1/shell/sessions/{id}/connect` — the requester collects credentials for an approved
/// session, with a fresh step-up and the grant decided again.
pub(crate) async fn connect(
    State(state): State<AppState>,
    Extension(cip): Extension<ClientIp>,
    headers: HeaderMap,
    Path(id): Path<String>,
) -> Response {
    let result = async {
        let p = person(&state, &headers).await?;
        require_step_up(&state, &p)?;
        state.shell.available()?;
        let account = p.account.clone();
        let now = now_unix();
        let sid = id.clone();
        let agent = state
            .store
            .group_read(move |c| -> GResult<String> {
                let s = load_session(c, &sid)?.ok_or_else(no_session)?;
                if s.requester != account {
                    return Err(no_session());
                }
                Ok(s.agent)
            })
            .await?;
        decide(&state, &p, Action::ShellOpen, &agent, true)?;
        if !state.shell.connected(&agent) {
            return Err(GErr::conflict(
                "agent_offline",
                "the agent's machine is not connected",
            ));
        }
        let st = state.clone();
        let account = p.account.clone();
        let ip = Some(cip.as_str().to_string());
        let (out, event) = state
            .store
            .group_tx(move |tx| -> GResult<(Outcome, Value)> {
                let s = load_session(tx, &id)?.ok_or_else(no_session)?;
                if s.requester != account {
                    return Err(no_session());
                }
                if s.state != "approved" {
                    return Err(GErr::conflict(
                        "not_approved",
                        "this session is not approved, or its credentials were already issued",
                    )
                    .with("state", s.state.clone()));
                }
                if let Some(reason) = due_reason(tx, &s, now)? {
                    close_in(tx, &st, &s, reason, None, ActorKind::System, None, now)?;
                    return Err(GErr::conflict(
                        "session_ended",
                        "this session can no longer be opened",
                    )
                    .with("reason", reason));
                }
                let (body, event) = issue_in(tx, &st, &s, &account, ip.as_deref(), now)?;
                let mut out = Outcome::default();
                out.status = 201;
                out.body = body;
                Ok((out, event))
            })
            .await?;
        deliver_request(&state, event);
        Ok(out)
    }
    .await;
    no_store(finish(&state, result))
}

/// `DELETE /v1/shell/sessions/{id}` — the requester ends their session, or a supervisor kills it.
pub(crate) async fn close(
    State(state): State<AppState>,
    Extension(cip): Extension<ClientIp>,
    headers: HeaderMap,
    Path(id): Path<String>,
) -> Response {
    let result = async {
        let p = person(&state, &headers).await?;
        let account = p.account.clone();
        let sid = id.clone();
        let (agent, may) = state
            .store
            .group_read(move |c| -> GResult<(String, bool)> {
                let s = load_session(c, &sid)?.ok_or_else(no_session)?;
                let may = s.requester == account || supervisor_of_session(c, &account, &s)?;
                if !may {
                    return Err(no_session());
                }
                Ok((s.agent, may))
            })
            .await?;
        decide(&state, &p, Action::ShellView, &agent, may)?;
        let st = state.clone();
        let account = p.account.clone();
        let _ = cip;
        let now = now_unix();
        let (out, ended) = state
            .store
            .group_tx(
                move |tx| -> GResult<(Outcome, Vec<(String, String, String)>)> {
                    let s = load_session(tx, &id)?.ok_or_else(no_session)?;
                    let reason = if s.requester == account {
                        "closed_by_requester"
                    } else {
                        "killed"
                    };
                    let mut ended = Vec::new();
                    if close_in(
                        tx,
                        &st,
                        &s,
                        reason,
                        Some(&account),
                        ActorKind::Human,
                        None,
                        now,
                    )? {
                        ended.push((s.id.clone(), s.agent.clone(), reason.to_string()));
                    }
                    let s = load_session(tx, &s.id)?.ok_or_else(no_session)?;
                    Ok((
                        Outcome::ok(json!({ "session": session_json(tx, &s)? })),
                        ended,
                    ))
                },
            )
            .await?;
        notify_revoked(&state, &ended);
        Ok(out)
    }
    .await;
    no_store(finish(&state, result))
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct RotateReq {
    fingerprint: String,
}

/// `POST /v1/agents/{address}/shell/host-key` — approve the pending host key (explicit,
/// authenticated rotation, SDS §11.3). Ends the agent's open sessions.
pub(crate) async fn approve_host_key(
    State(state): State<AppState>,
    Extension(cip): Extension<ClientIp>,
    headers: HeaderMap,
    Path(address): Path<String>,
    body: Result<Json<Value>, JsonRejection>,
) -> Response {
    let result = async {
        let p = person(&state, &headers).await?;
        require_step_up(&state, &p)?;
        let (req, _): (RotateReq, String) = parse(body)?;
        let account = p.account.clone();
        let raw = address.clone();
        let (agent, sup) = state
            .store
            .group_read(move |c| -> GResult<(String, bool)> {
                let (agent, agent_account) = resolve_agent(c, &raw)?;
                Ok((agent.clone(), supervisor_in(c, &account, &agent, agent_account.as_deref())?))
            })
            .await?;
        if !sup {
            return Err(not_an_agent());
        }
        decide(&state, &p, Action::ShellApprove, &agent, sup)?;
        let st = state.clone();
        let account = p.account.clone();
        let ip = Some(cip.as_str().to_string());
        let now = now_unix();
        let (out, ended) = state
            .store
            .group_tx(move |tx| -> GResult<(Outcome, Vec<(String, String, String)>)> {
                let h = load_host(tx, &agent)?.ok_or_else(not_an_agent)?;
                if h.pending_fp.as_deref() != Some(req.fingerprint.trim()) {
                    return Err(GErr::conflict("fingerprint_mismatch", "that is not the pending host key's fingerprint")
                        .with("pending_host_key_fp", h.pending_fp.clone()));
                }
                tx.execute(
                    "UPDATE agent_shell_hosts SET host_key = pending_key, host_fp = pending_fp, pending_key = NULL,
                            pending_fp = NULL, pending_at = NULL, rotated_at = ?2, rotated_by = ?3 WHERE identity = ?1",
                    params![agent, now as i64, account],
                )?;
                audit_in(tx, &st, org_of_agent(tx, &agent)?.map(|o| o.0).as_deref(), ActorKind::Human, Some(&account),
                    "ssh.host_key.rotate", &agent, json!({ "from": h.host_fp, "to": req.fingerprint.trim() }),
                    ip.as_deref(), now)?;
                let mut stmt = tx.prepare(&format!(
                    "SELECT {SESSION_COLS} FROM ssh_sessions WHERE agent = ?1 AND state <> 'closed'"
                ))?;
                let open: Vec<SessionRow> = stmt.query_map([&agent], map_session)?.collect::<Result<_, _>>()?;
                drop(stmt);
                let mut ended = Vec::new();
                for s in open {
                    if close_in(tx, &st, &s, "host_key_rotated", Some(&account), ActorKind::Human, None, now)? {
                        ended.push((s.id.clone(), s.agent.clone(), "host_key_rotated".to_string()));
                    }
                }
                let h = load_host(tx, &agent)?.ok_or_else(not_an_agent)?;
                Ok((Outcome::ok(json!({ "host_key_fp": h.host_fp, "host_key": h.host_key })), ended))
            })
            .await?;
        notify_revoked(&state, &ended);
        Ok(out)
    }
    .await;
    no_store(finish(&state, result))
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct SwitchReq {
    enabled: bool,
}

/// `PUT /v1/orgs/{id}/agents/{address}/shell` — the agent admin's per-agent switch (SDS §6.5).
/// Turning it on needs step-up; turning it off ends the agent's sessions at once.
pub(crate) async fn org_agent_switch(
    State(state): State<AppState>,
    Extension(cip): Extension<ClientIp>,
    headers: HeaderMap,
    Path((org_id, address)): Path<(String, String)>,
    body: Result<Json<Value>, JsonRejection>,
) -> Response {
    let result = async {
        let ctx = org::org_ctx(&state, &headers, &org_id).await?;
        ctx.require(&state, Perm::ManageAgents)?;
        ctx.require_writable()?;
        if !feature_on(&state, Some(&ctx.account), capabilities::AGENT_SSH).await {
            return Err(GErr::new(StatusCode::NOT_FOUND, "feature_disabled", "agent SSH is not enabled for this account"));
        }
        let (req, _): (SwitchReq, String) = parse(body)?;
        if req.enabled {
            ctx.step_up(&state)?;
        }
        let st = state.clone();
        let account = ctx.account.clone();
        let ip = Some(cip.as_str().to_string());
        let now = now_unix();
        let oid = ctx.org.id.clone();
        let (out, ended) = state
            .store
            .group_tx(move |tx| -> GResult<(Outcome, Vec<(String, String, String)>)> {
                let agent = crate::org_agents::agent_in_org(tx, &oid, &address)?;
                tx.execute(
                    "INSERT INTO agent_shell_policy (identity, org_id, enabled, updated_by, updated_at)
                     VALUES (?1, ?2, ?3, ?4, ?5)
                     ON CONFLICT(identity) DO UPDATE SET enabled = excluded.enabled, org_id = excluded.org_id,
                        updated_by = excluded.updated_by, updated_at = excluded.updated_at",
                    params![agent, oid, req.enabled as i64, account, now as i64],
                )?;
                org::audit_in(tx, &st.auditor, &oid, ActorKind::Human, Some(&account),
                    if req.enabled { "ssh.agent.enable" } else { "ssh.agent.disable" }, Some(&agent),
                    json!({}), ip.as_deref(), now)?;
                let mut ended = Vec::new();
                if !req.enabled {
                    let mut stmt = tx.prepare(&format!(
                        "SELECT {SESSION_COLS} FROM ssh_sessions WHERE agent = ?1 AND state <> 'closed'"
                    ))?;
                    let open: Vec<SessionRow> = stmt.query_map([&agent], map_session)?.collect::<Result<_, _>>()?;
                    drop(stmt);
                    for s in open {
                        if close_in(tx, &st, &s, "ssh_not_enabled", Some(&account), ActorKind::Human, None, now)? {
                            ended.push((s.id.clone(), s.agent.clone(), "ssh_not_enabled".to_string()));
                        }
                    }
                }
                Ok((Outcome::ok(json!({ "agent": agent, "ssh_enabled": req.enabled })), ended))
            })
            .await?;
        notify_revoked(&state, &ended);
        Ok(out)
    }
    .await;
    no_store(finish(&state, result))
}

// ---- the tunnel broker (internal) --------------------------------------------------------------

fn broker_ok(state: &AppState, headers: &HeaderMap) -> Result<(), Box<Response>> {
    crate::internal_api::authorize_service(
        state,
        headers,
        crate::internal_api::Endpoint::ShellBroker,
        Action::ShellBroker,
    )
    .map(|_| ())
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct LegReq {
    session_id: String,
    leg_token: String,
}

/// `POST /internal/v1/shell/legs` — the broker presents a leg token it was handed; the postbox
/// says which side it is and spends it. One use, both legs before `connect_by`.
pub(crate) async fn broker_leg(
    State(state): State<AppState>,
    headers: HeaderMap,
    body: Result<Json<Value>, JsonRejection>,
) -> Response {
    if let Err(resp) = broker_ok(&state, &headers) {
        return *resp;
    }
    let result = async {
        let (req, _): (LegReq, String) = parse(body)?;
        let presented = crate::sha256(req.leg_token.as_bytes());
        let now = now_unix();
        state
            .store
            .group_tx(move |tx| -> GResult<Outcome> {
                let invalid = || GErr::forbidden("leg_invalid", "that leg token is not valid");
                let s = load_session(tx, &req.session_id)?.ok_or_else(invalid)?;
                if s.state != "issued" || s.connect_by.is_none_or(|t| t <= now as i64) {
                    return Err(invalid());
                }
                let matches = |h: &Option<Vec<u8>>| {
                    h.as_deref()
                        .and_then(|h| <&[u8; 32]>::try_from(h).ok())
                        .is_some_and(|h| crate::constant_time_eq(h, &presented))
                };
                let (side, column, used) = if matches(&s.client_leg_hash) {
                    ("client", "client_leg_at", s.client_leg_at)
                } else if matches(&s.agent_leg_hash) {
                    ("agent", "agent_leg_at", s.agent_leg_at)
                } else {
                    return Err(invalid());
                };
                if used.is_some() {
                    return Err(invalid());
                }
                tx.execute(
                    &format!("UPDATE ssh_sessions SET {column} = ?2 WHERE id = ?1"),
                    params![s.id, now as i64],
                )?;
                Ok(Outcome::ok(json!({
                    "session_id": s.id, "side": side, "ends_by": s.ends_by,
                    "connect_by": s.connect_by, "idle_secs": IDLE_SECS,
                })))
            })
            .await
    }
    .await;
    no_store(finish(&state, result))
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct CheckItem {
    session_id: String,
    #[serde(default)]
    bytes_client: i64,
    #[serde(default)]
    bytes_agent: i64,
    #[serde(default)]
    spliced: bool,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct CheckReq {
    sessions: Vec<CheckItem>,
}

/// `POST /internal/v1/shell/check` — the broker's heartbeat: byte counts for the sessions it
/// carries, and in return the ones it must end now (revoked, expired, unknown).
pub(crate) async fn broker_check(
    State(state): State<AppState>,
    headers: HeaderMap,
    body: Result<Json<Value>, JsonRejection>,
) -> Response {
    if let Err(resp) = broker_ok(&state, &headers) {
        return *resp;
    }
    let result = async {
        let (req, _): (CheckReq, String) = parse(body)?;
        if req.sessions.len() > 1_000 {
            return Err(GErr::bad("bad_request", "at most 1000 sessions per check"));
        }
        sweep(&state).await;
        let now = now_unix();
        let st = state.clone();
        let (out, ended) = state
            .store
            .group_tx(move |tx| -> GResult<(Outcome, Vec<(String, String, String)>)> {
                let mut end = Vec::new();
                let mut ended = Vec::new();
                for item in &req.sessions {
                    let Some(s) = load_session(tx, &item.session_id)? else {
                        end.push(json!({ "session_id": item.session_id, "reason": "unknown" }));
                        continue;
                    };
                    if s.state == "closed" {
                        end.push(json!({ "session_id": s.id, "reason": s.close_reason }));
                        continue;
                    }
                    let n = now as i64;
                    let (bc, ba) = (item.bytes_client.max(s.bytes_client), item.bytes_agent.max(s.bytes_agent));
                    tx.execute(
                        "UPDATE ssh_sessions SET bytes_client = ?2, bytes_agent = ?3, last_seen_at = ?4 WHERE id = ?1",
                        params![s.id, bc, ba, n],
                    )?;
                    if item.spliced && s.state == "issued" && s.client_leg_at.is_some() && s.agent_leg_at.is_some() {
                        tx.execute(
                            "UPDATE ssh_sessions SET state = 'connected', connected_at = ?2 WHERE id = ?1",
                            params![s.id, n],
                        )?;
                    }
                    if !matches!(s.state.as_str(), "issued" | "connected") {
                        let s = load_session(tx, &s.id)?.ok_or_else(no_session)?;
                        if close_in(tx, &st, &s, "not_issued", None, ActorKind::System, None, now)? {
                            ended.push((s.id.clone(), s.agent.clone(), "not_issued".into()));
                        }
                        end.push(json!({ "session_id": s.id, "reason": "not_issued" }));
                    }
                }
                Ok((Outcome::ok(json!({ "end": end, "server_time": now })), ended))
            })
            .await?;
        notify_revoked(&state, &ended);
        Ok(out)
    }
    .await;
    no_store(finish(&state, result))
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct ClosedReq {
    session_id: String,
    reason: String,
    #[serde(default)]
    bytes_client: i64,
    #[serde(default)]
    bytes_agent: i64,
}

/// `POST /internal/v1/shell/closed` — the broker saw a session end (a side hung up, idle, its own
/// limits).
pub(crate) async fn broker_closed(
    State(state): State<AppState>,
    headers: HeaderMap,
    body: Result<Json<Value>, JsonRejection>,
) -> Response {
    if let Err(resp) = broker_ok(&state, &headers) {
        return *resp;
    }
    let result = async {
        let (req, _): (ClosedReq, String) = parse(body)?;
        let reason: String = req
            .reason
            .chars()
            .filter(|c| c.is_ascii_lowercase() || *c == '_')
            .take(40)
            .collect();
        let reason = if reason.is_empty() {
            "tunnel_closed".to_string()
        } else {
            format!("tunnel_{reason}")
        };
        let now = now_unix();
        let st = state.clone();
        let (out, ended) = state
            .store
            .group_tx(
                move |tx| -> GResult<(Outcome, Vec<(String, String, String)>)> {
                    let s = load_session(tx, &req.session_id)?.ok_or_else(no_session)?;
                    let mut ended = Vec::new();
                    if close_in(
                        tx,
                        &st,
                        &s,
                        &reason,
                        None,
                        ActorKind::System,
                        Some((req.bytes_client, req.bytes_agent)),
                        now,
                    )? {
                        ended.push((s.id.clone(), s.agent.clone(), reason.clone()));
                    }
                    Ok((Outcome::ok(json!({ "closed": true })), ended))
                },
            )
            .await?;
        notify_revoked(&state, &ended);
        Ok(out)
    }
    .await;
    no_store(finish(&state, result))
}

fn no_store(mut r: Response) -> Response {
    r.headers_mut()
        .insert(header::CACHE_CONTROL, HeaderValue::from_static("no-store"));
    r
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn broker_urls_are_bare_and_secure() {
        assert!(valid_broker_url("wss://tunnel.pigeonpost.dev"));
        assert!(valid_broker_url("wss://tunnel.pigeonpost.dev:8443"));
        assert!(valid_broker_url("ws://127.0.0.1:9000"));
        for bad in [
            "ws://tunnel.pigeonpost.dev",
            "https://tunnel.pigeonpost.dev",
            "wss://tunnel.pigeonpost.dev/t",
            "wss://user@tunnel.pigeonpost.dev",
            "wss://tunnel.pigeonpost.dev?x=1",
            "wss://",
            "tunnel.pigeonpost.dev",
        ] {
            assert!(!valid_broker_url(bad), "{bad}");
        }
    }

    #[test]
    fn ids_and_principals() {
        let id = new_session_id();
        assert!(valid_session_id(&id), "{id}");
        assert!(!valid_session_id("ssh_../../etc"));
        assert_eq!(principal_for(&id), format!("pp-ssh-{id}"));
        let (t, h) = new_leg();
        assert!(t.starts_with("lt_") && t.len() > 40);
        assert_eq!(h, crate::sha256(t.as_bytes()).to_vec());
    }

    #[test]
    fn only_ed25519_keys() {
        let k = ssh_key::PrivateKey::from(ssh_key::private::Ed25519Keypair::from_seed(&[7; 32]));
        let line = k.public_key().to_openssh().unwrap();
        assert!(parse_ed25519(&line).is_ok());
        assert_eq!(
            parse_ed25519("ssh-rsa AAAAB3NzaC1yc2E x").unwrap_err().code,
            "bad_key"
        );
        assert_eq!(parse_ed25519("garbage").unwrap_err().code, "bad_key");
        // The comment never travels.
        let mut with = k.public_key().clone();
        with.set_comment("someone@laptop");
        assert!(!openssh(&with).contains("laptop"));
    }

    #[test]
    fn certificates_bind_the_session_and_expire() {
        let rt = Runtime::for_test([3; 32], "wss://tunnel.example");
        let (ca, _) = rt.available().unwrap();
        let client =
            ssh_key::PrivateKey::from(ssh_key::private::Ed25519Keypair::from_seed(&[9; 32]));
        let now = 1_800_000_000;
        let grant = json!({ "sid": "ssh_x", "agent": "/k/a" });
        let (line, serial) = sign_cert(ca, client.public_key(), "ssh_x", &grant, now).unwrap();
        let cert = ssh_key::Certificate::from_openssh(&line).unwrap();
        let ca_fp = ca.key.public_key().fingerprint(ssh_key::HashAlg::Sha256);
        assert!(cert.validate_at(now, [&ca_fp]).is_ok());
        assert!(
            cert.validate_at(now + CERT_SECS + 1, [&ca_fp]).is_err(),
            "expired"
        );
        let other =
            ssh_key::PrivateKey::from(ssh_key::private::Ed25519Keypair::from_seed(&[4; 32]));
        let other_fp = other.public_key().fingerprint(ssh_key::HashAlg::Sha256);
        assert!(cert.validate_at(now, [&other_fp]).is_err(), "another CA");
        assert_eq!(cert.valid_principals(), ["pp-ssh-ssh_x".to_string()]);
        assert_eq!(cert.cert_type(), ssh_key::certificate::CertType::User);
        assert_eq!(format!("{:016x}", cert.serial()), serial);
        let ext: Vec<_> = cert.extensions().keys().cloned().collect();
        assert_eq!(ext, ["permit-pty".to_string()], "no forwarding of any kind");
        assert!(cert.critical_options().is_empty());
        let id: Value = serde_json::from_str(cert.key_id()).unwrap();
        assert_eq!(id["agent"], "/k/a");
        assert_eq!(cert.public_key(), client.public_key().key_data());
    }
}
