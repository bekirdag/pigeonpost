//! Bounded loft HTTP surface.

use std::convert::Infallible;
use std::error::Error as StdError;
use std::future::Future;
use std::net::SocketAddr;
use std::pin::Pin;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::task::{Context, Poll};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use axum::body::{Body, Bytes, HttpBody};
use axum::extract::{ConnectInfo, FromRequest, FromRequestParts, Path, State};
use axum::http::request::Parts;
use axum::http::{header, HeaderMap, HeaderValue, Method, Request, StatusCode};
use axum::middleware::{self, Next};
use axum::response::{IntoResponse, Response};
use axum::routing::{get, post};
use axum::{Json, Router};
use hyper::body::Incoming;
use hyper_util::rt::{TokioExecutor, TokioIo, TokioTimer};
use hyper_util::server::conn::auto::Builder as ConnectionBuilder;
use hyper_util::service::TowerToHyperService;
use pigeonpost_core::{
    fetch_auth::validate_loft_origin, policy::RecipientPolicy, token::Presentation, Address,
    PROTOCOL_VERSION,
};
use tokio::sync::Semaphore;
use tower::ServiceExt;
use tower_http::timeout::TimeoutLayer;

use crate::attribution::{self, AttributionKeyResolver, UnconfiguredAttributionResolver};
use crate::config::LoftConfig;
use crate::error::{LoftError, Result};
use crate::limiter::{AdmissionController, AdmissionPermit};
use crate::store::{LoftStore, StorageStats, CONTROL_STORAGE_DIVISOR};
use crate::trace::{NoopTraceSink, TraceInput, TraceOperation, TraceSink, TRACE_BLOCKING_LANES};
use crate::wire::*;

const MAX_ADDRESS_SEGMENT_BYTES: usize = 256;
const POLICY_ADMISSION_RETRIES: usize = 3;

struct DeferredJsonBody(Bytes);

impl<S> FromRequest<S> for DeferredJsonBody
where
    S: Send + Sync,
{
    type Rejection = Response;

    async fn from_request(
        request: Request<Body>,
        state: &S,
    ) -> std::result::Result<Self, Self::Rejection> {
        if !json_content_type(request.headers()) {
            return Err((
                StatusCode::UNSUPPORTED_MEDIA_TYPE,
                Json(serde_json::json!({"error": "JSON content type required"})),
            )
                .into_response());
        }
        Bytes::from_request(request, state)
            .await
            .map(Self)
            .map_err(IntoResponse::into_response)
    }
}

struct PreparedPublish {
    wrap: pigeonpost_core::Wrap,
    token: Option<String>,
    encoded_size: usize,
    id: [u8; 32],
}

struct Peer {
    connected: Option<SocketAddr>,
    forwarded: Option<HeaderValue>,
    x_forwarded_for_present: bool,
    effective_source: Option<SocketAddr>,
}

#[derive(Clone, Copy)]
struct EffectiveSource(SocketAddr);

/// Keeps request admission attached to the actual response lifetime. Axum middleware futures end
/// when a `Response` is constructed, which can be long before Hyper finishes writing its body.
struct AdmittedBody {
    body: Pin<Box<Body>>,
    deadline: Pin<Box<tokio::time::Sleep>>,
    permit: Option<AdmissionPermit>,
    timed_out: bool,
}

impl AdmittedBody {
    fn new(body: Body, permit: AdmissionPermit, timeout: Duration) -> Self {
        Self {
            body: Box::pin(body),
            deadline: Box::pin(tokio::time::sleep(timeout)),
            permit: Some(permit),
            timed_out: false,
        }
    }
}

impl HttpBody for AdmittedBody {
    type Data = Bytes;
    type Error = Box<dyn StdError + Send + Sync>;

    fn poll_frame(
        mut self: Pin<&mut Self>,
        cx: &mut Context<'_>,
    ) -> Poll<Option<std::result::Result<http_body::Frame<Self::Data>, Self::Error>>> {
        if self.timed_out {
            return Poll::Ready(None);
        }
        if self.deadline.as_mut().poll(cx).is_ready() {
            self.permit.take();
            self.timed_out = true;
            return Poll::Ready(Some(Err(Box::new(std::io::Error::new(
                std::io::ErrorKind::TimedOut,
                "response body lifetime exceeded",
            )))));
        }

        match self.body.as_mut().poll_frame(cx) {
            Poll::Ready(Some(Ok(frame))) => Poll::Ready(Some(Ok(frame))),
            Poll::Ready(Some(Err(error))) => {
                self.permit.take();
                Poll::Ready(Some(Err(Box::new(error))))
            }
            Poll::Ready(None) => {
                self.permit.take();
                Poll::Ready(None)
            }
            Poll::Pending => Poll::Pending,
        }
    }

    fn is_end_stream(&self) -> bool {
        // Force one final poll so an empty inner body releases the permit at EOF. If Hyper elects
        // not to poll (for example for HEAD), dropping this wrapper releases it instead.
        false
    }

    fn size_hint(&self) -> http_body::SizeHint {
        self.body.size_hint()
    }
}

impl<S> FromRequestParts<S> for Peer
where
    S: Send + Sync,
{
    type Rejection = Infallible;

    async fn from_request_parts(
        parts: &mut Parts,
        _state: &S,
    ) -> std::result::Result<Self, Self::Rejection> {
        Ok(Self {
            connected: parts
                .extensions
                .get::<ConnectInfo<SocketAddr>>()
                .map(|connect| connect.0),
            forwarded: parts.headers.get("forwarded").cloned(),
            x_forwarded_for_present: parts.headers.contains_key("x-forwarded-for"),
            effective_source: parts
                .extensions
                .get::<EffectiveSource>()
                .map(|source| source.0),
        })
    }
}

pub struct Loft {
    pub config: LoftConfig,
    pub store: Arc<dyn LoftStore>,
    admission: AdmissionController,
    blocking: Arc<Semaphore>,
    trace_blocking: Arc<Semaphore>,
    pub(crate) attribution_resolver: Arc<dyn AttributionKeyResolver>,
    pub(crate) trace_sink: Arc<dyn TraceSink>,
    ready: AtomicBool,
    #[cfg(test)]
    fetch_encoding_thread: Arc<std::sync::Mutex<Option<std::thread::ThreadId>>>,
}

impl Loft {
    pub fn new(config: LoftConfig, store: Arc<dyn LoftStore>) -> Result<Self> {
        // Validate first. `Semaphore::new` may panic for values above Tokio's internal ceiling,
        // and the admission controller reserves attacker-keyed capacity from this configuration.
        validate_config(&config)?;
        Ok(Self {
            admission: AdmissionController::new(&config),
            blocking: Arc::new(Semaphore::new(config.max_blocking_operations)),
            // The supervised sink owns the one append-only writer. A small fixed number of callers
            // may wait for the same durable group commit; permits move into their blocking closures
            // and remain occupied after caller deadlines, bounding detached work under a stall.
            trace_blocking: Arc::new(Semaphore::new(TRACE_BLOCKING_LANES)),
            config,
            store,
            attribution_resolver: Arc::new(UnconfiguredAttributionResolver),
            trace_sink: Arc::new(NoopTraceSink),
            // Readiness is a lifecycle state, not a constructor-time approximation. Only the
            // supervised `serve` path may set it after validating every runtime dependency.
            ready: AtomicBool::new(false),
            #[cfg(test)]
            fetch_encoding_thread: Arc::new(std::sync::Mutex::new(None)),
        })
    }

    pub fn with_attribution_resolver(mut self, resolver: Arc<dyn AttributionKeyResolver>) -> Self {
        self.attribution_resolver = resolver;
        self
    }

    pub fn with_trace_sink(mut self, sink: Arc<dyn TraceSink>) -> Self {
        self.trace_sink = sink;
        self
    }

    pub(crate) fn router(self: Arc<Self>) -> Router {
        let state = Arc::clone(&self);
        let routes = Router::new()
            .route("/health", get(|| async { "ok" }))
            .route("/ready", get(readiness))
            .route("/v1/info", get(info))
            .route("/v1/publish", post(publish))
            .route("/v1/fetch", post(fetch))
            .route("/v1/policy", post(set_policy))
            .route("/v1/agent/{address}", get(get_agent).put(put_agent))
            .route(
                "/v1/rotation/{address}",
                get(get_rotation).put(put_rotation),
            );
        #[cfg(test)]
        let routes = routes.route(
            "/__test/slow-response",
            get(|| async { vec![0u8; crate::MAX_FETCH_RESPONSE_BYTES] }),
        );
        routes
            .layer(TimeoutLayer::with_status_code(
                StatusCode::REQUEST_TIMEOUT,
                Duration::from_secs(self.config.request_timeout_secs),
            ))
            .layer(axum::extract::DefaultBodyLimit::max(
                self.config.max_request_bytes,
            ))
            .layer(middleware::from_fn_with_state(state, admission_middleware))
            .with_state(self)
    }

    pub(crate) fn validate_startup(&self) -> Result<()> {
        validate_config(&self.config)?;
        self.store.health_check()?;
        let stats = self.store.stats()?;
        let control_total = stats
            .control_bytes_used
            .checked_add(stats.control_bytes_reserved)
            .ok_or(LoftError::Configuration(
                "loft control storage accounting overflow",
            ))?;
        if stats.bytes_used > self.config.capacity_bytes
            || control_total > self.config.capacity_bytes / CONTROL_STORAGE_DIVISOR
        {
            return Err(LoftError::Configuration(
                "persisted loft storage exceeds configured capacity",
            ));
        }
        if self.trace_sink.enabled() && !self.store.supports_durable_trace_admission() {
            return Err(LoftError::Configuration(
                "durable trace admission is unavailable",
            ));
        }
        self.trace_sink
            .readiness()
            .map_err(|_| LoftError::TraceUnavailable)?;
        if self.attribution_resolver.configured() {
            self.attribution_resolver
                .readiness(self.now_ms())
                .map_err(|_| LoftError::AttributionUnavailable)?;
        }
        Ok(())
    }

    pub(crate) fn set_ready(&self, ready: bool) {
        self.ready.store(ready, Ordering::Release);
    }

    fn now_secs(&self) -> u64 {
        SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map(|duration| duration.as_secs())
            .unwrap_or(0)
    }

    fn now_ms(&self) -> u64 {
        SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map(|duration| u64::try_from(duration.as_millis()).unwrap_or(u64::MAX))
            .unwrap_or(0)
    }

    async fn blocking<T, F>(&self, operation: F) -> Result<T>
    where
        T: Send + 'static,
        F: FnOnce(Arc<dyn LoftStore>) -> Result<T> + Send + 'static,
    {
        // Request deadlines do not include a user-space work queue. The permit belongs to the
        // blocking closure so cancellation cannot admit replacement work while that closure is
        // still using CPU, SQLite, or the filesystem.
        let permit = Arc::clone(&self.blocking)
            .try_acquire_owned()
            .map_err(|_| LoftError::Overloaded)?;
        let store = Arc::clone(&self.store);
        tokio::task::spawn_blocking(move || {
            let _permit = permit;
            operation(store)
        })
        .await
        .map_err(|_| LoftError::NotReady)?
    }

    async fn capture_trace(&self, peer: &Peer, operation: TraceOperation) -> Result<()> {
        if !self.trace_sink.enabled() {
            return Ok(());
        }
        let connected_source = resolve_peer_source(&self.config, peer)?;
        let input = TraceInput {
            timestamp_ms: self.now_ms(),
            connected_source,
            operation,
        };
        let store = Arc::clone(&self.store);
        let limit = self.config.global_requests_per_minute;
        self.run_trace_blocking(move |sink| {
            sink.readiness().map_err(|_| LoftError::TraceUnavailable)?;
            store.charge_trace_admission(input.timestamp_ms, limit)?;
            sink.capture(input).map_err(|_| LoftError::TraceUnavailable)
        })
        .await
    }

    async fn run_trace_blocking<T, F>(&self, operation: F) -> Result<T>
    where
        T: Send + 'static,
        F: FnOnce(Arc<dyn TraceSink>) -> Result<T> + Send + 'static,
    {
        // Never wait for lane capacity: the request deadline is evidence-admission time, not a
        // queueing budget. `try_acquire_owned` also prevents a timed-out blocking job from being
        // followed by an unbounded Tokio blocking queue.
        let permit = Arc::clone(&self.trace_blocking)
            .try_acquire_owned()
            .map_err(|_| LoftError::TraceUnavailable)?;
        let sink = Arc::clone(&self.trace_sink);
        tokio::time::timeout(
            Duration::from_millis(self.config.trace_timeout_ms),
            tokio::task::spawn_blocking(move || {
                let _permit = permit;
                operation(sink)
            }),
        )
        .await
        .map_err(|_| LoftError::TraceUnavailable)?
        .map_err(|_| LoftError::TraceUnavailable)?
    }
}

fn resolve_peer_source(config: &LoftConfig, peer: &Peer) -> Result<SocketAddr> {
    // Admission resolves once and places the result in request extensions. Trace capture consumes
    // that exact value, so source limiting and sealed custody cannot disagree about proxy chains.
    if let Some(source) = peer.effective_source {
        return Ok(source);
    }
    let connected = peer.connected.ok_or(LoftError::TraceUnavailable)?;
    if !config.trusted_proxies.contains(&connected.ip()) {
        return Ok(connected);
    }
    // Supporting two competing proxy-header conventions creates ambiguous chains. A trusted proxy
    // must emit one RFC 7239 chain with a port on every `for=` hop; XFF is deliberately not used.
    if peer.x_forwarded_for_present {
        return Err(LoftError::TraceUnavailable);
    }
    let forwarded = peer
        .forwarded
        .as_ref()
        .and_then(|value| value.to_str().ok())
        .filter(|value| value.len() <= 4_096)
        .ok_or(LoftError::TraceUnavailable)?;
    let elements = forwarded.split(',').collect::<Vec<_>>();
    if elements.is_empty() || elements.len() > 32 {
        return Err(LoftError::TraceUnavailable);
    }
    let mut current = connected;
    let mut consumed = false;
    for element in elements.into_iter().rev() {
        if !config.trusted_proxies.contains(&current.ip()) {
            break;
        }
        let mut forwarded_for = None;
        let mut parameter_count = 0usize;
        for parameter in element.split(';') {
            parameter_count += 1;
            if parameter_count > 32 {
                return Err(LoftError::TraceUnavailable);
            }
            let (name, value) = parameter
                .trim()
                .split_once('=')
                .ok_or(LoftError::TraceUnavailable)?;
            if name.trim().eq_ignore_ascii_case("for")
                && forwarded_for.replace(value.trim()).is_some()
            {
                return Err(LoftError::TraceUnavailable);
            }
        }
        current = parse_forwarded_socket(forwarded_for.ok_or(LoftError::TraceUnavailable)?)?;
        consumed = true;
    }
    if !consumed {
        return Err(LoftError::TraceUnavailable);
    }
    Ok(current)
}

fn parse_forwarded_socket(value: &str) -> Result<SocketAddr> {
    let value = if value.starts_with('"') && value.ends_with('"') && value.len() >= 2 {
        &value[1..value.len() - 1]
    } else if value.contains('"') {
        return Err(LoftError::TraceUnavailable);
    } else {
        value
    };
    if value.is_empty()
        || value
            .bytes()
            .any(|byte| byte.is_ascii_control() || byte == b'\\')
    {
        return Err(LoftError::TraceUnavailable);
    }
    let address = value
        .parse::<SocketAddr>()
        .map_err(|_| LoftError::TraceUnavailable)?;
    if address.port() == 0 || address.ip().is_unspecified() || address.ip().is_multicast() {
        return Err(LoftError::TraceUnavailable);
    }
    Ok(address)
}

fn validate_config(config: &LoftConfig) -> Result<()> {
    use crate::config::{
        MAX_BLOCKING_OPERATIONS, MAX_CAPACITY_BYTES, MAX_CONCURRENT_CONNECTIONS,
        MAX_CONCURRENT_REQUESTS, MAX_FETCH_LIMIT, MAX_FETCH_RESPONSE_BYTES,
        MAX_GLOBAL_REQUESTS_PER_MINUTE, MAX_HEADER_TIMEOUT_SECS, MAX_LIMITER_KEYS,
        MAX_RATE_BYTES_PER_MINUTE, MAX_RATE_LIMIT_PER_MINUTE, MAX_REQUEST_BYTES,
        MAX_REQUEST_TIMEOUT_SECS, MAX_RESPONSE_TIMEOUT_SECS, MAX_RETENTION_DAYS, MAX_SWEEP_BATCH,
        MAX_SWEEP_INTERVAL_SECS, MAX_TRACE_TIMEOUT_MS, MAX_TRUSTED_PROXIES,
    };
    use crate::MAX_EVENT_BYTES;

    validate_loft_origin(&config.origin)
        .map_err(|_| LoftError::Configuration("invalid canonical loft origin"))?;
    if config.capacity_bytes == 0 || config.capacity_bytes > MAX_CAPACITY_BYTES {
        return Err(LoftError::Configuration(
            "capacity is outside supported bounds",
        ));
    }
    if config.retention_days == 0 || config.retention_days > MAX_RETENTION_DAYS {
        return Err(LoftError::Configuration(
            "retention is outside supported bounds",
        ));
    }
    let minimum_request_bytes = config.max_event_bytes.checked_add(64 * 1024);
    if config.max_event_bytes == 0
        || config.max_event_bytes > MAX_EVENT_BYTES
        || config.max_request_bytes > MAX_REQUEST_BYTES
        || minimum_request_bytes.is_none_or(|minimum| config.max_request_bytes < minimum)
    {
        return Err(LoftError::Configuration(
            "invalid event/request body limits",
        ));
    }
    if config.max_fetch_limit == 0
        || config.default_fetch_limit == 0
        || config.default_fetch_limit > config.max_fetch_limit
        || config.max_fetch_limit > MAX_FETCH_LIMIT
        || config.max_fetch_response_bytes > MAX_FETCH_RESPONSE_BYTES
        || config.max_fetch_response_bytes < config.max_event_bytes.saturating_mul(2)
    {
        return Err(LoftError::Configuration("invalid fetch limits"));
    }
    if config.rate_limit_per_minute == 0
        || config.rate_limit_per_minute > MAX_RATE_LIMIT_PER_MINUTE
        || config.global_requests_per_minute == 0
        || config.global_requests_per_minute > MAX_GLOBAL_REQUESTS_PER_MINUTE
        || config.source_requests_per_minute == 0
        || config.source_requests_per_minute > MAX_RATE_LIMIT_PER_MINUTE
        || config.global_bytes_per_minute == 0
        || config.global_bytes_per_minute > MAX_RATE_BYTES_PER_MINUTE
        || config.source_bytes_per_minute == 0
        || config.source_bytes_per_minute > MAX_RATE_BYTES_PER_MINUTE
        || config.recipient_bytes_per_minute == 0
        || config.recipient_bytes_per_minute > MAX_RATE_BYTES_PER_MINUTE
    {
        return Err(LoftError::Configuration("invalid request-rate limits"));
    }
    if config.max_concurrent_connections == 0
        || config.max_concurrent_connections > MAX_CONCURRENT_CONNECTIONS
        || config.max_concurrent_requests == 0
        || config.max_concurrent_requests > MAX_CONCURRENT_REQUESTS
        || config.max_blocking_operations == 0
        || config.max_blocking_operations > MAX_BLOCKING_OPERATIONS
        || config.max_limiter_keys == 0
        || config.max_limiter_keys > MAX_LIMITER_KEYS
    {
        return Err(LoftError::Configuration(
            "admission limits are outside supported bounds",
        ));
    }
    if config.header_timeout_secs == 0
        || config.header_timeout_secs > MAX_HEADER_TIMEOUT_SECS
        || config.request_timeout_secs == 0
        || config.request_timeout_secs > MAX_REQUEST_TIMEOUT_SECS
        || config.response_timeout_secs == 0
        || config.response_timeout_secs > MAX_RESPONSE_TIMEOUT_SECS
        || config.trace_timeout_ms == 0
        || config.trace_timeout_ms > MAX_TRACE_TIMEOUT_MS
        || config.trace_timeout_ms > config.request_timeout_secs.saturating_mul(1_000)
        || config.sweep_batch == 0
        || config.sweep_batch > MAX_SWEEP_BATCH
        || config.sweep_interval_secs == 0
        || config.sweep_interval_secs > MAX_SWEEP_INTERVAL_SECS
    {
        return Err(LoftError::Configuration(
            "timeouts and retention work limits are outside supported bounds",
        ));
    }
    if config.trusted_proxies.len() > MAX_TRUSTED_PROXIES
        || config
            .trusted_proxies
            .iter()
            .any(|address| address.is_unspecified() || address.is_multicast())
    {
        return Err(LoftError::Configuration("invalid trusted proxy list"));
    }
    Ok(())
}

async fn admission_middleware(
    State(loft): State<Arc<Loft>>,
    request: Request<Body>,
    next: Next,
) -> Response {
    let permit = match loft.admission.try_enter() {
        Ok(permit) => permit,
        Err(error) => return error.into_response(),
    };

    let response = run_admitted_request(&loft, request, next).await;
    let timeout = Duration::from_secs(loft.config.response_timeout_secs);
    response.map(|body| Body::new(AdmittedBody::new(body, permit, timeout)))
}

async fn run_admitted_request(
    loft: &Arc<Loft>,
    mut request: Request<Body>,
    next: Next,
) -> Response {
    if let Some(encoding) = request.headers().get(header::CONTENT_ENCODING) {
        if encoding.as_bytes() != b"identity" {
            return (
                StatusCode::UNSUPPORTED_MEDIA_TYPE,
                Json(serde_json::json!({"error": "content encoding is not supported"})),
            )
                .into_response();
        }
    }

    let declared = request
        .headers()
        .get(header::CONTENT_LENGTH)
        .and_then(|value| value.to_str().ok())
        .and_then(|value| value.parse::<u64>().ok());
    if declared.is_some_and(|bytes| bytes > loft.config.max_request_bytes as u64) {
        return (
            StatusCode::PAYLOAD_TOO_LARGE,
            Json(serde_json::json!({"error": "request too large"})),
        )
            .into_response();
    }
    let charged_bytes = match *request.method() {
        Method::GET | Method::HEAD => 0,
        _ => declared.unwrap_or(loft.config.max_request_bytes as u64),
    };
    // Local service managers and container health checks connect directly to the loopback listener,
    // which is also the reverse proxy's configured source. Probes carry no user operation and no
    // traceable source, so requiring a synthetic Forwarded header would make a correctly proxied
    // service permanently unready. They still consume the global concurrency/request budget.
    let is_probe = matches!(request.uri().path(), "/health" | "/ready");
    let source = if is_probe {
        None
    } else {
        let peer = Peer {
            connected: request
                .extensions()
                .get::<ConnectInfo<SocketAddr>>()
                .map(|connect| connect.0),
            forwarded: request.headers().get("forwarded").cloned(),
            x_forwarded_for_present: request.headers().contains_key("x-forwarded-for"),
            effective_source: None,
        };
        match peer.connected {
            Some(_) => match resolve_peer_source(&loft.config, &peer) {
                Ok(source) => {
                    request.extensions_mut().insert(EffectiveSource(source));
                    Some(source.ip())
                }
                Err(error) => return error.into_response(),
            },
            // In-process routers may not install ConnectInfo. Production serving always does;
            // traced operations still fail closed in their handler if no source is available.
            None => None,
        }
    };
    if let Err(error) = loft.admission.charge_shared(source, charged_bytes) {
        return error.into_response();
    }
    next.run(request).await
}

async fn readiness(State(loft): State<Arc<Loft>>) -> Result<&'static str> {
    if !loft.ready.load(Ordering::Acquire) {
        return Err(LoftError::NotReady);
    }
    loft.blocking(|store| store.health_check()).await?;
    loft.run_trace_blocking(|sink| sink.readiness().map_err(|_| LoftError::TraceUnavailable))
        .await
        .map_err(|_| LoftError::NotReady)?;
    if loft.attribution_resolver.configured() {
        loft.attribution_resolver
            .readiness(loft.now_ms())
            .map_err(|_| LoftError::NotReady)?;
    }
    Ok("ready")
}

async fn info(State(loft): State<Arc<Loft>>) -> Result<Json<InfoResponse>> {
    let stats = loft.blocking(|store| store.stats()).await?;
    let capacity = loft.config.capacity_bytes;
    Ok(Json(info_response(&loft, stats, capacity)))
}

fn info_response(loft: &Loft, stats: StorageStats, capacity: u64) -> InfoResponse {
    InfoResponse {
        software: "pigeonpost-loft".into(),
        version: env!("CARGO_PKG_VERSION").into(),
        protocol: PROTOCOL_VERSION.into(),
        pubkey: hex(&loft.config.pubkey),
        origin: loft.config.origin.clone(),
        capacity_bytes: capacity,
        used_bytes: stats.bytes_used,
        utilization: (stats.bytes_used as f64 / capacity as f64).min(1.0),
        retention_days: loft.config.retention_days,
        open: true,
        pow_floor: 0,
        max_event_bytes: loft.config.max_event_bytes,
        event_count: stats.event_count,
        accepting: stats.bytes_used < capacity && loft.ready.load(Ordering::Acquire),
    }
}

async fn publish(
    State(loft): State<Arc<Loft>>,
    peer: Peer,
    DeferredJsonBody(body): DeferredJsonBody,
) -> Result<Json<PublishResponse>> {
    let max_event_bytes = loft.config.max_event_bytes;
    // Body buffering is bounded by `DefaultBodyLimit`, but JSON decoding, re-encoding, signature
    // verification, and message-id hashing are proportional to the attacker-controlled body. Do
    // all of them only after non-queueing blocking admission.
    let prepared = loft
        .blocking(move |_| prepare_publish(&body, max_event_bytes).map(Arc::new))
        .await?;
    loft.admission.charge_recipient(
        &prepared.wrap.recipient,
        u64::try_from(prepared.encoded_size).map_err(|_| pigeonpost_core::Error::TooLarge)?,
    )?;

    let id = prepared.id;
    let now = loft.now_secs();
    let expires_at = loft
        .config
        .retention_days
        .checked_mul(86_400)
        .and_then(|seconds| now.checked_add(seconds))
        .ok_or(pigeonpost_core::Error::TooLarge)?;
    let capacity_bytes = loft.config.capacity_bytes;
    let mut traced = false;

    for _ in 0..POLICY_ADMISSION_RETRIES {
        let prepared_for_policy = Arc::clone(&prepared);
        let attribution_resolver = Arc::clone(&loft.attribution_resolver);
        let validation_time_ms = loft.now_ms();
        let expected_seq = loft
            .blocking(move |store| {
                validate_publish_policy(
                    store.as_ref(),
                    &prepared_for_policy,
                    &attribution_resolver,
                    validation_time_ms,
                )
            })
            .await?;

        if !traced {
            loft.capture_trace(
                &peer,
                TraceOperation::Publish {
                    event_id: id,
                    recipient: prepared.wrap.recipient,
                    size_bytes: u32::try_from(prepared.encoded_size)
                        .map_err(|_| pigeonpost_core::Error::TooLarge)?,
                },
            )
            .await?;
            traced = true;
        }

        let prepared_for_store = Arc::clone(&prepared);
        let stored = loft
            .blocking(move |store| {
                store.admit(
                    &prepared_for_store.wrap,
                    &id,
                    now,
                    expires_at,
                    capacity_bytes,
                    expected_seq,
                )
            })
            .await;
        match stored {
            Ok(stored) => {
                return Ok(Json(PublishResponse {
                    id: hex(&id),
                    stored,
                }));
            }
            Err(LoftError::PolicyChanged) => continue,
            Err(error) => return Err(error),
        }
    }
    Err(LoftError::PolicyChanged)
}

fn prepare_publish(body: &[u8], max_event_bytes: usize) -> Result<PreparedPublish> {
    let request: PublishRequest = serde_json::from_slice(body).map_err(|_| {
        LoftError::from(pigeonpost_core::Error::MalformedEnvelope(
            "invalid publish request",
        ))
    })?;
    let encoded_size = serde_json::to_vec(&request.wrap)?.len();
    if encoded_size > max_event_bytes {
        return Err(pigeonpost_core::Error::TooLarge.into());
    }

    // V3 public structure and complete outer signature are verified before a recipient bucket,
    // policy lookup, trace selector, or storage row is touched.
    request.wrap.verify_public()?;
    let id = request.wrap.id();
    Ok(PreparedPublish {
        wrap: request.wrap,
        token: request.token,
        encoded_size,
        id,
    })
}

fn validate_publish_policy(
    store: &dyn LoftStore,
    prepared: &PreparedPublish,
    attribution_resolver: &Arc<dyn AttributionKeyResolver>,
    validation_time_ms: u64,
) -> Result<Option<u64>> {
    let (policy, expected_seq) = match store.policy(&prepared.wrap.recipient)? {
        Some(policy) => {
            let sequence = policy.seq;
            (policy, Some(sequence))
        }
        None => (RecipientPolicy::permissive(prepared.wrap.recipient), None),
    };
    prepared.wrap.verify_work(policy.pow_min)?;
    if policy.token_required {
        let presented = prepared
            .token
            .as_deref()
            .and_then(parse_presentation)
            .ok_or(pigeonpost_core::Error::MalformedEnvelope("missing token"))?;
        if !policy.accepts_token(&presented) {
            return Err(pigeonpost_core::Error::MalformedEnvelope("token not live").into());
        }
    }
    // Deployed v1/v2 policies authenticated only a boolean. A required legacy policy cannot
    // safely choose jurisdiction or custodian, so admission remains closed until its owner
    // publishes a scoped v3 replacement.
    if policy.attribution_required && policy.attribution_requirement.is_none() {
        return Err(LoftError::AttributionRejected);
    }
    attribution::validate(
        &prepared.wrap,
        policy.attribution_requirement.as_ref(),
        attribution_resolver,
        validation_time_ms,
    )?;
    Ok(expected_seq)
}

async fn fetch(
    State(loft): State<Arc<Loft>>,
    peer: Peer,
    Json(req): Json<FetchRequest>,
) -> Result<Response> {
    let now_minute = loft.now_secs() / 60;
    let owner = req
        .auth
        .verify(&loft.config.pubkey, &loft.config.origin, now_minute)?;
    let requested_limit = req
        .limit
        .unwrap_or(loft.config.default_fetch_limit)
        .clamp(1, loft.config.max_fetch_limit);
    let byte_limited = (loft.config.max_fetch_response_bytes / loft.config.max_event_bytes)
        .saturating_sub(1)
        .max(1);
    let limit = requested_limit.min(byte_limited);
    loft.capture_trace(
        &peer,
        TraceOperation::Fetch {
            owner: owner.to_bytes(),
        },
    )
    .await?;
    let recipient = owner.to_bytes();
    let cursor = req.auth.cursor;
    let max_response_bytes = loft.config.max_fetch_response_bytes;
    #[cfg(test)]
    let fetch_encoding_thread = Arc::clone(&loft.fetch_encoding_thread);
    // SQLite access, page assembly, and hex/JSON encoding all consume the same bounded blocking
    // lane. An 8 MiB legal response must never monopolize a Tokio scheduler worker after the
    // database permit has already been released.
    let encoded = loft
        .blocking(move |store| {
            let events = store.fetch(&recipient, cursor, limit.saturating_add(1))?;
            let more = events.len() > limit;
            let events: Vec<_> = events.into_iter().take(limit).collect();
            let next_cursor = events.last().map(|event| event.cursor).unwrap_or(cursor);
            let response = FetchResponse {
                events: events.into_iter().map(|event| event.wrap).collect(),
                next_cursor,
                more,
            };
            #[cfg(test)]
            {
                *fetch_encoding_thread.lock().unwrap() = Some(std::thread::current().id());
            }
            let encoded = serde_json::to_vec(&response)?;
            if encoded.len() > max_response_bytes {
                return Err(pigeonpost_core::Error::TooLarge.into());
            }
            Ok(encoded)
        })
        .await?;
    loft.admission.charge_egress(
        peer.effective_source.map(|source| source.ip()),
        &recipient,
        u64::try_from(encoded.len()).map_err(|_| pigeonpost_core::Error::TooLarge)?,
    )?;
    Ok(([(header::CONTENT_TYPE, "application/json")], encoded).into_response())
}

async fn set_policy(State(loft): State<Arc<Loft>>, Json(req): Json<PolicyRequest>) -> Result<()> {
    // Cheap signature/version/size check before trace and database work; the store repeats it
    // against the authoritative current sequence in the write transaction.
    req.policy.verify(None)?;
    let policy = req.policy;
    let capacity_bytes = loft.config.capacity_bytes;
    loft.blocking(move |store| store.put_policy(&policy, capacity_bytes))
        .await
}

async fn get_agent(
    State(loft): State<Arc<Loft>>,
    Path(segment): Path<String>,
) -> Result<Json<AgentRecordRequest>> {
    let address = decode_address(&segment)?;
    let address = address.to_string();
    let record = loft
        .blocking(move |store| store.agent_record(&address))
        .await?
        .ok_or(LoftError::NotFound)?;
    Ok(Json(AgentRecordRequest { record }))
}

async fn put_agent(
    State(loft): State<Arc<Loft>>,
    peer: Peer,
    Path(segment): Path<String>,
    Json(req): Json<AgentRecordRequest>,
) -> Result<()> {
    let address = decode_address(&segment)?;
    req.record.verify(&address)?;
    loft.capture_trace(&peer, TraceOperation::PutAgent).await?;
    let address = address.to_string();
    let record = req.record;
    let capacity_bytes = loft.config.capacity_bytes;
    loft.blocking(move |store| store.put_agent_record(&address, &record, capacity_bytes))
        .await
}

async fn get_rotation(
    State(loft): State<Arc<Loft>>,
    Path(segment): Path<String>,
) -> Result<Json<RotationRecordRequest>> {
    let address = decode_address(&segment)?;
    let address = address.to_string();
    let record = loft
        .blocking(move |store| store.rotation_record(&address))
        .await?
        .ok_or(LoftError::NotFound)?;
    Ok(Json(RotationRecordRequest { record }))
}

async fn put_rotation(
    State(loft): State<Arc<Loft>>,
    peer: Peer,
    Path(segment): Path<String>,
    Json(req): Json<RotationRecordRequest>,
) -> Result<()> {
    let address = decode_address(&segment)?;
    req.record.verify_source_address(&address)?;

    // Rotation records mutate the same public routing metadata as agent records. Keep them in
    // the existing sealed routing-record trace category instead of widening the trace schema.
    // Trace handoff deliberately precedes the authoritative write and therefore fails closed.
    loft.capture_trace(&peer, TraceOperation::PutAgent).await?;
    let now = loft.now_secs();
    let address = address.to_string();
    let record = req.record;
    let capacity_bytes = loft.config.capacity_bytes;
    loft.blocking(move |store| {
        store
            .put_rotation_record(&address, &record, now, capacity_bytes)
            .map(|_| ())
    })
    .await
}

fn decode_address(segment: &str) -> Result<Address> {
    if segment.len() > MAX_ADDRESS_SEGMENT_BYTES {
        return Err(pigeonpost_core::Error::TooLarge.into());
    }
    let restored = match segment.split_once('-') {
        Some((tier, body)) => format!("/{tier}/{body}"),
        None => segment.to_string(),
    };
    Ok(Address::parse(&restored)?)
}

fn json_content_type(headers: &HeaderMap) -> bool {
    let Some(content_type) = headers
        .get(header::CONTENT_TYPE)
        .and_then(|value| value.to_str().ok())
        .and_then(|value| value.split(';').next())
        .map(str::trim)
    else {
        return false;
    };
    let Some((top_level, subtype)) = content_type.split_once('/') else {
        return false;
    };
    top_level.eq_ignore_ascii_case("application")
        && (subtype.eq_ignore_ascii_case("json")
            || subtype
                .rsplit_once('+')
                .is_some_and(|(_, suffix)| suffix.eq_ignore_ascii_case("json")))
}

fn parse_presentation(value: &str) -> Option<Presentation> {
    if value.len() != 64 {
        return None;
    }
    let mut bytes = [0u8; 32];
    for (index, chunk) in value.as_bytes().chunks(2).enumerate() {
        let chunk = std::str::from_utf8(chunk).ok()?;
        bytes[index] = u8::from_str_radix(chunk, 16).ok()?;
    }
    Some(Presentation::from_bytes(bytes))
}

pub(crate) fn hex(bytes: &[u8]) -> String {
    bytes.iter().map(|byte| format!("{byte:02x}")).collect()
}

/// Serve HTTP with source-enforced connection, header, and whole-connection lifetimes.
///
/// `axum::serve` intentionally exposes few transport knobs and does not install Hyper's timer for
/// the HTTP/1 header deadline. Keeping this boundary here makes those limits non-optional for every
/// production caller of [`crate::serve`].
pub(crate) async fn serve_http(
    listener: tokio::net::TcpListener,
    app: Router,
    config: &LoftConfig,
    mut stop: tokio::sync::oneshot::Receiver<()>,
) -> std::io::Result<()> {
    let connections = Arc::new(Semaphore::new(config.max_concurrent_connections));
    let (connection_stop, _) = tokio::sync::watch::channel(false);
    let mut tasks = tokio::task::JoinSet::new();
    let header_timeout = Duration::from_secs(config.header_timeout_secs);
    let response_timeout = Duration::from_secs(config.response_timeout_secs);
    let connection_lifetime = Duration::from_secs(
        config
            .header_timeout_secs
            .saturating_add(config.request_timeout_secs)
            .saturating_add(config.response_timeout_secs),
    );
    let max_streams = u32::try_from(config.max_concurrent_requests).unwrap_or(u32::MAX);

    loop {
        tokio::select! {
            _ = &mut stop => break,
            completed = tasks.join_next(), if !tasks.is_empty() => {
                if completed.is_some_and(|result| result.is_err()) {
                    let _ = connection_stop.send(true);
                    tasks.abort_all();
                    while tasks.join_next().await.is_some() {}
                    return Err(std::io::Error::other("loft HTTP connection task failed"));
                }
            }
            accepted = listener.accept() => {
                let (stream, peer) = accepted?;
                let permit = match Arc::clone(&connections).try_acquire_owned() {
                    Ok(permit) => permit,
                    Err(_) => {
                        // Do not move overload into an unbounded user-space queue. Closing the
                        // accepted socket also gives HTTP clients a clear retry signal.
                        drop(stream);
                        continue;
                    }
                };
                let _ = stream.set_nodelay(true);
                let app = app.clone();
                let connection_stop = connection_stop.subscribe();
                tasks.spawn(async move {
                    let _permit = permit;
                    serve_connection(
                        stream,
                        peer,
                        app,
                        header_timeout,
                        response_timeout,
                        connection_lifetime,
                        max_streams,
                        connection_stop,
                    ).await;
                });
            }
        }
    }

    let _ = connection_stop.send(true);
    if tokio::time::timeout(response_timeout, async {
        while let Some(result) = tasks.join_next().await {
            if result.is_err() {
                return Err(std::io::Error::other("loft HTTP connection task failed"));
            }
        }
        Ok(())
    })
    .await
    .map_err(|_| std::io::Error::other("loft HTTP connection drain timed out"))?
    .is_err()
    {
        return Err(std::io::Error::other("loft HTTP connection task failed"));
    }
    Ok(())
}

#[allow(clippy::too_many_arguments)]
async fn serve_connection(
    stream: tokio::net::TcpStream,
    peer: SocketAddr,
    app: Router,
    header_timeout: Duration,
    response_timeout: Duration,
    connection_lifetime: Duration,
    max_streams: u32,
    mut stop: tokio::sync::watch::Receiver<bool>,
) {
    let tower_service = app
        .layer(axum::Extension(ConnectInfo(peer)))
        .map_request(|request: Request<Incoming>| request.map(Body::new));
    let service = TowerToHyperService::new(tower_service);
    let mut builder = ConnectionBuilder::new(TokioExecutor::new());
    builder
        .http1()
        .timer(TokioTimer::new())
        .header_read_timeout(header_timeout)
        .max_headers(100)
        .max_buf_size(64 * 1024);
    builder
        .http2()
        .timer(TokioTimer::new())
        .max_concurrent_streams(max_streams)
        .max_header_list_size(64 * 1024);

    let connection = builder
        .serve_connection(TokioIo::new(stream), service)
        .into_owned();
    tokio::pin!(connection);
    let lifetime = tokio::time::sleep(connection_lifetime);
    tokio::pin!(lifetime);

    tokio::select! {
        _ = &mut lifetime => {}
        result = &mut connection => {
            if result.is_err() {
                // Protocol and peer I/O errors are isolated to this connection. Avoid logging a
                // network identifier or attacker-controlled parser detail on the ordinary path.
                tracing::debug!(kind = "connection", "loft HTTP connection closed");
            }
        }
        () = wait_for_connection_stop(&mut stop) => {
            connection.as_mut().graceful_shutdown();
            let _ = tokio::time::timeout(response_timeout, &mut connection).await;
        }
    }
}

async fn wait_for_connection_stop(stop: &mut tokio::sync::watch::Receiver<bool>) {
    if *stop.borrow() {
        return;
    }
    while stop.changed().await.is_ok() {
        if *stop.borrow() {
            return;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::store::{SqliteStore, StoredEvent};
    use crate::trace::TraceSinkError;
    use std::sync::atomic::AtomicUsize;
    use tokio::io::{AsyncReadExt, AsyncWriteExt};

    const BACKPRESSURE_CHUNK_BYTES: usize = 64 * 1024;

    struct BackpressuredTestBody {
        chunk: Bytes,
        first_poll: Arc<tokio::sync::Notify>,
        notified: bool,
    }

    impl BackpressuredTestBody {
        fn new(first_poll: Arc<tokio::sync::Notify>) -> Self {
            Self {
                chunk: Bytes::from(vec![0; BACKPRESSURE_CHUNK_BYTES]),
                first_poll,
                notified: false,
            }
        }
    }

    impl HttpBody for BackpressuredTestBody {
        type Data = Bytes;
        type Error = Infallible;

        fn poll_frame(
            mut self: Pin<&mut Self>,
            _cx: &mut Context<'_>,
        ) -> Poll<Option<std::result::Result<http_body::Frame<Self::Data>, Self::Error>>> {
            if !self.notified {
                self.notified = true;
                self.first_poll.notify_one();
            }
            // This deliberately never reaches end-of-stream. A non-reading peer therefore must
            // encounter finite transport backpressure on every platform; the test no longer
            // assumes that a particular finite response exceeds the host's socket buffers.
            Poll::Ready(Some(Ok(http_body::Frame::data(self.chunk.clone()))))
        }

        fn is_end_stream(&self) -> bool {
            false
        }

        fn size_hint(&self) -> http_body::SizeHint {
            http_body::SizeHint::new()
        }
    }

    struct NotReadyTrace;

    struct CountingTrace(AtomicUsize);

    struct StalledTrace {
        captures: AtomicUsize,
        release: AtomicBool,
    }

    struct GatedFetchStore {
        inner: SqliteStore,
        events: Vec<StoredEvent>,
        started: Arc<AtomicBool>,
        release: Arc<AtomicBool>,
    }

    impl LoftStore for GatedFetchStore {
        fn admit(
            &self,
            wrap: &pigeonpost_core::Wrap,
            id: &[u8; 32],
            stored_at: u64,
            expires_at: u64,
            capacity_bytes: u64,
            expected_policy_seq: Option<u64>,
        ) -> Result<bool> {
            self.inner.admit(
                wrap,
                id,
                stored_at,
                expires_at,
                capacity_bytes,
                expected_policy_seq,
            )
        }

        fn fetch(
            &self,
            _recipient: &[u8; 32],
            _cursor: u64,
            _limit: usize,
        ) -> Result<Vec<StoredEvent>> {
            self.started.store(true, Ordering::Release);
            while !self.release.load(Ordering::Acquire) {
                std::thread::sleep(Duration::from_millis(1));
            }
            Ok(self.events.clone())
        }

        fn policy(&self, pubkey: &[u8; 32]) -> Result<Option<RecipientPolicy>> {
            self.inner.policy(pubkey)
        }

        fn put_policy(&self, policy: &RecipientPolicy, capacity_bytes: u64) -> Result<()> {
            self.inner.put_policy(policy, capacity_bytes)
        }

        fn agent_record(&self, address: &str) -> Result<Option<pigeonpost_core::AgentRecord>> {
            self.inner.agent_record(address)
        }

        fn put_agent_record(
            &self,
            address: &str,
            record: &pigeonpost_core::AgentRecord,
            capacity_bytes: u64,
        ) -> Result<()> {
            self.inner.put_agent_record(address, record, capacity_bytes)
        }

        fn sweep_expired(&self, now: u64, batch: usize) -> Result<usize> {
            self.inner.sweep_expired(now, batch)
        }

        fn retention_checkpoint(&self) -> Result<()> {
            self.inner.retention_checkpoint()
        }

        fn stats(&self) -> Result<StorageStats> {
            self.inner.stats()
        }

        fn health_check(&self) -> Result<()> {
            self.inner.health_check()
        }
    }

    impl StalledTrace {
        fn new() -> Self {
            Self {
                captures: AtomicUsize::new(0),
                release: AtomicBool::new(false),
            }
        }
    }

    impl TraceSink for StalledTrace {
        fn readiness(&self) -> std::result::Result<(), TraceSinkError> {
            Ok(())
        }

        fn capture(&self, _input: TraceInput) -> std::result::Result<(), TraceSinkError> {
            self.captures.fetch_add(1, Ordering::AcqRel);
            while !self.release.load(Ordering::Acquire) {
                std::thread::sleep(Duration::from_millis(1));
            }
            Ok(())
        }
    }

    impl TraceSink for NotReadyTrace {
        fn readiness(&self) -> std::result::Result<(), TraceSinkError> {
            Err(TraceSinkError::Unavailable)
        }

        fn capture(&self, _input: TraceInput) -> std::result::Result<(), TraceSinkError> {
            Err(TraceSinkError::Unavailable)
        }
    }

    impl TraceSink for CountingTrace {
        fn readiness(&self) -> std::result::Result<(), TraceSinkError> {
            Ok(())
        }

        fn capture(&self, _input: TraceInput) -> std::result::Result<(), TraceSinkError> {
            self.0.fetch_add(1, Ordering::AcqRel);
            Ok(())
        }
    }

    #[test]
    fn invalid_runtime_bounds_fail_startup_validation() {
        fn assert_bad(mutator: impl FnOnce(&mut LoftConfig)) {
            let mut config = LoftConfig::new([0; 32], "http://127.0.0.1:1");
            mutator(&mut config);
            assert!(validate_config(&config).is_err());
        }

        assert!(validate_config(&LoftConfig::new([0; 32], "http://127.0.0.1:1")).is_ok());
        assert_bad(|config| config.capacity_bytes = crate::MAX_CAPACITY_BYTES + 1);
        assert_bad(|config| config.retention_days = crate::MAX_RETENTION_DAYS + 1);
        assert_bad(|config| config.max_event_bytes = crate::MAX_EVENT_BYTES + 1);
        assert_bad(|config| config.max_request_bytes = crate::MAX_REQUEST_BYTES + 1);
        assert_bad(|config| config.max_fetch_limit = crate::MAX_FETCH_LIMIT + 1);
        assert_bad(|config| {
            config.max_fetch_response_bytes = crate::MAX_FETCH_RESPONSE_BYTES + 1;
        });
        assert_bad(|config| {
            config.global_requests_per_minute = crate::MAX_GLOBAL_REQUESTS_PER_MINUTE + 1;
        });
        assert_bad(|config| config.global_bytes_per_minute = crate::MAX_RATE_BYTES_PER_MINUTE + 1);
        assert_bad(|config| {
            config.max_concurrent_connections = crate::MAX_CONCURRENT_CONNECTIONS + 1;
        });
        assert_bad(|config| config.max_concurrent_requests = crate::MAX_CONCURRENT_REQUESTS + 1);
        assert_bad(|config| config.max_blocking_operations = crate::MAX_BLOCKING_OPERATIONS + 1);
        assert_bad(|config| config.max_limiter_keys = crate::MAX_LIMITER_KEYS + 1);
        assert_bad(|config| config.request_timeout_secs = crate::MAX_REQUEST_TIMEOUT_SECS + 1);
        assert_bad(|config| config.header_timeout_secs = crate::MAX_HEADER_TIMEOUT_SECS + 1);
        assert_bad(|config| config.response_timeout_secs = crate::MAX_RESPONSE_TIMEOUT_SECS + 1);
        assert_bad(|config| config.trace_timeout_ms = crate::MAX_TRACE_TIMEOUT_MS + 1);
        assert_bad(|config| config.sweep_batch = crate::MAX_SWEEP_BATCH + 1);
        assert_bad(|config| config.sweep_interval_secs = crate::MAX_SWEEP_INTERVAL_SECS + 1);
    }

    #[test]
    fn constructor_rejects_invalid_limits_instead_of_allocating_or_panicking() {
        let store: Arc<dyn LoftStore> = Arc::new(SqliteStore::in_memory().unwrap());
        let mut config = LoftConfig::new([0; 32], "http://127.0.0.1:1");
        config.max_concurrent_requests = crate::MAX_CONCURRENT_REQUESTS + 1;
        assert!(matches!(
            Loft::new(config, store),
            Err(LoftError::Configuration(_))
        ));
    }

    #[test]
    fn presentation_parser_is_exact_and_non_panicking() {
        assert!(parse_presentation(&"aa".repeat(32)).is_some());
        assert!(parse_presentation(&"aa".repeat(31)).is_none());
        assert!(parse_presentation(&"zz".repeat(32)).is_none());
    }

    #[tokio::test(flavor = "current_thread")]
    async fn publish_json_and_crypto_wait_for_nonqueueing_blocking_admission() {
        let store: Arc<dyn LoftStore> = Arc::new(SqliteStore::in_memory().unwrap());
        let mut config = LoftConfig::new([1; 32], "http://127.0.0.1:1");
        config.max_blocking_operations = 1;
        let max_request_bytes = config.max_request_bytes;
        let loft = Arc::new(Loft::new(config, store).unwrap());
        let app = Arc::clone(&loft).router();
        let started = Arc::new(AtomicBool::new(false));
        let release = Arc::new(AtomicBool::new(false));
        let blocking_task = tokio::spawn({
            let loft = Arc::clone(&loft);
            let started = Arc::clone(&started);
            let release = Arc::clone(&release);
            async move {
                loft.blocking(move |_| {
                    started.store(true, Ordering::Release);
                    while !release.load(Ordering::Acquire) {
                        std::thread::sleep(Duration::from_millis(1));
                    }
                    Ok(())
                })
                .await
            }
        });

        let entered = tokio::time::timeout(Duration::from_secs(10), async {
            while !started.load(Ordering::Acquire) {
                tokio::task::yield_now().await;
            }
        })
        .await
        .is_ok();

        // Parsing this body would scan almost the full public request ceiling before discovering
        // the trailing invalid token. Saturated blocking admission must reject it before parsing.
        let mut hostile_json = vec![b' '; max_request_bytes.saturating_sub(1)];
        hostile_json.push(b'!');
        let publish = tokio::time::timeout(
            Duration::from_secs(10),
            app.oneshot(
                Request::builder()
                    .method(Method::POST)
                    .uri("/v1/publish")
                    .header(header::CONTENT_TYPE, "application/json")
                    .header(header::CONTENT_LENGTH, hostile_json.len())
                    .body(Body::from(hostile_json))
                    .unwrap(),
            ),
        )
        .await;
        let publish_status = publish
            .as_ref()
            .ok()
            .and_then(|result| result.as_ref().ok())
            .map(Response::status);

        blocking_task.abort();
        let _ = blocking_task.await;
        let permit_retained = loft.blocking.available_permits() == 0;
        let saturated =
            tokio::time::timeout(Duration::from_secs(10), loft.blocking(|_| Ok(()))).await;

        // Release before asserting so a failed regression cannot strand a blocking runtime worker.
        release.store(true, Ordering::Release);
        let recovered = tokio::time::timeout(Duration::from_secs(10), async {
            while loft.blocking.available_permits() != 1 {
                tokio::task::yield_now().await;
            }
        })
        .await
        .is_ok();

        assert!(entered, "the request blocking lane did not start");
        assert_eq!(publish_status, Some(StatusCode::TOO_MANY_REQUESTS));
        assert!(
            permit_retained,
            "canceling a caller released capacity before its blocking job ended"
        );
        assert!(matches!(saturated, Ok(Err(LoftError::Overloaded))));
        assert!(recovered, "the request blocking permit was not recovered");
    }

    #[test]
    fn configured_trace_sink_must_be_ready_before_start() {
        let store: Arc<dyn LoftStore> = Arc::new(SqliteStore::in_memory().unwrap());
        let loft = Loft::new(LoftConfig::new([0; 32], "http://127.0.0.1:1"), store)
            .unwrap()
            .with_trace_sink(Arc::new(NotReadyTrace));
        assert!(matches!(
            loft.validate_startup(),
            Err(LoftError::TraceUnavailable)
        ));
    }

    #[tokio::test]
    async fn durable_trace_rate_is_consumed_before_sink_capture() {
        let store: Arc<dyn LoftStore> = Arc::new(SqliteStore::in_memory().unwrap());
        let mut config = LoftConfig::new([1; 32], "http://127.0.0.1:1");
        config.global_requests_per_minute = 1;
        let trace = Arc::new(CountingTrace(AtomicUsize::new(0)));
        let loft = Loft::new(config, store)
            .unwrap()
            .with_trace_sink(trace.clone());
        let peer = Peer {
            connected: Some("127.0.0.1:1234".parse().unwrap()),
            forwarded: None,
            x_forwarded_for_present: false,
            effective_source: Some("127.0.0.1:1234".parse().unwrap()),
        };
        let operation = TraceOperation::Fetch { owner: [9; 32] };

        loft.capture_trace(&peer, operation).await.unwrap();
        assert!(matches!(
            loft.capture_trace(&peer, operation).await,
            Err(LoftError::RateLimited)
        ));
        assert_eq!(trace.0.load(Ordering::Acquire), 1);
    }

    #[test]
    fn construction_and_ready_trace_sink_do_not_bypass_supervised_readiness() {
        let store: Arc<dyn LoftStore> = Arc::new(SqliteStore::in_memory().unwrap());
        let mut config = LoftConfig::new([0; 32], "http://127.0.0.1:1");
        config.retention_days = 0;
        assert!(matches!(
            Loft::new(config, store),
            Err(LoftError::Configuration(_))
        ));
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn timed_out_trace_jobs_hold_the_bounded_lanes_and_saturation_fails_fast() {
        let store: Arc<dyn LoftStore> = Arc::new(SqliteStore::in_memory().unwrap());
        let mut config = LoftConfig::new([0; 32], "http://127.0.0.1:1");
        config.trace_timeout_ms = 20;
        let sink = Arc::new(StalledTrace::new());
        let loft = Arc::new(
            Loft::new(config, store)
                .unwrap()
                .with_trace_sink(sink.clone()),
        );
        let peer = Peer {
            connected: Some("192.0.2.10:4567".parse().unwrap()),
            forwarded: None,
            x_forwarded_for_present: false,
            effective_source: None,
        };

        let mut timed_out = tokio::task::JoinSet::new();
        for _ in 0..TRACE_BLOCKING_LANES {
            let loft = Arc::clone(&loft);
            timed_out.spawn(async move {
                let peer = Peer {
                    connected: Some("192.0.2.10:4567".parse().unwrap()),
                    forwarded: None,
                    x_forwarded_for_present: false,
                    effective_source: None,
                };
                loft.capture_trace(&peer, TraceOperation::PutAgent).await
            });
        }
        let started = tokio::time::timeout(Duration::from_secs(60), async {
            while sink.captures.load(Ordering::Acquire) < TRACE_BLOCKING_LANES {
                tokio::task::yield_now().await;
            }
        })
        .await
        .is_ok();
        while let Some(result) = timed_out.join_next().await {
            assert!(matches!(result.unwrap(), Err(LoftError::TraceUnavailable)));
        }
        let saturated = loft.capture_trace(&peer, TraceOperation::PutAgent).await;
        let captures_while_stalled = sink.captures.load(Ordering::Acquire);

        // Release before asserting so a failed regression cannot strand a blocking test worker.
        sink.release.store(true, Ordering::Release);
        let lane_released = tokio::time::timeout(Duration::from_secs(60), async {
            while loft.trace_blocking.available_permits() != TRACE_BLOCKING_LANES {
                tokio::task::yield_now().await;
            }
        })
        .await
        .is_ok();

        assert!(
            started,
            "the bounded blocking trace lanes did not all start"
        );
        assert!(matches!(saturated, Err(LoftError::TraceUnavailable)));
        assert_eq!(
            captures_while_stalled, TRACE_BLOCKING_LANES,
            "saturation spawned work beyond the fixed lane bound"
        );
        assert!(
            lane_released,
            "the trace permit did not follow job completion"
        );
        loft.capture_trace(&peer, TraceOperation::PutAgent)
            .await
            .unwrap();
        assert_eq!(
            sink.captures.load(Ordering::Acquire),
            TRACE_BLOCKING_LANES + 1
        );
    }

    #[test]
    fn forwarding_headers_are_ignored_from_untrusted_peers() {
        let config = LoftConfig::new([1; 32], "http://127.0.0.1:1");
        let peer = Peer {
            connected: Some("198.51.100.20:4000".parse().unwrap()),
            forwarded: Some(HeaderValue::from_static("for=192.0.2.10:5000")),
            x_forwarded_for_present: false,
            effective_source: None,
        };
        assert_eq!(
            resolve_peer_source(&config, &peer).unwrap(),
            "198.51.100.20:4000".parse().unwrap()
        );
    }

    #[test]
    fn trusted_proxy_chain_stops_at_the_first_untrusted_source() {
        let config = LoftConfig::new([1; 32], "http://127.0.0.1:1")
            .with_trusted_proxies(["127.0.0.1".parse().unwrap(), "127.0.0.2".parse().unwrap()]);
        let peer = Peer {
            connected: Some("127.0.0.1:443".parse().unwrap()),
            forwarded: Some(HeaderValue::from_static(
                "for=192.0.2.10:5000;proto=https, for=127.0.0.2:4444",
            )),
            x_forwarded_for_present: false,
            effective_source: None,
        };
        assert_eq!(
            resolve_peer_source(&config, &peer).unwrap(),
            "192.0.2.10:5000".parse().unwrap()
        );
    }

    #[test]
    fn trusted_proxy_source_without_a_port_fails_closed() {
        let config = LoftConfig::new([1; 32], "http://127.0.0.1:1")
            .with_trusted_proxies(["127.0.0.1".parse().unwrap()]);
        let peer = Peer {
            connected: Some("127.0.0.1:443".parse().unwrap()),
            forwarded: Some(HeaderValue::from_static("for=192.0.2.10")),
            x_forwarded_for_present: false,
            effective_source: None,
        };
        assert!(resolve_peer_source(&config, &peer).is_err());
    }

    #[tokio::test]
    async fn request_permit_follows_response_until_body_drop() {
        let store: Arc<dyn LoftStore> = Arc::new(SqliteStore::in_memory().unwrap());
        let mut config = LoftConfig::new([1; 32], "http://127.0.0.1:1");
        config.max_concurrent_requests = 1;
        let app = Arc::new(Loft::new(config, store).unwrap()).router();

        let first = app
            .clone()
            .oneshot(
                Request::builder()
                    .uri("/health")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(first.status(), StatusCode::OK);
        let rejected = app
            .clone()
            .oneshot(
                Request::builder()
                    .uri("/health")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(rejected.status(), StatusCode::TOO_MANY_REQUESTS);

        drop(first);
        let admitted = app
            .oneshot(
                Request::builder()
                    .uri("/health")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(admitted.status(), StatusCode::OK);
    }

    #[tokio::test(flavor = "current_thread")]
    async fn fetch_encoding_uses_blocking_lane_and_response_keeps_request_permit() {
        let runtime_thread = std::thread::current().id();
        let identity = pigeonpost_core::Identity::from_seed([2; 32]);
        let recipient = identity.verifying_key().to_bytes();
        let events = (1..=16)
            .map(|cursor| StoredEvent {
                cursor,
                wrap: pigeonpost_core::Wrap {
                    version: 3,
                    ephemeral_pubkey: [3; 32],
                    recipient,
                    nonce: [4; 24],
                    ciphertext: vec![5; 512 * 1024],
                    created_at: 1,
                    signature: [6; 64],
                    pow_nonce: 0,
                    attribution: None,
                },
            })
            .collect();
        let started = Arc::new(AtomicBool::new(false));
        let release = Arc::new(AtomicBool::new(false));
        let store: Arc<dyn LoftStore> = Arc::new(GatedFetchStore {
            inner: SqliteStore::in_memory().unwrap(),
            events,
            started: Arc::clone(&started),
            release: Arc::clone(&release),
        });
        let mut config = LoftConfig::new([1; 32], "http://127.0.0.1:1");
        config.max_event_bytes = 64 * 1024;
        config.max_concurrent_requests = 1;
        let loft = Arc::new(Loft::new(config, store).unwrap());
        let minute = loft.now_secs() / 60;
        let auth = pigeonpost_core::FetchAuth::new(
            &identity,
            &loft.config.pubkey,
            &loft.config.origin,
            minute,
            0,
        )
        .unwrap();
        let body = serde_json::to_vec(&FetchRequest {
            auth,
            limit: Some(127),
        })
        .unwrap();
        let app = Arc::clone(&loft).router();
        let fetch_request = Request::builder()
            .method(Method::POST)
            .uri("/v1/fetch")
            .header(header::CONTENT_TYPE, "application/json")
            .header(header::CONTENT_LENGTH, body.len())
            .body(Body::from(body))
            .unwrap();
        let first = tokio::spawn(app.clone().oneshot(fetch_request));
        tokio::time::timeout(Duration::from_secs(10), async {
            while !started.load(Ordering::Acquire) {
                tokio::task::yield_now().await;
            }
        })
        .await
        .expect("fetch did not enter the bounded blocking lane");

        let overloaded = app
            .clone()
            .oneshot(
                Request::builder()
                    .uri("/health")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(overloaded.status(), StatusCode::TOO_MANY_REQUESTS);

        release.store(true, Ordering::Release);

        let response = tokio::time::timeout(Duration::from_secs(10), first)
            .await
            .expect("bounded fetch did not finish")
            .unwrap()
            .unwrap();
        assert_eq!(response.status(), StatusCode::PAYLOAD_TOO_LARGE);
        let encoding_thread = (*loft.fetch_encoding_thread.lock().unwrap())
            .expect("fetch encoding thread was not observed");
        assert_ne!(
            encoding_thread, runtime_thread,
            "fetch JSON encoding ran on the current-thread Tokio scheduler"
        );
        let still_held = app
            .clone()
            .oneshot(
                Request::builder()
                    .uri("/health")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(still_held.status(), StatusCode::TOO_MANY_REQUESTS);

        drop(response);
        let recovered = app
            .oneshot(
                Request::builder()
                    .uri("/health")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(recovered.status(), StatusCode::OK);
    }

    #[tokio::test]
    async fn incomplete_headers_are_closed_by_the_timer_backed_deadline() {
        let store: Arc<dyn LoftStore> = Arc::new(SqliteStore::in_memory().unwrap());
        let mut config = LoftConfig::new([1; 32], "http://127.0.0.1:1");
        config.header_timeout_secs = 1;
        config.request_timeout_secs = 1;
        config.response_timeout_secs = 1;
        let loft = Arc::new(Loft::new(config.clone(), store).unwrap());
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        let (stop, stopped) = tokio::sync::oneshot::channel();
        let app = loft.router();
        let task = tokio::spawn(async move { serve_http(listener, app, &config, stopped).await });

        let mut stream = tokio::net::TcpStream::connect(address).await.unwrap();
        stream
            .write_all(b"GET /health HTTP/1.1\r\nHost: localhost\r\n")
            .await
            .unwrap();
        let mut byte = [0u8; 1];
        let closed = tokio::time::timeout(Duration::from_secs(60), stream.read(&mut byte))
            .await
            .expect("incomplete headers must not retain a connection");
        assert!(matches!(closed, Ok(0) | Err(_)));

        let _ = stop.send(());
        task.await.unwrap().unwrap();
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn stalled_reader_cannot_release_admission_or_hold_transport_forever() {
        let store: Arc<dyn LoftStore> = Arc::new(SqliteStore::in_memory().unwrap());
        let mut config = LoftConfig::new([1; 32], "http://127.0.0.1:1");
        config.max_concurrent_connections = 4;
        config.max_concurrent_requests = 1;
        config.header_timeout_secs = 1;
        config.request_timeout_secs = 1;
        config.response_timeout_secs = 1;
        let loft = Arc::new(Loft::new(config.clone(), store).unwrap());
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        let (stop, stopped) = tokio::sync::oneshot::channel();
        let first_poll = Arc::new(tokio::sync::Notify::new());
        let response_first_poll = Arc::clone(&first_poll);
        let state = Arc::clone(&loft);
        let app = Router::new()
            .route("/health", get(|| async { "ok" }))
            .route(
                "/__test/slow-response",
                get(move || {
                    let first_poll = Arc::clone(&response_first_poll);
                    async move { Body::new(BackpressuredTestBody::new(first_poll)) }
                }),
            )
            .layer(TimeoutLayer::with_status_code(
                StatusCode::REQUEST_TIMEOUT,
                Duration::from_secs(config.request_timeout_secs),
            ))
            .layer(axum::extract::DefaultBodyLimit::max(
                config.max_request_bytes,
            ))
            .layer(middleware::from_fn_with_state(state, admission_middleware))
            .with_state(Arc::clone(&loft));
        let task = tokio::spawn(async move { serve_http(listener, app, &config, stopped).await });

        let mut stalled = tokio::net::TcpStream::connect(address).await.unwrap();
        stalled
            .write_all(
                b"GET /__test/slow-response HTTP/1.1\r\nHost: localhost\r\nConnection: close\r\n\r\n",
            )
            .await
            .unwrap();
        tokio::time::timeout(Duration::from_secs(60), first_poll.notified())
            .await
            .expect("large response body must reach the transport");
        assert!(matches!(
            loft.admission.try_enter(),
            Err(LoftError::Overloaded)
        ));

        let client = reqwest::Client::new();
        let overloaded = client
            .get(format!("http://{address}/health"))
            .send()
            .await
            .unwrap();
        assert_eq!(overloaded.status(), StatusCode::TOO_MANY_REQUESTS);
        drop(overloaded);

        tokio::time::timeout(Duration::from_secs(6), async {
            loop {
                let response = client
                    .get(format!("http://{address}/health"))
                    .send()
                    .await
                    .unwrap();
                let status = response.status();
                drop(response);
                if status == StatusCode::OK {
                    break;
                }
                assert_eq!(status, StatusCode::TOO_MANY_REQUESTS);
                tokio::time::sleep(Duration::from_millis(10)).await;
            }
        })
        .await
        .expect("stalled response must release request admission by its transport deadline");

        drop(stalled);
        let _ = stop.send(());
        task.await.unwrap().unwrap();
    }
}
