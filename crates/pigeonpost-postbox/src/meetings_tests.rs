//! Meeting summaries (Phase 7, SDS §14) over real sockets: who may open notes, consent records,
//! the audience meeting-intel may encrypt to, ciphertext-only storage, attendee-only reads,
//! removal, retention and service-token scoping.

use crate::*;
use p256::elliptic_curve::sec1::ToEncodedPoint;
use reqwest::Client;
use serde_json::{json, Value};

const OPS: &str = "svc_ops_0123456789abcdef0123456789abcdef";
const CALLS: &str = "svc_calls_0123456789abcdef0123456789abcdef";
const INTEL: &str = "svc_intel_0123456789abcdef0123456789abcdef";

fn state() -> AppState {
    let mut state = crate::tests::test_state();
    state.service_tokens = Arc::new(internal_api::tokens_for_test(&[
        ("POSTBOX_SVC_TOKEN_OPS", OPS),
        ("POSTBOX_SVC_TOKEN_CALLS", CALLS),
        ("POSTBOX_SVC_TOKEN_MEETING_INTEL", INTEL),
    ]));
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
    async fn flag(&self, account: &str, on: bool) {
        let (status, _) = self
            .put(
                "/internal/v1/features/meeting_summaries",
                Some(OPS),
                json!({ "scope": "account", "account_id": account, "enabled": on }),
            )
            .await;
        assert_eq!(status, StatusCode::OK);
    }
}

struct Person {
    token: String,
    cap: String,
    account: String,
    address: String,
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
    Person {
        token,
        cap: v["capability_token"].as_str().unwrap().to_string(),
        account: account.into(),
        address: v["address"].as_str().unwrap().to_string(),
    }
}

fn b64(bytes: &[u8]) -> String {
    crate::b64_encode(bytes)
}

/// A fresh P-256 public key, base64 of the uncompressed point.
fn p256_pub() -> String {
    let secret = p256::SecretKey::random(&mut rand_core::OsRng);
    b64(secret.public_key().to_encoded_point(false).as_bytes())
}

async fn open(s: &Server, host: &str) -> (StatusCode, Value) {
    s.post(
        "/internal/v1/meetings",
        Some(CALLS),
        json!({ "host": host, "room_ref": "abcdef0123456789", "kind": "room" }),
    )
    .await
}

async fn consent(
    s: &Server,
    mid: &str,
    address: &str,
    answer: &str,
    key: Option<(&str, &str)>,
) -> (StatusCode, Value) {
    let mut body = json!({ "address": address, "consent": answer });
    if let Some((id, pk)) = key {
        body["notes_key"] = json!({ "key_id": id, "public_key": pk, "label": "Test browser" });
    }
    s.post(
        &format!("/internal/v1/meetings/{mid}/attendees"),
        Some(CALLS),
        body,
    )
    .await
}

fn wrap(account: &str, key_id: &str) -> Value {
    json!({
        "account_id": account, "key_id": key_id, "epk": p256_pub(),
        "iv": b64(&[1u8; 12]), "wrapped": b64(&[2u8; 48]),
    })
}

async fn events_of(state: &AppState, account: &str, kind: &str) -> i64 {
    let (account, kind) = (account.to_string(), kind.to_string());
    state
        .store
        .group_read(move |c| -> Result<i64, crate::groups::GErr> {
            Ok(c.query_row(
                "SELECT COUNT(*) FROM account_events WHERE account_id = ?1 AND type = ?2",
                rusqlite::params![account, kind],
                |r| r.get(0),
            )?)
        })
        .await
        .unwrap()
}

#[tokio::test]
async fn only_flagged_hosts_open_notes_and_the_brief_reaches_only_consenting_attendees() {
    let state = state();
    let s = serve(state.clone()).await;
    let alice = person(&state, "acct_meet_alice").await;
    let bob = person(&state, "acct_meet_bob").await;
    let carol = person(&state, "acct_meet_carol").await;
    let dave = person(&state, "acct_meet_dave").await;

    // Flag off: no meeting, and nothing created.
    let (st, v) = open(&s, &alice.address).await;
    assert_eq!(st, StatusCode::OK);
    assert_eq!(v["allowed"], false);
    assert_eq!(v["reason"], "feature_disabled");
    let (_, v) = s
        .post(
            "/internal/v1/meetings",
            Some(CALLS),
            json!({ "host": "nobody@example.invalid", "check_only": true }),
        )
        .await;
    assert_eq!(v["reason"], "unknown_host");

    s.flag(&alice.account, true).await;
    let (_, v) = s
        .post(
            "/internal/v1/meetings",
            Some(CALLS),
            json!({ "host": alice.address, "check_only": true }),
        )
        .await;
    assert_eq!(v["allowed"], true);
    assert_eq!(v["retention_days"], 90);
    let (st, v) = open(&s, &alice.address).await;
    assert_eq!(st, StatusCode::OK, "{v}");
    let mid = v["meeting_id"].as_str().unwrap().to_string();
    assert!(mid.starts_with("mtg_") && mid.len() == 30);

    // Consent: alice and carol agree with their device keys; bob declines; dave never answers.
    let (alice_key, carol_key) = (p256_pub(), p256_pub());
    assert_eq!(
        consent(
            &s,
            &mid,
            &alice.address,
            "yes",
            Some(("alicebrowser1", &alice_key))
        )
        .await
        .0,
        StatusCode::OK
    );
    assert_eq!(
        consent(&s, &mid, &bob.address, "no", None).await.0,
        StatusCode::OK
    );
    assert_eq!(
        consent(
            &s,
            &mid,
            &carol.address,
            "yes",
            Some(("carolphone01", &carol_key))
        )
        .await
        .0,
        StatusCode::OK
    );
    // A key is never accepted with a decline, and a malformed key is refused.
    assert_eq!(
        consent(
            &s,
            &mid,
            &bob.address,
            "no",
            Some(("bobbrowser01", &p256_pub()))
        )
        .await
        .0,
        StatusCode::BAD_REQUEST
    );
    assert_eq!(
        consent(
            &s,
            &mid,
            &bob.address,
            "yes",
            Some(("bobbrowser01", &b64(&[4u8; 65])))
        )
        .await
        .0,
        StatusCode::BAD_REQUEST
    );

    // Recipients: exactly the consenting attendees and their keys. Calls may not read them.
    assert_eq!(
        s.get(
            &format!("/internal/v1/meetings/{mid}/recipients"),
            Some(CALLS)
        )
        .await
        .0,
        StatusCode::FORBIDDEN
    );
    let (st, v) = s
        .get(
            &format!("/internal/v1/meetings/{mid}/recipients"),
            Some(INTEL),
        )
        .await;
    assert_eq!(st, StatusCode::OK, "{v}");
    let accounts: Vec<&str> = v["recipients"]
        .as_array()
        .unwrap()
        .iter()
        .map(|r| r["account_id"].as_str().unwrap())
        .collect();
    assert_eq!(
        accounts,
        vec![alice.account.as_str(), carol.account.as_str()]
    );
    assert_eq!(
        v["recipients"][0]["keys"][0]["public_key"],
        json!(alice_key)
    );

    // meeting-intel may not widen the audience: a wrap for bob (declined) or a made-up key fails.
    let ct = b64(b"opaque ciphertext, never readable by the postbox");
    let brief = |wraps: Vec<Value>| json!({ "status": "ready", "alg": "A256GCM", "iv": b64(&[9u8; 12]), "ciphertext": ct, "wraps": wraps });
    let (st, v) = s
        .post(
            &format!("/internal/v1/meetings/{mid}/brief"),
            Some(INTEL),
            brief(vec![
                wrap(&alice.account, "alicebrowser1"),
                wrap(&bob.account, "bobbrowser01"),
            ]),
        )
        .await;
    assert_eq!(st, StatusCode::FORBIDDEN, "{v}");
    assert_eq!(v["error"], "not_a_recipient");
    let (st, _) = s
        .post(
            &format!("/internal/v1/meetings/{mid}/brief"),
            Some(INTEL),
            brief(vec![wrap(&alice.account, "notregistered")]),
        )
        .await;
    assert_eq!(st, StatusCode::FORBIDDEN);
    // The calls token cannot store a brief.
    let (st, _) = s
        .post(
            &format!("/internal/v1/meetings/{mid}/brief"),
            Some(CALLS),
            brief(vec![wrap(&alice.account, "alicebrowser1")]),
        )
        .await;
    assert_eq!(st, StatusCode::FORBIDDEN);

    assert_eq!(
        s.post(
            &format!("/internal/v1/meetings/{mid}/end"),
            Some(CALLS),
            json!({ "reason": "host_stopped" })
        )
        .await
        .0,
        StatusCode::OK
    );
    // Late consent after the end is refused.
    let (st, v) = consent(
        &s,
        &mid,
        &dave.address,
        "yes",
        Some(("davebrowser1", &p256_pub())),
    )
    .await;
    assert_eq!(st, StatusCode::CONFLICT, "{v}");

    let (st, v) = s
        .post(
            &format!("/internal/v1/meetings/{mid}/brief"),
            Some(INTEL),
            brief(vec![
                wrap(&alice.account, "alicebrowser1"),
                wrap(&carol.account, "carolphone01"),
            ]),
        )
        .await;
    assert_eq!(st, StatusCode::OK, "{v}");
    assert_eq!(v["notified"], 2);
    // A second result for the same meeting is refused.
    let (st, _) = s
        .post(
            &format!("/internal/v1/meetings/{mid}/brief"),
            Some(INTEL),
            json!({ "status": "failed", "reason": "late" }),
        )
        .await;
    assert_eq!(st, StatusCode::CONFLICT);

    // Attendees see ciphertext plus their own wraps only.
    let (st, v) = s
        .get(&format!("/v1/meetings/{mid}"), Some(&alice.token))
        .await;
    assert_eq!(st, StatusCode::OK, "{v}");
    assert_eq!(v["state"], "ready");
    assert_eq!(v["ciphertext"], json!(ct));
    let keys = v["keys"].as_array().unwrap();
    assert_eq!(keys.len(), 1);
    assert_eq!(keys[0]["key_id"], "alicebrowser1");
    let (_, v) = s.get("/v1/meetings", Some(&carol.token)).await;
    assert_eq!(v["meetings"].as_array().unwrap().len(), 1);
    assert_eq!(v["meetings"][0]["state"], "ready");

    // The decliner and a stranger cannot tell the meeting exists.
    for who in [&bob, &dave] {
        let (st, v) = s
            .get(&format!("/v1/meetings/{mid}"), Some(&who.token))
            .await;
        assert_eq!(st, StatusCode::NOT_FOUND, "{v}");
        let (_, v) = s.get("/v1/meetings", Some(&who.token)).await;
        assert!(v["meetings"].as_array().unwrap().is_empty());
    }
    // A mailbox token is not an account credential here.
    let (st, _) = s.get("/v1/meetings", Some(&alice.cap)).await;
    assert_eq!(st, StatusCode::FORBIDDEN);

    // Events reach exactly the consenting attendees.
    assert_eq!(events_of(&state, &alice.account, "meeting.brief").await, 1);
    assert_eq!(events_of(&state, &carol.account, "meeting.brief").await, 1);
    assert_eq!(events_of(&state, &bob.account, "meeting.brief").await, 0);

    // Removal: alice's copy and wraps go; the brief stays for carol until she removes hers.
    let (st, v) = s
        .delete(&format!("/v1/meetings/{mid}"), Some(&alice.token))
        .await;
    assert_eq!(st, StatusCode::OK);
    assert_eq!(v["brief_deleted"], false);
    assert_eq!(
        s.get(&format!("/v1/meetings/{mid}"), Some(&alice.token))
            .await
            .0,
        StatusCode::NOT_FOUND
    );
    assert_eq!(
        s.get(&format!("/v1/meetings/{mid}"), Some(&carol.token))
            .await
            .0,
        StatusCode::OK
    );
    let (_, v) = s
        .delete(&format!("/v1/meetings/{mid}"), Some(&carol.token))
        .await;
    assert_eq!(v["brief_deleted"], true);
    let gone: i64 = state
        .store
        .group_read(|c| -> Result<i64, crate::groups::GErr> {
            Ok(c.query_row(
                "SELECT (SELECT COUNT(*) FROM meetings) + (SELECT COUNT(*) FROM meeting_attendees)
                      + (SELECT COUNT(*) FROM meeting_brief_keys)",
                [],
                |r| r.get(0),
            )?)
        })
        .await
        .unwrap();
    assert_eq!(gone, 0);

    // Audit rows are metadata only.
    let details: Vec<String> = state
        .store
        .group_read(|c| -> Result<Vec<String>, crate::groups::GErr> {
            let mut stmt = c.prepare(
                "SELECT action || ' ' || details FROM audit_log WHERE target LIKE 'meeting:%'",
            )?;
            let rows = stmt
                .query_map([], |r| r.get(0))?
                .collect::<Result<Vec<String>, _>>()?;
            Ok(rows)
        })
        .await
        .unwrap();
    assert!(details
        .iter()
        .any(|d| d.starts_with("meeting.notes_started")));
    assert!(details.iter().any(|d| d.starts_with("meeting.consent")));
    assert!(details
        .iter()
        .any(|d| d.starts_with("meeting.brief_stored")));
    for d in &details {
        assert!(
            !d.contains("opaque") && !d.contains(&alice_key),
            "content in audit: {d}"
        );
    }
}

#[tokio::test]
async fn service_tokens_are_scoped_and_failures_reach_attendees() {
    let state = state();
    let s = serve(state.clone()).await;
    let alice = person(&state, "acct_meet_a2").await;
    s.flag(&alice.account, true).await;
    assert_eq!(
        s.post(
            "/internal/v1/meetings",
            None,
            json!({ "host": alice.address })
        )
        .await
        .0,
        StatusCode::UNAUTHORIZED
    );
    assert_eq!(
        s.post(
            "/internal/v1/meetings",
            Some(INTEL),
            json!({ "host": alice.address })
        )
        .await
        .0,
        StatusCode::FORBIDDEN
    );
    assert_eq!(
        s.post(
            "/internal/v1/meetings",
            Some(OPS),
            json!({ "host": alice.address })
        )
        .await
        .0,
        StatusCode::FORBIDDEN
    );
    let (_, v) = open(&s, &alice.address).await;
    let mid = v["meeting_id"].as_str().unwrap().to_string();
    consent(
        &s,
        &mid,
        &alice.address,
        "yes",
        Some(("alicebrowser1", &p256_pub())),
    )
    .await;
    // A failed result carries no content.
    let (st, _) = s
        .post(
            &format!("/internal/v1/meetings/{mid}/brief"),
            Some(INTEL),
            json!({ "status": "failed", "reason": "no_speech", "ciphertext": "AAAA" }),
        )
        .await;
    assert_eq!(st, StatusCode::BAD_REQUEST);
    let (st, _) = s
        .post(
            &format!("/internal/v1/meetings/{mid}/brief"),
            Some(INTEL),
            json!({ "status": "failed", "reason": "no_speech" }),
        )
        .await;
    assert_eq!(st, StatusCode::OK);
    let (_, v) = s
        .get(&format!("/v1/meetings/{mid}"), Some(&alice.token))
        .await;
    assert_eq!(v["state"], "failed");
    assert_eq!(v["failure"], "no_speech");
    assert!(v.get("ciphertext").is_none());
    assert_eq!(events_of(&state, &alice.account, "meeting.brief").await, 1);
    // Unknown meetings are 404 for both services.
    assert_eq!(
        s.get(
            "/internal/v1/meetings/mtg_00000000000000000000000000/recipients",
            Some(INTEL)
        )
        .await
        .0,
        StatusCode::NOT_FOUND
    );
    assert_eq!(
        s.post(
            "/internal/v1/meetings/not-a-meeting/end",
            Some(CALLS),
            json!({})
        )
        .await
        .0,
        StatusCode::NOT_FOUND
    );
}

#[tokio::test]
async fn notes_keys_are_registered_by_the_account_and_bounded() {
    let state = state();
    let s = serve(state.clone()).await;
    let alice = person(&state, "acct_meet_keys").await;
    let pk = p256_pub();
    // Flag off: registration is refused like an unknown feature.
    let (st, v) = s
        .put(
            "/v1/meetings/keys/browserkey01",
            Some(&alice.token),
            json!({ "public_key": pk }),
        )
        .await;
    assert_eq!(st, StatusCode::NOT_FOUND);
    assert_eq!(v["error"], "feature_disabled");
    s.flag(&alice.account, true).await;
    assert_eq!(
        s.put(
            "/v1/meetings/keys/browserkey01",
            Some(&alice.token),
            json!({ "public_key": pk, "label": "Web" })
        )
        .await
        .0,
        StatusCode::OK
    );
    // Idempotent for the same key; a different key under the same id is refused.
    assert_eq!(
        s.put(
            "/v1/meetings/keys/browserkey01",
            Some(&alice.token),
            json!({ "public_key": pk })
        )
        .await
        .0,
        StatusCode::OK
    );
    assert_eq!(
        s.put(
            "/v1/meetings/keys/browserkey01",
            Some(&alice.token),
            json!({ "public_key": p256_pub() })
        )
        .await
        .0,
        StatusCode::CONFLICT
    );
    assert_eq!(
        s.put(
            "/v1/meetings/keys/x",
            Some(&alice.token),
            json!({ "public_key": pk })
        )
        .await
        .0,
        StatusCode::BAD_REQUEST
    );
    assert_eq!(
        s.put(
            "/v1/meetings/keys/browserkey02",
            Some(&alice.token),
            json!({ "public_key": b64(&[0u8; 65]) })
        )
        .await
        .0,
        StatusCode::BAD_REQUEST
    );
    for i in 0..12 {
        let (st, _) = s
            .put(
                &format!("/v1/meetings/keys/device{i:04}"),
                Some(&alice.token),
                json!({ "public_key": p256_pub() }),
            )
            .await;
        assert_eq!(st, StatusCode::OK);
    }
    let (_, v) = s.get("/v1/meetings/keys", Some(&alice.token)).await;
    assert_eq!(
        v["keys"].as_array().unwrap().len(),
        crate::meetings::MAX_KEYS
    );
    let (st, _) = s
        .delete("/v1/meetings/keys/device0011", Some(&alice.token))
        .await;
    assert_eq!(st, StatusCode::OK);
    assert_eq!(
        s.delete("/v1/meetings/keys/device0011", Some(&alice.token))
            .await
            .0,
        StatusCode::NOT_FOUND
    );
}

#[tokio::test]
async fn retention_purges_expired_meetings_and_closes_stale_ones() {
    let state = state();
    let s = serve(state.clone()).await;
    let alice = person(&state, "acct_meet_ret").await;
    s.flag(&alice.account, true).await;
    let (_, v) = open(&s, &alice.address).await;
    let stale = v["meeting_id"].as_str().unwrap().to_string();
    let (_, v) = open(&s, &alice.address).await;
    let old = v["meeting_id"].as_str().unwrap().to_string();
    let now = now_unix();
    let (stale2, old2) = (stale.clone(), old.clone());
    state
        .store
        .group_tx(move |tx| -> Result<(), crate::groups::GErr> {
            tx.execute(
                "UPDATE meetings SET created_at = ?2 WHERE id = ?1",
                rusqlite::params![stale2, now - crate::meetings::PENDING_SECS - 10],
            )?;
            tx.execute(
                "UPDATE meetings SET expires_at = ?2 WHERE id = ?1",
                rusqlite::params![old2, now - 1],
            )?;
            Ok(())
        })
        .await
        .unwrap();
    let (purged, timed_out) = crate::meetings::purge(&state.store, now).await.unwrap();
    assert_eq!((purged, timed_out), (1, 1));
    let states: Vec<String> = state
        .store
        .group_read(|c| -> Result<Vec<String>, crate::groups::GErr> {
            let mut stmt = c.prepare("SELECT id || ':' || state FROM meetings ORDER BY id")?;
            let rows = stmt
                .query_map([], |r| r.get(0))?
                .collect::<Result<Vec<String>, _>>()?;
            Ok(rows)
        })
        .await
        .unwrap();
    assert_eq!(states, vec![format!("{stale}:failed")]);
}
