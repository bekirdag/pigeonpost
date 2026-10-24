//! End-to-end tests for Phase 1e over real sockets: edits, replies and receipts (SDS §14C).

use crate::*;
use reqwest::Client;
use serde_json::{json, Value};

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
    async fn put(&self, path: &str, token: &str, body: Value) -> (StatusCode, Value) {
        self.call(reqwest::Method::PUT, path, Some(token), Some(body))
            .await
    }
    async fn flag(&self, name: &str, account: &str) {
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
    async fn all_flags(&self, account: &str) {
        for f in ["message_edit", "message_reply", "receipts", "agent_status"] {
            self.flag(f, account).await;
        }
    }
    async fn send(&self, from: &Person, to: &Person, body: Value) -> (StatusCode, Value) {
        let mut req = json!({ "to": to.me.address });
        for (k, v) in body.as_object().unwrap() {
            req[k] = v.clone();
        }
        self.post("/v1/send", &from.token, req).await
    }
    /// The mailbox's conversation, in a given view.
    async fn listing(&self, who: &Person, query: &str) -> Vec<Value> {
        let (status, body) = self
            .get(
                &format!("/v1/inbox?include_sent=true&include_read=true{query}"),
                &who.token,
            )
            .await;
        assert_eq!(status, StatusCode::OK, "{body}");
        body["messages"].as_array().cloned().unwrap_or_default()
    }
    async fn sent_copy(&self, who: &Person, msg_ref: &str) -> Value {
        self.listing(who, "&actions=1")
            .await
            .into_iter()
            .find(|m| m["direction"] == "out" && m["msg_ref"] == msg_ref)
            .expect("sent copy")
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

async fn typed_events(state: &AppState, account: &str, kind: &str) -> Vec<Value> {
    let batch = state
        .store
        .stream_after(
            account.into(),
            Vec::new(),
            store::StreamCursor { mail: 0, event: 0 },
            true,
            1000,
        )
        .await
        .unwrap();
    batch
        .typed
        .into_iter()
        .filter(|e| e.kind == kind)
        .map(|e| serde_json::from_str(&e.data).unwrap())
        .collect()
}

fn envelope(verb: &str, task: &str) -> String {
    json!({ "v": 1, "verb": verb, "args": { "task": task }, "note": task }).to_string()
}

#[tokio::test]
async fn message_actions_are_refused_while_their_flags_are_off() {
    let state = state();
    let srv = serve(state.clone()).await;
    let a = person(&state, "acct_a").await;
    let b = person(&state, "acct_b").await;
    let (status, sent) = srv.send(&a, &b, json!({ "body": "hi" })).await;
    assert_eq!(status, StatusCode::CREATED);
    // Every message gets a reference even from a client that sent none.
    let r = sent["msg_ref"].as_str().unwrap().to_string();
    assert!(msgactions::is_msg_ref(&r));
    let (status, body) = srv
        .send(&a, &b, json!({ "body": "hi!", "edit_of": r }))
        .await;
    assert_eq!(
        (status, body["error"].as_str()),
        (StatusCode::FORBIDDEN, Some("feature_disabled"))
    );
    let (status, body) = srv
        .send(&b, &a, json!({ "body": "yes", "reply_to": r }))
        .await;
    assert_eq!(
        (status, body["error"].as_str()),
        (StatusCode::FORBIDDEN, Some("feature_disabled"))
    );
    // No receipt fields without the receipts flag.
    let copy = srv.sent_copy(&a, &r).await;
    assert!(copy.get("receipt").is_none());
}

#[tokio::test]
async fn concurrent_retries_deliver_one_message_with_one_reference() {
    let state = state();
    let srv = Arc::new(serve(state.clone()).await);
    let a = Arc::new(person(&state, "acct_retry_a").await);
    let b = Arc::new(person(&state, "acct_retry_b").await);
    let msg_ref = msgactions::new_msg_ref();
    let gate = Arc::new(tokio::sync::Barrier::new(17));
    let mut sends = tokio::task::JoinSet::new();
    for _ in 0..16 {
        let (srv, a, b, msg_ref, gate) = (
            srv.clone(),
            a.clone(),
            b.clone(),
            msg_ref.clone(),
            gate.clone(),
        );
        sends.spawn(async move {
            gate.wait().await;
            srv.send(
                &a,
                &b,
                json!({ "body": "one delivery", "msg_ref": msg_ref }),
            )
            .await
        });
    }
    gate.wait().await;
    let mut deliveries = std::collections::HashSet::new();
    let mut created = 0;
    while let Some(result) = sends.join_next().await {
        let (status, body) = result.unwrap();
        assert!(status.is_success(), "{status}: {body}");
        if status == StatusCode::CREATED {
            created += 1;
        } else {
            assert_eq!(status, StatusCode::OK);
            assert_eq!(body["duplicate"], true);
        }
        deliveries.insert((
            body["message_id"]
                .as_str()
                .expect("received copy id")
                .to_owned(),
            body["sent_copy_id"]
                .as_str()
                .expect("sent copy id")
                .to_owned(),
        ));
    }
    assert_eq!(created, 1);
    assert_eq!(deliveries.len(), 1);
    assert_eq!(srv.listing(&b, "&actions=1").await.len(), 1);
    assert_eq!(srv.listing(&a, "&actions=1").await.len(), 1);
}

#[tokio::test]
async fn simultaneous_senders_cannot_claim_the_same_recipient_reference() {
    let state = state();
    let srv = serve(state.clone()).await;
    let a = person(&state, "acct_ref_a").await;
    let b = person(&state, "acct_ref_b").await;
    let c = person(&state, "acct_ref_c").await;
    let msg_ref = msgactions::new_msg_ref();
    let (first, second) = tokio::join!(
        srv.send(&a, &b, json!({ "body": "one", "msg_ref": msg_ref })),
        srv.send(&c, &b, json!({ "body": "two", "msg_ref": msg_ref })),
    );
    let mut statuses = [first.0.as_u16(), second.0.as_u16()];
    statuses.sort();
    assert_eq!(statuses, [201, 409], "{first:?}, {second:?}");
    assert_eq!(srv.listing(&b, "&actions=1").await.len(), 1);
    assert_eq!(
        srv.listing(&a, "&actions=1").await.len() + srv.listing(&c, "&actions=1").await.len(),
        1
    );
}

#[tokio::test]
async fn editing_after_disabling_a_timer_does_not_extend_the_original_expiry() {
    let state = state();
    let srv = serve(state.clone()).await;
    let a = person(&state, "acct_expiry_a").await;
    let b = person(&state, "acct_expiry_b").await;
    srv.all_flags(&a.account).await;
    srv.flag("disappearing", &a.account).await;
    let (_, bootstrap) = srv.send(&a, &b, json!({ "body": "bootstrap" })).await;
    let thread = srv
        .listing(&a, "&actions=1")
        .await
        .into_iter()
        .find(|m| m["message_id"] == bootstrap["sent_copy_id"])
        .unwrap()["thread_id"]
        .as_str()
        .unwrap()
        .to_string();
    let timer_path = format!("/v1/threads/{thread}");
    let (status, _) = srv
        .call(
            reqwest::Method::PATCH,
            &timer_path,
            Some(&a.token),
            Some(json!({ "ephemeral_secs": 300 })),
        )
        .await;
    assert_eq!(status, StatusCode::OK);
    let (_, original) = srv
        .send(&a, &b, json!({ "body": "temporary secret" }))
        .await;
    let original_id = original["message_id"].as_str().unwrap().to_string();
    let expires_at = state
        .store
        .message_expiries(vec![original_id.clone()])
        .await
        .unwrap()[&original_id];
    let (status, _) = srv
        .call(
            reqwest::Method::PATCH,
            &timer_path,
            Some(&a.token),
            Some(json!({ "ephemeral_secs": null })),
        )
        .await;
    assert_eq!(status, StatusCode::OK);
    let (status, edited) = srv
        .send(
            &a,
            &b,
            json!({ "body": "revised secret", "edit_of": original["msg_ref"] }),
        )
        .await;
    assert_eq!(status, StatusCode::CREATED, "{edited}");
    let ids = vec![
        edited["message_id"].as_str().unwrap().to_string(),
        edited["sent_copy_id"].as_str().unwrap().to_string(),
    ];
    let deadlines = state.store.message_expiries(ids.clone()).await.unwrap();
    assert_eq!(deadlines.len(), 2);
    for id in ids {
        assert_eq!(deadlines[&id], expires_at);
    }
    state
        .store
        .expire_direct_messages(expires_at, 100)
        .await
        .unwrap();
    let remaining = srv.listing(&b, "&actions=1").await;
    assert_eq!(remaining.len(), 1);
    assert_eq!(remaining[0]["body"], "bootstrap");
}

#[tokio::test]
async fn only_the_sender_may_edit_and_only_within_a_day() {
    let state = state();
    let srv = serve(state.clone()).await;
    let a = person(&state, "acct_a").await;
    let b = person(&state, "acct_b").await;
    let c = person(&state, "acct_c").await;
    for p in [&a, &b, &c] {
        srv.all_flags(&p.account).await;
    }
    let mine = msgactions::new_msg_ref();
    let (status, sent) = srv
        .send(&a, &b, json!({ "body": "draft", "msg_ref": mine }))
        .await;
    assert_eq!(status, StatusCode::CREATED);
    assert_eq!(sent["msg_ref"], mine);

    // The same msg_ref again is a retry, not a second message.
    let (status, again) = srv
        .send(&a, &b, json!({ "body": "draft", "msg_ref": mine }))
        .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(again["duplicate"], true);
    assert_eq!(again["sent_copy_id"], sent["sent_copy_id"]);
    assert_eq!(srv.listing(&b, "&actions=1").await.len(), 1);

    // Nobody else can reuse a reference that already lives in the recipient's mailbox.
    let (status, body) = srv
        .send(&c, &b, json!({ "body": "imposter", "msg_ref": mine }))
        .await;
    assert_eq!(
        (status, body["error"].as_str()),
        (StatusCode::CONFLICT, Some("msg_ref_in_use"))
    );

    // The recipient cannot edit it, by reference or by their own copy's id.
    for key in [
        mine.clone(),
        sent["message_id"].as_str().unwrap().to_string(),
    ] {
        let (status, body) = srv
            .send(&b, &a, json!({ "body": "tampered", "edit_of": key }))
            .await;
        assert_eq!(
            (status, body["error"].as_str()),
            (StatusCode::NOT_FOUND, Some("reference_not_found"))
        );
    }
    // A third party cannot reach into the conversation.
    let (status, _) = srv
        .send(&c, &b, json!({ "body": "x", "edit_of": mine }))
        .await;
    assert_eq!(status, StatusCode::NOT_FOUND);
    let (status, _) = srv
        .send(&c, &a, json!({ "body": "x", "reply_to": mine }))
        .await;
    assert_eq!(status, StatusCode::NOT_FOUND);

    // Edits carry text only.
    let (status, body) = srv
        .send(
            &a,
            &b,
            json!({ "body": "x", "edit_of": mine, "attachments": ["att_x"] }),
        )
        .await;
    assert_eq!(
        (status, body["error"].as_str()),
        (StatusCode::BAD_REQUEST, Some("edit_has_attachments"))
    );

    // The sender edits it, by sent-copy id; an edit of the edit is an edit of the original.
    let (status, e1) = srv
        .send(
            &a,
            &b,
            json!({ "body": "final", "edit_of": sent["sent_copy_id"] }),
        )
        .await;
    assert_eq!(status, StatusCode::CREATED, "{e1}");
    let (status, _) = srv
        .send(
            &a,
            &b,
            json!({ "body": "final final", "edit_of": e1["msg_ref"] }),
        )
        .await;
    assert_eq!(status, StatusCode::CREATED);

    // A day later the window is closed.
    state
        .store
        .backdate_message(
            sent["sent_copy_id"].as_str().unwrap().into(),
            now_unix() - 86_401,
        )
        .await
        .unwrap();
    let (status, body) = srv
        .send(&a, &b, json!({ "body": "too late", "edit_of": mine }))
        .await;
    assert_eq!(
        (status, body["error"].as_str()),
        (StatusCode::FORBIDDEN, Some("edit_window_closed"))
    );
}

#[tokio::test]
async fn the_recipient_sees_the_latest_text_with_its_history() {
    let state = state();
    let srv = serve(state.clone()).await;
    let a = person(&state, "acct_a").await;
    let b = person(&state, "acct_b").await;
    srv.all_flags(&a.account).await;
    srv.all_flags(&b.account).await;
    let (_, sent) = srv.send(&a, &b, json!({ "body": "meet at 3" })).await;
    let r = sent["msg_ref"].as_str().unwrap();
    let (_, edit) = srv
        .send(&a, &b, json!({ "body": "meet at 4", "edit_of": r }))
        .await;

    // People's clients: one row, latest text, marked edited, unread until the edit is read too.
    let folded: Vec<Value> = srv
        .listing(&b, "&actions=1&fold=1")
        .await
        .into_iter()
        .filter(|m| m["direction"] == "in")
        .collect();
    assert_eq!(folded.len(), 1);
    assert_eq!(folded[0]["body"], "meet at 4");
    assert_eq!(folded[0]["edit_count"], 1);
    assert!(folded[0]["edited_at"].as_u64().is_some());
    assert_eq!(folded[0]["read"], false);

    // Agents: the edit is its own row and says what it edits.
    let raw = srv.listing(&b, "&actions=1").await;
    let edit_row = raw.iter().find(|m| m["edit_of"] == r).unwrap();
    assert_eq!(edit_row["body"], "meet at 4");
    assert_eq!(edit_row["edit_of_message_id"], sent["message_id"]);
    assert_eq!(edit_row["message_id"], edit["message_id"]);

    // Old clients: a new message saying "Edited:", which is never acted on.
    let legacy = srv.listing(&b, "").await;
    let old = legacy
        .iter()
        .find(|m| m["message_id"] == edit["message_id"])
        .unwrap();
    assert_eq!(old["body"], "Edited: meet at 4");
    assert_eq!(old["autonomy"], "review");
    assert_eq!(old["held_because"], "edit");

    // The edit was announced to the recipient's account.
    let edited = typed_events(&state, &b.account, "message.edited").await;
    assert_eq!(edited.len(), 1);
    assert_eq!(edited[0]["edit_of"], r);
    assert_eq!(edited[0]["original_message_id"], sent["message_id"]);

    // Message info lists both versions, oldest first, on both sides.
    for (who, id) in [
        (&b, sent["message_id"].clone()),
        (&a, sent["sent_copy_id"].clone()),
    ] {
        let (status, info) = srv
            .get(
                &format!("/v1/messages/{}/info", id.as_str().unwrap()),
                &who.token,
            )
            .await;
        assert_eq!(status, StatusCode::OK, "{info}");
        let bodies: Vec<&str> = info["versions"]
            .as_array()
            .unwrap()
            .iter()
            .map(|v| v["body"].as_str().unwrap())
            .collect();
        assert_eq!(bodies, ["meet at 3", "meet at 4"]);
    }

    // Reading the original with its edits clears the unread mark.
    let (status, _) = srv
        .post(
            "/v1/ack",
            &b.token,
            json!({ "message_id": sent["message_id"], "with_edits": true }),
        )
        .await;
    assert_eq!(status, StatusCode::OK);
    let folded = srv.listing(&b, "&actions=1&fold=1").await;
    assert_eq!(folded[0]["read"], true);

    // Deleting the original deletes every version.
    let (status, _) = srv
        .post(
            "/v1/messages/delete",
            &b.token,
            json!({ "message_id": sent["message_id"] }),
        )
        .await;
    assert_eq!(status, StatusCode::OK);
    assert!(srv
        .listing(&b, "&actions=1")
        .await
        .iter()
        .all(|m| m["direction"] != "in"));
}

#[tokio::test]
async fn an_edit_is_evaluated_again_and_never_raises_a_held_request() {
    let state = state();
    let srv = serve(state.clone()).await;
    let a = person(&state, "acct_a").await;
    let b = person(&state, "acct_b").await;
    srv.all_flags(&a.account).await;
    srv.all_flags(&b.account).await;
    let (status, _) = srv
        .put(
            "/v1/contacts",
            &b.token,
            json!({ "peer": a.me.address, "admission": "allow", "autonomy": "auto",
                    "allowed_verbs": ["make_change"] }),
        )
        .await;
    assert_eq!(status, StatusCode::OK);

    // Granted request, edited into one that was not granted: the edit is held.
    let (_, granted) = srv
        .send(
            &a,
            &b,
            json!({ "body": envelope("make_change", "fix the typo") }),
        )
        .await;
    let (_, widened) = srv
        .send(
            &a,
            &b,
            json!({ "body": envelope("deploy", "fix the typo and deploy"),
                    "edit_of": granted["msg_ref"] }),
        )
        .await;
    // Prose (held), edited into a granted request: still held.
    let (_, prose) = srv.send(&a, &b, json!({ "body": "please fix it" })).await;
    let (_, promoted) = srv
        .send(
            &a,
            &b,
            json!({ "body": envelope("make_change", "fix it"), "edit_of": prose["msg_ref"] }),
        )
        .await;
    // Granted request, edited with new args: evaluated as the new request, still auto.
    let (_, ok) = srv
        .send(&a, &b, json!({ "body": envelope("make_change", "one") }))
        .await;
    let (_, ok_edit) = srv
        .send(
            &a,
            &b,
            json!({ "body": envelope("make_change", "two"), "edit_of": ok["msg_ref"] }),
        )
        .await;

    let raw = srv.listing(&b, "&actions=1").await;
    let row = |id: &Value| raw.iter().find(|m| m["message_id"] == *id).unwrap().clone();
    assert_eq!(row(&granted["message_id"])["autonomy"], "auto");
    let w = row(&widened["message_id"]);
    assert_eq!(
        (w["autonomy"].as_str(), w["verb"].as_str()),
        (Some("review"), Some("deploy"))
    );
    let p = row(&promoted["message_id"]);
    assert_eq!(
        p["autonomy"], "review",
        "an edit cannot raise a held request"
    );
    assert_eq!(p["held_because"], "not_a_request");
    let o = row(&ok_edit["message_id"]);
    assert_eq!(o["autonomy"], "auto");
    assert_eq!(o["edit_of"], ok["msg_ref"]);

    // The folded view decides on the latest text under the same rule.
    let folded = srv.listing(&b, "&actions=1&fold=1").await;
    let f = |id: &Value| {
        folded
            .iter()
            .find(|m| m["message_id"] == *id)
            .unwrap()
            .clone()
    };
    assert_eq!(f(&granted["message_id"])["autonomy"], "review");
    assert_eq!(f(&prose["message_id"])["autonomy"], "review");
    assert_eq!(f(&ok["message_id"])["autonomy"], "auto");
}

#[tokio::test]
async fn a_reply_resolves_on_both_sides_of_the_conversation() {
    let state = state();
    let srv = serve(state.clone()).await;
    let a = person(&state, "acct_a").await;
    let b = person(&state, "acct_b").await;
    srv.all_flags(&a.account).await;
    srv.all_flags(&b.account).await;
    let (_, question) = srv
        .send(
            &a,
            &b,
            json!({ "body": envelope("answer_question", "Is the A2A endpoint open?") }),
        )
        .await;
    // B answers by its own copy's id; the server maps it to the shared reference.
    let (status, answer) = srv
        .send(
            &b,
            &a,
            json!({ "body": "Private beta.", "reply_to": question["message_id"],
                    "quote": "Is the A2A endpoint open?" }),
        )
        .await;
    assert_eq!(status, StatusCode::CREATED, "{answer}");

    let at_a = srv.listing(&a, "&actions=1").await;
    let reply = at_a
        .iter()
        .find(|m| m["message_id"] == answer["message_id"])
        .unwrap();
    assert_eq!(reply["reply_to"]["msg_ref"], question["msg_ref"]);
    assert_eq!(reply["reply_to"]["message_id"], question["sent_copy_id"]);
    assert_eq!(reply["reply_to"]["direction"], "out");
    assert_eq!(reply["reply_to"]["excerpt"], "Is the A2A endpoint open?");
    assert_eq!(reply["reply_to"]["verb"], "answer_question");
    assert_eq!(reply["reply_to"]["quote"], "Is the A2A endpoint open?");

    let at_b = srv.listing(&b, "&actions=1").await;
    let mine = at_b
        .iter()
        .find(|m| m["message_id"] == answer["sent_copy_id"])
        .unwrap();
    assert_eq!(mine["reply_to"]["message_id"], question["message_id"]);
    assert_eq!(mine["reply_to"]["direction"], "in");

    // An old client gets the quote as a first line of the prose.
    let legacy = srv.listing(&a, "").await;
    let old = legacy
        .iter()
        .find(|m| m["message_id"] == answer["message_id"])
        .unwrap();
    assert_eq!(old["body"], "> Is the A2A endpoint open?\n\nPrivate beta.");

    // With the parent gone from the reader's mailbox, the quote still shows.
    srv.post(
        "/v1/messages/delete",
        &a.token,
        json!({ "message_id": question["sent_copy_id"] }),
    )
    .await;
    let at_a = srv.listing(&a, "&actions=1").await;
    let reply = at_a
        .iter()
        .find(|m| m["message_id"] == answer["message_id"])
        .unwrap();
    assert_eq!(reply["reply_to"]["message_id"], Value::Null);
    assert_eq!(reply["reply_to"]["quote"], "Is the A2A endpoint open?");
}

#[tokio::test]
async fn ticks_move_from_sent_to_delivered_to_seen_and_the_agent_gear() {
    let state = state();
    let srv = serve(state.clone()).await;
    let a = person(&state, "acct_a").await;
    let b = person(&state, "acct_b").await;
    srv.all_flags(&a.account).await;
    srv.all_flags(&b.account).await;
    let (_, sent) = srv.send(&a, &b, json!({ "body": "status?" })).await;
    let r = sent["msg_ref"].as_str().unwrap();
    assert_eq!(srv.sent_copy(&a, r).await["receipt"]["state"], "sent");

    // B's client fetches its inbox: delivered.
    srv.listing(&b, "&actions=1&fold=1").await;
    let copy = srv.sent_copy(&a, r).await;
    assert_eq!(copy["receipt"]["state"], "delivered");
    assert!(copy["receipt"]["delivered_at"].as_u64().is_some());

    // B opens it: seen.
    srv.post(
        "/v1/ack",
        &b.token,
        json!({ "message_id": sent["message_id"] }),
    )
    .await;
    assert_eq!(srv.sent_copy(&a, r).await["receipt"]["state"], "seen");

    // B's agent works on it, then finishes.
    let (status, body) = srv
        .post(
            "/v1/receipts",
            &b.token,
            json!({ "msg_refs": [r], "kind": "agent_status", "status": "working" }),
        )
        .await;
    assert_eq!(
        (status, body["updated"].as_u64()),
        (StatusCode::OK, Some(1))
    );
    assert_eq!(
        srv.sent_copy(&a, r).await["agent_status"]["state"],
        "working"
    );
    srv.post(
        "/v1/receipts",
        &b.token,
        json!({ "message_ids": [sent["message_id"]], "kind": "agent_status", "status": "done" }),
    )
    .await;
    assert_eq!(srv.sent_copy(&a, r).await["agent_status"]["state"], "done");

    // States never move back.
    srv.post(
        "/v1/receipts",
        &b.token,
        json!({ "message_ids": [sent["message_id"]], "kind": "delivered" }),
    )
    .await;
    assert_eq!(srv.sent_copy(&a, r).await["receipt"]["state"], "seen");

    let states: Vec<String> = typed_events(&state, &a.account, "message.receipt")
        .await
        .iter()
        .map(|e| e["state"].as_str().unwrap().to_string())
        .collect();
    assert_eq!(states, ["delivered", "seen", "working", "done"]);

    // The sender cannot post receipts for its own message, nor a stranger for it.
    let (_, body) = srv
        .post(
            "/v1/receipts",
            &a.token,
            json!({ "message_ids": [sent["sent_copy_id"]], "kind": "seen" }),
        )
        .await;
    assert_eq!(body["updated"], 0);
    let (status, _) = srv
        .post(
            "/v1/receipts",
            &b.token,
            json!({ "message_ids": [], "kind": "read" }),
        )
        .await;
    assert_eq!(status, StatusCode::BAD_REQUEST);
}

#[tokio::test]
async fn read_receipts_off_neither_sends_nor_shows_seen() {
    let state = state();
    let srv = serve(state.clone()).await;
    let a = person(&state, "acct_a").await;
    let b = person(&state, "acct_b").await;
    srv.all_flags(&a.account).await;
    srv.all_flags(&b.account).await;

    // B turns read receipts off: A's message to B stops at delivered.
    let (status, policy) = srv
        .put("/v1/policy", &b.token, json!({ "read_receipts": false }))
        .await;
    assert_eq!(
        (status, policy["read_receipts"].as_bool()),
        (StatusCode::OK, Some(false))
    );
    let (_, p) = srv.get("/v1/policy", &b.token).await;
    assert_eq!(p["read_receipts"], false);
    let (_, to_b) = srv.send(&a, &b, json!({ "body": "one" })).await;
    srv.listing(&b, "").await;
    srv.post(
        "/v1/ack",
        &b.token,
        json!({ "message_id": to_b["message_id"] }),
    )
    .await;
    let copy = srv.sent_copy(&a, to_b["msg_ref"].as_str().unwrap()).await;
    assert_eq!(copy["receipt"]["state"], "delivered");

    // And B does not see A reading B's messages either.
    let (_, to_a) = srv.send(&b, &a, json!({ "body": "two" })).await;
    srv.listing(&a, "").await;
    srv.post(
        "/v1/ack",
        &a.token,
        json!({ "message_id": to_a["message_id"] }),
    )
    .await;
    let copy = srv.sent_copy(&b, to_a["msg_ref"].as_str().unwrap()).await;
    assert_eq!(copy["receipt"]["state"], "delivered");
    assert_eq!(copy["receipt"]["seen_at"], Value::Null);
    let seen_events = typed_events(&state, &b.account, "message.receipt")
        .await
        .into_iter()
        .filter(|e| e["state"] == "seen")
        .count();
    assert_eq!(seen_events, 0);
}

#[tokio::test]
async fn blocked_and_filtered_senders_learn_no_more_than_they_should() {
    let state = state();
    let srv = serve(state.clone()).await;
    let a = person(&state, "acct_a").await;
    let b = person(&state, "acct_b").await;
    let s = person(&state, "acct_s").await;
    for p in [&a, &b, &s] {
        srv.all_flags(&p.account).await;
        srv.flag("message_filtering", &p.account).await;
    }

    // Filtered: B files unknown senders away. S's message is delivered, never seen, no gear.
    let (status, _) = srv
        .put(
            "/v1/policy",
            &b.token,
            json!({ "unknown_messages": "filter" }),
        )
        .await;
    assert_eq!(status, StatusCode::OK);
    let (_, filtered) = srv.send(&s, &b, json!({ "body": "hello?" })).await;
    let r = filtered["msg_ref"].as_str().unwrap();
    // Any listing delivers, the normal inbox included, so delivery says nothing about Filtered.
    srv.listing(&b, "").await;
    assert_eq!(srv.sent_copy(&s, r).await["receipt"]["state"], "delivered");
    let (_, list) = srv.get("/v1/inbox?filtered=1", &b.token).await;
    assert_eq!(list["messages"].as_array().unwrap().len(), 1);
    srv.post(
        "/v1/ack",
        &b.token,
        json!({ "message_id": filtered["message_id"] }),
    )
    .await;
    srv.post(
        "/v1/receipts",
        &b.token,
        json!({ "message_ids": [filtered["message_id"]], "kind": "agent_status", "status": "working" }),
    )
    .await;
    let copy = srv.sent_copy(&s, r).await;
    assert_eq!(copy["receipt"]["state"], "delivered");
    assert_eq!(copy["agent_status"], Value::Null);

    // An edit of a filtered message is filed in Filtered too.
    let (_, edit) = srv
        .send(&s, &b, json!({ "body": "hello!", "edit_of": r }))
        .await;
    let (_, list) = srv.get("/v1/inbox?filtered=1&actions=1", &b.token).await;
    assert!(list["messages"]
        .as_array()
        .unwrap()
        .iter()
        .any(|m| m["message_id"] == edit["message_id"]));

    // Blocked after delivery: the sender stays at sent.
    let (_, to_b) = srv.send(&a, &b, json!({ "body": "hi" })).await;
    srv.put(
        "/v1/contacts",
        &b.token,
        json!({ "peer": a.me.address, "admission": "block" }),
    )
    .await;
    srv.listing(&b, "").await;
    srv.post(
        "/v1/ack",
        &b.token,
        json!({ "message_id": to_b["message_id"] }),
    )
    .await;
    let copy = srv.sent_copy(&a, to_b["msg_ref"].as_str().unwrap()).await;
    assert_eq!(copy["receipt"]["state"], "sent");
    assert!(typed_events(&state, &a.account, "message.receipt")
        .await
        .is_empty());
}
