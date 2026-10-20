//! End-to-end tests for Phase 1c (incoming-call alerting), over real sockets: device registration
//! with `kind`/`os`/`wns`, and the internal call-ring endpoints' tokens, flag, filtering and device
//! selection. No push provider is configured here, so nothing leaves the process.

use crate::*;
use serde_json::{json, Value};

const OPS: &str = "svc_ops_0123456789abcdef0123456789abcdef";
const CALLS: &str = "svc_calls_0123456789abcdef0123456789abcdef";

fn state() -> AppState {
    let mut state = crate::tests::test_state();
    state.service_tokens = Arc::new(internal_api::tokens_for_test(&[
        ("POSTBOX_SVC_TOKEN_OPS", OPS),
        ("POSTBOX_SVC_TOKEN_CALLS", CALLS),
    ]));
    state
}

async fn serve(state: AppState) -> (String, String) {
    let public = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let internal = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let public_url = format!("http://{}", public.local_addr().unwrap());
    let internal_url = format!("http://{}", internal.local_addr().unwrap());
    let app =
        build_router(state.clone()).into_make_service_with_connect_info::<std::net::SocketAddr>();
    let internal_app =
        internal_api::router(state).into_make_service_with_connect_info::<std::net::SocketAddr>();
    tokio::spawn(async move { axum::serve(public, app).await.unwrap() });
    tokio::spawn(async move { axum::serve(internal, internal_app).await.unwrap() });
    (public_url, internal_url)
}

/// An account with an API key and two mailboxes.
async fn account(state: &AppState, id: &str) -> (String, String, String) {
    let (token, key) = new_api_key();
    state
        .store
        .create_account(id.into(), key, now_unix())
        .await
        .unwrap();
    let mut out = Vec::new();
    for _ in 0..2 {
        let v = do_create_identity(state, Some(id.into()), None, None)
            .await
            .unwrap();
        out.push(v["address"].as_str().unwrap().to_string());
    }
    (token, out.remove(0), out.remove(0))
}

async fn register(base: &str, token: &str, body: Value) -> reqwest::Response {
    reqwest::Client::new()
        .post(format!("{base}/v1/devices"))
        .bearer_auth(token)
        .json(&body)
        .send()
        .await
        .unwrap()
}

async fn internal(base: &str, token: &str, path: &str, body: Value) -> (StatusCode, Value) {
    let r = reqwest::Client::new()
        .post(format!("{base}/internal/v1/{path}"))
        .bearer_auth(token)
        .json(&body)
        .send()
        .await
        .unwrap();
    let status = StatusCode::from_u16(r.status().as_u16()).unwrap();
    (status, r.json().await.unwrap_or(Value::Null))
}

async fn flag(base: &str, enabled: bool) {
    let r = reqwest::Client::new()
        .put(format!(
            "{base}/internal/v1/features/{}",
            capabilities::CALL_PUSH
        ))
        .bearer_auth(OPS)
        .json(&json!({ "scope": "global", "enabled": enabled }))
        .send()
        .await
        .unwrap();
    assert_eq!(r.status(), 200);
}

fn ring_body(from: &str, to: &str) -> Value {
    json!({
        "room_id": "R".repeat(22), "code": "ABC234", "kind": "video", "ring_token": "t".repeat(43),
        "from": { "address": from, "handle": "/forged", "display": "Alp" }, "to": to,
        "expires_at": now_unix() + 45,
    })
}

#[tokio::test]
async fn devices_register_with_kind_os_and_wns_channels() {
    let state = state();
    let (public, _) = serve(state.clone()).await;
    let (token, mailbox, _) = account(&state, "acct_dev").await;
    let hex = "ab".repeat(32);
    for (body, want) in [
        (
            json!({"token": hex, "platform": "apns", "kind": "voip", "os": "ios", "identity": mailbox}),
            200,
        ),
        (
            json!({"token": "cd".repeat(32), "platform": "apns", "os": "macos", "identity": mailbox}),
            200,
        ),
        (
            json!({"token": "https://db5p.notify.windows.com/?token=abc", "platform": "wns", "os": "windows", "identity": mailbox}),
            200,
        ),
        (
            json!({"token": "fcm:tok", "platform": "fcm", "kind": "voip", "identity": mailbox}),
            400,
        ),
        (
            json!({"token": "https://evil.example/?x", "platform": "wns", "identity": mailbox}),
            400,
        ),
        (
            json!({"token": "http://db5p.notify.windows.com/?token=a", "platform": "wns", "identity": mailbox}),
            400,
        ),
        (
            json!({"token": "ef".repeat(32), "platform": "apns", "os": "plan9", "identity": mailbox}),
            400,
        ),
        (
            json!({"token": "ef".repeat(32), "platform": "apns", "kind": "pager", "identity": mailbox}),
            400,
        ),
    ] {
        let r = register(&public, &token, body.clone()).await;
        assert_eq!(r.status().as_u16(), want, "{body}");
    }
    let rows = state.store.devices_for(mailbox).await.unwrap();
    assert_eq!(rows.len(), 3);
    let voip = rows.iter().find(|d| d.token == hex).unwrap();
    assert_eq!(
        (voip.kind.as_str(), voip.os.as_deref()),
        ("voip", Some("ios"))
    );
}

#[tokio::test]
async fn only_the_calls_service_may_ring_and_only_with_a_valid_body() {
    let state = state();
    let (_, internal_url) = serve(state.clone()).await;
    let (_, callee, _) = account(&state, "acct_callee").await;
    let body = ring_body("/k/nobody", &callee);
    assert_eq!(
        internal(
            &internal_url,
            "svc_nope_0123456789abcdef0123456789abcdef",
            "call-ring",
            body.clone()
        )
        .await
        .0,
        StatusCode::UNAUTHORIZED
    );
    assert_eq!(
        internal(&internal_url, OPS, "call-ring", body.clone())
            .await
            .0,
        StatusCode::FORBIDDEN
    );
    assert_eq!(
        internal(
            &internal_url,
            OPS,
            "call-ring-cancel",
            json!({"room_id": "R".repeat(22), "to": callee, "reason": "declined"})
        )
        .await
        .0,
        StatusCode::FORBIDDEN
    );
    for (field, value) in [
        ("room_id", json!("short")),
        ("ring_token", json!("x")),
        ("kind", json!("screen")),
        ("to", json!("not-an-address")),
        ("code", json!("<b>")),
    ] {
        let mut bad = body.clone();
        bad[field] = value;
        assert_eq!(
            internal(&internal_url, CALLS, "call-ring", bad).await.0,
            StatusCode::BAD_REQUEST,
            "{field}"
        );
    }
    // The public port has no internal routes at all.
    let (public, _) = serve(state.clone()).await;
    let r = reqwest::Client::new()
        .post(format!("{public}/internal/v1/call-ring"))
        .bearer_auth(CALLS)
        .json(&body)
        .send()
        .await
        .unwrap();
    assert_eq!(r.status(), 404);
}

#[tokio::test]
async fn a_ring_wakes_the_callees_devices_across_the_account_when_the_flag_is_on() {
    let state = state();
    let (public, internal_url) = serve(state.clone()).await;
    let (token, callee, other) = account(&state, "acct_bekir").await;
    let (_, caller, _) = account(&state, "acct_alp").await;
    // An iPhone (VoIP + its own alert token), a Mac, an old build that said nothing, an Android
    // phone reading the account's *other* mailbox, and a Windows PC.
    for body in [
        json!({"token": "01".repeat(32), "platform": "apns", "kind": "voip", "os": "ios", "identity": callee}),
        json!({"token": "02".repeat(32), "platform": "apns", "os": "ios", "identity": callee}),
        json!({"token": "03".repeat(32), "platform": "apns", "os": "macos", "identity": callee}),
        json!({"token": "04".repeat(32), "platform": "apns", "identity": callee}),
        json!({"token": "fcm:other-mailbox", "platform": "fcm", "os": "android", "identity": other}),
        json!({"token": "https://db5p.notify.windows.com/?token=w", "platform": "wns", "os": "windows", "identity": callee}),
    ] {
        assert_eq!(register(&public, &token, body).await.status(), 200);
    }
    let body = ring_body(&caller, &callee);
    let (status, v) = internal(&internal_url, CALLS, "call-ring", body.clone()).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(v["decision"], "disabled", "off until the flag says so");

    flag(&internal_url, true).await;
    let (_, v) = internal(&internal_url, CALLS, "call-ring", body.clone()).await;
    assert_eq!(
        v,
        json!({"decision": "ring", "devices": 5}),
        "every device but the iPhone's alert token"
    );
    let (_, v) = internal(&internal_url, CALLS, "call-ring-cancel", json!({"room_id": "R".repeat(22), "to": callee, "reason": "no_answer", "kind": "video", "from": {"address": caller}})).await;
    assert_eq!(
        v,
        json!({"devices": 4}),
        "a ring end never travels as a VoIP push"
    );

    // Unknown callee, expired ring.
    let (_, v) = internal(
        &internal_url,
        CALLS,
        "call-ring",
        ring_body(&caller, "/k/nobody00000000"),
    )
    .await;
    assert_eq!(v["decision"], "unknown");
    let mut late = body.clone();
    late["expires_at"] = json!(now_unix() - 1);
    let (_, v) = internal(&internal_url, CALLS, "call-ring", late).await;
    assert_eq!(v["decision"], "expired");
}

#[tokio::test]
async fn blocked_and_unwanted_callers_wake_nothing() {
    let state = state();
    let (public, internal_url) = serve(state.clone()).await;
    let (token, callee, _) = account(&state, "acct_quiet").await;
    let (_, caller, _) = account(&state, "acct_pest").await;
    assert_eq!(register(&public, &token, json!({"token": "05".repeat(32), "platform": "apns", "kind": "voip", "os": "ios", "identity": callee})).await.status(), 200);
    flag(&internal_url, true).await;

    state
        .store
        .upsert_contact(store::ContactUpdate {
            owner: callee.clone(),
            peer: caller.clone(),
            alias: None,
            admission: Some("block".into()),
            autonomy: None,
            allowed_verbs: None,
            now: now_unix(),
        })
        .await
        .unwrap();
    let (_, v) = internal(
        &internal_url,
        CALLS,
        "call-ring",
        ring_body(&caller, &callee),
    )
    .await;
    assert_eq!(v, json!({"decision": "block", "devices": 0}));

    // Unblocked but a stranger to a mailbox that accepts no strangers: silence.
    state
        .store
        .delete_contact(callee.clone(), caller.clone())
        .await
        .unwrap();
    state
        .store
        .set_inbox_policy(callee.clone(), Some(false), None, now_unix())
        .await
        .unwrap();
    let (_, v) = internal(
        &internal_url,
        CALLS,
        "call-ring",
        ring_body(&caller, &callee),
    )
    .await;
    assert_eq!(v["decision"], "silence");

    // A contact the callee allows rings even then.
    state
        .store
        .upsert_contact(store::ContactUpdate {
            owner: callee.clone(),
            peer: caller.clone(),
            alias: None,
            admission: Some("allow".into()),
            autonomy: None,
            allowed_verbs: None,
            now: now_unix(),
        })
        .await
        .unwrap();
    let (_, v) = internal(
        &internal_url,
        CALLS,
        "call-ring",
        ring_body(&caller, &callee),
    )
    .await;
    assert_eq!(v, json!({"decision": "ring", "devices": 1}));

    // Flag off for this account only.
    let r = reqwest::Client::new()
        .put(format!(
            "{internal_url}/internal/v1/features/{}",
            capabilities::CALL_PUSH
        ))
        .bearer_auth(OPS)
        .json(&json!({"scope": "account", "account_id": "acct_quiet", "enabled": false}))
        .send()
        .await
        .unwrap();
    assert_eq!(r.status(), 200);
    let (_, v) = internal(
        &internal_url,
        CALLS,
        "call-ring",
        ring_body(&caller, &callee),
    )
    .await;
    assert_eq!(v["decision"], "disabled");
}

#[tokio::test]
async fn mail_notices_skip_voip_and_windows_rows() {
    // fan_out reads every row for a mailbox; only `alert` rows on apns/fcm may get mail.
    let store = store::Store::open(":memory:").unwrap();
    for (token, platform, kind) in [
        ("aa", "apns", "voip"),
        ("https://db5p.notify.windows.com/?t", "wns", "alert"),
        ("bb", "apns", "alert"),
    ] {
        store
            .register_device(
                store::DeviceRegistration {
                    token: token.into(),
                    mailbox: "/k/m".into(),
                    account: None,
                    platform: platform.into(),
                    environment: "production".into(),
                    kind: kind.into(),
                    os: None,
                },
                1,
            )
            .await
            .unwrap();
    }
    let rows = store.devices_for("/k/m".into()).await.unwrap();
    let mail: Vec<_> = rows
        .iter()
        .filter(|d| d.kind == "alert" && matches!(d.platform.as_str(), "apns" | "fcm"))
        .map(|d| d.token.as_str())
        .collect();
    assert_eq!(mail, vec!["bb"]);
}
