//! Incoming-call alerting on every device (Phase 1c, `docs/call-alerting.md`).
//!
//! The calls service rings the event-stream sessions it holds. A phone with the app closed holds
//! none, so for every ring the calls service also calls `POST /internal/v1/call-ring` here, and
//! `POST /internal/v1/call-ring-cancel` when the ring stops. The postbox owns the device
//! registrations, so it decides which devices to wake and how:
//!
//! | device row                         | ring                          | ring end                  |
//! |------------------------------------|-------------------------------|---------------------------|
//! | `apns` `voip`                      | PushKit VoIP push             | nothing (the phone polls) |
//! | `apns` `alert`, `os=macos` or none | time-sensitive call alert     | passive replacement       |
//! | `apns` `alert`, `os=ios`/`ipados`  | nothing (rings through VoIP)  | nothing                   |
//! | `fcm`                              | high-priority data message    | `call_cancel` data        |
//! | `wns`                              | `incomingCall` toast          | toast removed             |
//!
//! Before anything is pushed the callee's own rules are applied again here: a caller the callee
//! has blocked, or a stranger to a mailbox that does not accept strangers, wakes nothing. The
//! `call_push` flag (account value overriding global) switches the whole thing on.
//!
//! Payloads carry what a ringing screen needs and nothing more: the room, a ring token that is good
//! for this one ring, the caller's name and handle, and the kind of call.

use std::sync::Arc;

use axum::extract::State;
use axum::http::HeaderMap;
use axum::response::{IntoResponse, Response};
use axum::Json;
use serde::Deserialize;
use serde_json::{json, Value};

use crate::capabilities::CALL_PUSH;
use crate::internal_api::{authorize_service, Endpoint};
use crate::policy::Action;
use crate::push::ApnsPush;
use crate::store::{Device, Store};
use crate::{now_unix, ApiError, AppState};

/// The longest ring the calls service is allowed to ask for. Its own is 45 seconds.
const MAX_RING_SECS: u64 = 120;
const DISPLAY_CHARS: usize = 60;
pub const CALL_CATEGORY: &str = "CALL_RING";

#[derive(Debug, Clone, Deserialize)]
pub struct Who {
    pub address: String,
    /// The caller's handle is looked up here, never taken from the request, so a `handle` field is
    /// accepted and ignored.
    #[serde(default)]
    pub display: Option<String>,
}

#[derive(Debug, Deserialize)]
pub struct RingReq {
    pub room_id: String,
    #[serde(default)]
    pub code: Option<String>,
    pub kind: String,
    pub ring_token: String,
    pub from: Who,
    pub to: String,
    pub expires_at: u64,
}

#[derive(Debug, Deserialize)]
pub struct CancelReq {
    pub room_id: String,
    pub to: String,
    pub reason: String,
    #[serde(default)]
    pub kind: Option<String>,
    #[serde(default)]
    pub from: Option<Who>,
}

/// A validated ring, as the devices are told about it.
#[derive(Debug, Clone, PartialEq)]
pub struct Ring {
    pub room: String,
    pub code: Option<String>,
    pub kind: &'static str,
    pub ring_token: String,
    pub from_address: String,
    pub from_handle: Option<String>,
    pub from_display: Option<String>,
    /// The name the caller dialled (a handle or an address).
    pub to: String,
    /// The called mailbox's address.
    pub identity: String,
    pub expires_at: u64,
}

/// A validated ring end.
#[derive(Debug, Clone, PartialEq)]
pub struct RingEnd {
    pub room: String,
    pub reason: &'static str,
    pub kind: &'static str,
    pub caller: String,
    pub identity: String,
}

/// How one device is reached.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Route {
    Voip,
    MacAlert,
    Fcm,
    Wns,
    Skip,
}

pub fn route(device: &Device) -> Route {
    match (
        device.platform.as_str(),
        device.kind.as_str(),
        device.os.as_deref(),
    ) {
        ("apns", "voip", _) => Route::Voip,
        // An iPhone build from 1c on rings through its VoIP token; alerting it as well would ring
        // twice. Builds that did not say what they are get the alert, which opens the app.
        ("apns", "alert", Some("ios" | "ipados")) => Route::Skip,
        ("apns", "alert", _) => Route::MacAlert,
        ("fcm", _, _) => Route::Fcm,
        ("wns", _, _) => Route::Wns,
        _ => Route::Skip,
    }
}

fn valid_room(room: &str) -> bool {
    (10..=64).contains(&room.len())
        && room
            .bytes()
            .all(|c| c.is_ascii_alphanumeric() || c == b'_' || c == b'-')
}

fn valid_ring_token(token: &str) -> bool {
    token.len() == 43
        && token
            .bytes()
            .all(|c| c.is_ascii_alphanumeric() || c == b'_' || c == b'-')
}

fn valid_address(address: &str) -> bool {
    address.starts_with('/')
        && (2..=201).contains(&address.len())
        && address[1..]
            .bytes()
            .all(|c| c.is_ascii_alphanumeric() || matches!(c, b'.' | b'_' | b'~' | b'-' | b'/'))
}

fn kind(value: &str) -> Option<&'static str> {
    match value {
        "audio" => Some("audio"),
        "video" => Some("video"),
        _ => None,
    }
}

fn reason(value: &str) -> &'static str {
    match value {
        "answered_elsewhere" => "answered_elsewhere",
        "declined" => "declined",
        "busy" => "busy",
        "no_answer" => "no_answer",
        "cancelled" => "cancelled",
        _ => "ended",
    }
}

/// A name for a lock screen: no control or direction-override characters, one line, 60 characters.
pub fn clean_display(value: &str) -> Option<String> {
    let kept: String = value
        .chars()
        .filter(
            |c| !matches!(*c as u32, 0x200B..=0x200F | 0x202A..=0x202E | 0x2066..=0x2069 | 0xFEFF),
        )
        .map(|c| if c.is_control() { ' ' } else { c })
        .collect();
    let flat = kept.split_whitespace().collect::<Vec<_>>().join(" ");
    let cut: String = flat.chars().take(DISPLAY_CHARS).collect();
    (!cut.is_empty()).then_some(cut)
}

impl Ring {
    fn title(&self) -> String {
        self.from_display
            .clone()
            .or_else(|| self.from_handle.clone())
            .unwrap_or_else(|| short(&self.from_address))
    }
}

fn short(address: &str) -> String {
    if address.len() > 16 {
        format!("{}…", &address[..14])
    } else {
        address.to_string()
    }
}

fn kind_word(kind: &str) -> &'static str {
    if kind == "video" {
        "video"
    } else {
        "voice"
    }
}

fn from_json(ring: &Ring) -> Value {
    json!({
        "address": ring.from_address,
        "handle": ring.from_handle,
        "display": ring.from_display,
    })
}

fn collapse_id(room: &str) -> String {
    // APNs collapse ids are at most 64 bytes.
    format!("call-{}", &room[..room.len().min(59)])
}

/// PushKit payload. `aps` is empty: CallKit, not the notification centre, shows it.
pub fn voip_payload(ring: &Ring) -> Value {
    json!({
        "aps": {},
        "type": "call_ring",
        "room": ring.room,
        "code": ring.code,
        "kind": ring.kind,
        "ring_token": ring.ring_token,
        "from": from_json(ring),
        "to": ring.to,
        "identity": ring.identity,
        "expires_at": ring.expires_at,
    })
}

/// The Mac's call alert (and older iPhone builds'): Answer and Decline come from the category.
pub fn alert_payload(ring: &Ring) -> Value {
    let mut payload = voip_payload(ring);
    payload["aps"] = json!({
        "alert": {
            "title": ring.title(),
            "body": format!("Incoming {} call", kind_word(ring.kind)),
        },
        "sound": "default",
        "category": CALL_CATEGORY,
        "interruption-level": "time-sensitive",
        "thread-id": "calls",
    });
    payload
}

/// Replaces the ringing alert with a quiet line saying how it ended.
pub fn alert_end_payload(end: &RingEnd) -> Value {
    let body = match end.reason {
        "answered_elsewhere" => "Answered on another device".to_string(),
        "declined" => format!("Declined {} call", kind_word(end.kind)),
        _ => format!("Missed {} call", kind_word(end.kind)),
    };
    json!({
        "aps": {
            "alert": { "title": end.caller, "body": body },
            "interruption-level": "passive",
            "thread-id": "calls",
        },
        "type": "call_cancel",
        "room": end.room,
        "reason": end.reason,
        "identity": end.identity,
    })
}

/// FCM data message: every value a string, priority high, gone when the ring is.
pub fn fcm_ring(device: &Device, ring: &Ring, now: u64) -> Value {
    let ttl = ring.expires_at.saturating_sub(now).max(1);
    json!({"message": {
        "token": device.token,
        "android": {"priority": "high", "ttl": format!("{ttl}s"), "collapse_key": collapse_id(&ring.room)},
        "data": {
            "type": "call_ring",
            "room": ring.room,
            "code": ring.code.clone().unwrap_or_default(),
            "kind": ring.kind,
            "ring_token": ring.ring_token,
            "from_address": ring.from_address,
            "from_handle": ring.from_handle.clone().unwrap_or_default(),
            "from_display": ring.from_display.clone().unwrap_or_default(),
            "to": ring.to,
            "identity": device.mailbox,
            "expires_at": ring.expires_at.to_string(),
        }
    }})
}

pub fn fcm_cancel(device: &Device, end: &RingEnd) -> Value {
    json!({"message": {
        "token": device.token,
        "android": {"priority": "high", "ttl": "60s", "collapse_key": collapse_id(&end.room)},
        "data": {
            "type": "call_cancel",
            "room": end.room,
            "reason": end.reason,
            "identity": device.mailbox,
        }
    }})
}

fn launch_args(action: &str, ring: &Ring) -> String {
    // Room and token are [A-Za-z0-9_-] only, so nothing here needs URL encoding.
    format!(
        "action={action}&room={}&ring={}&kind={}",
        ring.room, ring.ring_token, ring.kind
    )
}

/// The Windows toast: stays on screen, rings, Answer and Decline. Every value is escaped.
pub fn wns_toast(ring: &Ring) -> String {
    use crate::wns::escape;
    let handle = ring
        .from_handle
        .clone()
        .unwrap_or_else(|| short(&ring.from_address));
    format!(
        concat!(
            r#"<toast scenario="incomingCall" launch="{open}" activationType="foreground">"#,
            r#"<visual><binding template="ToastGeneric">"#,
            r#"<text hint-maxLines="1">{title}</text><text>{body}</text><text>{handle}</text>"#,
            r#"</binding></visual>"#,
            r#"<actions>"#,
            r#"<action content="Answer" arguments="{answer}" activationType="foreground"/>"#,
            r#"<action content="Decline" arguments="{decline}" activationType="background"/>"#,
            r#"</actions>"#,
            r#"<audio src="ms-winsoundevent:Notification.Looping.Call" loop="true"/>"#,
            r#"</toast>"#
        ),
        open = escape(&launch_args("open", ring)),
        title = escape(&ring.title()),
        body = escape(&format!("Incoming {} call", kind_word(ring.kind))),
        handle = escape(&handle),
        answer = escape(&launch_args("answer", ring)),
        decline = escape(&launch_args("decline", ring)),
    )
}

/// The end of a ring on Windows: a raw notification. The app (running, or started in the
/// background by Windows for it) removes the ring's toast by its tag and group, like Android's
/// `call_cancel`. Room ids are `[A-Za-z0-9_-]`; `reason` is one of the calls service's fixed words.
pub fn wns_cancel(end: &RingEnd) -> Vec<u8> {
    json!({"type": "call_cancel", "room": end.room, "reason": end.reason})
        .to_string()
        .into_bytes()
}

/// What the callee's own rules say about this caller.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Decision {
    Ring,
    Silence,
    Block,
    Disabled,
}

impl Decision {
    fn as_str(self) -> &'static str {
        match self {
            Decision::Ring => "ring",
            Decision::Silence => "silence",
            Decision::Block => "block",
            Decision::Disabled => "disabled",
        }
    }
}

/// The callee, resolved from the name the caller dialled — the same lookup as sending and admission.
async fn resolve(state: &AppState, to: &str) -> Option<crate::store::StoredIdentity> {
    crate::resolve_recipient(state, to.trim())
        .await
        .ok()
        .flatten()
}

async fn push_enabled(store: &Store, account: Option<String>) -> bool {
    store
        .enabled_features(account, vec![CALL_PUSH])
        .await
        .map(|on| on.contains(&CALL_PUSH))
        .unwrap_or(false)
}

/// The callee's rules applied again to the caller's verified names: the Phase 1b admission rules
/// (blocks, filters, reputation; without counting another attempt), and the plain block list and
/// stranger policy, whichever is stricter.
async fn decide(
    state: &AppState,
    callee: &crate::store::StoredIdentity,
    caller_address: &str,
    caller: Option<&crate::store::StoredIdentity>,
) -> Decision {
    let store = &state.store;
    let mut known = false;
    let caller_handle = caller.and_then(|c| c.handle.as_deref());
    for subject in caller_handle
        .into_iter()
        .chain(std::iter::once(caller_address))
    {
        match store
            .governing_contact(callee.address.clone(), subject.to_string())
            .await
        {
            Ok(Some(c)) if c.admission == "block" => return Decision::Block,
            Ok(Some(_)) => known = true,
            Ok(None) => {}
            // Unable to read the rules: wake nothing rather than ring past a block.
            Err(_) => return Decision::Block,
        }
    }
    if let Some(caller) = caller {
        match crate::filtering::decide_call_with(state, caller, callee, false).await {
            Ok((crate::store::CallRule::Block, _)) | Err(_) => return Decision::Block,
            Ok((crate::store::CallRule::Silence, _)) => return Decision::Silence,
            Ok((crate::store::CallRule::Ring, _)) => {}
        }
    }
    if known {
        return Decision::Ring;
    }
    match store.inbox_policy(callee.address.clone()).await {
        Ok(policy) if policy.accept_all => Decision::Ring,
        Ok(_) => Decision::Silence,
        Err(_) => Decision::Block,
    }
}

fn bad(code: &'static str, message: &str) -> Response {
    ApiError::bad(code, message).into_response()
}

/// `POST /internal/v1/call-ring`.
pub(crate) async fn ring(
    State(state): State<AppState>,
    headers: HeaderMap,
    body: Result<Json<RingReq>, axum::extract::rejection::JsonRejection>,
) -> Response {
    if let Err(resp) = authorize_service(&state, &headers, Endpoint::CallRing, Action::PushCallRing)
    {
        return *resp;
    }
    let Ok(Json(req)) = body else {
        return bad("bad_request", "expected a call-ring body");
    };
    let now = now_unix();
    let Some(kind) = kind(&req.kind) else {
        return bad("bad_kind", "kind must be audio or video");
    };
    if !valid_room(&req.room_id)
        || !valid_ring_token(&req.ring_token)
        || !valid_address(&req.from.address)
        || !valid_address(&req.to)
        || req
            .code
            .as_deref()
            .is_some_and(|c| c.len() != 6 || !c.bytes().all(|b| b.is_ascii_alphanumeric()))
    {
        return bad("bad_request", "malformed room, token, code or address");
    }
    if req.expires_at <= now {
        return Json(json!({ "decision": "expired", "devices": 0 })).into_response();
    }
    let expires_at = req.expires_at.min(now + MAX_RING_SECS);
    let Some(callee) = resolve(&state, &req.to).await else {
        return Json(json!({ "decision": "unknown", "devices": 0 })).into_response();
    };
    // The caller's handle is the postbox's to say, not the request's.
    let caller = state
        .store
        .get(req.from.address.clone())
        .await
        .ok()
        .flatten();
    let caller_handle = caller.as_ref().and_then(|c| c.handle.clone());
    let decision = if !push_enabled(&state.store, callee.account_id.clone()).await {
        Decision::Disabled
    } else {
        decide(&state, &callee, &req.from.address, caller.as_ref()).await
    };
    if decision != Decision::Ring {
        tracing::info!(decision = decision.as_str(), "call ring not pushed");
        return Json(json!({ "decision": decision.as_str(), "devices": 0 })).into_response();
    }
    let ring = Ring {
        room: req.room_id,
        code: req.code,
        kind,
        ring_token: req.ring_token,
        from_address: req.from.address,
        from_handle: caller_handle,
        from_display: req.from.display.as_deref().and_then(clean_display),
        to: req.to,
        identity: callee.address.clone(),
        expires_at,
    };
    let devices = match state
        .store
        .devices_for_call(callee.address, callee.account_id)
        .await
    {
        Ok(devices) => devices,
        Err(e) => {
            tracing::warn!(error = %e, "call ring device lookup failed");
            return ApiError::server("store_error").into_response();
        }
    };
    let targets: Vec<Device> = devices
        .into_iter()
        .filter(|d| route(d) != Route::Skip)
        .collect();
    let count = targets.len();
    tracing::info!(devices = count, "call ring pushed");
    tokio::spawn(deliver_ring(state, targets, ring, now));
    Json(json!({ "decision": "ring", "devices": count })).into_response()
}

/// `POST /internal/v1/call-ring-cancel`.
pub(crate) async fn cancel(
    State(state): State<AppState>,
    headers: HeaderMap,
    body: Result<Json<CancelReq>, axum::extract::rejection::JsonRejection>,
) -> Response {
    if let Err(resp) = authorize_service(&state, &headers, Endpoint::CallRing, Action::PushCallRing)
    {
        return *resp;
    }
    let Ok(Json(req)) = body else {
        return bad("bad_request", "expected a call-ring-cancel body");
    };
    if !valid_room(&req.room_id) || !valid_address(&req.to) {
        return bad("bad_request", "malformed room or address");
    }
    let Some(callee) = resolve(&state, &req.to).await else {
        return Json(json!({ "devices": 0 })).into_response();
    };
    if !push_enabled(&state.store, callee.account_id.clone()).await {
        return Json(json!({ "devices": 0 })).into_response();
    }
    let caller = match &req.from {
        Some(who) if valid_address(&who.address) => {
            let handle = match state.store.get(who.address.clone()).await {
                Ok(Some(id)) => id.handle,
                _ => None,
            };
            who.display
                .as_deref()
                .and_then(clean_display)
                .or(handle)
                .unwrap_or_else(|| short(&who.address))
        }
        _ => "Call".to_string(),
    };
    let end = RingEnd {
        room: req.room_id,
        reason: reason(&req.reason),
        kind: req.kind.as_deref().and_then(kind).unwrap_or("audio"),
        caller,
        identity: callee.address.clone(),
    };
    let devices = match state
        .store
        .devices_for_call(callee.address, callee.account_id)
        .await
    {
        Ok(devices) => devices,
        Err(_) => return ApiError::server("store_error").into_response(),
    };
    // A VoIP push must always become a reported call on the phone, so a ring end never travels
    // as one: the phone asks the calls service with its ring token instead.
    let targets: Vec<Device> = devices
        .into_iter()
        .filter(|d| matches!(route(d), Route::MacAlert | Route::Fcm | Route::Wns))
        .collect();
    let count = targets.len();
    tokio::spawn(deliver_end(state, targets, end));
    Json(json!({ "devices": count })).into_response()
}

async fn retire(store: &Arc<Store>, device: &Device) {
    if let Err(e) = store.delete_device(device.token.clone()).await {
        tracing::warn!(error = %e, "stale device row could not be removed");
    }
}

async fn deliver_ring(state: AppState, devices: Vec<Device>, ring: Ring, now: u64) {
    let mut tasks = Vec::new();
    for device in devices {
        let state = state.clone();
        let ring = ring.clone();
        tasks.push(tokio::spawn(async move {
            let retired = match route(&device) {
                Route::Voip => match &state.apns {
                    Some(apns) => {
                        apns.send(
                            &device,
                            ApnsPush {
                                push_type: "voip",
                                voip: true,
                                priority: "10",
                                expiration: Some(ring.expires_at),
                                collapse_id: None,
                                payload: voip_payload(&ring),
                            },
                        )
                        .await
                    }
                    None => false,
                },
                Route::MacAlert => match &state.apns {
                    Some(apns) => {
                        apns.send(
                            &device,
                            ApnsPush {
                                push_type: "alert",
                                voip: false,
                                priority: "10",
                                expiration: Some(ring.expires_at),
                                collapse_id: Some(collapse_id(&ring.room)),
                                payload: alert_payload(&ring),
                            },
                        )
                        .await
                    }
                    None => false,
                },
                Route::Fcm => match &state.fcm {
                    Some(fcm) => fcm.send(fcm_ring(&device, &ring, now)).await,
                    None => false,
                },
                Route::Wns => match &state.wns {
                    Some(wns) => {
                        wns.toast(
                            &device.token,
                            wns_toast(&ring),
                            &crate::wns::tag_for(&ring.room),
                            ring.expires_at.saturating_sub(now),
                        )
                        .await
                    }
                    None => false,
                },
                Route::Skip => false,
            };
            if retired {
                retire(&state.store, &device).await;
            }
        }));
    }
    for task in tasks {
        let _ = task.await;
    }
}

async fn deliver_end(state: AppState, devices: Vec<Device>, end: RingEnd) {
    for device in devices {
        let retired = match route(&device) {
            Route::MacAlert => match &state.apns {
                Some(apns) => {
                    apns.send(
                        &device,
                        ApnsPush {
                            push_type: "alert",
                            voip: false,
                            priority: "5",
                            expiration: Some(now_unix() + 3600),
                            collapse_id: Some(collapse_id(&end.room)),
                            payload: alert_end_payload(&end),
                        },
                    )
                    .await
                }
                None => false,
            },
            Route::Fcm => match &state.fcm {
                Some(fcm) => fcm.send(fcm_cancel(&device, &end)).await,
                None => false,
            },
            Route::Wns => match &state.wns {
                Some(wns) => wns.raw(&device.token, wns_cancel(&end), 60).await,
                None => false,
            },
            _ => false,
        };
        if retired {
            retire(&state.store, &device).await;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn device(platform: &str, kind: &str, os: Option<&str>) -> Device {
        Device {
            token: "tok".into(),
            mailbox: "/k/callee".into(),
            platform: platform.into(),
            environment: "production".into(),
            kind: kind.into(),
            os: os.map(Into::into),
        }
    }

    fn ring() -> Ring {
        Ring {
            room: "R".repeat(22),
            code: Some("ABC234".into()),
            kind: "video",
            ring_token: "t".repeat(43),
            from_address: "/k/caller".into(),
            from_handle: Some("/alp".into()),
            from_display: Some("Alp <script>&".into()),
            to: "/bekir".into(),
            identity: "/k/callee".into(),
            expires_at: 1_000_045,
        }
    }

    #[test]
    fn each_device_is_reached_its_own_way() {
        assert_eq!(route(&device("apns", "voip", Some("ios"))), Route::Voip);
        assert_eq!(route(&device("apns", "alert", Some("ios"))), Route::Skip);
        assert_eq!(route(&device("apns", "alert", Some("ipados"))), Route::Skip);
        assert_eq!(
            route(&device("apns", "alert", Some("macos"))),
            Route::MacAlert
        );
        assert_eq!(route(&device("apns", "alert", None)), Route::MacAlert);
        assert_eq!(route(&device("fcm", "alert", Some("android"))), Route::Fcm);
        assert_eq!(route(&device("wns", "alert", Some("windows"))), Route::Wns);
        assert_eq!(route(&device("other", "alert", None)), Route::Skip);
    }

    #[test]
    fn the_voip_payload_carries_the_ring_and_nothing_else() {
        let v = voip_payload(&ring());
        assert_eq!(v["aps"], json!({}));
        assert_eq!(v["type"], "call_ring");
        assert_eq!(v["ring_token"], "t".repeat(43));
        assert_eq!(v["from"]["handle"], "/alp");
        assert_eq!(v["identity"], "/k/callee");
        assert_eq!(v["expires_at"], 1_000_045);
        let keys: Vec<_> = v.as_object().unwrap().keys().cloned().collect();
        assert_eq!(
            keys.len(),
            10,
            "no field beyond the documented ones: {keys:?}"
        );
    }

    #[test]
    fn the_mac_alert_is_a_time_sensitive_call_category() {
        let v = alert_payload(&ring());
        assert_eq!(v["aps"]["category"], CALL_CATEGORY);
        assert_eq!(v["aps"]["interruption-level"], "time-sensitive");
        assert_eq!(v["aps"]["alert"]["title"], "Alp <script>&");
        assert_eq!(v["aps"]["alert"]["body"], "Incoming video call");
        assert_eq!(v["room"], "R".repeat(22));
        let end = alert_end_payload(&RingEnd {
            room: "R".repeat(22),
            reason: "no_answer",
            kind: "audio",
            caller: "Alp".into(),
            identity: "/k/callee".into(),
        });
        assert_eq!(end["aps"]["alert"]["body"], "Missed voice call");
        assert_eq!(end["aps"]["interruption-level"], "passive");
        assert!(end["aps"].get("sound").is_none());
        assert!(collapse_id(&"x".repeat(64)).len() <= 64);
    }

    #[test]
    fn fcm_data_is_all_strings_and_lives_only_as_long_as_the_ring() {
        let d = device("fcm", "alert", Some("android"));
        let v = fcm_ring(&d, &ring(), 1_000_000);
        assert_eq!(v["message"]["android"]["ttl"], "45s");
        assert_eq!(v["message"]["android"]["priority"], "high");
        assert!(v["message"].get("notification").is_none());
        for (_, value) in v["message"]["data"].as_object().unwrap() {
            assert!(value.is_string(), "{value}");
        }
        assert_eq!(v["message"]["data"]["identity"], "/k/callee");
        // A ring already over still goes out with the smallest TTL rather than a negative one.
        let late = fcm_ring(&d, &ring(), 2_000_000);
        assert_eq!(late["message"]["android"]["ttl"], "1s");
    }

    #[test]
    fn the_windows_toast_escapes_every_value() {
        let xml = wns_toast(&ring());
        assert!(xml.starts_with(r#"<toast scenario="incomingCall""#));
        assert!(xml.contains("Alp &lt;script&gt;&amp;"));
        assert!(!xml.contains("<script>"));
        assert!(xml.contains(&format!(
            "action=answer&amp;room={}&amp;ring={}&amp;kind=video",
            "R".repeat(22),
            "t".repeat(43)
        )));
        assert!(xml.contains("Notification.Looping.Call"));
    }

    #[test]
    fn the_windows_ring_end_is_a_small_raw_cancel() {
        let payload = wns_cancel(&RingEnd {
            room: "R".repeat(22),
            reason: "answered_elsewhere",
            kind: "audio",
            caller: "Alp <b>".into(),
            identity: "/k/callee".into(),
        });
        let v: Value = serde_json::from_slice(&payload).unwrap();
        assert_eq!(v["type"], "call_cancel");
        assert_eq!(v["room"], "R".repeat(22));
        assert_eq!(v["reason"], "answered_elsewhere");
        // Nothing about the people involved: Microsoft carries it, the app needs only the room.
        assert_eq!(v.as_object().unwrap().len(), 3);
        assert!(payload.len() < 5000);
    }

    #[test]
    fn display_names_are_one_clean_line() {
        assert_eq!(
            clean_display("  Alp\u{202E}\n\tDağ \u{0007}"),
            Some("Alp Dağ".into())
        );
        assert_eq!(clean_display("\u{200B}"), None);
        assert_eq!(clean_display(&"x".repeat(100)).unwrap().len(), 60);
    }

    #[test]
    fn inputs_are_held_to_their_formats() {
        assert!(valid_room("abcdefghij_-12"));
        assert!(!valid_room("short"));
        assert!(!valid_room("has space in it"));
        assert!(valid_ring_token(&"a".repeat(43)));
        assert!(!valid_ring_token(&"a".repeat(42)));
        assert!(!valid_ring_token(&format!("{}=", "a".repeat(42))));
        assert!(valid_address("/k/abc"));
        assert!(valid_address("/bekir/agent1"));
        assert!(!valid_address("bekir"));
        assert!(!valid_address("/a b"));
        assert_eq!(reason("weird"), "ended");
        assert_eq!(kind("screen"), None);
    }
}
