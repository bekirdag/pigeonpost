//! `/v1/orgs…` and `/v1/org-invites…`: the organization surface the enterprise portal and the
//! apps use (SDS §5–§7, §9; contract `docs/orgs.md`).
//!
//! Every mutating route is one IMMEDIATE transaction (`Store::group_tx`) that re-reads the
//! caller's membership and roles, the target's rank and the org's capacity, makes the change,
//! appends the audit record and the account events, and commits them together.

// Row tuples read straight from SQL are clearer inline than as one-use type aliases.
#![allow(clippy::type_complexity)]

use crate::audit::ActorKind;
use crate::groups::{finish, parse, rate_limit, GErr, GResult, Outcome};
use crate::org::{
    self, can_manage, load_member, load_org, may_grant, member_json, org_json, OrgCtx, Perm, Role,
    Roles,
};
use crate::org_rules;
use crate::{capabilities, now_unix, AppState, ClientIp};
use axum::extract::rejection::JsonRejection;
use axum::extract::{Path, Query, State};
use axum::http::{header, HeaderMap, HeaderValue, StatusCode};
use axum::response::{IntoResponse, Response};
use axum::routing::{delete, get, patch, post};
use axum::{Extension, Json, Router};
use rusqlite::{params, Connection, OptionalExtension};
use serde::Deserialize;
use serde_json::{json, Value};
use std::sync::OnceLock;

/// Where invitation links point. `POSTBOX_ENTERPRISE_URL`, default the production portal.
pub(crate) fn enterprise_url() -> &'static str {
    static URL: OnceLock<String> = OnceLock::new();
    URL.get_or_init(|| {
        std::env::var("POSTBOX_ENTERPRISE_URL")
            .ok()
            .map(|u| u.trim().trim_end_matches('/').to_string())
            .filter(|u| {
                u.starts_with("https://")
                    || u.starts_with("http://localhost")
                    || u.starts_with("http://127.0.0.1")
            })
            .unwrap_or_else(|| "https://enterprise.pigeonpost.dev".into())
    })
}

pub(crate) fn router() -> Router<AppState> {
    Router::new()
        .route("/v1/orgs", get(list_mine))
        .route(
            "/v1/orgs/{id}",
            get(get_org).patch(patch_org).delete(delete_org),
        )
        .route("/v1/orgs/{id}/transfer", post(transfer))
        .route("/v1/orgs/{id}/me/mailbox", post(my_mailbox))
        .route("/v1/orgs/{id}/directory", get(directory))
        .route("/v1/orgs/{id}/billing", get(billing))
        .route("/v1/orgs/{id}/members", get(members))
        .route("/v1/orgs/{id}/members/{member}", patch(patch_member))
        .route("/v1/orgs/{id}/members/{member}/remove", post(remove_member))
        .route("/v1/orgs/{id}/invites", post(invite))
        .route("/v1/orgs/{id}/invites/{invite}/resend", post(resend))
        .route("/v1/orgs/{id}/invites/{invite}", delete(revoke_invite))
        .route("/v1/org-invites/preview", post(preview))
        .route("/v1/org-invites/accept", post(accept))
        .route("/v1/orgs/{id}/rooms", get(rooms).post(create_room))
        .route(
            "/v1/orgs/{id}/rooms/{room}",
            patch(patch_room).delete(delete_room),
        )
        .route("/v1/orgs/{id}/rules", get(rules).post(create_rule))
        .route("/v1/orgs/{id}/rules/evaluate", post(evaluate))
        .route(
            "/v1/orgs/{id}/rules/{rule}",
            patch(patch_rule).delete(delete_rule),
        )
        .route("/v1/orgs/{id}/domains", get(domains).post(add_domain))
        .route("/v1/orgs/{id}/domains/{domain}/verify", post(verify_domain))
        .route("/v1/orgs/{id}/domains/{domain}", delete(delete_domain))
        .route("/v1/orgs/{id}/audit", get(audit))
        .route("/v1/orgs/{id}/audit/export", get(audit_export))
        .merge(crate::org_agents::router())
}

type Body = Result<Json<Value>, JsonRejection>;

fn ip(e: &Extension<ClientIp>) -> Option<String> {
    Some(e.0.as_str().to_string())
}

pub(crate) fn text(field: &str, v: &str, max: usize) -> GResult<String> {
    let t = v.trim();
    if t.is_empty() || t.chars().count() > max || t.chars().any(char::is_control) {
        return Err(GErr::bad(
            "bad_request",
            format!("{field} must be 1 to {max} printable characters"),
        ));
    }
    Ok(t.to_string())
}

fn parse_roles(list: &[String]) -> GResult<Roles> {
    let mut out = Roles::default();
    for r in list {
        let role = Role::parse(r.trim())
            .ok_or_else(|| GErr::bad("invalid_role", format!("unknown role '{r}'")))?;
        if role == Role::Owner {
            return Err(GErr::bad(
                "invalid_role",
                "ownership moves only by transfer",
            ));
        }
        out.0.insert(role);
    }
    Ok(out)
}

/// Is the org feature on for this org (its owner's account, or the caller's)?
pub(crate) async fn org_flag(state: &AppState, accounts: &[&str]) -> bool {
    for a in accounts {
        if crate::filtering::feature_on(state, Some(a), capabilities::ORGS).await {
            return true;
        }
    }
    false
}

// ---- organizations -----------------------------------------------------------------------------

/// `GET /v1/orgs`
async fn list_mine(State(state): State<AppState>, headers: HeaderMap) -> Response {
    let result = async {
        let principal = crate::principal_for_token(&state, crate::bearer(&headers)).await?;
        let account = match &principal {
            crate::Principal::Account(a, _) => a.clone(),
            crate::Principal::Identity(_) => return Err(GErr::forbidden("account_required", "sign in to list your organizations")),
        };
        let acct = account.clone();
        let now = now_unix();
        let (orgs, operated) = state
            .store
            .group_read(move |c: &Connection| -> GResult<(Vec<(String, Value)>, Vec<Value>)> {
                let mut stmt = c.prepare(
                    "SELECT m.org_id, m.id, m.identity, m.handle, o.owner_account FROM org_members m
                       JOIN orgs o ON o.id = m.org_id
                      WHERE m.account_id = ?1 AND m.status = 'active'
                        AND o.status IN ('active','past_due','read_only','suspended')",
                )?;
                let rows: Vec<(String, String, Option<String>, Option<String>, String)> = stmt
                    .query_map([&acct], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?, r.get(4)?)))?
                    .collect::<Result<_, _>>()?;
                let mut out = Vec::new();
                for (oid, mid, identity, handle, owner) in rows {
                    let Some(o) = load_org(c, &oid)? else { continue };
                    let mut v = org_json(c, &o, now)?;
                    v["my_roles"] = json!(org::roles_of(c, &oid, &mid)?.names());
                    v["my_member_id"] = json!(mid);
                    v["my_mailbox"] = match identity {
                        Some(a) => json!({ "address": a, "handle": handle }),
                        None => Value::Null,
                    };
                    out.push((owner, v));
                }
                let mut stmt = c.prepare(
                    "SELECT a.identity, a.handle, d.org_id, d.capabilities FROM org_delegations d
                       JOIN org_members m ON m.org_id = d.org_id AND m.id = d.grantee_member_id
                       JOIN org_agents a ON a.org_id = d.org_id AND a.identity = d.target_identity
                      WHERE m.account_id = ?1 AND m.status = 'active' AND a.status = 'active'
                        AND d.revoked_at IS NULL AND (d.expires_at IS NULL OR d.expires_at > ?2)",
                )?;
                let operated: Vec<Value> = stmt
                    .query_map(params![acct, now as i64], |r| {
                        let caps: String = r.get(3)?;
                        Ok(json!({
                            "address": r.get::<_, String>(0)?,
                            "handle": r.get::<_, String>(1)?,
                            "org_id": r.get::<_, String>(2)?,
                            "capabilities": serde_json::from_str::<Value>(&caps).unwrap_or(Value::Null),
                        }))
                    })?
                    .collect::<Result<_, _>>()?;
                Ok((out, operated))
            })
            .await?;
        let mut visible = Vec::new();
        for (owner, v) in orgs {
            if org_flag(&state, &[&account, &owner]).await {
                visible.push(v);
            }
        }
        let ids: Vec<String> = visible.iter().filter_map(|v| v["id"].as_str().map(str::to_string)).collect();
        let operated: Vec<Value> = operated
            .into_iter()
            .filter(|o| o["capabilities"].as_array().is_some_and(|a| a.iter().any(|c| c == "read_inbox")))
            .filter(|o| o["org_id"].as_str().is_some_and(|id| ids.iter().any(|i| i == id)))
            .collect();
        Ok(Outcome::ok(json!({ "orgs": visible, "operated": operated })))
    }
    .await;
    finish(&state, result)
}

/// `GET /v1/orgs/{id}`
async fn get_org(
    State(state): State<AppState>,
    headers: HeaderMap,
    Path(id): Path<String>,
) -> Response {
    let result = async {
        let ctx = org::org_ctx(&state, &headers, &id).await?;
        let mid = ctx.member.id.clone();
        let now = now_unix();
        let roles = ctx.roles.names();
        let mailbox = ctx
            .member
            .identity
            .clone()
            .map(|a| json!({ "address": a, "handle": ctx.member.handle }));
        let body = state
            .store
            .group_read(move |c: &Connection| -> GResult<Value> {
                let o = load_org(c, &id)?.ok_or_else(GErr::not_found)?;
                Ok(json!({
                    "org": org_json(c, &o, now)?,
                    "me": { "member_id": mid, "roles": roles, "mailbox": mailbox },
                }))
            })
            .await?;
        Ok(Outcome::ok(body))
    }
    .await;
    finish(&state, result)
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct PatchOrg {
    #[serde(default)]
    name: Option<String>,
    #[serde(default)]
    settings: Option<PatchSettings>,
    #[serde(default)]
    expected_revision: Option<i64>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct PatchSettings {
    #[serde(default)]
    require_mfa: Option<bool>,
    #[serde(default)]
    invite_domains_only: Option<bool>,
    #[serde(default)]
    rooms_creation: Option<String>,
    #[serde(default)]
    ssh_allowed: Option<bool>,
    #[serde(default)]
    ssh_requires_approval: Option<bool>,
}

/// `PATCH /v1/orgs/{id}`
async fn patch_org(
    State(state): State<AppState>,
    Extension(cip): Extension<ClientIp>,
    headers: HeaderMap,
    Path(id): Path<String>,
    body: Body,
) -> Response {
    let result = async {
        let ctx = org::org_ctx(&state, &headers, &id).await?;
        ctx.require(&state, Perm::Security)?;
        let (req, _): (PatchOrg, String) = parse(body)?;
        let name = req.name.as_deref().map(|n| text("name", n, 120)).transpose()?;
        let mut settings = ctx.org.settings.clone();
        let mut changed = Vec::new();
        if let Some(s) = &req.settings {
            if let Some(v) = s.require_mfa {
                if v && !settings.require_mfa {
                    if !state.step_up.configured() {
                        return Err(GErr::conflict(
                            "step_up_unavailable",
                            "two-step sign-in is not configured on this server yet",
                        ));
                    }
                    // Whoever turns it on must pass it themselves, so they cannot lock everyone out.
                    ctx.step_up(&state)?;
                }
                settings.require_mfa = v;
                changed.push("require_mfa");
            }
            if let Some(v) = s.invite_domains_only {
                settings.invite_domains_only = v;
                changed.push("invite_domains_only");
            }
            if let Some(v) = &s.rooms_creation {
                if !matches!(v.as_str(), "everyone" | "admins") {
                    return Err(GErr::bad("bad_request", "rooms_creation is everyone or admins"));
                }
                settings.rooms_creation = v.clone();
                changed.push("rooms_creation");
            }
            // Allowing shells into the org's agents (SDS §11.5) is a security decision: whoever
            // turns it on, or removes the approval requirement, signs in strongly first.
            if let Some(v) = s.ssh_allowed {
                if v && !settings.ssh_allowed {
                    ctx.step_up(&state)?;
                }
                settings.ssh_allowed = v;
                changed.push("ssh_allowed");
            }
            if let Some(v) = s.ssh_requires_approval {
                if !v && settings.ssh_requires_approval {
                    ctx.step_up(&state)?;
                }
                settings.ssh_requires_approval = v;
                changed.push("ssh_requires_approval");
            }
        }
        if name.is_some() {
            changed.push("name");
        }
        let auditor = state.auditor.clone();
        let account = ctx.account.clone();
        let ipaddr = ip(&Extension(cip));
        let now = now_unix();
        state
            .store
            .group_tx(move |tx| -> GResult<Outcome> {
                let o = load_org(tx, &id)?.ok_or_else(GErr::not_found)?;
                if let Some(rev) = req.expected_revision {
                    if rev != o.revision {
                        return Err(GErr::conflict("revision_conflict", "the organization changed; reload and retry")
                            .with("revision", o.revision));
                    }
                }
                if settings.invite_domains_only {
                    let verified: bool = tx.query_row(
                        "SELECT EXISTS(SELECT 1 FROM org_domains WHERE org_id = ?1 AND verified_at IS NOT NULL)",
                        [&id],
                        |r| r.get(0),
                    )?;
                    if !verified {
                        return Err(GErr::conflict(
                            "domain_not_verified",
                            "verify at least one domain before limiting invitations to verified domains",
                        ));
                    }
                }
                tx.execute(
                    "UPDATE orgs SET name = COALESCE(?2, name), settings = ?3, revision = revision + 1 WHERE id = ?1",
                    params![id, name, serde_json::to_string(&settings).unwrap_or_default()],
                )?;
                org::audit_in(tx, &auditor, &id, ActorKind::Human, Some(&account), "org.update", Some(&id),
                    json!({ "changed": changed }), ipaddr.as_deref(), now)?;
                let o = load_org(tx, &id)?.ok_or_else(GErr::not_found)?;
                Ok(Outcome::ok(json!({ "org": org_json(tx, &o, now)? })))
            })
            .await
    }
    .await;
    // Authority may have changed: re-check open SSH sessions now (SDS §11.5).
    crate::shell::sweep_soon(&state);
    finish(&state, result)
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct DeleteOrg {
    confirm_namespace: String,
}

/// `DELETE /v1/orgs/{id}` — owner, step-up, typed confirmation (SDS §6.6 step 5).
async fn delete_org(
    State(state): State<AppState>,
    Extension(cip): Extension<ClientIp>,
    headers: HeaderMap,
    Path(id): Path<String>,
    body: Body,
) -> Response {
    let result = async {
        let ctx = org::org_ctx(&state, &headers, &id).await?;
        ctx.require(&state, Perm::DeleteOrg)?;
        let (req, _): (DeleteOrg, String) = parse(body)?;
        let confirm = req.confirm_namespace.trim().trim_start_matches('/').to_ascii_lowercase();
        if confirm != ctx.org.namespace {
            return Err(GErr::bad("confirmation_mismatch", "type the organization's namespace to confirm"));
        }
        let auditor = state.auditor.clone();
        let account = ctx.account.clone();
        let ipaddr = ip(&Extension(cip));
        let now = now_unix();
        state
            .store
            .group_tx(move |tx| -> GResult<Outcome> {
                let o = load_org(tx, &id)?.ok_or_else(GErr::not_found)?;
                let internal = o.internal_account.clone().unwrap_or_default();
                let mut out = Outcome::default();
                // Members' org mailboxes leave their accounts; history stays with the detached
                // mailbox (D-15) and nothing is readable by anyone else.
                let members: Vec<(String, Option<String>, Option<String>)> = {
                    let mut stmt = tx.prepare(
                        "SELECT id, identity, account_id FROM org_members WHERE org_id = ?1 AND status <> 'removed'",
                    )?;
                    let rows = stmt.query_map([&id], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)))?;
                    rows.collect::<Result<_, _>>()?
                };
                for (mid, identity, account) in &members {
                    if let Some(addr) = identity {
                        crate::org_rooms::remove_everywhere_in(tx, addr, "system", now, &mut out)?;
                        tx.execute(
                            "UPDATE identities SET account_id = ?2, handle = NULL WHERE address = ?1",
                            params![addr, internal],
                        )?;
                    }
                    if let Some(a) = account {
                        org::emit_to(tx, a, "org.member", &json!({ "org_id": id, "member_id": mid, "status": "removed" }), now)?;
                        out.wake.push(a.clone());
                    }
                }
                tx.execute(
                    "UPDATE org_members SET status = 'removed', removed_at = ?2, authority_revision = authority_revision + 1
                      WHERE org_id = ?1 AND status <> 'removed'",
                    params![id, now as i64],
                )?;
                crate::org_agents::revoke_all_devices_in(tx, &id, None, now)?;
                tx.execute(
                    "UPDATE org_delegations SET revoked_at = ?2 WHERE org_id = ?1 AND revoked_at IS NULL",
                    params![id, now as i64],
                )?;
                tx.execute(
                    "UPDATE org_invites SET revoked_at = ?2 WHERE org_id = ?1 AND used_at IS NULL AND revoked_at IS NULL",
                    params![id, now as i64],
                )?;
                // Rooms go too.
                let rooms: Vec<String> = {
                    let mut stmt = tx.prepare("SELECT id FROM groups WHERE org_id = ?1 AND deleted_at IS NULL")?;
                    let rows = stmt.query_map([&id], |r| r.get(0))?;
                    rows.collect::<Result<_, _>>()?
                };
                for gid in rooms {
                    let g = crate::groups::live_group(tx, &gid)?;
                    out.wake.extend(crate::groups::delete_in(tx, &g, "system", now)?);
                }
                // The namespace enters the 30-day recovery window, then is released.
                tx.execute(
                    "UPDATE namespaces SET expires_at = ?2, release_at = ?3 WHERE namespace = ?1",
                    params![o.namespace, now as i64, now as i64 + 30 * 86_400],
                )?;
                tx.execute(
                    "UPDATE orgs SET status = 'deleted', deleted_at = ?2, status_since = ?2, revision = revision + 1 WHERE id = ?1",
                    params![id, now as i64],
                )?;
                org::audit_in(tx, &auditor, &id, ActorKind::Human, Some(&account), "org.delete", Some(&id),
                    json!({ "namespace": o.namespace }), ipaddr.as_deref(), now)?;
                out.status = 200;
                out.body = json!({ "deleted": true, "namespace_release_at": now as i64 + 30 * 86_400 });
                Ok(out)
            })
            .await
    }
    .await;
    // Authority may have changed: re-check open SSH sessions now (SDS §11.5).
    crate::shell::sweep_soon(&state);
    finish(&state, result)
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct TransferReq {
    member_id: String,
}

/// `POST /v1/orgs/{id}/transfer` — owner → an active admin, step-up.
async fn transfer(
    State(state): State<AppState>,
    Extension(cip): Extension<ClientIp>,
    headers: HeaderMap,
    Path(id): Path<String>,
    body: Body,
) -> Response {
    let result = async {
        let ctx = org::org_ctx(&state, &headers, &id).await?;
        ctx.require(&state, Perm::TransferOwnership)?;
        let (req, _): (TransferReq, String) = parse(body)?;
        let me = ctx.member.id.clone();
        let account = ctx.account.clone();
        let auditor = state.auditor.clone();
        let ipaddr = ip(&Extension(cip));
        let now = now_unix();
        state
            .store
            .group_tx(move |tx| -> GResult<Outcome> {
                let target = load_member(tx, &id, &req.member_id)?
                    .filter(|m| m.status == "active" && m.id != me)
                    .ok_or_else(|| GErr::bad("invalid_target", "ownership goes to another active member"))?;
                let target_roles = org::roles_of(tx, &id, &target.id)?;
                if !target_roles.has(Role::Admin) {
                    return Err(GErr::bad("invalid_target", "make them an admin first; ownership goes to an admin"));
                }
                let new_owner = target.account_id.clone().ok_or_else(|| GErr::bad("invalid_target", "that member has not joined yet"))?;
                tx.execute("DELETE FROM org_roles WHERE org_id = ?1 AND role = 'owner'", [&id])?;
                tx.execute(
                    "INSERT INTO org_roles (org_id, member_id, role, granted_by, granted_at) VALUES (?1, ?2, 'owner', ?3, ?4)",
                    params![id, target.id, me, now as i64],
                )?;
                // The previous owner stays a full admin.
                tx.execute(
                    "INSERT OR IGNORE INTO org_roles (org_id, member_id, role, granted_by, granted_at) VALUES (?1, ?2, 'admin', ?2, ?3)",
                    params![id, me, now as i64],
                )?;
                tx.execute(
                    "UPDATE org_members SET authority_revision = authority_revision + 1 WHERE org_id = ?1 AND id IN (?2, ?3)",
                    params![id, me, target.id],
                )?;
                tx.execute("UPDATE orgs SET owner_account = ?2, revision = revision + 1 WHERE id = ?1", params![id, new_owner])?;
                org::audit_in(tx, &auditor, &id, ActorKind::Human, Some(&account), "org.transfer", Some(&target.id),
                    json!({ "from_member": me, "to_member": target.id }), ipaddr.as_deref(), now)?;
                Ok(Outcome::ok(json!({ "owner_member_id": target.id })))
            })
            .await
    }
    .await;
    // Authority may have changed: re-check open SSH sessions now (SDS §11.5).
    crate::shell::sweep_soon(&state);
    finish(&state, result)
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct MailboxReq {
    handle: String,
}

/// `POST /v1/orgs/{id}/me/mailbox` — a member without an org address (the owner) picks one; the
/// org's first room, `General`, is created with them as its keying owner.
async fn my_mailbox(
    State(state): State<AppState>,
    Extension(cip): Extension<ClientIp>,
    headers: HeaderMap,
    Path(id): Path<String>,
    body: Body,
) -> Response {
    let result = async {
        let ctx = org::org_ctx(&state, &headers, &id).await?;
        ctx.require_writable()?;
        if ctx.member.identity.is_some() {
            return Err(GErr::conflict("already_named", "you already have an address in this organization"));
        }
        let (req, _): (MailboxReq, String) = parse(body)?;
        let handle = org::org_handle(&ctx.org, &req.handle)?;
        let vault = state.vault.clone();
        let auditor = state.auditor.clone();
        let account = ctx.account.clone();
        let mid = ctx.member.id.clone();
        let ipaddr = ip(&Extension(cip));
        let now = now_unix();
        state
            .store
            .group_tx(move |tx| -> GResult<Outcome> {
                let m = load_member(tx, &id, &mid)?.ok_or_else(GErr::not_found)?;
                if m.identity.is_some() {
                    return Err(GErr::conflict("already_named", "you already have an address in this organization"));
                }
                if !org::handle_free_in(tx, &id, &handle, false)? {
                    return Err(GErr::conflict("handle_taken", format!("{handle} is taken")));
                }
                let address = org::mint_identity_in(tx, &vault, &handle, &account, None, now)?;
                tx.execute(
                    "UPDATE org_members SET identity = ?2, handle = ?3, joined_at = COALESCE(joined_at, ?4) WHERE id = ?1",
                    params![mid, address, handle, now as i64],
                )?;
                let mut out = Outcome::default();
                let has_rooms: bool = tx.query_row(
                    "SELECT EXISTS(SELECT 1 FROM groups WHERE org_id = ?1 AND deleted_at IS NULL)",
                    [&id],
                    |r| r.get(0),
                )?;
                let general = if has_rooms {
                    crate::org_rooms::auto_join_in(tx, &id, &address, now, &mut out)?;
                    None
                } else {
                    Some(crate::org_rooms::create_room_in(
                        tx,
                        &crate::org_rooms::NewRoom {
                            org_id: &id,
                            name: "General",
                            description: Some("Everyone at the company"),
                            join_policy: "public",
                            post_policy: "everyone",
                            listed: true,
                            auto_join: true,
                            creator: &address,
                            creator_account: Some(&account),
                        },
                        now,
                    )?)
                };
                org::audit_in(tx, &auditor, &id, ActorKind::Human, Some(&account), "member.mailbox", Some(&mid),
                    json!({ "handle": handle }), ipaddr.as_deref(), now)?;
                out.status = 201;
                out.body = json!({ "mailbox": { "address": address, "handle": handle }, "general_room": general });
                out.wake.push(account.clone());
                Ok(out)
            })
            .await
    }
    .await;
    finish(&state, result)
}

#[derive(Deserialize, Default)]
struct DirectoryQuery {
    #[serde(default)]
    q: Option<String>,
    #[serde(default)]
    limit: Option<usize>,
}

/// `GET /v1/orgs/{id}/directory`
async fn directory(
    State(state): State<AppState>,
    headers: HeaderMap,
    Path(id): Path<String>,
    Query(q): Query<DirectoryQuery>,
) -> Response {
    let result = async {
        let ctx = org::org_ctx(&state, &headers, &id).await?;
        rate_limit(&format!("orgdir:{}", ctx.account), 120, 60)?;
        let needle = q.q.unwrap_or_default().trim().to_lowercase().replace(['%', '_'], "");
        let limit = q.limit.unwrap_or(50).clamp(1, 50) as i64;
        let pattern = format!("%{}%", needle.chars().take(80).collect::<String>());
        let entries = state
            .store
            .group_read(move |c: &Connection| -> GResult<Vec<Value>> {
                let mut stmt = c.prepare(
                    "SELECT handle, identity, 'member', trim(first_name || ' ' || last_name) FROM org_members
                      WHERE org_id = ?1 AND status = 'active' AND identity IS NOT NULL
                        AND (lower(handle) LIKE ?2 OR lower(first_name || ' ' || last_name) LIKE ?2)
                     UNION ALL
                     SELECT handle, identity, 'agent', label FROM org_agents
                      WHERE org_id = ?1 AND status = 'active' AND (lower(handle) LIKE ?2 OR lower(coalesce(label,'')) LIKE ?2)
                     ORDER BY 1 LIMIT ?3",
                )?;
                let rows = stmt.query_map(params![id, pattern, limit], |r| {
                    Ok(json!({
                        "handle": r.get::<_, String>(0)?,
                        "address": r.get::<_, String>(1)?,
                        "kind": r.get::<_, String>(2)?,
                        "name": r.get::<_, Option<String>>(3)?.filter(|n| !n.is_empty()),
                    }))
                })?;
                Ok(rows.collect::<Result<_, _>>()?)
            })
            .await?;
        Ok(Outcome::ok(json!({ "entries": entries })))
    }
    .await;
    finish(&state, result)
}

/// `GET /v1/orgs/{id}/billing`
async fn billing(
    State(state): State<AppState>,
    headers: HeaderMap,
    Path(id): Path<String>,
) -> Response {
    let result = async {
        let ctx = org::org_ctx(&state, &headers, &id).await?;
        ctx.require(&state, Perm::ViewBilling)?;
        let now = now_unix();
        let ents = state.store.entitlements_for("org", id.clone(), now).await?;
        let best = ents
            .iter()
            .filter(|e| e.active)
            .max_by_key(|e| e.seats.unwrap_or(0))
            .or(ents.first());
        let o = ctx.org.clone();
        let body = state
            .store
            .group_read(move |c: &Connection| -> GResult<Value> {
                Ok(json!({
                    "plan_code": o.plan_code,
                    "seats": o.seats,
                    "seats_used": org::seats_used(c, &o.id, now)?,
                    "agent_limit": o.agent_limit,
                    "agents_used": org::agents_used(c, &o.id)?,
                    "status": o.status,
                    "period_end": o.period_end,
                    "storage_bytes": o.storage_bytes,
                }))
            })
            .await?;
        let mut body = body;
        body["paid_through"] = json!(best.and_then(|e| e.paid_through));
        body["entitlement_status"] = json!(best.map(|e| e.status.clone()));
        body["cancel_requested_at"] = json!(best.and_then(|e| e.cancel_requested_at));
        body["source"] = json!(best.map(|e| e.source.clone()));
        body["can_manage"] = json!(org::permits(&ctx.roles, Perm::ManageBilling, false));
        Ok(Outcome::ok(body))
    }
    .await;
    finish(&state, result)
}

// ---- people ------------------------------------------------------------------------------------

#[derive(Deserialize, Default)]
struct MembersQuery {
    #[serde(default)]
    status: Option<String>,
}

/// `GET /v1/orgs/{id}/members`
async fn members(
    State(state): State<AppState>,
    headers: HeaderMap,
    Path(id): Path<String>,
    Query(q): Query<MembersQuery>,
) -> Response {
    let result = async {
        let ctx = org::org_ctx(&state, &headers, &id).await?;
        // Members' list with contact hints and statuses is people-admin metadata (SDS §3.7).
        let full = org::permits(&ctx.roles, Perm::ManagePeople, false) || org::permits(&ctx.roles, Perm::Security, false);
        if !full {
            return Err(GErr::forbidden("forbidden", "only people admins see the member list; use the directory"));
        }
        let status = q.status;
        let now = now_unix();
        let list = state
            .store
            .group_read(move |c: &Connection| -> GResult<Vec<Value>> {
                let mut stmt = c.prepare(&format!(
                    "SELECT {} FROM org_members WHERE org_id = ?1 AND status <> 'removed'
                       AND (?2 IS NULL OR status = ?2) ORDER BY joined_at IS NULL, joined_at, invited_at",
                    org::MEMBER_COLS
                ))?;
                let rows: Vec<org::MemberRow> = stmt
                    .query_map(params![id, status], org::map_member)?
                    .collect::<Result<_, _>>()?;
                rows.iter().map(|m| member_json(c, m, now)).collect()
            })
            .await?;
        Ok(Outcome::ok(json!({ "members": list })))
    }
    .await;
    finish(&state, result)
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct InviteRow {
    first_name: String,
    last_name: String,
    contact: String,
    handle: String,
    #[serde(default)]
    roles: Vec<String>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct InvitesReq {
    invites: Vec<InviteRow>,
}

pub(crate) struct CheckedInvite {
    pub first: String,
    pub last: String,
    pub contact: String,
    pub handle: String,
    pub roles: Roles,
}

/// One invitation row, inside the transaction: capacity, address, uniqueness, then the rows.
#[allow(clippy::too_many_arguments)]
pub(crate) fn invite_one(
    tx: &Connection,
    vault: &crate::vault::Vault,
    org_id: &str,
    o: &org::OrgRow,
    inviter_member: &str,
    row: &CheckedInvite,
    deliver: bool,
    now: u64,
) -> GResult<Result<(String, String, String), &'static str>> {
    if org::seats_used(tx, org_id, now)? >= o.seats {
        return Ok(Err("seat_limit"));
    }
    if o.settings.invite_domains_only {
        let ok: bool = tx.query_row(
            "SELECT EXISTS(SELECT 1 FROM org_domains WHERE org_id = ?1 AND domain = ?2 AND verified_at IS NOT NULL)",
            params![org_id, org::contact_domain(&row.contact)],
            |r| r.get(0),
        )?;
        if !ok {
            return Ok(Err("domain_not_verified"));
        }
    }
    let contact_hash = vault.keyed_hash("org-invite-contact", row.contact.as_bytes());
    let duplicate: bool = tx.query_row(
        "SELECT EXISTS(SELECT 1 FROM org_members m WHERE m.org_id = ?1 AND m.contact_hash = ?2
            AND (m.status IN ('active','suspended')
                 OR (m.status = 'invited' AND EXISTS (SELECT 1 FROM org_invites i WHERE i.member_id = m.id
                       AND i.used_at IS NULL AND i.revoked_at IS NULL AND i.expires_at > ?3))))",
        params![org_id, contact_hash, now as i64],
        |r| r.get(0),
    )?;
    if duplicate {
        return Ok(Err("already_invited"));
    }
    // An expired invitation's reservation is released here, on reuse of its handle.
    tx.execute(
        "UPDATE org_members SET status = 'removed', removed_at = ?3 WHERE org_id = ?1 AND handle = ?2
            AND status = 'invited' AND NOT EXISTS (SELECT 1 FROM org_invites i WHERE i.member_id = org_members.id
                AND i.used_at IS NULL AND i.revoked_at IS NULL AND i.expires_at > ?3)",
        params![org_id, row.handle, now as i64],
    )?;
    if !org::handle_free_in(tx, org_id, &row.handle, true)? {
        return Ok(Err("handle_taken"));
    }
    tx.execute(
        "DELETE FROM org_handle_holds WHERE org_id = ?1 AND handle = ?2",
        params![org_id, row.handle],
    )?;
    let mid = org::new_id("mem");
    let sealed = vault.seal_bytes(row.contact.as_bytes()).map_err(|_| {
        GErr::new(
            StatusCode::INTERNAL_SERVER_ERROR,
            "vault_error",
            "vault_error",
        )
    })?;
    tx.execute(
        "INSERT INTO org_members (id, org_id, handle, first_name, last_name, contact_hash, contact_sealed,
                contact_masked, status, invited_by, invited_at)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, 'invited', ?9, ?10)",
        params![
            mid,
            org_id,
            row.handle,
            row.first,
            row.last,
            contact_hash,
            sealed,
            org::mask_contact(&row.contact),
            inviter_member,
            now as i64
        ],
    )?;
    let (inv, link) = new_invite_in(
        tx,
        vault,
        org_id,
        &mid,
        &row.handle,
        &contact_hash,
        &row.roles,
        inviter_member,
        deliver,
        now,
    )?;
    Ok(Ok((mid, inv, link)))
}

/// Issue a fresh invitation token for a member row. The token is never stored.
#[allow(clippy::too_many_arguments)]
pub(crate) fn new_invite_in(
    tx: &Connection,
    vault: &crate::vault::Vault,
    org_id: &str,
    member_id: &str,
    handle: &str,
    contact_hash: &str,
    roles: &Roles,
    by: &str,
    deliver: bool,
    now: u64,
) -> GResult<(String, String)> {
    let token = org::new_secret();
    let inv = org::new_id("inv");
    tx.execute(
        "INSERT INTO org_invites (id, org_id, member_id, handle, token_hash, contact_hash, roles, created_by,
                created_at, expires_at)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10)",
        params![
            inv,
            org_id,
            member_id,
            handle,
            org::secret_hash(&token),
            contact_hash,
            json!(roles.names()).to_string(),
            by,
            now as i64,
            now as i64 + org::INVITE_TTL_SECS as i64
        ],
    )?;
    let link = format!("{}/invite#{token}", enterprise_url());
    if deliver {
        crate::org_notify::enqueue_in(tx, vault, &inv, org_id, &link, now)?;
    }
    Ok((inv, link))
}

/// `POST /v1/orgs/{id}/invites`
async fn invite(
    State(state): State<AppState>,
    Extension(cip): Extension<ClientIp>,
    headers: HeaderMap,
    Path(id): Path<String>,
    body: Body,
) -> Response {
    let result = async {
        let ctx = org::org_ctx(&state, &headers, &id).await?;
        ctx.require(&state, Perm::ManagePeople)?;
        ctx.require_writable()?;
        rate_limit(&format!("orginv:{id}"), 1_000, 86_400)?;
        let (req, _): (InvitesReq, String) = parse(body)?;
        if req.invites.is_empty() || req.invites.len() > org::INVITES_PER_REQUEST {
            return Err(GErr::bad("bad_request", format!("send 1 to {} invitations", org::INVITES_PER_REQUEST)));
        }
        let mut rows: Vec<Result<CheckedInvite, (String, &'static str)>> = Vec::new();
        for r in req.invites {
            let handle_echo = r.handle.clone();
            let checked = (|| -> GResult<CheckedInvite> {
                let roles = parse_roles(&r.roles)?;
                for role in &roles.0 {
                    if !may_grant(&ctx.roles, *role) {
                        return Err(GErr::forbidden("forbidden", "role"));
                    }
                }
                // Inviting a full admin is the owner's sensitive action.
                if roles.has(Role::Admin) {
                    ctx.step_up(&state)?;
                }
                Ok(CheckedInvite {
                    first: text("first_name", &r.first_name, 80)?,
                    last: text("last_name", &r.last_name, 80)?,
                    contact: org::canonical_contact(&r.contact)
                        .ok_or_else(|| GErr::bad("invalid_contact", "contact"))?,
                    handle: org::org_handle(&ctx.org, &r.handle)?,
                    roles,
                })
            })();
            rows.push(checked.map_err(|e| (handle_echo, e.code)));
        }
        if let Some(Err((_, "step_up_required"))) = rows.iter().find(|r| matches!(r, Err((_, "step_up_required")))) {
            ctx.step_up(&state)?;
        }
        let deliver = org_flag_named(&state, &ctx, capabilities::ORG_INVITE_EMAIL).await;
        let vault = state.vault.clone();
        let auditor = state.auditor.clone();
        let account = ctx.account.clone();
        let inviter = ctx.member.id.clone();
        let ipaddr = ip(&Extension(cip));
        let now = now_unix();
        state
            .store
            .group_tx(move |tx| -> GResult<Outcome> {
                let o = load_org(tx, &id)?.ok_or_else(GErr::not_found)?;
                let mut results = Vec::new();
                for row in &rows {
                    match row {
                        Err((handle, code)) => results.push(json!({ "status": code, "handle": handle })),
                        Ok(row) => match invite_one(tx, &vault, &id, &o, &inviter, row, deliver, now)? {
                            Err(code) => results.push(json!({ "status": code, "handle": row.handle })),
                            Ok((mid, inv, link)) => {
                                org::audit_in(tx, &auditor, &id, ActorKind::Human, Some(&account), "member.invite",
                                    Some(&mid), json!({ "handle": row.handle, "roles": row.roles.names(), "invite": inv }),
                                    ipaddr.as_deref(), now)?;
                                results.push(json!({
                                    "status": "invited", "member_id": mid, "invite_id": inv, "handle": row.handle,
                                    "link": link, "expires_at": now as i64 + org::INVITE_TTL_SECS as i64,
                                    "delivery": if deliver { "queued" } else { "copy_link" },
                                }));
                            }
                        },
                    }
                }
                Ok(Outcome::ok(json!({ "results": results })))
            })
            .await
    }
    .await;
    no_store(finish(&state, result))
}

/// Responses carrying an invitation link are never cached.
fn no_store(mut r: Response) -> Response {
    r.headers_mut()
        .insert(header::CACHE_CONTROL, HeaderValue::from_static("no-store"));
    r
}

/// Is a named flag on for this org (owner's account) or the caller?
async fn org_flag_named(state: &AppState, ctx: &OrgCtx, name: &'static str) -> bool {
    crate::filtering::feature_on(state, Some(&ctx.org.owner_account), name).await
        || crate::filtering::feature_on(state, Some(&ctx.account), name).await
}

/// `POST /v1/orgs/{id}/invites/{invite}/resend`
async fn resend(
    State(state): State<AppState>,
    Extension(cip): Extension<ClientIp>,
    headers: HeaderMap,
    Path((id, invite_id)): Path<(String, String)>,
) -> Response {
    let result = async {
        let ctx = org::org_ctx(&state, &headers, &id).await?;
        ctx.require(&state, Perm::ManagePeople)?;
        ctx.require_writable()?;
        rate_limit(&format!("orgresend:{invite_id}"), 5, 3_600)?;
        let deliver = org_flag_named(&state, &ctx, capabilities::ORG_INVITE_EMAIL).await;
        let vault = state.vault.clone();
        let auditor = state.auditor.clone();
        let account = ctx.account.clone();
        let roles_actor = ctx.roles.clone();
        let me = ctx.member.id.clone();
        let ipaddr = ip(&Extension(cip));
        let now = now_unix();
        state
            .store
            .group_tx(move |tx| -> GResult<Outcome> {
                let prior: Option<(String, String, String, String, Option<i64>)> = tx
                    .query_row(
                        "SELECT member_id, handle, contact_hash, roles, used_at FROM org_invites WHERE id = ?1 AND org_id = ?2",
                        params![invite_id, id],
                        |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?, r.get(4)?)),
                    )
                    .optional()?;
                let Some((mid, handle, contact_hash, roles, used)) = prior else {
                    return Err(GErr::not_found());
                };
                let m = load_member(tx, &id, &mid)?.ok_or_else(GErr::not_found)?;
                if used.is_some() || m.status != "invited" {
                    return Err(GErr::conflict("invite_used", "this invitation was already accepted or withdrawn"));
                }
                let roles: Vec<String> = serde_json::from_str(&roles).unwrap_or_default();
                let roles = parse_roles(&roles)?;
                // The resender must still be allowed to hand out what the invitation grants.
                if !can_manage(&roles_actor, &roles, false) || roles.0.iter().any(|r| !may_grant(&roles_actor, *r)) {
                    return Err(GErr::forbidden("forbidden", "your role cannot resend this invitation"));
                }
                // Revoke every older token first, then recheck capacity (an expired invitation no
                // longer holds its seat).
                tx.execute(
                    "UPDATE org_invites SET revoked_at = ?2 WHERE member_id = ?1 AND used_at IS NULL AND revoked_at IS NULL",
                    params![mid, now as i64],
                )?;
                let o = load_org(tx, &id)?.ok_or_else(GErr::not_found)?;
                if org::seats_used(tx, &id, now)? >= o.seats {
                    return Err(GErr::conflict("seat_limit", "every seat is in use").with("billing", "/app/billing"));
                }
                let (inv, link) = new_invite_in(tx, &vault, &id, &mid, &handle, &contact_hash, &roles, &me, deliver, now)?;
                org::audit_in(tx, &auditor, &id, ActorKind::Human, Some(&account), "member.invite_resend", Some(&mid),
                    json!({ "invite": inv, "replaces": invite_id }), ipaddr.as_deref(), now)?;
                Ok(Outcome::ok(json!({
                    "invite_id": inv, "link": link, "expires_at": now as i64 + org::INVITE_TTL_SECS as i64,
                    "delivery": if deliver { "queued" } else { "copy_link" },
                })))
            })
            .await
    }
    .await;
    no_store(finish(&state, result))
}

/// `DELETE /v1/orgs/{id}/invites/{invite}`
async fn revoke_invite(
    State(state): State<AppState>,
    Extension(cip): Extension<ClientIp>,
    headers: HeaderMap,
    Path((id, invite_id)): Path<(String, String)>,
) -> Response {
    let result = async {
        let ctx = org::org_ctx(&state, &headers, &id).await?;
        ctx.require(&state, Perm::ManagePeople)?;
        let auditor = state.auditor.clone();
        let account = ctx.account.clone();
        let ipaddr = ip(&Extension(cip));
        let now = now_unix();
        state
            .store
            .group_tx(move |tx| -> GResult<Outcome> {
                let mid: Option<String> = tx
                    .query_row(
                        "SELECT member_id FROM org_invites WHERE id = ?1 AND org_id = ?2 AND used_at IS NULL",
                        params![invite_id, id],
                        |r| r.get(0),
                    )
                    .optional()?;
                let Some(mid) = mid else { return Err(GErr::not_found()) };
                tx.execute(
                    "UPDATE org_invites SET revoked_at = ?2 WHERE member_id = ?1 AND used_at IS NULL AND revoked_at IS NULL",
                    params![mid, now as i64],
                )?;
                tx.execute(
                    "UPDATE org_members SET status = 'removed', removed_at = ?2 WHERE id = ?1 AND status = 'invited'",
                    params![mid, now as i64],
                )?;
                tx.execute(
                    "UPDATE org_invite_deliveries SET state = 'cancelled', link_sealed = x'' WHERE invite_id IN
                        (SELECT id FROM org_invites WHERE member_id = ?1) AND state = 'pending'",
                    [&mid],
                )?;
                org::audit_in(tx, &auditor, &id, ActorKind::Human, Some(&account), "member.invite_revoke", Some(&mid),
                    json!({ "invite": invite_id }), ipaddr.as_deref(), now)?;
                Ok(Outcome::ok(json!({ "revoked": true })))
            })
            .await
    }
    .await;
    finish(&state, result)
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct PatchMember {
    #[serde(default)]
    first_name: Option<String>,
    #[serde(default)]
    last_name: Option<String>,
    #[serde(default)]
    roles: Option<Vec<String>>,
    #[serde(default)]
    status: Option<String>,
}

/// `PATCH /v1/orgs/{id}/members/{member}`
async fn patch_member(
    State(state): State<AppState>,
    Extension(cip): Extension<ClientIp>,
    headers: HeaderMap,
    Path((id, target)): Path<(String, String)>,
    body: Body,
) -> Response {
    let result = async {
        let ctx = org::org_ctx(&state, &headers, &id).await?;
        let (req, _): (PatchMember, String) = parse(body)?;
        let wants_people = req.first_name.is_some() || req.last_name.is_some() || req.status.is_some();
        if wants_people {
            ctx.require(&state, Perm::ManagePeople)?;
        }
        let new_roles = req.roles.as_deref().map(parse_roles).transpose()?;
        if new_roles.is_some() && !wants_people {
            // A role change needs at least scoped-admin authority; the per-role checks follow.
            ctx.require(&state, Perm::GrantScopedAdmin)?;
        }
        if let Some(s) = &req.status {
            if !matches!(s.as_str(), "active" | "suspended") {
                return Err(GErr::bad("bad_request", "status is active or suspended"));
            }
        }
        let first = req.first_name.as_deref().map(|v| text("first_name", v, 80)).transpose()?;
        let last = req.last_name.as_deref().map(|v| text("last_name", v, 80)).transpose()?;
        let actor_roles = ctx.roles.clone();
        let me = ctx.member.id.clone();
        // Touching a full admin role is the owner's sensitive action (§3.4).
        let current_target_roles = {
            let (id2, t2) = (id.clone(), target.clone());
            state
                .store
                .group_read(move |c: &Connection| -> GResult<Roles> { org::roles_of(c, &id2, &t2) })
                .await?
        };
        if let Some(nr) = &new_roles {
            if nr.has(Role::Admin) != current_target_roles.has(Role::Admin) {
                ctx.require(&state, Perm::GrantAdmin)?;
                ctx.step_up(&state)?;
            }
        }
        let auditor = state.auditor.clone();
        let account = ctx.account.clone();
        let ipaddr = ip(&Extension(cip));
        let now = now_unix();
        state
            .store
            .group_tx(move |tx| -> GResult<Outcome> {
                let m = load_member(tx, &id, &target)?
                    .filter(|m| m.status != "removed")
                    .ok_or_else(GErr::not_found)?;
                let target_roles = org::roles_of(tx, &id, &m.id)?;
                if !can_manage(&actor_roles, &target_roles, m.id == me) {
                    return Err(GErr::forbidden("forbidden", "your role cannot change this person"));
                }
                let mut out = Outcome::default();
                let mut changed = Vec::new();
                if first.is_some() || last.is_some() {
                    tx.execute(
                        "UPDATE org_members SET first_name = COALESCE(?2, first_name), last_name = COALESCE(?3, last_name) WHERE id = ?1",
                        params![m.id, first, last],
                    )?;
                    changed.push("name");
                }
                if let Some(nr) = &new_roles {
                    if nr.has(Role::Owner) || target_roles.has(Role::Owner) {
                        return Err(GErr::forbidden("owner_only", "ownership moves only by transfer"));
                    }
                    for role in Role::ALL {
                        let had = target_roles.has(role);
                        let wants = nr.has(role);
                        if had == wants {
                            continue;
                        }
                        if !may_grant(&actor_roles, role) {
                            return Err(GErr::forbidden("forbidden", format!("your role cannot change {}", role.as_str())));
                        }
                        if wants {
                            tx.execute(
                                "INSERT INTO org_roles (org_id, member_id, role, granted_by, granted_at) VALUES (?1, ?2, ?3, ?4, ?5)",
                                params![id, m.id, role.as_str(), me, now as i64],
                            )?;
                        } else {
                            tx.execute(
                                "DELETE FROM org_roles WHERE org_id = ?1 AND member_id = ?2 AND role = ?3",
                                params![id, m.id, role.as_str()],
                            )?;
                        }
                        org::audit_in(tx, &auditor, &id, ActorKind::Human, Some(&account),
                            if wants { "role.grant" } else { "role.revoke" }, Some(&m.id),
                            json!({ "role": role.as_str() }), ipaddr.as_deref(), now)?;
                    }
                    if m.status == "invited" {
                        // An invitation's roles follow the member row.
                        tx.execute(
                            "UPDATE org_invites SET roles = ?2 WHERE member_id = ?1 AND used_at IS NULL AND revoked_at IS NULL",
                            params![m.id, json!(nr.names()).to_string()],
                        )?;
                    }
                    changed.push("roles");
                }
                if let Some(s) = &req.status {
                    if m.status == "invited" {
                        return Err(GErr::conflict("invite_pending", "they have not joined yet; revoke the invitation instead"));
                    }
                    if *s != m.status {
                        tx.execute("UPDATE org_members SET status = ?2 WHERE id = ?1", params![m.id, s])?;
                        if s == "suspended" {
                            crate::org_agents::revoke_member_grants_in(tx, &id, &m.id, now)?;
                        }
                        org::audit_in(tx, &auditor, &id, ActorKind::Human, Some(&account),
                            if s == "suspended" { "member.suspend" } else { "member.reinstate" }, Some(&m.id),
                            json!({}), ipaddr.as_deref(), now)?;
                        changed.push("status");
                    }
                }
                if changed.iter().any(|c| *c == "roles" || *c == "status") {
                    tx.execute(
                        "UPDATE org_members SET authority_revision = authority_revision + 1 WHERE id = ?1",
                        [&m.id],
                    )?;
                }
                if changed.is_empty() {
                    return Err(GErr::bad("bad_request", "nothing to change"));
                }
                let m = load_member(tx, &id, &target)?.ok_or_else(GErr::not_found)?;
                if let Some(a) = &m.account_id {
                    org::emit_to(tx, a, "org.member", &json!({
                        "org_id": id, "member_id": m.id, "status": m.status,
                        "roles": org::roles_of(tx, &id, &m.id)?.names(),
                    }), now)?;
                    out.wake.push(a.clone());
                }
                out.status = 200;
                out.body = json!({ "member": member_json(tx, &m, now)? });
                Ok(out)
            })
            .await
    }
    .await;
    // Authority may have changed: re-check open SSH sessions now (SDS §11.5).
    crate::shell::sweep_soon(&state);
    finish(&state, result)
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct RemoveReq {
    #[serde(default)]
    mode: Option<String>,
}

/// `POST /v1/orgs/{id}/members/{member}/remove` — offboarding (SDS §6.7, D-15).
async fn remove_member(
    State(state): State<AppState>,
    Extension(cip): Extension<ClientIp>,
    headers: HeaderMap,
    Path((id, target)): Path<(String, String)>,
    body: Body,
) -> Response {
    let result = async {
        let ctx = org::org_ctx(&state, &headers, &id).await?;
        ctx.require(&state, Perm::ManagePeople)?;
        let req: RemoveReq = match body {
            Ok(Json(v)) if !v.is_null() => serde_json::from_value(v).map_err(|e| GErr::bad("bad_request", e.to_string()))?,
            _ => RemoveReq { mode: None },
        };
        let mode = req.mode.unwrap_or_else(|| "reserve".into());
        if !matches!(mode.as_str(), "reserve" | "delete") {
            return Err(GErr::bad("bad_request", "mode is reserve or delete"));
        }
        let actor_roles = ctx.roles.clone();
        let me = ctx.member.id.clone();
        let auditor = state.auditor.clone();
        let account = ctx.account.clone();
        let ipaddr = ip(&Extension(cip));
        let internal = ctx.org.internal_account.clone().unwrap_or_default();
        let now = now_unix();
        state
            .store
            .group_tx(move |tx| -> GResult<Outcome> {
                let m = load_member(tx, &id, &target)?
                    .filter(|m| m.status != "removed")
                    .ok_or_else(GErr::not_found)?;
                let target_roles = org::roles_of(tx, &id, &m.id)?;
                if target_roles.has(Role::Owner) {
                    return Err(GErr::forbidden("owner_only", "transfer ownership before removing the owner"));
                }
                if !can_manage(&actor_roles, &target_roles, m.id == me) {
                    return Err(GErr::forbidden("forbidden", "your role cannot remove this person"));
                }
                let mut out = Outcome::default();
                offboard_in(tx, &id, &m, &internal, &mode, &account, now, &mut out)?;
                org::audit_in(tx, &auditor, &id, ActorKind::Human, Some(&account), "member.remove", Some(&m.id),
                    json!({ "mode": mode, "handle": m.handle }), ipaddr.as_deref(), now)?;
                out.status = 200;
                out.body = json!({ "removed": true, "mode": mode, "handle_held": mode == "reserve" && m.handle.is_some() });
                Ok(out)
            })
            .await
    }
    .await;
    // Authority may have changed: re-check open SSH sessions now (SDS §11.5).
    crate::shell::sweep_soon(&state);
    finish(&state, result)
}

/// Offboard one member inside a transaction (SDS §6.7): every grant they held or derived goes, their
/// org mailbox leaves their account (history stays with that detached mailbox, D-15), and the
/// readable address is either held for a successor or released.
#[allow(clippy::too_many_arguments)]
pub(crate) fn offboard_in(
    tx: &Connection,
    org_id: &str,
    m: &org::MemberRow,
    internal_account: &str,
    mode: &str,
    actor_account: &str,
    now: u64,
    out: &mut Outcome,
) -> GResult<()> {
    tx.execute(
        "UPDATE org_members SET status = 'removed', removed_at = ?2, authority_revision = authority_revision + 1 WHERE id = ?1",
        params![m.id, now as i64],
    )?;
    tx.execute(
        "DELETE FROM org_roles WHERE org_id = ?1 AND member_id = ?2",
        params![org_id, m.id],
    )?;
    tx.execute(
        "UPDATE org_invites SET revoked_at = ?2 WHERE member_id = ?1 AND used_at IS NULL AND revoked_at IS NULL",
        params![m.id, now as i64],
    )?;
    crate::org_agents::revoke_member_grants_in(tx, org_id, &m.id, now)?;
    if let Some(addr) = &m.identity {
        crate::org_rooms::remove_everywhere_in(tx, addr, actor_account, now, out)?;
        tx.execute(
            "UPDATE identities SET account_id = ?2, handle = NULL WHERE address = ?1",
            params![addr, internal_account],
        )?;
        tx.execute("DELETE FROM devices WHERE address = ?1", [addr])
            .ok();
    }
    if mode == "reserve" {
        if let Some(h) = &m.handle {
            tx.execute(
                "INSERT OR REPLACE INTO org_handle_holds (org_id, handle, reason, created_at) VALUES (?1, ?2, 'offboarded', ?3)",
                params![org_id, h, now as i64],
            )?;
        }
    }
    if let Some(a) = &m.account_id {
        org::emit_to(
            tx,
            a,
            "org.member",
            &json!({ "org_id": org_id, "member_id": m.id, "status": "removed" }),
            now,
        )?;
        out.wake.push(a.clone());
    }
    Ok(())
}

// ---- invitation preview and acceptance (SDS §7.2) -----------------------------------------------

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct TokenReq {
    token: String,
}

struct InviteFacts {
    invite_id: String,
    org: org::OrgRow,
    member: org::MemberRow,
    roles: Vec<String>,
    expires_at: i64,
    used_by: Option<String>,
    revoked: bool,
    contact_hash: String,
    created_by: String,
}

fn invite_by_token(c: &Connection, token: &str) -> GResult<Option<InviteFacts>> {
    if token.len() != 43
        || !token
            .chars()
            .all(|ch| ch.is_ascii_alphanumeric() || ch == '-' || ch == '_')
    {
        return Ok(None);
    }
    let row: Option<(String, String, String, String, i64, Option<String>, Option<i64>, String, String)> = c
        .query_row(
            "SELECT id, org_id, member_id, roles, expires_at, used_by, revoked_at, contact_hash, created_by
               FROM org_invites WHERE token_hash = ?1",
            [org::secret_hash(token)],
            |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?, r.get(4)?, r.get(5)?, r.get(6)?, r.get(7)?, r.get(8)?)),
        )
        .optional()?;
    let Some((
        invite_id,
        org_id,
        mid,
        roles,
        expires_at,
        used_by,
        revoked,
        contact_hash,
        created_by,
    )) = row
    else {
        return Ok(None);
    };
    let Some(o) = load_org(c, &org_id)? else {
        return Ok(None);
    };
    let Some(member) = load_member(c, &org_id, &mid)? else {
        return Ok(None);
    };
    Ok(Some(InviteFacts {
        invite_id,
        org: o,
        member,
        roles: serde_json::from_str(&roles).unwrap_or_default(),
        expires_at,
        used_by,
        revoked: revoked.is_some(),
        contact_hash,
        created_by,
    }))
}

fn invalid_invite() -> GErr {
    GErr::new(
        StatusCode::NOT_FOUND,
        "invite_invalid",
        "This invitation link isn't valid",
    )
}

/// `POST /v1/org-invites/preview` — no credential; one answer for every problem.
async fn preview(
    State(state): State<AppState>,
    Extension(cip): Extension<ClientIp>,
    body: Body,
) -> Response {
    let result = async {
        rate_limit(&format!("invprev:{}", cip.as_str()), 30, 600)?;
        let (req, _): (TokenReq, String) = parse(body)?;
        let now = now_unix();
        let token = req.token.trim().to_string();
        let facts = state
            .store
            .group_read(move |c: &Connection| -> GResult<Option<(InviteFacts, String, Vec<String>)>> {
                let Some(f) = invite_by_token(c, &token)? else { return Ok(None) };
                let inviter: String = c
                    .query_row(
                        "SELECT trim(first_name || ' ' || last_name) FROM org_members WHERE id = ?1",
                        [&f.created_by],
                        |r| r.get(0),
                    )
                    .optional()?
                    .filter(|n: &String| !n.is_empty())
                    .unwrap_or_else(|| "An administrator".into());
                let rooms: Vec<String> = {
                    let mut stmt = c.prepare(
                        "SELECT name FROM groups WHERE org_id = ?1 AND auto_join = 1 AND deleted_at IS NULL ORDER BY created_at",
                    )?;
                    let rows = stmt.query_map([&f.org.id], |r| r.get(0))?;
                    rows.collect::<Result<_, _>>()?
                };
                Ok(Some((f, inviter, rooms)))
            })
            .await?;
        let Some((f, inviter, rooms)) = facts else { return Err(invalid_invite()) };
        let live = !f.revoked && f.used_by.is_none() && f.expires_at > now as i64 && f.member.status == "invited"
            && matches!(f.org.status.as_str(), "active" | "past_due" | "read_only");
        if !live || !org_flag(&state, &[&f.org.owner_account]).await {
            return Err(invalid_invite());
        }
        Ok(Outcome::ok(json!({
            "org": { "name": f.org.name, "namespace": f.org.namespace },
            "handle": f.member.handle,
            "first_name": f.member.first_name,
            "role_names": f.roles,
            "inviter": inviter,
            "contact_masked": f.member.contact_masked,
            "expires_at": f.expires_at,
            "rooms": rooms,
            "requires_mfa": f.org.settings.require_mfa,
            "admin_visibility": "Admins see your company address, status, last activity and storage use — never your messages.",
        })))
    }
    .await;
    no_store(finish(&state, result))
}

/// `POST /v1/org-invites/accept` — a person, signed in, whose verified address matches.
async fn accept(
    State(state): State<AppState>,
    Extension(cip): Extension<ClientIp>,
    headers: HeaderMap,
    body: Body,
) -> Response {
    let result = async {
        rate_limit(&format!("invacc:{}", cip.as_str()), 30, 600)?;
        let token_header = crate::bearer(&headers).map(str::to_string);
        let principal = crate::principal_for_token(&state, token_header.as_deref()).await?;
        let (account, _session) = org::person(&principal)?;
        rate_limit(&format!("invacc-acct:{account}"), 20, 3_600)?;
        let (req, _): (TokenReq, String) = parse(body)?;
        // The address claim comes from the validated member token itself (§7.2 step 7).
        let claims = state
            .oidc
            .validate(token_header.as_deref().unwrap_or_default())
            .await
            .map_err(|_| GErr::new(StatusCode::UNAUTHORIZED, "unauthorized", "invalid member token"))?;
        let verified = claims.address_is_verified();
        let signed_in_as = claims.address().and_then(org::canonical_contact);
        let token = req.token.trim().to_string();
        // Facts first, to decide flag and MFA before the transaction.
        let probe = {
            let token = token.clone();
            state
                .store
                .group_read(move |c: &Connection| -> GResult<Option<(String, String, bool)>> {
                    Ok(invite_by_token(c, &token)?.map(|f| (f.org.id.clone(), f.org.owner_account.clone(), f.org.settings.require_mfa)))
                })
                .await?
        };
        let Some((org_id, owner, require_mfa)) = probe else { return Err(invalid_invite()) };
        if !org_flag(&state, &[&owner, &account]).await {
            return Err(invalid_invite());
        }
        if require_mfa {
            crate::principal::require_step_up(&principal, u64::MAX / 4, &state.step_up.acr_values, now_unix())
                .map_err(|d| match d {
                    crate::principal::AuthDenial::StepUpRequired { acr_values, .. } => {
                        org::auth_denial(crate::principal::AuthDenial::StepUpRequired { acr_values, max_age: state.step_up.max_age })
                            .with("reason", "mfa_required")
                    }
                    other => org::auth_denial(other),
                })?;
        }
        let vault = state.vault.clone();
        let auditor = state.auditor.clone();
        let ipaddr = Some(cip.as_str().to_string());
        let now = now_unix();
        let contact_hash_of_signed_in = signed_in_as.as_deref().map(|c| vault.keyed_hash("org-invite-contact", c.as_bytes()));
        state
            .store
            .group_tx(move |tx| -> GResult<Outcome> {
                let Some(f) = invite_by_token(tx, &token)? else { return Err(invalid_invite()) };
                if f.org.id != org_id {
                    return Err(invalid_invite());
                }
                // A retry by the account that already accepted gets the same answer.
                if let Some(by) = &f.used_by {
                    if *by == account {
                        let m = load_member(tx, &f.org.id, &f.member.id)?.ok_or_else(invalid_invite)?;
                        return Ok(Outcome::ok(accepted_body(tx, &f.org, &m, now)?));
                    }
                    return Err(invalid_invite());
                }
                if f.revoked || f.expires_at <= now as i64 || f.member.status != "invited"
                    || !matches!(f.org.status.as_str(), "active" | "past_due" | "read_only")
                {
                    return Err(invalid_invite());
                }
                let matches = verified
                    && contact_hash_of_signed_in.as_deref().is_some_and(|h| {
                        subtle::ConstantTimeEq::ct_eq(h.as_bytes(), f.contact_hash.as_bytes()).into()
                    });
                if !matches {
                    tx.execute("UPDATE org_invites SET attempts = attempts + 1 WHERE id = ?1", [&f.invite_id])?;
                    return Err(GErr::forbidden(
                        "invite_address_mismatch",
                        "this invitation was sent to a different address; sign in with that address",
                    )
                    .with("contact_masked", f.member.contact_masked.clone().unwrap_or_default())
                    .with("verified", verified));
                }
                if org::member_for_account(tx, &f.org.id, &account)?.is_some() {
                    return Err(GErr::conflict("already_member", "you are already in this organization"));
                }
                let handle = f.member.handle.clone().ok_or_else(invalid_invite)?;
                // The handle is still this invitation's (the unique index keeps it so).
                let taken_elsewhere: bool = tx.query_row(
                    "SELECT EXISTS(SELECT 1 FROM identities WHERE handle = ?1)",
                    [&handle],
                    |r| r.get(0),
                )?;
                if taken_elsewhere {
                    return Err(invalid_invite());
                }
                let address = org::mint_identity_in(tx, &vault, &handle, &account, None, now)?;
                tx.execute(
                    "UPDATE org_members SET account_id = ?2, identity = ?3, status = 'active', joined_at = ?4,
                            last_active_at = ?4, contact_sealed = NULL, authority_revision = authority_revision + 1
                      WHERE id = ?1",
                    params![f.member.id, account, address, now as i64],
                )?;
                tx.execute(
                    "UPDATE org_invites SET used_at = ?2, used_by = ?3 WHERE id = ?1",
                    params![f.invite_id, now as i64, account],
                )?;
                // Only roles the inviter can still hand out (§7.2: never beyond the admin's scope).
                let inviter_roles = org::roles_of(tx, &f.org.id, &f.created_by)?;
                let inviter_live = load_member(tx, &f.org.id, &f.created_by)?.is_some_and(|m| m.status == "active");
                let mut granted = Vec::new();
                for r in &f.roles {
                    if let Some(role) = Role::parse(r) {
                        if inviter_live && role != Role::Owner && may_grant(&inviter_roles, role) {
                            tx.execute(
                                "INSERT OR IGNORE INTO org_roles (org_id, member_id, role, granted_by, granted_at) VALUES (?1, ?2, ?3, ?4, ?5)",
                                params![f.org.id, f.member.id, role.as_str(), f.created_by, now as i64],
                            )?;
                            granted.push(role.as_str());
                        }
                    }
                }
                let mut out = Outcome::default();
                let rooms = crate::org_rooms::auto_join_in(tx, &f.org.id, &address, now, &mut out)?;
                org::audit_in(tx, &auditor, &f.org.id, ActorKind::Human, Some(&account), "member.join", Some(&f.member.id),
                    json!({ "handle": handle, "roles": granted, "rooms": rooms.len() }), ipaddr.as_deref(), now)?;
                org::emit_to(tx, &account, "org.member", &json!({
                    "org_id": f.org.id, "member_id": f.member.id, "status": "active", "roles": granted,
                }), now)?;
                out.wake.push(account.clone());
                let m = load_member(tx, &f.org.id, &f.member.id)?.ok_or_else(invalid_invite)?;
                out.status = 200;
                out.body = accepted_body(tx, &f.org, &m, now)?;
                Ok(out)
            })
            .await
    }
    .await;
    no_store(finish(&state, result))
}

fn accepted_body(c: &Connection, o: &org::OrgRow, m: &org::MemberRow, now: u64) -> GResult<Value> {
    Ok(json!({
        "org": { "id": o.id, "name": o.name, "namespace": o.namespace },
        "member": member_json(c, m, now)?,
        "mailbox": { "address": m.identity, "handle": m.handle },
    }))
}

// ---- rooms -------------------------------------------------------------------------------------

/// `GET /v1/orgs/{id}/rooms`
async fn rooms(
    State(state): State<AppState>,
    headers: HeaderMap,
    Path(id): Path<String>,
) -> Response {
    let result = async {
        let ctx = org::org_ctx(&state, &headers, &id).await?;
        let admin = org::permits(&ctx.roles, Perm::DeleteAnyRoom, false);
        let mine = ctx.member.identity.clone().unwrap_or_default();
        let list = state
            .store
            .group_read(move |c: &Connection| -> GResult<Vec<Value>> {
                let mut stmt = c.prepare(
                    "SELECT g.id FROM groups g WHERE g.org_id = ?1 AND g.deleted_at IS NULL
                       AND (?2 OR g.visibility = 'listed'
                            OR EXISTS (SELECT 1 FROM group_members m WHERE m.group_id = g.id AND m.member = ?3
                                         AND m.state IN ('active','keying','invited')))
                     ORDER BY g.created_at",
                )?;
                let ids: Vec<String> = stmt.query_map(params![id, admin, mine], |r| r.get(0))?.collect::<Result<_, _>>()?;
                ids.iter().map(|g| crate::org_rooms::room_json(c, g)).collect()
            })
            .await?;
        Ok(Outcome::ok(json!({ "rooms": list })))
    }
    .await;
    finish(&state, result)
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct RoomReq {
    name: String,
    #[serde(default)]
    description: Option<String>,
    #[serde(default)]
    join_policy: Option<String>,
    #[serde(default)]
    post_policy: Option<String>,
    #[serde(default)]
    listed: Option<bool>,
    #[serde(default)]
    auto_join: Option<bool>,
}

/// `POST /v1/orgs/{id}/rooms`
async fn create_room(
    State(state): State<AppState>,
    Extension(cip): Extension<ClientIp>,
    headers: HeaderMap,
    Path(id): Path<String>,
    body: Body,
) -> Response {
    let result = async {
        let ctx = org::org_ctx(&state, &headers, &id).await?;
        ctx.require(&state, Perm::CreateRoom)?;
        ctx.require_writable()?;
        if !crate::filtering::feature_on(&state, Some(&ctx.account), capabilities::GROUPS).await {
            return Err(GErr::new(
                StatusCode::NOT_FOUND,
                "feature_disabled",
                "groups are not enabled for this account",
            ));
        }
        let mailbox = ctx.member.identity.clone().ok_or_else(|| {
            GErr::conflict(
                "mailbox_required",
                "choose your address in this organization first",
            )
        })?;
        rate_limit(&format!("orgroom:{}", ctx.account), 20, 86_400)?;
        let (req, _): (RoomReq, String) = parse(body)?;
        let name = text("name", &req.name, 80)?;
        let description = req
            .description
            .as_deref()
            .map(|d| text("description", d, 500))
            .transpose()?;
        let join_policy = req.join_policy.unwrap_or_else(|| "public".into());
        let post_policy = req.post_policy.unwrap_or_else(|| "everyone".into());
        if !matches!(join_policy.as_str(), "public" | "private")
            || !matches!(post_policy.as_str(), "everyone" | "admins")
        {
            return Err(GErr::bad(
                "bad_request",
                "join_policy is public|private, post_policy everyone|admins",
            ));
        }
        let auto_join = req.auto_join.unwrap_or(false);
        if auto_join && !org::permits(&ctx.roles, Perm::DeleteAnyRoom, false) {
            return Err(GErr::forbidden(
                "forbidden",
                "only people admins can make a room join everyone automatically",
            ));
        }
        if auto_join && join_policy != "public" {
            return Err(GErr::bad(
                "bad_request",
                "an auto-join room is public within the organization",
            ));
        }
        let listed = req.listed.unwrap_or(true);
        let auditor = state.auditor.clone();
        let account = ctx.account.clone();
        let ipaddr = ip(&Extension(cip));
        let now = now_unix();
        state
            .store
            .group_tx(move |tx| -> GResult<Outcome> {
                let gid = crate::org_rooms::create_room_in(
                    tx,
                    &crate::org_rooms::NewRoom {
                        org_id: &id,
                        name: &name,
                        description: description.as_deref(),
                        join_policy: &join_policy,
                        post_policy: &post_policy,
                        listed,
                        auto_join,
                        creator: &mailbox,
                        creator_account: Some(&account),
                    },
                    now,
                )?;
                org::audit_in(
                    tx,
                    &auditor,
                    &id,
                    ActorKind::Human,
                    Some(&account),
                    "room.create",
                    Some(&gid),
                    json!({ "auto_join": auto_join, "listed": listed }),
                    ipaddr.as_deref(),
                    now,
                )?;
                let mut out =
                    Outcome::created(json!({ "room": crate::org_rooms::room_json(tx, &gid)? }));
                out.wake.push(account.clone());
                Ok(out)
            })
            .await
    }
    .await;
    finish(&state, result)
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct PatchRoom {
    #[serde(default)]
    auto_join: Option<bool>,
    #[serde(default)]
    listed: Option<bool>,
}

pub(crate) fn room_in_org(
    c: &Connection,
    org_id: &str,
    gid: &str,
) -> GResult<crate::groups::GroupRow> {
    let g = crate::groups::live_group(c, gid)?;
    if g.org_id.as_deref() != Some(org_id) {
        return Err(GErr::not_found());
    }
    Ok(g)
}

/// May this member manage this room from the org surface: people admins and up, or the room's own
/// owner/admin (SDS §6.4 "own rooms").
fn may_manage_room(
    c: &Connection,
    roles: &Roles,
    mailbox: Option<&str>,
    gid: &str,
) -> GResult<bool> {
    if org::permits(roles, Perm::DeleteAnyRoom, false) {
        return Ok(true);
    }
    let Some(mb) = mailbox else { return Ok(false) };
    Ok(crate::groups::load_member(c, gid, mb)?
        .is_some_and(|m| m.state == "active" && matches!(m.role.as_str(), "owner" | "admin")))
}

/// `PATCH /v1/orgs/{id}/rooms/{room}`
async fn patch_room(
    State(state): State<AppState>,
    Extension(cip): Extension<ClientIp>,
    headers: HeaderMap,
    Path((id, gid)): Path<(String, String)>,
    body: Body,
) -> Response {
    let result = async {
        let ctx = org::org_ctx(&state, &headers, &id).await?;
        let (req, _): (PatchRoom, String) = parse(body)?;
        let roles = ctx.roles.clone();
        let mailbox = ctx.member.identity.clone();
        let auditor = state.auditor.clone();
        let account = ctx.account.clone();
        let ipaddr = ip(&Extension(cip));
        let now = now_unix();
        state
            .store
            .group_tx(move |tx| -> GResult<Outcome> {
                let g = room_in_org(tx, &id, &gid)?;
                if !may_manage_room(tx, &roles, mailbox.as_deref(), &gid)? {
                    return Err(GErr::forbidden("forbidden", "you cannot manage this room"));
                }
                let mut changed = Vec::new();
                if let Some(a) = req.auto_join {
                    if a && (!org::permits(&roles, Perm::DeleteAnyRoom, false)
                        || g.join_policy != "public")
                    {
                        return Err(GErr::forbidden(
                            "forbidden",
                            "only people admins can make a public room join everyone",
                        ));
                    }
                    tx.execute(
                        "UPDATE groups SET auto_join = ?2 WHERE id = ?1",
                        params![gid, a as i64],
                    )?;
                    changed.push("auto_join");
                }
                if let Some(l) = req.listed {
                    tx.execute(
                        "UPDATE groups SET visibility = ?2 WHERE id = ?1",
                        params![gid, if l { "listed" } else { "unlisted" }],
                    )?;
                    changed.push("visibility");
                }
                let mut out = Outcome::default();
                out.wake
                    .extend(crate::org_rooms::settings_changed(tx, &gid, &changed, now)?);
                org::audit_in(
                    tx,
                    &auditor,
                    &id,
                    ActorKind::Human,
                    Some(&account),
                    "room.update",
                    Some(&gid),
                    json!({ "changed": changed }),
                    ipaddr.as_deref(),
                    now,
                )?;
                out.status = 200;
                out.body = json!({ "room": crate::org_rooms::room_json(tx, &gid)? });
                Ok(out)
            })
            .await
    }
    .await;
    finish(&state, result)
}

/// `DELETE /v1/orgs/{id}/rooms/{room}`
async fn delete_room(
    State(state): State<AppState>,
    Extension(cip): Extension<ClientIp>,
    headers: HeaderMap,
    Path((id, gid)): Path<(String, String)>,
) -> Response {
    let result = async {
        let ctx = org::org_ctx(&state, &headers, &id).await?;
        let roles = ctx.roles.clone();
        let mailbox = ctx.member.identity.clone();
        let actor = mailbox.clone().unwrap_or_else(|| ctx.account.clone());
        let auditor = state.auditor.clone();
        let account = ctx.account.clone();
        let ipaddr = ip(&Extension(cip));
        let now = now_unix();
        state
            .store
            .group_tx(move |tx| -> GResult<Outcome> {
                let g = room_in_org(tx, &id, &gid)?;
                let own = match mailbox.as_deref() {
                    Some(mb) => {
                        crate::groups::load_member(tx, &gid, mb)?.is_some_and(|m| m.role == "owner")
                    }
                    None => false,
                };
                if !(own || org::permits(&roles, Perm::DeleteAnyRoom, false)) {
                    return Err(GErr::forbidden("forbidden", "you cannot delete this room"));
                }
                let mut out = Outcome::default();
                out.wake
                    .extend(crate::groups::delete_in(tx, &g, &actor, now)?);
                org::audit_in(
                    tx,
                    &auditor,
                    &id,
                    ActorKind::Human,
                    Some(&account),
                    "room.delete",
                    Some(&gid),
                    json!({}),
                    ipaddr.as_deref(),
                    now,
                )?;
                out.status = 200;
                out.body = json!({ "deleted": true });
                Ok(out)
            })
            .await
    }
    .await;
    finish(&state, result)
}

// ---- rules -------------------------------------------------------------------------------------

/// `GET /v1/orgs/{id}/rules`
async fn rules(
    State(state): State<AppState>,
    headers: HeaderMap,
    Path(id): Path<String>,
) -> Response {
    let result = async {
        let _ctx = org::org_ctx(&state, &headers, &id).await?;
        let body = state
            .store
            .group_read(move |c: &Connection| -> GResult<Value> {
                let mut stmt = c.prepare(
                    "SELECT id FROM org_rules WHERE org_id = ?1 ORDER BY priority, created_at",
                )?;
                let ids: Vec<String> = stmt
                    .query_map([&id], |r| r.get(0))?
                    .collect::<Result<_, _>>()?;
                let list: Vec<Value> = ids
                    .iter()
                    .map(|r| org_rules::rule_json(c, r))
                    .collect::<GResult<_>>()?;
                let rev: i64 = c.query_row(
                    "SELECT rules_revision FROM orgs WHERE id = ?1",
                    [&id],
                    |r| r.get(0),
                )?;
                Ok(json!({ "rules": list, "revision": rev }))
            })
            .await?;
        Ok(Outcome::ok(body))
    }
    .await;
    finish(&state, result)
}

pub(crate) fn policy_changed(
    tx: &Connection,
    org_id: &str,
    now: u64,
    out: &mut Outcome,
) -> GResult<()> {
    let rev = org_rules::bump_rules(tx, org_id)?;
    out.wake.extend(org::emit_members(
        tx,
        org_id,
        "org.policy",
        &json!({ "org_id": org_id, "revision": rev }),
        now,
    )?);
    Ok(())
}

/// `POST /v1/orgs/{id}/rules`
async fn create_rule(
    State(state): State<AppState>,
    Extension(cip): Extension<ClientIp>,
    headers: HeaderMap,
    Path(id): Path<String>,
    body: Body,
) -> Response {
    let result = async {
        let ctx = org::org_ctx(&state, &headers, &id).await?;
        let (req, _): (org_rules::RuleBody, String) = parse(body)?;
        let mut d = org_rules::Draft {
            owners: String::new(),
            peers: String::new(),
            admission: None,
            autonomy: None,
            verbs: None,
            known: false,
            locked: false,
            priority: 100,
            description: None,
        };
        if req.owners.is_none() || req.peers.is_none() {
            return Err(GErr::bad("invalid_rule", "owners and peers are required"));
        }
        org_rules::apply(&mut d, req);
        let (owners, _) = org_rules::validate(&d, &ctx.org.namespace)?;
        if !org_rules::editor_may(&ctx.roles, &owners) {
            return Err(GErr::forbidden("forbidden", "your role cannot edit rules for these mailboxes"));
        }
        let auditor = state.auditor.clone();
        let account = ctx.account.clone();
        let ipaddr = ip(&Extension(cip));
        let now = now_unix();
        state
            .store
            .group_tx(move |tx| -> GResult<Outcome> {
                let rid = org_rules::insert(tx, &id, &d, &account, now)?;
                let mut out = Outcome::default();
                policy_changed(tx, &id, now, &mut out)?;
                org::audit_in(tx, &auditor, &id, ActorKind::Human, Some(&account), "rule.create", Some(&rid),
                    json!({ "owners": d.owners, "peers": d.peers, "admission": d.admission, "autonomy": d.autonomy,
                            "verbs": d.verbs, "known": d.known, "locked": d.locked, "priority": d.priority }),
                    ipaddr.as_deref(), now)?;
                out.status = 201;
                out.body = json!({ "rule": org_rules::rule_json(tx, &rid)? });
                Ok(out)
            })
            .await
    }
    .await;
    finish(&state, result)
}

/// `PATCH /v1/orgs/{id}/rules/{rule}`
async fn patch_rule(
    State(state): State<AppState>,
    Extension(cip): Extension<ClientIp>,
    headers: HeaderMap,
    Path((id, rid)): Path<(String, String)>,
    body: Body,
) -> Response {
    let result = async {
        let ctx = org::org_ctx(&state, &headers, &id).await?;
        let (req, _): (org_rules::RuleBody, String) = parse(body)?;
        let roles = ctx.roles.clone();
        let ns = ctx.org.namespace.clone();
        let auditor = state.auditor.clone();
        let account = ctx.account.clone();
        let ipaddr = ip(&Extension(cip));
        let now = now_unix();
        state
            .store
            .group_tx(move |tx| -> GResult<Outcome> {
                let mut d = org_rules::load_draft(tx, &id, &rid)?.ok_or_else(GErr::not_found)?;
                let before = org_rules::parse_pat(&d.owners, &ns, true).map_err(|m| GErr::bad("invalid_rule", m))?;
                if !org_rules::editor_may(&roles, &before) {
                    return Err(GErr::forbidden("forbidden", "your role cannot edit this rule"));
                }
                org_rules::apply(&mut d, req);
                let (owners, _) = org_rules::validate(&d, &ns)?;
                if !org_rules::editor_may(&roles, &owners) {
                    return Err(GErr::forbidden("forbidden", "your role cannot edit rules for these mailboxes"));
                }
                org_rules::update(tx, &rid, &d, now)?;
                let mut out = Outcome::default();
                policy_changed(tx, &id, now, &mut out)?;
                org::audit_in(tx, &auditor, &id, ActorKind::Human, Some(&account), "rule.update", Some(&rid),
                    json!({ "owners": d.owners, "peers": d.peers, "admission": d.admission, "autonomy": d.autonomy,
                            "verbs": d.verbs, "known": d.known, "locked": d.locked, "priority": d.priority }),
                    ipaddr.as_deref(), now)?;
                out.status = 200;
                out.body = json!({ "rule": org_rules::rule_json(tx, &rid)? });
                Ok(out)
            })
            .await
    }
    .await;
    finish(&state, result)
}

/// `DELETE /v1/orgs/{id}/rules/{rule}`
async fn delete_rule(
    State(state): State<AppState>,
    Extension(cip): Extension<ClientIp>,
    headers: HeaderMap,
    Path((id, rid)): Path<(String, String)>,
) -> Response {
    let result = async {
        let ctx = org::org_ctx(&state, &headers, &id).await?;
        let roles = ctx.roles.clone();
        let ns = ctx.org.namespace.clone();
        let auditor = state.auditor.clone();
        let account = ctx.account.clone();
        let ipaddr = ip(&Extension(cip));
        let now = now_unix();
        state
            .store
            .group_tx(move |tx| -> GResult<Outcome> {
                let d = org_rules::load_draft(tx, &id, &rid)?.ok_or_else(GErr::not_found)?;
                let owners = org_rules::parse_pat(&d.owners, &ns, true)
                    .map_err(|m| GErr::bad("invalid_rule", m))?;
                if !org_rules::editor_may(&roles, &owners) {
                    return Err(GErr::forbidden(
                        "forbidden",
                        "your role cannot delete this rule",
                    ));
                }
                tx.execute(
                    "DELETE FROM org_rules WHERE id = ?1 AND org_id = ?2",
                    params![rid, id],
                )?;
                let mut out = Outcome::default();
                policy_changed(tx, &id, now, &mut out)?;
                org::audit_in(
                    tx,
                    &auditor,
                    &id,
                    ActorKind::Human,
                    Some(&account),
                    "rule.delete",
                    Some(&rid),
                    json!({ "owners": d.owners, "peers": d.peers }),
                    ipaddr.as_deref(),
                    now,
                )?;
                out.status = 200;
                out.body = json!({ "deleted": true });
                Ok(out)
            })
            .await
    }
    .await;
    finish(&state, result)
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct EvalReq {
    pub from: String,
    pub to: String,
    #[serde(default)]
    pub verb: Option<String>,
}

/// `POST /v1/orgs/{id}/rules/evaluate` — a dry run with the same evaluation `do_send` uses.
async fn evaluate(
    State(state): State<AppState>,
    headers: HeaderMap,
    Path(id): Path<String>,
    body: Body,
) -> Response {
    let result = async {
        let _ctx = org::org_ctx(&state, &headers, &id).await?;
        let (req, _): (EvalReq, String) = parse(body)?;
        let v = evaluate_in(&state, &id, &req.from, &req.to, req.verb.as_deref()).await?;
        Ok(Outcome::ok(v))
    }
    .await;
    finish(&state, result)
}

/// The rule dry run, shared with `/org/v1/rules/evaluate`. The recipient must be a live mailbox of
/// this org: the evaluation never describes anybody else's.
pub(crate) async fn evaluate_in(
    state: &AppState,
    id: &str,
    from_raw: &str,
    to_raw: &str,
    verb: Option<&str>,
) -> GResult<Value> {
    let to = crate::resolve_recipient(state, to_raw.trim())
        .await?
        .ok_or_else(|| GErr::bad("unknown_recipient", "no mailbox at that address"))?;
    let owned = {
        let (id2, a) = (id.to_string(), to.address.clone());
        state
            .store
            .group_read(move |c: &Connection| -> GResult<bool> {
                Ok(c.query_row(
                    "SELECT EXISTS(SELECT 1 FROM org_members WHERE org_id = ?1 AND identity = ?2 AND status = 'active')
                         OR EXISTS(SELECT 1 FROM org_agents WHERE org_id = ?1 AND identity = ?2 AND status = 'active')",
                    params![id2, a],
                    |r| r.get(0),
                )?)
            })
            .await?
    };
    if !owned {
        return Err(GErr::bad(
            "not_org_mailbox",
            "the recipient must be a mailbox of this organization",
        ));
    }
    let from = crate::resolve_recipient(state, from_raw.trim()).await?;
    let (from_address, from_handle) = match &from {
        Some(f) => (f.address.clone(), f.handle.clone()),
        None => {
            // A sender that is not hosted here is evaluated by the spelling given.
            let raw = from_raw.trim().to_ascii_lowercase();
            (raw.clone(), Some(raw))
        }
    };
    let verdict = org_rules::verdict(state, &to, &from_address, from_handle.as_deref())
        .await
        .unwrap_or_default();
    let subject = from_handle.clone().unwrap_or_else(|| from_address.clone());
    let contact = state
        .store
        .governing_contact(to.address.clone(), subject)
        .await?;
    let rel = state
        .store
        .relationship(
            to.address.clone(),
            from_address.clone(),
            from_handle.clone(),
        )
        .await?;
    let policy = state.store.inbox_policy(to.address.clone()).await?;
    let d = crate::decide_admission(&rel, contact.as_ref(), Some(&verdict), policy);
    let autonomy = crate::effective_autonomy(contact.as_ref(), policy, Some(&verdict));
    let verb_auto = verb.map(|v| autonomy.0 && autonomy.1.iter().any(|g| g == v));
    Ok(json!({
        "admitted": d.admitted,
        "known": d.known,
        "decided_by": d.decided_by,
        "autonomy": if autonomy.0 { "auto" } else { "review" },
        "verbs": autonomy.1,
        "verb_auto": verb_auto,
        "matched_rule": verdict.matched_json(),
        "explanation": d.explanation,
    }))
}

// ---- domains (SDS §7.5) ------------------------------------------------------------------------

fn canonical_domain(raw: &str) -> GResult<String> {
    let d = raw.trim().trim_end_matches('.').to_ascii_lowercase();
    let ok = d.len() >= 3
        && d.len() <= 253
        && d.contains('.')
        && d.split('.').all(|l| {
            !l.is_empty()
                && l.len() <= 63
                && !l.starts_with('-')
                && !l.ends_with('-')
                && l.chars().all(|c| c.is_ascii_alphanumeric() || c == '-')
        });
    if ok {
        Ok(d)
    } else {
        Err(GErr::bad(
            "invalid_domain",
            "expected a domain like example.com",
        ))
    }
}

fn domain_json(r: &rusqlite::Row<'_>) -> rusqlite::Result<Value> {
    let domain: String = r.get(0)?;
    let token: String = r.get(1)?;
    Ok(json!({
        "domain": domain,
        "txt_name": domain,
        "txt_value": format!("pigeonpost-verify={token}"),
        "verified_at": r.get::<_, Option<i64>>(2)?,
        "last_checked_at": r.get::<_, Option<i64>>(3)?,
        "failures": r.get::<_, i64>(4)?,
    }))
}

/// `GET /v1/orgs/{id}/domains`
async fn domains(
    State(state): State<AppState>,
    headers: HeaderMap,
    Path(id): Path<String>,
) -> Response {
    let result = async {
        let ctx = org::org_ctx(&state, &headers, &id).await?;
        ctx.require(&state, Perm::Security)?;
        let list = state
            .store
            .group_read(move |c: &Connection| -> GResult<Vec<Value>> {
                let mut stmt = c.prepare(
                    "SELECT domain, token, verified_at, last_checked_at, failures FROM org_domains WHERE org_id = ?1 ORDER BY domain",
                )?;
                let rows = stmt.query_map([&id], domain_json)?;
                Ok(rows.collect::<Result<_, _>>()?)
            })
            .await?;
        Ok(Outcome::ok(json!({ "domains": list })))
    }
    .await;
    finish(&state, result)
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct DomainReq {
    domain: String,
}

/// `POST /v1/orgs/{id}/domains`
async fn add_domain(
    State(state): State<AppState>,
    Extension(cip): Extension<ClientIp>,
    headers: HeaderMap,
    Path(id): Path<String>,
    body: Body,
) -> Response {
    let result = async {
        let ctx = org::org_ctx(&state, &headers, &id).await?;
        ctx.require(&state, Perm::Security)?;
        let (req, _): (DomainReq, String) = parse(body)?;
        let domain = canonical_domain(&req.domain)?;
        let auditor = state.auditor.clone();
        let account = ctx.account.clone();
        let ipaddr = ip(&Extension(cip));
        let now = now_unix();
        state
            .store
            .group_tx(move |tx| -> GResult<Outcome> {
                let count: i64 = tx.query_row("SELECT COUNT(*) FROM org_domains WHERE org_id = ?1", [&id], |r| r.get(0))?;
                if count >= 20 {
                    return Err(GErr::conflict("too_many_domains", "an organization verifies at most 20 domains"));
                }
                let token = crate::groups::rand_b32(20).to_ascii_lowercase();
                let inserted = tx.execute(
                    "INSERT OR IGNORE INTO org_domains (org_id, domain, token, created_at) VALUES (?1, ?2, ?3, ?4)",
                    params![id, domain, token, now as i64],
                )?;
                if inserted == 0 {
                    return Err(GErr::conflict("domain_exists", "that domain is already listed"));
                }
                org::audit_in(tx, &auditor, &id, ActorKind::Human, Some(&account), "domain.add", Some(&domain),
                    json!({}), ipaddr.as_deref(), now)?;
                let v = tx.query_row(
                    "SELECT domain, token, verified_at, last_checked_at, failures FROM org_domains WHERE org_id = ?1 AND domain = ?2",
                    params![id, domain],
                    domain_json,
                )?;
                Ok(Outcome::created(v))
            })
            .await
    }
    .await;
    finish(&state, result)
}

/// Look up TXT records over DNS-over-HTTPS (JSON API), one resolver.
async fn doh_txt(url: &str, name: &str) -> Option<Vec<String>> {
    let client = reqwest::Client::builder()
        .timeout(std::time::Duration::from_secs(5))
        .build()
        .ok()?;
    let resp = client
        .get(url)
        .query(&[("name", name), ("type", "TXT")])
        .header("accept", "application/dns-json")
        .send()
        .await
        .ok()?;
    let v: Value = resp.json().await.ok()?;
    Some(
        v["Answer"]
            .as_array()
            .map(|a| {
                a.iter()
                    .filter(|r| r["type"].as_i64() == Some(16))
                    .filter_map(|r| r["data"].as_str())
                    .map(|d| d.replace("\" \"", "").trim_matches('"').to_string())
                    .collect()
            })
            .unwrap_or_default(),
    )
}

/// The two resolvers both have to see the record (SDS §7.5). Overridable for tests.
pub(crate) async fn domain_has_token(domain: &str, token: &str) -> bool {
    let want = format!("pigeonpost-verify={token}");
    let resolvers: Vec<String> = std::env::var("POSTBOX_DOH_RESOLVERS")
        .ok()
        .map(|v| {
            v.split(',')
                .map(|s| s.trim().to_string())
                .filter(|s| !s.is_empty())
                .collect()
        })
        .unwrap_or_else(|| {
            vec![
                "https://cloudflare-dns.com/dns-query".into(),
                "https://dns.google/resolve".into(),
            ]
        });
    if resolvers.is_empty() {
        return false;
    }
    for r in &resolvers {
        match doh_txt(r, domain).await {
            Some(records) if records.iter().any(|t| t == &want) => {}
            _ => return false,
        }
    }
    true
}

/// `POST /v1/orgs/{id}/domains/{domain}/verify`
async fn verify_domain(
    State(state): State<AppState>,
    Extension(cip): Extension<ClientIp>,
    headers: HeaderMap,
    Path((id, domain)): Path<(String, String)>,
) -> Response {
    let result = async {
        let ctx = org::org_ctx(&state, &headers, &id).await?;
        ctx.require(&state, Perm::Security)?;
        rate_limit(&format!("domverify:{id}"), 30, 3_600)?;
        let domain = canonical_domain(&domain)?;
        let token: String = {
            let (i, d) = (id.clone(), domain.clone());
            state
                .store
                .group_read(move |c: &Connection| -> GResult<Option<String>> {
                    Ok(c.query_row("SELECT token FROM org_domains WHERE org_id = ?1 AND domain = ?2", params![i, d], |r| r.get(0)).optional()?)
                })
                .await?
                .ok_or_else(GErr::not_found)?
        };
        let ok = domain_has_token(&domain, &token).await;
        let auditor = state.auditor.clone();
        let account = ctx.account.clone();
        let ipaddr = ip(&Extension(cip));
        let now = now_unix();
        state
            .store
            .group_tx(move |tx| -> GResult<Outcome> {
                tx.execute(
                    "UPDATE org_domains SET last_checked_at = ?3,
                            verified_at = CASE WHEN ?4 THEN COALESCE(verified_at, ?3) ELSE verified_at END,
                            failures = CASE WHEN ?4 THEN 0 ELSE failures END
                      WHERE org_id = ?1 AND domain = ?2",
                    params![id, domain, now as i64, ok],
                )?;
                if ok {
                    org::audit_in(tx, &auditor, &id, ActorKind::Human, Some(&account), "domain.verify", Some(&domain),
                        json!({}), ipaddr.as_deref(), now)?;
                }
                let mut v = tx.query_row(
                    "SELECT domain, token, verified_at, last_checked_at, failures FROM org_domains WHERE org_id = ?1 AND domain = ?2",
                    params![id, domain],
                    domain_json,
                )?;
                v["verified"] = json!(ok);
                Ok(Outcome::ok(v))
            })
            .await
    }
    .await;
    finish(&state, result)
}

/// `DELETE /v1/orgs/{id}/domains/{domain}`
async fn delete_domain(
    State(state): State<AppState>,
    Extension(cip): Extension<ClientIp>,
    headers: HeaderMap,
    Path((id, domain)): Path<(String, String)>,
) -> Response {
    let result = async {
        let ctx = org::org_ctx(&state, &headers, &id).await?;
        ctx.require(&state, Perm::Security)?;
        let domain = canonical_domain(&domain)?;
        let auditor = state.auditor.clone();
        let account = ctx.account.clone();
        let ipaddr = ip(&Extension(cip));
        let now = now_unix();
        state
            .store
            .group_tx(move |tx| -> GResult<Outcome> {
                let n = tx.execute(
                    "DELETE FROM org_domains WHERE org_id = ?1 AND domain = ?2",
                    params![id, domain],
                )?;
                if n == 0 {
                    return Err(GErr::not_found());
                }
                org::audit_in(
                    tx,
                    &auditor,
                    &id,
                    ActorKind::Human,
                    Some(&account),
                    "domain.delete",
                    Some(&domain),
                    json!({}),
                    ipaddr.as_deref(),
                    now,
                )?;
                Ok(Outcome::ok(json!({ "deleted": true })))
            })
            .await
    }
    .await;
    finish(&state, result)
}

/// Weekly re-check of verified domains; three failures in a row unverify (SDS §7.5).
pub(crate) async fn recheck_domains(state: &AppState) {
    let now = now_unix();
    let due: Vec<(String, String, String)> = state
        .store
        .group_read(
            move |c: &Connection| -> GResult<Vec<(String, String, String)>> {
                let mut stmt = c.prepare(
                    "SELECT org_id, domain, token FROM org_domains WHERE verified_at IS NOT NULL
                   AND (last_checked_at IS NULL OR last_checked_at < ?1) LIMIT 50",
                )?;
                let rows = stmt.query_map([now as i64 - 7 * 86_400], |r| {
                    Ok((r.get(0)?, r.get(1)?, r.get(2)?))
                })?;
                Ok(rows.collect::<Result<_, _>>()?)
            },
        )
        .await
        .unwrap_or_default();
    for (org_id, domain, token) in due {
        let ok = domain_has_token(&domain, &token).await;
        let _ = state
            .store
            .group_tx(move |tx| -> GResult<()> {
                tx.execute(
                    "UPDATE org_domains SET last_checked_at = ?3,
                            failures = CASE WHEN ?4 THEN 0 ELSE failures + 1 END,
                            verified_at = CASE WHEN ?4 THEN verified_at WHEN failures + 1 >= 3 THEN NULL ELSE verified_at END
                      WHERE org_id = ?1 AND domain = ?2",
                    params![org_id, domain, now as i64, ok],
                )?;
                Ok(())
            })
            .await;
    }
}

// ---- audit (SDS §6.8) --------------------------------------------------------------------------

#[derive(Deserialize, Default)]
pub(crate) struct AuditQuery {
    #[serde(default)]
    pub action: Option<String>,
    #[serde(default)]
    pub before_id: Option<i64>,
    #[serde(default)]
    limit: Option<i64>,
    #[serde(default)]
    format: Option<String>,
}

pub(crate) fn audit_rows(
    c: &Connection,
    org: &str,
    q: &AuditQuery,
    limit: i64,
) -> GResult<Vec<Value>> {
    let action = q.action.as_deref().map(crate::org_api::like_prefix);
    let mut stmt = c.prepare(
        "SELECT id, at, actor_kind, actor_account, action, target, details FROM audit_log
          WHERE org_id = ?1 AND (?2 IS NULL OR action LIKE ?2 ESCAPE '\\') AND (?3 IS NULL OR id < ?3)
          ORDER BY id DESC LIMIT ?4",
    )?;
    let rows = stmt.query_map(params![org, action, q.before_id, limit], |r| {
        let details: Option<String> = r.get(6)?;
        Ok(json!({
            "id": r.get::<_, i64>(0)?,
            "at": r.get::<_, i64>(1)?,
            "actor_kind": r.get::<_, String>(2)?,
            "actor": r.get::<_, Option<String>>(3)?,
            "action": r.get::<_, String>(4)?,
            "target": r.get::<_, Option<String>>(5)?,
            "details": details.and_then(|d| serde_json::from_str::<Value>(&d).ok()),
        }))
    })?;
    let mut out: Vec<Value> = rows.collect::<Result<_, _>>()?;
    // Name the actor by their org handle when they are a member; account ids are internal.
    for v in out.iter_mut() {
        // An org API key is named by its key id (SDS §18.2: every use is logged with it).
        if v["actor_kind"] == "api_key" {
            continue;
        }
        if let Some(a) = v["actor"].as_str() {
            let name: Option<String> = c
                .query_row(
                    "SELECT COALESCE(handle, trim(first_name || ' ' || last_name)) FROM org_members
                      WHERE org_id = ?1 AND account_id = ?2 ORDER BY status = 'removed' LIMIT 1",
                    params![org, a],
                    |r| r.get(0),
                )
                .optional()?;
            v["actor"] = json!(name
                .filter(|n| !n.is_empty())
                .unwrap_or_else(|| "system".into()));
        }
    }
    Ok(out)
}

/// `GET /v1/orgs/{id}/audit`
async fn audit(
    State(state): State<AppState>,
    headers: HeaderMap,
    Path(id): Path<String>,
    Query(q): Query<AuditQuery>,
) -> Response {
    let result = async {
        let ctx = org::org_ctx(&state, &headers, &id).await?;
        ctx.require(&state, Perm::Security)?;
        let limit = q.limit.unwrap_or(50).clamp(1, 200);
        let entries = state
            .store
            .group_read(move |c: &Connection| -> GResult<Vec<Value>> {
                audit_rows(c, &id, &q, limit)
            })
            .await?;
        let next = if entries.len() as i64 == limit {
            entries.last().and_then(|e| e["id"].as_i64())
        } else {
            None
        };
        Ok(Outcome::ok(
            json!({ "entries": entries, "next_before_id": next }),
        ))
    }
    .await;
    finish(&state, result)
}

fn csv_field(v: &Value) -> String {
    let s = match v {
        Value::Null => String::new(),
        Value::String(s) => s.clone(),
        other => other.to_string(),
    };
    // Neutralise spreadsheet formulas as well as quoting.
    let s = if s.starts_with(['=', '+', '-', '@']) {
        format!("'{s}")
    } else {
        s
    };
    format!("\"{}\"", s.replace('"', "\"\""))
}

/// `GET /v1/orgs/{id}/audit/export` — owner/admin with step-up (§3.4).
async fn audit_export(
    State(state): State<AppState>,
    Extension(cip): Extension<ClientIp>,
    headers: HeaderMap,
    Path(id): Path<String>,
    Query(q): Query<AuditQuery>,
) -> Response {
    let result: GResult<(String, Vec<u8>)> = async {
        let ctx = org::org_ctx(&state, &headers, &id).await?;
        ctx.require(&state, Perm::ExportAudit)?;
        rate_limit(&format!("auditexp:{id}"), 10, 3_600)?;
        let format = q.format.clone().unwrap_or_else(|| "csv".into());
        if !matches!(format.as_str(), "csv" | "json") {
            return Err(GErr::bad("bad_request", "format is csv or json"));
        }
        let auditor = state.auditor.clone();
        let account = ctx.account.clone();
        let ipaddr = ip(&Extension(cip));
        let now = now_unix();
        let rows = state
            .store
            .group_tx(move |tx| -> GResult<Vec<Value>> {
                let rows = audit_rows(
                    tx,
                    &id,
                    &AuditQuery {
                        action: q.action.clone(),
                        ..AuditQuery::default()
                    },
                    100_000,
                )?;
                org::audit_in(
                    tx,
                    &auditor,
                    &id,
                    ActorKind::Human,
                    Some(&account),
                    "audit.export",
                    None,
                    json!({ "format": q.format, "rows": rows.len() }),
                    ipaddr.as_deref(),
                    now,
                )?;
                Ok(rows)
            })
            .await?;
        if format == "json" {
            return Ok((
                "application/json".into(),
                serde_json::to_vec(&json!({ "entries": rows })).unwrap_or_default(),
            ));
        }
        let mut out = String::from("id,at,actor_kind,actor,action,target,details\n");
        for r in rows {
            let fields = [
                "id",
                "at",
                "actor_kind",
                "actor",
                "action",
                "target",
                "details",
            ]
            .iter()
            .map(|k| csv_field(&r[*k]))
            .collect::<Vec<_>>()
            .join(",");
            out.push_str(&fields);
            out.push('\n');
        }
        Ok(("text/csv; charset=utf-8".into(), out.into_bytes()))
    }
    .await;
    match result {
        Ok((ct, bytes)) => {
            let mut r = (StatusCode::OK, bytes).into_response();
            let h = r.headers_mut();
            if let Ok(v) = HeaderValue::from_str(&ct) {
                h.insert(header::CONTENT_TYPE, v);
            }
            h.insert(
                header::CONTENT_DISPOSITION,
                HeaderValue::from_static("attachment; filename=\"audit-log\""),
            );
            h.insert(header::CACHE_CONTROL, HeaderValue::from_static("no-store"));
            r
        }
        Err(e) => e.into_response(),
    }
}
