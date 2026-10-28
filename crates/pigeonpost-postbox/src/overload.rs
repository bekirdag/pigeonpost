//! Admission control for the public API (Phase 9b): load shedding, a request timeout, and body
//! size limits per route class.
//!
//! The Phase 9 load test drove the postbox past its knee and watched it keep accepting work:
//! abandoned requests ran to completion, each holding a mailbox listing, and the process grew to
//! 5.6 GB. nginx gave up on them after 60 s; the postbox did not. Now every request is put in one
//! of four classes, as the outermost thing that happens to it:
//!
//! | Class | Routes | Admission | Time limit |
//! |---|---|---|---|
//! | stream | `GET /v1/events`, `GET /v1/shell/agent/events` | `POSTBOX_MAX_STREAMS` open at once, else 503 | none; the stream ends after `POSTBOX_STREAM_MAX_SECS` (±10 %) and the client resumes (`Last-Event-ID`); 15 s keep-alives as before |
//! | long poll | `GET /v1/inbox?wait=N` | `POSTBOX_MAX_LONG_POLLS` parked at once, else 503 | `N` (≤ 60) + `POSTBOX_REQUEST_TIMEOUT_SECS` |
//! | ordinary | everything else | `POSTBOX_MAX_INFLIGHT` running at once; a request waits up to `POSTBOX_QUEUE_WAIT_MS` for a slot, then gets 503 | `POSTBOX_REQUEST_TIMEOUT_SECS` |
//! | exempt | `GET /health` | — | — |
//!
//! A shed or timed-out request is answered `503` with `Retry-After` and a JSON error
//! (`overloaded` / `request_timeout`), which every client already treats as "try again".
//! Dropping a handler at the time limit is the same cancellation a client disconnect has always
//! caused, so handlers are already written for it: each write is one transaction.
//!
//! Inbox listings, the expensive read, also share `POSTBOX_MAX_LISTINGS` slots
//! ([`listing_slot`]): with `POSTBOX_INBOX_MAX_ROWS` that bounds the memory listings can hold at
//! once, whatever the arrival rate.
//!
//! Body sizes: request bodies are limited by class — none (64 KiB) on GET/HEAD/DELETE/OPTIONS,
//! [`CONTROL_BODY_MAX`] on ordinary JSON routes, [`MESSAGE_BODY_MAX`] on the routes that carry a
//! message, and each upload route keeps its own explicit limit (`build_router`).

use axum::body::Body;
use axum::extract::Request;
use axum::http::{header, HeaderValue, Method, StatusCode};
use axum::middleware::Next;
use axum::response::{IntoResponse, Response};
use futures_util::StreamExt;
use serde_json::json;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, OnceLock};
use std::time::Duration;
use tokio::sync::{OwnedSemaphorePermit, Semaphore};

/// Ordinary JSON bodies: settings, contacts, groups, calendar, org administration.
pub(crate) const CONTROL_BODY_MAX: usize = 512 * 1024;
/// Bodies that carry a message (direct send, group post, MCP, group resync): axum's old default,
/// so no message that was accepted before is refused now.
pub(crate) const MESSAGE_BODY_MAX: usize = 2 * 1024 * 1024;
/// Requests that have no business carrying a body.
pub(crate) const BODYLESS_MAX: u64 = 64 * 1024;

/// Requests shed and requests cut off at their time limit, since this process started.
pub(crate) static SHED: AtomicU64 = AtomicU64::new(0);
pub(crate) static TIMED_OUT: AtomicU64 = AtomicU64::new(0);

/// The admission limits, from the environment once.
pub(crate) struct Limits {
    pub request_timeout: Duration,
    pub queue_wait: Duration,
    pub stream_max_age: Duration,
    inflight: Arc<Semaphore>,
    long_polls: Arc<Semaphore>,
    streams: Arc<Semaphore>,
    listings: Arc<Semaphore>,
}

fn env_num(name: &str, default: u64) -> u64 {
    std::env::var(name)
        .ok()
        .and_then(|v| v.trim().parse::<u64>().ok())
        .filter(|n| *n > 0)
        .unwrap_or(default)
}

impl Limits {
    #[allow(clippy::too_many_arguments)]
    pub(crate) fn new(
        request_timeout: Duration,
        queue_wait: Duration,
        stream_max_age: Duration,
        inflight: usize,
        long_polls: usize,
        streams: usize,
        listings: usize,
    ) -> Self {
        Limits {
            request_timeout,
            queue_wait,
            stream_max_age,
            inflight: Arc::new(Semaphore::new(inflight)),
            long_polls: Arc::new(Semaphore::new(long_polls)),
            streams: Arc::new(Semaphore::new(streams)),
            listings: Arc::new(Semaphore::new(listings)),
        }
    }

    fn from_env() -> Self {
        let limits = Limits::new(
            Duration::from_secs(env_num("POSTBOX_REQUEST_TIMEOUT_SECS", 30)),
            Duration::from_millis(env_num("POSTBOX_QUEUE_WAIT_MS", 250)),
            Duration::from_secs(env_num("POSTBOX_STREAM_MAX_SECS", 4 * 3600)),
            env_num("POSTBOX_MAX_INFLIGHT", 256) as usize,
            env_num("POSTBOX_MAX_LONG_POLLS", 20_000) as usize,
            env_num("POSTBOX_MAX_STREAMS", 20_000) as usize,
            env_num("POSTBOX_MAX_LISTINGS", 32) as usize,
        );
        tracing::info!(
            timeout_secs = limits.request_timeout.as_secs(),
            queue_wait_ms = limits.queue_wait.as_millis() as u64,
            stream_max_secs = limits.stream_max_age.as_secs(),
            inflight = limits.inflight.available_permits(),
            long_polls = limits.long_polls.available_permits(),
            streams = limits.streams.available_permits(),
            listings = limits.listings.available_permits(),
            "admission limits"
        );
        limits
    }
}

/// The process-wide limits.
pub(crate) fn limits() -> Arc<Limits> {
    static LIMITS: OnceLock<Arc<Limits>> = OnceLock::new();
    LIMITS.get_or_init(|| Arc::new(Limits::from_env())).clone()
}

/// Wait for one of the inbox listing slots. Held for the listing's duration.
pub(crate) async fn listing_slot() -> Option<OwnedSemaphorePermit> {
    limits().listings.clone().acquire_owned().await.ok()
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Class {
    Exempt,
    Stream,
    LongPoll(u64),
    Ordinary,
}

/// Which class a request is in, from its method, path and query alone.
pub(crate) fn classify(method: &Method, path: &str, query: Option<&str>) -> Class {
    if method == Method::GET {
        match path {
            "/health" => return Class::Exempt,
            "/v1/events" | "/v1/shell/agent/events" => return Class::Stream,
            "/v1/inbox" => {
                let wait = query
                    .unwrap_or_default()
                    .split('&')
                    .filter_map(|kv| kv.split_once('='))
                    .find(|(k, _)| *k == "wait")
                    .and_then(|(_, v)| v.parse::<u64>().ok())
                    .unwrap_or(0);
                if wait > 0 {
                    return Class::LongPoll(wait.min(crate::MAX_INBOX_WAIT_SECS));
                }
            }
            _ => {}
        }
    }
    Class::Ordinary
}

fn refusal(status: StatusCode, code: &'static str, message: &'static str, retry: u64) -> Response {
    let mut resp = (
        status,
        axum::Json(json!({ "error": code, "message": message })),
    )
        .into_response();
    if status == StatusCode::SERVICE_UNAVAILABLE {
        resp.headers_mut()
            .insert(header::RETRY_AFTER, HeaderValue::from(retry));
    }
    resp
}

fn overloaded() -> Response {
    SHED.fetch_add(1, Ordering::Relaxed);
    refusal(
        StatusCode::SERVICE_UNAVAILABLE,
        "overloaded",
        "the server is busy; retry shortly",
        1,
    )
}

fn timed_out() -> Response {
    TIMED_OUT.fetch_add(1, Ordering::Relaxed);
    refusal(
        StatusCode::SERVICE_UNAVAILABLE,
        "request_timeout",
        "the request took too long; retry shortly",
        2,
    )
}

/// The admission middleware. See the module documentation.
pub(crate) async fn admission(
    axum::extract::State(limits): axum::extract::State<Arc<Limits>>,
    req: Request,
    next: Next,
) -> Response {
    let class = classify(req.method(), req.uri().path(), req.uri().query());
    if class == Class::Exempt {
        return next.run(req).await;
    }
    if matches!(
        *req.method(),
        Method::GET | Method::HEAD | Method::DELETE | Method::OPTIONS
    ) {
        let declared = req
            .headers()
            .get(header::CONTENT_LENGTH)
            .and_then(|v| v.to_str().ok())
            .and_then(|v| v.parse::<u64>().ok());
        if declared.is_some_and(|n| n > BODYLESS_MAX) {
            return refusal(
                StatusCode::PAYLOAD_TOO_LARGE,
                "payload_too_large",
                "this request does not take a body that large",
                0,
            );
        }
    }
    match class {
        Class::Exempt => next.run(req).await,
        Class::Stream => {
            let Ok(permit) = limits.streams.clone().try_acquire_owned() else {
                return overloaded();
            };
            let resp = next.run(req).await;
            bounded_stream(resp, jittered(limits.stream_max_age), permit)
        }
        Class::LongPoll(wait) => {
            let Ok(_permit) = limits.long_polls.clone().try_acquire_owned() else {
                return overloaded();
            };
            let budget = Duration::from_secs(wait) + limits.request_timeout;
            match tokio::time::timeout(budget, next.run(req)).await {
                Ok(resp) => resp,
                Err(_) => timed_out(),
            }
        }
        Class::Ordinary => {
            let permit = match limits.inflight.clone().try_acquire_owned() {
                Ok(p) => p,
                Err(_) => match tokio::time::timeout(
                    limits.queue_wait,
                    limits.inflight.clone().acquire_owned(),
                )
                .await
                {
                    Ok(Ok(p)) => p,
                    _ => return overloaded(),
                },
            };
            let resp = match tokio::time::timeout(limits.request_timeout, next.run(req)).await {
                Ok(resp) => resp,
                Err(_) => timed_out(),
            };
            drop(permit);
            resp
        }
    }
}

/// `max_age` ±10 %, so streams opened together (after a deploy) do not all end together.
fn jittered(max_age: Duration) -> Duration {
    let spread = max_age.as_millis() as u64 / 10;
    if spread == 0 {
        return max_age;
    }
    let r = rand_core::RngCore::next_u64(&mut rand_core::OsRng) % (2 * spread + 1);
    Duration::from_millis(max_age.as_millis() as u64 - spread + r)
}

/// End a streaming response cleanly after `max_age`, holding the stream slot until then.
///
/// Ending is how a stream that a client stopped reading — a laptop asleep behind a NAT that never
/// sent a FIN — gives its slot back. Both streams resume: `/v1/events` from `Last-Event-ID`,
/// agentd's shell control stream by reconnecting.
fn bounded_stream(resp: Response, max_age: Duration, permit: OwnedSemaphorePermit) -> Response {
    if !resp.status().is_success() {
        return resp;
    }
    let (parts, body) = resp.into_parts();
    let stream = async_stream::stream! {
        let _permit = permit;
        let mut data = body.into_data_stream();
        let deadline = tokio::time::sleep(max_age);
        tokio::pin!(deadline);
        loop {
            tokio::select! {
                _ = &mut deadline => break,
                next = data.next() => match next {
                    Some(chunk) => yield chunk,
                    None => break,
                },
            }
        }
    };
    Response::from_parts(parts, Body::from_stream(stream))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn classes() {
        assert_eq!(classify(&Method::GET, "/health", None), Class::Exempt);
        assert_eq!(
            classify(&Method::GET, "/v1/events", Some("typed=1")),
            Class::Stream
        );
        assert_eq!(
            classify(&Method::GET, "/v1/shell/agent/events", None),
            Class::Stream
        );
        assert_eq!(
            classify(&Method::GET, "/v1/inbox", Some("include_read=true&wait=25")),
            Class::LongPoll(25)
        );
        assert_eq!(
            classify(&Method::GET, "/v1/inbox", Some("wait=600")),
            Class::LongPoll(crate::MAX_INBOX_WAIT_SECS)
        );
        assert_eq!(
            classify(&Method::GET, "/v1/inbox", Some("wait=0")),
            Class::Ordinary
        );
        assert_eq!(classify(&Method::GET, "/v1/inbox", None), Class::Ordinary);
        assert_eq!(classify(&Method::POST, "/v1/events", None), Class::Ordinary);
        assert_eq!(classify(&Method::POST, "/v1/send", None), Class::Ordinary);
    }

    #[test]
    fn jitter_stays_within_ten_percent() {
        let base = Duration::from_secs(3600);
        for _ in 0..1000 {
            let d = jittered(base);
            assert!(d >= Duration::from_secs(3240) && d <= Duration::from_secs(3960));
        }
    }
}
