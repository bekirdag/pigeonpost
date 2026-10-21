//! Call history endpoints (SDS §12.4) and the calls service's lifecycle reports (§12.2).

use crate::capabilities;
use crate::filtering::{feature_on, mailbox_for};
use crate::policy::Action;
use crate::store::{self, CallEvent, CallEventKind, LogFilter};
use crate::{now_unix, ApiError, AppState};
use axum::extract::{Path, Query, State};
use axum::http::{HeaderMap, StatusCode};
use axum::response::{IntoResponse, Response};
use axum::Json;
use serde_json::json;

const MAX_PAGE: usize = 200;

/// Refuse while the feature is off for this mailbox's account, the way an unknown route answers.
async fn require_flag(state: &AppState, me: &store::StoredIdentity) -> Result<(), ApiError> {
    if feature_on(state, me.account_id.as_deref(), capabilities::CALL_HISTORY).await {
        Ok(())
    } else {
        Err(ApiError::new(
            StatusCode::NOT_FOUND,
            "feature_disabled",
            "call history is not enabled for this account",
        ))
    }
}

#[derive(serde::Deserialize)]
pub(crate) struct LogQuery {
    #[serde(default)]
    identity: Option<String>,
    #[serde(default)]
    filter: Option<String>,
    #[serde(default)]
    before: Option<u64>,
    #[serde(default)]
    limit: Option<usize>,
}

/// `GET /v1/calls/log` — one page of history, newest first.
pub(crate) async fn list(
    State(state): State<AppState>,
    headers: HeaderMap,
    Query(q): Query<LogQuery>,
) -> Response {
    let me = match mailbox_for(
        &state,
        &headers,
        q.identity.as_deref(),
        Action::ManageOwnMailbox,
    )
    .await
    {
        Ok((_, me)) => me,
        Err(e) => return e.into_response(),
    };
    if let Err(e) = require_flag(&state, &me).await {
        return e.into_response();
    }
    let filter = match q.filter.as_deref().unwrap_or("all") {
        "all" => LogFilter::All,
        "missed" => LogFilter::Missed,
        "filtered" => LogFilter::Filtered,
        _ => {
            return ApiError::bad("bad_request", "filter must be all, missed or filtered")
                .into_response()
        }
    };
    let limit = q.limit.unwrap_or(50).clamp(1, MAX_PAGE);
    match state
        .store
        .call_log(me.address.clone(), filter, q.before, limit)
        .await
    {
        Ok((entries, unseen)) => {
            let next_before = (entries.len() == limit)
                .then(|| entries.last().map(|e| e.started_at))
                .flatten();
            Json(json!({
                "entries": entries,
                "unseen_missed": unseen,
                "next_before": next_before,
            }))
            .into_response()
        }
        Err(e) => {
            tracing::error!(error = %e, "call log read failed");
            ApiError::server("store_error").into_response()
        }
    }
}

#[derive(serde::Deserialize)]
pub(crate) struct SeenReq {
    up_to: u64,
    #[serde(default)]
    identity: Option<String>,
}

/// `POST /v1/calls/log/seen` — clear the missed-call badge up to a point.
pub(crate) async fn seen(
    State(state): State<AppState>,
    headers: HeaderMap,
    Json(req): Json<SeenReq>,
) -> Response {
    let me = match mailbox_for(
        &state,
        &headers,
        req.identity.as_deref(),
        Action::ManageOwnMailbox,
    )
    .await
    {
        Ok((_, me)) => me,
        Err(e) => return e.into_response(),
    };
    if let Err(e) = require_flag(&state, &me).await {
        return e.into_response();
    }
    match state
        .store
        .mark_calls_seen(me.address.clone(), req.up_to)
        .await
    {
        Ok((updated, unseen)) => {
            Json(json!({ "updated": updated, "unseen_missed": unseen })).into_response()
        }
        Err(_) => ApiError::server("store_error").into_response(),
    }
}

#[derive(serde::Deserialize)]
pub(crate) struct IdentityQuery {
    #[serde(default)]
    identity: Option<String>,
}

/// `DELETE /v1/calls/log/{id}` — delete one entry.
pub(crate) async fn delete_one(
    State(state): State<AppState>,
    headers: HeaderMap,
    Path(id): Path<String>,
    Query(q): Query<IdentityQuery>,
) -> Response {
    let me = match mailbox_for(
        &state,
        &headers,
        q.identity.as_deref(),
        Action::ManageOwnMailbox,
    )
    .await
    {
        Ok((_, me)) => me,
        Err(e) => return e.into_response(),
    };
    if let Err(e) = require_flag(&state, &me).await {
        return e.into_response();
    }
    match state
        .store
        .delete_call_entry(me.address.clone(), id, now_unix())
        .await
    {
        Ok(true) => Json(json!({ "deleted": true })).into_response(),
        // Not yours and not there are the same answer: an id says nothing about other mailboxes.
        Ok(false) => ApiError::new(
            StatusCode::NOT_FOUND,
            "not_found",
            "no such call in your history",
        )
        .into_response(),
        Err(_) => ApiError::server("store_error").into_response(),
    }
}

/// `DELETE /v1/calls/log` — clear the whole history.
pub(crate) async fn clear(
    State(state): State<AppState>,
    headers: HeaderMap,
    Query(q): Query<IdentityQuery>,
) -> Response {
    let me = match mailbox_for(
        &state,
        &headers,
        q.identity.as_deref(),
        Action::ManageOwnMailbox,
    )
    .await
    {
        Ok((_, me)) => me,
        Err(e) => return e.into_response(),
    };
    if let Err(e) = require_flag(&state, &me).await {
        return e.into_response();
    }
    match state
        .store
        .clear_call_log(me.address.clone(), now_unix())
        .await
    {
        Ok(n) => Json(json!({ "deleted": n })).into_response(),
        Err(_) => ApiError::server("store_error").into_response(),
    }
}

/// Report the caller of one incoming call (SDS §13.3): feeds the same reputation as messages.
pub(crate) async fn do_report_call(
    state: &AppState,
    me: &store::StoredIdentity,
    call_id: String,
) -> Result<serde_json::Value, ApiError> {
    let entry = state
        .store
        .call_entry(me.address.clone(), call_id.clone())
        .await
        .map_err(|_| ApiError::server("store_error"))?
        .filter(|e| e.direction == "incoming")
        .ok_or_else(|| {
            ApiError::new(
                StatusCode::NOT_FOUND,
                "not_found",
                "no such incoming call in your history",
            )
        })?;
    let caller = entry
        .peer
        .clone()
        .ok_or_else(|| ApiError::bad("invalid_report", "this call has no caller to report"))?;
    if caller == me.address {
        return Err(ApiError::bad(
            "invalid_report",
            "an inbox cannot report itself",
        ));
    }
    let my_score = crate::sender_score(state, me).await;
    if !crate::reputation::report_counts(my_score) {
        return Err(ApiError::new(
            StatusCode::FORBIDDEN,
            "reporter_not_in_good_standing",
            "this inbox's own standing is too low for its reports to count",
        ));
    }
    // Keyed by call, so one call cannot be charged twice however often it is reported.
    let counted = state
        .store
        .record_spam_report(
            format!("call:{}", entry.id),
            me.address.clone(),
            caller.clone(),
            now_unix(),
        )
        .await
        .map_err(|_| ApiError::server("store_error"))?;
    let reports = state
        .store
        .reports_against(caller.clone())
        .await
        .unwrap_or(0);
    tracing::info!(reporter = %me.address, %caller, counted, reports, "spam call reported");
    Ok(json!({
        "reported": counted,
        "sender": caller,
        "reports_against_sender": reports,
        "detail": if counted { "report recorded" } else { "this call was already reported" },
    }))
}

// ---- internal: lifecycle events ----------------------------------------------------------------

#[derive(serde::Deserialize)]
pub(crate) struct EventFrom {
    address: String,
}

#[derive(serde::Deserialize)]
pub(crate) struct EventReq {
    event_id: String,
    room_id: String,
    #[serde(default)]
    seq: i64,
    kind: String,
    event: String,
    from: EventFrom,
    #[serde(default)]
    callee: Option<String>,
    #[serde(default)]
    to: Vec<String>,
    #[serde(default)]
    by: Option<String>,
    #[serde(default)]
    participants: Vec<String>,
    at: u64,
    /// Set for a group call (Phase 3a): start and end become notices in the group.
    #[serde(default)]
    group_id: Option<String>,
    /// Set for a scheduled meeting room (Phase 3B).
    #[serde(default)]
    calendar_event_id: Option<String>,
    #[serde(default)]
    occurrence_id: Option<String>,
}

fn token_ok(s: &str, max: usize) -> bool {
    !s.is_empty()
        && s.len() <= max
        && s.bytes()
            .all(|b| b.is_ascii_alphanumeric() || b"-_.".contains(&b))
}

fn name_ok(s: &str) -> bool {
    s.len() <= 300 && s.starts_with('/') && !s.chars().any(char::is_control)
}

impl EventReq {
    fn validate(self, now: u64) -> Result<CallEvent, &'static str> {
        if !token_ok(&self.event_id, 128) || !token_ok(&self.room_id, 128) {
            return Err("event_id and room_id must be short tokens");
        }
        if self
            .calendar_event_id
            .as_deref()
            .is_some_and(|e| !token_ok(e, 64))
            || self
                .occurrence_id
                .as_deref()
                .is_some_and(|o| !token_ok(o, 80))
        {
            return Err("calendar ids must be short tokens");
        }
        if !matches!(self.kind.as_str(), "audio" | "video") {
            return Err("kind must be audio or video");
        }
        let event = CallEventKind::parse(&self.event).ok_or("unknown event")?;
        if !name_ok(&self.from.address)
            || self.callee.as_deref().is_some_and(|c| !name_ok(c))
            || self.by.as_deref().is_some_and(|b| !name_ok(b))
            || self.to.len() > 50
            || self.to.iter().any(|t| !name_ok(t))
            || self.participants.len() > 100
        {
            return Err("bad address");
        }
        // A timestamp from the far future would pin an entry to the top of the list for ever.
        if self.at > now + 300 {
            return Err("at is in the future");
        }
        let participants = self
            .participants
            .into_iter()
            .filter_map(|p| {
                if name_ok(&p) {
                    Some(p)
                } else {
                    let name: String = p
                        .strip_prefix("guest:")?
                        .chars()
                        .filter(|c| !c.is_control())
                        .take(60)
                        .collect();
                    (!name.trim().is_empty()).then(|| format!("guest:{}", name.trim()))
                }
            })
            .collect();
        Ok(CallEvent {
            event_id: self.event_id,
            room_id: self.room_id,
            seq: self.seq,
            kind: self.kind,
            event,
            from: self.from.address,
            callee: self.callee,
            to: self.to,
            by: self.by,
            participants,
            at: self.at,
            calendar_event_id: self.calendar_event_id,
            occurrence_id: self.occurrence_id,
        })
    }
}

/// `POST /internal/v1/call-events` — fold one lifecycle event into everyone's history.
pub(crate) async fn ingest(
    State(state): State<AppState>,
    headers: HeaderMap,
    body: Result<Json<EventReq>, axum::extract::rejection::JsonRejection>,
) -> Response {
    if let Err(resp) = crate::internal_api::authorize_service(
        &state,
        &headers,
        crate::internal_api::Endpoint::CallEvents,
        Action::CallEvents,
    ) {
        return *resp;
    }
    let Ok(Json(req)) = body else {
        return ApiError::bad("bad_request", "malformed call event").into_response();
    };
    let now = now_unix();
    let group_call = req.group_id.clone().map(|g| {
        (
            g,
            req.event.clone(),
            req.room_id.clone(),
            req.from.address.clone(),
        )
    });
    let ev = match req.validate(now) {
        Ok(ev) => ev,
        Err(why) => return ApiError::bad("bad_request", why).into_response(),
    };
    // Rows are written only for mailboxes whose account has `call_history` on (checked per owner
    // inside the fold), so one call can be recorded on one side and not the other.
    match state.store.apply_call_event(ev, now).await {
        Ok(result) => {
            for (account, _) in &result.changed {
                state.event_hub.wake(account);
            }
            if let Some((group, event, room, from)) = group_call {
                let kind = match event.as_str() {
                    "ring" => Some("call_started"),
                    "end" => Some("call_ended"),
                    _ => None,
                };
                if let Some(kind) = kind {
                    crate::mls::call_notice(&state, &group, kind, &room, Some(&from)).await;
                }
            }
            Json(json!({ "accepted": true, "duplicate": result.duplicate })).into_response()
        }
        Err(e) => {
            tracing::error!(error = %e, "call event fold failed");
            ApiError::server("store_error").into_response()
        }
    }
}
