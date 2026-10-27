//! Phase 5 webhook tests: registration-time SSRF checks, signed delivery to a local receiver,
//! retries with backoff, the 24 h window, rotation, DNS rebinding and redirect refusal, the
//! durable outbox across a restart, and the trigger-recorded event set (SDS §18.5).

use crate::org_api_tests::{api, code, get, make_key};
use crate::org_tests::{enc, serve_with, Person, Srv};
use crate::webhooks::{self, Egress, Resolver};
use crate::*;
use ed25519_dalek::{Signer, SigningKey};
use reqwest::Method;
use serde_json::{json, Value};
use std::collections::HashMap;
use std::net::{IpAddr, SocketAddr};
use std::sync::Mutex;

// ---- a fake resolver -----------------------------------------------------------------------------

/// Host → addresses, changeable mid-test (DNS rebinding).
#[derive(Clone, Default)]
struct Dns(Arc<Mutex<HashMap<String, Vec<IpAddr>>>>);

impl Dns {
    fn set(&self, host: &str, ips: &[&str]) {
        self.0.lock().unwrap().insert(
            host.to_string(),
            ips.iter().map(|i| i.parse().unwrap()).collect(),
        );
    }
    fn resolver(&self) -> Resolver {
        let map = self.0.clone();
        Arc::new(move |host: String, port: u16| {
            let found = map.lock().unwrap().get(&host).cloned();
            Box::pin(async move {
                match found {
                    Some(ips) => Ok(ips
                        .into_iter()
                        .map(|ip| SocketAddr::new(ip, port))
                        .collect()),
                    None => Err(std::io::Error::new(
                        std::io::ErrorKind::NotFound,
                        "nxdomain",
                    )),
                }
            })
        })
    }
}

// ---- a local receiver --------------------------------------------------------------------------

#[derive(Clone, Debug)]
struct Hit {
    path: String,
    headers: HashMap<String, String>,
    body: Vec<u8>,
}

#[derive(Clone, Default)]
struct Receiver {
    hits: Arc<Mutex<Vec<Hit>>>,
    /// Statuses to answer with, in order; then 200.
    script: Arc<Mutex<Vec<u16>>>,
    port: u16,
}

impl Receiver {
    async fn start(script: &[u16]) -> Receiver {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let port = listener.local_addr().unwrap().port();
        let r = Receiver {
            hits: Arc::default(),
            script: Arc::new(Mutex::new(script.to_vec())),
            port,
        };
        let r2 = r.clone();
        let app = axum::Router::new().fallback(move |req: axum::extract::Request| {
            let r = r2.clone();
            async move {
                let path = req.uri().path().to_string();
                let headers = req
                    .headers()
                    .iter()
                    .map(|(k, v)| (k.as_str().to_string(), v.to_str().unwrap_or("").to_string()))
                    .collect();
                let body = axum::body::to_bytes(req.into_body(), 1 << 20)
                    .await
                    .unwrap()
                    .to_vec();
                r.hits.lock().unwrap().push(Hit {
                    path,
                    headers,
                    body,
                });
                let status = {
                    let mut s = r.script.lock().unwrap();
                    if s.is_empty() {
                        200
                    } else {
                        s.remove(0)
                    }
                };
                let mut resp = axum::response::Response::new(axum::body::Body::from("ok"));
                *resp.status_mut() = StatusCode::from_u16(status).unwrap();
                if (300..400).contains(&status) {
                    resp.headers_mut().insert(
                        header::LOCATION,
                        HeaderValue::from_str(&format!("http://127.0.0.1:{}/elsewhere", r.port))
                            .unwrap(),
                    );
                }
                resp
            }
        });
        tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
        r
    }
    fn hits(&self) -> Vec<Hit> {
        self.hits.lock().unwrap().clone()
    }
    fn url(&self, host: &str) -> String {
        format!("http://{host}:{}/hook", self.port)
    }
}

// ---- setup -------------------------------------------------------------------------------------

async fn setup_with(dns: &Dns, insecure: bool, db: Option<&str>) -> (Srv, Person, String) {
    let egress = Egress::for_test(dns.resolver(), insecure);
    let db = db.map(str::to_string);
    let srv = serve_with(move |s| {
        s.webhook_egress = Arc::new(egress);
        if let Some(path) = &db {
            s.store = Arc::new(store::Store::open(path).unwrap());
        }
    })
    .await;
    for f in ["orgs", "groups", "org_api"] {
        srv.global_flag(f).await;
    }
    let owner = srv.person("owner").await;
    let org = srv.org(&owner, "acme", 5).await;
    let (s, v) = srv
        .post(
            &format!("/v1/orgs/{org}/me/mailbox"),
            &owner.token,
            json!({ "handle": "boss" }),
        )
        .await;
    assert_eq!(s, 201, "{v}");
    (srv, owner, org)
}

async fn add_hook(
    srv: &Srv,
    org: &str,
    owner: &Person,
    url: &str,
    events: Value,
) -> (String, String) {
    let (s, v) = srv
        .post(
            &format!("/v1/orgs/{org}/webhooks"),
            &owner.token,
            json!({ "url": url, "events": events }),
        )
        .await;
    assert_eq!(s, 201, "{v}");
    (
        v["webhook"]["id"].as_str().unwrap().to_string(),
        v["secret"].as_str().unwrap().to_string(),
    )
}

async fn invite(srv: &Srv, org: &str, owner: &Person, handle: &str) {
    let (s, v) = srv
        .post(&format!("/v1/orgs/{org}/invites"), &owner.token, json!({ "invites": [{
            "first_name": "F", "last_name": "L", "contact": format!("{handle}@example.com"), "handle": handle,
        }]}))
        .await;
    assert_eq!(s, 200, "{v}");
}

async fn deliveries(srv: &Srv, org: &str, owner: &Person, hook: &str) -> Vec<Value> {
    let (s, v) = srv
        .get(
            &format!("/v1/orgs/{org}/webhooks/{hook}/deliveries"),
            &owner.token,
        )
        .await;
    assert_eq!(s, 200, "{v}");
    v["deliveries"].as_array().unwrap().clone()
}

async fn sql<T: Send + 'static>(
    srv: &Srv,
    f: impl FnOnce(&rusqlite::Transaction<'_>) -> rusqlite::Result<T> + Send + 'static,
) -> T {
    srv.state
        .store
        .group_tx(move |tx| -> crate::groups::GResult<T> { Ok(f(tx)?) })
        .await
        .unwrap()
}

// ---- registration ------------------------------------------------------------------------------

#[tokio::test]
async fn webhook_urls_are_checked_against_the_egress_policy_at_registration() {
    let dns = Dns::default();
    dns.set("hooks.example.com", &["93.184.215.14"]);
    dns.set("internal.example.com", &["10.0.0.5"]);
    dns.set("meta.example.com", &["169.254.169.254"]);
    dns.set("mixed.example.com", &["93.184.215.14", "127.0.0.1"]);
    dns.set("v6.example.com", &["fd00::1"]);
    dns.set("mapped.example.com", &["::ffff:192.168.0.1"]);
    let (srv, owner, org) = setup_with(&dns, false, None).await;
    let path = format!("/v1/orgs/{org}/webhooks");
    for bad in [
        "https://internal.example.com/hook",
        "https://meta.example.com/hook",
        "https://mixed.example.com/hook",
        "https://v6.example.com/hook",
        "https://mapped.example.com/hook",
        "https://nxdomain.example.com/hook",
        "http://hooks.example.com/hook",
        "https://hooks.example.com:8080/hook",
        "https://127.0.0.1/hook",
        "https://[::1]/hook",
        "https://169.254.169.254/latest/meta-data/",
        "https://localhost/hook",
        "https://user:pw@hooks.example.com/hook",
    ] {
        let (s, v) = srv
            .post(&path, &owner.token, json!({ "url": bad, "events": ["*"] }))
            .await;
        assert_eq!(
            (s, v["error"].as_str()),
            (400, Some("invalid_webhook_url")),
            "{bad}: {v}"
        );
    }
    let (s, v) = srv
        .post(
            &path,
            &owner.token,
            json!({ "url": "https://hooks.example.com/hook", "events": ["nonsense"] }),
        )
        .await;
    assert_eq!((s, v["error"].as_str()), (400, Some("bad_request")));
    let (_, secret) = add_hook(
        &srv,
        &org,
        &owner,
        "https://hooks.example.com/hook",
        json!(["member.joined"]),
    )
    .await;
    assert!(secret.starts_with("whsec_"));
    // The secret is sealed at rest, never stored plainly.
    let raw: Vec<u8> = sql(&srv, |tx| {
        tx.query_row("SELECT secret_sealed FROM webhooks", [], |r| r.get(0))
    })
    .await;
    assert!(!String::from_utf8_lossy(&raw).contains(&secret));
    // Ten per org.
    for i in 1..10 {
        add_hook(
            &srv,
            &org,
            &owner,
            &format!("https://hooks.example.com/{i}"),
            json!(["*"]),
        )
        .await;
    }
    let (s, v) = srv
        .post(
            &path,
            &owner.token,
            json!({ "url": "https://hooks.example.com/11", "events": ["*"] }),
        )
        .await;
    assert_eq!((s, v["error"].as_str()), (409, Some("webhook_limit")));
    // People admins cannot manage webhooks.
    let pa = srv.person("pa").await;
    srv.join(&org, &owner, &pa, "pa", &["people_admin"]).await;
    assert_eq!(srv.get(&path, &pa.token).await.0, 403);
}

// ---- delivery ----------------------------------------------------------------------------------

#[tokio::test]
async fn deliveries_are_signed_retried_with_backoff_and_logged() {
    let dns = Dns::default();
    dns.set("hooks.test.example", &["127.0.0.1"]);
    let rx = Receiver::start(&[500]).await;
    let (srv, owner, org) = setup_with(&dns, true, None).await;
    // No webhook yet: nothing is recorded for this org at all.
    invite(&srv, &org, &owner, "early").await;
    let n: i64 = sql(&srv, |tx| {
        tx.query_row("SELECT COUNT(*) FROM webhook_events", [], |r| r.get(0))
    })
    .await;
    assert_eq!(n, 0);

    let (hook, secret) = add_hook(
        &srv,
        &org,
        &owner,
        &rx.url("hooks.test.example"),
        json!(["member.invited"]),
    )
    .await;
    invite(&srv, &org, &owner, "jane").await;
    let now = now_unix();
    assert_eq!(webhooks::tick(&srv.state, now).await.unwrap(), 1);
    let log = deliveries(&srv, &org, &owner, &hook).await;
    assert_eq!(log[0]["status"], "retrying");
    assert_eq!(log[0]["response_status"], 500);
    assert_eq!(log[0]["attempt"], 1);
    // Not due yet; then due after the first backoff step.
    assert_eq!(webhooks::tick(&srv.state, now + 5).await.unwrap(), 0);
    assert_eq!(
        webhooks::tick(&srv.state, now_unix() + 31).await.unwrap(),
        1
    );
    let log = deliveries(&srv, &org, &owner, &hook).await;
    assert_eq!(log[0]["status"], "delivered", "{log:?}");
    assert_eq!(log[0]["attempt"], 2);
    assert_eq!(
        webhooks::tick(&srv.state, now_unix() + 10_000)
            .await
            .unwrap(),
        0,
        "done"
    );

    let hits = rx.hits();
    assert_eq!(hits.len(), 2);
    let (a, b) = (&hits[0], &hits[1]);
    assert_eq!(
        a.headers["pigeonpost-event-id"],
        b.headers["pigeonpost-event-id"]
    );
    assert_ne!(
        a.headers["pigeonpost-delivery-id"],
        b.headers["pigeonpost-delivery-id"]
    );
    assert_eq!(a.body, b.body, "the same bytes every attempt");
    assert_eq!(a.headers["pigeonpost-event-type"], "member.invited");
    assert_eq!(a.headers["user-agent"], "Pigeonpost-Webhooks/1");
    assert_eq!(a.headers["content-type"], "application/json");
    let sig = &b.headers["pigeonpost-signature"];
    assert!(
        webhooks::verify_signature(sig, &secret, &b.body, now_unix() as i64, 300),
        "{sig}"
    );
    assert!(!webhooks::verify_signature(
        sig,
        "whsec_wrong",
        &b.body,
        now_unix() as i64,
        300
    ));
    let payload: Value = serde_json::from_slice(&b.body).unwrap();
    assert_eq!(payload["type"], "member.invited");
    assert_eq!(payload["org_id"], org);
    assert!(payload["id"].as_str().unwrap().starts_with("evt_"));
    assert!(payload["created_at"].as_str().unwrap().ends_with('Z'));
    assert_eq!(payload["data"]["handle"], "/acme/jane");
    // Ids and handles only: the contact address never leaves.
    assert_eq!(payload["data"].as_object().unwrap().len(), 2, "{payload}");
    assert!(!String::from_utf8_lossy(&b.body).contains("example.com"));

    // With the flag off the deliveries wait; back on, they go.
    srv.call(
        Method::PUT,
        "/internal/v1/features/org_api",
        Some(crate::org_tests::OPS),
        Some(json!({ "scope": "global", "enabled": false })),
    )
    .await;
    invite(&srv, &org, &owner, "paused").await;
    assert_eq!(webhooks::tick(&srv.state, now_unix()).await.unwrap(), 0);
    assert_eq!(rx.hits().len(), 2);
    srv.global_flag("org_api").await;
    assert_eq!(
        webhooks::tick(&srv.state, now_unix() + 301).await.unwrap(),
        1
    );
    assert_eq!(rx.hits().len(), 3);

    // Rotation: both secrets sign for a day.
    let (s, rot) = srv
        .post(
            &format!("/v1/orgs/{org}/webhooks/{hook}/rotate"),
            &owner.token,
            json!({}),
        )
        .await;
    assert_eq!(s, 200, "{rot}");
    let fresh = rot["secret"].as_str().unwrap().to_string();
    assert_ne!(fresh, secret);
    invite(&srv, &org, &owner, "kim").await;
    webhooks::tick(&srv.state, now_unix()).await.unwrap();
    let last = rx.hits().last().unwrap().clone();
    let sig = &last.headers["pigeonpost-signature"];
    assert_eq!(sig.matches("v1=").count(), 2);
    for s in [&fresh, &secret] {
        assert!(webhooks::verify_signature(
            sig,
            s,
            &last.body,
            now_unix() as i64,
            300
        ));
    }
}

#[tokio::test]
async fn retries_stop_after_24_hours_and_deleting_a_webhook_cancels_its_backlog() {
    let dns = Dns::default();
    dns.set("hooks.test.example", &["127.0.0.1"]);
    let rx = Receiver::start(&[503, 503, 503, 503]).await;
    let (srv, owner, org) = setup_with(&dns, true, None).await;
    let (hook, _) = add_hook(
        &srv,
        &org,
        &owner,
        &rx.url("hooks.test.example"),
        json!(["*"]),
    )
    .await;
    invite(&srv, &org, &owner, "jane").await;
    webhooks::tick(&srv.state, now_unix()).await.unwrap();
    // The event is now more than a day old.
    sql(&srv, |tx| {
        tx.execute(
            "UPDATE webhook_events SET created_at = created_at - 90000",
            [],
        )
    })
    .await;
    webhooks::tick(&srv.state, now_unix() + 3_600)
        .await
        .unwrap();
    let log = deliveries(&srv, &org, &owner, &hook).await;
    assert_eq!(log[0]["status"], "failed", "{log:?}");
    let state: String = sql(&srv, |tx| {
        tx.query_row("SELECT state FROM webhook_outbox", [], |r| r.get(0))
    })
    .await;
    assert_eq!(state, "failed");
    assert_eq!(
        webhooks::tick(&srv.state, now_unix() + 90_000)
            .await
            .unwrap(),
        0
    );

    // A backlog for a deleted webhook is cancelled, not sent.
    invite(&srv, &org, &owner, "kim").await;
    let (s, _) = srv
        .call(
            Method::DELETE,
            &format!("/v1/orgs/{org}/webhooks/{hook}"),
            Some(&owner.token),
            None,
        )
        .await;
    assert_eq!(s, 200);
    let before = rx.hits().len();
    assert_eq!(webhooks::tick(&srv.state, now_unix()).await.unwrap(), 0);
    assert_eq!(rx.hits().len(), before);
}

#[tokio::test]
async fn dns_rebinding_after_registration_is_blocked_at_delivery() {
    let dns = Dns::default();
    dns.set("rebind.test.example", &["127.0.0.1"]);
    let rx = Receiver::start(&[]).await;
    let (srv, owner, org) = setup_with(&dns, true, None).await;
    let (hook, _) = add_hook(
        &srv,
        &org,
        &owner,
        &rx.url("rebind.test.example"),
        json!(["*"]),
    )
    .await;
    // The name now answers with a private address (and, separately, a public + private mix).
    for answer in [vec!["10.0.0.7"], vec!["127.0.0.1", "192.168.1.1"]] {
        dns.set("rebind.test.example", &answer);
        invite(&srv, &org, &owner, &format!("p{}", answer.len())).await;
        webhooks::tick(&srv.state, now_unix() + 100_000)
            .await
            .unwrap();
    }
    let log = deliveries(&srv, &org, &owner, &hook).await;
    assert!(log.len() >= 2);
    for d in &log {
        assert_eq!(d["status"], "blocked", "{d}");
        assert_eq!(d["error"], "blocked_destination");
    }
    assert!(rx.hits().is_empty(), "no connection was made");
}

#[tokio::test]
async fn redirects_are_never_followed() {
    let dns = Dns::default();
    dns.set("hooks.test.example", &["127.0.0.1"]);
    let rx = Receiver::start(&[302]).await;
    let (srv, owner, org) = setup_with(&dns, true, None).await;
    let (hook, _) = add_hook(
        &srv,
        &org,
        &owner,
        &rx.url("hooks.test.example"),
        json!(["*"]),
    )
    .await;
    invite(&srv, &org, &owner, "jane").await;
    webhooks::tick(&srv.state, now_unix()).await.unwrap();
    let log = deliveries(&srv, &org, &owner, &hook).await;
    assert_eq!(log[0]["status"], "retrying");
    assert_eq!(log[0]["response_status"], 302);
    assert_eq!(log[0]["error"], "redirect_not_followed");
    let hits = rx.hits();
    assert_eq!(hits.len(), 1);
    assert_eq!(
        hits[0].path, "/hook",
        "the redirect target was not requested"
    );
}

#[tokio::test]
async fn the_outbox_survives_a_restart_and_a_lost_worker() {
    let dir = std::env::temp_dir().join(format!("pp-webhooks-{}", crate::groups::rand_b32(8)));
    std::fs::create_dir_all(&dir).unwrap();
    let db = dir.join("postbox.db").to_string_lossy().to_string();
    let dns = Dns::default();
    dns.set("hooks.test.example", &["127.0.0.1"]);
    let rx = Receiver::start(&[]).await;
    let hook = {
        let (srv, owner, org) = setup_with(&dns, true, Some(&db)).await;
        let (hook, _) = add_hook(
            &srv,
            &org,
            &owner,
            &rx.url("hooks.test.example"),
            json!(["*"]),
        )
        .await;
        invite(&srv, &org, &owner, "jane").await;
        // Fan out, and pretend a worker claimed the delivery and then died mid-flight.
        sql(&srv, |tx| {
            let now = now_unix() as i64;
            tx.execute("UPDATE webhook_events SET fanned_out = 1", [])?;
            tx.execute(
                "INSERT INTO webhook_outbox (webhook_id, event_id, state, attempts, next_attempt_at, lease_until, created_at)
                 SELECT w.id, e.id, 'pending', 1, ?1, ?1 + 60, ?1 FROM webhooks w, webhook_events e",
                [now],
            )
        })
        .await;
        hook
        // The first process's state is dropped here.
    };
    // A new process over the same database: the lease holds for its minute, then it delivers.
    let egress = Egress::for_test(dns.resolver(), true);
    let srv2 = serve_with(move |s| {
        s.webhook_egress = Arc::new(egress);
        s.store = Arc::new(store::Store::open(&db).unwrap());
    })
    .await;
    assert_eq!(
        webhooks::tick(&srv2.state, now_unix()).await.unwrap(),
        0,
        "still leased"
    );
    assert!(rx.hits().is_empty());
    assert_eq!(
        webhooks::tick(&srv2.state, now_unix() + 61).await.unwrap(),
        1
    );
    let hits = rx.hits();
    assert_eq!(hits.len(), 1);
    let state: (String, i64) = sql(&srv2, move |tx| {
        tx.query_row(
            "SELECT state, attempts FROM webhook_outbox WHERE webhook_id = ?1",
            [&hook],
            |r| Ok((r.get(0)?, r.get(1)?)),
        )
    })
    .await;
    assert_eq!(state, ("delivered".to_string(), 2));
    drop(srv2);
    let _ = std::fs::remove_dir_all(&dir);
}

// ---- the event set -----------------------------------------------------------------------------

#[tokio::test]
async fn every_documented_event_is_recorded_from_its_source() {
    let dns = Dns::default();
    dns.set("hooks.example.com", &["93.184.215.14"]);
    let (srv, owner, org) = setup_with(&dns, false, None).await;
    add_hook(
        &srv,
        &org,
        &owner,
        "https://hooks.example.com/all",
        json!(["*"]),
    )
    .await;

    let jane = srv.person("jane").await;
    let (jane_id, _) = srv.join(&org, &owner, &jane, "jane", &[]).await; // invited, joined
    srv.patch(
        &format!("/v1/orgs/{org}/members/{jane_id}"),
        &owner.token,
        json!({ "status": "suspended" }),
    )
    .await;
    srv.post(
        &format!("/v1/orgs/{org}/members/{jane_id}/remove"),
        &owner.token,
        json!({ "mode": "delete" }),
    )
    .await;
    let (_, a) = srv
        .post(
            &format!("/v1/orgs/{org}/agents"),
            &owner.token,
            json!({ "handle": "bot" }),
        )
        .await;
    let agent = a["agent"]["address"].as_str().unwrap().to_string();
    let (s, e) = srv
        .post(
            &format!("/v1/orgs/{org}/agents/{}/enrollments", enc(&agent)),
            &owner.strong,
            json!({ "ttl_hours": 1 }),
        )
        .await;
    assert_eq!(s, 201, "{e}");
    let code_value = e["code"].as_str().unwrap().to_string();
    let k = SigningKey::from_bytes(&[9u8; 32]);
    let (s, r) = srv
        .call(Method::POST, "/v1/enrollments/redeem", None, Some(json!({
            "code": code_value, "device_public_key": crate::org_tests::b64(k.verifying_key().as_bytes()),
            "signature": crate::org_tests::b64(&k.sign(format!("pigeonpost-enroll-v1\0{code_value}").as_bytes()).to_bytes()),
        })))
        .await;
    assert_eq!(s, 200, "{r}"); // agent.connected
    let device = r["device_id"].as_str().unwrap().to_string();
    srv.call(
        Method::DELETE,
        &format!("/v1/orgs/{org}/devices/{device}"),
        Some(&owner.token),
        None,
    )
    .await; // disconnected
    let (s, room) = srv
        .post(
            &format!("/v1/orgs/{org}/rooms"),
            &owner.token,
            json!({ "name": "Ops" }),
        )
        .await;
    assert_eq!(s, 201, "{room}");
    let rid = room["room"]["id"].as_str().unwrap().to_string();
    srv.call(
        Method::DELETE,
        &format!("/v1/orgs/{org}/rooms/{rid}"),
        Some(&owner.token),
        None,
    )
    .await;
    // Payment lapses: the lifecycle moves the org to past_due.
    srv.entitlement(&org, 5, "expired", now_unix() as i64 - 86_400, 2, false)
        .await;

    let events: Vec<(String, String)> = sql(&srv, |tx| {
        let mut st = tx.prepare("SELECT type, data FROM webhook_events ORDER BY rowid")?;
        let rows = st.query_map([], |r| Ok((r.get(0)?, r.get(1)?)))?;
        rows.collect::<Result<Vec<_>, _>>()
    })
    .await;
    let kinds: Vec<&str> = events.iter().map(|(k, _)| k.as_str()).collect();
    for want in [
        "member.invited",
        "member.joined",
        "member.suspended",
        "member.removed",
        "agent.created",
        "agent.connected",
        "agent.disconnected",
        "room.created",
        "room.deleted",
        "billing.status_changed",
    ] {
        assert!(kinds.contains(&want), "{want} missing from {kinds:?}");
    }
    let billing = events
        .iter()
        .find(|(k, _)| k == "billing.status_changed")
        .unwrap();
    let data: Value = serde_json::from_str(&billing.1).unwrap();
    assert_eq!(data["previous_status"], "active");
    // Every payload is ids and handles: nothing that looks like content or a contact.
    for (_, d) in &events {
        assert!(!d.contains("example.com") && !d.contains("body"), "{d}");
    }
    // Fan-out queues each for the one subscribed webhook.
    webhooks::tick(&srv.state, now_unix()).await.unwrap();
    let queued: i64 = sql(&srv, |tx| {
        tx.query_row("SELECT COUNT(*) FROM webhook_outbox", [], |r| r.get(0))
    })
    .await;
    assert_eq!(queued as usize, events.len());
}

// ---- the org API's webhook routes --------------------------------------------------------------

#[tokio::test]
async fn webhooks_through_the_org_api() {
    let dns = Dns::default();
    dns.set("hooks.example.com", &["93.184.215.14"]);
    dns.set("internal.example.com", &["10.1.1.1"]);
    let (srv, owner, org) = setup_with(&dns, false, None).await;
    let (_, key) = make_key(
        &srv,
        &org,
        &owner,
        &["org:read", "webhooks:write"],
        json!({}),
    )
    .await;
    let (_, reader) = make_key(&srv, &org, &owner, &["org:read"], json!({})).await;
    let r = api(
        &srv,
        Method::POST,
        "/org/v1/webhooks",
        Some(&key),
        Some(json!({ "url": "https://internal.example.com/x", "events": ["*"] })),
        None,
    )
    .await;
    assert_eq!(code(&r), (400, "invalid_webhook_url"));
    let r = api(
        &srv,
        Method::POST,
        "/org/v1/webhooks",
        Some(&reader),
        Some(json!({ "url": "https://hooks.example.com/x", "events": ["*"] })),
        None,
    )
    .await;
    assert_eq!(code(&r), (403, "insufficient_scope"));
    let r = api(&srv, Method::POST, "/org/v1/webhooks", Some(&key),
        Some(json!({ "url": "https://hooks.example.com/x", "events": ["member.joined", "room.created"],
                     "description": "SIEM" })), Some("wh-1")).await;
    assert_eq!(r.status, 201, "{}", r.body);
    assert_eq!(r.headers.get("cache-control").unwrap(), "no-store");
    let id = r.body["data"]["webhook"]["id"]
        .as_str()
        .unwrap()
        .to_string();
    let secret = r.body["data"]["secret"].as_str().unwrap().to_string();
    // An idempotent retry returns the same secret (the record is sealed at rest).
    let again = api(&srv, Method::POST, "/org/v1/webhooks", Some(&key),
        Some(json!({ "url": "https://hooks.example.com/x", "events": ["member.joined", "room.created"],
                     "description": "SIEM" })), Some("wh-1")).await;
    assert_eq!(again.body["data"]["secret"], secret.as_str());
    let raw: Vec<u8> = sql(&srv, |tx| {
        tx.query_row("SELECT response FROM org_api_idempotency", [], |r| r.get(0))
    })
    .await;
    assert!(!String::from_utf8_lossy(&raw).contains(&secret));
    let list = get(&srv, "/webhooks", &reader).await;
    assert_eq!(list.body["data"][0]["id"], id);
    assert!(!list.body.to_string().contains(&secret));
    let r = api(
        &srv,
        Method::POST,
        &format!("/org/v1/webhooks/{id}/rotate"),
        Some(&key),
        None,
        None,
    )
    .await;
    assert_eq!(r.status, 200, "{}", r.body);
    assert!(r.body["data"]["previous_secret_expires_at"]
        .as_str()
        .unwrap()
        .ends_with('Z'));
    let r = get(&srv, &format!("/webhooks/{id}/deliveries"), &reader).await;
    assert_eq!(r.body["data"], json!([]));
    let r = api(
        &srv,
        Method::DELETE,
        &format!("/org/v1/webhooks/{id}"),
        Some(&key),
        None,
        None,
    )
    .await;
    assert_eq!(r.status, 200, "{}", r.body);
    assert_eq!(
        code(&get(&srv, &format!("/webhooks/{id}/deliveries"), &reader).await),
        (404, "not_found")
    );
}
