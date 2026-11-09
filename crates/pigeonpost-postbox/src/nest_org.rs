//! Organization-owned Nests (Nests Phase B7, R-74).
//!
//! | Route | Who | What |
//! |---|---|---|
//! | `GET /v1/orgs/{org}/nests` | an active member | the org's Nests: all of them for its Nest admins (owner, admin, agent_admin), those they operate for everyone else |
//! | `POST /v1/orgs/{org}/nests {handle, label, operators?}` | a Nest admin (`ManageAgents`) | a new Nest on a new org agent address |
//!
//! An org Nest is an org agent (its mailbox is under the org's internal account and its handle in
//! the org's namespace), so it uses one of the org's agent addresses (10 × seats). Separately, it
//! needs a free slot of a Nest package the **organization** bought: address accounting and compute
//! entitlement are never the same thing. Operators get the `operate_nest` delegation; removing a
//! member or revoking it ends their access everywhere at once (see `nest_access`).

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
    Router::new().route("/v1/orgs/{org}/nests", get(list).post(create))
}

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
        if req.operators.len() > 50 {
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
                let caps = vec!["operate_nest".to_string()];
                for op in &ops {
                    crate::org_agents::delegate_in(tx, &o_id, op, &address, &caps, None, &me, now)?;
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
