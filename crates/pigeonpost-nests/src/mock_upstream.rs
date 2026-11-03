//! A local stand-in for mswarm's OpenAI-compatible endpoint, used until the Pigeonpost tenant key
//! exists and in tests. It answers deterministically, reports token usage the way mswarm does
//! (`usage.prompt_tokens` / `usage.completion_tokens`) and requires the configured key.
//!
//! It never stores or logs message content.

use axum::extract::State;
use axum::http::{HeaderMap, StatusCode};
use axum::routing::post;
use axum::{Json, Router};
use serde_json::{json, Value};
use std::sync::Arc;

#[derive(Clone)]
struct Mock {
    key_sha256: Arc<String>,
}

pub fn router(key: &str) -> Router {
    Router::new()
        .route("/v1/chat/completions", post(chat))
        .with_state(Mock {
            key_sha256: Arc::new(crate::util::sha256_hex(key.as_bytes())),
        })
}

async fn chat(
    State(m): State<Mock>,
    headers: HeaderMap,
    Json(body): Json<Value>,
) -> (StatusCode, Json<Value>) {
    let presented = headers
        .get("x-api-key")
        .and_then(|v| v.to_str().ok())
        .unwrap_or("");
    if !crate::util::ct_eq(
        &crate::util::sha256_hex(presented.as_bytes()),
        &m.key_sha256,
    ) {
        return (
            StatusCode::UNAUTHORIZED,
            Json(json!({"error": "unauthorized"})),
        );
    }
    let messages = body["messages"].as_array().cloned().unwrap_or_default();
    let prompt_chars: usize = messages
        .iter()
        .map(|m| m["content"].as_str().unwrap_or("").len())
        .sum();
    let last = messages
        .iter()
        .rev()
        .find(|m| m["role"] == "user")
        .and_then(|m| m["content"].as_str())
        .unwrap_or("");
    // The question the agent was asked is the last non-empty line agentd put in the prompt.
    let question: String = last
        .lines()
        .rev()
        .find(|l| !l.trim().is_empty())
        .unwrap_or("")
        .chars()
        .take(160)
        .collect();
    let answer = format!(
        "Hello from your Nest. This answer came through the Pigeonpost model broker (mock \
         upstream, model {}). You asked: {}",
        body["model"].as_str().unwrap_or("?"),
        question.trim()
    );
    let max = body["max_tokens"].as_u64().unwrap_or(256);
    let completion = ((answer.len() as u64).div_ceil(4)).min(max);
    (
        StatusCode::OK,
        Json(json!({
            "id": format!("mock-{}", crate::util::random_hex(8)),
            "object": "chat.completion",
            "model": body["model"],
            "choices": [{"index": 0, "message": {"role": "assistant", "content": answer}, "finish_reason": "stop"}],
            "usage": {
                "prompt_tokens": (prompt_chars as u64).div_ceil(4),
                "completion_tokens": completion,
                "total_tokens": (prompt_chars as u64).div_ceil(4) + completion
            }
        })),
    )
}

#[cfg(test)]
mod tests {
    use crate::broker::{self, Broker};
    use crate::db::Db;
    use crate::util::sha256_hex;
    use serde_json::json;

    async fn serve(router: axum::Router) -> String {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        tokio::spawn(async move { axum::serve(listener, router).await.unwrap() });
        format!("http://{addr}")
    }

    /// Broker → upstream round trip: reservation before the call, actual usage settled after,
    /// allowance enforced, the upstream key never visible to the caller.
    #[tokio::test]
    async fn broker_reserves_calls_and_settles() {
        let upstream = serve(super::router("upstream-key")).await;
        let dir = std::env::temp_dir();
        let mut cfg = crate::config::test_config(&dir).broker;
        cfg.upstream_url = format!("{upstream}/v1/chat/completions");
        let db = Db::open_memory().unwrap();
        let id = "nst_0000000000000a01";
        db.insert_nest(id, "acc", "/k/x", "t", 1, 2000, 120)
            .unwrap();
        let g = db.next_generation(id).unwrap();
        db.set_state(id, "running", "running", None, None).unwrap();
        db.set_broker_token(id, &sha256_hex(b"nbt_good"), g)
            .unwrap();
        let b = Broker::new(cfg, db.clone(), "upstream-key".into()).unwrap();
        let base = serve(broker::router(b)).await;
        let http = reqwest::Client::new();
        let ask = |token: &'static str, max: u64| {
            http.post(format!("{base}/v1/chat/completions"))
                .bearer_auth(token)
                .json(&json!({"model": "evil/model", "max_tokens": max,
                    "messages": [{"role": "user", "content": "What is a Nest?"}]}))
                .send()
        };
        let r = ask("nbt_good", 100).await.unwrap();
        assert_eq!(r.status(), 200);
        let v: serde_json::Value = r.json().await.unwrap();
        assert!(v["choices"][0]["message"]["content"]
            .as_str()
            .unwrap()
            .contains("What is a Nest?"));
        // The broker chose the model, not the caller.
        assert_eq!(v["model"], "deepseek/test");
        assert!(!v.to_string().contains("upstream-key"));
        let period = crate::util::period_of(crate::util::now());
        let u = db.usage(id, &period).unwrap();
        assert!(u.output_used > 0 && u.output_reserved == 0 && u.requests == 1);

        // 120 output tokens a month: a second worst-case reservation of 100 does not fit.
        let r = ask("nbt_good", 100).await.unwrap();
        assert_eq!(r.status(), 429);
        let r = ask("nbt_bad", 10).await.unwrap();
        assert_eq!(r.status(), 401);
        db.set_state(id, "paused", "paused", None, None).unwrap();
        let r = ask("nbt_good", 5).await.unwrap();
        assert_eq!(r.status(), 403);
    }

    #[tokio::test]
    async fn an_unreachable_upstream_costs_the_customer_nothing() {
        let dir = std::env::temp_dir();
        let mut cfg = crate::config::test_config(&dir).broker;
        cfg.upstream_url = "http://127.0.0.1:9/v1/chat/completions".into();
        let db = Db::open_memory().unwrap();
        let id = "nst_0000000000000a02";
        db.insert_nest(id, "acc", "/k/x", "t", 1, 2000, 120)
            .unwrap();
        let g = db.next_generation(id).unwrap();
        db.set_state(id, "running", "running", None, None).unwrap();
        db.set_broker_token(id, &sha256_hex(b"nbt_good"), g)
            .unwrap();
        let b = Broker::new(cfg, db.clone(), "k".into()).unwrap();
        let base = serve(broker::router(b)).await;
        let r = reqwest::Client::new()
            .post(format!("{base}/v1/chat/completions"))
            .bearer_auth("nbt_good")
            .json(&json!({"max_tokens": 10, "messages": [{"role": "user", "content": "hi"}]}))
            .send()
            .await
            .unwrap();
        assert_eq!(r.status(), 502);
        let u = db
            .usage(id, &crate::util::period_of(crate::util::now()))
            .unwrap();
        assert_eq!((u.input_used, u.output_used, u.input_reserved), (0, 0, 0));
    }
}
