//! The model broker: Pigeonpost's budget and identity façade in front of mswarm.
//!
//! It is the only host service a sandbox can reach (nftables admits `broker_ip:port` from the
//! Nest bridge and nothing else). For every request it:
//!
//! 1. authenticates the Nest by its per-start broker token (fenced by generation, revoked at stop);
//! 2. bounds the request: fixed model, capped input and output, text-only messages, size limit;
//! 3. atomically **reserves** the worst case against the Nest's monthly input and output allowance
//!    before anything is sent upstream — an exhausted allowance is refused, never overdrawn;
//! 4. calls the upstream with the Pigeonpost tenant key, which never enters a sandbox;
//! 5. settles the actual usage once, releasing the rest. A sent request with an unknown result
//!    (timeout, missing usage) is charged to the platform's error budget, not the customer, and is
//!    never retried blindly.
//!
//! Prompt and answer text are never logged.

use crate::config::BrokerConfig;
use crate::db::{Db, Outcome, ReserveError};
use crate::util::{now, period_of, sha256_hex};
use axum::extract::State;
use axum::http::{HeaderMap, StatusCode};
use axum::routing::{get, post};
use axum::{Json, Router};
use serde_json::{json, Value};
use std::sync::Arc;
use std::time::{Duration, Instant};

#[derive(Clone)]
pub struct Broker {
    pub cfg: Arc<BrokerConfig>,
    pub db: Db,
    upstream_key: Arc<String>,
    http: reqwest::Client,
}

impl Broker {
    pub fn new(cfg: BrokerConfig, db: Db, upstream_key: String) -> Result<Self, String> {
        let http = reqwest::Client::builder()
            .timeout(Duration::from_secs(cfg.upstream_timeout_secs))
            .connect_timeout(Duration::from_secs(10))
            .user_agent(concat!(
                "pigeonpost-nests-broker/",
                env!("CARGO_PKG_VERSION")
            ))
            .build()
            .map_err(|e| e.to_string())?;
        Ok(Broker {
            cfg: Arc::new(cfg),
            db,
            upstream_key: Arc::new(upstream_key),
            http,
        })
    }
}

type Reply = (StatusCode, Json<Value>);

fn refuse(status: StatusCode, code: &str, detail: &str) -> Reply {
    (status, Json(json!({"error": code, "detail": detail})))
}

fn bearer(headers: &HeaderMap) -> Option<String> {
    headers
        .get(axum::http::header::AUTHORIZATION)?
        .to_str()
        .ok()?
        .strip_prefix("Bearer ")
        .filter(|t| t.starts_with("nbt_") && t.len() < 200)
        .map(str::to_string)
}

/// A conservative token estimate for text: about three bytes per token for English and Turkish,
/// plus per-message overhead. Over-estimating only reserves more; the settled charge is actual.
pub fn estimate_input_tokens(messages: &[(String, String)]) -> u64 {
    messages
        .iter()
        .map(|(_, c)| (c.len() as u64).div_ceil(3) + 8)
        .sum::<u64>()
        + 16
}

/// Reduce a client request to what the broker will forward: text messages with known roles.
/// Model, tools, provider and routing fields are the broker's decision, not the sandbox's.
/// Text messages as (role, content), and the requested output cap.
pub type Sanitized = (Vec<(String, String)>, Option<u64>);

pub fn sanitize(body: &Value) -> Result<Sanitized, &'static str> {
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
    let max_tokens = body["max_tokens"].as_u64();
    Ok((out, max_tokens))
}

pub fn router(b: Broker) -> Router {
    Router::new()
        .route("/v1/chat/completions", post(chat))
        .route("/v1/budget", get(budget))
        .layer(axum::extract::DefaultBodyLimit::max(256 * 1024))
        .with_state(b)
}

async fn budget(State(b): State<Broker>, headers: HeaderMap) -> Reply {
    let Some(token) = bearer(&headers) else {
        return refuse(
            StatusCode::UNAUTHORIZED,
            "unauthorized",
            "broker token required",
        );
    };
    let hash = sha256_hex(token.as_bytes());
    let db = b.db.clone();
    let found = tokio::task::spawn_blocking(move || db.nest_for_token(&hash))
        .await
        .ok()
        .and_then(|r| r.ok())
        .flatten();
    let Some(nest) = found else {
        return refuse(StatusCode::UNAUTHORIZED, "unauthorized", "unknown token");
    };
    let period = period_of(now());
    let u = b.db.usage(&nest.id, &period).unwrap_or_default();
    (
        StatusCode::OK,
        Json(json!({
            "period": period,
            "input_tokens": {"used": u.input_used, "limit": nest.input_limit},
            "output_tokens": {"used": u.output_used, "limit": nest.output_limit},
        })),
    )
}

async fn chat(State(b): State<Broker>, headers: HeaderMap, Json(body): Json<Value>) -> Reply {
    let Some(token) = bearer(&headers) else {
        return refuse(
            StatusCode::UNAUTHORIZED,
            "unauthorized",
            "broker token required",
        );
    };
    let (messages, asked) = match sanitize(&body) {
        Ok(v) => v,
        Err(e) => return refuse(StatusCode::BAD_REQUEST, "bad_request", e),
    };
    let input_est = estimate_input_tokens(&messages);
    if input_est > b.cfg.max_input_tokens {
        return refuse(
            StatusCode::PAYLOAD_TOO_LARGE,
            "too_large",
            "this request is larger than a Nest may send in one call",
        );
    }
    let max_out = asked
        .unwrap_or(b.cfg.max_output_tokens)
        .clamp(1, b.cfg.max_output_tokens);

    let hash = sha256_hex(token.as_bytes());
    let db = b.db.clone();
    let model = b.cfg.model.clone();
    let caps = b.cfg.caps();
    let reserved = tokio::task::spawn_blocking(move || {
        db.reserve_capped(&hash, input_est as i64, max_out as i64, &model, caps)
    })
    .await;
    let reservation = match reserved {
        Ok(Ok(r)) => r,
        Ok(Err(e)) => {
            return match e {
                ReserveError::Unauthorized => refuse(
                    StatusCode::UNAUTHORIZED,
                    "unauthorized",
                    "unknown or expired token",
                ),
                ReserveError::NotRunning => refuse(
                    StatusCode::FORBIDDEN,
                    "not_running",
                    "this Nest is paused or stopped",
                ),
                ReserveError::Busy => {
                    refuse(StatusCode::CONFLICT, "busy", "one model request at a time")
                }
                ReserveError::Exhausted(which) => refuse(
                    StatusCode::TOO_MANY_REQUESTS,
                    "allowance_exhausted",
                    &format!("the monthly {which} token allowance is used up"),
                ),
                ReserveError::NoModel => refuse(
                    StatusCode::PAYMENT_REQUIRED,
                    "no_model",
                    "this Nest has no AI connected",
                ),
                ReserveError::PlatformCap(which) => {
                    tracing::warn!(which, "platform monthly cap reached");
                    refuse(
                        StatusCode::TOO_MANY_REQUESTS,
                        "platform_limit",
                        "the service's monthly model budget is used up; try again later",
                    )
                }
                ReserveError::Store(e) => {
                    tracing::error!(error = %e, "reservation failed");
                    refuse(
                        StatusCode::SERVICE_UNAVAILABLE,
                        "unavailable",
                        "try again later",
                    )
                }
            };
        }
        Err(_) => {
            return refuse(
                StatusCode::SERVICE_UNAVAILABLE,
                "unavailable",
                "try again later",
            )
        }
    };

    let upstream_body = json!({
        "model": b.cfg.model,
        "messages": messages
            .iter()
            .map(|(role, content)| json!({"role": role, "content": content}))
            .collect::<Vec<_>>(),
        "max_tokens": max_out,
        "stream": false,
    });
    let started = Instant::now();
    let sent = b
        .http
        .post(&b.cfg.upstream_url)
        .header("x-api-key", b.upstream_key.as_str())
        .json(&upstream_body)
        .send()
        .await;

    let settle = |outcome: Outcome, i: i64, o: i64, id: Option<String>| {
        let db = b.db.clone();
        let rid = reservation.id.clone();
        async move {
            let r =
                tokio::task::spawn_blocking(move || db.settle(&rid, outcome, i, o, id.as_deref()))
                    .await;
            if !matches!(r, Ok(Ok(()))) {
                tracing::error!("settlement failed; the reservation expires as unknown");
            }
        }
    };

    let response = match sent {
        Ok(r) => r,
        Err(e) => {
            // Refused before the request left (connect failure): nothing was spent. Anything else
            // may have reached the supplier.
            let outcome = if e.is_connect() {
                Outcome::NotSent
            } else {
                Outcome::Unknown
            };
            settle(outcome, 0, 0, None).await;
            tracing::warn!(nest = %reservation.nest_id, ?outcome, "upstream unreachable");
            return refuse(
                StatusCode::BAD_GATEWAY,
                "upstream_error",
                "the model service did not answer",
            );
        }
    };
    let status = response.status();
    let value: Option<Value> = response.json().await.ok();
    let ms = started.elapsed().as_millis();
    let Some(value) = value.filter(|_| status.is_success()) else {
        let outcome = if status.is_client_error() {
            Outcome::NotSent
        } else {
            Outcome::Unknown
        };
        settle(outcome, 0, 0, None).await;
        tracing::warn!(nest = %reservation.nest_id, %status, ms, "upstream refused");
        return refuse(
            StatusCode::BAD_GATEWAY,
            "upstream_error",
            "the model service refused the request",
        );
    };
    let usage = (
        value["usage"]["prompt_tokens"].as_i64(),
        value["usage"]["completion_tokens"].as_i64(),
    );
    let upstream_id = value["id"].as_str().map(str::to_string);
    let (input_used, output_used) = match usage {
        (Some(i), Some(o)) => {
            settle(Outcome::Ok, i, o, upstream_id.clone()).await;
            (i, o)
        }
        _ => {
            settle(Outcome::Unknown, 0, 0, upstream_id.clone()).await;
            (0, 0)
        }
    };
    tracing::info!(
        nest = %reservation.nest_id,
        input_used,
        output_used,
        ms,
        "model request settled"
    );
    let choice = &value["choices"][0];
    (
        StatusCode::OK,
        Json(json!({
            "id": upstream_id,
            "model": b.cfg.model,
            "choices": [{
                "index": 0,
                "message": {"role": "assistant", "content": choice["message"]["content"].as_str().unwrap_or("")},
                "finish_reason": choice["finish_reason"],
            }],
            "usage": {"prompt_tokens": input_used, "completion_tokens": output_used},
        })),
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sanitize_drops_everything_but_text_messages() {
        let body = json!({
            "model": "openai/gpt-anything",
            "provider": {"order": ["x"]},
            "tools": [{"type": "function"}],
            "max_tokens": 50,
            "messages": [{"role": "user", "content": "hi"}]
        });
        let (m, max) = sanitize(&body).unwrap();
        assert_eq!(m, vec![("user".to_string(), "hi".to_string())]);
        assert_eq!(max, Some(50));
        assert!(sanitize(&json!({"messages": [{"role": "tool", "content": "x"}]})).is_err());
        assert!(sanitize(
            &json!({"messages": [{"role": "user", "content": [{"type": "image_url"}]}]})
        )
        .is_err());
        assert!(sanitize(&json!({"messages": []})).is_err());
    }

    #[test]
    fn estimates_are_conservative() {
        let m = vec![("user".to_string(), "x".repeat(3000))];
        assert!(estimate_input_tokens(&m) >= 1000);
    }

    #[test]
    fn only_broker_tokens_are_accepted_as_bearers() {
        let mut h = HeaderMap::new();
        h.insert("authorization", "Bearer svc_something".parse().unwrap());
        assert_eq!(bearer(&h), None);
        h.insert("authorization", "Bearer nbt_abc".parse().unwrap());
        assert_eq!(bearer(&h), Some("nbt_abc".into()));
    }
}
