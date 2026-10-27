//! Org webhooks (SDS §18.5; contract `docs/org-api.md` §6).
//!
//! * **Events** are written by database triggers (`store/org_api.rs`) in the same transaction as
//!   the change that caused them. The worker fans each event out to the org's subscribed webhooks
//!   through `webhook_outbox`, a durable table: a restart loses nothing, and a delivery in flight
//!   when the process stops is retried once its lease lapses.
//! * **Signing**: `Pigeonpost-Signature: t=<unix>,v1=<hex HMAC-SHA256(secret, "<t>.<raw body>")>`,
//!   with a second `v1=` for the previous secret during the 24 h after a rotation. Secrets are sealed
//!   with the vault at rest, shown once, and never logged.
//! * **Egress**: [`Egress`] decides where a delivery may connect. A URL must be `https` on 443 or
//!   8443, and every address its name resolves to — at registration and again at every delivery —
//!   must be a public unicast address. The connection is pinned to the address that was checked,
//!   follows no redirects, uses no proxy, and has connect/total timeouts and a response-size cap.
//! * **Retries**: exponential backoff (30 s, 2 min, 10 min, 30 min, 1 h, then every 2 h) until 24 h
//!   after the event, then `failed`. Every attempt is logged in `webhook_deliveries`.
//!
//! Payloads are ids and handles only — never message content (§18.5).

use crate::audit::ActorKind;
use crate::groups::{finish, parse, rate_limit, GErr, GResult, Outcome};
use crate::org::{self, Perm};
use crate::{capabilities, now_unix, AppState, ClientIp};
use axum::extract::rejection::JsonRejection;
use axum::extract::{Path, Query, State};
use axum::http::{header, HeaderMap, HeaderValue, StatusCode};
use axum::response::Response;
use axum::routing::{delete, get, post};
use axum::{Extension, Json, Router};
use hmac::{Hmac, Mac};
use rusqlite::{params, Connection, OptionalExtension};
use serde::Deserialize;
use serde_json::{json, Value};
use sha2::Sha256;
use std::future::Future;
use std::net::{IpAddr, Ipv4Addr, Ipv6Addr, SocketAddr};
use std::pin::Pin;
use std::sync::Arc;
use std::time::Duration;

/// Events a webhook may subscribe to. The last two are reserved for later phases.
pub(crate) const EVENTS: &[&str] = &[
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
    "meeting.brief_ready",
    "security.ssh_session",
];

pub(crate) const MAX_WEBHOOKS_PER_ORG: i64 = 10;
/// Retries stop this long after the event.
pub(crate) const RETRY_WINDOW_SECS: i64 = 86_400;
/// The previous secret keeps signing this long after a rotation.
pub(crate) const ROTATION_OVERLAP_SECS: i64 = 86_400;
/// A claimed delivery is retried after this if its worker never reports back.
const LEASE_SECS: i64 = 60;
/// Backoff after the n-th failed attempt; after the table, every two hours.
const BACKOFF: &[i64] = &[30, 120, 600, 1_800, 3_600];
const BACKOFF_TAIL: i64 = 7_200;
/// Undelivered events kept per webhook; beyond this, new events for it are dropped (and logged).
const MAX_PENDING_PER_HOOK: i64 = 10_000;
/// Deliveries attempted per worker pass.
const BATCH: i64 = 20;
const CONNECT_TIMEOUT: Duration = Duration::from_secs(5);
const TOTAL_TIMEOUT: Duration = Duration::from_secs(10);
const DNS_TIMEOUT: Duration = Duration::from_secs(5);
/// Bytes of a receiver's response read before the connection is dropped.
const MAX_RESPONSE_READ: usize = 4_096;
const MAX_URL: usize = 2_048;
/// Ports a webhook may use.
const PORTS: &[u16] = &[443, 8_443];

// ---- egress: where a delivery may connect ------------------------------------------------------

type ResolveFuture = Pin<Box<dyn Future<Output = std::io::Result<Vec<SocketAddr>>> + Send>>;
/// `(host, port)` → the addresses it resolves to.
pub(crate) type Resolver = Arc<dyn Fn(String, u16) -> ResolveFuture + Send + Sync>;

/// The resolver and policy deliveries use. Production uses the system resolver and the strict
/// policy; tests substitute a resolver (to simulate DNS rebinding) and may allow plain HTTP to
/// loopback for a local receiver.
#[derive(Clone)]
pub(crate) struct Egress {
    resolver: Resolver,
    /// Test-only: `http://` and loopback receivers are allowed. Never set outside tests.
    insecure_loopback: bool,
}

impl Egress {
    pub(crate) fn system() -> Self {
        Egress {
            resolver: Arc::new(|host: String, port: u16| -> ResolveFuture {
                Box::pin(async move {
                    Ok(tokio::net::lookup_host((host.as_str(), port))
                        .await?
                        .collect())
                })
            }),
            insecure_loopback: false,
        }
    }

    #[cfg(test)]
    pub(crate) fn for_test(resolver: Resolver, insecure_loopback: bool) -> Self {
        Egress {
            resolver,
            insecure_loopback,
        }
    }

    fn allowed(&self, ip: IpAddr) -> bool {
        public_ip(ip) || (self.insecure_loopback && ip.is_loopback())
    }
}

/// Is `ip` a public unicast address a webhook may reach? Everything special-purpose is refused:
/// loopback, private and unique-local, link-local (including cloud metadata at 169.254.169.254),
/// CGNAT, multicast, broadcast, documentation, benchmarking, reserved, unspecified, and the IPv6
/// forms that embed an IPv4 address (mapped, compatible, NAT64, 6to4, Teredo).
pub(crate) fn public_ip(ip: IpAddr) -> bool {
    match ip {
        IpAddr::V4(v4) => public_v4(v4),
        IpAddr::V6(v6) => public_v6(v6),
    }
}

fn public_v4(ip: Ipv4Addr) -> bool {
    let o = ip.octets();
    let n = u32::from(ip);
    let within = |net: u32, bits: u32| (n >> (32 - bits)) == (net >> (32 - bits));
    !(o[0] == 0                                   // 0.0.0.0/8 "this network"
        || o[0] == 10                             // RFC 1918
        || within(0x6440_0000, 10)                // 100.64.0.0/10 CGNAT
        || o[0] == 127                            // loopback
        || within(0xA9FE_0000, 16)                // 169.254.0.0/16 link-local, metadata
        || within(0xAC10_0000, 12)                // 172.16.0.0/12 RFC 1918
        || within(0xC000_0000, 24)                // 192.0.0.0/24 IETF protocol assignments
        || within(0xC000_0200, 24)                // 192.0.2.0/24 TEST-NET-1
        || within(0xC058_6300, 24)                // 192.88.99.0/24 6to4 relay anycast
        || within(0xC0A8_0000, 16)                // 192.168.0.0/16 RFC 1918
        || within(0xC612_0000, 15)                // 198.18.0.0/15 benchmarking
        || within(0xC633_6400, 24)                // 198.51.100.0/24 TEST-NET-2
        || within(0xCB00_7100, 24)                // 203.0.113.0/24 TEST-NET-3
        || o[0] >= 224) // multicast 224/4, reserved 240/4, broadcast
}

fn public_v6(ip: Ipv6Addr) -> bool {
    let s = ip.segments();
    let n = u128::from(ip);
    let within = |net: u128, bits: u32| (n >> (128 - bits)) == (net >> (128 - bits));
    if ip.is_unspecified() || ip.is_loopback() {
        return false;
    }
    // Forms that carry an IPv4 address are judged by it — or refused outright where the embedded
    // address is not reliably the destination.
    if let Some(v4) = ip.to_ipv4_mapped() {
        return public_v4(v4);
    }
    if within(0, 96) {
        return false; // ::/96 IPv4-compatible (deprecated)
    }
    if within(0x0064_ff9b_0000_0000_0000_0000_0000_0000, 96) {
        return false; // 64:ff9b::/96 NAT64
    }
    if within(0x0064_ff9b_0001_0000_0000_0000_0000_0000, 48) {
        return false; // 64:ff9b:1::/48 local-use NAT64
    }
    !(within(0x0100_0000_0000_0000_0000_0000_0000_0000, 64)   // 100::/64 discard
        || within(0x2001_0000_0000_0000_0000_0000_0000_0000, 23) // 2001::/23 IETF (Teredo, ORCHID…)
        || within(0x2001_0db8_0000_0000_0000_0000_0000_0000, 32) // documentation
        || within(0x2002_0000_0000_0000_0000_0000_0000_0000, 16) // 6to4
        || within(0x3fff_0000_0000_0000_0000_0000_0000_0000, 20) // documentation (RFC 9637)
        || (s[0] & 0xfe00) == 0xfc00                              // fc00::/7 unique local
        || (s[0] & 0xffc0) == 0xfe80                              // fe80::/10 link-local
        || (s[0] & 0xffc0) == 0xfec0                              // fec0::/10 site-local (deprecated)
        || (s[0] & 0xff00) == 0xff00) // multicast
}

/// A webhook URL that passed the static checks.
#[derive(Debug, Clone)]
pub(crate) struct Target {
    pub url: reqwest::Url,
    pub host: String,
    pub port: u16,
    /// The host was an IP literal.
    pub literal: Option<IpAddr>,
}

/// Static checks on a webhook URL (no DNS). `Err` is a short reason for `invalid_webhook_url`.
pub(crate) fn parse_target(raw: &str, egress: &Egress) -> Result<Target, &'static str> {
    let raw = raw.trim();
    if raw.is_empty() || raw.len() > MAX_URL {
        return Err("the URL must be 1 to 2048 characters");
    }
    let url = reqwest::Url::parse(raw).map_err(|_| "not a valid URL")?;
    let insecure = egress.insecure_loopback && url.scheme() == "http";
    if url.scheme() != "https" && !insecure {
        return Err("webhooks must use https");
    }
    if !url.username().is_empty() || url.password().is_some() {
        return Err("the URL may not contain credentials");
    }
    if url.fragment().is_some() {
        return Err("the URL may not contain a fragment");
    }
    let port = url.port_or_known_default().ok_or("the URL has no port")?;
    if !PORTS.contains(&port) && !insecure {
        return Err("webhooks may use port 443 or 8443 only");
    }
    let raw_host = url.host_str().ok_or("the URL has no host")?;
    let literal: Option<IpAddr> = match raw_host.strip_prefix('[').and_then(|h| h.strip_suffix(']'))
    {
        Some(v6) => Some(IpAddr::V6(v6.parse().map_err(|_| "not a valid URL")?)),
        None => raw_host.parse::<Ipv4Addr>().ok().map(IpAddr::V4),
    };
    let host = match literal {
        Some(ip) => ip.to_string(),
        None => {
            let d = raw_host.trim_end_matches('.').to_ascii_lowercase();
            let internal = d == "localhost"
                || !d.contains('.')
                || [
                    ".localhost",
                    ".local",
                    ".internal",
                    ".home.arpa",
                    ".lan",
                    ".intranet",
                    ".corp",
                ]
                .iter()
                .any(|suffix| d.ends_with(suffix));
            if internal {
                return Err("the URL must name a public host");
            }
            d
        }
    };
    if let Some(ip) = literal {
        if !egress.allowed(ip) {
            return Err("the URL points at a private or reserved address");
        }
    }
    Ok(Target {
        url,
        host,
        port,
        literal,
    })
}

/// Resolve the target now and return the one address to connect to. Every resolved address must
/// be public: a name that resolves to both a public and a private address is refused, since a
/// client could otherwise be steered to the private one.
pub(crate) async fn resolve_checked(
    egress: &Egress,
    t: &Target,
) -> Result<SocketAddr, &'static str> {
    if let Some(ip) = t.literal {
        return if egress.allowed(ip) {
            Ok(SocketAddr::new(ip, t.port))
        } else {
            Err("blocked_destination")
        };
    }
    let lookup = (egress.resolver)(t.host.clone(), t.port);
    let addrs = match tokio::time::timeout(DNS_TIMEOUT, lookup).await {
        Ok(Ok(a)) if !a.is_empty() => a,
        _ => return Err("dns_failed"),
    };
    if addrs.iter().any(|a| !egress.allowed(a.ip())) {
        return Err("blocked_destination");
    }
    Ok(SocketAddr::new(addrs[0].ip(), t.port))
}

/// Validate a URL for registration: static checks, then a resolution that must succeed and be
/// entirely public.
pub(crate) async fn check_url(egress: &Egress, raw: &str) -> GResult<Target> {
    let t = parse_target(raw, egress).map_err(|m| GErr::bad("invalid_webhook_url", m))?;
    match resolve_checked(egress, &t).await {
        Ok(_) => Ok(t),
        Err("dns_failed") => Err(GErr::bad(
            "invalid_webhook_url",
            "the URL's host name does not resolve",
        )),
        Err(_) => Err(GErr::bad(
            "invalid_webhook_url",
            "the URL resolves to a private or reserved address",
        )),
    }
}

/// What one delivery attempt came to.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct Attempt {
    /// `delivered`, `blocked` or `failed` (the caller turns a non-final failure into `retrying`).
    pub outcome: &'static str,
    pub response_status: Option<u16>,
    pub error: Option<String>,
    pub duration_ms: i64,
}

/// POST one signed payload to a webhook, pinned to the address checked just now.
pub(crate) async fn send(
    egress: &Egress,
    raw_url: &str,
    headers: Vec<(&'static str, String)>,
    body: Vec<u8>,
) -> Attempt {
    let started = std::time::Instant::now();
    let elapsed = |s: std::time::Instant| s.elapsed().as_millis().min(i64::MAX as u128) as i64;
    let target = match parse_target(raw_url, egress) {
        Ok(t) => t,
        Err(m) => {
            return Attempt {
                outcome: "blocked",
                response_status: None,
                error: Some(m.to_string()),
                duration_ms: 0,
            }
        }
    };
    let addr = match resolve_checked(egress, &target).await {
        Ok(a) => a,
        Err(reason) => {
            return Attempt {
                outcome: if reason == "dns_failed" {
                    "failed"
                } else {
                    "blocked"
                },
                response_status: None,
                error: Some(reason.to_string()),
                duration_ms: elapsed(started),
            }
        }
    };
    let mut builder = reqwest::Client::builder()
        .redirect(reqwest::redirect::Policy::none())
        .no_proxy()
        .connect_timeout(CONNECT_TIMEOUT)
        .timeout(TOTAL_TIMEOUT)
        .user_agent("Pigeonpost-Webhooks/1")
        .https_only(!egress.insecure_loopback);
    if target.literal.is_none() {
        // Connect to exactly the address that was checked; TLS still verifies the URL's host name.
        builder = builder.resolve(&target.host, addr);
    }
    let client = match builder.build() {
        Ok(c) => c,
        Err(_) => {
            return Attempt {
                outcome: "failed",
                response_status: None,
                error: Some("client_error".into()),
                duration_ms: elapsed(started),
            }
        }
    };
    let mut req = client
        .post(target.url.clone())
        .header(header::CONTENT_TYPE, "application/json");
    for (k, v) in headers {
        req = req.header(k, v);
    }
    match req.body(body).send().await {
        Ok(mut resp) => {
            let status = resp.status();
            // Read a little of the answer so the receiver sees a clean exchange, then stop.
            let mut read = 0usize;
            while read < MAX_RESPONSE_READ {
                match resp.chunk().await {
                    Ok(Some(chunk)) => read += chunk.len(),
                    _ => break,
                }
            }
            let ok = status.is_success();
            Attempt {
                outcome: if ok { "delivered" } else { "failed" },
                response_status: Some(status.as_u16()),
                error: if ok {
                    None
                } else if status.is_redirection() {
                    Some("redirect_not_followed".into())
                } else {
                    Some(format!("http_{}", status.as_u16()))
                },
                duration_ms: elapsed(started),
            }
        }
        Err(e) => Attempt {
            outcome: "failed",
            response_status: None,
            error: Some(
                if e.is_timeout() {
                    "timeout"
                } else if e.is_connect() {
                    "connect_failed"
                } else {
                    "request_failed"
                }
                .into(),
            ),
            duration_ms: elapsed(started),
        },
    }
}

// ---- signing -----------------------------------------------------------------------------------

fn hmac_hex(secret: &str, t: i64, body: &[u8]) -> String {
    let mut mac =
        <Hmac<Sha256> as Mac>::new_from_slice(secret.as_bytes()).expect("HMAC accepts any key");
    mac.update(t.to_string().as_bytes());
    mac.update(b".");
    mac.update(body);
    crate::hex_str(&mac.finalize().into_bytes())
}

/// `t=<t>,v1=<sig>[,v1=<sig of the previous secret>]`.
pub(crate) fn signature_header(secrets: &[&str], t: i64, body: &[u8]) -> String {
    let mut out = format!("t={t}");
    for s in secrets {
        out.push_str(",v1=");
        out.push_str(&hmac_hex(s, t, body));
    }
    out
}

/// What a receiver does (the docs' snippets implement the same): parse `t` and every `v1`, check
/// the timestamp tolerance, compare in constant time.
#[cfg_attr(not(test), allow(dead_code))]
pub(crate) fn verify_signature(
    header: &str,
    secret: &str,
    body: &[u8],
    now: i64,
    tolerance: i64,
) -> bool {
    use subtle::ConstantTimeEq;
    let mut t = None;
    let mut sigs = Vec::new();
    for part in header.split(',') {
        match part.trim().split_once('=') {
            Some(("t", v)) => t = v.parse::<i64>().ok(),
            Some(("v1", v)) => sigs.push(v.to_string()),
            _ => {}
        }
    }
    let Some(t) = t else { return false };
    if (now - t).abs() > tolerance {
        return false;
    }
    let want = hmac_hex(secret, t, body);
    sigs.iter()
        .any(|s| s.len() == want.len() && bool::from(s.as_bytes().ct_eq(want.as_bytes())))
}

fn new_signing_secret() -> String {
    format!("whsec_{}", org::new_secret())
}

// ---- rows --------------------------------------------------------------------------------------

/// One webhook, as both APIs show it (timestamps rendered by the caller).
pub(crate) struct HookRow {
    pub id: String,
    pub url: String,
    pub events: Vec<String>,
    pub description: Option<String>,
    pub created_at: i64,
    pub created_by: Option<String>,
    pub rotated_at: Option<i64>,
    pub previous_expires_at: Option<i64>,
    pub last_delivery: Option<(i64, String, Option<i64>)>,
}

impl HookRow {
    /// `iso`: the org API's ISO 8601 timestamps; otherwise the dashboard's Unix seconds.
    pub(crate) fn json(&self, iso: bool) -> Value {
        let ts = |t: Option<i64>| {
            if iso {
                crate::org_api::iso(t)
            } else {
                json!(t)
            }
        };
        json!({
            "id": self.id,
            "url": self.url,
            "events": self.events,
            "description": self.description,
            "status": "active",
            "created_at": ts(Some(self.created_at)),
            "created_by": self.created_by,
            "rotated_at": ts(self.rotated_at),
            "previous_secret_expires_at": ts(self.previous_expires_at),
            "last_delivery": self.last_delivery.as_ref().map(|(at, status, code)| json!({
                "at": ts(Some(*at)), "status": status, "response_status": code,
            })),
        })
    }
}

const HOOK_COLS: &str = "w.id, w.url, w.events, w.description, w.created_at, w.created_by, w.rotated_at,
    w.previous_expires_at,
    (SELECT at FROM webhook_deliveries d WHERE d.webhook_id = w.id ORDER BY at DESC, rowid DESC LIMIT 1),
    (SELECT status FROM webhook_deliveries d WHERE d.webhook_id = w.id ORDER BY at DESC, rowid DESC LIMIT 1),
    (SELECT response_status FROM webhook_deliveries d WHERE d.webhook_id = w.id ORDER BY at DESC, rowid DESC LIMIT 1)";

fn map_hook(r: &rusqlite::Row<'_>) -> rusqlite::Result<HookRow> {
    let events: String = r.get(2)?;
    let at: Option<i64> = r.get(8)?;
    let status: Option<String> = r.get(9)?;
    Ok(HookRow {
        id: r.get(0)?,
        url: r.get(1)?,
        events: serde_json::from_str(&events).unwrap_or_default(),
        description: r.get(3)?,
        created_at: r.get(4)?,
        created_by: r.get(5)?,
        rotated_at: r.get(6)?,
        previous_expires_at: r.get(7)?,
        last_delivery: at
            .zip(status)
            .map(|(a, s)| (a, s, r.get(10).ok().flatten())),
    })
}

pub(crate) fn list_in(c: &Connection, org_id: &str) -> GResult<Vec<HookRow>> {
    let mut stmt = c.prepare(&format!(
        "SELECT {HOOK_COLS} FROM webhooks w WHERE w.org_id = ?1 AND w.deleted_at IS NULL
          ORDER BY w.created_at, w.id"
    ))?;
    let rows = stmt
        .query_map([org_id], map_hook)?
        .collect::<Result<Vec<_>, _>>()?;
    Ok(rows)
}

pub(crate) fn get_in(c: &Connection, org_id: &str, id: &str) -> GResult<HookRow> {
    c.query_row(
        &format!(
            "SELECT {HOOK_COLS} FROM webhooks w WHERE w.org_id = ?1 AND w.id = ?2 AND w.deleted_at IS NULL"
        ),
        params![org_id, id],
        map_hook,
    )
    .optional()?
    .ok_or_else(|| GErr::new(StatusCode::NOT_FOUND, "not_found", "no such webhook"))
}

/// The latest delivery attempts of one webhook, newest first.
pub(crate) fn deliveries_in(
    c: &Connection,
    hook: &str,
    limit: i64,
    iso: bool,
) -> GResult<Vec<Value>> {
    let mut stmt = c.prepare(
        "SELECT id, event_id, event_type, attempt, status, response_status, error, duration_ms, at
           FROM webhook_deliveries WHERE webhook_id = ?1 ORDER BY at DESC, rowid DESC LIMIT ?2",
    )?;
    let rows = stmt
        .query_map(params![hook, limit], |r| {
            let at: i64 = r.get(8)?;
            Ok(json!({
                "id": r.get::<_, String>(0)?,
                "event_id": r.get::<_, String>(1)?,
                "event_type": r.get::<_, String>(2)?,
                "attempt": r.get::<_, i64>(3)?,
                "status": r.get::<_, String>(4)?,
                "response_status": r.get::<_, Option<i64>>(5)?,
                "error": r.get::<_, Option<String>>(6)?,
                "duration_ms": r.get::<_, Option<i64>>(7)?,
                "at": if iso { crate::org_api::iso(Some(at)) } else { json!(at) },
            }))
        })?
        .collect::<Result<Vec<_>, _>>()?;
    Ok(rows)
}

// ---- administration (shared by the dashboard and the org API) ----------------------------------

/// A validated new webhook (URL already checked against the egress policy).
pub(crate) struct NewHook {
    pub url: String,
    pub events: Vec<String>,
    pub description: Option<String>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct HookReq {
    pub url: String,
    pub events: Vec<String>,
    #[serde(default)]
    pub description: Option<String>,
}

/// Check a creation request: events, description, then the URL (including DNS).
pub(crate) async fn validate(egress: &Egress, req: HookReq) -> GResult<NewHook> {
    let mut events: Vec<String> = Vec::new();
    for e in &req.events {
        let e = e.trim();
        if e != "*" && !EVENTS.contains(&e) {
            return Err(GErr::bad("bad_request", format!("unknown event '{e}'")));
        }
        if !events.iter().any(|x| x == e) {
            events.push(e.to_string());
        }
    }
    if events.is_empty() {
        return Err(GErr::bad(
            "bad_request",
            "subscribe to at least one event, or [\"*\"]",
        ));
    }
    if events.iter().any(|e| e == "*") {
        events = vec!["*".into()];
    }
    let description = match req.description.as_deref().map(str::trim) {
        None | Some("") => None,
        Some(d) if d.chars().count() <= 200 && !d.chars().any(char::is_control) => {
            Some(d.to_string())
        }
        Some(_) => {
            return Err(GErr::bad(
                "bad_request",
                "description is at most 200 printable characters",
            ))
        }
    };
    let target = check_url(egress, &req.url).await?;
    Ok(NewHook {
        url: target.url.to_string(),
        events,
        description,
    })
}

/// Who changed a webhook, for the audit record.
pub(crate) struct By<'a> {
    pub kind: ActorKind,
    /// The person's account, or the API key's id.
    pub actor: &'a str,
    /// What `created_by` shows: the member's handle or the key id.
    pub label: &'a str,
    pub ip: Option<&'a str>,
}

/// Create a webhook. Returns its id and the signing secret (shown once).
pub(crate) fn create_in(
    tx: &Connection,
    state_vault: &crate::vault::Vault,
    auditor: &crate::audit::Auditor,
    org_id: &str,
    h: &NewHook,
    by: &By<'_>,
    now: u64,
) -> GResult<(String, String)> {
    let count: i64 = tx.query_row(
        "SELECT COUNT(*) FROM webhooks WHERE org_id = ?1 AND deleted_at IS NULL",
        [org_id],
        |r| r.get(0),
    )?;
    if count >= MAX_WEBHOOKS_PER_ORG {
        return Err(GErr::conflict(
            "webhook_limit",
            format!("an organization holds at most {MAX_WEBHOOKS_PER_ORG} webhooks"),
        ));
    }
    let secret = new_signing_secret();
    let sealed = state_vault.seal_bytes(secret.as_bytes()).map_err(|_| {
        GErr::new(
            StatusCode::INTERNAL_SERVER_ERROR,
            "vault_error",
            "vault_error",
        )
    })?;
    let id = org::new_id("wh");
    tx.execute(
        "INSERT INTO webhooks (id, org_id, url, events, description, secret_sealed, created_by, created_at)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8)",
        params![id, org_id, h.url, json!(h.events).to_string(), h.description, sealed, by.label, now as i64],
    )?;
    org::audit_in(
        tx,
        auditor,
        org_id,
        by.kind,
        Some(by.actor),
        "webhook.create",
        Some(&id),
        json!({ "url_host": host_of(&h.url), "events": h.events }),
        by.ip,
        now,
    )?;
    Ok((id, secret))
}

fn host_of(url: &str) -> Option<String> {
    reqwest::Url::parse(url)
        .ok()
        .and_then(|u| u.host_str().map(str::to_string))
}

/// Delete a webhook; its undelivered events are cancelled.
pub(crate) fn delete_in(
    tx: &Connection,
    auditor: &crate::audit::Auditor,
    org_id: &str,
    id: &str,
    by: &By<'_>,
    now: u64,
) -> GResult<()> {
    let n = tx.execute(
        "UPDATE webhooks SET deleted_at = ?3 WHERE org_id = ?1 AND id = ?2 AND deleted_at IS NULL",
        params![org_id, id, now as i64],
    )?;
    if n == 0 {
        return Err(GErr::new(
            StatusCode::NOT_FOUND,
            "not_found",
            "no such webhook",
        ));
    }
    tx.execute(
        "UPDATE webhook_outbox SET state = 'cancelled', finished_at = ?2 WHERE webhook_id = ?1 AND state = 'pending'",
        params![id, now as i64],
    )?;
    org::audit_in(
        tx,
        auditor,
        org_id,
        by.kind,
        Some(by.actor),
        "webhook.delete",
        Some(id),
        json!({}),
        by.ip,
        now,
    )?;
    Ok(())
}

/// Rotate a webhook's secret. The previous one keeps signing for [`ROTATION_OVERLAP_SECS`].
pub(crate) fn rotate_in(
    tx: &Connection,
    vault: &crate::vault::Vault,
    auditor: &crate::audit::Auditor,
    org_id: &str,
    id: &str,
    by: &By<'_>,
    now: u64,
) -> GResult<(String, i64)> {
    let current: Vec<u8> = tx
        .query_row(
            "SELECT secret_sealed FROM webhooks WHERE org_id = ?1 AND id = ?2 AND deleted_at IS NULL",
            params![org_id, id],
            |r| r.get(0),
        )
        .optional()?
        .ok_or_else(|| GErr::new(StatusCode::NOT_FOUND, "not_found", "no such webhook"))?;
    let secret = new_signing_secret();
    let sealed = vault.seal_bytes(secret.as_bytes()).map_err(|_| {
        GErr::new(
            StatusCode::INTERNAL_SERVER_ERROR,
            "vault_error",
            "vault_error",
        )
    })?;
    let until = now as i64 + ROTATION_OVERLAP_SECS;
    tx.execute(
        "UPDATE webhooks SET secret_sealed = ?3, previous_secret_sealed = ?4, previous_expires_at = ?5,
                rotated_at = ?6 WHERE org_id = ?1 AND id = ?2",
        params![org_id, id, sealed, current, until, now as i64],
    )?;
    org::audit_in(
        tx,
        auditor,
        org_id,
        by.kind,
        Some(by.actor),
        "webhook.rotate",
        Some(id),
        json!({}),
        by.ip,
        now,
    )?;
    Ok((secret, until))
}

// ---- the worker --------------------------------------------------------------------------------

/// Run the delivery worker: every 5 s, fan out new events and deliver what is due.
pub(crate) fn start_worker(state: AppState) {
    tokio::spawn(async move {
        let mut pass: u64 = 0;
        loop {
            if let Err(e) = tick(&state, now_unix()).await {
                tracing::warn!(error = %e.message, "webhook worker pass failed");
            }
            pass += 1;
            if pass.is_multiple_of(720) {
                if let Err(e) = prune(&state, now_unix()).await {
                    tracing::warn!(error = %e.message, "webhook prune failed");
                }
            }
            tokio::time::sleep(Duration::from_secs(5)).await;
        }
    });
}

/// Copy unfanned events into the outbox, once per subscribed webhook that existed when the event
/// happened.
fn fan_out_in(tx: &Connection, now: u64) -> GResult<usize> {
    let events: Vec<(String, String, String, i64)> = {
        let mut stmt = tx.prepare(
            "SELECT id, org_id, type, created_at FROM webhook_events WHERE fanned_out = 0
              ORDER BY created_at, rowid LIMIT 500",
        )?;
        let rows = stmt.query_map([], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?)))?;
        rows.collect::<Result<_, _>>()?
    };
    let mut queued = 0;
    for (eid, org_id, kind, at) in &events {
        let hooks: Vec<(String, String)> = {
            let mut stmt = tx.prepare(
                "SELECT id, events FROM webhooks WHERE org_id = ?1 AND deleted_at IS NULL AND created_at <= ?2",
            )?;
            let rows = stmt.query_map(params![org_id, at], |r| Ok((r.get(0)?, r.get(1)?)))?;
            rows.collect::<Result<_, _>>()?
        };
        for (hid, subs) in hooks {
            let subs: Vec<String> = serde_json::from_str(&subs).unwrap_or_default();
            if !subs.iter().any(|s| s == "*" || s == kind) {
                continue;
            }
            let pending: i64 = tx.query_row(
                "SELECT COUNT(*) FROM webhook_outbox WHERE webhook_id = ?1 AND state = 'pending'",
                [&hid],
                |r| r.get(0),
            )?;
            if pending >= MAX_PENDING_PER_HOOK {
                tracing::warn!(webhook = %hid, "webhook backlog full; event dropped");
                continue;
            }
            queued += tx.execute(
                "INSERT OR IGNORE INTO webhook_outbox (webhook_id, event_id, state, next_attempt_at, created_at)
                 VALUES (?1, ?2, 'pending', ?3, ?3)",
                params![hid, eid, now as i64],
            )?;
        }
        tx.execute(
            "UPDATE webhook_events SET fanned_out = 1 WHERE id = ?1",
            [eid],
        )?;
    }
    Ok(queued)
}

/// A claimed delivery, with everything needed to send it.
struct Claimed {
    outbox_id: i64,
    webhook_id: String,
    url: String,
    secret_sealed: Vec<u8>,
    previous_sealed: Option<Vec<u8>>,
    previous_expires_at: Option<i64>,
    event_id: String,
    event_type: String,
    event_at: i64,
    org_id: String,
    data: String,
    attempt: i64,
    owner_account: String,
}

fn claim_in(tx: &Connection, now: u64) -> GResult<Vec<Claimed>> {
    let n = now as i64;
    // Deliveries whose webhook was deleted meanwhile are cancelled, not sent.
    tx.execute(
        "UPDATE webhook_outbox SET state = 'cancelled', finished_at = ?1 WHERE state = 'pending'
            AND webhook_id IN (SELECT id FROM webhooks WHERE deleted_at IS NOT NULL)",
        [n],
    )?;
    let rows: Vec<Claimed> = {
        let mut stmt = tx.prepare(
            "SELECT o.id, w.id, w.url, w.secret_sealed, w.previous_secret_sealed, w.previous_expires_at,
                    e.id, e.type, e.created_at, e.org_id, e.data, o.attempts + 1, g.owner_account
               FROM webhook_outbox o
               JOIN webhooks w ON w.id = o.webhook_id
               JOIN webhook_events e ON e.id = o.event_id
               JOIN orgs g ON g.id = e.org_id
              WHERE o.state = 'pending' AND o.next_attempt_at <= ?1 AND o.lease_until <= ?1
              ORDER BY o.next_attempt_at, o.id LIMIT ?2",
        )?;
        let rows = stmt.query_map(params![n, BATCH], |r| {
            Ok(Claimed {
                outbox_id: r.get(0)?,
                webhook_id: r.get(1)?,
                url: r.get(2)?,
                secret_sealed: r.get(3)?,
                previous_sealed: r.get(4)?,
                previous_expires_at: r.get(5)?,
                event_id: r.get(6)?,
                event_type: r.get(7)?,
                event_at: r.get(8)?,
                org_id: r.get(9)?,
                data: r.get(10)?,
                attempt: r.get(11)?,
                owner_account: r.get(12)?,
            })
        })?;
        rows.collect::<Result<_, _>>()?
    };
    for c in &rows {
        tx.execute(
            "UPDATE webhook_outbox SET lease_until = ?2, attempts = attempts + 1 WHERE id = ?1",
            params![c.outbox_id, n + LEASE_SECS],
        )?;
    }
    Ok(rows)
}

/// The exact bytes delivered for an event (the same on every attempt).
pub(crate) fn payload(event_id: &str, kind: &str, at: i64, org_id: &str, data: &str) -> Vec<u8> {
    let data: Value = serde_json::from_str(data).unwrap_or(Value::Null);
    serde_json::to_vec(&json!({
        "id": event_id,
        "type": kind,
        "created_at": crate::org_api::iso(Some(at)),
        "org_id": org_id,
        "data": data,
    }))
    .unwrap_or_default()
}

fn backoff(attempt: i64) -> i64 {
    BACKOFF
        .get((attempt.max(1) - 1) as usize)
        .copied()
        .unwrap_or(BACKOFF_TAIL)
}

/// One worker pass: fan out, claim, deliver, record. Returns the number of attempts made.
pub(crate) async fn tick(state: &AppState, now: u64) -> GResult<usize> {
    let claimed = state
        .store
        .group_tx(move |tx| -> GResult<Vec<Claimed>> {
            fan_out_in(tx, now)?;
            claim_in(tx, now)
        })
        .await?;
    if claimed.is_empty() {
        return Ok(0);
    }
    // With the `org_api` flag off for an org, its deliveries wait rather than go out.
    let mut paused = Vec::new();
    let mut live = Vec::new();
    for c in claimed {
        if crate::filtering::feature_on(state, Some(&c.owner_account), capabilities::ORG_API).await
        {
            live.push(c);
        } else {
            paused.push(c.outbox_id);
        }
    }
    if !paused.is_empty() {
        state
            .store
            .group_tx(move |tx| -> GResult<()> {
                for id in &paused {
                    tx.execute(
                        "UPDATE webhook_outbox SET lease_until = 0, attempts = attempts - 1,
                                next_attempt_at = ?2 WHERE id = ?1",
                        params![id, now as i64 + 300],
                    )?;
                }
                Ok(())
            })
            .await?;
    }
    if live.is_empty() {
        return Ok(0);
    }
    let claimed = live;
    let mut set = tokio::task::JoinSet::new();
    for c in claimed {
        let vault = state.vault.clone();
        let egress = state.webhook_egress.clone();
        set.spawn(async move {
            let open = |sealed: &[u8]| -> Option<String> {
                vault
                    .open_bytes(sealed)
                    .ok()
                    .and_then(|b| String::from_utf8(b).ok())
            };
            let mut secrets = Vec::new();
            if let Some(s) = open(&c.secret_sealed) {
                secrets.push(s);
            }
            if let (Some(prev), Some(until)) = (&c.previous_sealed, c.previous_expires_at) {
                if until > now as i64 {
                    if let Some(s) = open(prev) {
                        secrets.push(s);
                    }
                }
            }
            let body = payload(&c.event_id, &c.event_type, c.event_at, &c.org_id, &c.data);
            let delivery_id = org::new_id("whd");
            let attempt = if secrets.is_empty() {
                Attempt {
                    outcome: "failed",
                    response_status: None,
                    error: Some("secret_unavailable".into()),
                    duration_ms: 0,
                }
            } else {
                let refs: Vec<&str> = secrets.iter().map(String::as_str).collect();
                let t = now_unix() as i64;
                let headers = vec![
                    ("Pigeonpost-Event-Id", c.event_id.clone()),
                    ("Pigeonpost-Event-Type", c.event_type.clone()),
                    ("Pigeonpost-Delivery-Id", delivery_id.clone()),
                    ("Pigeonpost-Signature", signature_header(&refs, t, &body)),
                ];
                send(&egress, &c.url, headers, body).await
            };
            (c, attempt, delivery_id)
        });
    }
    let mut results = Vec::new();
    while let Some(done) = set.join_next().await {
        if let Ok(r) = done {
            results.push(r);
        }
    }
    let count = results.len();
    let finished_at = now_unix();
    state
        .store
        .group_tx(move |tx| -> GResult<()> {
            for (c, a, delivery_id) in &results {
                let n = finished_at as i64;
                let expired = n - c.event_at >= RETRY_WINDOW_SECS;
                let (status, outbox_state, next) = match a.outcome {
                    "delivered" => ("delivered", "delivered", n),
                    other if expired => (if other == "blocked" { "blocked" } else { "failed" }, "failed", n),
                    "blocked" => ("blocked", "pending", n + backoff(c.attempt)),
                    _ => ("retrying", "pending", n + backoff(c.attempt)),
                };
                tx.execute(
                    "INSERT INTO webhook_deliveries (id, webhook_id, event_id, event_type, attempt, status,
                            response_status, error, duration_ms, at)
                     VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10)",
                    params![
                        delivery_id,
                        c.webhook_id,
                        c.event_id,
                        c.event_type,
                        c.attempt,
                        status,
                        a.response_status.map(i64::from),
                        a.error,
                        a.duration_ms,
                        n
                    ],
                )?;
                let finished = (outbox_state != "pending").then_some(n);
                tx.execute(
                    "UPDATE webhook_outbox SET state = CASE WHEN state = 'pending' THEN ?2 ELSE state END,
                            next_attempt_at = ?3, lease_until = 0, finished_at = COALESCE(?4, finished_at)
                      WHERE id = ?1",
                    params![c.outbox_id, outbox_state, next, finished],
                )?;
            }
            Ok(())
        })
        .await?;
    Ok(count)
}

/// Drop finished outbox rows after 7 days, delivery logs after 30, fanned-out events after 7, and
/// org API idempotency records after 24 h.
pub(crate) async fn prune(state: &AppState, now: u64) -> GResult<()> {
    let n = now as i64;
    state
        .store
        .group_tx(move |tx| -> GResult<()> {
            tx.execute(
                "DELETE FROM webhook_outbox WHERE state <> 'pending' AND COALESCE(finished_at, created_at) < ?1",
                [n - 7 * 86_400],
            )?;
            tx.execute("DELETE FROM webhook_deliveries WHERE at < ?1", [n - 30 * 86_400])?;
            tx.execute(
                "DELETE FROM webhook_events WHERE fanned_out = 1 AND created_at < ?1
                    AND id NOT IN (SELECT event_id FROM webhook_outbox)",
                [n - 7 * 86_400],
            )?;
            tx.execute(
                "DELETE FROM org_api_idempotency WHERE created_at < ?1",
                [n - crate::org_api::IDEMPOTENCY_SECS],
            )?;
            Ok(())
        })
        .await
}

// ---- dashboard routes (a person signed in) -----------------------------------------------------

pub(crate) fn router() -> Router<AppState> {
    Router::new()
        .route("/v1/orgs/{id}/webhooks", get(list).post(create))
        .route("/v1/orgs/{id}/webhooks/{webhook}", delete(remove))
        .route("/v1/orgs/{id}/webhooks/{webhook}/rotate", post(rotate))
        .route(
            "/v1/orgs/{id}/webhooks/{webhook}/deliveries",
            get(deliveries),
        )
}

type Body = Result<Json<Value>, JsonRejection>;

/// Owner/admin on the dashboard, with the `org_api` flag on.
async fn admin(state: &AppState, headers: &HeaderMap, id: &str) -> GResult<org::OrgCtx> {
    let ctx = org::org_ctx(state, headers, id).await?;
    let on =
        crate::filtering::feature_on(state, Some(&ctx.org.owner_account), capabilities::ORG_API)
            .await
            || crate::filtering::feature_on(state, Some(&ctx.account), capabilities::ORG_API).await;
    if !on {
        return Err(GErr::new(
            StatusCode::NOT_FOUND,
            "feature_disabled",
            "the enterprise API is not enabled for this organization",
        ));
    }
    ctx.require(state, Perm::Security)?;
    Ok(ctx)
}

fn member_label(ctx: &org::OrgCtx) -> String {
    ctx.member
        .handle
        .clone()
        .unwrap_or_else(|| ctx.member.id.clone())
}

fn no_store(mut r: Response) -> Response {
    r.headers_mut()
        .insert(header::CACHE_CONTROL, HeaderValue::from_static("no-store"));
    r
}

async fn list(
    State(state): State<AppState>,
    headers: HeaderMap,
    Path(id): Path<String>,
) -> Response {
    let result = async {
        admin(&state, &headers, &id).await?;
        let hooks = state
            .store
            .group_read(move |c: &Connection| -> GResult<Vec<Value>> {
                Ok(list_in(c, &id)?.iter().map(|h| h.json(false)).collect())
            })
            .await?;
        Ok(Outcome::ok(json!({ "webhooks": hooks })))
    }
    .await;
    finish(&state, result)
}

async fn create(
    State(state): State<AppState>,
    Extension(cip): Extension<ClientIp>,
    headers: HeaderMap,
    Path(id): Path<String>,
    body: Body,
) -> Response {
    let result = async {
        let ctx = admin(&state, &headers, &id).await?;
        rate_limit(&format!("webhookcreate:{id}"), 30, 3_600)?;
        let (req, _): (HookReq, String) = parse(body)?;
        let hook = validate(&state.webhook_egress, req).await?;
        let (vault, auditor) = (state.vault.clone(), state.auditor.clone());
        let account = ctx.account.clone();
        let label = member_label(&ctx);
        let ip = cip.as_str().to_string();
        let now = now_unix();
        state
            .store
            .group_tx(move |tx| -> GResult<Outcome> {
                let by = By {
                    kind: ActorKind::Human,
                    actor: &account,
                    label: &label,
                    ip: Some(&ip),
                };
                let (hid, secret) = create_in(tx, &vault, &auditor, &id, &hook, &by, now)?;
                Ok(Outcome::created(json!({
                    "webhook": get_in(tx, &id, &hid)?.json(false),
                    "secret": secret,
                })))
            })
            .await
    }
    .await;
    no_store(finish(&state, result))
}

async fn remove(
    State(state): State<AppState>,
    Extension(cip): Extension<ClientIp>,
    headers: HeaderMap,
    Path((id, hook)): Path<(String, String)>,
) -> Response {
    let result = async {
        let ctx = admin(&state, &headers, &id).await?;
        let auditor = state.auditor.clone();
        let account = ctx.account.clone();
        let label = member_label(&ctx);
        let ip = cip.as_str().to_string();
        let now = now_unix();
        state
            .store
            .group_tx(move |tx| -> GResult<Outcome> {
                let by = By {
                    kind: ActorKind::Human,
                    actor: &account,
                    label: &label,
                    ip: Some(&ip),
                };
                delete_in(tx, &auditor, &id, &hook, &by, now)?;
                Ok(Outcome::ok(json!({ "deleted": true })))
            })
            .await
    }
    .await;
    finish(&state, result)
}

async fn rotate(
    State(state): State<AppState>,
    Extension(cip): Extension<ClientIp>,
    headers: HeaderMap,
    Path((id, hook)): Path<(String, String)>,
) -> Response {
    let result = async {
        let ctx = admin(&state, &headers, &id).await?;
        rate_limit(&format!("webhookrotate:{hook}"), 20, 3_600)?;
        let (vault, auditor) = (state.vault.clone(), state.auditor.clone());
        let account = ctx.account.clone();
        let label = member_label(&ctx);
        let ip = cip.as_str().to_string();
        let now = now_unix();
        state
            .store
            .group_tx(move |tx| -> GResult<Outcome> {
                let by = By {
                    kind: ActorKind::Human,
                    actor: &account,
                    label: &label,
                    ip: Some(&ip),
                };
                let (secret, until) = rotate_in(tx, &vault, &auditor, &id, &hook, &by, now)?;
                Ok(Outcome::ok(json!({
                    "webhook": get_in(tx, &id, &hook)?.json(false),
                    "secret": secret,
                    "previous_secret_expires_at": until,
                })))
            })
            .await
    }
    .await;
    no_store(finish(&state, result))
}

#[derive(Deserialize, Default)]
struct LimitQuery {
    #[serde(default)]
    limit: Option<i64>,
}

async fn deliveries(
    State(state): State<AppState>,
    headers: HeaderMap,
    Path((id, hook)): Path<(String, String)>,
    Query(q): Query<LimitQuery>,
) -> Response {
    let result = async {
        admin(&state, &headers, &id).await?;
        let limit = q.limit.unwrap_or(50).clamp(1, 100);
        let list = state
            .store
            .group_read(move |c: &Connection| -> GResult<Vec<Value>> {
                get_in(c, &id, &hook)?;
                deliveries_in(c, &hook, limit, false)
            })
            .await?;
        Ok(Outcome::ok(json!({ "deliveries": list })))
    }
    .await;
    finish(&state, result)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ip(s: &str) -> IpAddr {
        s.parse().unwrap()
    }

    #[test]
    fn only_public_unicast_addresses_are_reachable() {
        for bad in [
            "0.0.0.0",
            "0.1.2.3",
            "10.0.0.1",
            "100.64.0.1",
            "100.127.255.254",
            "127.0.0.1",
            "127.255.0.1",
            "169.254.169.254",
            "169.254.0.1",
            "172.16.0.1",
            "172.31.255.255",
            "192.0.0.8",
            "192.0.2.1",
            "192.88.99.1",
            "192.168.1.1",
            "198.18.0.1",
            "198.19.255.1",
            "198.51.100.7",
            "203.0.113.7",
            "224.0.0.1",
            "239.255.255.250",
            "240.0.0.1",
            "255.255.255.255",
            "::",
            "::1",
            "::ffff:127.0.0.1",
            "::ffff:10.0.0.1",
            "::ffff:169.254.169.254",
            "::127.0.0.1",
            "64:ff9b::a00:1",
            "64:ff9b:1::1",
            "100::1",
            "2001::1",
            "2001:db8::1",
            "2002:a00:1::1",
            "3fff::1",
            "fc00::1",
            "fd12:3456::1",
            "fe80::1",
            "fec0::1",
            "ff02::1",
        ] {
            assert!(!public_ip(ip(bad)), "{bad} must be refused");
        }
        for good in [
            "1.1.1.1",
            "8.8.8.8",
            "93.184.215.14",
            "172.15.255.255",
            "172.32.0.1",
            "100.63.255.255",
            "100.128.0.1",
            "198.17.255.255",
            "198.20.0.1",
            "192.169.0.1",
            "2606:4700:4700::1111",
            "2a00:1450:4001::200e",
            "::ffff:8.8.8.8",
        ] {
            assert!(public_ip(ip(good)), "{good} must be allowed");
        }
    }

    #[test]
    fn webhook_urls_are_https_on_approved_ports_with_public_hosts() {
        let e = Egress::system();
        assert!(parse_target("https://hooks.example.com/pigeonpost", &e).is_ok());
        assert!(parse_target("https://hooks.example.com:8443/x?y=1", &e).is_ok());
        assert!(parse_target("https://8.8.8.8/x", &e).is_ok());
        for bad in [
            "http://hooks.example.com/x",
            "ftp://hooks.example.com/x",
            "https://hooks.example.com:8080/x",
            "https://user:pass@hooks.example.com/x",
            "https://hooks.example.com/x#frag",
            "https://localhost/x",
            "https://foo.localhost/x",
            "https://printer.local/x",
            "https://metadata.google.internal/x",
            "https://intranet/x",
            "https://127.0.0.1/x",
            "https://10.1.2.3/x",
            "https://169.254.169.254/latest/meta-data",
            "https://[::1]/x",
            "https://[fd00::1]/x",
            "https://[::ffff:7f00:1]/x",
            "https://2130706433/x",
            "https://0x7f.1/x",
            "https://017700000001/x",
            "not a url",
            "",
        ] {
            assert!(parse_target(bad, &e).is_err(), "{bad} must be refused");
        }
        let long = format!("https://hooks.example.com/{}", "a".repeat(2100));
        assert!(parse_target(&long, &e).is_err());
    }

    #[test]
    fn signatures_follow_the_documented_scheme() {
        let body = br#"{"id":"evt_1"}"#;
        let h = signature_header(&["whsec_new", "whsec_old"], 1_793_095_200, body);
        assert!(h.starts_with("t=1793095200,v1="));
        assert_eq!(h.matches("v1=").count(), 2);
        // HMAC-SHA256 over "<t>.<body>" with the secret's bytes, lower-case hex.
        let mut mac = <Hmac<Sha256> as Mac>::new_from_slice(b"whsec_new").unwrap();
        mac.update(b"1793095200.");
        mac.update(body);
        let expect = crate::hex_str(&mac.finalize().into_bytes());
        assert!(h.contains(&format!("v1={expect}")));
        let now = 1_793_095_200 + 60;
        assert!(verify_signature(&h, "whsec_new", body, now, 300));
        assert!(verify_signature(&h, "whsec_old", body, now, 300));
        assert!(!verify_signature(&h, "whsec_other", body, now, 300));
        assert!(!verify_signature(
            &h,
            "whsec_new",
            b"{\"id\":\"evt_2\"}",
            now,
            300
        ));
        assert!(!verify_signature(&h, "whsec_new", body, now + 600, 300));
        assert!(!verify_signature("v1=abc", "whsec_new", body, now, 300));
    }

    #[test]
    fn backoff_grows_then_settles() {
        assert_eq!(backoff(1), 30);
        assert_eq!(backoff(2), 120);
        assert_eq!(backoff(5), 3_600);
        assert_eq!(backoff(6), 7_200);
        assert_eq!(backoff(40), 7_200);
    }
}
