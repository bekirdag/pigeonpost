//! Org agents, delegations and device login with a pre-assigned handle (SDS §6.5, §10;
//! `docs/orgs.md` §4, §8).
//!
//! # Credentials
//!
//! A machine never receives a person's token. It proves possession of an Ed25519 device key (and,
//! for a pairing, a claim secret), and receives a **device credential**: a `cap_…` token bound to
//! one agent mailbox, stored here only as a hash, independently revocable, and derived from one
//! `device_grants` row that records whose authority approved it (a role, or a delegation). Every
//! use re-checks that whole chain ([`credential_live_in`]): the grant, the agent, the org, and the
//! approver's membership at the same `authority_revision` with the same role or live delegation.
//! A role change, offboarding or delegation revocation therefore stops every credential it
//! approved, and re-granting never revives one.

// Row tuples read straight from SQL are clearer inline than as one-use type aliases.
#![allow(clippy::type_complexity)]

use crate::audit::ActorKind;
use crate::groups::{finish, parse, rate_limit, GErr, GResult, Outcome};
use crate::org::{self, load_member, load_org, Perm, Role};
use crate::policy::{authorize, Action, Actor, Facts, Resource};
use crate::{now_unix, store, AppState, ClientIp};
use axum::extract::rejection::JsonRejection;
use axum::extract::{Path, State};
use axum::http::{header, HeaderMap, HeaderValue, StatusCode};
use axum::response::Response;
use axum::routing::{delete, get, patch, post};
use axum::{Extension, Json, Router};
use ed25519_dalek::{Signature, Verifier, VerifyingKey};
use rusqlite::{params, Connection, OptionalExtension};
use serde::Deserialize;
use serde_json::{json, Value};

/// Capabilities a delegation may carry (SDS §6.5). `ssh` is stored for Phase 8 and grants nothing.
pub(crate) const CAPABILITIES: &[&str] = &[
    "enroll",
    "bind_handle",
    "ssh",
    "configure_rules",
    "read_inbox",
];
/// What a device credential may do: be the agent's mailbox.
const DEVICE_CAPS: &str = "[\"mailbox\"]";
/// Device credentials last a year unless their authority expires first.
const CREDENTIAL_SECS: u64 = 365 * 86_400;
/// Pairings live ten minutes (SDS §10.4).
const PAIRING_SECS: u64 = 600;
/// Claim polling interval.
const POLL_SECS: u64 = 5;
/// A credential issued by a claim can be replaced by a retry within this window.
const RECLAIM_SECS: i64 = 600;

pub(crate) fn router() -> Router<AppState> {
    Router::new()
        .route("/v1/orgs/{id}/agents", get(agents).post(create_agent))
        .route(
            "/v1/orgs/{id}/agents/{address}",
            patch(patch_agent).delete(delete_agent),
        )
        .route(
            "/v1/orgs/{id}/agents/{address}/enrollments",
            post(create_enrollment),
        )
        .route("/v1/orgs/{id}/agents/{address}/devices", get(devices))
        .route("/v1/orgs/{id}/devices/{device}", delete(revoke_device))
        .route(
            "/v1/orgs/{id}/delegations",
            get(delegations).post(create_delegation),
        )
        .route(
            "/v1/orgs/{id}/delegations/{delegation}",
            delete(revoke_delegation),
        )
        .route("/v1/device-pairings", post(create_pairing))
        .route("/v1/device-pairings/resolve", post(resolve_pairing))
        .route("/v1/device-pairings/claim", post(claim_pairing))
        .route(
            "/v1/device-pairings/{pairing}/approve",
            post(approve_pairing),
        )
        .route("/v1/device-pairings/{pairing}/deny", post(deny_pairing))
        .route("/v1/enrollments/redeem", post(redeem_enrollment))
}

type Body = Result<Json<Value>, JsonRejection>;

fn ipaddr(e: &ClientIp) -> Option<String> {
    Some(e.as_str().to_string())
}

fn no_store(mut r: Response) -> Response {
    r.headers_mut()
        .insert(header::CACHE_CONTROL, HeaderValue::from_static("no-store"));
    r
}

// ---- revocation helpers (used by offboarding, suspension and org deletion) ---------------------

/// Revoke every device credential, grant, enrollment and pending pairing for one agent, or for
/// the whole org when `identity` is `None`.
pub(crate) fn revoke_all_devices_in(
    c: &Connection,
    org_id: &str,
    identity: Option<&str>,
    now: u64,
) -> GResult<()> {
    let n = now as i64;
    c.execute(
        "UPDATE device_credentials SET revoked_at = ?3 WHERE org_id = ?1 AND (?2 IS NULL OR identity = ?2) AND revoked_at IS NULL",
        params![org_id, identity, n],
    )?;
    c.execute(
        "UPDATE device_grants SET revoked_at = ?3 WHERE org_id = ?1 AND (?2 IS NULL OR identity = ?2) AND revoked_at IS NULL",
        params![org_id, identity, n],
    )?;
    c.execute(
        "UPDATE enrollments SET revoked_at = ?3 WHERE org_id = ?1 AND (?2 IS NULL OR identity = ?2) AND revoked_at IS NULL AND redeemed_at IS NULL",
        params![org_id, identity, n],
    )?;
    c.execute(
        "UPDATE device_pairings SET revoked_at = ?3 WHERE org_id = ?1 AND (?2 IS NULL OR target_identity = ?2) AND revoked_at IS NULL",
        params![org_id, identity, n],
    )?;
    Ok(())
}

/// A departing or suspended member's delegations and every grant they approved stop at once.
pub(crate) fn revoke_member_grants_in(
    c: &Connection,
    org_id: &str,
    member_id: &str,
    now: u64,
) -> GResult<()> {
    let n = now as i64;
    c.execute(
        "UPDATE org_delegations SET revoked_at = ?3, revision = revision + 1 WHERE org_id = ?1 AND grantee_member_id = ?2 AND revoked_at IS NULL",
        params![org_id, member_id, n],
    )?;
    c.execute(
        "UPDATE device_credentials SET revoked_at = ?3 WHERE revoked_at IS NULL AND parent_grant_id IN
            (SELECT id FROM device_grants WHERE org_id = ?1 AND approver_member_id = ?2)",
        params![org_id, member_id, n],
    )?;
    c.execute(
        "UPDATE enrollments SET revoked_at = ?3 WHERE revoked_at IS NULL AND redeemed_at IS NULL AND grant_id IN
            (SELECT id FROM device_grants WHERE org_id = ?1 AND approver_member_id = ?2)",
        params![org_id, member_id, n],
    )?;
    c.execute(
        "UPDATE device_grants SET revoked_at = ?3 WHERE org_id = ?1 AND approver_member_id = ?2 AND revoked_at IS NULL",
        params![org_id, member_id, n],
    )?;
    Ok(())
}

// ---- the authority chain -----------------------------------------------------------------------

/// The authority a member holds to enroll or approve a device for one agent: a role, or a live
/// delegation with `enroll`.
#[derive(Debug, Clone)]
pub(crate) enum Authority {
    Role(Role),
    Delegation(String),
}

pub(crate) fn enroll_authority_in(
    c: &Connection,
    org_id: &str,
    member: &org::MemberRow,
    identity: &str,
    now: u64,
) -> GResult<Option<Authority>> {
    if member.status != "active" {
        return Ok(None);
    }
    let roles = org::roles_of(c, org_id, &member.id)?;
    for r in [Role::Owner, Role::Admin, Role::AgentAdmin] {
        if roles.has(r) {
            return Ok(Some(Authority::Role(r)));
        }
    }
    let dlg: Option<String> = c
        .query_row(
            "SELECT id FROM org_delegations WHERE org_id = ?1 AND grantee_member_id = ?2 AND target_identity = ?3
                AND revoked_at IS NULL AND (expires_at IS NULL OR expires_at > ?4)
                AND EXISTS (SELECT 1 FROM json_each(capabilities) WHERE value = 'enroll')",
            params![org_id, member.id, identity, now as i64],
            |r| r.get(0),
        )
        .optional()?;
    Ok(dlg.map(Authority::Delegation))
}

/// Record a grant for one approval or enrollment.
fn new_grant_in(
    c: &Connection,
    org_id: &str,
    member: &org::MemberRow,
    identity: &str,
    authority: &Authority,
    now: u64,
) -> GResult<String> {
    let id = org::new_id("dgr");
    let (kind, role, dlg) = match authority {
        Authority::Role(r) => ("role", Some(r.as_str()), None),
        Authority::Delegation(d) => ("delegation", None, Some(d.clone())),
    };
    let dlg_expiry: Option<i64> = match &dlg {
        Some(d) => c.query_row(
            "SELECT expires_at FROM org_delegations WHERE id = ?1",
            [d],
            |r| r.get(0),
        )?,
        None => None,
    };
    let expires = (now + CREDENTIAL_SECS) as i64;
    let expires = dlg_expiry.map(|e| e.min(expires)).unwrap_or(expires);
    c.execute(
        "INSERT INTO device_grants (id, org_id, approver_member_id, identity, capabilities, authority_kind,
                authority_role, delegation_id, authority_revision, created_at, expires_at)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11)",
        params![id, org_id, member.id, identity, DEVICE_CAPS, kind, role, dlg, member.authority_revision, now as i64, expires],
    )?;
    Ok(id)
}

/// Whether a grant is still backed by live authority (SDS §10.3).
pub(crate) fn grant_live_in(c: &Connection, grant_id: &str, now: u64) -> GResult<bool> {
    type Row = (
        String,
        String,
        String,
        String,
        Option<String>,
        Option<String>,
        i64,
        i64,
        Option<i64>,
    );
    let g: Option<Row> = c
        .query_row(
            "SELECT org_id, approver_member_id, identity, authority_kind, authority_role, delegation_id,
                    authority_revision, expires_at, revoked_at FROM device_grants WHERE id = ?1",
            [grant_id],
            |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?, r.get(4)?, r.get(5)?, r.get(6)?, r.get(7)?, r.get(8)?)),
        )
        .optional()?;
    let Some((org_id, approver, identity, kind, role, dlg, rev, expires, revoked)) = g else {
        return Ok(false);
    };
    if revoked.is_some() || expires <= now as i64 {
        return Ok(false);
    }
    let live_org_agent: bool = c.query_row(
        "SELECT EXISTS(SELECT 1 FROM org_agents a JOIN orgs o ON o.id = a.org_id
            WHERE a.org_id = ?1 AND a.identity = ?2 AND a.status = 'active'
              AND o.status IN ('active','past_due','read_only'))",
        params![org_id, identity],
        |r| r.get(0),
    )?;
    if !live_org_agent {
        return Ok(false);
    }
    let Some(m) = load_member(c, &org_id, &approver)? else {
        return Ok(false);
    };
    if m.status != "active" || m.authority_revision != rev {
        return Ok(false);
    }
    match kind.as_str() {
        "role" => {
            let roles = org::roles_of(c, &org_id, &m.id)?;
            Ok(role
                .as_deref()
                .and_then(Role::parse)
                .is_some_and(|r| roles.has(r) && org::permits(&org::Roles::of(&[r]), Perm::ApproveDevice, false)))
        }
        "delegation" => Ok(c.query_row(
            "SELECT EXISTS(SELECT 1 FROM org_delegations WHERE id = ?1 AND grantee_member_id = ?2 AND target_identity = ?3
                AND revoked_at IS NULL AND (expires_at IS NULL OR expires_at > ?4)
                AND EXISTS (SELECT 1 FROM json_each(capabilities) WHERE value = 'enroll'))",
            params![dlg, m.id, identity, now as i64],
            |r| r.get(0),
        )?),
        _ => Ok(false),
    }
}

/// Mint a device credential under a live grant. Returns `(token, credential id, expires_at)`.
fn issue_credential_in(
    c: &Connection,
    grant_id: &str,
    device_key: &[u8],
    label: Option<&str>,
    source: &str,
    now: u64,
) -> GResult<(String, String, i64)> {
    let (org_id, identity, grant_expiry): (String, String, i64) = c.query_row(
        "SELECT org_id, identity, expires_at FROM device_grants WHERE id = ?1",
        [grant_id],
        |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)),
    )?;
    let token = crate::gen_cap_token();
    let id = org::new_id("dev");
    let expires = grant_expiry.min((now + CREDENTIAL_SECS) as i64);
    c.execute(
        "INSERT INTO device_credentials (id, org_id, identity, device_public_key, token_hash, capabilities,
                parent_grant_id, label, source, created_at, expires_at)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11)",
        params![id, org_id, identity, device_key, &crate::sha256(token.as_bytes())[..], DEVICE_CAPS, grant_id, label, source, now as i64, expires],
    )?;
    Ok((token, id, expires))
}

/// Whether a credential row is usable now (its own state plus its grant's chain).
pub(crate) fn credential_live_in(
    c: &Connection,
    cred_id: &str,
    now: u64,
) -> GResult<Option<String>> {
    let row: Option<(String, String, i64, Option<i64>, Option<i64>)> = c
        .query_row(
            "SELECT identity, parent_grant_id, expires_at, revoked_at, last_used_at FROM device_credentials WHERE id = ?1",
            [cred_id],
            |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?, r.get(4)?)),
        )
        .optional()?;
    let Some((identity, grant, expires, revoked, last_used)) = row else {
        return Ok(None);
    };
    if revoked.is_some() || expires <= now as i64 || !grant_live_in(c, &grant, now)? {
        return Ok(None);
    }
    if last_used.is_none_or(|t| t + 300 < now as i64) {
        c.execute(
            "UPDATE device_credentials SET last_used_at = ?2 WHERE id = ?1",
            params![cred_id, now as i64],
        )?;
        c.execute(
            "UPDATE org_agents SET last_seen_at = ?2 WHERE identity = ?1",
            params![identity, now as i64],
        )?;
    }
    Ok(Some(identity))
}

/// Authentication hook: a `cap_…` token that is not a mailbox's own capability may be a device
/// credential. Returns the agent's identity when the credential and its whole chain are live.
pub(crate) async fn identity_for_device_token(
    state: &AppState,
    hash: [u8; 32],
) -> Result<Option<store::StoredIdentity>, crate::ApiError> {
    let now = now_unix();
    let found = state
        .store
        .group_tx(move |tx| -> GResult<Option<String>> {
            let id: Option<String> = tx
                .query_row(
                    "SELECT id FROM device_credentials WHERE token_hash = ?1",
                    [&hash[..]],
                    |r| r.get(0),
                )
                .optional()?;
            match id {
                Some(id) => credential_live_in(tx, &id, now),
                None => Ok(None),
            }
        })
        .await
        .map_err(|_| crate::ApiError::server("store_error"))?;
    match found {
        Some(address) => state
            .store
            .get(address)
            .await
            .map_err(|_| crate::ApiError::server("store_error")),
        None => Ok(None),
    }
}

// ---- agents ------------------------------------------------------------------------------------

pub(crate) fn agent_json(c: &Connection, org_id: &str, identity: &str, now: u64) -> GResult<Value> {
    let (handle, label, status, created_at, last_seen): (String, Option<String>, String, i64, Option<i64>) = c.query_row(
        "SELECT handle, label, status, created_at, last_seen_at FROM org_agents WHERE org_id = ?1 AND identity = ?2",
        params![org_id, identity],
        |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?, r.get(4)?)),
    )?;
    let devices: i64 = c.query_row(
        "SELECT COUNT(*) FROM device_credentials WHERE identity = ?1 AND revoked_at IS NULL AND expires_at > ?2",
        params![identity, now as i64],
        |r| r.get(0),
    )?;
    let mut stmt = c.prepare(
        "SELECT d.grantee_member_id, m.handle, d.capabilities FROM org_delegations d
           JOIN org_members m ON m.id = d.grantee_member_id
          WHERE d.org_id = ?1 AND d.target_identity = ?2 AND d.revoked_at IS NULL
            AND (d.expires_at IS NULL OR d.expires_at > ?3)",
    )?;
    let operators: Vec<Value> = stmt
        .query_map(params![org_id, identity, now as i64], |r| {
            let caps: String = r.get(2)?;
            Ok(json!({ "member_id": r.get::<_, String>(0)?, "handle": r.get::<_, Option<String>>(1)?,
                       "capabilities": serde_json::from_str::<Value>(&caps).unwrap_or(Value::Null) }))
        })?
        .collect::<Result<_, _>>()?;
    Ok(json!({
        "address": identity, "handle": handle, "label": label, "status": status, "created_at": created_at,
        "last_seen_at": last_seen, "devices": devices, "operators": operators,
    }))
}

pub(crate) fn agent_in_org(c: &Connection, org_id: &str, address: &str) -> GResult<String> {
    let found: Option<String> = c
        .query_row(
            "SELECT identity FROM org_agents WHERE org_id = ?1 AND (identity = ?2 OR handle = ?2) AND status <> 'deleted'",
            params![org_id, address],
            |r| r.get(0),
        )
        .optional()?;
    found.ok_or_else(|| GErr::new(StatusCode::NOT_FOUND, "not_found", "no such agent"))
}

/// `GET /v1/orgs/{id}/agents`
async fn agents(
    State(state): State<AppState>,
    headers: HeaderMap,
    Path(id): Path<String>,
) -> Response {
    let result = async {
        let ctx = org::org_ctx(&state, &headers, &id).await?;
        let full = org::permits(&ctx.roles, Perm::ManageAgents, false) || org::permits(&ctx.roles, Perm::Security, false);
        let mid = ctx.member.id.clone();
        let now = now_unix();
        let list = state
            .store
            .group_read(move |c: &Connection| -> GResult<Vec<Value>> {
                // Members see the agents delegated to them; agent admins see all.
                let mut stmt = c.prepare(
                    "SELECT identity FROM org_agents a WHERE a.org_id = ?1 AND a.status <> 'deleted'
                       AND (?2 OR EXISTS (SELECT 1 FROM org_delegations d WHERE d.org_id = a.org_id
                             AND d.target_identity = a.identity AND d.grantee_member_id = ?3 AND d.revoked_at IS NULL))
                     ORDER BY a.handle",
                )?;
                let ids: Vec<String> = stmt.query_map(params![id, full, mid], |r| r.get(0))?.collect::<Result<_, _>>()?;
                ids.iter().map(|a| agent_json(c, &id, a, now)).collect()
            })
            .await?;
        Ok(Outcome::ok(json!({ "agents": list })))
    }
    .await;
    finish(&state, result)
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct AgentReq {
    handle: String,
    #[serde(default)]
    label: Option<String>,
    #[serde(default)]
    operators: Vec<String>,
}

pub(crate) fn label_ok(l: Option<&str>) -> GResult<Option<String>> {
    match l.map(str::trim).filter(|l| !l.is_empty()) {
        None => Ok(None),
        Some(l) if l.chars().count() <= 80 && !l.chars().any(char::is_control) => {
            Ok(Some(l.to_string()))
        }
        Some(_) => Err(GErr::bad(
            "bad_request",
            "label is at most 80 printable characters",
        )),
    }
}

/// Create an agent inside a transaction (route and pairing approval share it).
#[allow(clippy::too_many_arguments)]
pub(crate) fn create_agent_in(
    tx: &Connection,
    vault: &crate::vault::Vault,
    o: &org::OrgRow,
    handle: &str,
    label: Option<&str>,
    by_member: &str,
    now: u64,
) -> GResult<String> {
    if org::agents_used(tx, &o.id)? >= o.agent_limit {
        return Err(GErr::conflict(
            "agent_limit",
            format!("this plan includes {} agent addresses", o.agent_limit),
        )
        .with("billing", "/app/billing"));
    }
    if !org::handle_free_in(tx, &o.id, handle, false)? {
        return Err(GErr::conflict("handle_taken", format!("{handle} is taken")));
    }
    let internal = o
        .internal_account
        .clone()
        .ok_or_else(|| GErr::conflict("org_inactive", "organization not provisioned"))?;
    let address = org::mint_identity_in(tx, vault, handle, &internal, label, now)?;
    tx.execute(
        "INSERT INTO org_agents (org_id, identity, handle, label, created_by, created_at) VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
        params![o.id, address, handle, label, by_member, now as i64],
    )?;
    Ok(address)
}

#[allow(clippy::too_many_arguments)]
fn delegate_in(
    tx: &Connection,
    org_id: &str,
    member_id: &str,
    identity: &str,
    caps: &[String],
    expires: Option<i64>,
    by: &str,
    now: u64,
) -> GResult<String> {
    let m = load_member(tx, org_id, member_id)?
        .filter(|m| m.status == "active")
        .ok_or_else(|| GErr::bad("invalid_member", "delegations go to active members"))?;
    // One live delegation per (member, agent): a new one replaces the old.
    tx.execute(
        "UPDATE org_delegations SET revoked_at = ?4, revision = revision + 1 WHERE org_id = ?1 AND grantee_member_id = ?2
            AND target_identity = ?3 AND revoked_at IS NULL",
        params![org_id, m.id, identity, now as i64],
    )?;
    let id = org::new_id("dlg");
    tx.execute(
        "INSERT INTO org_delegations (id, org_id, grantee_member_id, target_identity, capabilities, granted_by, granted_at, expires_at)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8)",
        params![id, org_id, m.id, identity, json!(caps).to_string(), by, now as i64, expires],
    )?;
    if let Some(a) = &m.account_id {
        org::emit_to(
            tx,
            a,
            "org.member",
            &json!({ "org_id": org_id, "member_id": m.id, "delegation": id, "agent": identity }),
            now,
        )?;
    }
    Ok(id)
}

/// `POST /v1/orgs/{id}/agents`
async fn create_agent(
    State(state): State<AppState>,
    Extension(cip): Extension<ClientIp>,
    headers: HeaderMap,
    Path(id): Path<String>,
    body: Body,
) -> Response {
    let result = async {
        let ctx = org::org_ctx(&state, &headers, &id).await?;
        ctx.require(&state, Perm::ManageAgents)?;
        ctx.require_writable()?;
        rate_limit(&format!("orgagent:{id}"), 500, 86_400)?;
        let (req, _): (AgentReq, String) = parse(body)?;
        let handle = org::org_handle(&ctx.org, &req.handle)?;
        let label = label_ok(req.label.as_deref())?;
        let vault = state.vault.clone();
        let auditor = state.auditor.clone();
        let account = ctx.account.clone();
        let me = ctx.member.id.clone();
        let ip = ipaddr(&cip);
        let now = now_unix();
        state
            .store
            .group_tx(move |tx| -> GResult<Outcome> {
                let o = load_org(tx, &id)?.ok_or_else(GErr::not_found)?;
                let address = create_agent_in(tx, &vault, &o, &handle, label.as_deref(), &me, now)?;
                let caps = vec!["enroll".to_string(), "bind_handle".to_string()];
                for op in &req.operators {
                    delegate_in(tx, &id, op, &address, &caps, None, &me, now)?;
                }
                org::audit_in(
                    tx,
                    &auditor,
                    &id,
                    ActorKind::Human,
                    Some(&account),
                    "agent.create",
                    Some(&address),
                    json!({ "handle": handle, "operators": req.operators.len() }),
                    ip.as_deref(),
                    now,
                )?;
                Ok(Outcome::created(
                    json!({ "agent": agent_json(tx, &id, &address, now)? }),
                ))
            })
            .await
    }
    .await;
    finish(&state, result)
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct PatchAgent {
    #[serde(default)]
    label: Option<String>,
    #[serde(default)]
    status: Option<String>,
}

/// `PATCH /v1/orgs/{id}/agents/{address}`
async fn patch_agent(
    State(state): State<AppState>,
    Extension(cip): Extension<ClientIp>,
    headers: HeaderMap,
    Path((id, address)): Path<(String, String)>,
    body: Body,
) -> Response {
    let result = async {
        let ctx = org::org_ctx(&state, &headers, &id).await?;
        ctx.require(&state, Perm::ManageAgents)?;
        let (req, _): (PatchAgent, String) = parse(body)?;
        let label = req
            .label
            .as_deref()
            .map(|l| label_ok(Some(l)))
            .transpose()?
            .flatten();
        if let Some(s) = &req.status {
            if !matches!(s.as_str(), "active" | "suspended") {
                return Err(GErr::bad("bad_request", "status is active or suspended"));
            }
        }
        let auditor = state.auditor.clone();
        let account = ctx.account.clone();
        let ip = ipaddr(&cip);
        let now = now_unix();
        state
            .store
            .group_tx(move |tx| -> GResult<Outcome> {
                let identity = agent_in_org(tx, &id, &address)?;
                if req.label.is_some() {
                    tx.execute(
                        "UPDATE org_agents SET label = ?3 WHERE org_id = ?1 AND identity = ?2",
                        params![id, identity, label],
                    )?;
                    tx.execute(
                        "UPDATE identities SET label = ?2 WHERE address = ?1",
                        params![identity, label],
                    )?;
                }
                if let Some(s) = &req.status {
                    tx.execute(
                        "UPDATE org_agents SET status = ?3 WHERE org_id = ?1 AND identity = ?2",
                        params![id, identity, s],
                    )?;
                    org::audit_in(
                        tx,
                        &auditor,
                        &id,
                        ActorKind::Human,
                        Some(&account),
                        if s == "suspended" {
                            "agent.suspend"
                        } else {
                            "agent.reinstate"
                        },
                        Some(&identity),
                        json!({}),
                        ip.as_deref(),
                        now,
                    )?;
                }
                Ok(Outcome::ok(
                    json!({ "agent": agent_json(tx, &id, &identity, now)? }),
                ))
            })
            .await
    }
    .await;
    finish(&state, result)
}

/// `DELETE /v1/orgs/{id}/agents/{address}`
async fn delete_agent(
    State(state): State<AppState>,
    Extension(cip): Extension<ClientIp>,
    headers: HeaderMap,
    Path((id, address)): Path<(String, String)>,
) -> Response {
    let result = async {
        let ctx = org::org_ctx(&state, &headers, &id).await?;
        ctx.require(&state, Perm::ManageAgents)?;
        let auditor = state.auditor.clone();
        let account = ctx.account.clone();
        let ip = ipaddr(&cip);
        let now = now_unix();
        state
            .store
            .group_tx(move |tx| -> GResult<Outcome> {
                let identity = agent_in_org(tx, &id, &address)?;
                revoke_all_devices_in(tx, &id, Some(&identity), now)?;
                tx.execute(
                    "UPDATE org_delegations SET revoked_at = ?3, revision = revision + 1 WHERE org_id = ?1 AND target_identity = ?2 AND revoked_at IS NULL",
                    params![id, identity, now as i64],
                )?;
                let mut out = Outcome::default();
                crate::org_rooms::remove_everywhere_in(tx, &identity, &identity, now, &mut out)?;
                tx.execute(
                    "UPDATE org_agents SET status = 'deleted', deleted_at = ?3 WHERE org_id = ?1 AND identity = ?2",
                    params![id, identity, now as i64],
                )?;
                // The readable name may be used again — by a new identity with new keys.
                tx.execute("UPDATE identities SET handle = NULL WHERE address = ?1", [&identity])?;
                org::audit_in(tx, &auditor, &id, ActorKind::Human, Some(&account), "agent.delete", Some(&identity), json!({}), ip.as_deref(), now)?;
                out.status = 200;
                out.body = json!({ "deleted": true });
                Ok(out)
            })
            .await
    }
    .await;
    finish(&state, result)
}

#[derive(Deserialize, Default)]
#[serde(deny_unknown_fields)]
struct EnrollReq {
    #[serde(default)]
    ttl_hours: Option<u64>,
}

/// `POST /v1/orgs/{id}/agents/{address}/enrollments` — a one-time code, shown once (§10.2).
async fn create_enrollment(
    State(state): State<AppState>,
    Extension(cip): Extension<ClientIp>,
    headers: HeaderMap,
    Path((id, address)): Path<(String, String)>,
    body: Body,
) -> Response {
    let result = async {
        let ctx = org::org_ctx(&state, &headers, &id).await?;
        ctx.require_writable()?;
        // Enrollment authority (role or `enroll` delegation) is checked below; the step-up is
        // required for every issuer (§10.2).
        ctx.step_up(&state)?;
        rate_limit(&format!("enrollissue:{}", ctx.account), 50, 3_600)?;
        let req: EnrollReq = match body {
            Ok(Json(v)) if !v.is_null() => serde_json::from_value(v).map_err(|e| GErr::bad("bad_request", e.to_string()))?,
            _ => EnrollReq::default(),
        };
        let ttl = req.ttl_hours.unwrap_or(24);
        if !(1..=24).contains(&ttl) {
            return Err(GErr::bad("bad_request", "ttl_hours is 1 to 24"));
        }
        let me = ctx.member.clone();
        let auditor = state.auditor.clone();
        let account = ctx.account.clone();
        let ip = ipaddr(&cip);
        let now = now_unix();
        state
            .store
            .group_tx(move |tx| -> GResult<Outcome> {
                let identity = agent_in_org(tx, &id, &address)?;
                let auth = enroll_authority_in(tx, &id, &me, &identity, now)?
                    .ok_or_else(|| GErr::forbidden("delegation_required", "you need agent-admin rights or an enroll delegation for this agent"))?;
                let grant = new_grant_in(tx, &id, &me, &identity, &auth, now)?;
                let code = format!("enr_{}", org::new_secret());
                let eid = org::new_id("enr");
                let expires = now as i64 + ttl as i64 * 3_600;
                tx.execute(
                    "INSERT INTO enrollments (id, code_hash, org_id, identity, capabilities, grant_id, created_by, created_at, expires_at)
                     VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9)",
                    params![eid, org::secret_hash(&code), id, identity, DEVICE_CAPS, grant, me.id, now as i64, expires],
                )?;
                org::audit_in(tx, &auditor, &id, ActorKind::Human, Some(&account), "agent.enrollment_issue", Some(&identity),
                    json!({ "enrollment": eid, "ttl_hours": ttl }), ip.as_deref(), now)?;
                Ok(Outcome::created(json!({ "code": code, "enrollment_id": eid, "expires_at": expires })))
            })
            .await
    }
    .await;
    no_store(finish(&state, result))
}

/// `GET /v1/orgs/{id}/agents/{address}/devices`
async fn devices(
    State(state): State<AppState>,
    headers: HeaderMap,
    Path((id, address)): Path<(String, String)>,
) -> Response {
    let result = async {
        let ctx = org::org_ctx(&state, &headers, &id).await?;
        ctx.require(&state, Perm::ManageAgents)?;
        let list = state
            .store
            .group_read(move |c: &Connection| -> GResult<Vec<Value>> {
                let identity = agent_in_org(c, &id, &address)?;
                let mut stmt = c.prepare(
                    "SELECT id, label, created_at, expires_at, last_used_at, revoked_at, source FROM device_credentials
                      WHERE identity = ?1 ORDER BY created_at DESC LIMIT 200",
                )?;
                let rows = stmt.query_map([identity], |r| {
                    Ok(json!({
                        "id": r.get::<_, String>(0)?, "label": r.get::<_, Option<String>>(1)?,
                        "created_at": r.get::<_, i64>(2)?, "expires_at": r.get::<_, i64>(3)?,
                        "last_used_at": r.get::<_, Option<i64>>(4)?, "revoked_at": r.get::<_, Option<i64>>(5)?,
                        "source": r.get::<_, String>(6)?,
                    }))
                })?;
                Ok(rows.collect::<Result<_, _>>()?)
            })
            .await?;
        Ok(Outcome::ok(json!({ "devices": list })))
    }
    .await;
    finish(&state, result)
}

/// `DELETE /v1/orgs/{id}/devices/{device}`
async fn revoke_device(
    State(state): State<AppState>,
    Extension(cip): Extension<ClientIp>,
    headers: HeaderMap,
    Path((id, device)): Path<(String, String)>,
) -> Response {
    let result = async {
        let ctx = org::org_ctx(&state, &headers, &id).await?;
        ctx.require(&state, Perm::ManageAgents)?;
        let auditor = state.auditor.clone();
        let account = ctx.account.clone();
        let ip = ipaddr(&cip);
        let now = now_unix();
        state
            .store
            .group_tx(move |tx| -> GResult<Outcome> {
                let n = tx.execute(
                    "UPDATE device_credentials SET revoked_at = ?3 WHERE id = ?1 AND org_id = ?2 AND revoked_at IS NULL",
                    params![device, id, now as i64],
                )?;
                if n == 0 {
                    return Err(GErr::not_found());
                }
                org::audit_in(tx, &auditor, &id, ActorKind::Human, Some(&account), "device.revoke", Some(&device), json!({}), ip.as_deref(), now)?;
                Ok(Outcome::ok(json!({ "revoked": true })))
            })
            .await
    }
    .await;
    finish(&state, result)
}

// ---- delegations -------------------------------------------------------------------------------

/// `GET /v1/orgs/{id}/delegations`
async fn delegations(
    State(state): State<AppState>,
    headers: HeaderMap,
    Path(id): Path<String>,
) -> Response {
    let result = async {
        let ctx = org::org_ctx(&state, &headers, &id).await?;
        ctx.require(&state, Perm::Delegate)?;
        let now = now_unix();
        let list = state
            .store
            .group_read(move |c: &Connection| -> GResult<Vec<Value>> {
                let mut stmt = c.prepare(
                    "SELECT d.id, d.grantee_member_id, m.handle, a.handle, d.capabilities, d.expires_at, d.granted_by, d.granted_at
                       FROM org_delegations d JOIN org_members m ON m.id = d.grantee_member_id
                       JOIN org_agents a ON a.org_id = d.org_id AND a.identity = d.target_identity
                      WHERE d.org_id = ?1 AND d.revoked_at IS NULL AND (d.expires_at IS NULL OR d.expires_at > ?2)
                      ORDER BY d.granted_at DESC",
                )?;
                let rows = stmt.query_map(params![id, now as i64], |r| {
                    let caps: String = r.get(4)?;
                    Ok(json!({
                        "id": r.get::<_, String>(0)?, "member_id": r.get::<_, String>(1)?,
                        "member_handle": r.get::<_, Option<String>>(2)?, "target": r.get::<_, String>(3)?,
                        "capabilities": serde_json::from_str::<Value>(&caps).unwrap_or(Value::Null),
                        "expires_at": r.get::<_, Option<i64>>(5)?, "granted_by": r.get::<_, Option<String>>(6)?,
                        "granted_at": r.get::<_, i64>(7)?,
                    }))
                })?;
                Ok(rows.collect::<Result<_, _>>()?)
            })
            .await?;
        Ok(Outcome::ok(json!({ "delegations": list })))
    }
    .await;
    finish(&state, result)
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct DelegationReq {
    member_id: String,
    target: String,
    capabilities: Vec<String>,
    #[serde(default)]
    expires_at: Option<i64>,
}

/// `POST /v1/orgs/{id}/delegations` — a pattern is materialized to every matching agent now.
async fn create_delegation(
    State(state): State<AppState>,
    Extension(cip): Extension<ClientIp>,
    headers: HeaderMap,
    Path(id): Path<String>,
    body: Body,
) -> Response {
    let result = async {
        let ctx = org::org_ctx(&state, &headers, &id).await?;
        ctx.require(&state, Perm::Delegate)?;
        let (req, _): (DelegationReq, String) = parse(body)?;
        if req.capabilities.is_empty() || req.capabilities.iter().any(|c| !CAPABILITIES.contains(&c.as_str())) {
            return Err(GErr::bad("bad_request", format!("capabilities are a subset of {}", CAPABILITIES.join(", "))));
        }
        let now = now_unix();
        if req.expires_at.is_some_and(|e| e <= now as i64) {
            return Err(GErr::bad("bad_request", "expires_at is in the past"));
        }
        let pat = crate::org_rules::parse_pat(&req.target, &ctx.org.namespace, true).map_err(|m| GErr::bad("invalid_target", m))?;
        let prefix = match &pat {
            crate::org_rules::Pat::Exact(h) => (h.clone(), false),
            crate::org_rules::Pat::Prefix(p) => (p.clone(), true),
            _ => return Err(GErr::bad("invalid_target", "target is an agent address or /ns/prefix-*")),
        };
        let auditor = state.auditor.clone();
        let account = ctx.account.clone();
        let me = ctx.member.id.clone();
        let ip = ipaddr(&cip);
        state
            .store
            .group_tx(move |tx| -> GResult<Outcome> {
                let mut stmt = tx.prepare(
                    "SELECT identity FROM org_agents WHERE org_id = ?1 AND status = 'active'
                       AND (CASE WHEN ?3 THEN substr(handle, 1, length(?2)) = ?2 ELSE handle = ?2 END)",
                )?;
                let targets: Vec<String> = stmt.query_map(params![id, prefix.0, prefix.1], |r| r.get(0))?.collect::<Result<_, _>>()?;
                drop(stmt);
                if targets.is_empty() {
                    return Err(GErr::bad("invalid_target", "no agent matches that target"));
                }
                let mut created = Vec::new();
                for t in &targets {
                    let did = delegate_in(tx, &id, &req.member_id, t, &req.capabilities, req.expires_at, &me, now)?;
                    created.push(json!({ "id": did, "target": t }));
                }
                org::audit_in(tx, &auditor, &id, ActorKind::Human, Some(&account), "delegation.grant", Some(&req.member_id),
                    json!({ "target": req.target, "capabilities": req.capabilities, "agents": targets.len() }), ip.as_deref(), now)?;
                Ok(Outcome::created(json!({ "delegations": created })))
            })
            .await
    }
    .await;
    finish(&state, result)
}

/// `DELETE /v1/orgs/{id}/delegations/{delegation}`
async fn revoke_delegation(
    State(state): State<AppState>,
    Extension(cip): Extension<ClientIp>,
    headers: HeaderMap,
    Path((id, did)): Path<(String, String)>,
) -> Response {
    let result = async {
        let ctx = org::org_ctx(&state, &headers, &id).await?;
        ctx.require(&state, Perm::Delegate)?;
        let auditor = state.auditor.clone();
        let account = ctx.account.clone();
        let ip = ipaddr(&cip);
        let now = now_unix();
        state
            .store
            .group_tx(move |tx| -> GResult<Outcome> {
                let n = tx.execute(
                    "UPDATE org_delegations SET revoked_at = ?3, revision = revision + 1 WHERE id = ?1 AND org_id = ?2 AND revoked_at IS NULL",
                    params![did, id, now as i64],
                )?;
                if n == 0 {
                    return Err(GErr::not_found());
                }
                // Credentials it approved stop with it.
                tx.execute(
                    "UPDATE device_credentials SET revoked_at = ?2 WHERE revoked_at IS NULL AND parent_grant_id IN
                        (SELECT id FROM device_grants WHERE delegation_id = ?1)",
                    params![did, now as i64],
                )?;
                tx.execute("UPDATE device_grants SET revoked_at = ?2 WHERE delegation_id = ?1 AND revoked_at IS NULL", params![did, now as i64])?;
                org::audit_in(tx, &auditor, &id, ActorKind::Human, Some(&account), "delegation.revoke", Some(&did), json!({}), ip.as_deref(), now)?;
                Ok(Outcome::ok(json!({ "revoked": true })))
            })
            .await
    }
    .await;
    finish(&state, result)
}

// ---- device pairing (SDS §10.2) ----------------------------------------------------------------

fn b64_std(s: &str, field: &'static str) -> GResult<Vec<u8>> {
    crate::groups::unb64(field, s.trim())
}

fn device_key(s: &str) -> GResult<VerifyingKey> {
    let raw = b64_std(s, "device_public_key")?;
    let arr: [u8; 32] = raw
        .try_into()
        .map_err(|_| GErr::bad("bad_request", "device_public_key is 32 bytes"))?;
    VerifyingKey::from_bytes(&arr)
        .map_err(|_| GErr::bad("bad_request", "device_public_key is not an Ed25519 key"))
}

fn verify_sig(key: &VerifyingKey, msg: &[u8], sig_b64: &str) -> bool {
    let Ok(raw) = b64_std(sig_b64, "signature") else {
        return false;
    };
    let Ok(arr): Result<[u8; 64], _> = raw.try_into() else {
        return false;
    };
    key.verify(msg, &Signature::from_bytes(&arr)).is_ok()
}

/// `ABCD-EFGH`, from an alphabet without look-alikes (~40 bits; it is only a lookup handle — the
/// claim needs the secret and the device key).
fn user_code() -> String {
    const A: &[u8] = b"BCDFGHJKLMNPQRSTVWXZ23456789";
    let mut raw = [0u8; 8];
    rand_core::RngCore::fill_bytes(&mut rand_core::OsRng, &mut raw);
    let s: String = raw
        .iter()
        .map(|b| A[*b as usize % A.len()] as char)
        .collect();
    format!("{}-{}", &s[..4], &s[4..])
}

fn normalize_code(s: &str) -> String {
    let c: String = s
        .chars()
        .filter(|c| c.is_ascii_alphanumeric())
        .collect::<String>()
        .to_ascii_uppercase();
    if c.len() == 8 {
        format!("{}-{}", &c[..4], &c[4..])
    } else {
        c
    }
}

/// The number shown on the terminal and on the approval screen: bound to the pairing and the key.
fn confirmation(pairing: &str, key: &[u8]) -> String {
    use sha2::{Digest, Sha256};
    let mut h = Sha256::new();
    h.update(b"pigeonpost-pairing-confirm-v1\0");
    h.update(pairing.as_bytes());
    h.update([0]);
    h.update(key);
    let d = h.finalize();
    let n = u32::from_be_bytes([d[0], d[1], d[2], d[3]]) % 1_000_000;
    format!("{:03} {:03}", n / 1000, n % 1000)
}

fn inbox_url() -> String {
    std::env::var("POSTBOX_INBOX_URL")
        .ok()
        .map(|u| u.trim().trim_end_matches('/').to_string())
        .filter(|u| !u.is_empty())
        .unwrap_or_else(|| "https://inbox.pigeonpost.dev".into())
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct PairingReq {
    handle: String,
    device_public_key: String,
    claim_challenge: String,
    #[serde(default)]
    label: Option<String>,
}

/// `POST /v1/device-pairings` — no credential. The answer never says whether the handle exists.
async fn create_pairing(
    State(state): State<AppState>,
    Extension(cip): Extension<ClientIp>,
    body: Body,
) -> Response {
    let result = async {
        rate_limit(&format!("pairnew:{}", cip.as_str()), 10, 600)?;
        let (req, _): (PairingReq, String) = parse(body)?;
        let key = device_key(&req.device_public_key)?;
        let challenge = b64_std(&req.claim_challenge, "claim_challenge")?;
        if challenge.len() != 32 {
            return Err(GErr::bad("bad_request", "claim_challenge is the 32-byte SHA-256 of the claim secret"));
        }
        let label = label_ok(req.label.as_deref())?;
        let handle = pigeonpost_core::Destination::for_handle(req.handle.trim())
            .ok()
            .and_then(|d| d.handle().map(str::to_string))
            .filter(|h| h.trim_start_matches('/').split('/').count() == 2)
            .ok_or_else(|| GErr::bad("invalid_handle", "expected /<org>/<agent>"))?;
        let ns = handle.trim_start_matches('/').split('/').next().unwrap_or_default().to_string();
        let now = now_unix();
        let pid = org::new_id("pair");
        let code = user_code();
        let conf = confirmation(&pid, key.as_bytes());
        let key_bytes = key.to_bytes().to_vec();
        let (p2, c2) = (pid.clone(), code.clone());
        state
            .store
            .group_tx(move |tx| -> GResult<()> {
                let org: Option<String> = tx
                    .query_row(
                        "SELECT id FROM orgs WHERE namespace = ?1 AND status IN ('active','past_due','read_only')",
                        [&ns],
                        |r| r.get(0),
                    )
                    .optional()?;
                let target: Option<String> = match &org {
                    Some(o) => tx
                        .query_row(
                            "SELECT identity FROM org_agents WHERE org_id = ?1 AND handle = ?2 AND status = 'active'",
                            params![o, handle],
                            |r| r.get(0),
                        )
                        .optional()?,
                    None => None,
                };
                tx.execute(
                    "INSERT INTO device_pairings (id, user_code, claim_challenge, device_public_key, requested_handle,
                            target_identity, org_id, capabilities, label, created_at, expires_at)
                     VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11)",
                    params![p2, c2, challenge, key_bytes, handle, target, org, DEVICE_CAPS, label, now as i64, (now + PAIRING_SECS) as i64],
                )?;
                Ok(())
            })
            .await?;
        let base = inbox_url();
        Ok(Outcome::created(json!({
            "pairing_id": pid,
            "user_code": code,
            "verification_uri": format!("{base}/pair.html"),
            "verification_uri_complete": format!("{base}/pair.html#{code}"),
            "confirmation": conf,
            "expires_in": PAIRING_SECS,
            "interval": POLL_SECS,
        })))
    }
    .await;
    no_store(finish(&state, result))
}

struct PairingRow {
    id: String,
    key: Vec<u8>,
    challenge: Vec<u8>,
    handle: String,
    target: Option<String>,
    org: Option<String>,
    label: Option<String>,
    ip_country: Option<String>,
    created_at: i64,
    expires_at: i64,
    approved_grant: Option<String>,
    approved_at: Option<i64>,
    denied_at: Option<i64>,
    claimed_at: Option<i64>,
    credential: Option<String>,
    revoked_at: Option<i64>,
    last_poll_at: Option<i64>,
}

const PAIR_COLS: &str = "id, device_public_key, claim_challenge, requested_handle, target_identity, org_id, label, ip_country,
    created_at, expires_at, approved_grant_id, approved_at, denied_at, claimed_at, credential_id, revoked_at, last_poll_at";

fn map_pairing(r: &rusqlite::Row<'_>) -> rusqlite::Result<PairingRow> {
    Ok(PairingRow {
        id: r.get(0)?,
        key: r.get(1)?,
        challenge: r.get(2)?,
        handle: r.get(3)?,
        target: r.get(4)?,
        org: r.get(5)?,
        label: r.get(6)?,
        ip_country: r.get(7)?,
        created_at: r.get(8)?,
        expires_at: r.get(9)?,
        approved_grant: r.get(10)?,
        approved_at: r.get(11)?,
        denied_at: r.get(12)?,
        claimed_at: r.get(13)?,
        credential: r.get(14)?,
        revoked_at: r.get(15)?,
        last_poll_at: r.get(16)?,
    })
}

fn pairing_not_found() -> GErr {
    GErr::new(
        StatusCode::NOT_FOUND,
        "pairing_not_found",
        "that code is not valid; check the terminal",
    )
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct ResolveReq {
    user_code: String,
}

/// The person resolving or approving: active member of the pairing's org.
fn approver_in(
    c: &Connection,
    p: &PairingRow,
    account: &str,
) -> GResult<(org::OrgRow, org::MemberRow)> {
    let org_id = p.org.clone().ok_or_else(pairing_not_found)?;
    let o = load_org(c, &org_id)?.ok_or_else(pairing_not_found)?;
    let m = org::member_for_account(c, &org_id, account)?
        .filter(|m| m.status == "active")
        .ok_or_else(pairing_not_found)?;
    Ok((o, m))
}

/// What may this person do with this pairing, and why not.
fn approval_scope(
    c: &Connection,
    o: &org::OrgRow,
    m: &org::MemberRow,
    p: &PairingRow,
    now: u64,
) -> GResult<Result<(), &'static str>> {
    if !matches!(o.status.as_str(), "active" | "past_due" | "read_only") {
        return Ok(Err("org_inactive"));
    }
    match &p.target {
        Some(t) => Ok(match enroll_authority_in(c, &o.id, m, t, now)? {
            Some(_) => Ok(()),
            None => Err("delegation_required"),
        }),
        None => {
            let roles = org::roles_of(c, &o.id, &m.id)?;
            Ok(if org::permits(&roles, Perm::ManageAgents, false) {
                Ok(())
            } else {
                Err("delegation_required")
            })
        }
    }
}

/// `POST /v1/device-pairings/resolve`
async fn resolve_pairing(
    State(state): State<AppState>,
    headers: HeaderMap,
    body: Body,
) -> Response {
    let result = async {
        let (_principal, account, _session) = person_ctx(&state, &headers).await?;
        rate_limit(&format!("pairres:{account}"), 30, 600)?;
        let (req, _): (ResolveReq, String) = parse(body)?;
        let code = normalize_code(&req.user_code);
        let now = now_unix();
        state
            .store
            .group_read(move |c: &Connection| -> GResult<Outcome> {
                let p = c
                    .query_row(
                        &format!("SELECT {PAIR_COLS} FROM device_pairings WHERE user_code = ?1"),
                        [&code],
                        map_pairing,
                    )
                    .optional()?
                    .ok_or_else(pairing_not_found)?;
                let (o, m) = approver_in(c, &p, &account)?;
                if p.expires_at <= now as i64 || p.revoked_at.is_some() {
                    return Err(GErr::new(
                        StatusCode::GONE,
                        "pairing_expired",
                        "this request expired; start again on the machine",
                    ));
                }
                let state_name = if p.denied_at.is_some() {
                    "denied"
                } else if p.approved_at.is_some() {
                    "approved"
                } else {
                    "pending"
                };
                let scope = approval_scope(c, &o, &m, &p, now)?;
                Ok(Outcome::ok(json!({
                    "pairing_id": p.id,
                    "org": { "id": o.id, "name": o.name },
                    "handle": p.handle,
                    "address": p.target,
                    "creates_agent": p.target.is_none(),
                    "label": p.label,
                    "confirmation": confirmation(&p.id, &p.key),
                    "capabilities": ["mailbox"],
                    "ip_country": p.ip_country,
                    "requested_at": p.created_at,
                    "expires_at": p.expires_at,
                    "state": state_name,
                    "can_approve": scope.is_ok() && state_name == "pending",
                    "reason": scope.err(),
                })))
            })
            .await
    }
    .await;
    no_store(finish(&state, result))
}

async fn person_ctx(
    state: &AppState,
    headers: &HeaderMap,
) -> GResult<(crate::Principal, String, crate::principal::MemberSession)> {
    let principal = crate::principal_for_token(state, crate::bearer(headers)).await?;
    let (account, session) = org::person(&principal)?;
    Ok((principal, account, session))
}

/// `POST /v1/device-pairings/{pairing}/approve` — a person, step-up, live authority.
async fn approve_pairing(
    State(state): State<AppState>,
    Extension(cip): Extension<ClientIp>,
    headers: HeaderMap,
    Path(pid): Path<String>,
    body: Body,
) -> Response {
    let result = async {
        let (principal, account, _session) = person_ctx(&state, &headers).await?;
        rate_limit(&format!("pairok:{account}"), 30, 600)?;
        let (req, _): (ResolveReq, String) = parse(body)?;
        let facts = Facts {
            now: now_unix(),
            step_up: (*state.step_up).clone(),
            org_permitted: true,
            ..Facts::default()
        };
        // Credential kind and step-up through the policy; the role/delegation grant is re-decided
        // in the transaction below.
        authorize(&Actor::Principal(&principal), Action::OrgSensitive, &Resource::Org(&pid), &facts).map_err(|d| match d {
            crate::policy::DenyReason::Auth(a) => org::auth_denial(a),
            other => GErr::forbidden(other.code(), "not allowed"),
        })?;
        let code = normalize_code(&req.user_code);
        let vault = state.vault.clone();
        let auditor = state.auditor.clone();
        let ip = ipaddr(&cip);
        let now = now_unix();
        state
            .store
            .group_tx(move |tx| -> GResult<Outcome> {
                let p = tx
                    .query_row(&format!("SELECT {PAIR_COLS} FROM device_pairings WHERE id = ?1 AND user_code = ?2"), params![pid, code], map_pairing)
                    .optional()?
                    .ok_or_else(pairing_not_found)?;
                let (o, m) = approver_in(tx, &p, &account)?;
                if p.expires_at <= now as i64 || p.revoked_at.is_some() {
                    return Err(GErr::new(StatusCode::GONE, "pairing_expired", "this request expired"));
                }
                if p.approved_at.is_some() || p.denied_at.is_some() {
                    return Err(GErr::conflict("pairing_decided", "this request was already decided"));
                }
                if let Err(reason) = approval_scope(tx, &o, &m, &p, now)? {
                    return Err(GErr::forbidden(reason, "you cannot approve a device for this agent"));
                }
                let target = match &p.target {
                    Some(t) => t.clone(),
                    None => {
                        let t = create_agent_in(tx, &vault, &o, &p.handle, p.label.as_deref(), &m.id, now)?;
                        org::audit_in(tx, &auditor, &o.id, ActorKind::Human, Some(&account), "agent.create", Some(&t),
                            json!({ "handle": p.handle, "via": "pairing" }), ip.as_deref(), now)?;
                        t
                    }
                };
                let auth = enroll_authority_in(tx, &o.id, &m, &target, now)?
                    .ok_or_else(|| GErr::forbidden("delegation_required", "you cannot approve a device for this agent"))?;
                let grant = new_grant_in(tx, &o.id, &m, &target, &auth, now)?;
                tx.execute(
                    "UPDATE device_pairings SET target_identity = ?2, approved_by = ?3, approved_grant_id = ?4, approved_at = ?5 WHERE id = ?1",
                    params![p.id, target, m.id, grant, now as i64],
                )?;
                org::audit_in(tx, &auditor, &o.id, ActorKind::Human, Some(&account), "agent.pairing_approve", Some(&target),
                    json!({ "pairing": p.id, "label": p.label }), ip.as_deref(), now)?;
                Ok(Outcome::ok(json!({ "approved": true, "address": target, "handle": p.handle })))
            })
            .await
    }
    .await;
    no_store(finish(&state, result))
}

/// `POST /v1/device-pairings/{pairing}/deny`
async fn deny_pairing(
    State(state): State<AppState>,
    headers: HeaderMap,
    Path(pid): Path<String>,
    body: Body,
) -> Response {
    let result = async {
        let (_p, account, _s) = person_ctx(&state, &headers).await?;
        let (req, _): (ResolveReq, String) = parse(body)?;
        let code = normalize_code(&req.user_code);
        let now = now_unix();
        state
            .store
            .group_tx(move |tx| -> GResult<Outcome> {
                let p = tx
                    .query_row(&format!("SELECT {PAIR_COLS} FROM device_pairings WHERE id = ?1 AND user_code = ?2"), params![pid, code], map_pairing)
                    .optional()?
                    .ok_or_else(pairing_not_found)?;
                let _ = approver_in(tx, &p, &account)?;
                if p.approved_at.is_none() {
                    tx.execute("UPDATE device_pairings SET denied_at = ?2 WHERE id = ?1 AND denied_at IS NULL", params![p.id, now as i64])?;
                }
                Ok(Outcome::ok(json!({ "denied": true })))
            })
            .await
    }
    .await;
    finish(&state, result)
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct ClaimReq {
    pairing_id: String,
    claim_secret: String,
    signature: String,
}

/// `POST /v1/device-pairings/claim` — the machine, proving the claim secret and the device key.
async fn claim_pairing(
    State(state): State<AppState>,
    Extension(cip): Extension<ClientIp>,
    body: Body,
) -> Response {
    let result = async {
        rate_limit(&format!("pairclaim:{}", cip.as_str()), 240, 600)?;
        let (req, _): (ClaimReq, String) = parse(body)?;
        let secret = b64_std(&req.claim_secret, "claim_secret")?;
        let now = now_unix();
        let pid = req.pairing_id.clone();
        rate_limit(&format!("pairclaim-id:{pid}"), 200, 600)?;
        state
            .store
            .group_tx(move |tx| -> GResult<Outcome> {
                let p = tx
                    .query_row(&format!("SELECT {PAIR_COLS} FROM device_pairings WHERE id = ?1"), [&pid], map_pairing)
                    .optional()?
                    .ok_or_else(pairing_not_found)?;
                // Proofs first: without both, the caller learns nothing about the pairing's state.
                let challenge_ok: bool = subtle::ConstantTimeEq::ct_eq(crate::sha256(&secret).as_slice(), p.challenge.as_slice()).into();
                let key_arr: [u8; 32] = p.key.clone().try_into().map_err(|_| pairing_not_found())?;
                let key = VerifyingKey::from_bytes(&key_arr).map_err(|_| pairing_not_found())?;
                let msg = format!("pigeonpost-pairing-claim-v1\0{}", p.id);
                if !challenge_ok || !verify_sig(&key, msg.as_bytes(), &req.signature) {
                    return Err(pairing_not_found());
                }
                device_policy(&p.id, true)?;
                if p.denied_at.is_some() {
                    return Err(GErr::forbidden("access_denied", "the request was denied"));
                }
                if p.revoked_at.is_some() {
                    return Err(GErr::new(StatusCode::GONE, "pairing_expired", "this request is no longer valid"));
                }
                let Some(grant) = p.approved_grant.clone() else {
                    if p.expires_at <= now as i64 {
                        return Err(GErr::new(StatusCode::GONE, "pairing_expired", "this request expired; start again"));
                    }
                    let too_fast = p.last_poll_at.is_some_and(|t| t + (POLL_SECS as i64) - 1 > now as i64);
                    tx.execute("UPDATE device_pairings SET last_poll_at = ?2 WHERE id = ?1", params![p.id, now as i64])?;
                    let mut e = GErr::new(StatusCode::PRECONDITION_REQUIRED, if too_fast { "slow_down" } else { "authorization_pending" },
                        "waiting for a person to approve this machine");
                    e = e.with("interval", POLL_SECS);
                    return Err(e);
                };
                if let Some(claimed) = p.claimed_at {
                    // A retry after a lost response replaces the credential, never adds one.
                    if claimed + RECLAIM_SECS < now as i64 {
                        return Err(GErr::new(StatusCode::GONE, "pairing_expired", "this request was already used"));
                    }
                    if let Some(old) = &p.credential {
                        tx.execute("UPDATE device_credentials SET revoked_at = ?2 WHERE id = ?1 AND revoked_at IS NULL", params![old, now as i64])?;
                    }
                } else if p.approved_at.is_some_and(|a| a + PAIRING_SECS as i64 * 2 < now as i64) {
                    return Err(GErr::new(StatusCode::GONE, "pairing_expired", "this approval expired; start again"));
                }
                if !grant_live_in(tx, &grant, now)? {
                    return Err(GErr::forbidden("access_denied", "the approval is no longer valid"));
                }
                let (token, cred, expires) = issue_credential_in(tx, &grant, &p.key, p.label.as_deref(), "pairing", now)?;
                tx.execute(
                    "UPDATE device_pairings SET claimed_at = ?2, credential_id = ?3 WHERE id = ?1",
                    params![p.id, now as i64, cred],
                )?;
                let address = p.target.clone().unwrap_or_default();
                Ok(Outcome::ok(json!({
                    "credential": token, "address": address, "handle": p.handle, "expires_at": expires, "device_id": cred,
                })))
            })
            .await
    }
    .await;
    no_store(finish(&state, result))
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct RedeemReq {
    code: String,
    device_public_key: String,
    signature: String,
    #[serde(default)]
    label: Option<String>,
}

/// The policy's view of a device proof: possession of the code or claim secret plus the device
/// key is the credential (SDS §10.2), checked by the caller before asking.
fn device_policy(id: &str, proved: bool) -> GResult<()> {
    let facts = Facts {
        now: now_unix(),
        device_proof: proved,
        ..Facts::default()
    };
    authorize(
        &Actor::Anonymous,
        Action::DeviceEnroll,
        &Resource::DeviceGrant(id),
        &facts,
    )
    .map_err(|_| GErr::forbidden("access_denied", "device proof required"))
}

fn enrollment_invalid() -> GErr {
    GErr::bad("enrollment_invalid", "that enrollment code is not valid")
}

/// `POST /v1/enrollments/redeem` — no credential; one answer for every bad code.
async fn redeem_enrollment(
    State(state): State<AppState>,
    Extension(cip): Extension<ClientIp>,
    body: Body,
) -> Response {
    let result = async {
        rate_limit(&format!("enroll:{}", cip.as_str()), 10, 600).map_err(|e| GErr {
            code: "enrollment_rate_limited",
            ..e
        })?;
        let (req, _): (RedeemReq, String) = parse(body)?;
        let code = req.code.trim().to_string();
        if !code.starts_with("enr_") || code.len() > 100 {
            return Err(enrollment_invalid());
        }
        let key = device_key(&req.device_public_key)?;
        let msg = format!("pigeonpost-enroll-v1\0{code}");
        if !verify_sig(&key, msg.as_bytes(), &req.signature) {
            return Err(enrollment_invalid());
        }
        device_policy("enrollment", true)?;
        let label = label_ok(req.label.as_deref())?;
        let ip_hash = state.auditor.ip_hash(cip.as_str().parse().unwrap_or(std::net::IpAddr::from([0, 0, 0, 0])));
        let auditor = state.auditor.clone();
        let ip = ipaddr(&cip);
        let now = now_unix();
        let key_bytes = key.to_bytes().to_vec();
        state
            .store
            .group_tx(move |tx| -> GResult<Outcome> {
                let row: Option<(String, String, String, String, i64, Option<i64>, Option<i64>)> = tx
                    .query_row(
                        "SELECT id, org_id, identity, grant_id, expires_at, redeemed_at, revoked_at FROM enrollments WHERE code_hash = ?1",
                        [org::secret_hash(&code)],
                        |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?, r.get(4)?, r.get(5)?, r.get(6)?)),
                    )
                    .optional()?;
                let Some((eid, org_id, identity, grant, expires, redeemed, revoked)) = row else { return Err(enrollment_invalid()) };
                if redeemed.is_some() || revoked.is_some() || expires <= now as i64 || !grant_live_in(tx, &grant, now)? {
                    return Err(enrollment_invalid());
                }
                let (token, cred, cexp) = issue_credential_in(tx, &grant, &key_bytes, label.as_deref(), "enrollment", now)?;
                tx.execute(
                    "UPDATE enrollments SET redeemed_at = ?2, redeemed_label = ?3, redeemed_ip_hash = ?4 WHERE id = ?1",
                    params![eid, now as i64, label, ip_hash],
                )?;
                org::audit_in(tx, &auditor, &org_id, ActorKind::System, None, "agent.enrollment_redeem", Some(&identity),
                    json!({ "enrollment": eid, "device": cred }), ip.as_deref(), now)?;
                let handle: Option<String> = tx.query_row("SELECT handle FROM org_agents WHERE identity = ?1", [&identity], |r| r.get(0)).optional()?;
                Ok(Outcome::ok(json!({ "credential": token, "address": identity, "handle": handle, "expires_at": cexp, "device_id": cred })))
            })
            .await
    }
    .await;
    no_store(finish(&state, result))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn user_codes_and_confirmations() {
        let c = user_code();
        assert_eq!(c.len(), 9);
        assert_eq!(normalize_code(&c.to_lowercase().replace('-', " ")), c);
        let a = confirmation("pair_x", &[1; 32]);
        assert_eq!(a.len(), 7);
        assert_eq!(a, confirmation("pair_x", &[1; 32]));
        assert_ne!(
            a,
            confirmation("pair_x", &[2; 32]),
            "bound to the device key"
        );
        assert_ne!(a, confirmation("pair_y", &[1; 32]), "bound to the pairing");
    }
}
