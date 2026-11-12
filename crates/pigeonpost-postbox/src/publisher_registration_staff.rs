//! Verified publisher registration: the staff review API the back office screen uses (developer
//! program plan §2 "Flow" step 5).
//!
//! | Route | Action | Who | Audit |
//! |---|---|---|---|
//! | `GET /v1/staff/publisher-registrations?state=&account_type=&q=` | `NestReview` | a reviewer with step-up | — |
//! | `GET /v1/staff/publisher-registrations/{application}` | `NestReview` | a reviewer, step-up, no stake | `staff.publisher_registration.view` |
//! | `POST /v1/staff/publisher-registrations/{application}/claim` | `NestReview` | same | `…claim` |
//! | `GET /v1/staff/publisher-registrations/{application}/documents/{document}` | `NestReview` or `StaffFinanceRead` | a reviewer or finance, step-up, no stake | `…document` |
//! | `POST /v1/staff/publisher-registrations/{application}/reveal` | `StaffFinanceRead` | finance, step-up, no stake | `…reveal` (fields and reason) |
//! | `POST /v1/staff/publisher-registrations/{application}/decisions` | `NestReview` | a reviewer, step-up, no stake | `…decision` |
//! | `GET /v1/staff/publisher-registration-alerts` | `NestReview` | a reviewer with step-up | — |
//! | `POST /v1/staff/publisher-registrations/{application}/alerts/{alert}/resolve` | `NestReview` | a reviewer, step-up, no stake | `…alert.resolve` |
//!
//! **Alerts** are the dashboard flags: `kyc_reversed` (identity verification reversed after
//! approval; the publisher's payouts are held) and `payee_push_failed` (finance never received the
//! payee profile). Resolving the last open `kyc_reversed` alert lifts that payout hold.
//!
//! **Roles** (the back office's staff roles). Reviews are the `reviewer` role's (`NestReview`).
//! Documents are for reviewers and for `finance`, who verify payout details before the first
//! payout ([`DOCUMENT_ACTIONS`]). The unmasked bank and tax details are finance's alone
//! (`StaffFinanceRead`, [`REVEAL_ACTIONS`]): a reviewer judges the masked copy and the holder name.
//!
//! The conflict rule is the publisher one, applied to the application: a reviewer who filed it,
//! owns it, or belongs to the owning org (or the linked publisher) is refused `staff_conflict`.

use crate::groups::{GErr, GResult};
use crate::marketplace::{caller, deny, facts, into_response, iso, not_found, parse};
use crate::marketplace_m1::conflict_code;
use crate::marketplace_m2::{audit_row, new_id, staff_gate, wake, PAGE};
use crate::policy::{authorize, Action, Actor, Resource};
use crate::publisher_registration::{
    self as reg, app_in, apps_where, base_json, blocking_failures, checks_in, current_check,
    docs_in, events_in, facts_for, open_sealed, record_event_in, serve_document, type_spec, App,
    Bank, Gate, Tax, MANUAL_CHECKS, OVERRIDABLE,
};
use crate::store::marketplace as mstore;
use crate::store::publisher_registration as rstore;
use crate::{now_unix, AppState, ClientIp, Principal};
use axum::extract::rejection::JsonRejection;
use axum::extract::{Path, Query, State};
use axum::http::{HeaderMap, StatusCode};
use axum::response::Response;
use axum::routing::{get, post};
use axum::{Extension, Json, Router};
use rusqlite::{params, OptionalExtension};
use serde::Deserialize;
use serde_json::{json, Map, Value};
use std::collections::HashMap;

/// Who may view registration documents: a reviewer or finance (any one of these actions).
pub(crate) const DOCUMENT_ACTIONS: [Action; 2] = [Action::NestReview, Action::StaffFinanceRead];
/// Who may reveal the unmasked bank and tax details: finance.
pub(crate) const REVEAL_ACTIONS: [Action; 1] = [Action::StaffFinanceRead];

/// How long a rejected application's documents and sealed details are kept (days).
pub(crate) fn rejected_retention_days() -> i64 {
    crate::env_num::<i64>("POSTBOX_PUBLISHER_DOCS_REJECTED_DAYS", 365)
}

pub(crate) fn router() -> Router<AppState> {
    Router::new()
        .route(
            "/v1/staff/publisher-registrations",
            get(staff_list_registrations),
        )
        .route(
            "/v1/staff/publisher-registrations/{application}",
            get(staff_read_registration),
        )
        .route(
            "/v1/staff/publisher-registrations/{application}/claim",
            post(staff_claim_registration),
        )
        .route(
            "/v1/staff/publisher-registrations/{application}/documents/{document}",
            get(staff_registration_document),
        )
        .route(
            "/v1/staff/publisher-registrations/{application}/reveal",
            post(staff_reveal_registration),
        )
        .route(
            "/v1/staff/publisher-registrations/{application}/decisions",
            post(staff_decide_registration),
        )
        .route(
            "/v1/staff/publisher-registration-alerts",
            get(staff_list_registration_alerts),
        )
        .route(
            "/v1/staff/publisher-registrations/{application}/alerts/{alert}/resolve",
            post(staff_resolve_registration_alert),
        )
}

/// Role (any of `actions`), step-up, then the conflict rule against the application. A missing
/// application is answered 404 once the caller is known to be staff.
async fn app_gate(
    state: &AppState,
    headers: &HeaderMap,
    actions: &[Action],
    id: &str,
) -> GResult<(Principal, String, App)> {
    let (principal, account) = caller(state, headers).await?;
    let (who, aid) = (account.clone(), id.to_string());
    let (roles, conflict, app) = state
        .store
        .group_read(move |c| -> GResult<_> {
            let app = app_in(c, &aid)?;
            let conflict = app.is_some() && rstore::application_conflict_in(c, &who, &aid)?;
            Ok((mstore::live_staff_roles_in(c, &who)?, conflict, app))
        })
        .await?;
    let f = crate::policy::Facts {
        staff_roles: roles,
        staff_conflict: conflict,
        ..facts(state)
    };
    // Any one action suffices. Refused by all: the most useful reason wins (a reviewer without a
    // fresh sign-in is told to step up, not that finance's action needs another role).
    let mut refusals = Vec::new();
    for action in actions {
        match authorize(
            &Actor::Principal(&principal),
            *action,
            &Resource::StaffSubject(id),
            &f,
        ) {
            Ok(()) => {
                refusals.clear();
                break;
            }
            Err(d) => refusals.push(d),
        }
    }
    if !refusals.is_empty() {
        let pick = refusals
            .iter()
            .position(|d| !matches!(d, crate::policy::DenyReason::StaffRequired))
            .unwrap_or(0);
        return Err(deny(refusals.swap_remove(pick), false));
    }
    let app = app.ok_or_else(not_found)?;
    Ok((principal, account, app))
}

#[allow(clippy::too_many_arguments)]
fn registration_audit(
    state: &AppState,
    principal: &Principal,
    account: &str,
    action: &str,
    target: String,
    details: Value,
    client_ip: &Option<Extension<ClientIp>>,
    headers: &HeaderMap,
) -> crate::store::audit::AuditRow {
    audit_row(
        state, principal, account, action, target, details, client_ip, headers,
    )
}

// ---- list and detail ------------------------------------------------------------------------------------

/// `GET /v1/staff/publisher-registrations?state=&account_type=&q=` — the queue. Default: the
/// applications waiting for staff (`submitted`, `in_review`), oldest submission first.
async fn staff_list_registrations(
    State(state): State<AppState>,
    headers: HeaderMap,
    Query(q): Query<HashMap<String, String>>,
) -> Response {
    let result = async {
        let (_, account) = staff_gate(
            &state,
            &headers,
            Action::NestReview,
            "publisher-registrations",
            None,
        )
        .await?;
        const STATES: [&str; 6] = ["draft", "submitted", "in_review", "needs_info", "approved", "rejected"];
        let states: Vec<String> = match q.get("state").map(String::as_str) {
            None | Some("") | Some("open") => vec!["submitted".into(), "in_review".into()],
            Some(s) => {
                let list: Vec<String> = s.split(',').map(|x| x.trim().to_string()).collect();
                if list.iter().any(|x| !STATES.contains(&x.as_str())) {
                    return Err(GErr::bad(
                        "invalid_request",
                        "state: open, or a comma list of draft, submitted, in_review, needs_info, approved, rejected",
                    ));
                }
                list
            }
        };
        let account_type = q.get("account_type").cloned().filter(|s| !s.is_empty());
        if account_type.as_deref().is_some_and(|t| t != "legacy" && type_spec(t).is_none()) {
            return Err(GErr::bad("invalid_request", "unknown account_type"));
        }
        let needle = q
            .get("q")
            .map(|s| s.trim().to_lowercase())
            .filter(|s| !s.is_empty())
            .map(|s| s.chars().take(80).collect::<String>());
        let limit = q
            .get("limit")
            .and_then(|l| l.parse::<usize>().ok())
            .unwrap_or(PAGE)
            .clamp(1, PAGE);
        let rows = state
            .store
            .group_read(move |c| -> GResult<Vec<Value>> {
                let placeholders = (0..states.len()).map(|i| format!("?{}", i + 1)).collect::<Vec<_>>().join(",");
                let args: Vec<&dyn rusqlite::ToSql> = states.iter().map(|s| s as &dyn rusqlite::ToSql).collect();
                let apps = apps_where(
                    c,
                    &format!(
                        "state IN ({placeholders}) ORDER BY COALESCE(submitted_at, created_at), id LIMIT 1000"
                    ),
                    &args,
                )?;
                let mut out = Vec::new();
                for a in apps {
                    if account_type.as_deref().is_some_and(|t| t != a.account_type) {
                        continue;
                    }
                    if let Some(n) = &needle {
                        let hay = format!(
                            "{} {} {} {}",
                            a.public_name,
                            a.profile.legal_name.clone().unwrap_or_default(),
                            a.owner_id,
                            a.id
                        )
                        .to_lowercase();
                        if !hay.contains(n.as_str()) {
                            continue;
                        }
                    }
                    let checks = checks_in(c, &a.id)?;
                    let docs = docs_in(c, &a.id, false)?;
                    let mut v = base_json(&a);
                    v["legal_name"] = json!(a.profile.legal_name);
                    v["country"] = json!(a.profile.country);
                    v["reviewer"] = json!(a.reviewer);
                    v["kyc"] = json!(current_check(&checks).map(|k| json!({
                        "result": k.result, "sandbox": k.provider == "mock", "aml": k.aml_result,
                    })));
                    v["documents"] = json!({
                        "count": docs.len(),
                        "pending_scan": docs.iter().filter(|d| d.scan_state == "pending").count(),
                        "infected": docs.iter().filter(|d| d.scan_state == "infected").count(),
                    });
                    v["conflict"] = json!(rstore::application_conflict_in(c, &account, &a.id)?);
                    v["open_alerts"] = json!(open_alerts_in(c, &a.id)?.len());
                    out.push(v);
                    if out.len() >= limit {
                        break;
                    }
                }
                Ok(out)
            })
            .await?;
        Ok(Json(json!({ "data": rows, "next_cursor": null })))
    }
    .await;
    into_response(result)
}

/// The staff view of one application: everything but the sealed values.
async fn staff_view(state: &AppState, id: &str) -> GResult<Value> {
    let aid = id.to_string();
    let (a, docs, checks, events, pushes, publisher) = state
        .store
        .group_read(move |c| -> GResult<_> {
            let a = app_in(c, &aid)?.ok_or_else(not_found)?;
            let pushes: Vec<Value> = {
                let mut stmt = c.prepare(
                    "SELECT id, status, attempts, last_error, created_at, sent_at
                       FROM publisher_payee_pushes WHERE application_id = ?1 ORDER BY created_at",
                )?;
                let rows = stmt
                    .query_map([&aid], |r| {
                        Ok(json!({
                            "id": r.get::<_, String>(0)?,
                            "status": r.get::<_, String>(1)?,
                            "attempts": r.get::<_, i64>(2)?,
                            "last_error": r.get::<_, Option<String>>(3)?,
                            "created_at": iso(r.get::<_, i64>(4)?),
                            "sent_at": r.get::<_, Option<i64>>(5)?.map(iso),
                        }))
                    })?
                    .collect::<rusqlite::Result<Vec<_>>>()?;
                rows
            };
            let publisher = match &a.publisher_id {
                Some(p) => {
                    let hold: Option<String> = c
                        .query_row(
                            "SELECT payout_hold FROM publisher_profiles WHERE id = ?1",
                            [p],
                            |r| r.get(0),
                        )
                        .optional()?
                        .flatten();
                    crate::marketplace_m2::pub_in(c, p)?.map(|p| {
                        json!({
                            "id": p.id, "status": p.status, "display_name": p.display_name,
                            "payout_hold": hold,
                        })
                    })
                }
                None => None,
            };
            Ok((
                a,
                docs_in(c, &aid, true)?,
                checks_in(c, &aid)?,
                events_in(c, &aid)?,
                pushes,
                publisher,
            ))
        })
        .await?;
    let (docs, checks, list) = facts_for(state, &a, docs, checks, now_unix() as i64);
    let mut v = base_json(&a);
    v["applicant"] = json!(a.applicant);
    v["payout_hold"] = json!(publisher
        .as_ref()
        .and_then(|p| p["payout_hold"].as_str().map(str::to_string)));
    v["reviewer"] = json!(a.reviewer);
    v["profile"] = serde_json::to_value(&a.profile).unwrap_or(json!({}));
    v["bank"] = a.bank_masked.clone().unwrap_or(Value::Null);
    v["tax"] = a.tax_masked.clone().unwrap_or(Value::Null);
    v["agreement"] = json!({
        "agreement_version": a.agreement_version,
        "content_policy_version": a.content_policy_version,
        "accepted_at": a.agreement_accepted_at.map(iso),
    });
    v["documents"] = json!(docs
        .iter()
        .map(|d| {
            let mut j = d.json();
            j["removed"] = json!(d.removed_at.is_some());
            j["scan_detail"] = json!(d.scan_detail);
            j
        })
        .collect::<Vec<_>>());
    v["kyc"] = json!(checks.iter().map(|c| c.staff_json()).collect::<Vec<_>>());
    v["checklist"] = json!(list);
    v["manual_checks"] = json!(MANUAL_CHECKS
        .iter()
        .map(|(id, label)| json!({ "id": id, "label": label }))
        .collect::<Vec<_>>());
    v["requirements"] = json!(type_spec(&a.account_type).map(|t| t.docs.iter().map(|d| json!({
        "type": d.kind, "label": d.en, "label_tr": d.tr,
        "need": match d.need { reg::Need::Required => "required", reg::Need::Optional => "optional", reg::Need::IfTreaty => "if_treaty" },
    })).collect::<Vec<_>>()));
    v["events"] = json!(events);
    v["decision"] = json!({
        "decided_by": a.decided_by, "reason": a.decision_reason, "checklist": a.checklist,
    });
    v["payee_pushes"] = json!(pushes);
    let aid = a.id.clone();
    v["alerts"] = json!(
        state
            .store
            .group_read(move |c| -> GResult<Vec<Value>> {
                Ok(alerts_in(c, "application_id = ?1", &aid)?)
            })
            .await?
    );
    v["publisher"] = json!(publisher);
    Ok(v)
}

/// `GET /v1/staff/publisher-registrations/{application}` — audited: it shows personal data.
async fn staff_read_registration(
    State(state): State<AppState>,
    client_ip: Option<Extension<ClientIp>>,
    headers: HeaderMap,
    Path(id): Path<String>,
) -> Response {
    let result = async {
        let (principal, account, a) =
            app_gate(&state, &headers, &[Action::NestReview], &id).await?;
        let row = registration_audit(
            &state,
            &principal,
            &account,
            "staff.publisher_registration.view",
            format!("publisher_application:{}", a.id),
            json!({ "state": a.state }),
            &client_ip,
            &headers,
        );
        state
            .store
            .group_tx(move |tx| -> GResult<()> {
                crate::store::audit::append_in(tx, &row)?;
                Ok(())
            })
            .await?;
        let mut v = staff_view(&state, &a.id).await?;
        v["conflict"] = json!(false);
        Ok(Json(json!({ "data": v })))
    }
    .await;
    into_response(result)
}

/// `POST /v1/staff/publisher-registrations/{application}/claim` — take a submitted application
/// into review (`in_review`, the reviewer recorded). Claiming one already in review re-assigns it.
async fn staff_claim_registration(
    State(state): State<AppState>,
    client_ip: Option<Extension<ClientIp>>,
    headers: HeaderMap,
    Path(id): Path<String>,
) -> Response {
    let result = async {
        let (principal, account, a) =
            app_gate(&state, &headers, &[Action::NestReview], &id).await?;
        if !matches!(a.state.as_str(), "submitted" | "in_review") {
            return Err(conflict_code(
                "application_not_waiting",
                "this application is not waiting for review",
            ));
        }
        let now = now_unix() as i64;
        let row = registration_audit(
            &state,
            &principal,
            &account,
            "staff.publisher_registration.claim",
            format!("publisher_application:{}", a.id),
            json!({ "from": a.state }),
            &client_ip,
            &headers,
        );
        let (aid, by, from) = (a.id.clone(), account.clone(), a.state.clone());
        let woke = state
            .store
            .group_tx(move |tx| -> GResult<Vec<String>> {
                let n = tx.execute(
                    "UPDATE publisher_applications SET state = 'in_review', reviewer = ?2,
                        revision = revision + 1, updated_at = ?3
                      WHERE id = ?1 AND state IN ('submitted','in_review')",
                    params![aid, by, now],
                )?;
                if n == 0 {
                    return Err(conflict_code(
                        "application_not_waiting",
                        "this application is not waiting for review",
                    ));
                }
                record_event_in(
                    tx,
                    &aid,
                    &by,
                    "staff",
                    "claimed",
                    Some(&from),
                    Some("in_review"),
                    None,
                    now,
                )?;
                crate::store::audit::append_in(tx, &row)?;
                reg::announce_in(tx, &a, "in_review", now)
            })
            .await?;
        wake(&state, woke);
        Ok(Json(json!({ "data": staff_view(&state, &id).await? })))
    }
    .await;
    into_response(result)
}

/// `GET /v1/staff/publisher-registrations/{application}/documents/{document}?inline=1` — audited.
async fn staff_registration_document(
    State(state): State<AppState>,
    client_ip: Option<Extension<ClientIp>>,
    headers: HeaderMap,
    Path((id, doc)): Path<(String, String)>,
    Query(q): Query<HashMap<String, String>>,
) -> Response {
    let result = async {
        let (principal, account, a) = app_gate(&state, &headers, &DOCUMENT_ACTIONS, &id).await?;
        let aid = a.id.clone();
        let d = state
            .store
            .group_read(move |c| -> GResult<Option<reg::Doc>> {
                Ok(docs_in(c, &aid, true)?.into_iter().find(|d| d.id == doc))
            })
            .await?
            .ok_or_else(not_found)?;
        let row = registration_audit(
            &state,
            &principal,
            &account,
            "staff.publisher_registration.document",
            format!("publisher_application:{}", a.id),
            json!({ "document_id": d.id, "type": d.doc_type, "scan": d.scan_state }),
            &client_ip,
            &headers,
        );
        // Recorded before the bytes leave: a view that happened always has its row.
        state
            .store
            .group_tx(move |tx| -> GResult<()> {
                crate::store::audit::append_in(tx, &row)?;
                Ok(())
            })
            .await?;
        serve_document(&state, &d, q.get("inline").is_some_and(|v| v == "1"))
    }
    .await;
    into_response(result)
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct RevealReq {
    fields: Vec<String>,
    reason: String,
}

/// `POST /v1/staff/publisher-registrations/{application}/reveal` `{fields: ["bank","tax"], reason}`
/// — the one read that shows bank and tax details unmasked. The fields and the reason are audited
/// before anything is opened.
async fn staff_reveal_registration(
    State(state): State<AppState>,
    client_ip: Option<Extension<ClientIp>>,
    headers: HeaderMap,
    Path(id): Path<String>,
    body: Result<Json<Value>, JsonRejection>,
) -> Response {
    let result = async {
        let req: RevealReq = parse(body)?;
        let reason = crate::marketplace_m2::text(&req.reason, "reason", 5, 500)?;
        if req.fields.is_empty() || req.fields.iter().any(|f| f != "bank" && f != "tax") {
            return Err(GErr::bad("invalid_request", "fields: bank and/or tax"));
        }
        let (principal, account, a) = app_gate(&state, &headers, &REVEAL_ACTIONS, &id).await?;
        let row = registration_audit(
            &state,
            &principal,
            &account,
            "staff.publisher_registration.reveal",
            format!("publisher_application:{}", a.id),
            json!({ "fields": req.fields, "reason": reason }),
            &client_ip,
            &headers,
        );
        state
            .store
            .group_tx(move |tx| -> GResult<()> {
                crate::store::audit::append_in(tx, &row)?;
                Ok(())
            })
            .await?;
        let mut out = Map::new();
        for f in &req.fields {
            let v = match f.as_str() {
                "bank" => match &a.bank_sealed {
                    Some(s) => serde_json::to_value(open_sealed::<Bank>(&state, &a.id, "bank", s)?)
                        .unwrap_or(Value::Null),
                    None => Value::Null,
                },
                _ => match &a.tax_sealed {
                    Some(s) => serde_json::to_value(open_sealed::<Tax>(&state, &a.id, "tax", s)?)
                        .unwrap_or(Value::Null),
                    None => Value::Null,
                },
            };
            out.insert(f.clone(), v);
        }
        let mut resp = axum::response::IntoResponse::into_response(Json(json!({ "data": out })));
        resp.headers_mut().insert(
            axum::http::header::CACHE_CONTROL,
            axum::http::HeaderValue::from_static("no-store"),
        );
        Ok(resp)
    }
    .await;
    into_response(result)
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct DecideReq {
    decision: String,
    #[serde(default)]
    message: Option<String>,
    /// The revision the reviewer read; a stale decision is refused.
    revision: i64,
    /// The reviewer's own judgements ([`MANUAL_CHECKS`]); an approval needs every one `true`.
    #[serde(default)]
    checklist: Map<String, Value>,
    /// Approving on a sandbox (mock) KYC result needs this, explicitly. Refused on production.
    #[serde(default)]
    acknowledge_sandbox_kyc: bool,
    /// Approving while no malware scanner is configured needs this, explicitly (audited).
    #[serde(default)]
    acknowledge_unscanned_documents: bool,
    /// A written reason for approving past a failing reviewer-judged check
    /// ([`OVERRIDABLE`]: `names_match`, `aml`), audited.
    #[serde(default)]
    overrides: Map<String, Value>,
}

/// `POST /v1/staff/publisher-registrations/{application}/decisions` —
/// `approved` | `needs_info` (with the message the developer sees) | `rejected` (with the reason).
async fn staff_decide_registration(
    State(state): State<AppState>,
    client_ip: Option<Extension<ClientIp>>,
    headers: HeaderMap,
    Path(id): Path<String>,
    body: Result<Json<Value>, JsonRejection>,
) -> Response {
    let result = async {
        let req: DecideReq = parse(body)?;
        if !matches!(req.decision.as_str(), "approved" | "needs_info" | "rejected") {
            return Err(GErr::bad("invalid_request", "decision: approved, needs_info or rejected"));
        }
        let message = match (&req.message, req.decision.as_str()) {
            (Some(m), _) => Some(crate::marketplace_m2::text(m, "message", 1, 4000)?),
            (None, "approved") => None,
            (None, _) => return Err(GErr::bad("message_required", "tell the developer what to fix, or why")),
        };
        let (principal, account, a) = app_gate(&state, &headers, &[Action::NestReview], &id).await?;
        if !matches!(a.state.as_str(), "submitted" | "in_review") {
            return Err(conflict_code("application_not_waiting", "this application is not waiting for a decision"));
        }
        if req.revision != a.revision {
            return Err(GErr::conflict("revision_mismatch", "the application changed since you read it")
                .with("revision", a.revision));
        }
        let now = now_unix() as i64;
        let aid = a.id.clone();
        let (docs, checks) = state
            .store
            .group_read(move |c| -> GResult<_> { Ok((docs_in(c, &aid, false)?, checks_in(c, &aid)?)) })
            .await?;
        let (_, checks, list) = facts_for(&state, &a, docs, checks, now);
        let mut manual = Map::new();
        let mut overrides = Map::new();
        let production = !state.nests.mp.staging;
        if req.acknowledge_sandbox_kyc && production {
            return Err(GErr::new(
                StatusCode::UNPROCESSABLE_ENTITY,
                "sandbox_kyc_not_allowed",
                "a sandbox identity check never approves a publisher on production",
            ));
        }
        if req.decision == "approved" {
            for (k, v) in &req.overrides {
                if !OVERRIDABLE.contains(&k.as_str()) {
                    return Err(GErr::bad("invalid_request", "overrides: names_match or aml"));
                }
                let reason = crate::marketplace_m2::text(v.as_str().unwrap_or_default(), "override reason", 10, 1000)?;
                overrides.insert(k.clone(), json!(reason));
            }
            let failing: Vec<String> = blocking_failures(&list, Gate::Approve)
                .into_iter()
                .filter(|id| !overrides.contains_key(id))
                .collect();
            if !failing.is_empty() {
                let overridable: Vec<&String> = failing.iter().filter(|f| OVERRIDABLE.contains(&f.as_str())).collect();
                return Err(GErr::new(
                    StatusCode::UNPROCESSABLE_ENTITY,
                    "checks_failing",
                    "the automated checks must pass before approval",
                )
                .with("failing", failing.clone())
                .with("overridable", json!(overridable)));
            }
            let unscanned = list.iter().any(|i| i["id"] == "documents_scanned" && i["ok"].is_null());
            if unscanned && !req.acknowledge_unscanned_documents {
                return Err(GErr::new(
                    StatusCode::UNPROCESSABLE_ENTITY,
                    "documents_not_scanned",
                    "no malware scanner is configured; acknowledge that the documents are not malware-scanned",
                ));
            }
            let missing: Vec<&str> = MANUAL_CHECKS
                .iter()
                .map(|(id, _)| *id)
                .filter(|id| req.checklist.get(*id) != Some(&Value::Bool(true)))
                .collect();
            if !missing.is_empty() {
                return Err(GErr::new(
                    StatusCode::UNPROCESSABLE_ENTITY,
                    "checklist_incomplete",
                    "confirm every reviewer check before approving",
                )
                .with("missing", missing));
            }
            if current_check(&checks).is_some_and(|c| c.provider == "mock") && !req.acknowledge_sandbox_kyc {
                return Err(GErr::new(
                    StatusCode::UNPROCESSABLE_ENTITY,
                    "sandbox_kyc",
                    "identity was verified in the sandbox only; acknowledge that to approve",
                ));
            }
        }
        for (k, v) in &req.checklist {
            if MANUAL_CHECKS.iter().any(|(id, _)| id == k) {
                manual.insert(k.clone(), json!(v == &Value::Bool(true)));
            }
        }
        let unscanned_ack = req.decision == "approved" && req.acknowledge_unscanned_documents;
        let record = json!({
            "automated": list,
            "reviewer": manual,
            "overrides": overrides,
            "sandbox_kyc_acknowledged": req.acknowledge_sandbox_kyc,
            "documents_not_malware_scanned_acknowledged": unscanned_ack,
        });
        let row = registration_audit(
            &state,
            &principal,
            &account,
            "staff.publisher_registration.decision",
            format!("publisher_application:{}", a.id),
            json!({
                "decision": req.decision,
                "revision": a.revision,
                "overrides": overrides,
                "acknowledged": {
                    "sandbox_kyc": req.acknowledge_sandbox_kyc,
                    "documents_not_malware_scanned": unscanned_ack,
                },
            }),
            &client_ip,
            &headers,
        );
        let decision = req.decision.clone();
        let keep_days = rejected_retention_days();
        let (aid, by) = (a.id.clone(), account.clone());
        let woke = state
            .store
            .group_tx(move |tx| -> GResult<Vec<String>> {
                let current = app_in(tx, &aid)?.ok_or_else(not_found)?;
                if current.revision != a.revision || !matches!(current.state.as_str(), "submitted" | "in_review") {
                    return Err(GErr::conflict("revision_mismatch", "the application changed since you read it"));
                }
                let mut woke = Vec::new();
                let mut publisher = current.publisher_id.clone();
                match decision.as_str() {
                    "approved" => {
                        let pid = certify_in(tx, &current, &by, message.as_deref(), now)?;
                        woke.extend(mstore::publisher_event_in(
                            tx,
                            &pid,
                            "nest.publisher.decision",
                            &json!({ "publisher_id": pid, "status": "approved" }),
                            now as u64,
                        )?);
                        tx.execute(
                            "INSERT INTO publisher_payee_pushes (id, application_id, publisher_id, approved_by, next_at, created_at)
                             VALUES (?1, ?2, ?3, ?4, ?5, ?5)",
                            params![new_id("ppp"), aid, pid, by, now],
                        )?;
                        publisher = Some(pid);
                    }
                    "rejected" => {
                        tx.execute(
                            "UPDATE publisher_application_documents SET retain_until = ?2
                              WHERE application_id = ?1 AND purged_at IS NULL",
                            params![aid, now + keep_days * 86_400],
                        )?;
                        // A legacy profile still waiting is declined with it.
                        if let Some(p) = &current.publisher_id {
                            let n = tx.execute(
                                "UPDATE publisher_profiles SET status = 'rejected', decided_by = ?2, decided_at = ?3,
                                    decision_reason = ?4, updated_at = ?3 WHERE id = ?1 AND status = 'applied'",
                                params![p, by, now, message],
                            )?;
                            if n > 0 {
                                tx.execute(
                                    "INSERT INTO publisher_certifications (id, publisher_id, decision, reason, decided_by, created_at)
                                     VALUES (?1, ?2, 'rejected', ?3, ?4, ?5)",
                                    params![new_id("crt"), p, message.clone().unwrap_or_default(), by, now],
                                )?;
                            }
                        }
                    }
                    _ => {}
                }
                tx.execute(
                    "UPDATE publisher_applications SET state = ?2, publisher_id = ?3, reviewer = ?4,
                        decided_at = CASE WHEN ?2 = 'needs_info' THEN decided_at ELSE ?5 END,
                        decided_by = CASE WHEN ?2 = 'needs_info' THEN decided_by ELSE ?4 END,
                        decision_reason = CASE WHEN ?2 = 'needs_info' THEN decision_reason ELSE ?6 END,
                        checklist = ?7, revision = revision + 1, updated_at = ?5
                      WHERE id = ?1",
                    params![aid, decision, publisher, by, now, message, record.to_string()],
                )?;
                record_event_in(
                    tx,
                    &aid,
                    &by,
                    "staff",
                    "decision",
                    Some(&current.state),
                    Some(&decision),
                    message.as_deref(),
                    now,
                )?;
                crate::store::audit::append_in(tx, &row)?;
                woke.extend(reg::announce_in(tx, &current, &decision, now)?);
                Ok(woke)
            })
            .await?;
        wake(&state, woke);
        Ok((StatusCode::CREATED, Json(json!({ "data": staff_view(&state, &id).await? }))))
    }
    .await;
    into_response(result)
}

/// Certify the application's publisher profile, creating it when the owner has none. Returns its
/// id. A revoked publisher is not restored by a registration: that is a separate staff decision.
fn certify_in(
    tx: &rusqlite::Transaction<'_>,
    a: &App,
    by: &str,
    reason: Option<&str>,
    now: i64,
) -> GResult<String> {
    let linked = match &a.publisher_id {
        Some(p) => Some(p.clone()),
        None => tx
            .query_row(
                "SELECT id FROM publisher_profiles WHERE owner_kind = ?1 AND owner_id = ?2
                   AND status IN ('applied','approved') LIMIT 1",
                params![a.owner_kind, a.owner_id],
                |r| r.get::<_, String>(0),
            )
            .optional()?,
    };
    let reason = reason.unwrap_or("Verified publisher registration approved");
    let id = match linked {
        Some(p) => {
            let status: Option<String> = tx
                .query_row(
                    "SELECT status FROM publisher_profiles WHERE id = ?1",
                    [&p],
                    |r| r.get(0),
                )
                .optional()?;
            match status.as_deref() {
                Some("revoked") => {
                    return Err(conflict_code(
                        "publisher_revoked",
                        "this publisher's certification is revoked; lift that separately",
                    ))
                }
                None => return Err(not_found()),
                _ => {}
            }
            // A declined profile is certified again only if the owner has no other live one.
            let p = if status.as_deref() == Some("rejected") {
                tx.query_row(
                    "SELECT id FROM publisher_profiles WHERE owner_kind = ?1 AND owner_id = ?2
                       AND status IN ('applied','approved') LIMIT 1",
                    params![a.owner_kind, a.owner_id],
                    |r| r.get::<_, String>(0),
                )
                .optional()?
                .unwrap_or(p)
            } else {
                p
            };
            tx.execute(
                "UPDATE publisher_profiles SET status = 'approved', display_name = ?2, decided_by = ?3,
                    decided_at = ?4, decision_reason = ?5, updated_at = ?4, verified_application_id = ?6,
                    payout_hold = NULL
                  WHERE id = ?1",
                params![p, a.public_name, by, now, reason, a.id],
            )?;
            p
        }
        None => {
            let p = new_id("pub");
            tx.execute(
                "INSERT INTO publisher_profiles
                    (id, owner_kind, owner_id, display_name, status, applied_by, decided_by, decided_at,
                     decision_reason, created_at, updated_at, verified_application_id)
                 VALUES (?1, ?2, ?3, ?4, 'approved', ?5, ?6, ?7, ?8, ?7, ?7, ?9)",
                params![p, a.owner_kind, a.owner_id, a.public_name, a.applicant, by, now, reason, a.id],
            )?;
            p
        }
    };
    tx.execute(
        "INSERT INTO publisher_certifications (id, publisher_id, decision, reason, decided_by, created_at)
         VALUES (?1, ?2, 'approved', ?3, ?4, ?5)",
        params![new_id("crt"), id, reason, by, now],
    )?;
    Ok(id)
}

// ---- alerts ------------------------------------------------------------------------------------------

fn alert_json(r: &rusqlite::Row<'_>) -> rusqlite::Result<Value> {
    Ok(json!({
        "id": r.get::<_, String>(0)?,
        "application_id": r.get::<_, String>(1)?,
        "publisher_id": r.get::<_, Option<String>>(2)?,
        "kind": r.get::<_, String>(3)?,
        "detail": r.get::<_, String>(4)?,
        "created_at": iso(r.get::<_, i64>(5)?),
        "resolved_at": r.get::<_, Option<i64>>(6)?.map(iso),
        "resolved_by": r.get::<_, Option<String>>(7)?,
        "resolution": r.get::<_, Option<String>>(8)?,
    }))
}

pub(crate) fn alerts_in(
    c: &rusqlite::Connection,
    filter: &str,
    arg: &str,
) -> rusqlite::Result<Vec<Value>> {
    let mut stmt = c.prepare(&format!(
        "SELECT id, application_id, publisher_id, kind, detail, created_at, resolved_at, resolved_by,
                resolution FROM publisher_staff_alerts WHERE {filter} ORDER BY created_at, id LIMIT 200"
    ))?;
    let rows = stmt.query_map([arg], alert_json)?.collect();
    rows
}

pub(crate) fn open_alerts_in(c: &rusqlite::Connection, app: &str) -> rusqlite::Result<Vec<Value>> {
    alerts_in(c, "application_id = ?1 AND resolved_at IS NULL", app)
}

/// `GET /v1/staff/publisher-registration-alerts?state=open|all` — the dashboard flags.
async fn staff_list_registration_alerts(
    State(state): State<AppState>,
    headers: HeaderMap,
    Query(q): Query<HashMap<String, String>>,
) -> Response {
    let result = async {
        staff_gate(
            &state,
            &headers,
            Action::NestReview,
            "publisher-registration-alerts",
            None,
        )
        .await?;
        let all = q.get("state").is_some_and(|s| s == "all");
        let rows = state
            .store
            .group_read(move |c| -> GResult<Vec<Value>> {
                Ok(if all {
                    alerts_in(c, "?1 = ?1", "")?
                } else {
                    alerts_in(c, "resolved_at IS NULL AND ?1 = ?1", "")?
                })
            })
            .await?;
        Ok(Json(json!({ "data": rows, "next_cursor": null })))
    }
    .await;
    into_response(result)
}

/// `POST /v1/staff/publisher-registrations/{application}/alerts/{alert}/resolve` `{reason}` — close
/// an alert with a written reason (audited). The last open `kyc_reversed` alert lifts that hold.
async fn staff_resolve_registration_alert(
    State(state): State<AppState>,
    client_ip: Option<Extension<ClientIp>>,
    headers: HeaderMap,
    Path((id, alert)): Path<(String, String)>,
    body: Result<Json<Value>, JsonRejection>,
) -> Response {
    let result = async {
        let req: ResolveReq = parse(body)?;
        let reason = crate::marketplace_m2::text(&req.reason, "reason", 10, 1000)?;
        let (principal, account, a) = app_gate(&state, &headers, &[Action::NestReview], &id).await?;
        let row = registration_audit(
            &state,
            &principal,
            &account,
            "staff.publisher_registration.alert.resolve",
            format!("publisher_application:{}", a.id),
            json!({ "alert_id": alert, "reason": reason }),
            &client_ip,
            &headers,
        );
        let now = now_unix() as i64;
        let (aid, by) = (a.id.clone(), account.clone());
        let out = state
            .store
            .group_tx(move |tx| -> GResult<Value> {
                let kind: Option<String> = tx
                    .query_row(
                        "SELECT kind FROM publisher_staff_alerts
                          WHERE id = ?1 AND application_id = ?2 AND resolved_at IS NULL",
                        params![alert, aid],
                        |r| r.get(0),
                    )
                    .optional()?;
                let Some(kind) = kind else { return Err(not_found()) };
                tx.execute(
                    "UPDATE publisher_staff_alerts SET resolved_at = ?2, resolved_by = ?3, resolution = ?4
                      WHERE id = ?1",
                    params![alert, now, by, reason],
                )?;
                if kind == "kyc_reversed" {
                    if let Some(p) = &a.publisher_id {
                        tx.execute(
                            "UPDATE publisher_profiles SET payout_hold = NULL, updated_at = ?2
                              WHERE id = ?1 AND payout_hold = 'kyc_reversed'",
                            params![p, now],
                        )?;
                    }
                }
                record_event_in(tx, &aid, &by, "staff", "alert_resolved", None, None, Some(&reason), now)?;
                crate::store::audit::append_in(tx, &row)?;
                Ok(json!({ "alert_id": alert, "kind": kind, "resolved": true }))
            })
            .await?;
        Ok(Json(json!({ "data": out })))
    }
    .await;
    into_response(result)
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct ResolveReq {
    reason: String,
}
