//! Feature flags and `GET /v1/capabilities` (SDS §22.5).
//!
//! Every later feature ships behind a flag named here. Clients ask `/v1/capabilities` what this
//! server will do for them before showing a feature, so a client released ahead of its server (or
//! talking to an older one) degrades instead of failing.

use crate::policy::{authorize, Action, Actor, Facts, Resource};
use crate::principal::{credential_kind, require_human, require_step_up, AccountCredential};
use crate::{bearer, now_unix, principal_for_token, ApiError, AppState, Principal};
use axum::extract::State;
use axum::http::HeaderMap;
use axum::response::{IntoResponse, Response};
use axum::Json;
use serde_json::json;

/// The member API version. Bumped when `/v1` gains something a client may want to detect; never
/// for a breaking change (that would be `/v2`).
pub(crate) const API_VERSION: &str = "1.1";

/// Flags this server understands, with what each switches on. A flag not listed here cannot be
/// set, so a typo is an error rather than a silently inert row.
pub(crate) const KNOWN_FEATURES: &[(&str, &str)] = &[
    (
        ACCOUNT_EVENTS,
        "Typed events, durable resume and resync_required on /v1/events",
    ),
    (
        MESSAGE_FILTERING,
        "Filtering settings applied at delivery, the Filtered list and accept-from-Filtered",
    ),
    (
        CALL_ADMISSION,
        "The calls service asks the postbox before ringing (blocks, filters, rate limits)",
    ),
    (
        CALL_HISTORY,
        "Call history: recorded calls, /v1/calls/log and call.log events",
    ),
    (
        AVATARS,
        "Profile pictures: upload, visibility, lookup and profile.avatar events",
    ),
    (
        GROUPS,
        "End-to-end-encrypted groups: /v1/groups, /v1/mls, group.* events (SDS §4)",
    ),
    (
        DISAPPEARING,
        "Disappearing-message timers for threads and groups, message.expired (SDS §17)",
    ),
    (
        CALL_PUSH,
        "Incoming calls pushed to the callee's devices (VoIP, FCM, Mac alert, WNS)",
    ),
];

/// Incoming-call pushes (Phase 1c, docs/call-alerting.md).
pub(crate) const CALL_PUSH: &str = "call_push";

/// Typed events on `/v1/events`.
pub(crate) const ACCOUNT_EVENTS: &str = "account_events";
/// Message filtering at delivery (SDS §13.4).
pub(crate) const MESSAGE_FILTERING: &str = "message_filtering";
/// Call admission (SDS §13.3).
pub(crate) const CALL_ADMISSION: &str = "call_admission";
/// Call history (SDS §12). Kept equal to `store::calllog::CALL_HISTORY_FLAG`.
pub(crate) const CALL_HISTORY: &str = "call_history";
/// Profile pictures (SDS §16).
pub(crate) const AVATARS: &str = "avatars";
/// Groups (SDS §4).
pub(crate) const GROUPS: &str = "groups";
/// Disappearing messages (SDS §17).
pub(crate) const DISAPPEARING: &str = "disappearing";

/// Typed event names the account stream may emit when `account_events` is on.
pub(crate) const EVENT_TYPES: &[&str] = &[
    "features.changed",
    "resync_required",
    "call.log",
    "profile.avatar",
    "group.message",
    "group.handshake",
    "group.member",
    "group.settings",
    "group.deleted",
    "group.read",
    "group.crypto_pending",
    "group.device",
    "mls.welcome",
    "mls.key_packages_low",
    "message.expired",
    "thread.settings",
];

pub(crate) fn known_names() -> Vec<&'static str> {
    KNOWN_FEATURES.iter().map(|(name, _)| *name).collect()
}

/// The account a principal belongs to, if any (an anonymous `/k/` mailbox has none).
pub(crate) fn principal_account(principal: &Principal) -> Option<&str> {
    match principal {
        Principal::Account(account, _) => Some(account),
        Principal::Identity(identity) => identity.account_id.as_deref(),
    }
}

/// `GET /v1/capabilities` — what this server enables for the caller, and how it signed in.
pub(crate) async fn capabilities(State(state): State<AppState>, headers: HeaderMap) -> Response {
    let principal = match principal_for_token(&state, bearer(&headers)).await {
        Ok(p) => p,
        Err(e) => return e.into_response(),
    };
    let account = principal_account(&principal).map(str::to_string);
    let now = now_unix();
    if let Some(account) = account.as_deref() {
        let facts = Facts {
            now,
            ..Facts::default()
        };
        if let Err(deny) = authorize(
            &Actor::Principal(&principal),
            Action::ReadCapabilities,
            &Resource::Account(account),
            &facts,
        ) {
            return deny.into_response();
        }
    }
    let features = match state.store.enabled_features(account, known_names()).await {
        Ok(on) => on,
        Err(e) => {
            tracing::error!(error = %e, "capabilities read failed");
            return ApiError::server("store_error").into_response();
        }
    };
    let event_types: &[&str] = if features.contains(&ACCOUNT_EVENTS) {
        EVENT_TYPES
    } else {
        &[]
    };

    let step_up = &state.step_up;
    let session = require_human(&principal).ok();
    let satisfied = require_step_up(&principal, step_up.max_age, &step_up.acr_values, now).is_ok();
    let client = match &principal {
        Principal::Account(_, AccountCredential::Connector { client }) => Some(client.clone()),
        _ => session.and_then(|s| s.client.clone()),
    };
    Json(json!({
        "api_version": API_VERSION,
        "server_version": env!("CARGO_PKG_VERSION"),
        "features": features,
        "event_types": event_types,
        "credential": {
            "kind": credential_kind(&principal),
            "human": session.is_some(),
            "client": client,
            "issuer": session.map(|s| s.issuer.clone()),
            "audience": session.map(|s| s.audience.clone()),
            "auth_time": session.and_then(|s| s.auth_time),
            "acr": session.and_then(|s| s.acr.clone()),
            "amr": session.map(|s| s.amr.clone()),
            "step_up": {
                "configured": step_up.configured(),
                "satisfied": satisfied,
                "acr_values": step_up.acr_values,
                "max_age": step_up.max_age,
            },
        },
    }))
    .into_response()
}
