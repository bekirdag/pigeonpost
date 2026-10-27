//! The broker against a scripted postbox internal API, over real loopback sockets: admission by
//! one-time leg tokens, the splice, revocation from the postbox, idle and connect timeouts, and
//! failing closed when the postbox is unreachable.

use axum::extract::State;
use axum::http::HeaderMap;
use axum::routing::post;
use axum::{Json, Router};
use futures_util::{SinkExt, StreamExt};
use pigeonpost_tunnel::{Broker, Config};
use serde_json::{json, Value};
use std::collections::{HashMap, HashSet};
use std::sync::{Arc, Mutex};
use std::time::{Duration, SystemTime, UNIX_EPOCH};
use tokio_tungstenite::tungstenite::Message;

const TOKEN: &str = "svc_tunnel_0123456789abcdef0123456789abcdef";
const SID: &str = "ssh_abcdefghijklmnopqrstuvwxyz";

#[derive(Default)]
struct Fake {
    /// leg token -> (session, side)
    legs: Mutex<HashMap<String, (String, String)>>,
    connect_secs: Mutex<u64>,
    end: Mutex<HashSet<String>>,
    closed: Mutex<Vec<Value>>,
    checks: Mutex<Vec<Value>>,
    down: Mutex<bool>,
}

fn now() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap()
        .as_secs()
}

fn authed(h: &HeaderMap) -> bool {
    h.get("authorization").and_then(|v| v.to_str().ok()) == Some(&format!("Bearer {TOKEN}"))
}

async fn fake_postbox(fake: Arc<Fake>) -> String {
    async fn legs(
        State(f): State<Arc<Fake>>,
        h: HeaderMap,
        Json(b): Json<Value>,
    ) -> (axum::http::StatusCode, Json<Value>) {
        if !authed(&h) || *f.down.lock().unwrap() {
            return (
                axum::http::StatusCode::UNAUTHORIZED,
                Json(json!({"error": "invalid_service_token"})),
            );
        }
        let token = b["leg_token"].as_str().unwrap_or("").to_string();
        let found = f.legs.lock().unwrap().remove(&token);
        match found {
            Some((sid, side)) if sid == b["session_id"].as_str().unwrap_or("") => (
                axum::http::StatusCode::OK,
                Json(
                    json!({ "session_id": sid, "side": side, "ends_by": now() + 3600,
                             "connect_by": now() + *f.connect_secs.lock().unwrap(), "idle_secs": 900 }),
                ),
            ),
            _ => (
                axum::http::StatusCode::FORBIDDEN,
                Json(json!({"error": "leg_invalid"})),
            ),
        }
    }
    async fn check(
        State(f): State<Arc<Fake>>,
        h: HeaderMap,
        Json(b): Json<Value>,
    ) -> (axum::http::StatusCode, Json<Value>) {
        if !authed(&h) || *f.down.lock().unwrap() {
            return (axum::http::StatusCode::SERVICE_UNAVAILABLE, Json(json!({})));
        }
        f.checks.lock().unwrap().push(b.clone());
        let end: Vec<Value> = f
            .end
            .lock()
            .unwrap()
            .iter()
            .map(|s| json!({ "session_id": s, "reason": "delegation_required" }))
            .collect();
        (axum::http::StatusCode::OK, Json(json!({ "end": end })))
    }
    async fn closed(State(f): State<Arc<Fake>>, h: HeaderMap, Json(b): Json<Value>) -> Json<Value> {
        if authed(&h) {
            f.closed.lock().unwrap().push(b);
        }
        Json(json!({ "closed": true }))
    }
    let app = Router::new()
        .route("/internal/v1/shell/legs", post(legs))
        .route("/internal/v1/shell/check", post(check))
        .route("/internal/v1/shell/closed", post(closed))
        .with_state(fake);
    let l = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let url = format!("http://{}", l.local_addr().unwrap());
    tokio::spawn(async move { axum::serve(l, app).await.unwrap() });
    url
}

struct Rig {
    fake: Arc<Fake>,
    broker: Broker,
    url: String,
}

async fn rig(idle: u64, fail_closed: u64) -> Rig {
    let fake = Arc::new(Fake::default());
    *fake.connect_secs.lock().unwrap() = 60;
    let postbox = fake_postbox(fake.clone()).await;
    let broker = Broker::new(Config {
        postbox,
        token: TOKEN.into(),
        max_sessions: 10,
        idle: Duration::from_secs(idle),
        check_every: Duration::from_millis(300),
        fail_closed_after: Duration::from_secs(fail_closed),
        auth_timeout: Duration::from_secs(2),
        per_ip_per_minute: 1000,
        trust_proxy: false,
    });
    broker.start_heartbeat();
    let l = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let url = format!("ws://{}", l.local_addr().unwrap());
    let app = broker
        .router()
        .into_make_service_with_connect_info::<std::net::SocketAddr>();
    tokio::spawn(async move { axum::serve(l, app).await.unwrap() });
    Rig { fake, broker, url }
}

type Ws =
    tokio_tungstenite::WebSocketStream<tokio_tungstenite::MaybeTlsStream<tokio::net::TcpStream>>;

impl Rig {
    fn issue(&self, sid: &str) -> (String, String) {
        let c = format!("lt_client_{sid}");
        let a = format!("lt_agent_{sid}");
        let mut legs = self.fake.legs.lock().unwrap();
        legs.insert(c.clone(), (sid.into(), "client".into()));
        legs.insert(a.clone(), (sid.into(), "agent".into()));
        (c, a)
    }
    async fn dial(&self, sid: &str, token: Option<&str>) -> Ws {
        let (mut ws, _) = tokio_tungstenite::connect_async(format!("{}/t/{sid}", self.url))
            .await
            .unwrap();
        if let Some(t) = token {
            ws.send(Message::Text(json!({ "leg": t }).to_string().into()))
                .await
                .unwrap();
        }
        ws
    }
}

/// The next data message, or the close frame's (code, reason).
async fn next(ws: &mut Ws) -> Result<Vec<u8>, (u16, String)> {
    loop {
        match tokio::time::timeout(Duration::from_secs(8), ws.next()).await {
            Ok(Some(Ok(Message::Binary(b)))) => return Ok(b.to_vec()),
            Ok(Some(Ok(Message::Close(Some(f))))) => {
                return Err((f.code.into(), f.reason.to_string()))
            }
            Ok(Some(Ok(Message::Close(None)))) | Ok(None) | Ok(Some(Err(_))) => {
                return Err((0, String::new()))
            }
            Ok(Some(Ok(_))) => continue,
            Err(_) => panic!("timed out"),
        }
    }
}

#[tokio::test]
async fn splices_both_ways_and_reports_bytes() {
    let r = rig(900, 30).await;
    let (c, a) = r.issue(SID);
    let mut client = r.dial(SID, Some(&c)).await;
    let mut agent = r.dial(SID, Some(&a)).await;
    client
        .send(Message::Binary(b"SSH-2.0-client\r\n".to_vec().into()))
        .await
        .unwrap();
    assert_eq!(next(&mut agent).await.unwrap(), b"SSH-2.0-client\r\n");
    agent
        .send(Message::Binary(vec![7u8; 100_000].into()))
        .await
        .unwrap();
    assert_eq!(next(&mut client).await.unwrap().len(), 100_000);
    // Reported as spliced, with byte counts, to the postbox.
    tokio::time::sleep(Duration::from_millis(800)).await;
    let last = r.fake.checks.lock().unwrap().last().cloned().unwrap();
    let s = &last["sessions"][0];
    assert_eq!(
        (
            s["spliced"].as_bool(),
            s["bytes_client"].as_u64(),
            s["bytes_agent"].as_u64()
        ),
        (Some(true), Some(16), Some(100_000))
    );
    // A text frame after admission is a protocol violation: the session ends for both.
    client.send(Message::Text("hello".into())).await.unwrap();
    let (code, reason) = next(&mut agent).await.unwrap_err();
    assert_eq!((code, reason.as_str()), (4000, "client_protocol_violation"));
    tokio::time::sleep(Duration::from_millis(300)).await;
    let closed = r.fake.closed.lock().unwrap().clone();
    assert_eq!(closed.len(), 1, "reported once");
    assert_eq!(closed[0]["bytes_agent"], 100_000);
    assert_eq!(closed[0]["reason"], "client_protocol_violation");
    assert_eq!(r.broker.session_count(), 0);
}

#[tokio::test]
async fn admission_needs_a_valid_one_time_token() {
    let r = rig(900, 30).await;
    let (c, _a) = r.issue(SID);
    // No token, a wrong token, a token for another session: refused before anything is spliced.
    let mut ws = r.dial(SID, None).await;
    ws.send(Message::Binary(b"x".to_vec().into()))
        .await
        .unwrap();
    assert_eq!(next(&mut ws).await.unwrap_err().0, 4400);
    let mut ws = r.dial(SID, Some("lt_nope")).await;
    assert_eq!(next(&mut ws).await.unwrap_err().0, 4403);
    let other = "ssh_bbbbbbbbbbbbbbbbbbbbbbbbbb";
    let mut ws = r.dial(other, Some(&c)).await;
    assert_eq!(next(&mut ws).await.unwrap_err().0, 4403);
    // A token is spent on first presentation; a second presentation is refused.
    let r2 = rig(900, 30).await;
    let (c2, _) = r2.issue(SID);
    let _first = r2.dial(SID, Some(&c2)).await;
    tokio::time::sleep(Duration::from_millis(200)).await;
    let mut again = r2.dial(SID, Some(&c2)).await;
    assert_eq!(next(&mut again).await.unwrap_err().0, 4403, "spent");
    // Session ids are validated before upgrading.
    assert!(
        tokio_tungstenite::connect_async(format!("{}/t/..%2F..%2Fetc", r.url))
            .await
            .is_err()
    );
    assert!(tokio_tungstenite::connect_async(format!("{}/x", r.url))
        .await
        .is_err());
}

#[tokio::test]
async fn a_revocation_from_the_postbox_ends_both_sides() {
    let r = rig(900, 30).await;
    let (c, a) = r.issue(SID);
    let mut client = r.dial(SID, Some(&c)).await;
    let mut agent = r.dial(SID, Some(&a)).await;
    client
        .send(Message::Binary(b"hi".to_vec().into()))
        .await
        .unwrap();
    assert_eq!(next(&mut agent).await.unwrap(), b"hi");
    r.fake.end.lock().unwrap().insert(SID.into());
    let (code, reason) = next(&mut client).await.unwrap_err();
    assert_eq!(
        (code, reason.as_str()),
        (4000, "revoked:delegation_required")
    );
    assert_eq!(next(&mut agent).await.unwrap_err().0, 4000);
}

#[tokio::test]
async fn idle_sessions_end() {
    let r = rig(1, 30).await;
    let (c, a) = r.issue(SID);
    let mut client = r.dial(SID, Some(&c)).await;
    let _agent = r.dial(SID, Some(&a)).await;
    let (code, reason) = next(&mut client).await.unwrap_err();
    assert_eq!((code, reason.as_str()), (4000, "idle"));
}

#[tokio::test]
async fn a_leg_whose_peer_never_comes_times_out() {
    let r = rig(900, 30).await;
    *r.fake.connect_secs.lock().unwrap() = 1;
    let (c, _a) = r.issue(SID);
    let mut client = r.dial(SID, Some(&c)).await;
    let (code, reason) = next(&mut client).await.unwrap_err();
    assert_eq!((code, reason.as_str()), (4408, "connect_timeout"));
}

#[tokio::test]
async fn an_unreachable_postbox_fails_closed() {
    let r = rig(900, 1).await;
    let (c, a) = r.issue(SID);
    let mut client = r.dial(SID, Some(&c)).await;
    let mut agent = r.dial(SID, Some(&a)).await;
    client
        .send(Message::Binary(b"x".to_vec().into()))
        .await
        .unwrap();
    assert_eq!(next(&mut agent).await.unwrap(), b"x");
    *r.fake.down.lock().unwrap() = true;
    let (code, reason) = next(&mut client).await.unwrap_err();
    assert_eq!((code, reason.as_str()), (4000, "postbox_unreachable"));
}
