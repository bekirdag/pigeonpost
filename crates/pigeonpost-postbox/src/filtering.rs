//! Call and message filtering (SDS §13): the decisions, the settings endpoints, accepting a sender
//! out of Filtered, and call admission for the calls service.
//!
//! Every decision is made here, on the server, so every device and every client agrees on it. The
//! decision functions are pure — facts in, rule out — and the async code around them only gathers
//! facts from the store.
//!
//! What a refused party learns is fixed and deliberately uninformative: a blocked or filtered
//! message is refused (block) with the same `not_admitted` a closed inbox gives, or accepted
//! (filter) exactly as if it had been delivered; a blocked or silenced call rings out as an
//! ordinary unanswered one.

use crate::capabilities;
use crate::policy::{authorize, Action, Actor, Facts, Resource};
use crate::store::{
    self, CallRule, Filters, FiltersUpdate, MessageRule, ReputationRule, CALL_HISTORY_CHOICES,
};
use crate::{
    bearer, now_unix, principal_for_token, reputation, resolve_acting_identity, sender_score,
    ApiError, AppState, Principal, TrustActor,
};
use axum::extract::{Query, State};
use axum::http::{HeaderMap, StatusCode};
use axum::response::{IntoResponse, Response};
use axum::Json;
use serde_json::json;
use std::collections::{HashMap, VecDeque};
use std::sync::Mutex;

/// Whether a flag is on for an account (or globally, for a mailbox with no account).
pub(crate) async fn feature_on(
    state: &AppState,
    account: Option<&str>,
    name: &'static str,
) -> bool {
    state
        .store
        .enabled_features(account.map(str::to_string), vec![name])
        .await
        .map(|on| on.contains(&name))
        .unwrap_or(false)
}

// ---- decisions ---------------------------------------------------------------------------------

/// What happens to one message, given the recipient's settings and how they stand with the sender.
///
/// Rules combine by strictness (`block > filter > show`). Somebody the recipient knows is exempt
/// from all three filters: knowing someone is exactly what the filters ask about. A block is not
/// decided here; it refused the message before this was asked.
pub(crate) fn message_rule(
    filters: &Filters,
    rel: &store::Relationship,
    unnamed: bool,
    sender_score: i64,
) -> MessageRule {
    if rel.known() {
        return MessageRule::Show;
    }
    let mut rule = filters.unknown_messages;
    if unnamed {
        rule = rule.max(filters.unnamed_messages);
    }
    if sender_score < 0 && filters.low_reputation == ReputationRule::Filter {
        rule = rule.max(MessageRule::Filter);
    }
    rule
}

/// Facts about one caller and one callee, gathered before deciding.
#[derive(Debug, Clone, Copy, Default)]
pub(crate) struct CallFacts {
    pub blocked: bool,
    pub known: bool,
    /// The callee has no contact row for the caller and closed its inbox to strangers.
    pub closed_to_strangers: bool,
    pub unnamed: bool,
    pub caller_score: i64,
    /// The caller is over the per-pair attempt limit for this callee.
    pub pair_limited: bool,
}

/// Why a call was not put straight through. Coarse on purpose: it is logged by the calls service and
/// never shown to the caller.
pub(crate) fn call_rule(filters: &Filters, f: &CallFacts) -> (CallRule, &'static str) {
    // Mandatory first, and for everyone, known or not (§13.3 step 2).
    if f.blocked {
        return (CallRule::Block, "blocked");
    }
    if reputation::standing(f.caller_score) == "reported_repeatedly" && !f.known {
        return (CallRule::Block, "caller_standing");
    }
    let mut rule = (CallRule::Ring, "ok");
    let mut raise = |next: CallRule, why: &'static str| {
        if next > rule.0 {
            rule = (next, why);
        }
    };
    if f.pair_limited {
        raise(CallRule::Silence, "pair_rate");
    }
    if !f.known {
        if f.closed_to_strangers {
            raise(CallRule::Block, "closed_inbox");
        }
        raise(filters.unknown_calls, "unknown");
        if f.unnamed {
            raise(filters.unnamed_calls, "unnamed");
        }
        if f.caller_score < 0 && filters.low_reputation == ReputationRule::Filter {
            raise(CallRule::Silence, "low_reputation");
        }
    }
    rule
}

// ---- per-pair call attempts --------------------------------------------------------------------

/// Ring attempts per caller→callee pair, in memory. A restart forgets them, which costs a
/// harasser at most one extra window.
pub(crate) struct PairLimiter {
    attempts: Mutex<HashMap<(String, String), VecDeque<u64>>>,
}

/// Attempts allowed in a short burst and over an hour before further calls are silenced.
const PAIR_BURST: (usize, u64) = (3, 120);
const PAIR_HOURLY: (usize, u64) = (10, 3_600);
const PAIR_TRACKED: usize = 20_000;

impl PairLimiter {
    pub(crate) fn new() -> Self {
        PairLimiter {
            attempts: Mutex::new(HashMap::new()),
        }
    }

    /// Record an attempt and say whether the pair is now over a limit.
    pub(crate) fn attempt(&self, caller: &str, callee: &str, now: u64) -> bool {
        let mut map = self.attempts.lock().expect("pair limiter");
        if map.len() >= PAIR_TRACKED {
            map.retain(|_, v| {
                v.back()
                    .is_some_and(|t| now.saturating_sub(*t) < PAIR_HOURLY.1)
            });
        }
        let q = map
            .entry((caller.to_string(), callee.to_string()))
            .or_default();
        while q
            .front()
            .is_some_and(|t| now.saturating_sub(*t) >= PAIR_HOURLY.1)
        {
            q.pop_front();
        }
        let burst = q
            .iter()
            .filter(|t| now.saturating_sub(**t) < PAIR_BURST.1)
            .count();
        let over = burst >= PAIR_BURST.0 || q.len() >= PAIR_HOURLY.0;
        q.push_back(now);
        over
    }
}

// ---- settings -----------------------------------------------------------------------------------

pub(crate) fn filters_json(f: &Filters) -> serde_json::Value {
    json!({
        "unknown_calls": f.unknown_calls.as_str(),
        "unnamed_calls": f.unnamed_calls.as_str(),
        "unnamed_messages": f.unnamed_messages.as_str(),
        "unknown_messages": f.unknown_messages.as_str(),
        "low_reputation": f.low_reputation.as_str(),
        "group_members_known": f.group_members_known,
        "call_history_days": f.call_history_days,
    })
}

/// The whole policy object: the stranger-defaults plus the filters.
pub(crate) fn full_policy_json(p: store::InboxPolicy, f: &Filters) -> serde_json::Value {
    let mut v = crate::policy_json(p);
    if let (Some(obj), serde_json::Value::Object(extra)) = (v.as_object_mut(), filters_json(f)) {
        obj.extend(extra);
    }
    v
}

/// The filter fields of a `PUT /v1/policy` body.
#[derive(Debug, Default, serde::Deserialize)]
pub(crate) struct FilterFields {
    #[serde(default)]
    unknown_calls: Option<String>,
    #[serde(default)]
    unnamed_calls: Option<String>,
    #[serde(default)]
    unnamed_messages: Option<String>,
    #[serde(default)]
    unknown_messages: Option<String>,
    #[serde(default)]
    low_reputation: Option<String>,
    #[serde(default)]
    group_members_known: Option<bool>,
    #[serde(default)]
    call_history_days: Option<u32>,
}

impl FilterFields {
    pub(crate) fn parse(&self) -> Result<FiltersUpdate, ApiError> {
        fn bad(field: &str, allowed: &str) -> ApiError {
            ApiError::bad(
                "invalid_policy",
                &format!("{field} must be one of {allowed}"),
            )
        }
        let call = |v: &Option<String>, field: &str| -> Result<Option<CallRule>, ApiError> {
            v.as_deref()
                .map(|s| CallRule::parse(s).ok_or_else(|| bad(field, "ring, silence, block")))
                .transpose()
        };
        let msg = |v: &Option<String>, field: &str| -> Result<Option<MessageRule>, ApiError> {
            v.as_deref()
                .map(|s| MessageRule::parse(s).ok_or_else(|| bad(field, "show, filter, block")))
                .transpose()
        };
        Ok(FiltersUpdate {
            unknown_calls: call(&self.unknown_calls, "unknown_calls")?,
            unnamed_calls: call(&self.unnamed_calls, "unnamed_calls")?,
            unnamed_messages: msg(&self.unnamed_messages, "unnamed_messages")?,
            unknown_messages: msg(&self.unknown_messages, "unknown_messages")?,
            low_reputation: self
                .low_reputation
                .as_deref()
                .map(|s| {
                    ReputationRule::parse(s).ok_or_else(|| bad("low_reputation", "filter, allow"))
                })
                .transpose()?,
            group_members_known: self.group_members_known,
            call_history_days: match self.call_history_days {
                Some(d) if !CALL_HISTORY_CHOICES.contains(&d) => {
                    return Err(bad("call_history_days", "0, 30, 180, 365"))
                }
                d => d,
            },
        })
    }
}

/// Apply a filter update for `me`. Agents may only tighten. Turning call history off purges it.
pub(crate) async fn do_set_filters(
    state: &AppState,
    me: &store::StoredIdentity,
    update: FiltersUpdate,
    actor: TrustActor,
) -> Result<Filters, ApiError> {
    let current = state
        .store
        .filters(me.address.clone())
        .await
        .map_err(|_| ApiError::server("store_error"))?;
    if update.is_empty() {
        return Ok(current);
    }
    if actor == TrustActor::Agent {
        if let Some(field) = update.loosens(&current) {
            return Err(ApiError::new(
                StatusCode::FORBIDDEN,
                "human_required",
                format!("an agent may make {field} stricter, never looser"),
            ));
        }
    }
    let now = now_unix();
    let next = state
        .store
        .set_filters(me.address.clone(), update, now)
        .await
        .map_err(|e| {
            tracing::error!(error = %e, "filters write failed");
            ApiError::server("store_error")
        })?;
    if next.call_history_days == 0 && current.call_history_days != 0 {
        if let Err(e) = state.store.clear_call_log(me.address.clone(), now).await {
            tracing::error!(error = %e, "call history purge failed");
        }
    }
    tracing::info!(address = %me.address, ?actor, "filtering settings set");
    Ok(next)
}

#[derive(serde::Deserialize)]
pub(crate) struct IdentityQuery {
    #[serde(default)]
    identity: Option<String>,
}

/// The acting mailbox, authorized for `action` through the policy.
pub(crate) async fn mailbox_for(
    state: &AppState,
    headers: &HeaderMap,
    identity: Option<&str>,
    action: Action,
) -> Result<(Principal, store::StoredIdentity), ApiError> {
    let principal = principal_for_token(state, bearer(headers)).await?;
    // The selection logic stays the existing one; the decision comes from the policy over the
    // stored binding, never from the request.
    let account_view = match &principal {
        Principal::Account(account, credential) => Some((account.clone(), credential.clone())),
        Principal::Identity(_) => None,
    };
    let me = resolve_acting_identity(state, principal, identity).await?;
    let principal = match account_view {
        Some((account, credential)) => Principal::Account(account, credential),
        None => Principal::Identity(me.clone()),
    };
    let facts = Facts {
        now: now_unix(),
        ..Facts::default()
    };
    authorize(
        &Actor::Principal(&principal),
        action,
        &Resource::Mailbox {
            address: &me.address,
            account: me.account_id.as_deref(),
        },
        &facts,
    )
    .map_err(|deny| {
        ApiError::new(
            StatusCode::FORBIDDEN,
            deny.code(),
            "this credential may not perform this action",
        )
    })?;
    Ok((principal, me))
}

/// `GET /v1/policy` — the acting mailbox's whole policy.
pub(crate) async fn get_policy(
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
    let (Ok(p), Ok(f)) = (
        state.store.inbox_policy(me.address.clone()).await,
        state.store.filters(me.address.clone()).await,
    ) else {
        return ApiError::server("store_error").into_response();
    };
    Json(full_policy_json(p, &f)).into_response()
}

// ---- accept from Filtered ----------------------------------------------------------------------

#[derive(serde::Deserialize)]
pub(crate) struct AcceptReq {
    peer: String,
    #[serde(default)]
    identity: Option<String>,
}

/// `POST /v1/filtered/accept` — add the sender as an allowed contact and move their filtered mail
/// into the inbox.
pub(crate) async fn accept_filtered(
    State(state): State<AppState>,
    headers: HeaderMap,
    Json(req): Json<AcceptReq>,
) -> Response {
    let me = match mailbox_for(
        &state,
        &headers,
        req.identity.as_deref(),
        Action::RaiseTrust,
    )
    .await
    {
        Ok((_, me)) => me,
        Err(e) => return e.into_response(),
    };
    let sender = match crate::resolve_recipient(&state, req.peer.trim()).await {
        Ok(Some(found)) => found,
        Ok(None) => {
            return ApiError::new(StatusCode::NOT_FOUND, "not_found", "no such sender")
                .into_response()
        }
        Err(e) => return e.into_response(),
    };
    // Trusted by name when it has one, as every other admission check matches it.
    let subject = sender
        .handle
        .clone()
        .unwrap_or_else(|| sender.address.clone());
    let contact = match crate::do_set_contact(
        &state,
        &me,
        subject,
        None,
        Some("allow".into()),
        None,
        None,
        TrustActor::Human,
    )
    .await
    {
        Ok(c) => c,
        Err(e) => return e.into_response(),
    };
    // An exact block on the key address would still refuse them; accepting clears it.
    if sender.handle.is_some() {
        if let Ok(Some(row)) = state
            .store
            .contact(me.address.clone(), sender.address.clone())
            .await
        {
            if row.admission == "block" {
                let _ = state
                    .store
                    .delete_contact(me.address.clone(), sender.address.clone())
                    .await;
            }
        }
    }
    let moved = match state
        .store
        .unfilter_from(me.address.clone(), sender.address.clone())
        .await
    {
        Ok(n) => n,
        Err(e) => {
            tracing::error!(error = %e, "unfilter failed");
            return ApiError::server("store_error").into_response();
        }
    };
    if moved > 0 {
        state.inbox_signal.notify_waiters();
        if let Some(account) = me.account_id.as_deref() {
            state.event_hub.wake(account);
        }
    }
    Json(json!({ "accepted": true, "contact": contact, "moved": moved })).into_response()
}

// ---- call admission (internal) -----------------------------------------------------------------

#[derive(serde::Deserialize)]
pub(crate) struct AdmissionFrom {
    mailbox: String,
}

#[derive(serde::Deserialize)]
pub(crate) struct AdmissionReq {
    from: AdmissionFrom,
    to: String,
    kind: String,
    #[serde(default)]
    #[allow(dead_code)]
    group_id: Option<String>,
}

/// Decide one call: every target independently, combined `block > silence > ring`.
pub(crate) async fn decide_call(
    state: &AppState,
    caller: &store::StoredIdentity,
    callee: &store::StoredIdentity,
    now: u64,
) -> Result<(CallRule, &'static str), ApiError> {
    let pair_limited = state
        .call_pairs
        .attempt(&caller.address, &callee.address, now);
    decide_call_with(state, caller, callee, pair_limited).await
}

/// [`decide_call`] without counting an attempt: the push fan-out (Phase 1c, `call_alerts`) asks
/// again after admission already counted this ring.
pub(crate) async fn decide_call_with(
    state: &AppState,
    caller: &store::StoredIdentity,
    callee: &store::StoredIdentity,
    pair_limited: bool,
) -> Result<(CallRule, &'static str), ApiError> {
    let se = |_| ApiError::server("store_error");
    let rel = state
        .store
        .relationship(
            callee.address.clone(),
            caller.address.clone(),
            caller.handle.clone(),
        )
        .await
        .map_err(se)?;
    let enabled = feature_on(
        state,
        callee.account_id.as_deref(),
        capabilities::CALL_ADMISSION,
    )
    .await;
    if !enabled {
        // Before the flag, calls rang as they always have — except that a block was already a
        // block for messages, and a person who blocked someone expects it to hold for calls too.
        return Ok(if rel.blocked {
            (CallRule::Block, "blocked")
        } else {
            (CallRule::Ring, "flag_off")
        });
    }
    let filters = state
        .store
        .filters(callee.address.clone())
        .await
        .map_err(se)?;
    let policy = state
        .store
        .inbox_policy(callee.address.clone())
        .await
        .map_err(se)?;
    let facts = CallFacts {
        blocked: rel.blocked,
        known: rel.known(),
        closed_to_strangers: !rel.allowed && !policy.accept_all,
        unnamed: caller.handle.is_none(),
        caller_score: sender_score(state, caller).await,
        pair_limited,
    };
    Ok(call_rule(&filters, &facts))
}

/// `POST /internal/v1/call-admission` — the calls service asks before it rings.
pub(crate) async fn call_admission(
    State(state): State<AppState>,
    headers: HeaderMap,
    body: Result<Json<AdmissionReq>, axum::extract::rejection::JsonRejection>,
) -> Response {
    if let Err(resp) = crate::internal_api::authorize_service(
        &state,
        &headers,
        crate::internal_api::Endpoint::CallAdmission,
        Action::CallAdmission,
    ) {
        return *resp;
    }
    let Ok(Json(req)) = body else {
        return ApiError::bad("bad_request", "expected {from:{mailbox}, to, kind}").into_response();
    };
    if !matches!(req.kind.as_str(), "audio" | "video") || req.to.len() > 300 {
        return ApiError::bad("bad_request", "kind must be audio or video").into_response();
    }
    let now = now_unix();
    let block = |reason: &'static str, targets: Vec<String>| {
        Json(json!({ "decision": "block", "targets": targets, "reason": reason })).into_response()
    };
    // 1. The caller must be a live mailbox here. The calls service proved the member owns it.
    let caller = match state.store.get(req.from.mailbox.clone()).await {
        Ok(Some(c)) => c,
        Ok(None) => return block("caller_invalid", Vec::new()),
        Err(_) => return ApiError::server("store_error").into_response(),
    };
    // A mailbox, by address or handle. A namespace or an unknown name rings nobody, which is what
    // the calls service did before admission existed.
    let callee = match crate::resolve_recipient(&state, req.to.trim()).await {
        Ok(Some(c)) if req.to.trim().starts_with("/k/") || c.handle.is_some() => c,
        Ok(_) => {
            return Json(json!({ "decision": "ring", "targets": [], "reason": "no_target" }))
                .into_response()
        }
        Err(e) => return e.into_response(),
    };
    if callee.address == caller.address {
        return block("self", Vec::new());
    }
    match decide_call(&state, &caller, &callee, now).await {
        Ok((rule, reason)) => {
            tracing::info!(decision = rule.as_str(), reason, "call admission decided");
            Json(json!({
                "decision": rule.as_str(),
                "targets": [callee.address],
                "reason": reason,
            }))
            .into_response()
        }
        Err(e) => e.into_response(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use store::Relationship;

    fn stranger() -> Relationship {
        Relationship::default()
    }

    #[test]
    fn known_senders_are_never_filtered_and_rules_combine_by_strictness() {
        let mut f = Filters::default();
        let known = Relationship {
            allowed: true,
            ..Relationship::default()
        };
        let wrote_first = Relationship {
            messaged: true,
            ..Relationship::default()
        };
        f.unknown_messages = MessageRule::Block;
        assert_eq!(message_rule(&f, &known, true, -100), MessageRule::Show);
        assert_eq!(
            message_rule(&f, &wrote_first, true, -100),
            MessageRule::Show
        );
        assert_eq!(message_rule(&f, &stranger(), false, 0), MessageRule::Block);

        let f = Filters {
            unnamed_messages: MessageRule::Filter,
            ..Filters::default()
        };
        assert_eq!(message_rule(&f, &stranger(), true, 0), MessageRule::Filter);
        assert_eq!(message_rule(&f, &stranger(), false, 0), MessageRule::Show);
        // Low reputation filters by default, and "allow" lifts it.
        assert_eq!(
            message_rule(&Filters::default(), &stranger(), false, -25),
            MessageRule::Filter
        );
        let allow = Filters {
            low_reputation: ReputationRule::Allow,
            ..Filters::default()
        };
        assert_eq!(
            message_rule(&allow, &stranger(), false, -25),
            MessageRule::Show
        );
        // Defaults deliver an ordinary stranger as before.
        assert_eq!(
            message_rule(&Filters::default(), &stranger(), true, 0),
            MessageRule::Show
        );
    }

    #[test]
    fn call_decisions() {
        let d = Filters::default();
        let named_stranger = CallFacts {
            caller_score: 20,
            ..CallFacts::default()
        };
        assert_eq!(call_rule(&d, &named_stranger).0, CallRule::Ring);
        // Unnamed strangers are silenced by default.
        let unnamed = CallFacts {
            unnamed: true,
            ..named_stranger
        };
        assert_eq!(call_rule(&d, &unnamed), (CallRule::Silence, "unnamed"));
        // A known caller rings even unnamed…
        let known = CallFacts {
            known: true,
            ..unnamed
        };
        assert_eq!(call_rule(&d, &known).0, CallRule::Ring);
        // …but known never bypasses a block or the per-pair limit.
        assert_eq!(
            call_rule(
                &d,
                &CallFacts {
                    blocked: true,
                    ..known
                }
            ),
            (CallRule::Block, "blocked")
        );
        assert_eq!(
            call_rule(
                &d,
                &CallFacts {
                    pair_limited: true,
                    ..known
                }
            )
            .0,
            CallRule::Silence
        );
        // Block beats silence when both apply.
        let strict = Filters {
            unknown_calls: CallRule::Block,
            ..d
        };
        assert_eq!(call_rule(&strict, &unnamed).0, CallRule::Block);
        // Low reputation silences strangers; reported-repeatedly callers reach only people who
        // know them.
        assert_eq!(
            call_rule(
                &d,
                &CallFacts {
                    caller_score: -25,
                    ..named_stranger
                }
            ),
            (CallRule::Silence, "low_reputation")
        );
        assert_eq!(
            call_rule(
                &d,
                &CallFacts {
                    caller_score: -100,
                    ..named_stranger
                }
            )
            .0,
            CallRule::Block
        );
        assert_eq!(
            call_rule(
                &d,
                &CallFacts {
                    caller_score: -100,
                    known: true,
                    ..named_stranger
                }
            )
            .0,
            CallRule::Ring
        );
        // A closed inbox is closed to stranger calls too.
        assert_eq!(
            call_rule(
                &d,
                &CallFacts {
                    closed_to_strangers: true,
                    ..named_stranger
                }
            )
            .0,
            CallRule::Block
        );
    }

    #[test]
    fn the_pair_limiter_silences_bursts_and_hourly_floods() {
        let l = PairLimiter::new();
        assert!(!l.attempt("/k/a", "/k/b", 0));
        assert!(!l.attempt("/k/a", "/k/b", 1));
        assert!(!l.attempt("/k/a", "/k/b", 2));
        assert!(
            l.attempt("/k/a", "/k/b", 3),
            "fourth call inside two minutes"
        );
        // Another pair is unaffected.
        assert!(!l.attempt("/k/a", "/k/c", 3));
        // Spread out, ten an hour.
        let l = PairLimiter::new();
        for i in 0..10 {
            assert!(!l.attempt("/k/a", "/k/b", i * 300), "attempt {i}");
        }
        assert!(l.attempt("/k/a", "/k/b", 3_000));
        assert!(!l.attempt("/k/a", "/k/b", 3_000 + 3_600));
    }
}
