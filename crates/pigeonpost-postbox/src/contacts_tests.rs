//! Phase 10 (SDS §14D) over real sockets: who may verify, discover and invite; discovery returns
//! only people who opted in; enumeration limits; SMS budgets refuse before anything is sent;
//! codes expire and lock; recycled numbers move; display names and their visibility; retention.
//!
//! Each test gets its own state (so its own captured SMS log) and its own client IPs (the
//! in-process rate limiter is shared by every test in the binary).

use crate::*;
use reqwest::Client;
use serde_json::{json, Value};
use std::sync::atomic::{AtomicU32, Ordering};

const ISSUER: &str = "https://auth.example/realms/x";
const OPS: &str = "svc_ops_0123456789abcdef0123456789abcdef";

static NEXT_IP: AtomicU32 = AtomicU32::new(1);

fn fresh_ip() -> String {
    let n = NEXT_IP.fetch_add(1, Ordering::SeqCst);
    format!("10.77.{}.{}", (n >> 8) & 0xff, n & 0xff)
}

struct Srv {
    public: String,
    internal: String,
    http: Client,
    state: AppState,
    ip: String,
}

async fn serve_with(runtime: contacts::Runtime) -> Srv {
    let mut state = crate::tests::test_state();
    state.service_tokens = Arc::new(internal_api::tokens_for_test(&[(
        "POSTBOX_SVC_TOKEN_OPS",
        OPS,
    )]));
    state.trusted_proxy_hops = 1;
    state.contacts = Arc::new(runtime);
    let public = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let internal = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let public_url = format!("http://{}", public.local_addr().unwrap());
    let internal_url = format!("http://{}", internal.local_addr().unwrap());
    let app =
        build_router(state.clone()).into_make_service_with_connect_info::<std::net::SocketAddr>();
    let internal_app = internal_api::router(state.clone())
        .into_make_service_with_connect_info::<std::net::SocketAddr>();
    tokio::spawn(async move { axum::serve(public, app).await.unwrap() });
    tokio::spawn(async move { axum::serve(internal, internal_app).await.unwrap() });
    Srv {
        public: public_url,
        internal: internal_url,
        http: Client::new(),
        state,
        ip: fresh_ip(),
    }
}

async fn serve() -> Srv {
    serve_with(contacts::Runtime::for_test()).await
}

impl Srv {
    async fn call_from(
        &self,
        ip: &str,
        method: reqwest::Method,
        path: &str,
        token: Option<&str>,
        body: Option<Value>,
    ) -> (u16, Value) {
        let base = if path.starts_with("/internal/") {
            &self.internal
        } else {
            &self.public
        };
        let mut req = self
            .http
            .request(method, format!("{base}{path}"))
            .header("x-forwarded-for", ip);
        if let Some(t) = token {
            req = req.bearer_auth(t);
        }
        if let Some(b) = body {
            req = req.json(&b);
        }
        let resp = req.send().await.unwrap();
        let status = resp.status().as_u16();
        let text = resp.text().await.unwrap();
        (status, serde_json::from_str(&text).unwrap_or(Value::Null))
    }
    async fn call(
        &self,
        method: reqwest::Method,
        path: &str,
        token: Option<&str>,
        body: Option<Value>,
    ) -> (u16, Value) {
        self.call_from(&self.ip.clone(), method, path, token, body)
            .await
    }
    async fn get(&self, path: &str, token: &str) -> (u16, Value) {
        self.call(reqwest::Method::GET, path, Some(token), None)
            .await
    }
    async fn post(&self, path: &str, token: Option<&str>, body: Value) -> (u16, Value) {
        self.call(reqwest::Method::POST, path, token, Some(body))
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
        assert_eq!(status, 200, "flag {name}");
    }
    async fn sql(&self, sql: &'static str, args: Vec<rusqlite::types::Value>) {
        self.state
            .store
            .group_tx(move |tx| -> Result<(), crate::groups::GErr> {
                tx.execute(sql, rusqlite::params_from_iter(args))?;
                Ok(())
            })
            .await
            .unwrap();
    }
    async fn count(&self, sql: &'static str) -> i64 {
        self.state
            .store
            .group_read(move |c| -> Result<i64, crate::groups::GErr> {
                Ok(c.query_row(sql, [], |r| r.get(0))?)
            })
            .await
            .unwrap()
    }
}

/// A person with a member session, an API key and one mailbox, with every Phase 10 flag on.
struct Person {
    token: String,
    api_key: String,
    cap: String,
    account: String,
    address: String,
}

async fn person(srv: &Srv, name: &str) -> Person {
    let account = format!("acct_c_{name}");
    let sub = format!("sub-c-{name}");
    srv.state
        .store
        .account_for_sub(sub.clone(), account.clone(), now_unix())
        .await
        .unwrap();
    let (api_key, key) = new_api_key();
    srv.state
        .store
        .add_api_key(account.clone(), key, now_unix())
        .await
        .unwrap();
    let v = do_create_identity(&srv.state, Some(account.clone()), None, None)
        .await
        .unwrap();
    for f in [
        "phone_numbers",
        "contact_discovery",
        "sms_invites",
        "display_names",
        "account_events",
    ] {
        srv.flag(f, &account).await;
    }
    Person {
        token: oidc::test_member_token(ISSUER, &sub),
        api_key,
        cap: v["capability_token"].as_str().unwrap().to_string(),
        account,
        address: v["address"].as_str().unwrap().to_string(),
    }
}

/// The code in the last SMS sent to `to`.
fn code_for(srv: &Srv, to: &str) -> String {
    let sms = srv
        .state
        .contacts
        .sent()
        .into_iter()
        .rev()
        .find(|s| s.to == to)
        .expect("an SMS to that number");
    let at = sms.message.find("code is ").unwrap() + "code is ".len();
    sms.message[at..at + 6].to_string()
}

/// Verify `number` for `p` (optionally discoverable) and return the number's id.
async fn verify(srv: &Srv, p: &Person, number: &str, discoverable: bool) -> String {
    let (s, v) = srv
        .post(
            "/v1/phone/verify",
            Some(&p.token),
            json!({ "phone": number }),
        )
        .await;
    assert_eq!(s, 202, "{v}");
    let challenge = v["challenge_id"].as_str().unwrap().to_string();
    let code = code_for(srv, number);
    let (s, v) = srv
        .post(
            &format!("/v1/phone/verify/{challenge}"),
            Some(&p.token),
            json!({ "code": code, "discoverable": discoverable }),
        )
        .await;
    assert_eq!(s, 200, "{v}");
    v["number"]["id"].as_str().unwrap().to_string()
}

fn h(number: &str) -> String {
    contacts::client_hash(number)
        .iter()
        .map(|b| format!("{b:02x}"))
        .collect()
}

#[tokio::test]
async fn only_a_person_may_verify_discover_or_invite() {
    let srv = serve().await;
    let a = person(&srv, "authz").await;
    let calls: [(&str, Value); 3] = [
        ("/v1/phone/verify", json!({ "phone": "+15555550101" })),
        (
            "/v1/contacts/discover",
            json!({ "hashes": [h("+15555550101")] }),
        ),
        (
            "/v1/contacts/invites",
            json!({ "phone": "+15555550101", "confirm": true }),
        ),
    ];
    for (path, body) in &calls {
        for token in [&a.api_key, &a.cap] {
            let (s, v) = srv.post(path, Some(token), body.clone()).await;
            assert_eq!(s, 403, "{path} with automation: {v}");
            assert!(
                ["human_required", "credential_not_accepted"]
                    .contains(&v["error"].as_str().unwrap_or("")),
                "{path}: {v}"
            );
        }
        let (s, _) = srv.post(path, None, body.clone()).await;
        assert_eq!(s, 401, "{path} anonymous");
    }
    let (s, _) = srv.get("/v1/phone", &a.api_key).await;
    assert_eq!(s, 403);
    // A person may; nothing was texted by any refused call.
    assert!(srv.state.contacts.sent().is_empty());
    let (s, v) = srv
        .post(
            "/v1/contacts/discover",
            Some(&a.token),
            json!({ "hashes": [h("+15555550101")] }),
        )
        .await;
    assert_eq!(s, 200, "{v}");
    // Someone else's mailbox cannot receive my number.
    let b = person(&srv, "authz-b").await;
    let (s, v) = srv
        .post(
            "/v1/phone/verify",
            Some(&a.token),
            json!({ "phone": "+15555550102", "identity": b.address }),
        )
        .await;
    assert_eq!(s, 403, "{v}");
}

#[tokio::test]
async fn flags_gate_every_surface() {
    let srv = serve().await;
    let sub = "sub-c-noflags";
    srv.state
        .store
        .account_for_sub(sub.into(), "acct_c_noflags".into(), now_unix())
        .await
        .unwrap();
    do_create_identity(&srv.state, Some("acct_c_noflags".into()), None, None)
        .await
        .unwrap();
    let token = oidc::test_member_token(ISSUER, sub);
    for (path, body) in [
        ("/v1/phone/verify", json!({ "phone": "+15555550103" })),
        (
            "/v1/contacts/discover",
            json!({ "hashes": [h("+15555550103")] }),
        ),
        (
            "/v1/contacts/invites",
            json!({ "phone": "+15555550103", "confirm": true }),
        ),
    ] {
        let (s, v) = srv.post(path, Some(&token), body).await;
        assert_eq!(
            (s, v["error"].as_str()),
            (404, Some("feature_disabled")),
            "{path}"
        );
    }
    let (s, _) = srv.get("/v1/profile/name", &token).await;
    assert_eq!(s, 404);
}

#[tokio::test]
async fn verification_codes_expire_lock_and_are_masked() {
    let srv = serve().await;
    let a = person(&srv, "verify").await;
    let n = "+447700900123";
    let (s, v) = srv
        .post("/v1/phone/verify", Some(&a.token), json!({ "phone": n }))
        .await;
    assert_eq!(s, 202, "{v}");
    let challenge = v["challenge_id"].as_str().unwrap().to_string();
    let sms = srv.state.contacts.sent();
    assert_eq!(sms.len(), 1);
    assert!(sms[0]
        .message
        .starts_with("Pigeonpost: your verification code is "));
    assert!(sms[0].message.contains("@inbox.pigeonpost.dev #"));
    // An immediate resend is throttled, and sends nothing.
    let (s, v) = srv
        .post("/v1/phone/verify", Some(&a.token), json!({ "phone": n }))
        .await;
    assert_eq!((s, v["limit"].as_str()), (429, Some("resend")), "{v}");
    assert_eq!(srv.state.contacts.sent().len(), 1);
    // Wrong codes count down; the fifth ends the challenge, and the right code then fails too.
    let right = code_for(&srv, n);
    let wrong = if right == "000000" {
        "111111"
    } else {
        "000000"
    };
    for left in (1..=4).rev() {
        let (s, v) = srv
            .post(
                &format!("/v1/phone/verify/{challenge}"),
                Some(&a.token),
                json!({ "code": wrong }),
            )
            .await;
        assert_eq!((s, v["attempts_left"].as_i64()), (400, Some(left)), "{v}");
    }
    let (s, v) = srv
        .post(
            &format!("/v1/phone/verify/{challenge}"),
            Some(&a.token),
            json!({ "code": wrong }),
        )
        .await;
    assert_eq!((s, v["error"].as_str()), (410, Some("challenge_expired")));
    let (s, _) = srv
        .post(
            &format!("/v1/phone/verify/{challenge}"),
            Some(&a.token),
            json!({ "code": right }),
        )
        .await;
    assert_eq!(s, 410);
    // An expired challenge is dead even with the right code.
    srv.sql(
        "UPDATE sms_ledger SET created_at = created_at - 120",
        vec![],
    )
    .await;
    let (s, v) = srv
        .post("/v1/phone/verify", Some(&a.token), json!({ "phone": n }))
        .await;
    assert_eq!(s, 202, "{v}");
    let challenge = v["challenge_id"].as_str().unwrap().to_string();
    srv.sql("UPDATE phone_challenges SET expires_at = 1", vec![])
        .await;
    let (s, _) = srv
        .post(
            &format!("/v1/phone/verify/{challenge}"),
            Some(&a.token),
            json!({ "code": code_for(&srv, n) }),
        )
        .await;
    assert_eq!(s, 410);
    // Another account cannot use my challenge.
    srv.sql(
        "UPDATE sms_ledger SET created_at = created_at - 120",
        vec![],
    )
    .await;
    let (_, v) = srv
        .post("/v1/phone/verify", Some(&a.token), json!({ "phone": n }))
        .await;
    let challenge = v["challenge_id"].as_str().unwrap().to_string();
    let b = person(&srv, "verify-b").await;
    let (s, _) = srv
        .post(
            &format!("/v1/phone/verify/{challenge}"),
            Some(&b.token),
            json!({ "code": code_for(&srv, n) }),
        )
        .await;
    assert_eq!(s, 410);
    // The right code links it, masked, not discoverable unless asked.
    let (s, v) = srv
        .post(
            &format!("/v1/phone/verify/{challenge}"),
            Some(&a.token),
            json!({ "code": code_for(&srv, n) }),
        )
        .await;
    assert_eq!(s, 200, "{v}");
    assert_eq!(v["number"]["masked"], "+44 ••• ••• ••23");
    assert_eq!(v["number"]["discoverable"], false);
    assert_eq!(v["number"]["identity"], a.address.as_str());
    let (_, list) = srv.get("/v1/phone", &a.token).await;
    assert_eq!(list["numbers"].as_array().unwrap().len(), 1);
    assert!(
        !list.to_string().contains("7700900123"),
        "never the full number"
    );
    // The database holds no plaintext or unkeyed hash of it either.
    let plain = srv
        .count("SELECT COUNT(*) FROM phone_numbers WHERE instr(number_sealed, '447700900123') > 0 OR instr(hex(tag), upper('5c4fd5bb0f19345b15863f865cb76cee')) > 0")
        .await;
    assert_eq!(plain, 0);
    // Audit rows carry no number.
    let rows = srv
        .state
        .store
        .audit_for_actor(Some(a.account.clone()), 20)
        .await
        .unwrap();
    assert!(rows.iter().any(|r| r.action == "phone.verified"));
    assert!(rows.iter().all(|r| !r.details.contains("7700900123")));
}

#[tokio::test]
async fn unsupported_numbers_are_refused_without_a_text() {
    let srv = serve().await;
    let a = person(&srv, "premium").await;
    for n in ["+19005550100", "+881612345678", "+449012345678"] {
        let (s, v) = srv
            .post("/v1/phone/verify", Some(&a.token), json!({ "phone": n }))
            .await;
        assert_eq!(
            (s, v["error"].as_str()),
            (422, Some("phone_not_supported")),
            "{n}"
        );
    }
    let (s, v) = srv
        .post(
            "/v1/phone/verify",
            Some(&a.token),
            json!({ "phone": "05321234567" }),
        )
        .await;
    assert_eq!((s, v["error"].as_str()), (400, Some("invalid_phone")));
    assert!(srv.state.contacts.sent().is_empty());
}

#[tokio::test]
async fn discovery_returns_only_people_who_opted_in() {
    let srv = serve().await;
    let hidden = person(&srv, "disc-hidden").await;
    let found = person(&srv, "disc-found").await;
    let me = person(&srv, "disc-me").await;
    let blocker = person(&srv, "disc-blocker").await;
    let (nh, nf, nm, nb, nobody) = (
        "+15555550110",
        "+15555550111",
        "+15555550112",
        "+15555550113",
        "+15555550119",
    );
    let hidden_id = verify(&srv, &hidden, nh, false).await;
    verify(&srv, &found, nf, true).await;
    verify(&srv, &me, nm, true).await;
    verify(&srv, &blocker, nb, true).await;
    // The blocker blocks me.
    let (s, v) = srv
        .call(
            reqwest::Method::PUT,
            "/v1/contacts",
            Some(&blocker.api_key),
            Some(json!({ "peer": me.address, "admission": "block" })),
        )
        .await;
    assert_eq!(s, 200, "{v}");
    let ask = json!({ "hashes": [h(nh), h(nf), h(nm), h(nb), h(nobody)] });
    let (s, v) = srv
        .post("/v1/contacts/discover", Some(&me.token), ask.clone())
        .await;
    assert_eq!(s, 200, "{v}");
    let matches = v["matches"].as_array().unwrap();
    assert_eq!(matches.len(), 1, "{v}");
    assert_eq!(matches[0]["hash"], h(nf));
    assert_eq!(matches[0]["address"], found.address.as_str());
    assert_eq!(v["checked"], 5);
    assert_eq!(v["new"], 5);
    // Opting in later makes them findable; re-checking costs no new budget.
    let (s, _) = srv
        .call(
            reqwest::Method::PATCH,
            &format!("/v1/phone/{hidden_id}"),
            Some(&hidden.token),
            Some(json!({ "discoverable": true })),
        )
        .await;
    assert_eq!(s, 200);
    let (_, v) = srv
        .post("/v1/contacts/discover", Some(&me.token), ask.clone())
        .await;
    assert_eq!(v["matches"].as_array().unwrap().len(), 2, "{v}");
    assert_eq!(v["new"], 0);
    // An unlinked number disappears.
    let (s, _) = srv
        .call(
            reqwest::Method::DELETE,
            &format!("/v1/phone/{hidden_id}"),
            Some(&hidden.token),
            None,
        )
        .await;
    assert_eq!(s, 200);
    let (_, v) = srv
        .post("/v1/contacts/discover", Some(&me.token), ask)
        .await;
    assert_eq!(v["matches"].as_array().unwrap().len(), 1);
    // Nothing of the upload is stored: only 12-byte marks and a count.
    assert_eq!(
        srv.count("SELECT COUNT(*) FROM discovery_marks WHERE length(mark) <> 12")
            .await,
        0
    );
    assert_eq!(
        srv.count("SELECT new_hashes FROM discovery_usage WHERE account_id = 'acct_c_disc-me'")
            .await,
        5
    );
}

#[tokio::test]
async fn discovery_enumeration_limits() {
    let srv = serve().await;
    let a = person(&srv, "enum").await;
    let batch = |from: u32, n: u32| -> Value {
        json!({ "hashes": (from..from + n).map(|i| h(&format!("+1555600{i:04}"))).collect::<Vec<_>>() })
    };
    // Batch bounds.
    let (s, _) = srv
        .post("/v1/contacts/discover", Some(&a.token), batch(0, 501))
        .await;
    assert_eq!(s, 400);
    let (s, _) = srv
        .post(
            "/v1/contacts/discover",
            Some(&a.token),
            json!({ "hashes": [] }),
        )
        .await;
    assert_eq!(s, 400);
    let (s, _) = srv
        .post(
            "/v1/contacts/discover",
            Some(&a.token),
            json!({ "hashes": ["XYZ"] }),
        )
        .await;
    assert_eq!(s, 400);
    // A day-old account may check 200 new numbers today.
    let (s, v) = srv
        .post("/v1/contacts/discover", Some(&a.token), batch(0, 200))
        .await;
    assert_eq!((s, v["remaining_today"].as_i64()), (200, Some(0)), "{v}");
    let (s, v) = srv
        .post("/v1/contacts/discover", Some(&a.token), batch(200, 1))
        .await;
    assert_eq!((s, v["limit"].as_str()), (429, Some("daily")), "{v}");
    // The same numbers again cost nothing new.
    let (s, _) = srv
        .post("/v1/contacts/discover", Some(&a.token), batch(0, 200))
        .await;
    assert_eq!(s, 200);
    // An older account gets 1,000 a day.
    srv.sql(
        "UPDATE accounts SET created_at = created_at - 172800 WHERE id = 'acct_c_enum'",
        vec![],
    )
    .await;
    let (s, v) = srv
        .post("/v1/contacts/discover", Some(&a.token), batch(1000, 500))
        .await;
    assert_eq!(s, 200, "{v}");
    // Hitting the cap on a third day in 30 locks discovery.
    srv.sql(
        "INSERT INTO discovery_usage (account_id, day, new_hashes, capped) VALUES
           ('acct_c_enum', CAST(strftime('%s','now') AS INTEGER) / 86400 - 3, 1000, 1),
           ('acct_c_enum', CAST(strftime('%s','now') AS INTEGER) / 86400 - 9, 1000, 1)",
        vec![],
    )
    .await;
    let (s, v) = srv
        .post("/v1/contacts/discover", Some(&a.token), batch(3000, 500))
        .await;
    assert_eq!(
        (s, v["error"].as_str()),
        (403, Some("discovery_locked")),
        "{v}"
    );
    let (s, _) = srv
        .post("/v1/contacts/discover", Some(&a.token), batch(0, 1))
        .await;
    assert_eq!(s, 403, "the lockout holds for already-checked numbers too");
    let rows = srv
        .state
        .store
        .audit_for_actor(Some("acct_c_enum".into()), 50)
        .await
        .unwrap();
    assert!(rows.iter().any(|r| r.action == "contacts.discovery_locked"));
    // Per-account requests per hour: 20, then 429 (a fresh person, a fresh IP).
    let b = person(&srv, "enum-rate").await;
    for i in 0..20 {
        let (s, v) = srv
            .call_from(
                &fresh_ip(),
                reqwest::Method::POST,
                "/v1/contacts/discover",
                Some(&b.token),
                Some(batch(5000 + i, 1)),
            )
            .await;
        assert_eq!(s, 200, "request {i}: {v}");
    }
    let (s, v) = srv
        .call_from(
            &fresh_ip(),
            reqwest::Method::POST,
            "/v1/contacts/discover",
            Some(&b.token),
            Some(batch(6000, 1)),
        )
        .await;
    assert_eq!((s, v["limit"].as_str()), (429, Some("requests")));
    // Per IP: 30 an hour across accounts.
    let ip = fresh_ip();
    let mut people = Vec::new();
    for i in 0..2 {
        people.push(person(&srv, &format!("enum-ip-{i}")).await);
    }
    let mut ok = 0;
    for i in 0..31u32 {
        let who = &people[(i % 2) as usize];
        let (s, _) = srv
            .call_from(
                &ip,
                reqwest::Method::POST,
                "/v1/contacts/discover",
                Some(&who.token),
                Some(batch(7000 + i, 1)),
            )
            .await;
        if s == 200 {
            ok += 1;
        }
    }
    assert_eq!(ok, 30);
}

#[tokio::test]
async fn discovery_circuit_breaker() {
    let srv = serve_with(
        contacts::Runtime::for_test().with_budgets(contacts::Budgets {
            discover_global_per_hour: 5,
            ..contacts::Budgets::default()
        }),
    )
    .await;
    let a = person(&srv, "breaker-a").await;
    let b = person(&srv, "breaker-b").await;
    let hashes = |from: u32, n: u32| -> Value {
        json!({ "hashes": (from..from + n).map(|i| h(&format!("+1555700{i:04}"))).collect::<Vec<_>>() })
    };
    let (s, _) = srv
        .post("/v1/contacts/discover", Some(&a.token), hashes(0, 4))
        .await;
    assert_eq!(s, 200);
    let (s, v) = srv
        .post("/v1/contacts/discover", Some(&b.token), hashes(10, 2))
        .await;
    assert_eq!((s, v["error"].as_str()), (503, Some("discovery_busy")));
    // Nothing was recorded for the refused request.
    assert_eq!(
        srv.count("SELECT COUNT(*) FROM discovery_marks WHERE account_id = 'acct_c_breaker-b'")
            .await,
        0
    );
}

#[tokio::test]
async fn sms_budgets_refuse_before_sending() {
    let srv = serve_with(
        contacts::Runtime::for_test().with_budgets(contacts::Budgets {
            sms_hourly: 2,
            ..contacts::Budgets::default()
        }),
    )
    .await;
    let a = person(&srv, "budget-a").await;
    let b = person(&srv, "budget-b").await;
    let c = person(&srv, "budget-c").await;
    for (p, n) in [(&a, "+15555550120"), (&b, "+15555550121")] {
        let (s, _) = srv
            .post("/v1/phone/verify", Some(&p.token), json!({ "phone": n }))
            .await;
        assert_eq!(s, 202);
    }
    let (s, v) = srv
        .post(
            "/v1/phone/verify",
            Some(&c.token),
            json!({ "phone": "+15555550122" }),
        )
        .await;
    assert_eq!((s, v["error"].as_str()), (503, Some("sms_unavailable")));
    assert_eq!(
        srv.state.contacts.sent().len(),
        2,
        "the refused code was never sent"
    );
    assert_eq!(srv.count("SELECT COUNT(*) FROM phone_challenges").await, 2);

    // Per country per day.
    let srv = serve_with(
        contacts::Runtime::for_test().with_budgets(contacts::Budgets {
            sms_country_daily: 1,
            ..contacts::Budgets::default()
        }),
    )
    .await;
    let a = person(&srv, "country-a").await;
    let b = person(&srv, "country-b").await;
    let (s, _) = srv
        .post(
            "/v1/phone/verify",
            Some(&a.token),
            json!({ "phone": "+905321234560" }),
        )
        .await;
    assert_eq!(s, 202);
    let (s, _) = srv
        .post(
            "/v1/phone/verify",
            Some(&b.token),
            json!({ "phone": "+905321234561" }),
        )
        .await;
    assert_eq!(s, 503);
    let (s, _) = srv
        .post(
            "/v1/phone/verify",
            Some(&b.token),
            json!({ "phone": "+447700900124" }),
        )
        .await;
    assert_eq!(s, 202, "another country still has budget");

    // A provider failure releases nothing it should not: the challenge goes, the cost is counted.
    let srv = serve().await;
    let a = person(&srv, "fail").await;
    srv.state.contacts.fail_sends(true);
    let (s, _) = srv
        .post(
            "/v1/phone/verify",
            Some(&a.token),
            json!({ "phone": "+15555550123" }),
        )
        .await;
    assert_eq!(s, 503);
    assert_eq!(srv.count("SELECT COUNT(*) FROM phone_challenges").await, 0);
    assert_eq!(
        srv.count("SELECT COUNT(*) FROM sms_ledger WHERE status = 'failed'")
            .await,
        1
    );
}

#[tokio::test]
async fn per_account_code_limits() {
    let srv = serve().await;
    let a = person(&srv, "codes").await;
    // Three pending challenges at once, then 429 pending.
    for n in ["+15555550130", "+15555550131", "+15555550132"] {
        let (s, _) = srv
            .post("/v1/phone/verify", Some(&a.token), json!({ "phone": n }))
            .await;
        assert_eq!(s, 202);
    }
    let (s, v) = srv
        .post(
            "/v1/phone/verify",
            Some(&a.token),
            json!({ "phone": "+15555550133" }),
        )
        .await;
    assert_eq!((s, v["limit"].as_str()), (429, Some("pending")));
    // Ten codes a day per account.
    srv.sql("DELETE FROM phone_challenges", vec![]).await;
    srv.sql(
        "INSERT INTO sms_ledger (id, purpose, account_id, tag, country_code, status, created_at)
         SELECT 'sms_fill_' || value, 'verify', 'acct_c_codes', randomblob(16), '1', 'sent',
                CAST(strftime('%s','now') AS INTEGER) - 100
           FROM json_each('[1,2,3,4,5,6,7]')",
        vec![],
    )
    .await;
    let (s, v) = srv
        .post(
            "/v1/phone/verify",
            Some(&a.token),
            json!({ "phone": "+15555550134" }),
        )
        .await;
    assert_eq!((s, v["limit"].as_str()), (429, Some("account")), "{v}");
}

#[tokio::test]
async fn a_recycled_number_moves_to_whoever_proves_it_last() {
    let srv = serve().await;
    let old = person(&srv, "recycled-old").await;
    let new = person(&srv, "recycled-new").await;
    let n = "+15555550140";
    verify(&srv, &old, n, true).await;
    srv.sql(
        "UPDATE sms_ledger SET created_at = created_at - 120",
        vec![],
    )
    .await;
    verify(&srv, &new, n, false).await;
    let (_, v) = srv.get("/v1/phone", &old.token).await;
    assert_eq!(v["numbers"].as_array().unwrap().len(), 0);
    let (_, v) = srv.get("/v1/phone", &new.token).await;
    assert_eq!(v["numbers"].as_array().unwrap().len(), 1);
    assert_eq!(v["numbers"][0]["discoverable"], false);
    let unlinked = srv
        .count("SELECT COUNT(*) FROM account_events WHERE account_id = 'acct_c_recycled-old' AND type = 'phone.unlinked' AND data LIKE '%verified_elsewhere%'")
        .await;
    assert_eq!(unlinked, 1);
}

#[tokio::test]
async fn invitations() {
    let srv = serve().await;
    let a = person(&srv, "inv-a").await;
    let b = person(&srv, "inv-b").await;
    let target = "+15555550150";
    // A verified number first; then an explicit confirmation.
    let (s, v) = srv
        .post(
            "/v1/contacts/invites",
            Some(&a.token),
            json!({ "phone": target, "confirm": true }),
        )
        .await;
    assert_eq!((s, v["error"].as_str()), (403, Some("phone_required")));
    verify(&srv, &a, "+15555550151", false).await;
    let (s, v) = srv
        .post(
            "/v1/contacts/invites",
            Some(&a.token),
            json!({ "phone": target }),
        )
        .await;
    assert_eq!((s, v["error"].as_str()), (400, Some("confirm_required")));
    let (s, _) = srv
        .call(
            reqwest::Method::PUT,
            "/v1/profile/name",
            Some(&a.token),
            Some(json!({ "display_name": "Ada Lovelace" })),
        )
        .await;
    assert_eq!(s, 200);
    let before = srv.state.contacts.sent().len();
    let (s, v) = srv
        .post(
            "/v1/contacts/invites",
            Some(&a.token),
            json!({ "phone": target, "confirm": true }),
        )
        .await;
    assert_eq!((s, v["status"].as_str()), (200, Some("sent")), "{v}");
    let sms = srv.state.contacts.sent();
    assert_eq!(sms.len(), before + 1);
    let text = &sms.last().unwrap().message;
    assert!(
        text.starts_with("Pigeonpost: Ada Lovelace invited you"),
        "{text}"
    );
    assert!(text.contains("https://pigeonpost.dev/invite?c="));
    assert!(text.ends_with("Reply STOP to opt out."));
    assert!(!text.contains(&a.address), "never the sender's address");
    let code = text
        .split("invite?c=")
        .nth(1)
        .unwrap()
        .split_whitespace()
        .next()
        .unwrap()
        .to_string();
    // Same pair again: already invited; another sender within a week: unavailable.
    let (_, v) = srv
        .post(
            "/v1/contacts/invites",
            Some(&a.token),
            json!({ "phone": target, "confirm": true }),
        )
        .await;
    assert_eq!(v["reason"], "already_invited");
    verify(&srv, &b, "+15555550152", false).await;
    let (_, v) = srv
        .post(
            "/v1/contacts/invites",
            Some(&b.token),
            json!({ "phone": target, "confirm": true }),
        )
        .await;
    assert_eq!(v["reason"], "unavailable");
    // Opt-out by the texted code: constant answer for any code, and then never again.
    for junk in ["", "nope", "abcdefghjkmn"] {
        let (s, v) = srv
            .post("/v1/sms/opt-out", None, json!({ "code": junk }))
            .await;
        assert_eq!((s, v["ok"].as_bool()), (200, Some(true)));
    }
    let (s, v) = srv
        .post("/v1/sms/opt-out", None, json!({ "code": code }))
        .await;
    assert_eq!((s, v["ok"].as_bool()), (200, Some(true)));
    assert_eq!(srv.count("SELECT COUNT(*) FROM sms_optouts").await, 1);
    srv.sql(
        "UPDATE sms_invites SET created_at = created_at - 40 * 86400",
        vec![],
    )
    .await;
    let (_, v) = srv
        .post(
            "/v1/contacts/invites",
            Some(&a.token),
            json!({ "phone": target, "confirm": true }),
        )
        .await;
    assert_eq!(v["reason"], "unavailable", "opted out");
    // Sender caps: 5 a day.
    let mut sent = 0;
    for i in 0..6 {
        let (s, v) = srv
            .post(
                "/v1/contacts/invites",
                Some(&b.token),
                json!({ "phone": format!("+1555555016{i}"), "confirm": true }),
            )
            .await;
        if s == 200 && v["status"] == "sent" {
            sent += 1;
        } else {
            assert_eq!((s, v["limit"].as_str()), (429, Some("daily")), "{v}");
        }
    }
    assert_eq!(sent, 5);
    // A premium number is just "unavailable" — no oracle, no text.
    let c = person(&srv, "inv-c").await;
    verify(&srv, &c, "+15555550153", false).await;
    let n = srv.state.contacts.sent().len();
    let (_, v) = srv
        .post(
            "/v1/contacts/invites",
            Some(&c.token),
            json!({ "phone": "+19005550100", "confirm": true }),
        )
        .await;
    assert_eq!(v["reason"], "unavailable");
    assert_eq!(srv.state.contacts.sent().len(), n);
    // Invite sub-budget.
    let srv = serve_with(
        contacts::Runtime::for_test().with_budgets(contacts::Budgets {
            sms_invite_daily: 1,
            ..contacts::Budgets::default()
        }),
    )
    .await;
    let d = person(&srv, "inv-d").await;
    verify(&srv, &d, "+15555550170", false).await;
    let (_, v) = srv
        .post(
            "/v1/contacts/invites",
            Some(&d.token),
            json!({ "phone": "+15555550171", "confirm": true }),
        )
        .await;
    assert_eq!(v["status"], "sent");
    let (s, v) = srv
        .post(
            "/v1/contacts/invites",
            Some(&d.token),
            json!({ "phone": "+15555550172", "confirm": true }),
        )
        .await;
    assert_eq!((s, v["error"].as_str()), (503, Some("sms_unavailable")));
}

#[tokio::test]
async fn display_names_follow_their_visibility() {
    let srv = serve().await;
    let a = person(&srv, "names-a").await;
    let friend = person(&srv, "names-friend").await;
    let stranger = person(&srv, "names-stranger").await;
    // Validation.
    for bad in ["/admin", "x@y", "Pigeonpost", "Bek\u{202e}ir", ""] {
        let (s, v) = srv
            .call(
                reqwest::Method::PUT,
                "/v1/profile/name",
                Some(&a.token),
                Some(json!({ "display_name": bad })),
            )
            .await;
        assert_eq!(
            (s, v["error"].as_str()),
            (400, Some("invalid_name")),
            "{bad:?}"
        );
    }
    // The mailbox's own token may set it.
    let (s, v) = srv
        .call(
            reqwest::Method::PUT,
            "/v1/profile/name",
            Some(&a.cap),
            Some(json!({ "display_name": "  Ada   Lovelace " })),
        )
        .await;
    assert_eq!(s, 200, "{v}");
    assert_eq!(v["name"]["display_name"], "Ada Lovelace");
    assert_eq!(v["name"]["visibility"], "known");
    // Known: a is "known" to friend only once a wrote to friend.
    let (s, _) = srv
        .call(
            reqwest::Method::PUT,
            "/v1/contacts",
            Some(&a.api_key),
            Some(json!({ "peer": friend.address, "admission": "allow" })),
        )
        .await;
    assert_eq!(s, 200);
    let path = format!("/v1/profiles?addresses={}", a.address);
    let (_, v) = srv.get(&path, &friend.token).await;
    assert_eq!(v["profiles"][&a.address]["display_name"], "Ada Lovelace");
    let (_, v) = srv.get(&path, &stranger.token).await;
    assert_eq!(v["profiles"][&a.address]["display_name"], Value::Null);
    let (s, _) = srv.call(reqwest::Method::GET, &path, None, None).await;
    assert_eq!(s, 401, "no anonymous lookup");
    // Everyone.
    let (s, _) = srv
        .call(
            reqwest::Method::PUT,
            "/v1/profile/name",
            Some(&a.token),
            Some(json!({ "display_name": "Ada", "visibility": "everyone" })),
        )
        .await;
    assert_eq!(s, 200);
    let (_, v) = srv.get(&path, &stranger.cap).await;
    assert_eq!(v["profiles"][&a.address]["display_name"], "Ada");
    // Removal.
    let (s, v) = srv
        .call(
            reqwest::Method::DELETE,
            "/v1/profile/name",
            Some(&a.token),
            None,
        )
        .await;
    assert_eq!((s, v["deleted"].as_bool()), (200, Some(true)));
    let (_, v) = srv.get(&path, &friend.token).await;
    assert_eq!(v["profiles"][&a.address]["display_name"], Value::Null);
    // Discovery carries a visible name.
    srv.call(
        reqwest::Method::PUT,
        "/v1/profile/name",
        Some(&a.token),
        Some(json!({ "display_name": "Ada", "visibility": "everyone" })),
    )
    .await;
    verify(&srv, &a, "+15555550180", true).await;
    let (_, v) = srv
        .post(
            "/v1/contacts/discover",
            Some(&stranger.token),
            json!({ "hashes": [h("+15555550180")] }),
        )
        .await;
    assert_eq!(v["matches"][0]["display_name"], "Ada");
}

#[tokio::test]
async fn retention_sweep() {
    let srv = serve().await;
    let a = person(&srv, "sweep").await;
    verify(&srv, &a, "+15555550190", true).await;
    srv.post(
        "/v1/contacts/discover",
        Some(&a.token),
        json!({ "hashes": [h("+15555550191")] }),
    )
    .await;
    let later = now_unix() + 40 * 86_400;
    let swept = srv.state.store.sweep_contacts(later).await.unwrap();
    assert_eq!(swept.marks, 1);
    assert!(swept.ledger >= 1);
    assert_eq!(swept.usage, 1);
    // A verified number is not a retention item.
    assert_eq!(srv.count("SELECT COUNT(*) FROM phone_numbers").await, 1);
}
