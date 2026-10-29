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
pub(crate) const API_VERSION: &str = "1.2";

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
    (
        ORGS,
        "Organizations: /v1/orgs, invitations, roles, agents, delegations, org rules, org rooms and device login (SDS §6, §7, §9, §10)",
    ),
    (
        ENTERPRISE_PORTAL,
        "Clients may show the enterprise portal's start wizard and dashboard (SDS §5)",
    ),
    (
        ORG_INVITE_EMAIL,
        "Org invitations are delivered through saas_be's pigeonpost-org-invite workflow (SDS §7.2)",
    ),
    (
        CALENDAR,
        "Calendar and scheduled meetings: /v1/calendars, /v1/calendar/*, event.* events (SDS §14B)",
    ),
    (
        MEETING_SUMMARIES,
        "Meeting summaries: consented notes in calls, /v1/meetings, meeting.brief events (SDS §14)",
    ),
    (
        MESSAGE_EDIT,
        "Editing sent direct messages: edit_of on /v1/send, message.edited (SDS §14C)",
    ),
    (
        MESSAGE_REPLY,
        "Replying to a specific direct message: reply_to and quote on /v1/send (SDS §14C)",
    ),
    (
        RECEIPTS,
        "Delivered and seen receipts on sent messages, /v1/receipts, message.receipt (SDS §14C)",
    ),
    (
        AGENT_STATUS,
        "Agent run status (working, done, failed) on sent messages, reported by agentd (SDS §14C)",
    ),
    (
        RECIPIENT_LOOKUP,
        "Canonical recipient lookup before protected native sends: /v1/recipients/resolve",
    ),
    (
        ORG_API,
        "Enterprise API: org API keys, /org/v1 and org webhooks (SDS §18, docs/org-api.md)",
    ),
    (
        STREAMING,
        "Live streams in groups: /v1/groups/{id}/streams, /v1/streams/*, group.stream events (SDS §14A)",
    ),
    (
        AGENT_SSH,
        "SSH to agent workspaces through the tunnel broker: /v1/shell/*, /v1/agents/{address}/shell (SDS §11)",
    ),
    (
        CALENDAR_EMAIL,
        "Calendar invitations, updates, reminders and codes sent to contact addresses (SDS §14B.6)",
    ),
    (
        PHONE_NUMBERS,
        "Phone numbers on mailboxes: SMS-verified, opt-in discoverable (/v1/phone, SDS §14D)",
    ),
    (
        CONTACT_DISCOVERY,
        "Find people I know: hashed contact matching against discoverable numbers (/v1/contacts/discover)",
    ),
    (
        SMS_INVITES,
        "Invite a contact by SMS through saas_be/Novu (/v1/contacts/invites, SDS §14D)",
    ),
    (
        DISPLAY_NAMES,
        "Self-set display names per mailbox (/v1/profile/name, /v1/profiles, profile.name events)",
    ),
];

/// Phone numbers on mailboxes (SDS §14D).
pub(crate) const PHONE_NUMBERS: &str = "phone_numbers";
/// Contact discovery by hashed phone number (SDS §14D).
pub(crate) const CONTACT_DISCOVERY: &str = "contact_discovery";
/// SMS invitations (SDS §14D).
pub(crate) const SMS_INVITES: &str = "sms_invites";
/// Self-set display names (SDS §14D).
pub(crate) const DISPLAY_NAMES: &str = "display_names";

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
/// Live streams in groups (SDS §14A). Needs `groups` too.
pub(crate) const STREAMING: &str = "streaming";
/// Disappearing messages (SDS §17).
pub(crate) const DISAPPEARING: &str = "disappearing";
/// Organizations (SDS §6).
pub(crate) const ORGS: &str = "orgs";
/// The enterprise portal's signed-in surfaces (SDS §5).
pub(crate) const ENTERPRISE_PORTAL: &str = "enterprise_portal";
/// Invitation delivery through saas_be/Novu (SDS §7.2). Off until the sender domain is verified.
pub(crate) const ORG_INVITE_EMAIL: &str = "org_invite_email";
/// Calendar and scheduled meetings (SDS §14B).
pub(crate) const CALENDAR: &str = "calendar";
/// Calendar messages to contact addresses through saas_be/Novu (SDS §14B.6). Separate, because
/// it waits on the sender domain being verified.
pub(crate) const CALENDAR_EMAIL: &str = "calendar_email";

/// Meeting summaries (SDS §14, docs/meeting-summaries.md).
pub(crate) const MEETING_SUMMARIES: &str = "meeting_summaries";

/// Message edits (SDS §14C).
pub(crate) const MESSAGE_EDIT: &str = "message_edit";
/// Replies to a specific message (SDS §14C).
pub(crate) const MESSAGE_REPLY: &str = "message_reply";
/// Delivered/seen receipts (SDS §14C).
pub(crate) const RECEIPTS: &str = "receipts";
/// Agent run status on sent messages (SDS §14C).
pub(crate) const AGENT_STATUS: &str = "agent_status";
/// Canonical recipient lookup before persisting an immutable delivery intent.
pub(crate) const RECIPIENT_LOOKUP: &str = "recipient_lookup";
/// Enterprise API (Phase 5): org API keys, `/org/v1`, webhooks.
pub(crate) const ORG_API: &str = "org_api";
/// SSH to agent workspaces (Phase 8, SDS §11, docs/agent-ssh.md).
pub(crate) const AGENT_SSH: &str = "agent_ssh";

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
    "org.member",
    "org.policy",
    "org.status",
    "event.invite",
    "event.updated",
    "event.cancelled",
    "event.rsvp",
    "event.reminder",
    "message.receipt",
    "message.edited",
    "meeting.brief",
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
