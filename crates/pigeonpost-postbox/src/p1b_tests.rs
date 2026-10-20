//! End-to-end tests for Phase 1b over real sockets: filtered delivery, blocking, call admission,
//! call history, profile pictures — and that a refused party learns nothing about why.

use crate::*;
use reqwest::Client;
use serde_json::{json, Value};

const OPS: &str = "svc_ops_0123456789abcdef0123456789abcdef";
const CALLS: &str = "svc_calls_0123456789abcdef0123456789abcdef";

fn state() -> AppState {
    let mut state = crate::tests::test_state();
    state.service_tokens = Arc::new(internal_api::tokens_for_test(&[
        ("POSTBOX_SVC_TOKEN_OPS", OPS),
        ("POSTBOX_SVC_TOKEN_CALLS", CALLS),
    ]));
    state.avatar_blobs = Some(Arc::new(blobs::Blobs::for_test("avatars")));
    state
}

struct Server {
    public: String,
    internal: String,
    http: Client,
}

async fn serve(state: AppState) -> Server {
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
    Server {
        public: public_url,
        internal: internal_url,
        http: Client::new(),
    }
}

impl Server {
    async fn call(
        &self,
        method: reqwest::Method,
        path: &str,
        token: Option<&str>,
        body: Option<Value>,
    ) -> (StatusCode, Value) {
        let base = if path.starts_with("/internal/") {
            &self.internal
        } else {
            &self.public
        };
        let mut req = self.http.request(method, format!("{base}{path}"));
        if let Some(t) = token {
            req = req.bearer_auth(t);
        }
        if let Some(b) = body {
            req = req.json(&b);
        }
        let resp = req.send().await.unwrap();
        let status = StatusCode::from_u16(resp.status().as_u16()).unwrap();
        let text = resp.text().await.unwrap();
        (status, serde_json::from_str(&text).unwrap_or(Value::Null))
    }
    async fn get(&self, path: &str, token: Option<&str>) -> (StatusCode, Value) {
        self.call(reqwest::Method::GET, path, token, None).await
    }
    async fn post(&self, path: &str, token: Option<&str>, body: Value) -> (StatusCode, Value) {
        self.call(reqwest::Method::POST, path, token, Some(body))
            .await
    }
    async fn put(&self, path: &str, token: Option<&str>, body: Value) -> (StatusCode, Value) {
        self.call(reqwest::Method::PUT, path, token, Some(body))
            .await
    }
    async fn delete(&self, path: &str, token: Option<&str>) -> (StatusCode, Value) {
        self.call(reqwest::Method::DELETE, path, token, None).await
    }
    async fn flag(&self, name: &str, account: &str) {
        let (status, _) = self
            .put(
                &format!("/internal/v1/features/{name}"),
                Some(OPS),
                json!({ "scope": "account", "account_id": account, "enabled": true }),
            )
            .await;
        assert_eq!(status, StatusCode::OK, "flag {name}");
    }
}

/// An account with one API key and one mailbox, optionally named.
struct Person {
    token: String,
    account: String,
    me: store::StoredIdentity,
}

async fn person(state: &AppState, account: &str, handle: Option<&str>) -> Person {
    let (token, key) = new_api_key();
    state
        .store
        .create_account(account.into(), key, now_unix())
        .await
        .unwrap();
    if let Some(h) = handle {
        let ns = h.trim_start_matches('/').split('/').next().unwrap();
        state
            .store
            .set_namespace_owner(ns.into(), account.into(), "test", 1, None)
            .await
            .unwrap();
    }
    let v = do_create_identity(
        state,
        Some(account.into()),
        None,
        handle.map(str::to_string),
    )
    .await
    .unwrap();
    let me = state
        .store
        .get(v["address"].as_str().unwrap().to_string())
        .await
        .unwrap()
        .unwrap();
    Person {
        token,
        account: account.into(),
        me,
    }
}

fn enc(address: &str) -> String {
    address.replace('/', "%2F")
}

#[tokio::test]
async fn filtered_messages_are_delivered_out_of_sight_and_accept_brings_them_back() {
    let state = state();
    let srv = serve(state.clone()).await;
    let bob = person(&state, "acct_bob", None).await;
    let alice = person(&state, "acct_alice", None).await;
    srv.flag("message_filtering", &bob.account).await;

    let (status, policy) = srv
        .put(
            "/v1/policy",
            Some(&bob.token),
            json!({ "unknown_messages": "filter" }),
        )
        .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(policy["unknown_messages"], "filter");
    assert_eq!(policy["accept_all"], true, "the full policy comes back");

    // The sender cannot tell: an ordinary 201.
    let (status, sent) = srv
        .post(
            "/v1/send",
            Some(&alice.token),
            json!({ "to": bob.me.address, "body": "hello from a stranger" }),
        )
        .await;
    assert_eq!(status, StatusCode::CREATED);
    assert!(sent["message_id"].is_string());

    let (_, inbox) = srv
        .get("/v1/inbox?include_read=true", Some(&bob.token))
        .await;
    assert!(inbox["messages"].as_array().unwrap().is_empty());
    let (_, filtered) = srv
        .get("/v1/inbox?filtered=1&include_read=true", Some(&bob.token))
        .await;
    let list = filtered["messages"].as_array().unwrap();
    assert_eq!(list.len(), 1);
    assert_eq!(list[0]["filtered"], true);
    assert_eq!(list[0]["body"], "hello from a stranger");
    assert_eq!(
        state
            .store
            .unread_count(bob.me.address.clone())
            .await
            .unwrap(),
        0
    );
    let (_, threads) = srv.get("/v1/threads", Some(&bob.token)).await;
    assert!(
        threads["threads"].as_array().unwrap().is_empty(),
        "a thread holding only filtered mail stays out of the list"
    );

    // Accept: a contact, and the mail moves.
    let (status, accepted) = srv
        .post(
            "/v1/filtered/accept",
            Some(&bob.token),
            json!({ "peer": alice.me.address }),
        )
        .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(accepted["moved"], 1);
    assert_eq!(accepted["contact"]["admission"], "allow");
    let (_, inbox) = srv
        .get("/v1/inbox?include_read=true", Some(&bob.token))
        .await;
    assert_eq!(inbox["messages"].as_array().unwrap().len(), 1);
    assert_eq!(inbox["messages"][0]["filtered"], false);

    // Known now: the next message is not filtered.
    srv.post(
        "/v1/send",
        Some(&alice.token),
        json!({ "to": bob.me.address, "body": "again" }),
    )
    .await;
    let (_, inbox) = srv
        .get("/v1/inbox?include_read=true", Some(&bob.token))
        .await;
    assert_eq!(inbox["messages"].as_array().unwrap().len(), 2);
}

#[tokio::test]
async fn a_block_setting_is_indistinguishable_from_a_closed_inbox() {
    let state = state();
    let srv = serve(state.clone()).await;
    let bob = person(&state, "acct_bob", None).await;
    let carol = person(&state, "acct_carol", None).await;
    let alice = person(&state, "acct_alice", None).await;
    srv.flag("message_filtering", &bob.account).await;
    srv.put(
        "/v1/policy",
        Some(&bob.token),
        json!({ "unnamed_messages": "block" }),
    )
    .await;
    // Carol closes her inbox to strangers the old way.
    srv.put(
        "/v1/policy",
        Some(&carol.token),
        json!({ "accept_all": false }),
    )
    .await;

    let to_bob = srv
        .post(
            "/v1/send",
            Some(&alice.token),
            json!({ "to": bob.me.address, "body": "x" }),
        )
        .await;
    let to_carol = srv
        .post(
            "/v1/send",
            Some(&alice.token),
            json!({ "to": carol.me.address, "body": "x" }),
        )
        .await;
    assert_eq!(to_bob.0, StatusCode::FORBIDDEN);
    assert_eq!(to_bob, to_carol, "byte-for-byte the same refusal");
}

#[tokio::test]
async fn a_block_on_the_key_address_holds_against_the_handle_and_wildcards_work() {
    let state = state();
    let srv = serve(state.clone()).await;
    let bob = person(&state, "acct_bob", None).await;
    let spammer = person(&state, "acct_spam", Some("/spamco/bot")).await;
    let other = person(&state, "acct_spam2", None).await;
    assert_eq!(spammer.me.handle.as_deref(), Some("/spamco/bot"));

    // Exact block on the /k/ address.
    srv.put(
        "/v1/contacts",
        Some(&bob.token),
        json!({ "peer": spammer.me.address, "admission": "block" }),
    )
    .await;
    let (status, body) = srv
        .post(
            "/v1/send",
            Some(&spammer.token),
            json!({ "to": bob.me.address, "body": "x" }),
        )
        .await;
    assert_eq!(
        (status, body["error"].as_str()),
        (StatusCode::FORBIDDEN, Some("not_admitted"))
    );

    // The Blocked list shows exact and wildcard entries.
    srv.put(
        "/v1/contacts",
        Some(&bob.token),
        json!({ "peer": "/spamco/*", "admission": "block" }),
    )
    .await;
    srv.put(
        "/v1/contacts",
        Some(&bob.token),
        json!({ "peer": other.me.address, "admission": "allow" }),
    )
    .await;
    let (_, blocked) = srv
        .get("/v1/contacts?admission=block", Some(&bob.token))
        .await;
    let peers: Vec<&str> = blocked["contacts"]
        .as_array()
        .unwrap()
        .iter()
        .map(|c| c["peer"].as_str().unwrap())
        .collect();
    assert_eq!(peers.len(), 2);
    assert!(peers.contains(&"/spamco/*") && peers.contains(&spammer.me.address.as_str()));

    // Unblocking the exact row leaves the wildcard: still refused.
    let (status, _) = srv
        .delete(
            &format!("/v1/contacts?peer={}", enc(&spammer.me.address)),
            Some(&bob.token),
        )
        .await;
    assert_eq!(status, StatusCode::OK);
    let (status, _) = srv
        .post(
            "/v1/send",
            Some(&spammer.token),
            json!({ "to": bob.me.address, "body": "x" }),
        )
        .await;
    assert_eq!(status, StatusCode::FORBIDDEN);
    // Unblocking the wildcard lets them through.
    srv.delete("/v1/contacts?peer=%2Fspamco%2F*", Some(&bob.token))
        .await;
    let (status, _) = srv
        .post(
            "/v1/send",
            Some(&spammer.token),
            json!({ "to": bob.me.address, "body": "x" }),
        )
        .await;
    assert_eq!(status, StatusCode::CREATED);
}

#[tokio::test]
async fn agents_only_tighten_and_bad_policy_values_change_nothing() {
    let state = state();
    let srv = serve(state.clone()).await;
    let bob = person(&state, "acct_bob", None).await;
    let tighten = store::FiltersUpdate {
        unknown_calls: Some(store::CallRule::Block),
        ..Default::default()
    };
    filtering::do_set_filters(&state, &bob.me, tighten, TrustActor::Agent)
        .await
        .unwrap();
    let loosen = store::FiltersUpdate {
        unknown_calls: Some(store::CallRule::Ring),
        ..Default::default()
    };
    let err = filtering::do_set_filters(&state, &bob.me, loosen, TrustActor::Agent)
        .await
        .unwrap_err();
    assert_eq!(err.into_response().status(), StatusCode::FORBIDDEN);

    // One bad field refuses the whole request.
    let (status, body) = srv
        .put(
            "/v1/policy",
            Some(&bob.token),
            json!({ "accept_all": false, "unknown_calls": "maybe" }),
        )
        .await;
    assert_eq!(
        (status, body["error"].as_str()),
        (StatusCode::BAD_REQUEST, Some("invalid_policy"))
    );
    let (_, policy) = srv.get("/v1/policy", Some(&bob.token)).await;
    assert_eq!(policy["accept_all"], true);
    assert_eq!(policy["unknown_calls"], "block");
    let (status, _) = srv
        .put(
            "/v1/policy",
            Some(&bob.token),
            json!({ "call_history_days": 7 }),
        )
        .await;
    assert_eq!(status, StatusCode::BAD_REQUEST);
}

async fn admit(srv: &Server, from: &str, to: &str) -> (StatusCode, Value) {
    srv.post(
        "/internal/v1/call-admission",
        Some(CALLS),
        json!({ "from": { "mailbox": from }, "to": to, "kind": "video" }),
    )
    .await
}

#[tokio::test]
async fn call_admission_decides_per_callee_and_only_for_the_calls_service() {
    let state = state();
    let srv = serve(state.clone()).await;
    let bob = person(&state, "acct_bob", None).await;
    let stranger = person(&state, "acct_x", None).await;
    let named = person(&state, "acct_n", Some("/nadia/main")).await;
    let friend = person(&state, "acct_f", None).await;

    // Only the calls service: no token, a member token, the ops token are all refused.
    let body = json!({ "from": { "mailbox": stranger.me.address }, "to": bob.me.address, "kind": "audio" });
    assert_eq!(
        srv.post("/internal/v1/call-admission", None, body.clone())
            .await
            .0,
        StatusCode::UNAUTHORIZED
    );
    assert_eq!(
        srv.post("/internal/v1/call-admission", Some(OPS), body.clone())
            .await
            .0,
        StatusCode::FORBIDDEN
    );
    assert_eq!(
        srv.post("/internal/v1/call-admission", Some(&bob.token), body)
            .await
            .0,
        StatusCode::UNAUTHORIZED
    );
    // Not on the public listener at all.
    let (status, _) = srv
        .call(
            reqwest::Method::POST,
            "/v1/../internal/v1/call-admission",
            Some(CALLS),
            Some(json!({})),
        )
        .await;
    assert_ne!(status, StatusCode::OK);

    // Flag off: everything rings, except a block, which is a block.
    let (_, d) = admit(&srv, &stranger.me.address, &bob.me.address).await;
    assert_eq!(d["decision"], "ring");
    srv.put(
        "/v1/contacts",
        Some(&bob.token),
        json!({ "peer": stranger.me.address, "admission": "block" }),
    )
    .await;
    let (_, d) = admit(&srv, &stranger.me.address, &bob.me.address).await;
    assert_eq!(d["decision"], "block");
    assert_eq!(d["targets"], json!([bob.me.address]));

    srv.flag("call_admission", &bob.account).await;
    // Defaults: an unnamed stranger is silenced, a named one rings, a known one rings.
    let (_, d) = admit(&srv, &friend.me.address, &bob.me.address).await;
    assert_eq!(d["decision"], "silence");
    let (_, d) = admit(&srv, &named.me.address, &bob.me.address).await;
    assert_eq!(d["decision"], "ring");
    // Bob writing to the friend first makes the friend known.
    srv.post(
        "/v1/send",
        Some(&bob.token),
        json!({ "to": friend.me.address, "body": "call me" }),
    )
    .await;
    let (_, d) = admit(&srv, &friend.me.address, &bob.me.address).await;
    assert_eq!(d["decision"], "ring");
    // Handles resolve; the decision shape is identical whatever it says.
    let (_, d) = admit(&srv, &bob.me.address, "/nadia/main").await;
    assert_eq!(d["targets"], json!([named.me.address]));
    // Unknown target: nobody to ring.
    let (_, d) = admit(&srv, &bob.me.address, "/k/nobody").await;
    assert_eq!(d["targets"], json!([]));
    // Stricter settings.
    srv.put(
        "/v1/policy",
        Some(&bob.token),
        json!({ "unknown_calls": "block" }),
    )
    .await;
    let (_, d) = admit(&srv, &named.me.address, &bob.me.address).await;
    assert_eq!(d["decision"], "block");
}

#[tokio::test]
async fn the_per_pair_limit_silences_even_a_known_caller() {
    let state = state();
    let srv = serve(state.clone()).await;
    let bob = person(&state, "acct_bob", None).await;
    let friend = person(&state, "acct_f", Some("/fred/main")).await;
    srv.flag("call_admission", &bob.account).await;
    srv.put(
        "/v1/contacts",
        Some(&bob.token),
        json!({ "peer": "/fred/main", "admission": "allow" }),
    )
    .await;
    for _ in 0..3 {
        assert_eq!(
            admit(&srv, &friend.me.address, &bob.me.address).await.1["decision"],
            "ring"
        );
    }
    assert_eq!(
        admit(&srv, &friend.me.address, &bob.me.address).await.1["decision"],
        "silence"
    );
}

async fn event(srv: &Server, body: Value) -> (StatusCode, Value) {
    srv.post("/internal/v1/call-events", Some(CALLS), body)
        .await
}

#[tokio::test]
async fn call_history_is_per_mailbox_and_private_to_its_owner() {
    let state = state();
    let srv = serve(state.clone()).await;
    let alice = person(&state, "acct_alice", Some("/alice/main")).await;
    let bob = person(&state, "acct_bob", None).await;
    let now = now_unix();
    let base = |id: &str, ev: &str| {
        json!({
            "event_id": id, "room_id": "roomABCDEFGH", "seq": 1, "kind": "audio", "event": ev,
            "from": { "address": alice.me.address }, "callee": bob.me.address,
            "to": [bob.me.address], "at": now,
        })
    };
    // Flag off for both: events are accepted and nothing is recorded.
    assert_eq!(event(&srv, base("e0", "ring")).await.0, StatusCode::OK);
    srv.flag("call_history", &alice.account).await;
    srv.flag("call_history", &bob.account).await;
    let (status, _) = srv.get("/v1/calls/log", Some(&bob.token)).await;
    assert_eq!(status, StatusCode::OK);

    let mut ring = base("e1", "ring");
    ring["room_id"] = json!("room2ABCDEFG");
    event(&srv, ring).await;
    let mut missed = base("e2", "missed");
    missed["room_id"] = json!("room2ABCDEFG");
    let (_, r) = event(&srv, missed.clone()).await;
    assert_eq!(r["duplicate"], false);
    let (_, r) = event(&srv, missed).await;
    assert_eq!(r["duplicate"], true);

    let (_, log) = srv.get("/v1/calls/log", Some(&bob.token)).await;
    assert_eq!(log["unseen_missed"], 1);
    let entry = &log["entries"][0];
    assert_eq!(entry["direction"], "incoming");
    assert_eq!(entry["outcome"], "missed");
    assert_eq!(entry["peer_handle"], "/alice/main");
    let id = entry["id"].as_str().unwrap().to_string();
    let (_, log) = srv.get("/v1/calls/log", Some(&alice.token)).await;
    assert_eq!(log["entries"][0]["outcome"], "missed");
    assert_eq!(log["entries"][0]["direction"], "outgoing");
    let alice_id = log["entries"][0]["id"].as_str().unwrap().to_string();

    // Somebody else's entry: the same 404 as one that never existed.
    let (s1, b1) = srv
        .delete(&format!("/v1/calls/log/{alice_id}"), Some(&bob.token))
        .await;
    let (s2, b2) = srv
        .delete("/v1/calls/log/clg_nonexistent", Some(&bob.token))
        .await;
    assert_eq!((s1, &b1), (s2, &b2));
    assert_eq!(s1, StatusCode::NOT_FOUND);
    // Nor can it be reported.
    let (status, _) = srv
        .post(
            "/v1/report-spam",
            Some(&bob.token),
            json!({ "call_id": alice_id }),
        )
        .await;
    assert_eq!(status, StatusCode::NOT_FOUND);
    // Bob reports Alice's call; it counts once.
    let (status, report) = srv
        .post(
            "/v1/report-spam",
            Some(&bob.token),
            json!({ "call_id": id }),
        )
        .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(report["reported"], true);
    let (_, again) = srv
        .post(
            "/v1/report-spam",
            Some(&bob.token),
            json!({ "call_id": id }),
        )
        .await;
    assert_eq!(again["reported"], false);
    assert_eq!(
        state
            .store
            .reports_against(alice.me.address.clone())
            .await
            .unwrap(),
        1
    );

    // Seen, then delete.
    let (_, seen) = srv
        .post(
            "/v1/calls/log/seen",
            Some(&bob.token),
            json!({ "up_to": now }),
        )
        .await;
    assert_eq!(seen["unseen_missed"], 0);
    let (status, _) = srv
        .delete(&format!("/v1/calls/log/{id}"), Some(&bob.token))
        .await;
    assert_eq!(status, StatusCode::OK);
    let (_, log) = srv.get("/v1/calls/log", Some(&bob.token)).await;
    assert!(log["entries"].as_array().unwrap().is_empty());
    // Clear all on Alice's side.
    let (_, cleared) = srv.delete("/v1/calls/log", Some(&alice.token)).await;
    assert_eq!(cleared["deleted"], 1);

    // Malformed events are refused.
    let (status, _) = event(&srv, json!({ "event_id": "x" })).await;
    assert_eq!(status, StatusCode::BAD_REQUEST);
    let mut future = base("e9", "ring");
    future["at"] = json!(now + 86_400);
    assert_eq!(event(&srv, future).await.0, StatusCode::BAD_REQUEST);
}

#[tokio::test]
async fn a_filtered_call_shows_only_in_the_callees_filtered_section() {
    let state = state();
    let srv = serve(state.clone()).await;
    let alice = person(&state, "acct_alice", None).await;
    let bob = person(&state, "acct_bob", None).await;
    srv.flag("call_history", &alice.account).await;
    srv.flag("call_history", &bob.account).await;
    let now = now_unix();
    let ev = |id: &str, kind: &str, to: Value| {
        json!({
            "event_id": id, "room_id": "roomFILTERED", "kind": "video", "event": kind,
            "from": { "address": alice.me.address }, "callee": bob.me.address, "to": to, "at": now,
        })
    };
    event(&srv, ev("f1", "ring", json!([]))).await;
    event(&srv, ev("f2", "filtered", json!([bob.me.address]))).await;
    event(&srv, ev("f3", "missed", json!([]))).await;
    let (_, all) = srv.get("/v1/calls/log", Some(&bob.token)).await;
    assert!(all["entries"].as_array().unwrap().is_empty());
    assert_eq!(all["unseen_missed"], 0);
    let (_, filtered) = srv
        .get("/v1/calls/log?filter=filtered", Some(&bob.token))
        .await;
    assert_eq!(filtered["entries"][0]["outcome"], "filtered");
    // The caller's own history reads exactly like an unanswered call.
    let (_, mine) = srv.get("/v1/calls/log", Some(&alice.token)).await;
    assert_eq!(mine["entries"][0]["outcome"], "missed");
}

fn sample_png(w: u32, h: u32) -> Vec<u8> {
    let img = image::RgbaImage::from_fn(w, h, |x, y| {
        image::Rgba([(x % 256) as u8, (y % 256) as u8, 128, 255])
    });
    let mut out = Vec::new();
    image::DynamicImage::ImageRgba8(img)
        .write_to(&mut std::io::Cursor::new(&mut out), image::ImageFormat::Png)
        .unwrap();
    out
}

async fn upload(
    srv: &Server,
    token: &str,
    query: &str,
    bytes: Vec<u8>,
    ct: &str,
) -> (StatusCode, Value) {
    let resp = srv
        .http
        .put(format!("{}/v1/profile/avatar{query}", srv.public))
        .bearer_auth(token)
        .header("content-type", ct)
        .body(bytes)
        .send()
        .await
        .unwrap();
    let status = StatusCode::from_u16(resp.status().as_u16()).unwrap();
    (status, resp.json().await.unwrap_or(Value::Null))
}

#[tokio::test]
async fn profile_pictures_honour_visibility_everywhere() {
    let state = state();
    let srv = serve(state.clone()).await;
    let owner = person(&state, "acct_o", Some("/olga/main")).await;
    let friend = person(&state, "acct_f", None).await;
    let stranger = person(&state, "acct_s", None).await;

    // Off by default.
    let (status, _) = upload(&srv, &owner.token, "", sample_png(64, 64), "image/png").await;
    assert_eq!(status, StatusCode::NOT_FOUND);
    srv.flag("avatars", &owner.account).await;

    let (status, set) = upload(
        &srv,
        &owner.token,
        "?crop=0,0,200,200&visibility=known",
        sample_png(300, 200),
        "image/png",
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{set}");
    let avatar = &set["avatar"];
    assert_eq!(avatar["visibility"], "known");
    let url_128 = avatar["urls"]["128"].as_str().unwrap().to_string();
    // The owner knows the friend (an allowed contact); the stranger is nobody to them.
    srv.put(
        "/v1/contacts",
        Some(&owner.token),
        json!({ "peer": friend.me.address, "admission": "allow" }),
    )
    .await;

    let path = format!("/v1/avatars/{}", enc("/olga/main"));
    let (_, anon) = srv.get(&path, None).await;
    let (_, strange) = srv.get(&path, Some(&stranger.token)).await;
    let (_, known) = srv.get(&path, Some(&friend.token)).await;
    let (_, nobody_there) = srv
        .get(
            &format!("/v1/avatars/{}", enc("/k/nothing")),
            Some(&stranger.token),
        )
        .await;
    assert!(anon["avatar"].is_null());
    assert!(strange["avatar"].is_null());
    assert!(nobody_there["avatar"].is_null());
    assert_eq!(known["avatar"]["urls"]["128"], url_128);
    assert!(
        known["avatar"]["visibility"].is_null(),
        "the setting is the owner's business"
    );

    // Blobs are authorized on every request.
    let fetch = |token: Option<String>, url: String| {
        let http = srv.http.clone();
        let base = srv.public.clone();
        async move {
            let mut req = http.get(format!("{base}{url}"));
            if let Some(t) = token {
                req = req.bearer_auth(t);
            }
            req.send().await.unwrap()
        }
    };
    let ok = fetch(Some(friend.token.clone()), url_128.clone()).await;
    assert_eq!(ok.status().as_u16(), 200);
    assert_eq!(ok.headers()["content-type"], "image/webp");
    assert_eq!(ok.headers()["cache-control"], "private, max-age=300");
    let bytes = ok.bytes().await.unwrap();
    assert_eq!(image::load_from_memory(&bytes).unwrap().width(), 128);
    assert_eq!(
        fetch(Some(stranger.token.clone()), url_128.clone())
            .await
            .status()
            .as_u16(),
        404
    );
    assert_eq!(fetch(None, url_128.clone()).await.status().as_u16(), 404);

    // Everyone: public and cacheable; the old URL dies with the version bump.
    let (status, patched) = srv
        .call(
            reqwest::Method::PATCH,
            "/v1/profile/avatar",
            Some(&owner.token),
            Some(json!({ "visibility": "everyone" })),
        )
        .await;
    assert_eq!(status, StatusCode::OK);
    let public_128 = patched["avatar"]["urls"]["128"]
        .as_str()
        .unwrap()
        .to_string();
    assert_ne!(public_128, url_128);
    let public = fetch(None, public_128.clone()).await;
    assert_eq!(public.status().as_u16(), 200);
    assert_eq!(
        public.headers()["cache-control"],
        "public, max-age=31536000, immutable"
    );
    assert_eq!(
        fetch(Some(friend.token.clone()), url_128)
            .await
            .status()
            .as_u16(),
        404
    );
    let (_, bulk) = srv
        .get(
            &format!(
                "/v1/avatars?addresses={},{}",
                enc("/olga/main"),
                enc(&stranger.me.address)
            ),
            None,
        )
        .await;
    assert!(bulk["avatars"]["/olga/main"]["urls"].is_object());
    assert!(bulk["avatars"][stranger.me.address.as_str()].is_null());

    // Nobody: not even the friend.
    srv.call(
        reqwest::Method::PATCH,
        "/v1/profile/avatar",
        Some(&owner.token),
        Some(json!({ "visibility": "nobody" })),
    )
    .await;
    let (_, known) = srv.get(&path, Some(&friend.token)).await;
    assert!(known["avatar"].is_null());
    let (_, own) = srv.get("/v1/profile/avatar", Some(&owner.token)).await;
    assert_eq!(own["avatar"]["visibility"], "nobody");

    // The owner's account heard about each change.
    let batch = state
        .store
        .stream_after(
            owner.account.clone(),
            vec![owner.me.address.clone()],
            store::StreamCursor::default(),
            true,
            100,
        )
        .await
        .unwrap();
    assert!(
        batch
            .typed
            .iter()
            .filter(|e| e.kind == "profile.avatar")
            .count()
            >= 3
    );
}

#[tokio::test]
async fn hostile_uploads_are_refused_and_changes_are_rate_limited() {
    let state = state();
    let srv = serve(state.clone()).await;
    let owner = person(&state, "acct_o", None).await;
    srv.flag("avatars", &owner.account).await;
    let (status, body) = upload(&srv, &owner.token, "", b"<svg/>".to_vec(), "image/svg+xml").await;
    assert_eq!(
        (status, body["error"].as_str()),
        (
            StatusCode::UNSUPPORTED_MEDIA_TYPE,
            Some("unsupported_image")
        )
    );
    let (status, _) = upload(&srv, &owner.token, "", b"GIF89a....".to_vec(), "image/gif").await;
    assert_eq!(status, StatusCode::UNSUPPORTED_MEDIA_TYPE);
    let mut broken = sample_png(32, 32);
    broken.truncate(60);
    let (status, body) = upload(&srv, &owner.token, "", broken, "image/png").await;
    assert_eq!(
        (status, body["error"].as_str()),
        (StatusCode::UNPROCESSABLE_ENTITY, Some("bad_image"))
    );
    let (status, _) = upload(
        &srv,
        &owner.token,
        "",
        vec![0u8; avatars::MAX_UPLOAD + 10],
        "image/png",
    )
    .await;
    assert_eq!(status, StatusCode::PAYLOAD_TOO_LARGE);
    let (status, _) = upload(
        &srv,
        &owner.token,
        "?crop=nope",
        sample_png(32, 32),
        "image/png",
    )
    .await;
    assert_eq!(status, StatusCode::BAD_REQUEST);

    for i in 0..5 {
        let (status, _) = upload(&srv, &owner.token, "", sample_png(40 + i, 40), "image/png").await;
        assert_eq!(status, StatusCode::OK, "change {i}");
    }
    let (status, body) = upload(&srv, &owner.token, "", sample_png(90, 90), "image/png").await;
    assert_eq!(
        (status, body["error"].as_str()),
        (StatusCode::TOO_MANY_REQUESTS, Some("avatar_rate_limited"))
    );
    // Removing is always allowed.
    let (status, removed) = srv.delete("/v1/profile/avatar", Some(&owner.token)).await;
    assert_eq!(
        (status, removed["removed"].as_bool()),
        (StatusCode::OK, Some(true))
    );
}

#[tokio::test]
async fn reported_pictures_reach_the_operator_who_can_take_them_down() {
    let state = state();
    let srv = serve(state.clone()).await;
    let owner = person(&state, "acct_o", Some("/oscar/main")).await;
    let viewer = person(&state, "acct_v", None).await;
    srv.flag("avatars", &owner.account).await;
    upload(
        &srv,
        &owner.token,
        "?visibility=everyone",
        sample_png(80, 80),
        "image/png",
    )
    .await;
    let (status, _) = srv
        .post(
            "/v1/report-avatar",
            Some(&viewer.token),
            json!({ "address": "/oscar/main" }),
        )
        .await;
    assert_eq!(status, StatusCode::OK);
    // A picture you cannot see cannot be reported.
    let (status, _) = srv
        .post(
            "/v1/report-avatar",
            Some(&viewer.token),
            json!({ "address": "/k/nobody" }),
        )
        .await;
    assert_eq!(status, StatusCode::NOT_FOUND);
    // Only the operator sees reports.
    assert_eq!(
        srv.get("/internal/v1/avatar-reports", Some(CALLS)).await.0,
        StatusCode::FORBIDDEN
    );
    let (_, reports) = srv.get("/internal/v1/avatar-reports", Some(OPS)).await;
    let id = reports["reports"][0]["avatar_id"]
        .as_str()
        .unwrap()
        .to_string();
    assert_eq!(reports["reports"][0]["reports"], 1);
    let (status, _) = srv
        .post(
            &format!("/internal/v1/avatars/{id}/remove"),
            Some(OPS),
            json!({}),
        )
        .await;
    assert_eq!(status, StatusCode::OK);
    let (_, after) = srv
        .get(&format!("/v1/avatars/{}", enc("/oscar/main")), None)
        .await;
    assert!(after["avatar"].is_null());
}
