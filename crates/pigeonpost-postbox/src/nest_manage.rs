//! Managing one's own Nests (Nests Phase B5): pause, resume, rename, usage, export, delete.
//!
//! | Route | Action | What |
//! |---|---|---|
//! | `POST /v1/nests/{nest}/pause`, `/resume` | `NestOwn` | stop or restart answering; a Nest paused for funding resumes only when it is paid for |
//! | `PATCH /v1/nests/{nest}` | `NestOwn` | rename |
//! | `DELETE /v1/nests/{nest}` | `NestOwn` | delete, with the label typed back as confirmation |
//! | `GET /v1/nests/{nest}/usage` | `NestView` | this month's model tokens, running hours and storage, from the Nests ledger |
//! | `GET /v1/nests/{nest}/export` | `NestOwn` | a zip of the files, their text and the conversations |
//!
//! Every route answers 404 for another account's Nest, the same as for a Nest that does not exist.

use crate::groups::{GErr, GResult};
use crate::nest_catalog;
use crate::nests::NestdError;
use crate::policy::{authorize, Action, Actor, Facts, Resource};
use crate::store::nest_funding;
use crate::{capabilities, now_unix, principal_for_token, AppState};
use axum::extract::rejection::JsonRejection;
use axum::extract::{Path, State};
use axum::http::{header, HeaderMap, HeaderValue, StatusCode};
use axum::response::{IntoResponse, Response};
use axum::routing::{get, post};
use axum::{Json, Router};
use rusqlite::{params, OptionalExtension};
use serde_json::{json, Value};

pub(crate) fn router() -> Router<AppState> {
    Router::new()
        .route("/v1/nests/{nest}/pause", post(pause))
        .route("/v1/nests/{nest}/resume", post(resume))
        .route("/v1/nests/{nest}/usage", get(usage))
        .route("/v1/nests/{nest}/export", get(export))
}

/// One owned Nest, as these routes need it.
pub(crate) struct Owned {
    pub account: String,
    pub nest_id: String,
    pub address: Option<String>,
    pub label: String,
    pub state: String,
    pub funding: String,
    pub paused_reason: Option<String>,
}

pub(crate) async fn owned(
    state: &AppState,
    headers: &HeaderMap,
    action: Action,
    nest: &str,
) -> GResult<Owned> {
    let principal = principal_for_token(state, crate::bearer(headers)).await?;
    let account = capabilities::principal_account(&principal)
        .map(str::to_string)
        .ok_or_else(|| {
            GErr::forbidden(
                "credential_not_accepted",
                "an account credential is required",
            )
        })?;
    let facts = Facts {
        now: now_unix(),
        ..Facts::default()
    };
    authorize(
        &Actor::Principal(&principal),
        action,
        &Resource::Account(&account),
        &facts,
    )
    .map_err(|deny| {
        let code = deny.code();
        GErr::new(
            deny.into_response().status(),
            code,
            "this credential may not do that",
        )
    })?;
    if !crate::filtering::feature_on(state, Some(&account), capabilities::NESTS).await {
        return Err(GErr::forbidden(
            "feature_disabled",
            "Nests are not enabled for this account",
        ));
    }
    let missing = || GErr::new(StatusCode::NOT_FOUND, "no_nest", "no such Nest");
    if !crate::nests::nest_id_ok(nest) {
        return Err(missing());
    }
    let (n, acct) = (nest.to_string(), account.clone());
    type Row = (Option<String>, String, String, String, Option<String>);
    let row: Option<Row> = state
        .store
        .group_read(move |c| -> GResult<Option<Row>> {
            Ok(c.query_row(
                "SELECT address, label, state, funding, paused_reason FROM nests
                  WHERE id = ?1 AND account_id = ?2 AND deleted_at IS NULL",
                params![n, acct],
                |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?, r.get(4)?)),
            )
            .optional()?)
        })
        .await?;
    let (address, label, st, funding, paused_reason) = row.ok_or_else(missing)?;
    Ok(Owned {
        account,
        nest_id: nest.to_string(),
        address,
        label,
        state: st,
        funding,
        paused_reason,
    })
}

fn nestd_err(e: NestdError) -> GErr {
    match e {
        NestdError::Unavailable => GErr::new(
            StatusCode::SERVICE_UNAVAILABLE,
            "nests_unavailable",
            "no Nests host is configured",
        ),
        NestdError::Failed(_) => GErr::new(
            StatusCode::BAD_GATEWAY,
            "nestd_failed",
            "the Nests host did not answer",
        ),
    }
}

async fn record(
    state: &AppState,
    o: &Owned,
    new_state: &'static str,
    reason: Option<&'static str>,
    action: &'static str,
) -> GResult<()> {
    let audit = state.auditor.row(
        crate::audit::ActorKind::Human,
        Some(o.account.clone()),
        action,
        Some(format!("nest:{}", o.nest_id)),
        json!({}),
        None,
        None,
        now_unix(),
    );
    let id = o.nest_id.clone();
    state
        .store
        .group_tx(move |tx| -> GResult<()> {
            tx.execute(
                "UPDATE nests SET state = ?2, paused_reason = ?3, updated_at = ?4 WHERE id = ?1",
                params![id, new_state, reason, now_unix() as i64],
            )?;
            crate::store::audit::append_in(tx, &audit)?;
            Ok(())
        })
        .await
}

/// `POST /v1/nests/{nest}/pause` — the owner stops it answering. Mail waits; nothing is lost.
async fn pause(
    State(state): State<AppState>,
    headers: HeaderMap,
    Path(nest): Path<String>,
) -> Response {
    let result = async {
        let o = owned(&state, &headers, Action::NestOwn, &nest).await?;
        if o.state == "paused" {
            return Ok(Json(json!({ "nest_id": nest, "state": "paused" })).into_response());
        }
        crate::nests::runtime_action(&state, &nest, "pause").await?;
        record(&state, &o, "paused", Some("owner"), "nest.owner_pause").await?;
        Ok::<_, GErr>(Json(json!({ "nest_id": nest, "state": "paused" })).into_response())
    }
    .await;
    result.unwrap_or_else(|e| e.into_response())
}

/// `POST /v1/nests/{nest}/resume` — only a Nest its owner paused. A Nest paused for funding comes
/// back when it is paid for (the funding sweep resumes it), never from this button.
async fn resume(
    State(state): State<AppState>,
    headers: HeaderMap,
    Path(nest): Path<String>,
) -> Response {
    let result = async {
        let o = owned(&state, &headers, Action::NestOwn, &nest).await?;
        match o.paused_reason.as_deref() {
            Some("unfunded") => {
                return Err(GErr::new(
                    StatusCode::PAYMENT_REQUIRED,
                    "needs_payment",
                    "this Nest is paused until its package is paid for",
                ))
            }
            Some("over_capacity") => {
                return Err(GErr::conflict(
                    "over_capacity",
                    "your package has fewer Nests than you have; delete one or upgrade",
                ))
            }
            _ => {}
        }
        if o.state != "paused" && o.state != "stopped" {
            return Ok(Json(json!({ "nest_id": nest, "state": o.state })).into_response());
        }
        crate::nests::runtime_action(&state, &nest, "resume").await?;
        record(&state, &o, "running", None, "nest.owner_resume").await?;
        Ok::<_, GErr>(Json(json!({ "nest_id": nest, "state": "running" })).into_response())
    }
    .await;
    result.unwrap_or_else(|e| e.into_response())
}

/// `PATCH /v1/nests/{nest} {label}`
pub(crate) async fn rename(
    State(state): State<AppState>,
    headers: HeaderMap,
    Path(nest): Path<String>,
    body: Result<Json<Value>, JsonRejection>,
) -> Response {
    let result = async {
        let o = owned(&state, &headers, Action::NestOwn, &nest).await?;
        let Json(req) = body.map_err(|_| GErr::bad("invalid_json", "expected {label}"))?;
        let label = req["label"]
            .as_str()
            .map(str::trim)
            .unwrap_or("")
            .to_string();
        if label.is_empty()
            || label.chars().count() > 64
            || label.chars().any(char::is_control)
            || req.as_object().is_some_and(|m| m.len() != 1)
        {
            return Err(GErr::bad("bad_label", "label: 1-64 printable characters"));
        }
        let (id, l) = (o.nest_id.clone(), label.clone());
        state
            .store
            .group_tx(move |tx| -> GResult<()> {
                tx.execute(
                    "UPDATE nests SET label = ?2, updated_at = ?3 WHERE id = ?1",
                    params![id, l, now_unix() as i64],
                )?;
                Ok(())
            })
            .await?;
        Ok::<_, GErr>(Json(json!({ "nest_id": nest, "label": label })).into_response())
    }
    .await;
    result.unwrap_or_else(|e| e.into_response())
}

/// `DELETE /v1/nests/{nest} {confirm}` — `confirm` must equal the Nest's label. The runtime and its
/// encrypted volume are destroyed, the mailbox credential revoked, open approvals closed. A
/// package slot is free again at once.
pub(crate) async fn remove(
    State(state): State<AppState>,
    headers: HeaderMap,
    Path(nest): Path<String>,
    body: Result<Json<Value>, JsonRejection>,
) -> Response {
    let result = async {
        let o = owned(&state, &headers, Action::NestOwn, &nest).await?;
        let Json(req) = body.map_err(|_| GErr::bad("confirm_required", "type the Nest's name to delete it"))?;
        if req["confirm"].as_str() != Some(o.label.as_str()) {
            return Err(GErr::bad("confirm_required", "type the Nest's name to delete it"));
        }
        let audit = state.auditor.row(
            crate::audit::ActorKind::Human,
            Some(o.account.clone()),
            "nest.delete",
            Some(format!("nest:{nest}")),
            json!({ "by": "owner" }),
            None,
            None,
            now_unix(),
        );
        crate::nests::teardown(&state, &nest, o.address.clone(), audit).await?;
        let id = nest.clone();
        state
            .store
            .group_tx(move |tx| -> GResult<()> {
                let now = now_unix() as i64;
                tx.execute("UPDATE nest_files SET deleted_at = ?2 WHERE nest_id = ?1 AND deleted_at IS NULL", params![id, now])?;
                tx.execute("UPDATE nest_approvals SET status = 'expired', decided_at = ?2 WHERE nest_id = ?1 AND status = 'pending'", params![id, now])?;
                Ok(())
            })
            .await?;
        Ok::<_, GErr>(Json(json!({ "nest_id": nest, "state": "deleted" })).into_response())
    }
    .await;
    result.unwrap_or_else(|e| e.into_response())
}

/// `GET /v1/nests/{nest}/usage` — the Nests host's ledger for this month, next to the package's
/// allowance, plus the files stored. The numbers are the ledger's own, not recomputed here.
async fn usage(
    State(state): State<AppState>,
    headers: HeaderMap,
    Path(nest): Path<String>,
) -> Response {
    let result = async {
        let o = owned(&state, &headers, Action::NestView, &nest).await?;
        let (status, runtime) = state
            .nests
            .call(reqwest::Method::GET, &format!("/v1/nests/{nest}"), None)
            .await
            .map_err(nestd_err)?;
        if status != 200 {
            return Err(GErr::new(StatusCode::BAD_GATEWAY, "nestd_failed", format!("the Nests host answered {status}")));
        }
        let (acct, n, funding) = (o.account.clone(), nest.clone(), o.funding.clone());
        let now = now_unix();
        let (stored, package): (i64, Option<Value>) = state
            .store
            .group_read(move |c| -> GResult<(i64, Option<Value>)> {
                let stored: i64 = c.query_row(
                    "SELECT COALESCE(SUM(bytes),0) FROM nest_files WHERE nest_id = ?1 AND deleted_at IS NULL",
                    [&n],
                    |r| r.get(0),
                )?;
                let package = if funding == "package" {
                    nest_funding::allowance(c, &acct, now)?
                        .funding
                        .and_then(|f| nest_catalog::by_code(f.package))
                        .map(|p| {
                            json!({
                                "code": p.code,
                                "active_hours_pooled": p.active_hours,
                                "storage_bytes_per_nest": p.per_nest_storage_bytes(),
                            })
                        })
                } else {
                    None
                };
                Ok((stored, package))
            })
            .await?;
        let u = &runtime["usage"];
        Ok::<_, GErr>(
            Json(json!({
                "nest_id": nest,
                "period": u["period"],
                "input_tokens": { "used": u["input_tokens"]["used"], "limit": u["input_tokens"]["limit"] },
                "output_tokens": { "used": u["output_tokens"]["used"], "limit": u["output_tokens"]["limit"] },
                "requests": u["requests"],
                "running_secs": u["running_secs"],
                "files_bytes": stored,
                "disk": runtime["disk"],
                "package": package,
            }))
            .into_response(),
        )
    }
    .await;
    result.unwrap_or_else(|e| e.into_response())
}

/// `GET /v1/nests/{nest}/export` — a zip built by the Nests host from the Nest's own volume.
async fn export(
    State(state): State<AppState>,
    headers: HeaderMap,
    Path(nest): Path<String>,
) -> Response {
    let result = async {
        let o = owned(&state, &headers, Action::NestOwn, &nest).await?;
        let (status, bytes) = state
            .nests
            .get_bytes(&format!("/v1/nests/{nest}/export"))
            .await
            .map_err(nestd_err)?;
        match status {
            200 if bytes.starts_with(b"PK") => {}
            413 => {
                return Err(GErr::new(
                    StatusCode::PAYLOAD_TOO_LARGE,
                    "export_too_large",
                    "this Nest is larger than one download; contact support",
                ))
            }
            s => {
                return Err(GErr::new(
                    StatusCode::BAD_GATEWAY,
                    "nestd_failed",
                    format!("the Nests host answered {s}"),
                ))
            }
        }
        let name: String = o
            .label
            .chars()
            .filter(|c| c.is_ascii_alphanumeric() || *c == '-' || *c == '_')
            .take(40)
            .collect();
        let mut resp = (StatusCode::OK, bytes).into_response();
        let h = resp.headers_mut();
        h.insert(
            header::CONTENT_TYPE,
            HeaderValue::from_static("application/zip"),
        );
        h.insert(header::CACHE_CONTROL, HeaderValue::from_static("no-store"));
        if let Ok(v) = HeaderValue::from_str(&format!(
            "attachment; filename=\"nest-{}.zip\"",
            if name.is_empty() {
                "export".into()
            } else {
                name
            }
        )) {
            h.insert(header::CONTENT_DISPOSITION, v);
        }
        Ok::<_, GErr>(resp)
    }
    .await;
    result.unwrap_or_else(|e| e.into_response())
}
