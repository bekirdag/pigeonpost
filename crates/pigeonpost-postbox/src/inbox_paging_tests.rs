//! Phase 9b: bounded inbox listings, change-based long polls, and admission control.
//!
//! The listing is checked against the old whole-history read (`InboxPage::legacy`) for every
//! combination of options the shipped clients use, on a mailbox with read and unread mail, edits,
//! replies, sent copies and a second thread.

use crate::*;
use reqwest::Client;
use serde_json::{json, Value};
use std::time::{Duration, Instant};

const OPS: &str = "svc_ops_0123456789abcdef0123456789abcdef";

fn state() -> AppState {
    let mut state = crate::tests::test_state();
    state.service_tokens = Arc::new(internal_api::tokens_for_test(&[(
        "POSTBOX_SVC_TOKEN_OPS",
        OPS,
    )]));
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
    async fn get(&self, path: &str, token: &str) -> (StatusCode, Value) {
        self.call(reqwest::Method::GET, path, Some(token), None)
            .await
    }
    async fn post(&self, path: &str, token: &str, body: Value) -> (StatusCode, Value) {
        self.call(reqwest::Method::POST, path, Some(token), Some(body))
            .await
    }
    async fn flags(&self, account: &str) {
        for name in ["message_edit", "message_reply", "receipts", "agent_status"] {
            let (status, _) = self
                .call(
                    reqwest::Method::PUT,
                    &format!("/internal/v1/features/{name}"),
                    Some(OPS),
                    Some(json!({ "scope": "account", "account_id": account, "enabled": true })),
                )
                .await;
            assert_eq!(status, StatusCode::OK, "flag {name}");
        }
    }
    async fn send(&self, from: &Person, to: &Person, body: Value) -> Value {
        let mut req = json!({ "to": to.me.address });
        for (k, v) in body.as_object().unwrap() {
            req[k] = v.clone();
        }
        let (status, sent) = self.post("/v1/send", &from.token, req).await;
        assert_eq!(status, StatusCode::CREATED, "{sent}");
        sent
    }
    async fn ack(&self, who: &Person, id: &str) {
        let (status, body) = self
            .post("/v1/ack", &who.token, json!({ "message_id": id }))
            .await;
        assert_eq!(status, StatusCode::OK, "{body}");
    }
}

struct Person {
    token: String,
    account: String,
    me: store::StoredIdentity,
}

async fn person(state: &AppState, account: &str) -> Person {
    let (token, key) = new_api_key();
    state
        .store
        .create_account(account.into(), key, now_unix())
        .await
        .unwrap();
    let v = do_create_identity(state, Some(account.into()), None, None)
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

/// A second credential for the same account: another signed-in device.
async fn second_device(state: &AppState, who: &Person) -> String {
    let (token, key) = new_api_key();
    state
        .store
        .add_api_key(who.account.clone(), key, now_unix())
        .await
        .unwrap();
    token
}

/// Bob's mailbox: read and unread mail, an edit of a read message, a reply, his own sent copies,
/// and a second thread.
async fn populated(srv: &Server, state: &AppState) -> (Person, Person, String) {
    let a = person(state, "acct_a").await;
    let b = person(state, "acct_b").await;
    srv.flags(&a.account).await;
    srv.flags(&b.account).await;
    for i in 0..12 {
        let sent = srv
            .send(&a, &b, json!({ "body": format!("filler {i}") }))
            .await;
        if i % 2 == 0 {
            srv.ack(&b, sent["message_id"].as_str().unwrap()).await;
        }
    }
    // Read, then edited: folded, the original shows as unread again.
    let m2 = srv.send(&a, &b, json!({ "body": "draft" })).await;
    srv.ack(&b, m2["message_id"].as_str().unwrap()).await;
    srv.send(&a, &b, json!({ "body": "final", "edit_of": m2["msg_ref"] }))
        .await;
    // Bob asks, Alice replies quoting him.
    let q = srv.send(&b, &a, json!({ "body": "question?" })).await;
    srv.send(
        &a,
        &b,
        json!({ "body": "answer", "reply_to": q["msg_ref"] }),
    )
    .await;
    // Another thread.
    let t = srv
        .send(&a, &b, json!({ "body": "in a thread", "thread": "deploy" }))
        .await;
    let thread_id = state
        .store
        .threads(b.me.address.clone(), None)
        .await
        .unwrap()
        .into_iter()
        .find(|th| !th.is_default)
        .map(|th| th.id)
        .expect("named thread");
    let _ = t;
    for i in 0..5 {
        srv.send(&b, &a, json!({ "body": format!("reply {i}") }))
            .await;
    }
    (a, b, thread_id)
}

fn strip(mut v: Value) -> Value {
    if let Some(obj) = v.as_object_mut() {
        obj.remove("page");
        obj.remove("version");
    }
    if let Some(rows) = v["messages"].as_array_mut() {
        for row in rows {
            if let Some(obj) = row.as_object_mut() {
                obj.remove("seq");
            }
        }
    }
    v
}

#[tokio::test]
async fn pages_list_exactly_what_the_whole_history_listed() {
    let state = state();
    let srv = serve(state.clone()).await;
    let (_a, b, thread) = populated(&srv, &state).await;
    let views = [
        msgactions::View {
            actions: false,
            fold: false,
        },
        msgactions::View {
            actions: true,
            fold: false,
        },
        msgactions::View {
            actions: true,
            fold: true,
        },
    ];
    let mut compared = 0;
    for include_sent in [false, true] {
        for include_read in [false, true] {
            for filtered in [false, true] {
                for thread_id in [None, Some(thread.as_str())] {
                    for view in views {
                        let old = do_inbox_page(
                            &state,
                            &b.me,
                            include_sent,
                            include_read,
                            thread_id,
                            filtered,
                            view,
                            InboxPage {
                                legacy: true,
                                ..InboxPage::default()
                            },
                        )
                        .await
                        .unwrap();
                        let new = do_inbox_page(
                            &state,
                            &b.me,
                            include_sent,
                            include_read,
                            thread_id,
                            filtered,
                            view,
                            InboxPage::default(),
                        )
                        .await
                        .unwrap();
                        assert_eq!(
                            strip(new.clone()),
                            strip(old),
                            "sent={include_sent} read={include_read} filtered={filtered} \
                             thread={thread_id:?} view={view:?}"
                        );
                        assert_eq!(new["page"]["has_more"], false);
                        compared += new["messages"].as_array().unwrap().len();
                    }
                }
            }
        }
    }
    assert!(
        compared > 100,
        "the combinations listed something: {compared}"
    );
}

#[tokio::test]
async fn a_history_listing_pages_back_and_forward_without_gaps() {
    let state = state();
    let srv = serve(state.clone()).await;
    let (a, b, _) = populated(&srv, &state).await;
    let (_, full) = srv
        .get("/v1/inbox?include_sent=true&include_read=true", &b.token)
        .await;
    let all: Vec<String> = full["messages"]
        .as_array()
        .unwrap()
        .iter()
        .map(|m| m["message_id"].as_str().unwrap().to_string())
        .collect();
    assert!(all.len() > 20);

    // Back from the newest, five at a time.
    let mut seen: Vec<String> = Vec::new();
    let mut before: Option<i64> = None;
    loop {
        let q = match before {
            Some(s) => format!("&before={s}"),
            None => String::new(),
        };
        let (status, page) = srv
            .get(
                &format!("/v1/inbox?include_sent=true&include_read=true&limit=5{q}"),
                &b.token,
            )
            .await;
        assert_eq!(status, StatusCode::OK, "{page}");
        let rows = page["messages"].as_array().unwrap();
        assert!(rows.len() <= 5);
        // Each page is in the listing's order, oldest first.
        let seqs: Vec<i64> = rows.iter().map(|m| m["seq"].as_i64().unwrap()).collect();
        assert!(seqs.windows(2).all(|w| w[0] < w[1]), "{seqs:?}");
        let mut ids: Vec<String> = rows
            .iter()
            .map(|m| m["message_id"].as_str().unwrap().to_string())
            .collect();
        ids.extend(seen);
        seen = ids;
        if page["page"]["has_more"] != true {
            break;
        }
        before = page["page"]["oldest_seq"].as_i64();
    }
    assert_eq!(seen, all, "the pages put together are the listing");

    // Forward from the newest: nothing, then exactly what arrives.
    let newest = full["page"]["newest_seq"].as_i64().unwrap();
    let (_, none) = srv
        .get(
            &format!("/v1/inbox?include_sent=true&include_read=true&after={newest}"),
            &b.token,
        )
        .await;
    assert_eq!(none["messages"].as_array().unwrap().len(), 0);
    assert_eq!(none["page"]["newest_seq"], Value::Null);
    let sent = srv.send(&a, &b, json!({ "body": "new" })).await;
    let (_, delta) = srv
        .get(
            &format!("/v1/inbox?include_sent=true&include_read=true&after={newest}"),
            &b.token,
        )
        .await;
    let rows = delta["messages"].as_array().unwrap();
    assert_eq!(rows.len(), 1);
    assert_eq!(rows[0]["message_id"], sent["message_id"]);
    assert!(rows[0]["seq"].as_i64().unwrap() > newest);

    // Bad cursors are refused, not guessed at.
    let (status, _) = srv
        .get("/v1/inbox?include_read=true&after=1&before=9", &b.token)
        .await;
    assert_eq!(status, StatusCode::BAD_REQUEST);
}

#[tokio::test]
async fn an_unread_poll_reads_only_unread_rows_and_drains_oldest_first() {
    let state = state();
    let srv = serve(state.clone()).await;
    let (_a, b, _) = populated(&srv, &state).await;
    let (_, unread) = srv.get("/v1/inbox", &b.token).await;
    let total = unread["messages"].as_array().unwrap().len();
    assert!(total >= 6, "{unread}");
    let (_, first) = srv.get("/v1/inbox?limit=2", &b.token).await;
    let first_ids: Vec<&Value> = first["messages"]
        .as_array()
        .unwrap()
        .iter()
        .map(|m| &m["message_id"])
        .collect();
    let all_ids: Vec<&Value> = unread["messages"]
        .as_array()
        .unwrap()
        .iter()
        .map(|m| &m["message_id"])
        .collect();
    assert_eq!(first_ids, all_ids[..2].to_vec(), "oldest unread first");
    assert_eq!(first["page"]["has_more"], true);

    // The unread step is an index lookup, not a scan of the mailbox.
    let plan = state
        .store
        .explain_unread(b.me.address.clone())
        .await
        .unwrap();
    assert!(plan.contains("messages_unread_by_mailbox"), "{plan}");
}

#[tokio::test]
async fn a_history_long_poll_waits_for_a_change_not_for_unread_mail() {
    let state = state();
    let srv = serve(state.clone()).await;
    let a = person(&state, "acct_a").await;
    let b = person(&state, "acct_b").await;
    srv.send(&a, &b, json!({ "body": "sits unread" })).await;
    let history = "/v1/inbox?include_sent=true&include_read=true";

    // A client this server has not answered gets an answer at once.
    let started = Instant::now();
    let (status, first) = srv.get(&format!("{history}&wait=2"), &b.token).await;
    assert_eq!(status, StatusCode::OK);
    assert!(started.elapsed() < Duration::from_millis(1500));
    let version = first["version"].as_str().unwrap().to_string();

    // Unread mail no longer ends the wait: before 9b this returned at once, every time.
    let started = Instant::now();
    srv.get(&format!("{history}&wait=1"), &b.token).await;
    assert!(started.elapsed() >= Duration::from_millis(900), "waited");

    // New mail does.
    let srv = Arc::new(srv);
    let (s2, a2, b2) = (srv.clone(), a.token.clone(), b.me.address.clone());
    let sender = tokio::spawn(async move {
        tokio::time::sleep(Duration::from_millis(300)).await;
        s2.post("/v1/send", &a2, json!({ "to": b2, "body": "new" }))
            .await
    });
    let started = Instant::now();
    let (_, after) = srv.get(&format!("{history}&wait=5"), &b.token).await;
    assert!(
        started.elapsed() < Duration::from_secs(3),
        "woken by the send"
    );
    sender.await.unwrap();
    assert_eq!(after["messages"].as_array().unwrap().len(), 2);

    // A read on another device does too.
    let other = second_device(&state, &b).await;
    let id = after["messages"][0]["message_id"]
        .as_str()
        .unwrap()
        .to_string();
    let (s3, o3) = (srv.clone(), other.clone());
    let acker = tokio::spawn(async move {
        tokio::time::sleep(Duration::from_millis(300)).await;
        s3.post("/v1/ack", &o3, json!({ "message_id": id })).await
    });
    let started = Instant::now();
    let (_, acked) = srv.get(&format!("{history}&wait=5"), &b.token).await;
    assert!(
        started.elapsed() < Duration::from_secs(3),
        "woken by the ack"
    );
    acker.await.unwrap();
    assert_eq!(acked["messages"][0]["read"], true);

    // `since`: a current version waits, a stale or foreign one answers at once. (The ack's
    // receipt may land just after the woken listing; take the version once it has.)
    let (_, settled) = srv.get(history, &b.token).await;
    let current = settled["version"].as_str().unwrap();
    let started = Instant::now();
    srv.get(&format!("{history}&wait=1&since={current}"), &other)
        .await;
    assert!(started.elapsed() >= Duration::from_millis(900));
    for stale in [version.as_str(), "0000000000000000.0", "junk"] {
        let started = Instant::now();
        srv.get(&format!("{history}&wait=2&since={stale}"), &other)
            .await;
        assert!(started.elapsed() < Duration::from_millis(1500), "{stale}");
    }

    // An unread poll still returns while there is unread mail (CLI `watch`, agents).
    srv.send(&a, &b, json!({ "body": "for the agent" })).await;
    let started = Instant::now();
    let (_, unread) = srv.get("/v1/inbox?wait=5", &b.token).await;
    assert!(started.elapsed() < Duration::from_millis(1500));
    assert!(!unread["messages"].as_array().unwrap().is_empty());
}

#[tokio::test]
async fn read_thread_reads_only_the_newest_rows_but_counts_them_all() {
    let state = state();
    let srv = serve(state.clone()).await;
    let a = person(&state, "acct_a").await;
    let b = person(&state, "acct_b").await;
    for i in 0..9 {
        srv.send(&a, &b, json!({ "body": format!("m{i}") })).await;
    }
    let thread = state
        .store
        .threads(b.me.address.clone(), None)
        .await
        .unwrap()
        .remove(0);
    let v = do_read_thread(&state, &b.me, &thread.id, 4).await.unwrap();
    assert_eq!(v["message_count"], 9);
    assert_eq!(v["returned"], 4);
    let bodies: Vec<&str> = v["messages"]
        .as_array()
        .unwrap()
        .iter()
        .map(|m| m["body"].as_str().unwrap())
        .collect();
    assert_eq!(bodies, ["m5", "m6", "m7", "m8"]);
}

// ---- admission ----------------------------------------------------------------------------------

async fn tiny(limits: overload::Limits) -> String {
    use axum::routing::{get, post};
    let app = axum::Router::new()
        .route(
            "/slow",
            get(|| async {
                tokio::time::sleep(Duration::from_millis(400)).await;
                "ok"
            }),
        )
        .route(
            "/hang",
            get(|| async {
                tokio::time::sleep(Duration::from_secs(30)).await;
                "late"
            }),
        )
        .route("/echo", post(|body: String| async move { body.len().to_string() }))
        .route(
            "/v1/events",
            get(|| async {
                let s = async_stream::stream! {
                    loop {
                        yield Ok::<_, std::convert::Infallible>(axum::body::Bytes::from_static(b": tick\n\n"));
                        tokio::time::sleep(Duration::from_millis(50)).await;
                    }
                };
                axum::body::Body::from_stream(s)
            }),
        )
        .layer(axum::extract::DefaultBodyLimit::max(
            overload::CONTROL_BODY_MAX,
        ))
        .layer(axum::middleware::from_fn_with_state(
            Arc::new(limits),
            overload::admission,
        ));
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let url = format!("http://{}", listener.local_addr().unwrap());
    tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
    url
}

fn limits(inflight: usize, streams: usize) -> overload::Limits {
    overload::Limits::new(
        Duration::from_millis(1000),
        Duration::from_millis(50),
        Duration::from_millis(600),
        inflight,
        10,
        streams,
        4,
    )
}

#[tokio::test]
async fn over_the_concurrency_limit_requests_are_shed_with_retry_after() {
    let url = tiny(limits(1, 1)).await;
    let http = Client::new();
    let first = tokio::spawn({
        let (http, url) = (http.clone(), url.clone());
        async move {
            http.get(format!("{url}/slow"))
                .send()
                .await
                .unwrap()
                .status()
        }
    });
    tokio::time::sleep(Duration::from_millis(100)).await;
    let shed = http.get(format!("{url}/slow")).send().await.unwrap();
    assert_eq!(shed.status().as_u16(), 503);
    assert_eq!(shed.headers()["retry-after"], "1");
    let body: Value = shed.json().await.unwrap();
    assert_eq!(body["error"], "overloaded");
    assert_eq!(first.await.unwrap().as_u16(), 200);
    // The slot came back.
    assert_eq!(
        http.get(format!("{url}/slow"))
            .send()
            .await
            .unwrap()
            .status()
            .as_u16(),
        200
    );
}

#[tokio::test]
async fn a_request_past_its_time_limit_is_cut_off() {
    let url = tiny(limits(4, 1)).await;
    let started = Instant::now();
    let resp = Client::new()
        .get(format!("{url}/hang"))
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status().as_u16(), 503);
    assert_eq!(resp.headers()["retry-after"], "2");
    assert!(started.elapsed() < Duration::from_secs(3));
    let body: Value = resp.json().await.unwrap();
    assert_eq!(body["error"], "request_timeout");
}

#[tokio::test]
async fn streams_have_their_own_slots_and_end_after_their_lifetime() {
    let url = tiny(limits(1, 1)).await;
    let http = Client::new();
    let started = Instant::now();
    let stream = http.get(format!("{url}/v1/events")).send().await.unwrap();
    assert_eq!(stream.status().as_u16(), 200);
    // The one stream slot is taken; ordinary requests are unaffected by it.
    let second = http.get(format!("{url}/v1/events")).send().await.unwrap();
    assert_eq!(second.status().as_u16(), 503);
    assert_eq!(
        http.get(format!("{url}/slow"))
            .send()
            .await
            .unwrap()
            .status()
            .as_u16(),
        200
    );
    // Not cut at the 1 s request timeout, but ended at its ~600 ms lifetime... measured from
    // admission, so allow for the jitter.
    let body = stream.bytes().await.unwrap();
    assert!(!body.is_empty());
    let lived = started.elapsed();
    assert!(
        lived >= Duration::from_millis(500) && lived < Duration::from_secs(3),
        "{lived:?}"
    );
    // And its slot is free again.
    let again = http.get(format!("{url}/v1/events")).send().await.unwrap();
    assert_eq!(again.status().as_u16(), 200);
}

#[tokio::test]
async fn bodies_are_limited_by_route_class() {
    let url = tiny(limits(4, 1)).await;
    let http = Client::new();
    let big = "x".repeat(overload::CONTROL_BODY_MAX + 1);
    let resp = http
        .post(format!("{url}/echo"))
        .body(big)
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status().as_u16(), 413);
    let ok = http
        .post(format!("{url}/echo"))
        .body("x".repeat(1000))
        .send()
        .await
        .unwrap();
    assert_eq!(ok.status().as_u16(), 200);
    let get_with_body = http
        .get(format!("{url}/slow"))
        .body(vec![0u8; overload::BODYLESS_MAX as usize + 1])
        .send()
        .await
        .unwrap();
    assert_eq!(get_with_body.status().as_u16(), 413);

    // On the real router: a message route takes up to 2 MiB, a settings route 512 KiB.
    let state = state();
    let srv = serve(state.clone()).await;
    let a = person(&state, "acct_a").await;
    let b = person(&state, "acct_b").await;
    // (A direct message is at most 64 KiB of text, refused by the envelope, not by size class.)
    let body = "y".repeat(700 * 1024);
    let (status, _) = srv
        .post(
            "/v1/send",
            &a.token,
            json!({ "to": b.me.address, "body": body }),
        )
        .await;
    assert_ne!(status, StatusCode::PAYLOAD_TOO_LARGE);
    let (status, sent) = srv
        .post(
            "/v1/send",
            &a.token,
            json!({ "to": b.me.address, "body": "fits" }),
        )
        .await;
    assert_eq!(status, StatusCode::CREATED, "{sent}");
    let (status, _) = srv
        .call(
            reqwest::Method::PUT,
            "/v1/contacts",
            Some(&a.token),
            Some(json!({ "peer": b.me.address, "alias": "z".repeat(600 * 1024) })),
        )
        .await;
    assert_eq!(status, StatusCode::PAYLOAD_TOO_LARGE);
}
