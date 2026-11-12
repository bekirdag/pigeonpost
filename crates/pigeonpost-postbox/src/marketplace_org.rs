//! Marketplace M2b: organization-owned installations (build plan R-51, R-74).
//!
//! | Route | Who | What |
//! |---|---|---|
//! | `GET /v1/orgs/{org}/nest-installations` | an active member | all of the org's installations for its Nest admins; those they operate for everyone else |
//! | `POST /v1/orgs/{org}/nest-installations {listing_id, handle, label?, configuration?, operators?, purchase_id?}` | a Nest admin (`ManageAgents`) | install a listing for the org, on a new org agent address; a priced listing needs the org's paid purchase |
//! | `PUT\|DELETE /v1/orgs/{org}/nest-installations/{installation}/operators/{member}` | a Nest admin | give or take a member's `operate_nest` delegation |
//!
//! The rest of an org installation's life is the M2 routes (`/v1/nest-installations/{id}` read,
//! configure, update, rollback, uninstall), allowed to the org's Nest admins; operators may read it.
//!
//! An org installation works like an org Nest (`nest_org`, `nest_access`): it holds one of the
//! org's agent addresses, its operators hold a live `operate_nest` delegation on that address,
//! and authority is resolved from live server state on every request. Removing a member, revoking
//! a delegation or deleting the agent ends that person's use at once. A member invokes it by
//! sending `x-pigeonpost-installation: <id>` with a quote and a job; its jobs run in one worker per
//! listing and org, with the org's configuration. Uninstalling deletes the agent address and with
//! it every delegation.

use crate::groups::{GErr, GResult};
use crate::marketplace::{feature_off, into_response, not_found, parse};
use crate::marketplace_grants::{account_grant_in, org_grant_in, Use};
use crate::marketplace_m1::buyer_context;
use crate::marketplace_m2::{
    inst_event_in, inst_json_in, live_attested_in, new_id, not_runnable, prepare_config,
    publisher_read, release_in, schema_of, store_secrets_in, Inst,
};
use crate::nest_access::{operates_in, Role};
use crate::org::Perm;
use crate::{capabilities, now_unix, AppState};
use axum::extract::rejection::JsonRejection;
use axum::extract::{Path, State};
use axum::http::{HeaderMap, StatusCode};
use axum::response::Response;
use axum::routing::{get, put};
use axum::{Json, Router};
use rusqlite::{params, Connection, OptionalExtension};
use serde::Deserialize;
use serde_json::{json, Value};
use std::collections::BTreeSet;

pub(crate) fn router() -> Router<AppState> {
    Router::new()
        .route("/v1/orgs/{org}/nest-installations", get(list).post(install))
        .route(
            "/v1/orgs/{org}/nest-installations/{installation}/operators/{member}",
            put(add_operator).delete(remove_operator),
        )
}

/// The caller's role on an org installation, from live state: `Owner` for the org's Nest admins
/// (owner, admin, agent_admin) while the org is live, `Operator` for a live `operate_nest`
/// delegation on its agent address. `None` for anyone else or an account's installation.
pub(crate) fn org_role_in(
    c: &Connection,
    inst: &Inst,
    account: &str,
    now: u64,
) -> rusqlite::Result<Option<Role>> {
    if inst.owner_kind != "org" {
        return Ok(None);
    }
    let admin: bool = c.query_row(
        "SELECT EXISTS(SELECT 1 FROM org_roles r JOIN org_members m ON m.org_id = r.org_id AND m.id = r.member_id
                         JOIN orgs o ON o.id = r.org_id
            WHERE r.org_id = ?1 AND m.account_id = ?2 AND m.status = 'active'
              AND r.role IN ('owner','admin','agent_admin') AND o.status IN ('active','past_due','read_only'))",
        params![inst.owner_id, account],
        |r| r.get(0),
    )?;
    if admin {
        return Ok(Some(Role::Owner));
    }
    let Some(agent) = &inst.agent_address else {
        return Ok(None);
    };
    Ok(operates_in(c, account, agent, now)?.then_some(Role::Operator))
}

/// Uninstall, org side: the agent address is deleted and every delegation on it revoked. Nothing
/// for an account's installation.
pub(crate) fn retire_agent_in(c: &Connection, installation: &str, now: u64) -> GResult<()> {
    let row: Option<(String, Option<String>)> = c
        .query_row(
            "SELECT owner_id, agent_address FROM nest_installations WHERE id = ?1 AND owner_kind = 'org'",
            [installation],
            |r| Ok((r.get(0)?, r.get(1)?)),
        )
        .optional()?;
    let Some((org, Some(agent))) = row else {
        return Ok(());
    };
    c.execute(
        "UPDATE org_delegations SET revoked_at = ?3, revision = revision + 1
          WHERE org_id = ?1 AND target_identity = ?2 AND revoked_at IS NULL",
        params![org, agent, now as i64],
    )?;
    c.execute(
        "UPDATE org_agents SET status = 'deleted', deleted_at = ?3 WHERE org_id = ?1 AND identity = ?2",
        params![org, agent, now as i64],
    )?;
    c.execute(
        "UPDATE identities SET handle = NULL WHERE address = ?1",
        [&agent],
    )?;
    Ok(())
}

async fn flag(state: &AppState, account: &str) -> GResult<()> {
    if crate::filtering::feature_on(state, Some(account), capabilities::MARKETPLACE).await {
        Ok(())
    } else {
        Err(feature_off())
    }
}

/// An org installation's own fields, beside the M2 view.
fn org_fields_in(c: &Connection, v: &mut Value, id: &str) -> rusqlite::Result<()> {
    let (kind, owner, agent): (String, String, Option<String>) = c.query_row(
        "SELECT owner_kind, owner_id, agent_address FROM nest_installations WHERE id = ?1",
        [id],
        |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)),
    )?;
    v["owner"] = json!({ "kind": kind, "id": owner });
    v["agent_address"] = json!(agent);
    Ok(())
}

/// `GET /v1/orgs/{org}/nest-installations`
async fn list(
    State(state): State<AppState>,
    headers: HeaderMap,
    Path(org): Path<String>,
) -> Response {
    let result = async {
        let ctx = crate::org::org_ctx(&state, &headers, &org).await?;
        ctx.require(&state, Perm::View)?;
        flag(&state, &ctx.account).await?;
        let (oid, account) = (ctx.org.id.clone(), ctx.account.clone());
        let now = now_unix();
        let rows = state
            .store
            .group_read(move |c| -> GResult<Vec<Value>> {
                let mut stmt = c.prepare(
                    "SELECT id FROM nest_installations WHERE owner_kind = 'org' AND owner_id = ?1
                      ORDER BY status = 'active' DESC, created_at DESC, id LIMIT 100",
                )?;
                let ids = stmt
                    .query_map([&oid], |r| r.get::<_, String>(0))?
                    .collect::<rusqlite::Result<Vec<_>>>()?;
                let mut out = Vec::new();
                for id in ids {
                    let Some(i) = crate::marketplace_m2::inst_in(c, &id)? else {
                        continue;
                    };
                    let Some(role) = org_role_in(c, &i, &account, now)? else {
                        continue;
                    };
                    let mut v = inst_json_in(c, &id)?;
                    org_fields_in(c, &mut v, &id)?;
                    v["role"] = json!(if role == Role::Owner {
                        "admin"
                    } else {
                        "operator"
                    });
                    out.push(v);
                }
                Ok(out)
            })
            .await?;
        Ok(Json(json!({ "data": rows, "next_cursor": null })))
    }
    .await;
    into_response(result)
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct InstallReq {
    listing_id: String,
    /// The org agent address the installation is used through, in the org's namespace.
    handle: String,
    #[serde(default)]
    label: Option<String>,
    #[serde(default)]
    configuration: Option<Value>,
    /// Members given `operate_nest` on it.
    #[serde(default)]
    operators: Vec<String>,
    /// A priced listing: the org's paid purchase this install consumes (`marketplace_pay`).
    #[serde(default)]
    purchase_id: Option<String>,
}

/// `POST /v1/orgs/{org}/nest-installations`
async fn install(
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
        let req: InstallReq = parse(body)?;
        // Not available yet: shown in the catalog, never sold, installed or run.
        crate::marketplace_listings::refuse_coming_soon(&state, &req.listing_id).await?;
        if req.operators.len() > 50 {
            return Err(GErr::bad("too_many_operators", "at most 50 operators"));
        }
        let label = match req.label.as_deref().map(str::trim) {
            None | Some("") => None,
            Some(l) if l.chars().count() <= 64 && !l.chars().any(char::is_control) => {
                Some(l.to_string())
            }
            Some(_) => return Err(GErr::bad("bad_label", "label: 1-64 printable characters")),
        };
        let handle = crate::org::org_handle(&ctx.org, &req.handle)?;
        type Found = (String, String, String, Option<crate::marketplace_m2::Rel>, bool, bool, bool, bool);
        let (lid, oid, acct) = (req.listing_id.clone(), ctx.org.id.clone(), ctx.account.clone());
        let found = state
            .store
            .group_read(move |c| -> GResult<Option<Found>> {
                let row: Option<(String, String, String, String)> = c
                    .query_row(
                        "SELECT publisher_id, visibility, status, release_id FROM nest_listings WHERE id = ?1",
                        [&lid],
                        |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?)),
                    )
                    .optional()?;
                let Some((publisher, visibility, status, release)) = row else {
                    return Ok(None);
                };
                let rel = release_in(c, &release)?;
                let attested = match &rel {
                    Some(r) => live_attested_in(c, r)?,
                    None => false,
                };
                let pub_ok: bool = c.query_row(
                    "SELECT status = 'approved' FROM publisher_profiles WHERE id = ?1",
                    [&publisher],
                    |r| r.get(0),
                )?;
                let see = org_grant_in(c, &lid, &visibility, &oid, Use::Discover)?
                    || account_grant_in(c, &lid, &visibility, &acct, Use::Discover)?;
                let may = org_grant_in(c, &lid, &visibility, &oid, Use::Install)?;
                Ok(Some((publisher, visibility, status, rel, attested, pub_ok, see, may)))
            })
            .await?;
        let Some((publisher, visibility, status, rel, attested, pub_ok, see, may)) = found else {
            return Err(not_found());
        };
        let visible = status == "published"
            && pub_ok
            && match visibility.as_str() {
                "public" | "unlisted" => true,
                "private" => publisher_read(&state, &headers, &ctx.account, &publisher).await,
                _ => {
                    if see && !may {
                        return Err(GErr::forbidden(
                            "grant_required",
                            "this organization's access to the listing does not include installing it",
                        ));
                    }
                    may
                }
            };
        if !visible {
            return Err(not_found());
        }
        let rel = rel.ok_or_else(not_found)?;
        if !attested {
            return Err(not_runnable(&rel));
        }
        // Marketplace payments: a priced listing installs only under the org's paid purchase.
        let pending = match crate::marketplace_pay::admit_install(
            &state,
            &req.listing_id,
            &ctx.account,
            Some(&ctx.org.id),
            req.purchase_id.as_deref(),
        )
        .await?
        {
            crate::marketplace_pay::Admission::Free => None,
            crate::marketplace_pay::Admission::Pending(p) => Some(p),
            crate::marketplace_pay::Admission::Replay(id) => {
                let v = state
                    .store
                    .group_read(move |c| -> GResult<Value> {
                        let mut v = inst_json_in(c, &id)?;
                        org_fields_in(c, &mut v, &id)?;
                        Ok(v)
                    })
                    .await?;
                return Ok((StatusCode::OK, Json(json!({ "data": v }))));
            }
        };
        let installation = pending
            .as_ref()
            .map(|p| p.installation_id.clone())
            .unwrap_or_else(|| new_id("ins"));
        let schema = schema_of(&rel.manifest);
        let config = req.configuration.clone().unwrap_or_else(|| json!({}));
        let (plain, sealed) =
            prepare_config(&state, &schema, &config, &installation, &BTreeSet::new())?;
        // Every check passed: consume the org's payment for this installation (idempotent).
        let paid = match pending {
            Some(p) => Some(crate::marketplace_pay::consume(&state, p).await?),
            None => None,
        };
        let now = now_unix();
        let vault = state.vault.clone();
        let auditor = state.auditor.clone();
        let (o_id, me, account) = (ctx.org.id.clone(), ctx.member.id.clone(), ctx.account.clone());
        let (iid, listing, release) = (installation.clone(), req.listing_id.clone(), rel.id.clone());
        let context = buyer_context(&listing, &format!("org:{o_id}"));
        let ops = req.operators.clone();
        let v = state
            .store
            .group_tx(move |tx| -> GResult<Value> {
                let live: Option<String> = tx
                    .query_row(
                        "SELECT id FROM nest_installations WHERE listing_id = ?1
                           AND owner_kind = 'org' AND owner_id = ?2 AND status = 'active'",
                        params![listing, o_id],
                        |r| r.get(0),
                    )
                    .optional()?;
                if let Some(existing) = live {
                    return Err(GErr::conflict(
                        "already_installed",
                        "this organization already has this listing installed",
                    )
                    .with("installation_id", existing));
                }
                let o = crate::org::load_org(tx, &o_id)?.ok_or_else(not_found)?;
                let address = crate::org_agents::create_agent_in(
                    tx,
                    &vault,
                    &o,
                    &handle,
                    label.as_deref(),
                    &me,
                    now,
                )?;
                let caps = vec!["operate_nest".to_string()];
                for op in &ops {
                    crate::org_agents::delegate_in(tx, &o_id, op, &address, &caps, None, &me, now)?;
                }
                tx.execute(
                    "INSERT INTO nest_installations (id, listing_id, owner_kind, owner_id, buyer_context,
                        release_id, config, status, installed_by, created_at, updated_at, agent_address)
                     VALUES (?1, ?2, 'org', ?3, ?4, ?5, ?6, 'active', ?7, ?8, ?8, ?9)",
                    params![iid, listing, o_id, context, release, plain, account, now as i64, address],
                )?;
                store_secrets_in(tx, &iid, &sealed, now as i64)?;
                if let Some(paid) = &paid {
                    crate::marketplace_pay::record_in(tx, paid, now as i64)?;
                }
                inst_event_in(tx, &iid, "installed", None, Some(&release), &account, now as i64)?;
                crate::org::audit_in(
                    tx,
                    &auditor,
                    &o_id,
                    crate::audit::ActorKind::Human,
                    Some(&account),
                    "marketplace.install",
                    Some(&address),
                    json!({ "installation_id": iid, "listing_id": listing, "operators": ops.len() }),
                    None,
                    now,
                )?;
                let mut v = inst_json_in(tx, &iid)?;
                org_fields_in(tx, &mut v, &iid)?;
                v["handle"] = json!(handle);
                Ok(v)
            })
            .await?;
        Ok((StatusCode::CREATED, Json(json!({ "data": v }))))
    }
    .await;
    into_response(result)
}

/// The org installation, for a Nest admin of `org`.
async fn admin_gate(
    state: &AppState,
    headers: &HeaderMap,
    org: &str,
    installation: &str,
) -> GResult<(crate::org::OrgCtx, Inst)> {
    let ctx = crate::org::org_ctx(state, headers, org).await?;
    ctx.require(state, Perm::ManageAgents)?;
    flag(state, &ctx.account).await?;
    let (iid, oid) = (installation.to_string(), ctx.org.id.clone());
    let inst = state
        .store
        .group_read(move |c| -> GResult<Option<Inst>> {
            Ok(crate::marketplace_m2::inst_in(c, &iid)?
                .filter(|i| i.owner_kind == "org" && i.owner_id == oid))
        })
        .await?
        .ok_or_else(not_found)?;
    if inst.status != "active" {
        return Err(GErr::conflict(
            "installation_removed",
            "this installation was uninstalled",
        ));
    }
    Ok((ctx, inst))
}

/// `PUT /v1/orgs/{org}/nest-installations/{installation}/operators/{member}` — a live
/// `operate_nest` delegation on the installation's agent address (a new one replaces the old).
async fn add_operator(
    State(state): State<AppState>,
    headers: HeaderMap,
    Path((org, installation, member)): Path<(String, String, String)>,
) -> Response {
    let result = async {
        let (ctx, inst) = admin_gate(&state, &headers, &org, &installation).await?;
        ctx.require_writable()?;
        let agent = inst.agent_address.clone().unwrap_or_default();
        let (oid, me, account) = (
            ctx.org.id.clone(),
            ctx.member.id.clone(),
            ctx.account.clone(),
        );
        let auditor = state.auditor.clone();
        let now = now_unix();
        let delegation = state
            .store
            .group_tx(move |tx| -> GResult<String> {
                let caps = vec!["operate_nest".to_string()];
                let id = crate::org_agents::delegate_in(
                    tx, &oid, &member, &agent, &caps, None, &me, now,
                )?;
                crate::org::audit_in(
                    tx,
                    &auditor,
                    &oid,
                    crate::audit::ActorKind::Human,
                    Some(&account),
                    "marketplace.operator.add",
                    Some(&agent),
                    json!({ "installation_id": inst.id, "member_id": member }),
                    None,
                    now,
                )?;
                Ok(id)
            })
            .await?;
        Ok(Json(
            json!({ "data": { "delegation_id": delegation, "capabilities": ["operate_nest"] } }),
        ))
    }
    .await;
    into_response(result)
}

/// `DELETE /v1/orgs/{org}/nest-installations/{installation}/operators/{member}`
async fn remove_operator(
    State(state): State<AppState>,
    headers: HeaderMap,
    Path((org, installation, member)): Path<(String, String, String)>,
) -> Response {
    let result = async {
        let (ctx, inst) = admin_gate(&state, &headers, &org, &installation).await?;
        let agent = inst.agent_address.clone().unwrap_or_default();
        let (oid, account) = (ctx.org.id.clone(), ctx.account.clone());
        let auditor = state.auditor.clone();
        let now = now_unix();
        let n = state
            .store
            .group_tx(move |tx| -> GResult<usize> {
                let n = tx.execute(
                    "UPDATE org_delegations SET revoked_at = ?4, revision = revision + 1
                      WHERE org_id = ?1 AND grantee_member_id = ?2 AND target_identity = ?3
                        AND revoked_at IS NULL",
                    params![oid, member, agent, now as i64],
                )?;
                if n > 0 {
                    crate::org::audit_in(
                        tx,
                        &auditor,
                        &oid,
                        crate::audit::ActorKind::Human,
                        Some(&account),
                        "marketplace.operator.remove",
                        Some(&agent),
                        json!({ "installation_id": inst.id, "member_id": member }),
                        None,
                        now,
                    )?;
                }
                Ok(n)
            })
            .await?;
        if n == 0 {
            return Err(not_found());
        }
        Ok(Json(json!({ "data": { "revoked": true } })))
    }
    .await;
    into_response(result)
}
