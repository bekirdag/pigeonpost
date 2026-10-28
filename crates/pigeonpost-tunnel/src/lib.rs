//! The Pigeonpost tunnel broker (SDS §11.3, `docs/agent-ssh.md`).
//!
//! Agent machines expose no ports. An SSH session to an agent's workspace is two outbound
//! WebSocket connections — one from the person's client, one from the agent's `agentd` — that this
//! broker splices together at `/t/<session id>`. What crosses the splice is the SSH protocol
//! itself, encrypted end to end between the client and the agent: the broker holds no key that
//! opens it, and the agent's host key is pinned by the client, so a broker that tried to sit in the
//! middle would be refused.
//!
//! What it does know, and all it logs: a session id, which side connected, byte counts, duration
//! and why the session ended.
//!
//! # Admission
//!
//! Each leg presents, as its first WebSocket message, a one-time **leg token** the postbox issued
//! for that session and side. The broker spends it at the postbox (`POST /internal/v1/shell/legs`
//! with its own service token) and learns which side it is and the session's limits. A token works
//! once, for one session, before the session's connect deadline.
//!
//! # Ending a session
//!
//! A session ends when either side hangs up, after [`Config::idle`] with no traffic in either
//! direction (at most 15 minutes), at the postbox's absolute deadline (at most 8 hours), when its
//! two legs do not meet before the connect deadline, or when the postbox says so — the broker
//! reports every live session every [`Config::check_every`] and ends the ones the postbox no
//! longer authorizes (a revoked delegation, a removed member, a killed session). If the postbox
//! cannot be reached for [`Config::fail_closed_after`], every session ends: the broker never keeps
//! a shell open on its own authority.

use axum::extract::ws::{CloseFrame, Message, WebSocket, WebSocketUpgrade};
use axum::extract::{ConnectInfo, Path, State};
use axum::http::{HeaderMap, StatusCode};
use axum::response::{IntoResponse, Response};
use axum::routing::get;
use axum::{Json, Router};
use futures_util::{SinkExt, StreamExt};
use serde_json::{json, Value};
use std::collections::HashMap;
use std::net::SocketAddr;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};
use tokio::sync::{mpsc, watch};

/// Largest WebSocket message accepted from either side. SSH packets are at most 35 000 bytes and
/// both of Pigeonpost's clients send at most 32 KiB per message.
pub const MAX_MESSAGE: usize = 256 * 1024;

#[derive(Clone, Debug)]
pub struct Config {
    /// The postbox's internal API, e.g. `http://127.0.0.1:18991`.
    pub postbox: String,
    /// This service's `svc_…` token (`POSTBOX_SVC_TOKEN_TUNNEL` on the postbox).
    pub token: String,
    pub max_sessions: usize,
    /// Upper bound on idleness; the postbox's own value applies when it is lower.
    pub idle: Duration,
    pub check_every: Duration,
    pub fail_closed_after: Duration,
    /// How long a leg may take to present its token after connecting.
    pub auth_timeout: Duration,
    /// New connections per client address per minute.
    pub per_ip_per_minute: usize,
    /// Take the client address from `X-Real-IP` — only on the Unix socket (Phase 9), which only
    /// the proxy can reach. Over TCP the header is never trusted, loopback or not: any local
    /// process can connect from loopback.
    pub trust_proxy: bool,
}

impl Config {
    pub fn from_env() -> Result<Config, String> {
        let var = |k: &str| std::env::var(k).ok().map(|v| v.trim().to_string());
        let num = |k: &str, d: u64| var(k).and_then(|v| v.parse().ok()).unwrap_or(d);
        let postbox = var("TUNNEL_POSTBOX_INTERNAL_URL")
            .filter(|v| !v.is_empty())
            .ok_or("TUNNEL_POSTBOX_INTERNAL_URL is required")?;
        let token = var("TUNNEL_SERVICE_TOKEN")
            .filter(|t| t.starts_with("svc_") && t.len() >= 36)
            .ok_or("TUNNEL_SERVICE_TOKEN must be an svc_ token")?;
        Ok(Config {
            postbox: postbox.trim_end_matches('/').to_string(),
            token,
            max_sessions: num("TUNNEL_MAX_SESSIONS", 200) as usize,
            idle: Duration::from_secs(num("TUNNEL_IDLE_SECS", 900).clamp(10, 900)),
            check_every: Duration::from_secs(num("TUNNEL_CHECK_SECS", 10).clamp(1, 30)),
            fail_closed_after: Duration::from_secs(
                num("TUNNEL_FAIL_CLOSED_SECS", 60).clamp(5, 300),
            ),
            auth_timeout: Duration::from_secs(10),
            per_ip_per_minute: num("TUNNEL_PER_IP_PER_MINUTE", 60) as usize,
            trust_proxy: var("TUNNEL_TRUST_PROXY").as_deref() == Some("1"),
        })
    }
}

#[derive(Clone)]
pub struct Broker(Arc<Inner>);

struct Inner {
    cfg: Config,
    http: reqwest::Client,
    sessions: Mutex<HashMap<String, Arc<Session>>>,
    last_ok: Mutex<Instant>,
    ips: Mutex<HashMap<String, Vec<Instant>>>,
    started: Instant,
}

struct Session {
    id: String,
    ends_by: u64,
    connect_by: u64,
    idle: Duration,
    started: Instant,
    legs: Mutex<Legs>,
    both: watch::Sender<bool>,
    kill: watch::Sender<Option<String>>,
    bytes_client: AtomicU64,
    bytes_agent: AtomicU64,
    last_activity: Mutex<Instant>,
    reported: AtomicBool,
}

#[derive(Default)]
struct Legs {
    client: Option<mpsc::Sender<Vec<u8>>>,
    agent: Option<mpsc::Sender<Vec<u8>>>,
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum Side {
    Client,
    Agent,
}

impl Side {
    fn name(self) -> &'static str {
        match self {
            Side::Client => "client",
            Side::Agent => "agent",
        }
    }
}

impl Session {
    fn kill(&self, reason: &str) {
        self.kill.send_if_modified(|r| {
            if r.is_none() {
                *r = Some(reason.to_string());
                true
            } else {
                false
            }
        });
    }
    fn spliced(&self) -> bool {
        *self.both.borrow()
    }
    fn touch(&self) {
        *self.last_activity.lock().expect("activity lock") = Instant::now();
    }
}

fn unix_now() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0)
}

fn valid_session_id(id: &str) -> bool {
    id.len() == 30
        && id.starts_with("ssh_")
        && id[4..]
            .bytes()
            .all(|b| b.is_ascii_lowercase() || (b'2'..=b'7').contains(&b))
}

impl Broker {
    pub fn new(cfg: Config) -> Broker {
        let http = reqwest::Client::builder()
            .timeout(Duration::from_secs(5))
            .user_agent(concat!("pigeonpost-tunnel/", env!("CARGO_PKG_VERSION")))
            .build()
            .expect("http client");
        Broker(Arc::new(Inner {
            cfg,
            http,
            sessions: Mutex::new(HashMap::new()),
            last_ok: Mutex::new(Instant::now()),
            ips: Mutex::new(HashMap::new()),
            started: Instant::now(),
        }))
    }

    pub fn router(&self) -> Router {
        Router::new()
            .route("/t/{sid}", get(upgrade))
            .route("/healthz", get(health))
            .fallback(|| async { StatusCode::NOT_FOUND })
            .with_state(self.clone())
    }

    /// Live sessions (for tests and the health endpoint).
    pub fn session_count(&self) -> usize {
        self.0.sessions.lock().expect("sessions lock").len()
    }

    /// Report live sessions to the postbox every `check_every`, end what it no longer authorizes
    /// and what has timed out here, and fail closed when the postbox is unreachable.
    pub fn start_heartbeat(&self) -> tokio::task::JoinHandle<()> {
        let b = self.clone();
        tokio::spawn(async move {
            let mut tick = tokio::time::interval(b.0.cfg.check_every);
            loop {
                tick.tick().await;
                b.heartbeat().await;
            }
        })
    }

    async fn heartbeat(&self) {
        let live: Vec<Arc<Session>> = self
            .0
            .sessions
            .lock()
            .expect("sessions lock")
            .values()
            .cloned()
            .collect();
        let now = unix_now();
        for s in &live {
            let idle = s.last_activity.lock().expect("activity lock").elapsed();
            if now >= s.ends_by {
                s.kill("max_duration");
            } else if !s.spliced() && now >= s.connect_by {
                s.kill("connect_timeout");
            } else if s.spliced() && idle >= s.idle {
                s.kill("idle");
            }
        }
        let body = json!({ "sessions": live.iter().map(|s| json!({
            "session_id": s.id, "bytes_client": s.bytes_client.load(Ordering::Relaxed),
            "bytes_agent": s.bytes_agent.load(Ordering::Relaxed), "spliced": s.spliced(),
        })).collect::<Vec<_>>() });
        match self.internal("/internal/v1/shell/check", &body).await {
            Ok(v) => {
                *self.0.last_ok.lock().expect("last_ok lock") = Instant::now();
                let end: Vec<(String, String)> = v["end"]
                    .as_array()
                    .map(|a| {
                        a.iter()
                            .filter_map(|e| {
                                Some((
                                    e["session_id"].as_str()?.to_string(),
                                    e["reason"].as_str().unwrap_or("revoked").to_string(),
                                ))
                            })
                            .collect()
                    })
                    .unwrap_or_default();
                for (sid, reason) in end {
                    let s = self
                        .0
                        .sessions
                        .lock()
                        .expect("sessions lock")
                        .get(&sid)
                        .cloned();
                    if let Some(s) = s {
                        s.kill(&format!("revoked:{reason}"));
                    }
                }
            }
            Err(e) => {
                let since = self.0.last_ok.lock().expect("last_ok lock").elapsed();
                tracing::warn!(error = %e, since_secs = since.as_secs(), "postbox check failed");
                if since >= self.0.cfg.fail_closed_after {
                    for s in &live {
                        s.kill("postbox_unreachable");
                    }
                }
            }
        }
    }

    async fn internal(&self, path: &str, body: &Value) -> Result<Value, String> {
        let resp = self
            .0
            .http
            .post(format!("{}{path}", self.0.cfg.postbox))
            .bearer_auth(&self.0.cfg.token)
            .json(body)
            .send()
            .await
            .map_err(|e| e.without_url().to_string())?;
        let status = resp.status();
        let v: Value = resp.json().await.unwrap_or(Value::Null);
        if status.is_success() {
            Ok(v)
        } else {
            Err(format!(
                "{status} {}",
                v["error"].as_str().unwrap_or("error")
            ))
        }
    }

    fn admit_ip(&self, ip: &str) -> bool {
        let mut ips = self.0.ips.lock().expect("ips lock");
        if ips.len() > 10_000 {
            ips.retain(|_, v| {
                v.last()
                    .is_some_and(|t| t.elapsed() < Duration::from_secs(60))
            });
        }
        let hits = ips.entry(ip.to_string()).or_default();
        hits.retain(|t| t.elapsed() < Duration::from_secs(60));
        if hits.len() >= self.0.cfg.per_ip_per_minute {
            return false;
        }
        hits.push(Instant::now());
        true
    }

    fn finish(&self, s: &Arc<Session>, reason: &str) {
        s.kill(reason);
        let reason = s
            .kill
            .borrow()
            .clone()
            .unwrap_or_else(|| reason.to_string());
        {
            let mut sessions = self.0.sessions.lock().expect("sessions lock");
            if sessions.get(&s.id).is_some_and(|x| Arc::ptr_eq(x, s)) {
                sessions.remove(&s.id);
            }
        }
        if s.reported.swap(true, Ordering::SeqCst) {
            return;
        }
        let (bc, ba) = (
            s.bytes_client.load(Ordering::Relaxed),
            s.bytes_agent.load(Ordering::Relaxed),
        );
        tracing::info!(
            session = %s.id, reason = %reason, bytes_client = bc, bytes_agent = ba,
            secs = s.started.elapsed().as_secs(), "session ended"
        );
        let b = self.clone();
        let body = json!({ "session_id": s.id, "reason": reason.split(':').next().unwrap_or("closed"),
                           "bytes_client": bc, "bytes_agent": ba });
        tokio::spawn(async move {
            for attempt in 0..3u64 {
                if b.internal("/internal/v1/shell/closed", &body).await.is_ok() {
                    return;
                }
                tokio::time::sleep(Duration::from_secs(1 + attempt * 2)).await;
            }
        });
    }
}

async fn health(State(b): State<Broker>) -> Response {
    Json(json!({ "status": "ok", "sessions": b.session_count(),
                 "uptime_secs": b.0.started.elapsed().as_secs() }))
    .into_response()
}

/// Who connected: a TCP peer (taken at face value), or the reverse proxy over the Unix socket,
/// whose permissions (`0660`, the proxy's group) are what make its `X-Real-IP` trustworthy.
#[derive(Clone, Copy, Debug)]
pub enum Peer {
    Tcp(SocketAddr),
    Proxy,
}

impl
    axum::extract::connect_info::Connected<axum::serve::IncomingStream<'_, tokio::net::TcpListener>>
    for Peer
{
    fn connect_info(s: axum::serve::IncomingStream<'_, tokio::net::TcpListener>) -> Self {
        Peer::Tcp(*s.remote_addr())
    }
}

#[cfg(unix)]
impl
    axum::extract::connect_info::Connected<
        axum::serve::IncomingStream<'_, tokio::net::UnixListener>,
    > for Peer
{
    fn connect_info(_: axum::serve::IncomingStream<'_, tokio::net::UnixListener>) -> Self {
        Peer::Proxy
    }
}

pub fn client_ip(trust_proxy: bool, peer: Peer, headers: &HeaderMap) -> String {
    match peer {
        Peer::Tcp(addr) => addr.ip().to_string(),
        Peer::Proxy => headers
            .get("x-real-ip")
            .filter(|_| trust_proxy)
            .and_then(|v| v.to_str().ok())
            .and_then(|v| v.trim().parse::<std::net::IpAddr>().ok())
            .map(|ip| ip.to_string())
            // Without the header every proxied client shares one bucket: safe, never spoofable.
            .unwrap_or_else(|| "proxy".to_string()),
    }
}

async fn upgrade(
    State(b): State<Broker>,
    ConnectInfo(peer): ConnectInfo<Peer>,
    headers: HeaderMap,
    Path(sid): Path<String>,
    ws: WebSocketUpgrade,
) -> Response {
    if !valid_session_id(&sid) {
        return StatusCode::NOT_FOUND.into_response();
    }
    if !b.admit_ip(&client_ip(b.0.cfg.trust_proxy, peer, &headers)) {
        return StatusCode::TOO_MANY_REQUESTS.into_response();
    }
    if b.session_count() >= b.0.cfg.max_sessions {
        return StatusCode::SERVICE_UNAVAILABLE.into_response();
    }
    ws.max_message_size(MAX_MESSAGE)
        .max_frame_size(MAX_MESSAGE)
        .on_upgrade(move |socket| async move { leg(b, sid, socket).await })
}

async fn close(mut socket: WebSocket, code: u16, reason: &str) {
    let _ = socket
        .send(Message::Close(Some(CloseFrame {
            code,
            reason: reason.chars().take(100).collect::<String>().into(),
        })))
        .await;
}

/// One leg: authenticate with the leg token, wait for the other side, then splice.
async fn leg(b: Broker, sid: String, mut socket: WebSocket) {
    // 1. The first message is the leg token, and nothing else.
    let first = tokio::time::timeout(b.0.cfg.auth_timeout, socket.recv()).await;
    let token = match first {
        Ok(Some(Ok(Message::Text(t)))) => serde_json::from_str::<Value>(t.as_str())
            .ok()
            .and_then(|v| v["leg"].as_str().map(str::to_string)),
        _ => None,
    };
    let Some(token) = token.filter(|t| t.len() <= 128) else {
        return close(socket, 4400, "expected a leg token").await;
    };
    let admitted = b
        .internal(
            "/internal/v1/shell/legs",
            &json!({ "session_id": sid, "leg_token": token }),
        )
        .await;
    let v = match admitted {
        Ok(v) => v,
        Err(e) => {
            tracing::info!(session = %sid, error = %e, "leg refused");
            return close(socket, 4403, "leg refused").await;
        }
    };
    let side = match v["side"].as_str() {
        Some("client") => Side::Client,
        Some("agent") => Side::Agent,
        _ => return close(socket, 4403, "leg refused").await,
    };
    let ends_by = v["ends_by"].as_u64().unwrap_or(0);
    let connect_by = v["connect_by"].as_u64().unwrap_or(0);
    let idle = Duration::from_secs(v["idle_secs"].as_u64().unwrap_or(900)).min(b.0.cfg.idle);

    // 2. Join (or create) the session and take this side's slot.
    let (to_me_tx, mut to_me) = mpsc::channel::<Vec<u8>>(64);
    let session = {
        let mut sessions = b.0.sessions.lock().expect("sessions lock");
        let s = sessions
            .entry(sid.clone())
            .or_insert_with(|| {
                Arc::new(Session {
                    id: sid.clone(),
                    ends_by,
                    connect_by,
                    idle,
                    started: Instant::now(),
                    legs: Mutex::new(Legs::default()),
                    both: watch::channel(false).0,
                    kill: watch::channel(None).0,
                    bytes_client: AtomicU64::new(0),
                    bytes_agent: AtomicU64::new(0),
                    last_activity: Mutex::new(Instant::now()),
                    reported: AtomicBool::new(false),
                })
            })
            .clone();
        let mut legs = s.legs.lock().expect("legs lock");
        let slot = match side {
            Side::Client => &mut legs.client,
            Side::Agent => &mut legs.agent,
        };
        if slot.is_some() {
            // The postbox spends each token once, so this cannot be a legitimate second leg.
            None
        } else {
            *slot = Some(to_me_tx);
            if legs.client.is_some() && legs.agent.is_some() {
                s.both.send_replace(true);
            }
            drop(legs);
            Some(s)
        }
    };
    let Some(session) = session else {
        return close(socket, 4409, "side already connected").await;
    };
    tracing::info!(session = %sid, side = side.name(), "leg connected");

    // 3. Wait for the other side (or the end).
    let mut both = session.both.subscribe();
    let mut kill = session.kill.subscribe();
    let wait = Duration::from_secs(connect_by.saturating_sub(unix_now()).max(1));
    let met = tokio::time::timeout(wait, async {
        loop {
            if *both.borrow_and_update() {
                return true;
            }
            if kill.borrow_and_update().is_some() {
                return false;
            }
            tokio::select! {
                r = both.changed() => if r.is_err() { return false; },
                r = kill.changed() => if r.is_err() { return false; },
            }
        }
    })
    .await
    .unwrap_or(false);
    if !met {
        let reason = session
            .kill
            .borrow()
            .clone()
            .unwrap_or_else(|| "connect_timeout".to_string());
        b.finish(&session, &reason);
        return close(socket, 4408, &reason).await;
    }
    session.touch();
    let peer = {
        let legs = session.legs.lock().expect("legs lock");
        match side {
            Side::Client => legs.agent.clone(),
            Side::Agent => legs.client.clone(),
        }
    };
    let Some(peer) = peer else {
        b.finish(&session, "peer_missing");
        return close(socket, 4500, "peer missing").await;
    };

    // 4. Splice: what this side sends goes to the other side's queue; what is queued for this
    //    side is written to it. Bounded queues give backpressure end to end.
    let (mut sink, mut stream) = socket.split();
    let counter = match side {
        Side::Client => &session.bytes_client,
        Side::Agent => &session.bytes_agent,
    };
    let mut kill_w = session.kill.subscribe();
    let writer_session = session.clone();
    let writer = tokio::spawn(async move {
        let mut ping = tokio::time::interval(Duration::from_secs(20));
        ping.tick().await;
        loop {
            tokio::select! {
                m = to_me.recv() => match m {
                    Some(bytes) => {
                        if sink.send(Message::Binary(bytes.into())).await.is_err() {
                            break;
                        }
                    }
                    None => break,
                },
                _ = ping.tick() => {
                    if sink.send(Message::Ping(Vec::new().into())).await.is_err() {
                        break;
                    }
                }
                r = kill_w.changed() => {
                    let reason = if r.is_ok() { kill_w.borrow().clone() } else { None };
                    let _ = sink.send(Message::Close(Some(CloseFrame {
                        code: 4000,
                        reason: reason.unwrap_or_else(|| "closed".into()).chars().take(100).collect::<String>().into(),
                    }))).await;
                    break;
                }
            }
        }
        writer_session.kill("writer_closed");
    });
    let mut kill_r = session.kill.subscribe();
    let ended_by = loop {
        tokio::select! {
            m = stream.next() => match m {
                Some(Ok(Message::Binary(bytes))) => {
                    counter.fetch_add(bytes.len() as u64, Ordering::Relaxed);
                    session.touch();
                    if peer.send(bytes.to_vec()).await.is_err() {
                        break "peer_closed".to_string();
                    }
                }
                Some(Ok(Message::Ping(_) | Message::Pong(_))) => {}
                Some(Ok(Message::Text(_))) => break format!("{}_protocol_violation", side.name()),
                Some(Ok(Message::Close(_))) | None => break format!("{}_hangup", side.name()),
                Some(Err(_)) => break format!("{}_error", side.name()),
            },
            _ = kill_r.changed() => break kill_r.borrow().clone().unwrap_or_else(|| "closed".into()),
        }
    };
    b.finish(&session, &ended_by);
    let _ = writer.await;
}

#[cfg(test)]
mod client_ip_tests {
    use super::*;

    #[test]
    fn x_real_ip_is_trusted_only_from_the_proxy_socket() {
        let mut h = HeaderMap::new();
        h.insert("x-real-ip", "203.0.113.7".parse().unwrap());
        let loopback = Peer::Tcp("127.0.0.1:5555".parse().unwrap());
        assert_eq!(
            client_ip(true, loopback, &h),
            "127.0.0.1",
            "loopback TCP is not the proxy"
        );
        assert_eq!(client_ip(true, Peer::Proxy, &h), "203.0.113.7");
        assert_eq!(client_ip(false, Peer::Proxy, &h), "proxy");
        assert_eq!(client_ip(true, Peer::Proxy, &HeaderMap::new()), "proxy");
        let mut junk = HeaderMap::new();
        junk.insert("x-real-ip", "not-an-ip".parse().unwrap());
        assert_eq!(client_ip(true, Peer::Proxy, &junk), "proxy");
    }
}
