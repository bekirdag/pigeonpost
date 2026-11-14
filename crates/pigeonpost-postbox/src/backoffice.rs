//! The Piyote back office (`pigeonpost.dev/staff`, plan `docs/planning/2026-11-11-backoffice-plan.md`,
//! Phase 1): read APIs over the postbox's own tables for staff, every one behind a staff role.
//!
//! | Route | Action | Roles | Step-up | Audit |
//! |---|---|---|---|---|
//! | `GET /v1/staff/me` | `StaffOverview` | any staff role | none | none (no personal data) |
//! | `GET /v1/staff/dashboard` | `StaffOverview` | any staff role | none | none (counts only) |
//! | `GET /v1/staff/accounts?q=` | `StaffPeopleRead` | owner, support | ≤ 1 h | `staff.accounts.search` |
//! | `GET /v1/staff/accounts/{account}` | `StaffPeopleRead` | owner, support | ≤ 1 h | `staff.accounts.read` |
//! | `GET /v1/staff/handles?q=` | `StaffPeopleRead` | owner, support | ≤ 1 h | `staff.handles.search` |
//! | `GET /v1/staff/handles/{namespace}` | `StaffPeopleRead` | owner, support | ≤ 1 h | `staff.handles.read` |
//! | `GET /v1/staff/mailboxes?q=&kind=` | `StaffPeopleRead` | owner, support | ≤ 1 h | `staff.mailboxes.search` |
//! | `GET /v1/staff/orgs?q=` | `StaffPeopleRead` | owner, support | ≤ 1 h | `staff.orgs.search` |
//! | `GET /v1/staff/orgs/{org}` | `StaffPeopleRead` | owner, support | ≤ 1 h | `staff.orgs.read` |
//! | `GET /v1/staff/entitlements?q=&source=&status=` | `StaffPlansRead` | support, finance | ≤ 1 h | `staff.plans.search` |
//! | `GET /v1/staff/subscriptions?store=&q=` | `StaffPlansRead` | support, finance | ≤ 1 h | `staff.plans.subscriptions` |
//! | `GET /v1/staff/audit?actor=&action=&target=&from=&to=` | `StaffAuditRead` | owner | ≤ 1 h | `staff.audit.search` / `.export` |
//!
//! The credit screens are `backoffice_finance`; staff role grants are `marketplace.rs`
//! (`/v1/staff/roles`, the configured platform owner only).
//!
//! Rules every handler here keeps:
//!
//! - **Audit before data.** A personal-data read runs its query and appends its audit row in one
//!   transaction; nothing is returned unless the row committed. The row names the actor, the verb
//!   (`staff.<area>.<verb>`), the target and the filters, never the result.
//! - **No content, no secrets.** Message bodies are never read (they are end-to-end encrypted
//!   anyway). Push tokens, store purchase references, provider references and org domain
//!   verification tokens are masked here, server-side, so the browser never receives them;
//!   key material and capability hashes are never selected at all.
//! - **Pages.** `limit` (1–100, default 25) and an opaque `cursor`; `next_cursor` is `null` on the
//!   last page. The audit reader pages by row id (rows are appended while one reads); the rest by
//!   position in a stable order.
//!
//! The configured platform owner (`POSTBOX_PLATFORM_OWNER_ACCOUNT`) always holds the `owner` role
//! here, so the back office can be opened before the first role row exists. Every other role is a
//! live `staff_roles` row. Unlike the marketplace staff routes, none of this depends on the
//! `marketplace` feature flag.

use crate::audit::ActorKind;
use crate::groups::{GErr, GResult};
use crate::marketplace::{deny, facts, into_response, ip_of, iso};
use crate::policy::{authorize, Action, Actor, Facts, Resource, StaffRole};
use crate::store::AuditRow;
use crate::{capabilities, now_unix, principal_for_token, AppState, ClientIp, Principal};
use axum::extract::{Path, Query, State};
use axum::http::{HeaderMap, StatusCode};
use axum::response::Response;
use axum::routing::get;
use axum::{Extension, Json, Router};
use rusqlite::types::Value as Sql;
use rusqlite::{params_from_iter, Connection, OptionalExtension};
use serde_json::{json, Value};
use std::collections::HashMap;

pub(crate) fn router() -> Router<AppState> {
    Router::new()
        .route("/v1/staff/me", get(me))
        .route("/v1/staff/dashboard", get(dashboard))
        .route("/v1/staff/accounts", get(search_accounts))
        .route("/v1/staff/accounts/{account}", get(read_account))
        .route("/v1/staff/handles", get(search_handles))
        .route("/v1/staff/handles/{namespace}", get(read_handle))
        .route("/v1/staff/mailboxes", get(search_mailboxes))
        .route("/v1/staff/orgs", get(search_orgs))
        .route("/v1/staff/orgs/{org}", get(read_org))
        .route("/v1/staff/entitlements", get(search_entitlements))
        .route("/v1/staff/subscriptions", get(search_subscriptions))
        .route("/v1/staff/audit", get(search_audit))
}

// ---- the gate --------------------------------------------------------------------------------------

/// A person who passed a staff gate.
pub(crate) struct Staff {
    pub principal: Principal,
    pub account: String,
    pub roles: Vec<StaffRole>,
    pub platform_owner: bool,
}

/// The caller's live staff roles: their `staff_roles` rows, plus `owner` for the configured
/// platform owner.
pub(crate) async fn roles_of(state: &AppState, account: &str) -> GResult<(Vec<StaffRole>, bool)> {
    let owner = state.platform_owner.as_deref() == Some(account) && !account.is_empty();
    let who = account.to_string();
    let mut roles = state
        .store
        .group_read(move |c| -> GResult<Vec<StaffRole>> {
            Ok(crate::store::marketplace::live_staff_roles_in(c, &who)?)
        })
        .await?;
    if owner && !roles.contains(&StaffRole::Owner) {
        roles.push(StaffRole::Owner);
    }
    roles.sort();
    roles.dedup();
    Ok((roles, owner))
}

/// Signed in, a person, holding one of `action`'s roles, with the step-up `action` needs. The
/// policy decides all of it (`policy::Action::staff_roles`, `Requirement::StaffRead`).
pub(crate) async fn require_staff(
    state: &AppState,
    headers: &HeaderMap,
    action: Action,
    subject: &str,
) -> GResult<Staff> {
    let principal = principal_for_token(state, crate::bearer(headers)).await?;
    let account = capabilities::principal_account(&principal)
        .unwrap_or_default()
        .to_string();
    let (roles, platform_owner) = roles_of(state, &account).await?;
    let f = Facts {
        staff_roles: roles.clone(),
        ..facts(state)
    };
    authorize(
        &Actor::Principal(&principal),
        action,
        &Resource::StaffSubject(subject),
        &f,
    )
    .map_err(|d| deny(d, false))?;
    Ok(Staff {
        principal,
        account,
        roles,
        platform_owner,
    })
}

/// One `staff.<area>.<verb>` record for `who`.
pub(crate) fn audit_row(
    state: &AppState,
    who: &Staff,
    action: &str,
    target: Option<String>,
    details: Value,
    client_ip: &Option<Extension<ClientIp>>,
    headers: &HeaderMap,
) -> AuditRow {
    state.auditor.row(
        ActorKind::of(&who.principal),
        Some(who.account.clone()),
        action,
        target,
        details,
        ip_of(client_ip),
        headers
            .get(axum::http::header::USER_AGENT)
            .and_then(|v| v.to_str().ok()),
        now_unix(),
    )
}

/// Run `read` and append `row` in one transaction: the data leaves only with its audit record.
async fn audited<T, F>(state: &AppState, row: AuditRow, read: F) -> GResult<T>
where
    T: Send + 'static,
    F: FnOnce(&Connection) -> GResult<T> + Send + 'static,
{
    state
        .store
        .group_tx(move |tx| -> GResult<T> {
            let out = read(tx)?;
            crate::store::audit::append_in(tx, &row)?;
            Ok(out)
        })
        .await
}

// ---- small helpers ------------------------------------------------------------------------------

/// A page: `limit` rows from `offset`. One extra row is fetched to know whether there is more.
#[derive(Debug, Clone, Copy)]
pub(crate) struct Page {
    pub limit: i64,
    pub offset: i64,
}

pub(crate) const MAX_LIMIT: i64 = 100;
const DEFAULT_LIMIT: i64 = 25;

pub(crate) fn page_of(q: &HashMap<String, String>, max: i64) -> GResult<Page> {
    let limit = match q.get("limit") {
        None => DEFAULT_LIMIT.min(max),
        Some(v) => v
            .parse::<i64>()
            .ok()
            .filter(|n| (1..=max).contains(n))
            .ok_or_else(|| GErr::bad("invalid_request", format!("limit: 1-{max}")))?,
    };
    let offset = match q.get("cursor").map(String::as_str) {
        None | Some("") => 0,
        Some(c) => c
            .strip_prefix("o")
            .and_then(|n| n.parse::<i64>().ok())
            .filter(|n| *n >= 0 && *n <= 1_000_000)
            .ok_or_else(|| GErr::bad("invalid_cursor", "this cursor is not one we issued"))?,
    };
    Ok(Page { limit, offset })
}

/// Trim `rows` to the page and say where the next one starts.
fn paged(mut rows: Vec<Value>, page: Page) -> Value {
    let more = rows.len() as i64 > page.limit;
    rows.truncate(page.limit as usize);
    let next = more.then(|| format!("o{}", page.offset + page.limit));
    json!({ "data": rows, "next_cursor": next })
}

/// The search text: trimmed, lowercase, at most 128 characters; `None` when empty.
pub(crate) fn query_text(q: &HashMap<String, String>) -> GResult<Option<String>> {
    let Some(raw) = q.get("q") else {
        return Ok(None);
    };
    let t = raw.trim().to_lowercase();
    if t.chars().count() > 128 {
        return Err(GErr::bad("invalid_request", "q: at most 128 characters"));
    }
    Ok((!t.is_empty()).then_some(t))
}

/// `%text%` for `LIKE … ESCAPE '\'`, with the wildcards in `text` taken literally.
fn like(text: &str) -> String {
    let mut out = String::with_capacity(text.len() + 2);
    out.push('%');
    for ch in text.chars() {
        if matches!(ch, '%' | '_' | '\\') {
            out.push('\\');
        }
        out.push(ch);
    }
    out.push('%');
    out
}

/// Everything but the last `keep` characters replaced, so a reference can be matched by eye but
/// not used. Short values are hidden entirely.
pub(crate) fn mask(value: &str, keep: usize) -> String {
    let n = value.chars().count();
    if n <= keep * 2 {
        return "••••".into();
    }
    let tail: String = value.chars().skip(n - keep).collect();
    format!("••••{tail}")
}

fn when(secs: Option<i64>) -> Value {
    secs.map(|s| json!(iso(s))).unwrap_or(Value::Null)
}

/// One of `allowed`, or `None` when absent.
fn choice(
    q: &HashMap<String, String>,
    key: &'static str,
    allowed: &[&str],
) -> GResult<Option<String>> {
    match q.get(key).map(|s| s.trim()) {
        None | Some("") => Ok(None),
        Some(v) if allowed.contains(&v) => Ok(Some(v.to_string())),
        Some(_) => Err(GErr::bad(
            "invalid_request",
            format!("{key}: one of {}", allowed.join(", ")),
        )),
    }
}

/// Rows of `sql` mapped by `f`.
fn rows<F>(c: &Connection, sql: &str, args: &[Sql], f: F) -> GResult<Vec<Value>>
where
    F: FnMut(&rusqlite::Row<'_>) -> rusqlite::Result<Value>,
{
    let mut stmt = c.prepare(sql)?;
    let out = stmt
        .query_map(params_from_iter(args.iter()), f)?
        .collect::<rusqlite::Result<Vec<_>>>()?;
    Ok(out)
}

fn text(v: &Option<String>) -> Sql {
    v.clone().map(Sql::Text).unwrap_or(Sql::Null)
}

fn not_found() -> GErr {
    GErr::new(StatusCode::NOT_FOUND, "not_found", "no such record")
}

fn filters(pairs: &[(&str, &Option<String>)]) -> Value {
    let mut m = serde_json::Map::new();
    for (k, v) in pairs {
        if let Some(v) = v {
            m.insert((*k).to_string(), json!(v));
        }
    }
    Value::Object(m)
}

// ---- me and the dashboard -----------------------------------------------------------------------

/// `GET /v1/staff/me` — the caller's roles and what each step-up window is, so the web app draws
/// only the screens this person can use.
async fn me(State(state): State<AppState>, headers: HeaderMap) -> Response {
    let result = async {
        let who = require_staff(&state, &headers, Action::StaffOverview, "me").await?;
        Ok(Json(json!({ "data": {
            "account_id": who.account,
            "roles": who.roles.iter().map(|r| r.as_str()).collect::<Vec<_>>(),
            "platform_owner": who.platform_owner,
            "step_up": {
                "acr_values": state.step_up.acr_values.join(" "),
                "write_max_age": state.step_up.max_age,
                "read_max_age": state.step_up.max_age.max(crate::policy::STAFF_READ_STEP_UP_SECS),
            },
            "finance_configured": state.staff_finance.configured(),
        }})))
    }
    .await;
    into_response(result)
}

/// `GET /v1/staff/dashboard` — platform counts. Counts only, so no step-up and no audit row.
/// Credit sales and refunds come from `backoffice_finance` (finance only).
async fn dashboard(State(state): State<AppState>, headers: HeaderMap) -> Response {
    let result = async {
        require_staff(&state, &headers, Action::StaffOverview, "dashboard").await?;
        let now = now_unix() as i64;
        let day = now - now.rem_euclid(86_400);
        let counts = state
            .store
            .group_read(move |c| -> GResult<Value> {
                let n = |sql: &str, args: &[i64]| -> GResult<i64> {
                    Ok(c.query_row(sql, params_from_iter(args.iter()), |r| r.get(0))?)
                };
                Ok(json!({
                    "accounts": n("SELECT COUNT(*) FROM accounts", &[])?,
                    "signups_today": n("SELECT COUNT(*) FROM accounts WHERE created_at >= ?1", &[day])?,
                    "signups_7d": n("SELECT COUNT(*) FROM accounts WHERE created_at >= ?1", &[now - 7 * 86_400])?,
                    "active_accounts_30d": n(
                        "SELECT COUNT(DISTINCT account) FROM devices WHERE account IS NOT NULL AND updated_at >= ?1",
                        &[now - 30 * 86_400],
                    )?,
                    "handles": n("SELECT COUNT(*) FROM namespaces", &[])?,
                    "mailboxes": n("SELECT COUNT(*) FROM identities", &[])?,
                    "orgs_active": n("SELECT COUNT(*) FROM orgs WHERE status = 'active'", &[])?,
                    "nests_running": n(
                        "SELECT COUNT(*) FROM nests WHERE deleted_at IS NULL AND state = 'running'",
                        &[],
                    )?,
                    "open_reviews": n(
                        "SELECT COUNT(*) FROM review_cases WHERE status IN ('open','blocked')",
                        &[],
                    )?,
                    "open_reports": n(
                        "SELECT COUNT(*) FROM nest_listing_reports WHERE status = 'open'",
                        &[],
                    )?,
                    "publisher_applications": n(
                        "SELECT COUNT(*) FROM publisher_dossiers WHERE state IN ('submitted','in_review')",
                        &[],
                    )?,
                    "hosted_jobs_failed_24h": n(
                        "SELECT COUNT(*) FROM nest_hosted_jobs WHERE state = 'failed' AND updated_at >= ?1",
                        &[now - 86_400],
                    )?,
                    "charges_refused": n(
                        "SELECT COUNT(*) FROM nest_hosted_charges WHERE state = 'refused'",
                        &[],
                    )?,
                }))
            })
            .await?;
        let mut alerts = Vec::new();
        if counts["charges_refused"].as_i64().unwrap_or(0) > 0 {
            alerts.push(json!({ "code": "charges_refused", "count": counts["charges_refused"] }));
        }
        if !state.staff_finance.configured() {
            alerts.push(json!({ "code": "finance_unconfigured" }));
        }
        Ok(Json(json!({ "data": {
            "counts": counts,
            "alerts": alerts,
            "as_of": iso(now),
        }})))
    }
    .await;
    into_response(result)
}

// ---- accounts -------------------------------------------------------------------------------------

/// The realm-proved sign-in address (`provider_identities`, provider `realm`), if any.
const SIGN_IN_SQL: &str = "(SELECT p.login FROM provider_identities p WHERE p.account_id = a.id
      AND p.provider = 'realm' ORDER BY p.verified_at, p.login LIMIT 1)";

/// `GET /v1/staff/accounts?q=` — by account id or OIDC subject (exact or part), sign-in address
/// or provider login, handle, namespace or mailbox address. Newest first.
async fn search_accounts(
    State(state): State<AppState>,
    client_ip: Option<Extension<ClientIp>>,
    headers: HeaderMap,
    Query(q): Query<HashMap<String, String>>,
) -> Response {
    let result = async {
        let who = require_staff(&state, &headers, Action::StaffPeopleRead, "accounts").await?;
        let page = page_of(&q, MAX_LIMIT)?;
        let text_q = query_text(&q)?;
        let row = audit_row(
            &state,
            &who,
            "staff.accounts.search",
            None,
            filters(&[("q", &text_q)]),
            &client_ip,
            &headers,
        );
        let out = audited(&state, row, move |c| {
            let bare = text_q.as_deref().map(|t| t.trim_start_matches('/').to_string());
            let pat = bare.as_deref().map(like);
            rows(
                c,
                &format!(
                    "SELECT a.id, a.created_at, {SIGN_IN_SQL},
                        (SELECT COUNT(*) FROM identities i WHERE i.account_id = a.id),
                        (SELECT COUNT(*) FROM namespaces n WHERE n.account_id = a.id),
                        (SELECT COUNT(*) FROM staff_roles s WHERE s.account_id = a.id AND s.revoked_at IS NULL)
                       FROM accounts a
                      WHERE ?1 IS NULL
                         OR lower(a.id) = ?1 OR lower(a.id) LIKE ?2 ESCAPE '\\' OR lower(a.oidc_sub) = ?1
                         OR EXISTS (SELECT 1 FROM provider_identities p WHERE p.account_id = a.id
                                     AND lower(p.login) LIKE ?2 ESCAPE '\\')
                         OR EXISTS (SELECT 1 FROM identities i WHERE i.account_id = a.id
                                     AND (lower(i.address) LIKE ?2 ESCAPE '\\' OR lower(i.handle) LIKE ?2 ESCAPE '\\'))
                         OR EXISTS (SELECT 1 FROM namespaces n WHERE n.account_id = a.id
                                     AND lower(n.namespace) LIKE ?2 ESCAPE '\\')
                      ORDER BY a.created_at DESC, a.id DESC LIMIT ?3 OFFSET ?4"
                ),
                &[
                    text(&bare),
                    text(&pat),
                    Sql::Integer(page.limit + 1),
                    Sql::Integer(page.offset),
                ],
                |r| {
                    Ok(json!({
                        "id": r.get::<_, String>(0)?,
                        "created_at": iso(r.get(1)?),
                        "sign_in_address": r.get::<_, Option<String>>(2)?,
                        "mailboxes": r.get::<_, i64>(3)?,
                        "handles": r.get::<_, i64>(4)?,
                        "staff": r.get::<_, i64>(5)? > 0,
                    }))
                },
            )
        })
        .await?;
        Ok(Json(paged(out, page)))
    }
    .await;
    into_response(result)
}

/// `GET /v1/staff/accounts/{account}` — sign-in identities, devices (tokens masked), mailboxes,
/// handles, orgs, entitlements and store subscriptions, Nests, staff roles, a pending deletion and
/// the account's recent audit trail. No keys, no message content.
async fn read_account(
    State(state): State<AppState>,
    client_ip: Option<Extension<ClientIp>>,
    headers: HeaderMap,
    Path(account): Path<String>,
) -> Response {
    let result = async {
        let who = require_staff(&state, &headers, Action::StaffPeopleRead, "accounts").await?;
        let row = audit_row(
            &state,
            &who,
            "staff.accounts.read",
            Some(format!("account:{account}")),
            json!({}),
            &client_ip,
            &headers,
        );
        let owner = who.roles.contains(&StaffRole::Owner);
        let out = audited(&state, row, move |c| account_detail(c, &account, owner))
            .await?
            .ok_or_else(not_found)?;
        Ok(Json(json!({ "data": out })))
    }
    .await;
    into_response(result)
}

/// `with_staff`: include other staff members' `staff.*` records about this account. Those are
/// the audit log's, which only `owner` reads (`StaffAuditRead`).
pub(crate) fn account_detail(c: &Connection, id: &str, with_staff: bool) -> GResult<Option<Value>> {
    let base: Option<(String, i64, Option<String>)> = c
        .query_row(
            "SELECT id, created_at, oidc_sub FROM accounts WHERE id = ?1",
            [id],
            |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)),
        )
        .optional()?;
    let Some((id, created_at, sub)) = base else {
        return Ok(None);
    };
    let a = [Sql::Text(id.clone())];
    let sign_ins = rows(
        c,
        "SELECT provider, login, verified_at FROM provider_identities WHERE account_id = ?1
          ORDER BY verified_at, provider, login",
        &a,
        |r| {
            Ok(json!({
                "provider": r.get::<_, String>(0)?,
                "login": r.get::<_, String>(1)?,
                "verified_at": iso(r.get(2)?),
            }))
        },
    )?;
    let devices = rows(
        c,
        "SELECT token, mailbox, platform, environment, kind, os, updated_at FROM devices
          WHERE account = ?1 OR mailbox IN (SELECT address FROM identities WHERE account_id = ?1)
          ORDER BY updated_at DESC LIMIT 100",
        &a,
        |r| {
            Ok(json!({
                "push_token": mask(&r.get::<_, String>(0)?, 6),
                "mailbox": r.get::<_, String>(1)?,
                "platform": r.get::<_, String>(2)?,
                "environment": r.get::<_, String>(3)?,
                "kind": r.get::<_, String>(4)?,
                "os": r.get::<_, Option<String>>(5)?,
                "updated_at": iso(r.get(6)?),
            }))
        },
    )?;
    let mailboxes = rows(
        c,
        &format!(
            "SELECT i.address, i.handle, i.label, i.created_at, {MAILBOX_KIND}, oa.org_id
               FROM identities i
               LEFT JOIN org_agents oa ON oa.identity = i.address AND oa.status <> 'deleted'
               LEFT JOIN nests n ON n.address = i.address AND n.deleted_at IS NULL
              WHERE i.account_id = ?1 ORDER BY i.created_at DESC LIMIT 200"
        ),
        &a,
        |r| {
            Ok(json!({
                "address": r.get::<_, String>(0)?,
                "handle": r.get::<_, Option<String>>(1)?,
                "label": r.get::<_, Option<String>>(2)?,
                "created_at": iso(r.get(3)?),
                "kind": r.get::<_, String>(4)?,
                "org_id": r.get::<_, Option<String>>(5)?,
            }))
        },
    )?;
    let handles = rows(
        c,
        "SELECT namespace, source, verified_at, expires_at, release_at FROM namespaces
          WHERE account_id = ?1 ORDER BY namespace",
        &a,
        namespace_json,
    )?;
    let orgs = rows(
        c,
        "SELECT o.id, o.namespace, o.name, o.status,
                CASE WHEN o.owner_account = ?1 THEN 'owner' ELSE 'member' END,
                (SELECT m.status FROM org_members m WHERE m.org_id = o.id AND m.account_id = ?1
                  ORDER BY m.joined_at DESC LIMIT 1)
           FROM orgs o
          WHERE o.owner_account = ?1
             OR o.id IN (SELECT org_id FROM org_members WHERE account_id = ?1)
          ORDER BY o.created_at",
        &a,
        |r| {
            Ok(json!({
                "id": r.get::<_, String>(0)?,
                "namespace": r.get::<_, String>(1)?,
                "name": r.get::<_, String>(2)?,
                "status": r.get::<_, String>(3)?,
                "relation": r.get::<_, String>(4)?,
                "member_status": r.get::<_, Option<String>>(5)?,
            }))
        },
    )?;
    let entitlements = rows(
        c,
        &format!("SELECT {ENT_COLS} FROM entitlements WHERE subject_kind = 'account' AND subject_id = ?1 ORDER BY updated_at DESC"),
        &a,
        entitlement_json,
    )?;
    let apple = rows(
        c,
        &format!("SELECT {APPLE_COLS} FROM apple_subscriptions WHERE account_id = ?1 ORDER BY expires_at DESC"),
        &a,
        apple_json,
    )?;
    let google = rows(
        c,
        &format!("SELECT {GOOGLE_COLS} FROM google_subscriptions WHERE account_id = ?1 ORDER BY expires_at DESC"),
        &a,
        google_json,
    )?;
    let nest_store = rows(
        c,
        &format!("SELECT {NEST_STORE_COLS} FROM nest_store_purchases WHERE account_id = ?1 ORDER BY expires_at DESC"),
        &a,
        nest_store_json,
    )?;
    let nests = rows(
        c,
        "SELECT id, address, label, state, funding, created_at, deleted_at FROM nests
          WHERE account_id = ?1 ORDER BY created_at DESC",
        &a,
        |r| {
            Ok(json!({
                "id": r.get::<_, String>(0)?,
                "address": r.get::<_, Option<String>>(1)?,
                "label": r.get::<_, String>(2)?,
                "state": r.get::<_, String>(3)?,
                "funding": r.get::<_, String>(4)?,
                "created_at": iso(r.get(5)?),
                "deleted_at": when(r.get(6)?),
            }))
        },
    )?;
    let staff_roles = rows(
        c,
        "SELECT id, role, granted_by, granted_at, revoked_at FROM staff_roles WHERE account_id = ?1
          ORDER BY granted_at",
        &a,
        |r| {
            Ok(json!({
                "id": r.get::<_, String>(0)?,
                "role": r.get::<_, String>(1)?,
                "granted_by": r.get::<_, String>(2)?,
                "granted_at": iso(r.get(3)?),
                "revoked_at": when(r.get(4)?),
            }))
        },
    )?;
    let deletion = c
        .query_row(
            "SELECT request_id, requested_at, complete_by, completed_at FROM account_deletion_requests
              WHERE account_id = ?1",
            [&id],
            |r| {
                Ok(json!({
                    "request_id": r.get::<_, String>(0)?,
                    "requested_at": iso(r.get(1)?),
                    "complete_by": iso(r.get(2)?),
                    "completed_at": when(r.get(3)?),
                }))
            },
        )
        .optional()?;
    let recent_audit = rows(
        c,
        &format!(
            "SELECT {AUDIT_COLS} FROM audit_log
              WHERE target = 'account:' || ?1 AND (?2 OR action NOT LIKE 'staff.%')
              ORDER BY id DESC LIMIT 20"
        ),
        &[Sql::Text(id.clone()), Sql::Integer(with_staff as i64)],
        audit_json,
    )?;
    Ok(Some(json!({
        "id": id,
        "created_at": iso(created_at),
        "oidc_subject": sub,
        "sign_in_identities": sign_ins,
        "devices": devices,
        "mailboxes": mailboxes,
        "handles": handles,
        "orgs": orgs,
        "entitlements": entitlements,
        "subscriptions": { "apple": apple, "google": google, "nest_store": nest_store },
        "nests": nests,
        "staff_roles": staff_roles,
        "deletion": deletion,
        "recent_audit": recent_audit,
    })))
}

// ---- handles --------------------------------------------------------------------------------------

fn namespace_json(r: &rusqlite::Row<'_>) -> rusqlite::Result<Value> {
    Ok(json!({
        "namespace": r.get::<_, String>(0)?,
        "source": r.get::<_, String>(1)?,
        "verified_at": iso(r.get(2)?),
        "expires_at": when(r.get(3)?),
        "release_at": when(r.get(4)?),
    }))
}

/// `GET /v1/staff/handles?q=` — every namespace and who holds it. `q` matches the namespace or
/// the holder's account id.
async fn search_handles(
    State(state): State<AppState>,
    client_ip: Option<Extension<ClientIp>>,
    headers: HeaderMap,
    Query(q): Query<HashMap<String, String>>,
) -> Response {
    let result = async {
        let who = require_staff(&state, &headers, Action::StaffPeopleRead, "handles").await?;
        let page = page_of(&q, MAX_LIMIT)?;
        let text_q = query_text(&q)?;
        let source = choice(
            &q,
            "source",
            &[
                "apple",
                "google",
                "entitlement",
                "grant",
                "test_preview",
                "github",
                "realm",
            ],
        )?;
        let row = audit_row(
            &state,
            &who,
            "staff.handles.search",
            None,
            filters(&[("q", &text_q), ("source", &source)]),
            &client_ip,
            &headers,
        );
        let out = audited(&state, row, move |c| {
            let bare = text_q.as_deref().map(|t| t.trim_start_matches('/').to_string());
            let pat = bare.as_deref().map(like);
            rows(
                c,
                "SELECT n.namespace, n.source, n.verified_at, n.expires_at, n.release_at, n.account_id,
                        (SELECT COUNT(*) FROM identities i
                          WHERE i.handle = '/' || n.namespace
                             OR substr(i.handle, 1, length(n.namespace) + 2) = '/' || n.namespace || '/')
                   FROM namespaces n
                  WHERE (?1 IS NULL OR lower(n.namespace) LIKE ?2 ESCAPE '\\' OR lower(n.account_id) = ?1)
                    AND (?3 IS NULL OR n.source = ?3)
                  ORDER BY n.namespace LIMIT ?4 OFFSET ?5",
                &[
                    text(&bare),
                    text(&pat),
                    text(&source),
                    Sql::Integer(page.limit + 1),
                    Sql::Integer(page.offset),
                ],
                |r| {
                    let mut v = namespace_json(r)?;
                    v["account_id"] = json!(r.get::<_, String>(5)?);
                    v["mailboxes"] = json!(r.get::<_, i64>(6)?);
                    Ok(v)
                },
            )
        })
        .await?;
        Ok(Json(paged(out, page)))
    }
    .await;
    into_response(result)
}

/// `GET /v1/staff/handles/{namespace}` — the holder, the mailboxes under it, the org that uses it
/// and its handle holds, store subscriptions naming it, and its audit history (grants, releases).
async fn read_handle(
    State(state): State<AppState>,
    client_ip: Option<Extension<ClientIp>>,
    headers: HeaderMap,
    Path(namespace): Path<String>,
) -> Response {
    let result = async {
        let who = require_staff(&state, &headers, Action::StaffPeopleRead, "handles").await?;
        let ns = namespace.trim_start_matches('/').to_lowercase();
        let owner = who.roles.contains(&StaffRole::Owner);
        let row = audit_row(
            &state,
            &who,
            "staff.handles.read",
            Some(format!("namespace:{ns}")),
            json!({}),
            &client_ip,
            &headers,
        );
        let out = audited(&state, row, move |c| -> GResult<Option<Value>> {
            let holder = c
                .query_row(
                    "SELECT namespace, source, verified_at, expires_at, release_at, account_id
                       FROM namespaces WHERE namespace = ?1",
                    [&ns],
                    |r| {
                        let mut v = namespace_json(r)?;
                        v["account_id"] = json!(r.get::<_, String>(5)?);
                        Ok(v)
                    },
                )
                .optional()?;
            let n = [Sql::Text(ns.clone())];
            let mailboxes = rows(
                c,
                "SELECT address, handle, label, account_id, created_at FROM identities
                  WHERE handle = '/' || ?1 OR substr(handle, 1, length(?1) + 2) = '/' || ?1 || '/'
                  ORDER BY handle LIMIT 200",
                &n,
                |r| {
                    Ok(json!({
                        "address": r.get::<_, String>(0)?,
                        "handle": r.get::<_, Option<String>>(1)?,
                        "label": r.get::<_, Option<String>>(2)?,
                        "account_id": r.get::<_, Option<String>>(3)?,
                        "created_at": iso(r.get(4)?),
                    }))
                },
            )?;
            let orgs = rows(
                c,
                "SELECT id, name, status FROM orgs WHERE namespace = ?1",
                &n,
                |r| {
                    Ok(json!({
                        "id": r.get::<_, String>(0)?,
                        "name": r.get::<_, String>(1)?,
                        "status": r.get::<_, String>(2)?,
                    }))
                },
            )?;
            let holds = rows(
                c,
                "SELECT h.org_id, h.handle, h.reason, h.created_at FROM org_handle_holds h
                  WHERE h.handle = '/' || ?1 OR substr(h.handle, 1, length(?1) + 2) = '/' || ?1 || '/'
                     OR h.org_id IN (SELECT id FROM orgs WHERE namespace = ?1)
                  ORDER BY h.created_at DESC LIMIT 200",
                &n,
                |r| {
                    Ok(json!({
                        "org_id": r.get::<_, String>(0)?,
                        "handle": r.get::<_, String>(1)?,
                        "reason": r.get::<_, String>(2)?,
                        "created_at": iso(r.get(3)?),
                    }))
                },
            )?;
            let apple = rows(
                c,
                &format!("SELECT {APPLE_COLS} FROM apple_subscriptions WHERE namespace = ?1 ORDER BY expires_at DESC"),
                &n,
                apple_json,
            )?;
            let google = rows(
                c,
                &format!("SELECT {GOOGLE_COLS} FROM google_subscriptions WHERE namespace = ?1 ORDER BY expires_at DESC"),
                &n,
                google_json,
            )?;
            let history = rows(
                c,
                &format!(
                    "SELECT {AUDIT_COLS} FROM audit_log WHERE target = 'namespace:' || ?1
                        AND (?2 OR action NOT LIKE 'staff.%')
                      ORDER BY id DESC LIMIT 50"
                ),
                &[Sql::Text(ns.clone()), Sql::Integer(owner as i64)],
                audit_json,
            )?;
            if holder.is_none() && mailboxes.is_empty() && orgs.is_empty() && history.is_empty() {
                return Ok(None);
            }
            Ok(Some(json!({
                "namespace": ns,
                "holder": holder,
                "mailboxes": mailboxes,
                "orgs": orgs,
                "holds": holds,
                "subscriptions": { "apple": apple, "google": google },
                "history": history,
            })))
        })
        .await?
        .ok_or_else(not_found)?;
        Ok(Json(json!({ "data": out })))
    }
    .await;
    into_response(result)
}

// ---- mailboxes ------------------------------------------------------------------------------------

/// What a mailbox is, from what else refers to it. Needs `oa` (org_agents) and `n` (nests).
const MAILBOX_KIND: &str = "CASE WHEN oa.identity IS NOT NULL THEN 'org_agent'
      WHEN n.id IS NOT NULL THEN 'nest'
      WHEN i.account_id IS NULL THEN 'anonymous'
      ELSE 'account' END";

/// `GET /v1/staff/mailboxes?q=&kind=` — mailboxes with their account, handle, kind and the last
/// time one of their devices checked in. `q` matches address, handle, label or account id.
async fn search_mailboxes(
    State(state): State<AppState>,
    client_ip: Option<Extension<ClientIp>>,
    headers: HeaderMap,
    Query(q): Query<HashMap<String, String>>,
) -> Response {
    let result = async {
        let who = require_staff(&state, &headers, Action::StaffPeopleRead, "mailboxes").await?;
        let page = page_of(&q, MAX_LIMIT)?;
        let text_q = query_text(&q)?;
        let kind = choice(&q, "kind", &["account", "org_agent", "nest", "anonymous"])?;
        let row = audit_row(
            &state,
            &who,
            "staff.mailboxes.search",
            None,
            filters(&[("q", &text_q), ("kind", &kind)]),
            &client_ip,
            &headers,
        );
        let out = audited(&state, row, move |c| {
            let pat = text_q.as_deref().map(like);
            rows(
                c,
                &format!(
                    "SELECT * FROM (
                       SELECT i.address, i.handle, i.label, i.account_id, i.created_at,
                              {MAILBOX_KIND} AS kind, oa.org_id,
                              (SELECT MAX(d.updated_at) FROM devices d WHERE d.mailbox = i.address)
                         FROM identities i
                         LEFT JOIN org_agents oa ON oa.identity = i.address AND oa.status <> 'deleted'
                         LEFT JOIN nests n ON n.address = i.address AND n.deleted_at IS NULL
                        WHERE ?1 IS NULL OR lower(i.address) LIKE ?1 ESCAPE '\\'
                           OR lower(i.handle) LIKE ?1 ESCAPE '\\' OR lower(i.label) LIKE ?1 ESCAPE '\\'
                           OR lower(i.account_id) LIKE ?1 ESCAPE '\\')
                     WHERE ?2 IS NULL OR kind = ?2
                     ORDER BY created_at DESC, address LIMIT ?3 OFFSET ?4"
                ),
                &[
                    text(&pat),
                    text(&kind),
                    Sql::Integer(page.limit + 1),
                    Sql::Integer(page.offset),
                ],
                |r| {
                    Ok(json!({
                        "address": r.get::<_, String>(0)?,
                        "handle": r.get::<_, Option<String>>(1)?,
                        "label": r.get::<_, Option<String>>(2)?,
                        "account_id": r.get::<_, Option<String>>(3)?,
                        "created_at": iso(r.get(4)?),
                        "kind": r.get::<_, String>(5)?,
                        "org_id": r.get::<_, Option<String>>(6)?,
                        "last_activity": when(r.get(7)?),
                    }))
                },
            )
        })
        .await?;
        Ok(Json(paged(out, page)))
    }
    .await;
    into_response(result)
}

// ---- organizations --------------------------------------------------------------------------------

/// `GET /v1/staff/orgs?q=` — by id, namespace, name or owner account.
async fn search_orgs(
    State(state): State<AppState>,
    client_ip: Option<Extension<ClientIp>>,
    headers: HeaderMap,
    Query(q): Query<HashMap<String, String>>,
) -> Response {
    let result = async {
        let who = require_staff(&state, &headers, Action::StaffPeopleRead, "orgs").await?;
        let page = page_of(&q, MAX_LIMIT)?;
        let text_q = query_text(&q)?;
        let status = choice(
            &q,
            "status",
            &[
                "pending",
                "active",
                "past_due",
                "read_only",
                "suspended",
                "deleted",
                "paid_unprovisioned",
            ],
        )?;
        let row = audit_row(
            &state,
            &who,
            "staff.orgs.search",
            None,
            filters(&[("q", &text_q), ("status", &status)]),
            &client_ip,
            &headers,
        );
        let out = audited(&state, row, move |c| {
            let pat = text_q.as_deref().map(like);
            rows(
                c,
                "SELECT o.id, o.namespace, o.name, o.owner_account, o.status, o.seats, o.plan_code,
                        o.created_at,
                        (SELECT COUNT(*) FROM org_members m WHERE m.org_id = o.id AND m.status = 'active'),
                        (SELECT COUNT(*) FROM org_agents g WHERE g.org_id = o.id AND g.status <> 'deleted')
                   FROM orgs o
                  WHERE (?1 IS NULL OR lower(o.id) LIKE ?1 ESCAPE '\\' OR lower(o.namespace) LIKE ?1 ESCAPE '\\'
                         OR lower(o.name) LIKE ?1 ESCAPE '\\' OR lower(o.owner_account) LIKE ?1 ESCAPE '\\')
                    AND (?2 IS NULL OR o.status = ?2)
                  ORDER BY o.created_at DESC, o.id LIMIT ?3 OFFSET ?4",
                &[
                    text(&pat),
                    text(&status),
                    Sql::Integer(page.limit + 1),
                    Sql::Integer(page.offset),
                ],
                |r| {
                    Ok(json!({
                        "id": r.get::<_, String>(0)?,
                        "namespace": r.get::<_, String>(1)?,
                        "name": r.get::<_, String>(2)?,
                        "owner_account": r.get::<_, String>(3)?,
                        "status": r.get::<_, String>(4)?,
                        "seats": r.get::<_, i64>(5)?,
                        "plan_code": r.get::<_, Option<String>>(6)?,
                        "created_at": iso(r.get(7)?),
                        "active_members": r.get::<_, i64>(8)?,
                        "agents": r.get::<_, i64>(9)?,
                    }))
                },
            )
        })
        .await?;
        Ok(Json(paged(out, page)))
    }
    .await;
    into_response(result)
}

/// `GET /v1/staff/orgs/{org}` — members and their roles (contact masked as the org stores it),
/// domains (never the verification token), agents, API keys (last four characters only),
/// entitlements and handle holds.
async fn read_org(
    State(state): State<AppState>,
    client_ip: Option<Extension<ClientIp>>,
    headers: HeaderMap,
    Path(org): Path<String>,
) -> Response {
    let result = async {
        let who = require_staff(&state, &headers, Action::StaffPeopleRead, "orgs").await?;
        let row = audit_row(
            &state,
            &who,
            "staff.orgs.read",
            Some(format!("org:{org}")),
            json!({}),
            &client_ip,
            &headers,
        );
        let out = audited(&state, row, move |c| org_detail(c, &org))
            .await?
            .ok_or_else(not_found)?;
        Ok(Json(json!({ "data": out })))
    }
    .await;
    into_response(result)
}

fn org_detail(c: &Connection, id: &str) -> GResult<Option<Value>> {
    let base = c
        .query_row(
            "SELECT id, namespace, name, owner_account, internal_account_id, status, seats,
                    agent_limit, storage_bytes, plan_code, period_end, created_at, deleted_at
               FROM orgs WHERE id = ?1",
            [id],
            |r| {
                Ok(json!({
                    "id": r.get::<_, String>(0)?,
                    "namespace": r.get::<_, String>(1)?,
                    "name": r.get::<_, String>(2)?,
                    "owner_account": r.get::<_, String>(3)?,
                    "internal_account_id": r.get::<_, Option<String>>(4)?,
                    "status": r.get::<_, String>(5)?,
                    "seats": r.get::<_, i64>(6)?,
                    "agent_limit": r.get::<_, i64>(7)?,
                    "storage_bytes": r.get::<_, i64>(8)?,
                    "plan_code": r.get::<_, Option<String>>(9)?,
                    "period_end": when(r.get(10)?),
                    "created_at": iso(r.get(11)?),
                    "deleted_at": when(r.get(12)?),
                }))
            },
        )
        .optional()?;
    let Some(mut out) = base else {
        return Ok(None);
    };
    let o = [Sql::Text(id.to_string())];
    out["members"] = json!(rows(
        c,
        "SELECT m.id, m.account_id, m.handle, m.first_name, m.last_name, m.contact_masked, m.status,
                m.joined_at, m.last_active_at,
                (SELECT group_concat(role, ',') FROM org_roles r WHERE r.org_id = m.org_id AND r.member_id = m.id)
           FROM org_members m WHERE m.org_id = ?1 ORDER BY m.status, m.last_name, m.first_name LIMIT 500",
        &o,
        |r| {
            let roles: Option<String> = r.get(9)?;
            let name = format!("{} {}", r.get::<_, String>(3)?, r.get::<_, String>(4)?)
                .trim()
                .to_string();
            Ok(json!({
                "id": r.get::<_, String>(0)?,
                "account_id": r.get::<_, Option<String>>(1)?,
                "handle": r.get::<_, Option<String>>(2)?,
                "name": name,
                "contact": r.get::<_, Option<String>>(5)?,
                "status": r.get::<_, String>(6)?,
                "joined_at": when(r.get(7)?),
                "last_active_at": when(r.get(8)?),
                "roles": roles.map(|s| s.split(',').map(str::to_string).collect::<Vec<_>>()).unwrap_or_default(),
            }))
        },
    )?);
    out["domains"] = json!(rows(
        c,
        "SELECT domain, verified_at, last_checked_at, failures, created_at FROM org_domains
          WHERE org_id = ?1 ORDER BY domain",
        &o,
        |r| {
            Ok(json!({
                "domain": r.get::<_, String>(0)?,
                "verified_at": when(r.get(1)?),
                "last_checked_at": when(r.get(2)?),
                "failures": r.get::<_, i64>(3)?,
                "created_at": iso(r.get(4)?),
            }))
        },
    )?);
    out["agents"] = json!(rows(
        c,
        "SELECT identity, handle, label, status, created_at, last_seen_at FROM org_agents
          WHERE org_id = ?1 ORDER BY created_at DESC LIMIT 500",
        &o,
        |r| {
            Ok(json!({
                "address": r.get::<_, String>(0)?,
                "handle": r.get::<_, String>(1)?,
                "label": r.get::<_, Option<String>>(2)?,
                "status": r.get::<_, String>(3)?,
                "created_at": iso(r.get(4)?),
                "last_seen_at": when(r.get(5)?),
            }))
        },
    )?);
    out["api_keys"] = json!(rows(
        c,
        "SELECT id, name, last4, scopes, created_by_account, created_at, expires_at, last_used_at, revoked_at
           FROM org_api_keys WHERE org_id = ?1 ORDER BY created_at DESC LIMIT 200",
        &o,
        |r| {
            let scopes: String = r.get(3)?;
            Ok(json!({
                "id": r.get::<_, String>(0)?,
                "name": r.get::<_, String>(1)?,
                "masked": format!("pk_org_••••{}", r.get::<_, String>(2)?),
                "scopes": serde_json::from_str::<Value>(&scopes).unwrap_or(json!(scopes)),
                "created_by": r.get::<_, String>(4)?,
                "created_at": iso(r.get(5)?),
                "expires_at": when(r.get(6)?),
                "last_used_at": when(r.get(7)?),
                "revoked_at": when(r.get(8)?),
            }))
        },
    )?);
    out["entitlements"] = json!(rows(
        c,
        &format!("SELECT {ENT_COLS} FROM entitlements WHERE subject_kind = 'org' AND subject_id = ?1 ORDER BY updated_at DESC"),
        &o,
        entitlement_json,
    )?);
    out["handle_holds"] = json!(rows(
        c,
        "SELECT handle, reason, created_at FROM org_handle_holds WHERE org_id = ?1 ORDER BY created_at DESC",
        &o,
        |r| {
            Ok(json!({
                "handle": r.get::<_, String>(0)?,
                "reason": r.get::<_, String>(1)?,
                "created_at": iso(r.get(2)?),
            }))
        },
    )?);
    Ok(Some(out))
}

// ---- plans ----------------------------------------------------------------------------------------

const ENT_COLS: &str = "id, subject_kind, subject_id, source, plan_code, status, names, seats,
    storage_bytes, period_end, paid_through, provider_ref, provider_environment, cancel_requested_at,
    pending_plan_code, updated_at";

fn entitlement_json(r: &rusqlite::Row<'_>) -> rusqlite::Result<Value> {
    Ok(json!({
        "id": r.get::<_, String>(0)?,
        "subject_kind": r.get::<_, String>(1)?,
        "subject_id": r.get::<_, String>(2)?,
        "source": r.get::<_, String>(3)?,
        "plan_code": r.get::<_, String>(4)?,
        "status": r.get::<_, String>(5)?,
        "names": r.get::<_, Option<i64>>(6)?,
        "seats": r.get::<_, Option<i64>>(7)?,
        "storage_bytes": r.get::<_, Option<i64>>(8)?,
        "period_end": when(r.get(9)?),
        "paid_through": when(r.get(10)?),
        "provider_ref": mask(&r.get::<_, String>(11)?, 4),
        "environment": r.get::<_, String>(12)?,
        "cancel_requested_at": when(r.get(13)?),
        "pending_plan_code": r.get::<_, Option<String>>(14)?,
        "updated_at": iso(r.get(15)?),
    }))
}

const APPLE_COLS: &str =
    "original_transaction_id, account_id, namespace, environment, product_id, expires_at, updated_at";

fn apple_json(r: &rusqlite::Row<'_>) -> rusqlite::Result<Value> {
    Ok(json!({
        "store": "apple",
        "reference": mask(&r.get::<_, String>(0)?, 4),
        "account_id": r.get::<_, String>(1)?,
        "namespace": r.get::<_, String>(2)?,
        "environment": r.get::<_, String>(3)?,
        "product_id": r.get::<_, String>(4)?,
        "expires_at": iso(r.get(5)?),
        "updated_at": iso(r.get(6)?),
    }))
}

const GOOGLE_COLS: &str =
    "purchase_token, account_id, product_id, namespace, expires_at, state, auto_renewing, test_purchase, verified_at";

fn google_json(r: &rusqlite::Row<'_>) -> rusqlite::Result<Value> {
    Ok(json!({
        "store": "google",
        "reference": mask(&r.get::<_, String>(0)?, 4),
        "account_id": r.get::<_, String>(1)?,
        "product_id": r.get::<_, String>(2)?,
        "namespace": r.get::<_, Option<String>>(3)?,
        "expires_at": iso(r.get(4)?),
        "state": r.get::<_, String>(5)?,
        "auto_renewing": r.get::<_, i64>(6)? != 0,
        "test_purchase": r.get::<_, i64>(7)? != 0,
        "updated_at": iso(r.get(8)?),
    }))
}

const NEST_STORE_COLS: &str =
    "source, environment, reference, account_id, product_id, package, expires_at, state, updated_at";

fn nest_store_json(r: &rusqlite::Row<'_>) -> rusqlite::Result<Value> {
    Ok(json!({
        "store": r.get::<_, String>(0)?,
        "environment": r.get::<_, String>(1)?,
        "reference": mask(&r.get::<_, String>(2)?, 4),
        "account_id": r.get::<_, String>(3)?,
        "product_id": r.get::<_, String>(4)?,
        "package": r.get::<_, String>(5)?,
        "expires_at": iso(r.get(6)?),
        "state": r.get::<_, String>(7)?,
        "updated_at": iso(r.get(8)?),
    }))
}

/// `GET /v1/staff/entitlements?q=&source=&status=` — account and org entitlements from every
/// source (Apple, Google, iyzico web subscriptions, grants, test). `q` matches the subject or the
/// plan code.
async fn search_entitlements(
    State(state): State<AppState>,
    client_ip: Option<Extension<ClientIp>>,
    headers: HeaderMap,
    Query(q): Query<HashMap<String, String>>,
) -> Response {
    let result = async {
        let who = require_staff(&state, &headers, Action::StaffPlansRead, "entitlements").await?;
        let page = page_of(&q, MAX_LIMIT)?;
        let text_q = query_text(&q)?;
        let source = choice(&q, "source", &["apple", "google", "iyzico", "grant", "test"])?;
        let status = choice(
            &q,
            "status",
            &["active", "past_due", "grace", "expired", "canceled"],
        )?;
        let row = audit_row(
            &state,
            &who,
            "staff.plans.search",
            None,
            filters(&[("q", &text_q), ("source", &source), ("status", &status)]),
            &client_ip,
            &headers,
        );
        let out = audited(&state, row, move |c| {
            let pat = text_q.as_deref().map(like);
            rows(
                c,
                &format!(
                    "SELECT {ENT_COLS} FROM entitlements
                      WHERE (?1 IS NULL OR lower(subject_id) LIKE ?1 ESCAPE '\\' OR lower(plan_code) LIKE ?1 ESCAPE '\\')
                        AND (?2 IS NULL OR source = ?2) AND (?3 IS NULL OR status = ?3)
                      ORDER BY updated_at DESC, id LIMIT ?4 OFFSET ?5"
                ),
                &[
                    text(&pat),
                    text(&source),
                    text(&status),
                    Sql::Integer(page.limit + 1),
                    Sql::Integer(page.offset),
                ],
                entitlement_json,
            )
        })
        .await?;
        Ok(Json(paged(out, page)))
    }
    .await;
    into_response(result)
}

/// `GET /v1/staff/subscriptions?store=apple|google|nest&q=` — the store tables behind the
/// entitlements: App Store and Google Play handle subscriptions, and Nest store purchases.
/// References are masked. `q` matches the account, the namespace or the product.
async fn search_subscriptions(
    State(state): State<AppState>,
    client_ip: Option<Extension<ClientIp>>,
    headers: HeaderMap,
    Query(q): Query<HashMap<String, String>>,
) -> Response {
    let result = async {
        let who = require_staff(&state, &headers, Action::StaffPlansRead, "subscriptions").await?;
        let page = page_of(&q, MAX_LIMIT)?;
        let text_q = query_text(&q)?;
        let store = choice(&q, "store", &["apple", "google", "nest"])?
            .unwrap_or_else(|| "apple".into());
        let row = audit_row(
            &state,
            &who,
            "staff.plans.subscriptions",
            None,
            filters(&[("q", &text_q), ("store", &Some(store.clone()))]),
            &client_ip,
            &headers,
        );
        let out = audited(&state, row, move |c| {
            let pat = text_q.as_deref().map(like);
            let args = [
                text(&pat),
                Sql::Integer(page.limit + 1),
                Sql::Integer(page.offset),
            ];
            match store.as_str() {
                "apple" => rows(
                    c,
                    &format!(
                        "SELECT {APPLE_COLS} FROM apple_subscriptions
                          WHERE ?1 IS NULL OR lower(account_id) LIKE ?1 ESCAPE '\\'
                             OR lower(namespace) LIKE ?1 ESCAPE '\\' OR lower(product_id) LIKE ?1 ESCAPE '\\'
                          ORDER BY expires_at DESC, original_transaction_id LIMIT ?2 OFFSET ?3"
                    ),
                    &args,
                    apple_json,
                ),
                "google" => rows(
                    c,
                    &format!(
                        "SELECT {GOOGLE_COLS} FROM google_subscriptions
                          WHERE ?1 IS NULL OR lower(account_id) LIKE ?1 ESCAPE '\\'
                             OR lower(namespace) LIKE ?1 ESCAPE '\\' OR lower(product_id) LIKE ?1 ESCAPE '\\'
                          ORDER BY expires_at DESC, purchase_token LIMIT ?2 OFFSET ?3"
                    ),
                    &args,
                    google_json,
                ),
                _ => rows(
                    c,
                    &format!(
                        "SELECT {NEST_STORE_COLS} FROM nest_store_purchases
                          WHERE ?1 IS NULL OR lower(account_id) LIKE ?1 ESCAPE '\\'
                             OR lower(package) LIKE ?1 ESCAPE '\\' OR lower(product_id) LIKE ?1 ESCAPE '\\'
                          ORDER BY expires_at DESC, reference LIMIT ?2 OFFSET ?3"
                    ),
                    &args,
                    nest_store_json,
                ),
            }
        })
        .await?;
        Ok(Json(paged(out, page)))
    }
    .await;
    into_response(result)
}

// ---- the platform audit log -----------------------------------------------------------------------

const AUDIT_COLS: &str = "id, at, actor_account, actor_kind, action, target, org_id, details";

fn audit_json(r: &rusqlite::Row<'_>) -> rusqlite::Result<Value> {
    let details: Option<String> = r.get(7)?;
    Ok(json!({
        "id": r.get::<_, i64>(0)?,
        "at": iso(r.get(1)?),
        "actor_account": r.get::<_, Option<String>>(2)?,
        "actor_kind": r.get::<_, String>(3)?,
        "action": r.get::<_, String>(4)?,
        "target": r.get::<_, Option<String>>(5)?,
        "org_id": r.get::<_, Option<String>>(6)?,
        "details": details
            .as_deref()
            .and_then(|d| serde_json::from_str::<Value>(d).ok())
            .unwrap_or(Value::Null),
    }))
}

/// A time filter: unix seconds or an RFC 3339 instant or a `YYYY-MM-DD` date (UTC midnight).
fn instant(q: &HashMap<String, String>, key: &'static str) -> GResult<Option<i64>> {
    let Some(v) = q.get(key).map(|s| s.trim()).filter(|s| !s.is_empty()) else {
        return Ok(None);
    };
    if let Ok(n) = v.parse::<i64>() {
        return Ok(Some(n));
    }
    if let Ok(t) = chrono::DateTime::parse_from_rfc3339(v) {
        return Ok(Some(t.timestamp()));
    }
    if let Ok(d) = chrono::NaiveDate::parse_from_str(v, "%Y-%m-%d") {
        if let Some(t) = d.and_hms_opt(0, 0, 0) {
            return Ok(Some(t.and_utc().timestamp()));
        }
    }
    Err(GErr::bad(
        "invalid_request",
        format!("{key}: unix seconds, an RFC 3339 time or a YYYY-MM-DD date"),
    ))
}

/// `GET /v1/staff/audit?actor=&action=&target=&from=&to=&cursor=&limit=` — the whole log, newest
/// first. `actor` is an account id (exact); `action` an exact action or a prefix ending in `.`
/// (`staff.` is every back office record); `target` exact, or a prefix ending in `:`. `to` is
/// inclusive. Paged by row id; `export=1` allows up to 500 rows and is recorded as an export.
async fn search_audit(
    State(state): State<AppState>,
    client_ip: Option<Extension<ClientIp>>,
    headers: HeaderMap,
    Query(q): Query<HashMap<String, String>>,
) -> Response {
    let result = async {
        let who = require_staff(&state, &headers, Action::StaffAuditRead, "audit").await?;
        let export = q.get("export").is_some_and(|v| v == "1" || v == "true");
        let limit = page_of(
            &HashMap::from_iter(q.get("limit").map(|l| ("limit".to_string(), l.clone()))),
            if export { 500 } else { MAX_LIMIT },
        )?
        .limit;
        let before = match q.get("cursor").map(String::as_str) {
            None | Some("") => None,
            Some(c) => Some(
                c.strip_prefix("a")
                    .and_then(|n| n.parse::<i64>().ok())
                    .filter(|n| *n > 0)
                    .ok_or_else(|| GErr::bad("invalid_cursor", "this cursor is not one we issued"))?,
            ),
        };
        let field = |k: &str| -> GResult<Option<String>> {
            match q.get(k).map(|s| s.trim()) {
                None | Some("") => Ok(None),
                Some(v) if v.chars().count() <= 200 => Ok(Some(v.to_string())),
                Some(_) => Err(GErr::bad("invalid_request", format!("{k}: at most 200 characters"))),
            }
        };
        let actor = field("actor")?;
        let action = field("action")?;
        let target = field("target")?;
        let from = instant(&q, "from")?;
        let to = instant(&q, "to")?;
        let row = audit_row(
            &state,
            &who,
            if export { "staff.audit.export" } else { "staff.audit.search" },
            None,
            json!({
                "actor": actor, "action": action, "target": target, "from": from, "to": to,
            }),
            &client_ip,
            &headers,
        );
        let prefix = |v: &Option<String>, end: char| -> (Sql, Sql) {
            match v {
                Some(s) if s.ends_with(end) => (Sql::Null, Sql::Text(like(s).trim_start_matches('%').to_string())),
                Some(s) => (Sql::Text(s.clone()), Sql::Null),
                None => (Sql::Null, Sql::Null),
            }
        };
        let (action_eq, action_like) = prefix(&action, '.');
        let (target_eq, target_like) = prefix(&target, ':');
        let mut out = audited(&state, row, move |c| {
            rows(
                c,
                &format!(
                    "SELECT {AUDIT_COLS} FROM audit_log
                      WHERE (?1 IS NULL OR id < ?1)
                        AND (?2 IS NULL OR actor_account = ?2)
                        AND (?3 IS NULL OR action = ?3) AND (?4 IS NULL OR action LIKE ?4 ESCAPE '\\')
                        AND (?5 IS NULL OR target = ?5) AND (?6 IS NULL OR target LIKE ?6 ESCAPE '\\')
                        AND (?7 IS NULL OR at >= ?7) AND (?8 IS NULL OR at <= ?8)
                      ORDER BY id DESC LIMIT ?9"
                ),
                &[
                    before.map(Sql::Integer).unwrap_or(Sql::Null),
                    text(&actor),
                    action_eq,
                    action_like,
                    target_eq,
                    target_like,
                    from.map(Sql::Integer).unwrap_or(Sql::Null),
                    to.map(Sql::Integer).unwrap_or(Sql::Null),
                    Sql::Integer(limit + 1),
                ],
                audit_json,
            )
        })
        .await?;
        let more = out.len() as i64 > limit;
        out.truncate(limit as usize);
        let next = if more {
            out.last()
                .and_then(|r| r["id"].as_i64())
                .map(|id| format!("a{id}"))
        } else {
            None
        };
        Ok(Json(json!({ "data": out, "next_cursor": next })))
    }
    .await;
    into_response(result)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn masks_keep_only_a_tail() {
        assert_eq!(mask("abcdefghijkl", 4), "••••ijkl");
        assert_eq!(mask("abcdefgh", 4), "••••");
        assert_eq!(mask("", 4), "••••");
        assert_eq!(mask("çğüşöıabcdef", 3), "••••def");
    }

    #[test]
    fn like_patterns_take_wildcards_literally() {
        assert_eq!(like("a%b_c\\"), "%a\\%b\\_c\\\\%");
    }

    #[test]
    fn page_cursors_are_validated() {
        let q = |pairs: &[(&str, &str)]| -> HashMap<String, String> {
            pairs
                .iter()
                .map(|(k, v)| (k.to_string(), v.to_string()))
                .collect()
        };
        let p = page_of(&q(&[]), MAX_LIMIT).unwrap();
        assert_eq!((p.limit, p.offset), (25, 0));
        let p = page_of(&q(&[("limit", "100"), ("cursor", "o50")]), MAX_LIMIT).unwrap();
        assert_eq!((p.limit, p.offset), (100, 50));
        for bad in [
            ("limit", "0"),
            ("limit", "101"),
            ("limit", "x"),
            ("cursor", "50"),
            ("cursor", "o-1"),
        ] {
            assert!(page_of(&q(&[bad]), MAX_LIMIT).is_err(), "{bad:?}");
        }
        let rows = (0..4).map(|i| json!(i)).collect();
        assert_eq!(
            paged(
                rows,
                Page {
                    limit: 3,
                    offset: 6
                }
            ),
            json!({ "data": [0, 1, 2], "next_cursor": "o9" })
        );
        assert_eq!(
            paged(
                vec![json!(1)],
                Page {
                    limit: 3,
                    offset: 0
                }
            ),
            json!({ "data": [1], "next_cursor": null })
        );
    }
}
