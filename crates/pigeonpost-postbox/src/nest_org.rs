//! Organization-owned Nests (Nests Phase B7, R-74).
//!
//! | Route | Who | What |
//! |---|---|---|
//! | `GET /v1/orgs/{org}/nests` | an active member | the org's Nests: all of them for its Nest admins (owner, admin, agent_admin), those they operate for everyone else |
//! | `POST /v1/orgs/{org}/nests {handle, label, operators?}` | a Nest admin (`ManageAgents`) | a new Nest on a new org agent address |
//! | `GET /v1/orgs/{org}/nests/{nest}/operators` | a Nest admin | the members who operate it |
//! | `PUT /v1/orgs/{org}/nests/{nest}/operators {operators}` | a Nest admin | replace that set |
//!
//! An org Nest is an org agent (its mailbox is under the org's internal account and its handle in
//! the org's namespace), so it uses one of the org's agent addresses (10 × seats). Separately, it
//! needs a free slot of a Nest package the **organization** bought: address accounting and compute
//! entitlement are never the same thing. Operators get the `operate_nest` delegation; removing a
//! member or revoking it ends their access everywhere at once (see `nest_access`). Nest admins always
//! have access, so they are never stored as operators: an admin named in a list is skipped.

use crate::groups::{GErr, GResult};
use crate::nest_access::{access_in, Role};
use crate::nest_catalog;
use crate::org::Perm;
use crate::store::nest_funding;
use crate::{capabilities, now_unix, AppState};
use axum::extract::rejection::JsonRejection;
use axum::extract::{Path, State};
use axum::http::{HeaderMap, StatusCode};
use axum::response::{IntoResponse, Response};
use axum::routing::get;
use axum::{Json, Router};
use rusqlite::params;
use serde::Deserialize;
use serde_json::{json, Value};

pub(crate) fn router() -> Router<AppState> {
    Router::new()
        .route("/v1/orgs/{org}/nests", get(list).post(create))
        .route(
            "/v1/orgs/{org}/nests/{nest}/operators",
            get(operators).put(set_operators),
        )
}

/// At most this many operators per Nest, at creation and after any change.
const MAX_OPERATORS: usize = 50;
const OPERATE: &str = "operate_nest";

async fn flag(state: &AppState, account: &str) -> GResult<()> {
    if crate::filtering::feature_on(state, Some(account), capabilities::NESTS).await {
        Ok(())
    } else {
        Err(GErr::forbidden(
            "feature_disabled",
            "Nests are not enabled for this account",
        ))
    }
}

/// `GET /v1/orgs/{org}/nests`
async fn list(
    State(state): State<AppState>,
    headers: HeaderMap,
    Path(org): Path<String>,
) -> Response {
    let result = async {
        let ctx = crate::org::org_ctx(&state, &headers, &org).await?;
        ctx.require(&state, Perm::View)?;
        flag(&state, &ctx.account).await?;
        let internal = ctx.org.internal_account.clone().unwrap_or_default();
        let account = ctx.account.clone();
        let now = now_unix();
        let body = state
            .store
            .group_read(move |c| -> GResult<Value> {
                let mut stmt = c.prepare(
                    "SELECT id FROM nests WHERE account_id = ?1 AND deleted_at IS NULL ORDER BY created_at, id",
                )?;
                let ids = stmt
                    .query_map([&internal], |r| r.get::<_, String>(0))?
                    .collect::<rusqlite::Result<Vec<_>>>()?;
                let mut nests = Vec::new();
                for id in ids {
                    if let Some((role, n)) = access_in(c, &id, &account, now)? {
                        nests.push(json!({
                            "nest_id": n.nest_id,
                            "address": n.address,
                            "label": n.label,
                            "state": n.state,
                            "role": if role == Role::Owner { "owner" } else { "operator" },
                        }));
                    }
                }
                let allowance = nest_funding::allowance(c, &internal, now)?;
                let package = allowance
                    .funding
                    .as_ref()
                    .and_then(|f| nest_catalog::by_code(f.package))
                    .map(|p| json!({ "code": p.code, "slots": p.slots }));
                Ok(json!({ "nests": nests, "allowance": { "slots": allowance.slots, "used": allowance.used }, "package": package }))
            })
            .await?;
        Ok::<_, GErr>(Json(body).into_response())
    }
    .await;
    result.unwrap_or_else(|e| e.into_response())
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct CreateReq {
    handle: String,
    label: String,
    #[serde(default)]
    operators: Vec<String>,
}

/// `POST /v1/orgs/{org}/nests`
async fn create(
    State(state): State<AppState>,
    headers: HeaderMap,
    Path(org): Path<String>,
    body: Result<Json<Value>, JsonRejection>,
) -> Response {
    let result = async {
        let ctx = crate::org::org_ctx(&state, &headers, &org).await?;
        ctx.require(&state, Perm::ManageAgents)?;
        ctx.require_writable()?;
        flag(&state, &ctx.account).await?;
        let Json(raw) = body.map_err(|_| GErr::bad("invalid_json", "expected {handle, label, operators?}"))?;
        let req: CreateReq = serde_json::from_value(raw).map_err(|e| GErr::bad("invalid_request", e.to_string()))?;
        let label = req.label.trim().to_string();
        if label.is_empty() || label.chars().count() > 64 || label.chars().any(char::is_control) {
            return Err(GErr::bad("bad_label", "label: 1-64 printable characters"));
        }
        if req.operators.len() > MAX_OPERATORS {
            return Err(GErr::bad("too_many_operators", "at most 50 operators"));
        }
        let handle = crate::org::org_handle(&ctx.org, &req.handle)?;
        let internal = ctx
            .org
            .internal_account
            .clone()
            .ok_or_else(|| GErr::conflict("org_inactive", "organization not provisioned"))?;
        let now = now_unix();
        // The package and its free slot first: no agent address is used for a Nest that cannot run.
        let (int, n) = (internal.clone(), now);
        let package = state
            .store
            .group_read(move |c| -> GResult<Option<&'static nest_catalog::Package>> {
                let a = nest_funding::allowance(c, &int, n)?;
                if a.used >= a.slots {
                    return Ok(None);
                }
                Ok(a.funding.and_then(|f| nest_catalog::by_code(f.package)))
            })
            .await?
            .ok_or_else(|| {
                GErr::new(StatusCode::PAYMENT_REQUIRED, "no_nest_slot", "the organization has no free Nest slot; buy or upgrade a Nest package")
            })?;
        // The org agent address (counted against 10 × seats) and the operators' delegations.
        let vault = state.vault.clone();
        let (o_id, me, h, l, ops) = (ctx.org.id.clone(), ctx.member.id.clone(), handle.clone(), label.clone(), req.operators.clone());
        let auditor = state.auditor.clone();
        let account = ctx.account.clone();
        let address = state
            .store
            .group_tx(move |tx| -> GResult<String> {
                let o = crate::org::load_org(tx, &o_id)?.ok_or_else(GErr::not_found)?;
                let address = crate::org_agents::create_agent_in(tx, &vault, &o, &h, Some(&l), &me, now)?;
                let caps = vec![OPERATE.to_string()];
                let mut ops = ops;
                ops.sort();
                ops.dedup();
                for op in &ops {
                    // Nest admins always have access; they are never stored as operators.
                    if !nest_admin_in(tx, &o_id, op)? {
                        crate::org_agents::delegate_in(tx, &o_id, op, &address, &caps, None, &me, now)?;
                    }
                }
                crate::org::audit_in(
                    tx,
                    &auditor,
                    &o_id,
                    crate::audit::ActorKind::Human,
                    Some(&account),
                    "nest.create",
                    Some(&address),
                    json!({ "handle": h, "operators": ops.len() }),
                    None,
                    now,
                )?;
                Ok(address)
            })
            .await?;
        let gib = (package.per_nest_storage_bytes() / (1024 * 1024 * 1024)) as u64;
        let provisioned = crate::nests::provision(
            &state,
            crate::nests::Provision {
                account: internal,
                label: label.clone(),
                senders: Vec::new(),
                funding: crate::nests::Funding::Package,
                budgets: package.per_nest_tokens(),
                quota_gib: Some(gib.max(1)),
                audit_service: None,
                address: Some(address.clone()),
            },
            None,
        )
        .await;
        match provisioned {
            Ok((nest_id, address, st)) => Ok((
                StatusCode::CREATED,
                Json(json!({ "nest_id": nest_id, "address": address, "handle": handle, "state": st })),
            )
                .into_response()),
            Err(e) => {
                // Give the address back: no agent slot is held by a Nest that does not exist.
                let (o_id, a) = (ctx.org.id.clone(), address.clone());
                let _ = state
                    .store
                    .group_tx(move |tx| -> GResult<()> {
                        tx.execute(
                            "UPDATE org_agents SET status = 'deleted', deleted_at = ?3 WHERE org_id = ?1 AND identity = ?2",
                            params![o_id, a, now_unix() as i64],
                        )?;
                        Ok(())
                    })
                    .await;
                Err(e)
            }
        }
    }
    .await;
    result.unwrap_or_else(|e: GErr| e.into_response())
}

/// Whether member `member` of `org` holds a Nest admin role (owner, admin, agent_admin).
fn nest_admin_in(c: &rusqlite::Connection, org: &str, member: &str) -> GResult<bool> {
    let roles = crate::org::roles_of(c, org, member)?;
    Ok(crate::org::permits(&roles, Perm::ManageAgents, false))
}

/// The mailbox address of the live Nest `nest` held by the org's internal account, or 404.
fn org_nest_address_in(c: &rusqlite::Connection, internal: &str, nest: &str) -> GResult<String> {
    use rusqlite::OptionalExtension;
    let missing = || GErr::new(StatusCode::NOT_FOUND, "no_nest", "no such Nest");
    if !crate::nests::nest_id_ok(nest) {
        return Err(missing());
    }
    c.query_row(
        "SELECT address FROM nests WHERE id = ?1 AND account_id = ?2 AND deleted_at IS NULL",
        params![nest, internal],
        |r| r.get::<_, Option<String>>(0),
    )
    .optional()?
    .flatten()
    .ok_or_else(missing)
}

/// One live delegation carrying `operate_nest` on a Nest's mailbox.
struct OperatorRow {
    member_id: String,
    delegation: String,
    caps: Vec<String>,
    expires_at: Option<i64>,
    json: Value,
}

/// The live `operate_nest` delegations on `address` held by active members (the conditions
/// `nest_access::operates_in` checks, minus the org's status), ordered by handle.
fn operators_in(
    c: &rusqlite::Connection,
    org: &str,
    address: &str,
    now: u64,
) -> GResult<Vec<OperatorRow>> {
    let mut stmt = c.prepare(
        "SELECT d.grantee_member_id, d.id, d.capabilities, d.expires_at, m.handle, m.first_name, m.last_name
           FROM org_delegations d JOIN org_members m ON m.org_id = d.org_id AND m.id = d.grantee_member_id
          WHERE d.org_id = ?1 AND d.target_identity = ?2 AND d.revoked_at IS NULL
            AND (d.expires_at IS NULL OR d.expires_at > ?3) AND m.status = 'active'
            AND EXISTS (SELECT 1 FROM json_each(d.capabilities) WHERE value = 'operate_nest')
          ORDER BY m.handle, m.id",
    )?;
    let rows = stmt.query_map(params![org, address, now as i64], |r| {
        let member_id: String = r.get(0)?;
        let caps: String = r.get(2)?;
        let json = json!({
            "member_id": member_id,
            "handle": r.get::<_, Option<String>>(4)?,
            "first_name": r.get::<_, Option<String>>(5)?,
            "last_name": r.get::<_, Option<String>>(6)?,
        });
        Ok(OperatorRow {
            member_id,
            delegation: r.get(1)?,
            caps: serde_json::from_str(&caps).unwrap_or_default(),
            expires_at: r.get(3)?,
            json,
        })
    })?;
    Ok(rows.collect::<rusqlite::Result<_>>()?)
}

/// `GET /v1/orgs/{org}/nests/{nest}/operators`
async fn operators(
    State(state): State<AppState>,
    headers: HeaderMap,
    Path((org, nest)): Path<(String, String)>,
) -> Response {
    let result = async {
        let ctx = crate::org::org_ctx(&state, &headers, &org).await?;
        ctx.require(&state, Perm::ManageAgents)?;
        flag(&state, &ctx.account).await?;
        let internal = ctx.org.internal_account.clone().unwrap_or_default();
        let o_id = ctx.org.id.clone();
        let now = now_unix();
        let list = state
            .store
            .group_read(move |c| -> GResult<Vec<Value>> {
                let address = org_nest_address_in(c, &internal, &nest)?;
                Ok(operators_in(c, &o_id, &address, now)?
                    .into_iter()
                    .map(|o| o.json)
                    .collect())
            })
            .await?;
        Ok::<_, GErr>(Json(json!({ "operators": list })).into_response())
    }
    .await;
    result.unwrap_or_else(|e| e.into_response())
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct OperatorsReq {
    operators: Vec<String>,
}

/// `PUT /v1/orgs/{org}/nests/{nest}/operators {operators: [member ids]}`: the new set.
///
/// Only active members may be named; Nest admins are skipped (they always have access). Members
/// who leave the set lose `operate_nest` at once; any other capability their delegation carried is
/// kept. Sending the current set again changes nothing and writes no audit record.
async fn set_operators(
    State(state): State<AppState>,
    headers: HeaderMap,
    Path((org, nest)): Path<(String, String)>,
    body: Result<Json<Value>, JsonRejection>,
) -> Response {
    let result = async {
        let ctx = crate::org::org_ctx(&state, &headers, &org).await?;
        ctx.require(&state, Perm::ManageAgents)?;
        flag(&state, &ctx.account).await?;
        let Json(raw) = body.map_err(|_| GErr::bad("invalid_json", "expected {operators}"))?;
        let req: OperatorsReq = serde_json::from_value(raw).map_err(|e| GErr::bad("invalid_request", e.to_string()))?;
        if req.operators.len() > MAX_OPERATORS {
            return Err(GErr::bad("too_many_operators", "at most 50 operators"));
        }
        // Adding authority is refused on a read-only or suspended org; taking it away never is.
        let read_only = ctx.require_writable().err();
        let internal = ctx.org.internal_account.clone().unwrap_or_default();
        let (o_id, me, account) = (ctx.org.id.clone(), ctx.member.id.clone(), ctx.account.clone());
        let auditor = state.auditor.clone();
        let now = now_unix();
        let list = state
            .store
            .group_tx(move |tx| -> GResult<Vec<Value>> {
                let address = org_nest_address_in(tx, &internal, &nest)?;
                let mut wanted = std::collections::BTreeSet::new();
                for id in &req.operators {
                    crate::org::load_member(tx, &o_id, id)?
                        .filter(|m| m.status == "active")
                        .ok_or_else(|| GErr::bad("invalid_member", "operators are active members of this organization"))?;
                    if !nest_admin_in(tx, &o_id, id)? {
                        wanted.insert(id.clone());
                    }
                }
                let current = operators_in(tx, &o_id, &address, now)?;
                let held: std::collections::BTreeSet<&String> = current.iter().map(|o| &o.member_id).collect();
                let added: Vec<&String> = wanted.iter().filter(|id| !held.contains(id)).collect();
                let removed: Vec<&OperatorRow> = current.iter().filter(|o| !wanted.contains(&o.member_id)).collect();
                if added.is_empty() && removed.is_empty() {
                    return Ok(current.into_iter().map(|o| o.json).collect());
                }
                if let (Some(e), false) = (read_only, added.is_empty()) {
                    return Err(e);
                }
                let known = |c: &String| crate::org_agents::CAPABILITIES.contains(&c.as_str());
                for id in &added {
                    // Keep whatever else a live delegation on this mailbox already grants.
                    let other: Option<(String, Option<i64>)> = {
                        use rusqlite::OptionalExtension;
                        tx.query_row(
                            "SELECT capabilities, expires_at FROM org_delegations
                              WHERE org_id = ?1 AND grantee_member_id = ?2 AND target_identity = ?3
                                AND revoked_at IS NULL AND (expires_at IS NULL OR expires_at > ?4)",
                            params![o_id, id, address, now as i64],
                            |r| Ok((r.get(0)?, r.get(1)?)),
                        )
                        .optional()?
                    };
                    let (mut caps, expires) = match other {
                        Some((c, e)) => (serde_json::from_str::<Vec<String>>(&c).unwrap_or_default(), e),
                        None => (Vec::new(), None),
                    };
                    caps.retain(known);
                    caps.push(OPERATE.to_string());
                    crate::org_agents::delegate_in(tx, &o_id, id, &address, &caps, expires, &me, now)?;
                }
                for o in &removed {
                    let rest: Vec<String> = o.caps.iter().filter(|c| c.as_str() != OPERATE && known(c)).cloned().collect();
                    if !rest.is_empty() {
                        crate::org_agents::delegate_in(tx, &o_id, &o.member_id, &address, &rest, o.expires_at, &me, now)?;
                        continue;
                    }
                    tx.execute(
                        "UPDATE org_delegations SET revoked_at = ?2, revision = revision + 1 WHERE id = ?1 AND revoked_at IS NULL",
                        params![o.delegation, now as i64],
                    )?;
                    // Credentials it approved stop with it, as on DELETE /v1/orgs/{id}/delegations/{d}.
                    tx.execute(
                        "UPDATE device_credentials SET revoked_at = ?2 WHERE revoked_at IS NULL AND parent_grant_id IN
                            (SELECT id FROM device_grants WHERE delegation_id = ?1)",
                        params![o.delegation, now as i64],
                    )?;
                    tx.execute(
                        "UPDATE device_grants SET revoked_at = ?2 WHERE delegation_id = ?1 AND revoked_at IS NULL",
                        params![o.delegation, now as i64],
                    )?;
                }
                let (n_added, n_removed) = (added.len(), removed.len());
                let after = operators_in(tx, &o_id, &address, now)?;
                crate::org::audit_in(
                    tx,
                    &auditor,
                    &o_id,
                    crate::audit::ActorKind::Human,
                    Some(&account),
                    "nest.operators",
                    Some(&address),
                    json!({ "added": n_added, "removed": n_removed, "operators": after.len() }),
                    None,
                    now,
                )?;
                Ok(after.into_iter().map(|o| o.json).collect())
            })
            .await?;
        // Authority may have changed: re-check open SSH sessions now (SDS §11.5).
        crate::shell::sweep_soon(&state);
        Ok::<_, GErr>(Json(json!({ "operators": list })).into_response())
    }
    .await;
    result.unwrap_or_else(|e| e.into_response())
}
