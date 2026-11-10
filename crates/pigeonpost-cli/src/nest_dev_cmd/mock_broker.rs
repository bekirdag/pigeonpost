//! The mock model broker of `nest run`: the one network peer a Nest app has, emulated on
//! 127.0.0.1 with deterministic replies.
//!
//! It keeps the real broker's contract (`pigeonpost_nests::broker`): bearer `nbt_` token, text
//! messages with known roles, per-request input and output caps, a monthly allowance reserved
//! before anything is answered, and the same reply and error shapes. It also answers the two
//! routes the SDKs use for progress and external effects, which the platform does not offer yet
//! (`/v1/progress`, `/v1/effects`), so apps can be built against them now.

use axum::body::Bytes;
use axum::extract::{Path as UrlPath, State};
use axum::http::{HeaderMap, StatusCode};
use axum::routing::{get, post};
use axum::{Json, Router};
use serde_json::{json, Value};
use std::collections::{BTreeMap, BTreeSet};
use std::sync::{Arc, Mutex};

/// The production broker's per-request caps (`deploy/nests/host/nestd.toml.example`).
pub const MAX_INPUT_TOKENS: u64 = 16_384;
pub const MAX_OUTPUT_TOKENS: u64 = 1_024;
/// One marketplace worker's monthly allowance (`pigeonpost_nests::dispatch`).
pub const INPUT_ALLOWANCE: i64 = 200_000;
pub const OUTPUT_ALLOWANCE: i64 = 20_000;

pub struct Config {
    pub token: String,
    /// Replies in order; the last one repeats. Empty: a deterministic echo.
    pub replies: Vec<String>,
    /// Answer as a Nest with no AI connected.
    pub no_model: bool,
    /// What the manifest declares; effects are refused without it.
    pub external_writes: bool,
    pub approve_all: bool,
    pub approve: BTreeSet<String>,
    /// The installation's secret settings from `--config` (`writeOnly` properties), served at
    /// `GET /v1/secrets/{name}` as the platform's broker does.
    pub secrets: serde_json::Map<String, Value>,
}

#[derive(Debug, Clone)]
pub struct EffectRecord {
    pub job_id: String,
    pub kind: String,
    pub summary: String,
    pub body: Value,
    pub requires_approval: bool,
    pub approved: bool,
    /// Set once performed.
    pub reference: Option<String>,
}

pub struct Mock {
    pub cfg: Config,
    pub input_used: i64,
    pub output_used: i64,
    pub requests: u64,
    /// Everything the app did through the broker since the last drain, in order.
    pub events: Vec<Value>,
    pub effects: BTreeMap<String, EffectRecord>,
    performed: u64,
}

pub type Shared = Arc<Mutex<Mock>>;

impl Mock {
    pub fn new(cfg: Config) -> Shared {
        Arc::new(Mutex::new(Mock {
            cfg,
            input_used: 0,
            output_used: 0,
            requests: 0,
            events: Vec::new(),
            effects: BTreeMap::new(),
            performed: 0,
        }))
    }

    pub fn drain(&mut self) -> Vec<Value> {
        std::mem::take(&mut self.events)
    }

    /// Approve the effects of `job_id` that wait for approval and that the run's flags approve.
    /// Returns the keys approved.
    pub fn approve_waiting(&mut self, job_id: &str) -> Vec<String> {
        let mut out = Vec::new();
        for (key, e) in self.effects.iter_mut() {
            if e.job_id == job_id
                && e.requires_approval
                && !e.approved
                && (self.cfg.approve_all || self.cfg.approve.contains(key))
            {
                e.approved = true;
                out.push(key.clone());
            }
        }
        out
    }

    /// Keys of `job_id`'s effects that still wait for approval.
    pub fn waiting(&self, job_id: &str) -> Vec<String> {
        self.effects
            .iter()
            .filter(|(_, e)| e.job_id == job_id && e.reference.is_none() && !e.approved)
            .map(|(k, _)| k.clone())
            .collect()
    }
}

type Reply = (StatusCode, Json<Value>);

fn refuse(status: StatusCode, code: &str, detail: &str) -> Reply {
    (status, Json(json!({ "error": code, "detail": detail })))
}

fn authorized(m: &Mock, headers: &HeaderMap) -> bool {
    headers
        .get(axum::http::header::AUTHORIZATION)
        .and_then(|v| v.to_str().ok())
        .and_then(|v| v.strip_prefix("Bearer "))
        .is_some_and(|t| t == m.cfg.token)
}

/// The real broker's estimate: about three bytes per token plus per-message overhead.
pub fn estimate_input_tokens(messages: &[(String, String)]) -> u64 {
    messages
        .iter()
        .map(|(_, c)| (c.len() as u64).div_ceil(3) + 8)
        .sum::<u64>()
        + 16
}

/// Text messages as (role, content), and the requested output cap.
type Sanitized = (Vec<(String, String)>, Option<u64>);

fn sanitize(body: &Value) -> Result<Sanitized, &'static str> {
    let msgs = body["messages"]
        .as_array()
        .ok_or("messages must be an array")?;
    if msgs.is_empty() || msgs.len() > 64 {
        return Err("1 to 64 messages");
    }
    let mut out = Vec::with_capacity(msgs.len());
    for m in msgs {
        let role = m["role"].as_str().ok_or("message role")?;
        if !matches!(role, "system" | "user" | "assistant") {
            return Err("roles are system, user and assistant");
        }
        let content = m["content"]
            .as_str()
            .ok_or("message content must be text")?;
        out.push((role.to_string(), content.to_string()));
    }
    Ok((out, body["max_tokens"].as_u64()))
}

fn period() -> String {
    let secs = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0);
    // Civil date from days since 1970-01-01 (H. Hinnant's algorithm).
    let z = (secs / 86_400) as i64 + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z - era * 146_097;
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let month = if mp < 10 { mp + 3 } else { mp - 9 };
    let year = yoe + era * 400 + i64::from(month <= 2);
    format!("{year:04}-{month:02}")
}

fn reply_text(m: &Mock, messages: &[(String, String)]) -> String {
    if !m.cfg.replies.is_empty() {
        let i = (m.requests as usize).min(m.cfg.replies.len() - 1);
        return m.cfg.replies[i].clone();
    }
    let asked = messages
        .iter()
        .rev()
        .find(|(role, _)| role == "user")
        .and_then(|(_, c)| c.lines().rev().find(|l| !l.trim().is_empty()))
        .unwrap_or("")
        .trim();
    let asked: String = asked.chars().take(160).collect();
    format!("[mock model reply {}] {asked}", m.requests + 1)
}

fn parse(body: &Bytes) -> Option<Value> {
    serde_json::from_slice(body).ok()
}

async fn chat(State(s): State<Shared>, headers: HeaderMap, body: Bytes) -> Reply {
    let mut m = s.lock().unwrap_or_else(|e| e.into_inner());
    if !authorized(&m, &headers) {
        return refuse(
            StatusCode::UNAUTHORIZED,
            "unauthorized",
            "broker token required",
        );
    }
    let Some(body) = parse(&body) else {
        return refuse(
            StatusCode::BAD_REQUEST,
            "bad_request",
            "the body is not JSON",
        );
    };
    let (messages, asked) = match sanitize(&body) {
        Ok(v) => v,
        Err(e) => return refuse(StatusCode::BAD_REQUEST, "bad_request", e),
    };
    let input = estimate_input_tokens(&messages);
    if input > MAX_INPUT_TOKENS {
        return refuse(
            StatusCode::PAYLOAD_TOO_LARGE,
            "too_large",
            "this request is larger than a Nest may send in one call",
        );
    }
    if m.cfg.no_model {
        return refuse(
            StatusCode::PAYMENT_REQUIRED,
            "no_model",
            "this Nest has no AI connected",
        );
    }
    let max_out = asked
        .unwrap_or(MAX_OUTPUT_TOKENS)
        .clamp(1, MAX_OUTPUT_TOKENS);
    // Reserve the worst case before answering, as the real broker does.
    if m.input_used + input as i64 > INPUT_ALLOWANCE {
        return refuse(
            StatusCode::TOO_MANY_REQUESTS,
            "allowance_exhausted",
            "the monthly input token allowance is used up",
        );
    }
    if m.output_used + max_out as i64 > OUTPUT_ALLOWANCE {
        return refuse(
            StatusCode::TOO_MANY_REQUESTS,
            "allowance_exhausted",
            "the monthly output token allowance is used up",
        );
    }
    let mut text = reply_text(&m, &messages);
    let limit = (max_out * 3) as usize;
    if text.len() > limit {
        let mut end = limit;
        while !text.is_char_boundary(end) {
            end -= 1;
        }
        text.truncate(end);
    }
    let output = (text.len() as u64).div_ceil(3).clamp(1, max_out);
    m.requests += 1;
    m.input_used += input as i64;
    m.output_used += output as i64;
    let id = format!("mock-{}", m.requests);
    m.events.push(json!({
        "type": "model",
        "input_tokens": input,
        "output_tokens": output,
        "max_tokens": max_out,
    }));
    (
        StatusCode::OK,
        Json(json!({
            "id": id,
            "model": "mock",
            "choices": [{
                "index": 0,
                "message": {"role": "assistant", "content": text},
                "finish_reason": "stop",
            }],
            "usage": {"prompt_tokens": input, "completion_tokens": output},
        })),
    )
}

async fn budget(State(s): State<Shared>, headers: HeaderMap) -> Reply {
    let m = s.lock().unwrap_or_else(|e| e.into_inner());
    if !authorized(&m, &headers) {
        return refuse(
            StatusCode::UNAUTHORIZED,
            "unauthorized",
            "broker token required",
        );
    }
    (
        StatusCode::OK,
        Json(json!({
            "period": period(),
            "input_tokens": {"used": m.input_used, "limit": INPUT_ALLOWANCE},
            "output_tokens": {"used": m.output_used, "limit": OUTPUT_ALLOWANCE},
        })),
    )
}

async fn progress(State(s): State<Shared>, headers: HeaderMap, body: Bytes) -> Reply {
    let mut m = s.lock().unwrap_or_else(|e| e.into_inner());
    if !authorized(&m, &headers) {
        return refuse(
            StatusCode::UNAUTHORIZED,
            "unauthorized",
            "broker token required",
        );
    }
    let Some(body) = parse(&body) else {
        return refuse(
            StatusCode::BAD_REQUEST,
            "bad_request",
            "the body is not JSON",
        );
    };
    let fraction = body["fraction"].as_f64().unwrap_or(0.0).clamp(0.0, 1.0);
    let message: String = body["message"]
        .as_str()
        .unwrap_or("")
        .chars()
        .take(200)
        .collect();
    m.events.push(json!({
        "type": "progress",
        "job_id": body["job_id"],
        "fraction": fraction,
        "message": message,
    }));
    (StatusCode::OK, Json(json!({ "ok": true })))
}

fn key_ok(key: &str) -> bool {
    !key.is_empty()
        && key.len() <= 128
        && key
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'.' | b'_' | b':' | b'-'))
}

fn kind_ok(kind: &str) -> bool {
    !kind.is_empty()
        && kind.split('.').all(|w| {
            w.bytes().next().is_some_and(|b| b.is_ascii_lowercase())
                && w.bytes()
                    .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'_')
        })
}

/// One external effect. The first request under a key is the one recorded, approved and
/// performed; a later request with the same key gets that request's outcome (never a second
/// write), and one naming another kind is a conflict.
async fn effects(State(s): State<Shared>, headers: HeaderMap, body: Bytes) -> Reply {
    let mut m = s.lock().unwrap_or_else(|e| e.into_inner());
    if !authorized(&m, &headers) {
        return refuse(
            StatusCode::UNAUTHORIZED,
            "unauthorized",
            "broker token required",
        );
    }
    let Some(body) = parse(&body) else {
        return refuse(
            StatusCode::BAD_REQUEST,
            "bad_request",
            "the body is not JSON",
        );
    };
    let key = body["key"].as_str().unwrap_or("").to_string();
    let kind = body["kind"].as_str().unwrap_or("").to_string();
    let header_key = headers
        .get("idempotency-key")
        .and_then(|v| v.to_str().ok())
        .unwrap_or("");
    if !key_ok(&key) || header_key != key {
        return refuse(
            StatusCode::BAD_REQUEST,
            "bad_request",
            "an effect needs a key, sent as the Idempotency-Key header too",
        );
    }
    if !kind_ok(&kind) {
        return refuse(
            StatusCode::BAD_REQUEST,
            "bad_request",
            "kind: dotted lower-case words",
        );
    }
    if !m.cfg.external_writes {
        m.events.push(json!({
            "type": "effect", "job_id": body["job_id"], "key": key, "kind": kind,
            "state": "refused",
        }));
        return refuse(
            StatusCode::FORBIDDEN,
            "effect_refused",
            "the manifest declares external_writes = false",
        );
    }
    let job_id = body["job_id"].as_str().unwrap_or("").to_string();
    let existing = m.effects.get(&key).cloned();
    let mut record = match existing {
        Some(e) if e.kind != kind => {
            return refuse(
                StatusCode::CONFLICT,
                "idempotency_conflict",
                "this key was used for another kind of effect",
            )
        }
        Some(e) => e,
        None => EffectRecord {
            job_id: job_id.clone(),
            kind: kind.clone(),
            summary: body["summary"]
                .as_str()
                .unwrap_or("")
                .chars()
                .take(500)
                .collect(),
            body: body["body"].clone(),
            requires_approval: body["requires_approval"] == true,
            approved: false,
            reference: None,
        },
    };
    let deduplicated = record.reference.is_some();
    if record.reference.is_none() && record.requires_approval && !record.approved {
        m.events.push(json!({
            "type": "effect", "job_id": job_id, "key": key, "kind": kind,
            "summary": record.summary, "body": record.body, "state": "awaiting_approval",
        }));
        m.effects.insert(key.clone(), record);
        return (
            StatusCode::ACCEPTED,
            Json(json!({ "state": "awaiting_approval", "key": key })),
        );
    }
    if record.reference.is_none() {
        m.performed += 1;
        record.reference = Some(format!("eff_local_{}", m.performed));
    }
    let outcome = json!({ "reference": record.reference, "body": record.body });
    m.events.push(json!({
        "type": "effect", "job_id": job_id, "key": key, "kind": kind,
        "summary": record.summary, "state": "done", "deduplicated": deduplicated,
        "reference": record.reference,
    }));
    m.effects.insert(key, record);
    (
        StatusCode::OK,
        Json(json!({ "state": "done", "deduplicated": deduplicated, "outcome": outcome })),
    )
}

async fn secret(
    State(s): State<Shared>,
    headers: HeaderMap,
    UrlPath(name): UrlPath<String>,
) -> Reply {
    let mut m = s.lock().unwrap_or_else(|e| e.into_inner());
    if !authorized(&m, &headers) {
        return refuse(
            StatusCode::UNAUTHORIZED,
            "unauthorized",
            "broker token required",
        );
    }
    // The value is never printed; the report says only that it was asked for.
    m.events.push(json!({ "type": "secret", "name": name }));
    match m.cfg.secrets.get(&name).and_then(Value::as_str) {
        Some(v) => (StatusCode::OK, Json(json!({ "name": name, "value": v }))),
        None => refuse(
            StatusCode::NOT_FOUND,
            "secret_not_set",
            "this installation has no such secret",
        ),
    }
}

pub fn router(s: Shared) -> Router {
    Router::new()
        .route("/v1/chat/completions", post(chat))
        .route("/v1/budget", get(budget))
        .route("/v1/secrets/{name}", get(secret))
        .route("/v1/progress", post(progress))
        .route("/v1/effects", post(effects))
        .layer(axum::extract::DefaultBodyLimit::max(256 * 1024))
        .with_state(s)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_period_is_a_year_and_month() {
        let p = period();
        assert_eq!(p.len(), 7);
        assert_eq!(&p[4..5], "-");
        assert!(p[..4].parse::<u32>().unwrap() >= 2026);
    }

    #[test]
    fn replies_are_deterministic() {
        let s = Mock::new(Config {
            token: "nbt_x".into(),
            replies: vec![],
            no_model: false,
            external_writes: false,
            approve_all: false,
            approve: BTreeSet::new(),
            secrets: Default::default(),
        });
        let m = s.lock().unwrap();
        let msgs = vec![(
            "user".to_string(),
            "Sections:\nabc\nQuestion: how long?".to_string(),
        )];
        assert_eq!(
            reply_text(&m, &msgs),
            "[mock model reply 1] Question: how long?"
        );
        assert_eq!(
            estimate_input_tokens(&msgs),
            (msgs[0].1.len() as u64).div_ceil(3) + 8 + 16
        );
    }
}
