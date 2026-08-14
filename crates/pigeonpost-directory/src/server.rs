//! The directory HTTP surface.
//!
//! `directory.json` is a static, cacheable, mirrorable file. The client's directory URL is
//! configuration, and multiple directories are accepted by design — the same exit right the
//! registry has, applied to the pool (`docs/network.md`).

use std::collections::HashMap;
use std::error::Error as StdError;
use std::future::Future;
use std::hash::Hash;
use std::net::{IpAddr, SocketAddr};
use std::pin::Pin;
use std::sync::{Arc, Mutex};
use std::task::{Context, Poll};
use std::time::{Duration, Instant};

use axum::{
    body::{Body, Bytes, HttpBody},
    extract::{ConnectInfo, Path, Query, Request, State},
    http::{header, HeaderMap, HeaderValue, Method, StatusCode},
    middleware::{self, Next},
    response::{IntoResponse, Response},
    routing::{get, post},
    Json, Router,
};
use hyper::body::Incoming;
use hyper_util::rt::{TokioExecutor, TokioIo, TokioTimer};
use hyper_util::server::conn::auto::Builder as ConnectionBuilder;
use hyper_util::service::TowerToHyperService;
use serde::{Deserialize, Serialize};
use tower::ServiceExt;

use pigeonpost_core::Identity;

use crate::directory::{Directory, ProbePage, ProbeResult, PROBE_INTERVAL_SECS};
use crate::document::{
    push_field, verify_document_signature, DirectoryDocument, MAX_DIRECTORY_DOCUMENT_BYTES,
};
use crate::entry::{hex, DirectoryEntry, DrainAuthorization};
use crate::error::{DirectoryError, Result};
use crate::prober;
use crate::registry_log::{DirectoryMutationReceipt, RegistryLogClient};

const PROBE_DOCUMENT_DOMAIN: &[u8] = b"pigeonpost/probe-document/v2";
const DEFAULT_PROBE_PAGE_SIZE: usize = 200;
const MAX_PROBE_PAGE_SIZE: usize = 500;
const MAX_BLOCKING_OPERATIONS: usize = 16;
const BLOCKING_OPERATION_TIMEOUT: Duration = Duration::from_secs(5);
const RATE_WINDOW: Duration = Duration::from_secs(60);
const READINESS_TIMEOUT: Duration = Duration::from_secs(2);
const MAX_RECOVERY_MUTATIONS_PER_PASS: usize = 64;
const RECOVERY_PASS_OVERHEAD: Duration = Duration::from_secs(5);
const RECOVERY_PASS_MAX: Duration = Duration::from_secs(5 * 60);
const RECOVERY_IDLE_INTERVAL: Duration = Duration::from_secs(1);
const RECOVERY_MAX_BACKOFF: Duration = Duration::from_secs(60);
const SERVER_DRAIN_TIMEOUT: Duration = Duration::from_secs(30);

#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct DirectoryLimits {
    pub max_concurrent_connections: usize,
    pub max_concurrent_requests: usize,
    pub max_concurrent_mutations: usize,
    pub max_blocking_operations: usize,
    pub header_timeout_ms: u64,
    pub request_timeout_ms: u64,
    pub response_timeout_ms: u64,
    pub blocking_timeout_ms: u64,
    pub global_requests_per_minute: u32,
    pub global_response_bytes_per_minute: u64,
    pub source_requests_per_minute: u32,
    pub source_response_bytes_per_minute: u64,
    pub source_mutations_per_minute: u32,
    pub loft_mutations_per_minute: u32,
    pub max_rate_keys: usize,
}

impl Default for DirectoryLimits {
    fn default() -> Self {
        Self {
            max_concurrent_connections: 128,
            max_concurrent_requests: 64,
            max_concurrent_mutations: 1,
            max_blocking_operations: MAX_BLOCKING_OPERATIONS,
            header_timeout_ms: 5_000,
            request_timeout_ms: 30_000,
            response_timeout_ms: 15_000,
            blocking_timeout_ms: BLOCKING_OPERATION_TIMEOUT.as_millis() as u64,
            global_requests_per_minute: 6_000,
            global_response_bytes_per_minute: 256 * 1024 * 1024,
            source_requests_per_minute: 600,
            source_response_bytes_per_minute: 64 * 1024 * 1024,
            source_mutations_per_minute: 20,
            loft_mutations_per_minute: 10,
            max_rate_keys: 4_096,
        }
    }
}

#[derive(Debug, Clone, Default)]
pub struct DirectoryHttpConfig {
    trusted_proxies: Vec<IpAddr>,
    limits: DirectoryLimits,
}

impl DirectoryHttpConfig {
    pub fn direct() -> Self {
        Self::default()
    }

    pub fn with_trusted_proxies(trusted_proxies: Vec<IpAddr>) -> Result<Self> {
        if trusted_proxies.len() > 64
            || trusted_proxies
                .iter()
                .any(|ip| ip.is_unspecified() || ip.is_multicast())
        {
            return Err(DirectoryError::Malformed(
                "trusted proxy list is invalid".into(),
            ));
        }
        Ok(Self {
            trusted_proxies,
            limits: DirectoryLimits::default(),
        })
    }

    pub fn with_limits(mut self, limits: DirectoryLimits) -> Result<Self> {
        validate_limits(limits)?;
        self.limits = limits;
        Ok(self)
    }
}

#[derive(Debug, Clone, Copy)]
struct RateBucket {
    requests: u32,
    response_bytes: u64,
    window_start: Instant,
}

impl RateBucket {
    fn fresh(now: Instant) -> Self {
        Self {
            requests: 0,
            response_bytes: 0,
            window_start: now,
        }
    }

    fn charge(&mut self, now: Instant, limit: u32) -> Result<()> {
        if now.duration_since(self.window_start) >= RATE_WINDOW {
            *self = Self::fresh(now);
        }
        if self.requests >= limit {
            return Err(DirectoryError::RateLimited);
        }
        self.requests = self.requests.saturating_add(1);
        Ok(())
    }

    fn charge_response_bytes(&mut self, now: Instant, bytes: u64, limit: u64) -> Result<()> {
        if now.duration_since(self.window_start) >= RATE_WINDOW {
            *self = Self::fresh(now);
        }
        let next = self.response_bytes.saturating_add(bytes);
        if next > limit {
            return Err(DirectoryError::RateLimited);
        }
        self.response_bytes = next;
        Ok(())
    }
}

#[derive(Debug)]
struct KeyBuckets<K> {
    state: Mutex<KeyBucketState<K>>,
    max_keys: usize,
}

#[derive(Debug)]
struct KeyBucketState<K> {
    buckets: HashMap<K, RateBucket>,
    next_cleanup_at: Option<Instant>,
    #[cfg(test)]
    cleanup_scans: usize,
}

impl<K> KeyBuckets<K>
where
    K: Eq + Hash + Clone,
{
    fn new(max_keys: usize) -> Self {
        Self {
            state: Mutex::new(KeyBucketState {
                buckets: HashMap::new(),
                next_cleanup_at: None,
                #[cfg(test)]
                cleanup_scans: 0,
            }),
            max_keys,
        }
    }

    fn make_room(
        state: &mut KeyBucketState<K>,
        key: &K,
        max_keys: usize,
        now: Instant,
    ) -> Result<()> {
        if state.buckets.contains_key(key) || state.buckets.len() < max_keys {
            return Ok(());
        }
        // Full live maps reject in O(1). Only scan after the earliest possible expiry; otherwise
        // rotating sources or loft keys could force an O(max_keys) retain under this mutex for
        // every request.
        if state
            .next_cleanup_at
            .is_some_and(|deadline| now >= deadline)
        {
            state
                .buckets
                .retain(|_, bucket| now.duration_since(bucket.window_start) < RATE_WINDOW);
            // Do not immediately chase staggered expiries with another full scan. A conservative
            // one-window delay in slot reuse bounds cleanup work to one O(N) pass per window.
            state.next_cleanup_at = (!state.buckets.is_empty()).then_some(now + RATE_WINDOW);
            #[cfg(test)]
            {
                state.cleanup_scans += 1;
            }
        }
        if state.buckets.len() >= max_keys {
            return Err(DirectoryError::RateLimited);
        }
        Ok(())
    }

    fn note_insert(state: &mut KeyBucketState<K>, now: Instant) {
        let expiry = now + RATE_WINDOW;
        state.next_cleanup_at = Some(
            state
                .next_cleanup_at
                .map_or(expiry, |earliest| earliest.min(expiry)),
        );
    }

    fn charge(&self, key: K, limit: u32) -> Result<()> {
        let now = Instant::now();
        let mut state = self.state.lock().unwrap_or_else(|error| error.into_inner());
        let inserting = !state.buckets.contains_key(&key);
        Self::make_room(&mut state, &key, self.max_keys, now)?;
        if inserting {
            Self::note_insert(&mut state, now);
        }
        state
            .buckets
            .entry(key)
            .or_insert_with(|| RateBucket::fresh(now))
            .charge(now, limit)
    }

    fn charge_response_bytes(&self, key: K, bytes: u64, limit: u64) -> Result<()> {
        let now = Instant::now();
        let mut state = self.state.lock().unwrap_or_else(|error| error.into_inner());
        let inserting = !state.buckets.contains_key(&key);
        Self::make_room(&mut state, &key, self.max_keys, now)?;
        if inserting {
            Self::note_insert(&mut state, now);
        }
        state
            .buckets
            .entry(key)
            .or_insert_with(|| RateBucket::fresh(now))
            .charge_response_bytes(now, bytes, limit)
    }

    #[cfg(test)]
    fn cleanup_scans(&self) -> usize {
        self.state
            .lock()
            .unwrap_or_else(|error| error.into_inner())
            .cleanup_scans
    }
}

#[derive(Debug)]
struct RequestLimiter {
    global: Mutex<RateBucket>,
    global_limit: u32,
    global_response_bytes_limit: u64,
    sources: KeyBuckets<IpAddr>,
    source_limit: u32,
    source_response_bytes_limit: u64,
    mutations: KeyBuckets<IpAddr>,
    mutation_limit: u32,
    lofts: KeyBuckets<String>,
    loft_limit: u32,
}

impl RequestLimiter {
    fn new(limits: DirectoryLimits) -> Self {
        Self {
            global: Mutex::new(RateBucket::fresh(Instant::now())),
            global_limit: limits.global_requests_per_minute,
            global_response_bytes_limit: limits.global_response_bytes_per_minute,
            sources: KeyBuckets::new(limits.max_rate_keys),
            source_limit: limits.source_requests_per_minute,
            source_response_bytes_limit: limits.source_response_bytes_per_minute,
            mutations: KeyBuckets::new(limits.max_rate_keys),
            mutation_limit: limits.source_mutations_per_minute,
            lofts: KeyBuckets::new(limits.max_rate_keys),
            loft_limit: limits.loft_mutations_per_minute,
        }
    }

    fn charge_global(&self) -> Result<()> {
        self.global
            .lock()
            .unwrap_or_else(|error| error.into_inner())
            .charge(Instant::now(), self.global_limit)
    }

    fn charge_request(&self, source: IpAddr) -> Result<()> {
        self.charge_global()?;
        self.sources.charge(source, self.source_limit)
    }

    fn charge_mutation(&self, source: IpAddr) -> Result<()> {
        self.mutations.charge(source, self.mutation_limit)
    }

    fn charge_loft(&self, loft_key: &str) -> Result<()> {
        self.lofts.charge(loft_key.to_owned(), self.loft_limit)
    }

    fn charge_egress(&self, source: Option<IpAddr>, bytes: u64) -> Result<()> {
        self.global
            .lock()
            .unwrap_or_else(|error| error.into_inner())
            .charge_response_bytes(Instant::now(), bytes, self.global_response_bytes_limit)?;
        if let Some(source) = source {
            self.sources
                .charge_response_bytes(source, bytes, self.source_response_bytes_limit)?;
        }
        Ok(())
    }
}

#[derive(Clone, Copy)]
struct EffectiveSource(IpAddr);

struct AdmittedBody {
    body: Pin<Box<Body>>,
    deadline: Pin<Box<tokio::time::Sleep>>,
    permit: Option<tokio::sync::OwnedSemaphorePermit>,
    timed_out: bool,
}

impl AdmittedBody {
    fn new(body: Body, permit: tokio::sync::OwnedSemaphorePermit, timeout: Duration) -> Self {
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
                "directory response body lifetime exceeded",
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
        false
    }

    fn size_hint(&self) -> http_body::SizeHint {
        self.body.size_hint()
    }
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SubmitRequest {
    pub entry: DirectoryEntry,
}

#[derive(Debug, Serialize)]
pub struct SubmitResponse {
    pub accepted: bool,
    pub registry: DirectoryMutationReceipt,
}

#[derive(Debug, Serialize)]
pub struct DrainResponse {
    pub draining: bool,
    pub registry: DirectoryMutationReceipt,
}

#[derive(Debug, Deserialize)]
struct ProbeQuery {
    endpoint: String,
    #[serde(default)]
    cursor: Option<u64>,
    #[serde(default)]
    limit: Option<usize>,
}

#[derive(Debug, Default, Deserialize)]
#[serde(deny_unknown_fields)]
struct ProbePageQuery {
    #[serde(default)]
    cursor: Option<u64>,
    #[serde(default)]
    limit: Option<usize>,
}

/// Signed raw measurements for one loft.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ProbeDocument {
    pub version: u32,
    pub generated_at: u64,
    pub endpoint: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cursor: Option<u64>,
    pub probes: Vec<ProbeResult>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub next_cursor: Option<u64>,
    pub more: bool,
    pub signing_key: String,
    pub signature: String,
}

#[derive(Clone, Copy, Serialize)]
struct ProbePagination {
    cursor: Option<u64>,
    next_cursor: Option<u64>,
    more: bool,
}

impl ProbeDocument {
    fn signed(
        directory: &Directory,
        generated_at: u64,
        endpoint: String,
        cursor: Option<u64>,
        page: ProbePage,
    ) -> Result<Self> {
        let signing_key = hex(&directory.signing_public_key());
        let pagination = ProbePagination {
            cursor,
            next_cursor: page.next_cursor,
            more: page.more,
        };
        let payload = probe_document_payload(
            2,
            generated_at,
            &endpoint,
            pagination,
            &page.probes,
            &signing_key,
        )?;
        let document = Self {
            version: 2,
            generated_at,
            endpoint,
            cursor,
            probes: page.probes,
            next_cursor: page.next_cursor,
            more: page.more,
            signing_key,
            signature: hex(&directory.sign(&payload)),
        };
        if serde_json::to_vec(&document)?.len() > MAX_DIRECTORY_DOCUMENT_BYTES {
            return Err(DirectoryError::ResponseTooLarge);
        }
        Ok(document)
    }

    pub fn verify(&self, expected_key: &[u8; 32]) -> Result<()> {
        if self.version != 2 {
            return Err(DirectoryError::Malformed(
                "unsupported probe document version".into(),
            ));
        }
        if self.probes.len() > MAX_PROBE_PAGE_SIZE {
            return Err(DirectoryError::Malformed(
                "probe document contains too many records".into(),
            ));
        }
        if self.endpoint.len() > 2_048
            || serde_json::to_vec(self)?.len() > MAX_DIRECTORY_DOCUMENT_BYTES
            || self.probes.iter().any(|probe| {
                probe.at > self.generated_at
                    || !probe.utilization.is_finite()
                    || !(0.0..=1.0).contains(&probe.utilization)
                    || probe
                        .detail
                        .as_ref()
                        .is_some_and(|detail| detail.len() > 1_024)
                    || probe.retention_age_secs.is_some() != probe.retention_ok.is_some()
            })
        {
            return Err(DirectoryError::Malformed(
                "probe document contains an invalid bounded measurement".into(),
            ));
        }
        if self.more != self.next_cursor.is_some()
            || self
                .probes
                .iter()
                .any(|probe| probe.endpoint != self.endpoint)
        {
            return Err(DirectoryError::Malformed(
                "probe document pagination or endpoint is inconsistent".into(),
            ));
        }
        verify_document_signature(
            expected_key,
            &self.signing_key,
            &self.signature,
            &probe_document_payload(
                self.version,
                self.generated_at,
                &self.endpoint,
                ProbePagination {
                    cursor: self.cursor,
                    next_cursor: self.next_cursor,
                    more: self.more,
                },
                &self.probes,
                &self.signing_key,
            )?,
        )
    }
}

struct DirectoryServer {
    directory: Arc<Directory>,
    registry_log: Option<Arc<RegistryLogClient>>,
    config: DirectoryHttpConfig,
    /// Requests and mutations use non-waiting permits; overload never creates an async queue.
    requests: Arc<tokio::sync::Semaphore>,
    mutations: Arc<tokio::sync::Semaphore>,
    /// Bounds synchronous SQLite work before it reaches Tokio's shared blocking pool.
    blocking: Arc<tokio::sync::Semaphore>,
    limiter: RequestLimiter,
    request_timeout: Duration,
    blocking_timeout: Duration,
    public_storage_required: bool,
}

impl DirectoryServer {
    #[cfg(any(test, feature = "test-utilities"))]
    fn new(
        directory: Arc<Directory>,
        registry_log: Option<Arc<RegistryLogClient>>,
        config: DirectoryHttpConfig,
    ) -> Arc<Self> {
        Self::new_with_storage_requirement(directory, registry_log, config, false)
    }

    fn new_public(
        directory: Arc<Directory>,
        registry_log: Arc<RegistryLogClient>,
        config: DirectoryHttpConfig,
    ) -> Arc<Self> {
        Self::new_with_storage_requirement(directory, Some(registry_log), config, true)
    }

    fn new_with_storage_requirement(
        directory: Arc<Directory>,
        registry_log: Option<Arc<RegistryLogClient>>,
        config: DirectoryHttpConfig,
        public_storage_required: bool,
    ) -> Arc<Self> {
        debug_assert!(validate_limits(config.limits).is_ok());
        Arc::new(Self {
            directory,
            registry_log,
            requests: Arc::new(tokio::sync::Semaphore::new(
                config.limits.max_concurrent_requests,
            )),
            mutations: Arc::new(tokio::sync::Semaphore::new(
                config.limits.max_concurrent_mutations,
            )),
            blocking: Arc::new(tokio::sync::Semaphore::new(
                config.limits.max_blocking_operations,
            )),
            limiter: RequestLimiter::new(config.limits),
            request_timeout: Duration::from_millis(config.limits.request_timeout_ms),
            blocking_timeout: Duration::from_millis(config.limits.blocking_timeout_ms),
            public_storage_required,
            config,
        })
    }

    fn try_request(&self) -> Result<tokio::sync::OwnedSemaphorePermit> {
        Arc::clone(&self.requests)
            .try_acquire_owned()
            .map_err(|_| DirectoryError::Overloaded)
    }

    fn try_mutation(&self) -> Result<tokio::sync::OwnedSemaphorePermit> {
        Arc::clone(&self.mutations)
            .try_acquire_owned()
            .map_err(|_| DirectoryError::Overloaded)
    }

    async fn blocking<T, F>(&self, task: F) -> Result<T>
    where
        T: Send + 'static,
        F: FnOnce() -> Result<T> + Send + 'static,
    {
        let permit = Arc::clone(&self.blocking)
            .try_acquire_owned()
            .map_err(|_| DirectoryError::Overloaded)?;
        let task = tokio::task::spawn_blocking(move || {
            let _permit = permit;
            task()
        });
        tokio::time::timeout(self.blocking_timeout, task)
            .await
            .map_err(|_| DirectoryError::Overloaded)?
            .map_err(|_| DirectoryError::Io(std::io::Error::other("directory worker failed")))?
    }

    async fn reconcile_reservations(&self) -> Result<()> {
        let directory = Arc::clone(&self.directory);
        let has_pending = self
            .blocking(move || directory.has_pending_mutations())
            .await?;
        if !has_pending {
            return Ok(());
        }
        let registry = self.registry_log.as_ref().ok_or(DirectoryError::NotReady)?;
        let recovery_timeout = registry
            .witness_wait()
            .saturating_add(RECOVERY_PASS_OVERHEAD)
            .min(RECOVERY_PASS_MAX);
        tokio::time::timeout(recovery_timeout, async {
            let directory = Arc::clone(&self.directory);
            let pending = self
                .blocking(move || directory.pending_mutations(MAX_RECOVERY_MUTATIONS_PER_PASS))
                .await?;
            for reservation in pending {
                let directory = Arc::clone(&self.directory);
                let previous = self
                    .blocking(move || directory.registry_checkpoint())
                    .await?;
                let receipt = registry
                    .append_pending(self.directory.as_ref(), &reservation, previous.as_ref())
                    .await?;
                let checkpoint = receipt.persisted_checkpoint()?;
                let directory = Arc::clone(&self.directory);
                self.blocking(move || {
                    directory.finalize_pending_mutation(&reservation, &checkpoint)
                })
                .await?;
            }
            Ok::<(), DirectoryError>(())
        })
        .await
        .map_err(|_| DirectoryError::RegistryPublicationTimeout)??;
        let directory = Arc::clone(&self.directory);
        if self
            .blocking(move || directory.has_pending_mutations())
            .await?
        {
            return Err(DirectoryError::NotReady);
        }
        Ok(())
    }
}

fn validate_limits(limits: DirectoryLimits) -> Result<()> {
    if limits.max_concurrent_connections == 0
        || limits.max_concurrent_connections > 4_096
        || limits.max_concurrent_requests == 0
        || limits.max_concurrent_requests > 4_096
        || limits.max_concurrent_mutations == 0
        || limits.max_concurrent_mutations > 64
        || limits.max_blocking_operations == 0
        || limits.max_blocking_operations > 64
        || limits.header_timeout_ms == 0
        || limits.header_timeout_ms > 5 * 60 * 1_000
        || limits.request_timeout_ms == 0
        || limits.request_timeout_ms > 5 * 60 * 1_000
        || limits.response_timeout_ms == 0
        || limits.response_timeout_ms > 5 * 60 * 1_000
        || limits.blocking_timeout_ms == 0
        || limits.blocking_timeout_ms > 30_000
        || limits.global_requests_per_minute == 0
        || limits.global_requests_per_minute > 1_000_000
        || limits.global_response_bytes_per_minute == 0
        || limits.global_response_bytes_per_minute > 1024 * 1024 * 1024 * 1024
        || limits.source_requests_per_minute == 0
        || limits.source_requests_per_minute > 100_000
        || limits.source_response_bytes_per_minute == 0
        || limits.source_response_bytes_per_minute > 1024 * 1024 * 1024 * 1024
        || limits.source_mutations_per_minute == 0
        || limits.source_mutations_per_minute > 10_000
        || limits.loft_mutations_per_minute == 0
        || limits.loft_mutations_per_minute > 10_000
        || limits.max_rate_keys == 0
        || limits.max_rate_keys > 65_536
    {
        return Err(DirectoryError::Malformed(
            "directory HTTP limits are outside the supported bounds".into(),
        ));
    }
    Ok(())
}

/// Serve the complete directory surface on an already-bound listener.
///
/// Route construction is intentionally private. This boundary owns HTTP admission, exact
/// reservation recovery, and the one production prober as a single runtime: requested shutdown
/// drains all three, while an early return, error, or panic in any one stops the other two and
/// fails the service closed.
pub async fn serve(
    listener: tokio::net::TcpListener,
    directory: Arc<Directory>,
    registry_log: Arc<RegistryLogClient>,
    config: DirectoryHttpConfig,
    prober_identity: Arc<Identity>,
    stop: tokio::sync::watch::Receiver<bool>,
) -> Result<()> {
    listener.local_addr()?;
    validate_limits(config.limits)?;
    directory.verify_public_storage_ready()?;
    directory.verify_registry_logging_ready()?;

    let state = DirectoryServer::new_public(Arc::clone(&directory), registry_log, config);
    serve_supervised(listener, state, stop, move |prober_stop| {
        prober::run_until(directory, prober_identity, PROBE_INTERVAL_SECS, prober_stop)
    })
    .await
}

/// Listener-bound, loopback-only read fixture for cross-crate integration tests.
///
/// Even when `test-utilities` is explicitly enabled, no raw Axum router and no mutation route is
/// exposed to callers.
#[cfg(feature = "test-utilities")]
#[doc(hidden)]
pub async fn serve_loopback_test<F>(
    listener: tokio::net::TcpListener,
    directory: Arc<Directory>,
    shutdown: F,
) -> Result<()>
where
    F: Future<Output = ()> + Send + 'static,
{
    validate_loopback_listener(&listener)?;
    // Cross-crate read fixtures have no production prober. Record one explicit synthetic sweep so
    // they exercise the same freshness gate instead of gaining a test-only router bypass.
    directory.mark_probe_sweep(now())?;
    let state = DirectoryServer::new(directory, None, DirectoryHttpConfig::direct());
    axum::serve(
        listener,
        build_read_only_router(state).into_make_service_with_connect_info::<SocketAddr>(),
    )
    .with_graceful_shutdown(shutdown)
    .await?;
    Ok(())
}

#[cfg(feature = "test-utilities")]
fn validate_loopback_listener(listener: &tokio::net::TcpListener) -> Result<()> {
    if !listener.local_addr()?.ip().is_loopback() {
        return Err(DirectoryError::Malformed(
            "directory test fixture may listen only on loopback".into(),
        ));
    }
    Ok(())
}

/// Read-capable raw router retained only for in-crate tests. Mutation endpoints fail closed until
/// witnessed registry logging is explicitly configured.
#[cfg(test)]
pub(crate) fn router(directory: Arc<Directory>) -> Router {
    build_router(DirectoryServer::new(
        directory,
        None,
        DirectoryHttpConfig::direct(),
    ))
}

#[cfg(test)]
pub(crate) fn router_with_registry_log(
    directory: Arc<Directory>,
    registry_log: Arc<RegistryLogClient>,
) -> Router {
    router_with_registry_log_and_config(directory, registry_log, DirectoryHttpConfig::direct())
}

#[cfg(test)]
pub(crate) fn router_with_registry_log_and_config(
    directory: Arc<Directory>,
    registry_log: Arc<RegistryLogClient>,
    config: DirectoryHttpConfig,
) -> Router {
    let state = DirectoryServer::new(directory, Some(registry_log), config);
    start_reservation_recovery(&state);
    build_router(state)
}

/// Keep recovery independent of the loft's HTTP retry. The weak reference makes the supervisor
/// stop when its router is dropped; every pass is count/time bounded and serialized with ordinary
/// mutations by the same non-queueing permit.
#[cfg(test)]
fn start_reservation_recovery(state: &Arc<DirectoryServer>) {
    // A configured mutation router without a live supervisor could strand a committed
    // reservation until another request happened to arrive. Refuse construction outside Tokio
    // instead of silently weakening crash recovery.
    let runtime = tokio::runtime::Handle::try_current()
        .expect("a registry-backed directory router requires an active Tokio runtime");
    let state = Arc::downgrade(state);
    runtime.spawn(async move {
        let mut delay = Duration::ZERO;
        loop {
            if !delay.is_zero() {
                tokio::time::sleep(delay).await;
            }
            let Some(state) = state.upgrade() else {
                return;
            };
            let recovered = run_reservation_recovery_tick(&state).await;
            delay = if recovered.is_ok() {
                RECOVERY_IDLE_INTERVAL
            } else {
                delay
                    .max(RECOVERY_IDLE_INTERVAL)
                    .saturating_mul(2)
                    .min(RECOVERY_MAX_BACKOFF)
            };
            drop(state);
        }
    });
}

async fn serve_supervised<F, Fut>(
    listener: tokio::net::TcpListener,
    state: Arc<DirectoryServer>,
    mut stop: tokio::sync::watch::Receiver<bool>,
    run_prober: F,
) -> Result<()>
where
    F: FnOnce(tokio::sync::watch::Receiver<bool>) -> Fut + Send + 'static,
    Fut: Future<Output = Result<()>> + Send + 'static,
{
    let (runtime_stop, runtime_stopped) = tokio::sync::watch::channel(false);
    let limits = state.config.limits;
    let app = build_router(Arc::clone(&state));
    let mut service_task = tokio::spawn(serve_http(listener, app, limits, runtime_stopped.clone()));
    let mut recovery_task = tokio::spawn(run_reservation_recovery(state, runtime_stopped.clone()));
    let mut prober_task = tokio::spawn(run_prober(runtime_stopped));

    enum RuntimeStop {
        Requested,
        Service(std::result::Result<Result<()>, tokio::task::JoinError>),
        Recovery(std::result::Result<Result<()>, tokio::task::JoinError>),
        Prober(std::result::Result<Result<()>, tokio::task::JoinError>),
    }

    let stopped = tokio::select! {
        () = wait_for_stop(&mut stop) => RuntimeStop::Requested,
        result = &mut service_task => RuntimeStop::Service(result),
        result = &mut recovery_task => RuntimeStop::Recovery(result),
        result = &mut prober_task => RuntimeStop::Prober(result),
    };
    let _ = runtime_stop.send(true);

    let (results, early_exit) = match stopped {
        RuntimeStop::Requested => (
            [
                drain_runtime_task(&mut service_task).await,
                drain_runtime_task(&mut recovery_task).await,
                drain_runtime_task(&mut prober_task).await,
            ],
            false,
        ),
        RuntimeStop::Service(result) => (
            [
                runtime_task_outcome(result),
                drain_runtime_task(&mut recovery_task).await,
                drain_runtime_task(&mut prober_task).await,
            ],
            true,
        ),
        RuntimeStop::Recovery(result) => (
            [
                runtime_task_outcome(result),
                drain_runtime_task(&mut service_task).await,
                drain_runtime_task(&mut prober_task).await,
            ],
            true,
        ),
        RuntimeStop::Prober(result) => (
            [
                runtime_task_outcome(result),
                drain_runtime_task(&mut service_task).await,
                drain_runtime_task(&mut recovery_task).await,
            ],
            true,
        ),
    };

    for result in results {
        result?;
    }
    if early_exit {
        return Err(DirectoryError::Unavailable);
    }
    Ok(())
}

async fn serve_http(
    listener: tokio::net::TcpListener,
    app: Router,
    limits: DirectoryLimits,
    mut stop: tokio::sync::watch::Receiver<bool>,
) -> Result<()> {
    let connections = Arc::new(tokio::sync::Semaphore::new(
        limits.max_concurrent_connections,
    ));
    let (connection_stop, _) = tokio::sync::watch::channel(false);
    let mut tasks = tokio::task::JoinSet::new();
    let header_timeout = Duration::from_millis(limits.header_timeout_ms);
    let response_timeout = Duration::from_millis(limits.response_timeout_ms);
    let connection_lifetime = Duration::from_millis(
        limits
            .header_timeout_ms
            .saturating_add(limits.request_timeout_ms)
            .saturating_add(limits.response_timeout_ms),
    );
    let max_streams = u32::try_from(limits.max_concurrent_requests).unwrap_or(u32::MAX);

    loop {
        tokio::select! {
            () = wait_for_stop(&mut stop) => break,
            completed = tasks.join_next(), if !tasks.is_empty() => {
                if completed.is_some_and(|result| result.is_err()) {
                    let _ = connection_stop.send(true);
                    tasks.abort_all();
                    while tasks.join_next().await.is_some() {}
                    return Err(DirectoryError::Unavailable);
                }
            }
            accepted = listener.accept() => {
                let (stream, peer) = accepted?;
                let permit = match Arc::clone(&connections).try_acquire_owned() {
                    Ok(permit) => permit,
                    Err(_) => {
                        drop(stream);
                        continue;
                    }
                };
                let _ = stream.set_nodelay(true);
                let app = app.clone();
                let connection_stop = connection_stop.subscribe();
                tasks.spawn(async move {
                    let _permit = permit;
                    serve_http_connection(
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
    let drain_timeout = response_timeout.min(SERVER_DRAIN_TIMEOUT);
    tokio::time::timeout(drain_timeout, async {
        while let Some(result) = tasks.join_next().await {
            if result.is_err() {
                return Err(DirectoryError::Unavailable);
            }
        }
        Ok(())
    })
    .await
    .map_err(|_| DirectoryError::Unavailable)??;
    Ok(())
}

#[allow(clippy::too_many_arguments)]
async fn serve_http_connection(
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
        .map_request(|request: axum::http::Request<Incoming>| request.map(Body::new));
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
                tracing::debug!(kind = "connection", "directory HTTP connection closed");
            }
        }
        () = wait_for_stop(&mut stop) => {
            connection.as_mut().graceful_shutdown();
            let _ = tokio::time::timeout(response_timeout, &mut connection).await;
        }
    }
}

async fn run_reservation_recovery(
    state: Arc<DirectoryServer>,
    mut stop: tokio::sync::watch::Receiver<bool>,
) -> Result<()> {
    let mut delay = Duration::ZERO;
    loop {
        if *stop.borrow() {
            return Ok(());
        }
        if !delay.is_zero() {
            tokio::select! {
                _ = tokio::time::sleep(delay) => {}
                () = wait_for_stop(&mut stop) => return Ok(()),
            }
        }

        let recovered = run_reservation_recovery_tick(&state).await;
        delay = if recovered.is_ok() {
            RECOVERY_IDLE_INTERVAL
        } else {
            delay
                .max(RECOVERY_IDLE_INTERVAL)
                .saturating_mul(2)
                .min(RECOVERY_MAX_BACKOFF)
        };
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum ReservationRecoveryTick {
    Idle,
    Busy,
    Reconciled,
}

async fn run_reservation_recovery_tick(
    state: &Arc<DirectoryServer>,
) -> Result<ReservationRecoveryTick> {
    // Check durable work before touching the sole fail-fast mutation permit. A reservation created
    // after this read is safe to leave for the next bounded tick; ordinary mutation requests also
    // reconcile it while holding that same permit. This keeps an idle supervisor from causing
    // transient 503s once per polling interval.
    let directory = Arc::clone(&state.directory);
    if !state
        .blocking(move || directory.has_pending_mutations())
        .await?
    {
        return Ok(ReservationRecoveryTick::Idle);
    }

    let permit = match state.try_mutation() {
        Ok(permit) => permit,
        Err(_) => return Ok(ReservationRecoveryTick::Busy),
    };
    let recovered = state.reconcile_reservations().await;
    drop(permit);
    recovered?;
    Ok(ReservationRecoveryTick::Reconciled)
}

async fn drain_runtime_task(task: &mut tokio::task::JoinHandle<Result<()>>) -> Result<()> {
    match tokio::time::timeout(SERVER_DRAIN_TIMEOUT, &mut *task).await {
        Ok(result) => runtime_task_outcome(result),
        Err(_) => {
            task.abort();
            let _ = task.await;
            Err(DirectoryError::Unavailable)
        }
    }
}

fn runtime_task_outcome(
    result: std::result::Result<Result<()>, tokio::task::JoinError>,
) -> Result<()> {
    result.map_err(|_| DirectoryError::Unavailable)?
}

async fn wait_for_stop(stop: &mut tokio::sync::watch::Receiver<bool>) {
    if *stop.borrow() {
        return;
    }
    while stop.changed().await.is_ok() {
        if *stop.borrow() {
            return;
        }
    }
}

fn build_router(state: Arc<DirectoryServer>) -> Router {
    let routes = read_routes()
        .route("/v1/directory/submit", post(submit))
        .route("/v1/directory/drain", post(drain));
    with_server_layers(routes, state)
}

#[cfg(feature = "test-utilities")]
fn build_read_only_router(state: Arc<DirectoryServer>) -> Router {
    with_server_layers(read_routes(), state)
}

fn read_routes() -> Router<Arc<DirectoryServer>> {
    Router::new()
        .route("/health", get(|| async { "ok" }))
        .route("/ready", get(readiness))
        .route("/directory.json", get(document))
        .route("/v1/probe", get(probes_query))
        .route("/v1/probe/measurements.json", get(probes_query))
        .route("/v1/probe/{*endpoint}", get(probes_path))
}

fn with_server_layers(routes: Router<Arc<DirectoryServer>>, state: Arc<DirectoryServer>) -> Router {
    routes
        .layer(axum::extract::DefaultBodyLimit::max(64 * 1024))
        .layer(middleware::from_fn_with_state(
            Arc::clone(&state),
            admission_middleware,
        ))
        .with_state(state)
}

async fn admission_middleware(
    State(state): State<Arc<DirectoryServer>>,
    request: Request,
    next: Next,
) -> axum::response::Response {
    let permit = match state.try_request() {
        Ok(permit) => permit,
        Err(error) => return error.into_response(),
    };
    let response = run_admitted_request(&state, request, next).await;
    response.map(|body| {
        Body::new(AdmittedBody::new(
            body,
            permit,
            Duration::from_millis(state.config.limits.response_timeout_ms),
        ))
    })
}

async fn run_admitted_request(
    state: &Arc<DirectoryServer>,
    mut request: Request,
    next: Next,
) -> axum::response::Response {
    // Liveness does not need a peer identity, but it is still public HTTP work. It shares the
    // global request budget, handler deadline, response-held permit, and connection boundary so
    // HTTP/2 streams cannot multiply a health-check bypass across every accepted connection.
    if request.uri().path() == "/health" {
        if let Err(error) = state.limiter.charge_global() {
            return error.into_response();
        }
        return match tokio::time::timeout(state.request_timeout, next.run(request)).await {
            Ok(response) => response,
            Err(_) => DirectoryError::Unavailable.into_response(),
        };
    }
    let is_readiness = request.uri().path() == "/ready";
    if is_readiness {
        if let Err(error) = state.limiter.charge_global() {
            return error.into_response();
        }
        let response = tokio::time::timeout(state.request_timeout, next.run(request)).await;
        return match response {
            Ok(response) => response,
            Err(_) => DirectoryError::Unavailable.into_response(),
        };
    }
    let connected = match request
        .extensions()
        .get::<ConnectInfo<SocketAddr>>()
        .map(|connect| connect.0)
    {
        Some(connected) => connected,
        None => return DirectoryError::Unavailable.into_response(),
    };
    let source = match request_source(&state.config, connected, request.headers()) {
        Ok(source) => source,
        Err(error) => return error.into_response(),
    };
    request
        .extensions_mut()
        .insert(EffectiveSource(source.ip()));
    let is_mutation = request.method() == Method::POST
        && matches!(
            request.uri().path(),
            "/v1/directory/submit" | "/v1/directory/drain"
        );
    if let Err(error) = state.limiter.charge_request(source.ip()).and_then(|()| {
        if is_mutation {
            state.limiter.charge_mutation(source.ip())
        } else {
            Ok(())
        }
    }) {
        return error.into_response();
    }

    let response = tokio::time::timeout(state.request_timeout, next.run(request)).await;
    match response {
        Ok(response) => response,
        Err(_) => DirectoryError::Unavailable.into_response(),
    }
}

async fn readiness(State(state): State<Arc<DirectoryServer>>) -> Result<&'static str> {
    let _mutation_permit = state.try_mutation().map_err(|_| DirectoryError::NotReady)?;
    state
        .reconcile_reservations()
        .await
        .map_err(|_| DirectoryError::NotReady)?;
    let generated_at = now();
    let directory = Arc::clone(&state.directory);
    let public_storage_required = state.public_storage_required;
    let checkpoint = state
        .blocking(move || {
            if public_storage_required {
                directory.verify_public_storage_ready()?;
            }
            directory.local_readiness(generated_at)?;
            directory.registry_checkpoint()
        })
        .await?;
    let registry = state
        .registry_log
        .as_ref()
        .ok_or(DirectoryError::NotReady)?;
    tokio::time::timeout(READINESS_TIMEOUT, registry.readiness(checkpoint.as_ref()))
        .await
        .map_err(|_| DirectoryError::NotReady)??;
    Ok("ok")
}

async fn document(
    State(state): State<Arc<DirectoryServer>>,
    source: Option<axum::Extension<EffectiveSource>>,
    headers: HeaderMap,
) -> Result<axum::response::Response> {
    // A one-minute bucket makes the signed representation and ETag stable throughout its cache
    // lifetime, so conditional reads are useful instead of racing a freshly signed timestamp.
    let freshness_at = now();
    let generated_at = freshness_at / 60 * 60;
    let directory = Arc::clone(&state.directory);
    let (encoded, etag) = state
        .blocking(move || {
            directory.prober_freshness(freshness_at)?;
            let document =
                DirectoryDocument::signed(&directory, generated_at, directory.entries()?)?;
            encode_cacheable_document(&document)
        })
        .await?;
    cacheable_document_response(
        &state,
        source.map(|axum::Extension(EffectiveSource(source))| source),
        encoded,
        etag,
        &headers,
    )
}

/// Open admission: no approval step, by design.
async fn submit(
    State(state): State<Arc<DirectoryServer>>,
    Json(req): Json<SubmitRequest>,
) -> Result<Json<SubmitResponse>> {
    // Reject malformed or forged loft input before reporting infrastructure availability. The
    // durable admission transaction repeats this verification before reserving anything.
    req.entry.registry_addition()?;
    state.limiter.charge_loft(&req.entry.pubkey)?;
    let registry = state
        .registry_log
        .as_ref()
        .ok_or(DirectoryError::Unavailable)?;
    let _mutation_permit = state.try_mutation()?;
    state.reconcile_reservations().await?;
    let submitted_at = now();
    let directory = Arc::clone(&state.directory);
    let entry = req.entry;
    let entry_to_reserve = entry.clone();
    let reserved = state
        .blocking(move || directory.reserve_add(&entry_to_reserve, submitted_at))
        .await?;
    let receipt = registry
        .append_add(
            state.directory.as_ref(),
            &reserved.mutation,
            reserved.previous_checkpoint.as_ref(),
        )
        .await?;
    let accepted_checkpoint = receipt.persisted_checkpoint()?;
    let directory = Arc::clone(&state.directory);
    state
        .blocking(move || directory.finalize_add(&entry, &accepted_checkpoint))
        .await?;
    Ok(Json(SubmitResponse {
        accepted: true,
        registry: receipt,
    }))
}

async fn drain(
    State(state): State<Arc<DirectoryServer>>,
    Json(req): Json<DrainAuthorization>,
) -> Result<Json<DrainResponse>> {
    let registry = state
        .registry_log
        .as_ref()
        .ok_or(DirectoryError::Unavailable)?;
    let directory = Arc::clone(&state.directory);
    let request_to_preflight = req.clone();
    let loft_pubkey = state
        .blocking(move || directory.preflight_drain(&request_to_preflight))
        .await?;
    state.limiter.charge_loft(&loft_pubkey)?;
    let _mutation_permit = state.try_mutation()?;
    state.reconcile_reservations().await?;
    let directory = Arc::clone(&state.directory);
    let request_to_reserve = req.clone();
    let reserved = state
        .blocking(move || directory.reserve_drain(&request_to_reserve, now()))
        .await?;
    let receipt = registry
        .append_remove(
            state.directory.as_ref(),
            &reserved.mutation,
            reserved.previous_checkpoint.as_ref(),
        )
        .await?;
    let accepted_checkpoint = receipt.persisted_checkpoint()?;
    let directory = Arc::clone(&state.directory);
    state
        .blocking(move || directory.finalize_drain(&req, &accepted_checkpoint))
        .await?;
    Ok(Json(DrainResponse {
        draining: true,
        registry: receipt,
    }))
}

/// Raw measurements, so anyone can recompute the weights we publish.
async fn probes_path(
    State(state): State<Arc<DirectoryServer>>,
    source: Option<axum::Extension<EffectiveSource>>,
    Path(endpoint): Path<String>,
    Query(query): Query<ProbePageQuery>,
) -> Result<Response> {
    signed_probes(
        state,
        source.map(|axum::Extension(EffectiveSource(source))| source),
        endpoint,
        query.cursor,
        query.limit,
    )
    .await
}

async fn probes_query(
    State(state): State<Arc<DirectoryServer>>,
    source: Option<axum::Extension<EffectiveSource>>,
    Query(query): Query<ProbeQuery>,
) -> Result<Response> {
    signed_probes(
        state,
        source.map(|axum::Extension(EffectiveSource(source))| source),
        query.endpoint,
        query.cursor,
        query.limit,
    )
    .await
}

async fn signed_probes(
    state: Arc<DirectoryServer>,
    source: Option<IpAddr>,
    endpoint: String,
    cursor: Option<u64>,
    limit: Option<usize>,
) -> Result<Response> {
    let limit = limit.unwrap_or(DEFAULT_PROBE_PAGE_SIZE);
    if limit == 0 || limit > MAX_PROBE_PAGE_SIZE {
        return Err(DirectoryError::Malformed(format!(
            "probe page limit must be between 1 and {MAX_PROBE_PAGE_SIZE}"
        )));
    }
    let generated_at = now();
    let directory = Arc::clone(&state.directory);
    let encoded = state
        .blocking(move || {
            directory.prober_freshness(generated_at)?;
            let page = directory.probe_page(&endpoint, cursor, limit, generated_at)?;
            let document =
                ProbeDocument::signed(&directory, generated_at, endpoint.clone(), cursor, page)?;
            let encoded = serde_json::to_vec(&document)?;
            if encoded.len() > MAX_DIRECTORY_DOCUMENT_BYTES {
                return Err(DirectoryError::ResponseTooLarge);
            }
            Ok(encoded)
        })
        .await?;
    state.limiter.charge_egress(
        source,
        u64::try_from(encoded.len()).map_err(|_| DirectoryError::ResponseTooLarge)?,
    )?;
    Ok(([(header::CONTENT_TYPE, "application/json")], encoded).into_response())
}

fn probe_document_payload(
    version: u32,
    generated_at: u64,
    endpoint: &str,
    pagination: ProbePagination,
    probes: &[ProbeResult],
    signing_key: &str,
) -> Result<Vec<u8>> {
    let mut payload = Vec::with_capacity(PROBE_DOCUMENT_DOMAIN.len() + endpoint.len() + 128);
    payload.extend_from_slice(PROBE_DOCUMENT_DOMAIN);
    payload.extend_from_slice(&version.to_le_bytes());
    payload.extend_from_slice(&generated_at.to_le_bytes());
    push_field(&mut payload, signing_key.as_bytes());
    push_field(&mut payload, endpoint.as_bytes());
    push_field(&mut payload, &serde_json::to_vec(&pagination)?);
    push_field(&mut payload, &serde_json::to_vec(probes)?);
    Ok(payload)
}

fn encode_cacheable_document(document: &DirectoryDocument) -> Result<(Vec<u8>, String)> {
    use sha2::{Digest, Sha256};

    let encoded = serde_json::to_vec(document)?;
    if encoded.len() > MAX_DIRECTORY_DOCUMENT_BYTES {
        return Err(DirectoryError::ResponseTooLarge);
    }
    let etag = format!("\"{}\"", hex(&Sha256::digest(&encoded)));
    Ok((encoded, etag))
}

fn cacheable_document_response(
    state: &DirectoryServer,
    source: Option<IpAddr>,
    encoded: Vec<u8>,
    etag: String,
    request_headers: &HeaderMap,
) -> Result<axum::response::Response> {
    let mut response = if request_headers
        .get(header::IF_NONE_MATCH)
        .and_then(|value| value.to_str().ok())
        == Some(etag.as_str())
    {
        StatusCode::NOT_MODIFIED.into_response()
    } else {
        state.limiter.charge_egress(
            source,
            u64::try_from(encoded.len()).map_err(|_| DirectoryError::ResponseTooLarge)?,
        )?;
        ([(header::CONTENT_TYPE, "application/json")], encoded).into_response()
    };
    response.headers_mut().insert(
        header::CACHE_CONTROL,
        HeaderValue::from_static("public, max-age=60, stale-while-revalidate=300"),
    );
    response.headers_mut().insert(
        header::ETAG,
        HeaderValue::from_str(&etag)
            .map_err(|_| DirectoryError::Malformed("invalid directory ETag".into()))?,
    );
    Ok(response)
}

fn now() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0)
}

fn request_source(
    config: &DirectoryHttpConfig,
    connected: SocketAddr,
    headers: &HeaderMap,
) -> Result<SocketAddr> {
    if connected.port() == 0 || connected.ip().is_unspecified() || connected.ip().is_multicast() {
        return Err(DirectoryError::Malformed(
            "request source is unavailable".into(),
        ));
    }
    if !config.trusted_proxies.contains(&connected.ip()) {
        return Ok(connected);
    }
    if headers.contains_key("x-forwarded-for") {
        return Err(DirectoryError::Malformed(
            "trusted proxy source is ambiguous".into(),
        ));
    }
    let forwarded = headers
        .get(header::FORWARDED)
        .and_then(|value| value.to_str().ok())
        .filter(|value| value.len() <= 4_096)
        .ok_or_else(|| DirectoryError::Malformed("trusted proxy source is missing".into()))?;
    let elements = forwarded.split(',').collect::<Vec<_>>();
    if elements.is_empty() || elements.len() > 32 {
        return Err(DirectoryError::Malformed(
            "trusted proxy source is malformed".into(),
        ));
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
                return Err(DirectoryError::Malformed(
                    "trusted proxy source is malformed".into(),
                ));
            }
            let (name, value) = parameter.trim().split_once('=').ok_or_else(|| {
                DirectoryError::Malformed("trusted proxy source is malformed".into())
            })?;
            if name.trim().eq_ignore_ascii_case("for")
                && forwarded_for.replace(value.trim()).is_some()
            {
                return Err(DirectoryError::Malformed(
                    "trusted proxy source is ambiguous".into(),
                ));
            }
        }
        current =
            parse_forwarded_socket(forwarded_for.ok_or_else(|| {
                DirectoryError::Malformed("trusted proxy source is missing".into())
            })?)?;
        consumed = true;
    }
    if !consumed {
        return Err(DirectoryError::Malformed(
            "trusted proxy source is unavailable".into(),
        ));
    }
    Ok(current)
}

fn parse_forwarded_socket(value: &str) -> Result<SocketAddr> {
    let value = if value.starts_with('"') && value.ends_with('"') && value.len() >= 2 {
        &value[1..value.len() - 1]
    } else if value.contains('"') {
        return Err(DirectoryError::Malformed(
            "trusted proxy source is malformed".into(),
        ));
    } else {
        value
    };
    if value.is_empty()
        || value
            .bytes()
            .any(|byte| byte.is_ascii_control() || byte == b'\\')
    {
        return Err(DirectoryError::Malformed(
            "trusted proxy source is malformed".into(),
        ));
    }
    let address = value.parse::<SocketAddr>().map_err(|_| {
        DirectoryError::Malformed("trusted proxy source must include an exact port".into())
    })?;
    if address.port() == 0 || address.ip().is_unspecified() || address.ip().is_multicast() {
        return Err(DirectoryError::Malformed(
            "trusted proxy source is invalid".into(),
        ));
    }
    Ok(address)
}

#[cfg(test)]
mod tests {
    use std::convert::Infallible;

    use super::*;
    use crate::entry::{LoftPolicy, LoftState};
    use ed25519_dalek::SigningKey;
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

    fn socket(value: &str) -> SocketAddr {
        value.parse().unwrap()
    }

    fn entry() -> DirectoryEntry {
        DirectoryEntry::signed(
            &SigningKey::from_bytes(&[1; 32]),
            "https://loft.example",
            None,
            10,
            30,
            LoftPolicy {
                open: true,
                pow_floor: 0,
                max_event_bytes: 65_536,
            },
            0.0,
        )
    }

    #[test]
    fn directory_document_signature_detects_tampering() {
        let directory = Directory::in_memory().unwrap();
        directory.submit(entry(), 1).unwrap();
        directory
            .set_state("https://loft.example", LoftState::Active)
            .unwrap();
        let key = directory.signing_public_key();
        let mut document =
            DirectoryDocument::signed(&directory, 2, directory.entries().unwrap()).unwrap();
        assert!(document.verify(&key).is_ok());

        document.lofts[0].state = LoftState::Degraded;
        assert!(matches!(
            document.verify(&key),
            Err(DirectoryError::BadSignature)
        ));
    }

    #[tokio::test]
    async fn unconfigured_router_cannot_bypass_logged_mutation_boundaries() {
        let directory = Arc::new(Directory::in_memory().unwrap());
        let loft_key = SigningKey::from_bytes(&[1; 32]);
        directory.submit(entry(), 1).unwrap();
        directory
            .set_state("https://loft.example", LoftState::Active)
            .unwrap();
        let before = directory.entry("https://loft.example").unwrap();

        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let base = format!("http://{}", listener.local_addr().unwrap());
        let app = router(Arc::clone(&directory));
        let server = tokio::spawn(async move {
            axum::serve(
                listener,
                app.into_make_service_with_connect_info::<SocketAddr>(),
            )
            .await
            .unwrap();
        });
        let client = reqwest::Client::new();

        let submission = client
            .post(format!("{base}/v1/directory/submit"))
            .json(&serde_json::json!({ "entry": entry() }))
            .send()
            .await
            .unwrap();
        assert_eq!(submission.status(), StatusCode::SERVICE_UNAVAILABLE);

        let authorization =
            DrainAuthorization::signed(&loft_key, "https://loft.example", now() + 3_600, 2);
        let drain = client
            .post(format!("{base}/v1/directory/drain"))
            .json(&authorization)
            .send()
            .await
            .unwrap();
        assert_eq!(drain.status(), StatusCode::SERVICE_UNAVAILABLE);

        let unchanged = directory.entry("https://loft.example").unwrap();
        assert_eq!(unchanged.state, LoftState::Active);
        assert_eq!(unchanged.drain_after, None);
        assert_eq!(
            unchanged.last_mutation_sequence,
            before.last_mutation_sequence
        );
        assert_eq!(unchanged.signature, before.signature);
        server.abort();
    }

    #[tokio::test]
    async fn stale_restart_cannot_refresh_signed_views_before_a_probe_sweep() {
        let directory = Arc::new(Directory::in_memory().unwrap());
        directory.submit(entry(), 1).unwrap();
        directory
            .set_state("https://loft.example", LoftState::Active)
            .unwrap();

        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let base = format!("http://{}", listener.local_addr().unwrap());
        let app = build_router(DirectoryServer::new(
            Arc::clone(&directory),
            None,
            DirectoryHttpConfig::direct(),
        ));
        let service = tokio::spawn(async move {
            axum::serve(
                listener,
                app.into_make_service_with_connect_info::<SocketAddr>(),
            )
            .await
            .unwrap();
        });
        let client = reqwest::Client::new();

        assert_eq!(
            client
                .get(format!("{base}/directory.json"))
                .send()
                .await
                .unwrap()
                .status(),
            StatusCode::SERVICE_UNAVAILABLE
        );
        assert_eq!(
            client
                .get(format!("{base}/v1/probe"))
                .query(&[("endpoint", "https://loft.example")])
                .send()
                .await
                .unwrap()
                .status(),
            StatusCode::SERVICE_UNAVAILABLE
        );

        directory.mark_probe_sweep(now()).unwrap();
        assert_eq!(
            client
                .get(format!("{base}/directory.json"))
                .send()
                .await
                .unwrap()
                .status(),
            StatusCode::OK
        );
        assert_eq!(
            client
                .get(format!("{base}/v1/probe"))
                .query(&[("endpoint", "https://loft.example")])
                .send()
                .await
                .unwrap()
                .status(),
            StatusCode::OK
        );
        service.abort();
    }

    #[tokio::test]
    async fn supervised_runtime_closes_http_when_the_prober_exits() {
        let directory = Arc::new(Directory::in_memory().unwrap());
        let state = DirectoryServer::new(directory, None, DirectoryHttpConfig::direct());
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let base = format!("http://{}", listener.local_addr().unwrap());
        let (_external_stop, stopped) = tokio::sync::watch::channel(false);
        let (fail_prober, prober_failure) = tokio::sync::oneshot::channel();
        let service = tokio::spawn(serve_supervised(
            listener,
            state,
            stopped,
            move |_stop| async move {
                let _ = prober_failure.await;
                Err(DirectoryError::Malformed("injected prober failure".into()))
            },
        ));

        let client = reqwest::Client::new();
        let mut healthy = false;
        for _ in 0..50 {
            if client
                .get(format!("{base}/health"))
                .send()
                .await
                .is_ok_and(|response| response.status() == StatusCode::OK)
            {
                healthy = true;
                break;
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
        assert!(healthy, "HTTP must be live before the injected prober exit");

        fail_prober.send(()).unwrap();
        let error = tokio::time::timeout(Duration::from_secs(60), service)
            .await
            .expect("supervision must terminate promptly")
            .unwrap()
            .unwrap_err();
        assert!(matches!(error, DirectoryError::Malformed(_)));
        assert!(reqwest::Client::new()
            .get(format!("{base}/health"))
            .send()
            .await
            .is_err());
    }

    #[cfg(feature = "test-utilities")]
    #[tokio::test]
    async fn loopback_test_fixture_exposes_reads_but_no_mutation_routes() {
        let directory = Arc::new(Directory::in_memory().unwrap());
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let base = format!("http://{}", listener.local_addr().unwrap());
        let (shutdown, stopped) = tokio::sync::oneshot::channel();
        let service = tokio::spawn(serve_loopback_test(listener, directory, async move {
            let _ = stopped.await;
        }));

        let client = reqwest::Client::new();
        assert_eq!(
            client
                .get(format!("{base}/directory.json"))
                .send()
                .await
                .unwrap()
                .status(),
            StatusCode::OK
        );
        assert_eq!(
            client
                .post(format!("{base}/v1/directory/submit"))
                .send()
                .await
                .unwrap()
                .status(),
            StatusCode::NOT_FOUND
        );
        shutdown.send(()).unwrap();
        service.await.unwrap().unwrap();
    }

    #[cfg(feature = "test-utilities")]
    #[tokio::test]
    async fn loopback_test_fixture_rejects_a_public_listener() {
        let listener = tokio::net::TcpListener::bind("0.0.0.0:0").await.unwrap();
        let error = serve_loopback_test(
            listener,
            Arc::new(Directory::in_memory().unwrap()),
            std::future::pending(),
        )
        .await
        .unwrap_err();
        assert!(matches!(error, DirectoryError::Malformed(_)));
    }

    #[test]
    fn raw_probe_document_signature_detects_tampering() {
        let directory = Directory::in_memory().unwrap();
        let key = directory.signing_public_key();
        let mut document = ProbeDocument::signed(
            &directory,
            2,
            "https://loft.example".into(),
            None,
            ProbePage {
                probes: vec![ProbeResult {
                    endpoint: "https://loft.example".into(),
                    at: 1,
                    reachable: true,
                    stored_and_returned: true,
                    utilization: 0.1,
                    retention_age_secs: None,
                    retention_ok: None,
                    detail: None,
                }],
                next_cursor: None,
                more: false,
            },
        )
        .unwrap();
        assert!(document.verify(&key).is_ok());

        document.cursor = Some(7);
        assert!(matches!(
            document.verify(&key),
            Err(DirectoryError::BadSignature)
        ));
        document.cursor = None;
        document.probes[0].reachable = false;
        assert!(matches!(
            document.verify(&key),
            Err(DirectoryError::BadSignature)
        ));
    }

    #[test]
    fn reservation_supervisor_refuses_to_start_without_a_runtime() {
        let state = DirectoryServer::new(
            Arc::new(Directory::in_memory().unwrap()),
            None,
            DirectoryHttpConfig::direct(),
        );
        assert!(std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            start_reservation_recovery(&state);
        }))
        .is_err());
    }

    #[tokio::test]
    async fn idle_recovery_tick_does_not_compete_for_mutation_admission() {
        let state = DirectoryServer::new(
            Arc::new(Directory::in_memory().unwrap()),
            None,
            DirectoryHttpConfig::direct(),
        );
        let held_admission = state.try_mutation().unwrap();

        assert_eq!(
            run_reservation_recovery_tick(&state).await.unwrap(),
            ReservationRecoveryTick::Idle
        );

        drop(held_admission);
        assert!(state.try_mutation().is_ok());
    }

    #[tokio::test]
    async fn blocking_work_is_bounded_and_times_out() {
        let state = DirectoryServer::new(
            Arc::new(Directory::in_memory().unwrap()),
            None,
            DirectoryHttpConfig::direct()
                .with_limits(DirectoryLimits {
                    max_blocking_operations: 1,
                    blocking_timeout_ms: 10,
                    ..DirectoryLimits::default()
                })
                .unwrap(),
        );
        let first_state = Arc::clone(&state);
        let first = tokio::spawn(async move {
            first_state
                .blocking(|| {
                    std::thread::sleep(Duration::from_millis(100));
                    Ok(())
                })
                .await
        });
        while state.blocking.available_permits() != 0 {
            tokio::task::yield_now().await;
        }

        assert!(matches!(
            state.blocking(|| Ok(())).await,
            Err(DirectoryError::Overloaded)
        ));
        assert!(matches!(
            state.reconcile_reservations().await,
            Err(DirectoryError::Overloaded)
        ));
        assert!(matches!(
            first.await.unwrap(),
            Err(DirectoryError::Overloaded)
        ));
    }

    #[test]
    fn direct_peers_cannot_spoof_forwarding_headers() {
        let connected = socket("198.51.100.7:44321");
        let mut headers = HeaderMap::new();
        headers.insert(
            header::FORWARDED,
            HeaderValue::from_static("for=203.0.113.9:9999"),
        );
        headers.insert("x-forwarded-for", HeaderValue::from_static("203.0.113.10"));
        assert_eq!(
            request_source(&DirectoryHttpConfig::direct(), connected, &headers).unwrap(),
            connected
        );
    }

    #[test]
    fn trusted_proxy_chain_preserves_the_exact_client_port() {
        let config = DirectoryHttpConfig::with_trusted_proxies(vec![
            "10.0.0.1".parse().unwrap(),
            "10.0.0.2".parse().unwrap(),
        ])
        .unwrap();
        let mut headers = HeaderMap::new();
        headers.insert(
            header::FORWARDED,
            HeaderValue::from_static("for=\"[2001:db8::42]:4567\";proto=https, for=10.0.0.1:8443"),
        );
        assert_eq!(
            request_source(&config, socket("10.0.0.2:443"), &headers).unwrap(),
            socket("[2001:db8::42]:4567")
        );
    }

    #[test]
    fn trusted_proxy_input_rejects_xff_ambiguity_and_portless_sources() {
        let config =
            DirectoryHttpConfig::with_trusted_proxies(vec!["10.0.0.1".parse().unwrap()]).unwrap();
        let connected = socket("10.0.0.1:443");
        let mut conflict = HeaderMap::new();
        conflict.insert(
            header::FORWARDED,
            HeaderValue::from_static("for=192.0.2.4:7654"),
        );
        conflict.insert("x-forwarded-for", HeaderValue::from_static("192.0.2.5"));
        assert!(request_source(&config, connected, &conflict).is_err());

        let mut portless = HeaderMap::new();
        portless.insert(header::FORWARDED, HeaderValue::from_static("for=192.0.2.4"));
        assert!(request_source(&config, connected, &portless).is_err());
    }

    #[test]
    fn request_source_and_loft_budgets_have_bounded_cardinality() {
        let limits = DirectoryLimits {
            global_requests_per_minute: 10,
            source_requests_per_minute: 1,
            source_mutations_per_minute: 1,
            loft_mutations_per_minute: 1,
            max_rate_keys: 2,
            ..DirectoryLimits::default()
        };
        let limiter = RequestLimiter::new(limits);
        let first: IpAddr = "192.0.2.1".parse().unwrap();
        let second: IpAddr = "192.0.2.2".parse().unwrap();
        assert!(limiter.charge_request(first).is_ok());
        assert!(matches!(
            limiter.charge_request(first),
            Err(DirectoryError::RateLimited)
        ));
        assert!(limiter.charge_request(second).is_ok());
        assert!(matches!(
            limiter.charge_request("192.0.2.3".parse().unwrap()),
            Err(DirectoryError::RateLimited)
        ));
        assert!(limiter.charge_mutation(first).is_ok());
        assert!(matches!(
            limiter.charge_mutation(first),
            Err(DirectoryError::RateLimited)
        ));
        assert!(limiter.charge_loft("loft-a").is_ok());
        assert!(matches!(
            limiter.charge_loft("loft-a"),
            Err(DirectoryError::RateLimited)
        ));
    }

    #[test]
    fn full_live_key_map_rejects_new_keys_without_repeated_scans() {
        let buckets = KeyBuckets::new(2);
        buckets.charge(1u32, 10).unwrap();
        buckets.charge(2u32, 10).unwrap();

        for key in 3..10_000u32 {
            assert!(matches!(
                buckets.charge(key, 10),
                Err(DirectoryError::RateLimited)
            ));
        }
        assert_eq!(buckets.cleanup_scans(), 0);

        {
            let mut state = buckets
                .state
                .lock()
                .unwrap_or_else(|error| error.into_inner());
            let expired = Instant::now() - RATE_WINDOW;
            state.buckets.get_mut(&1).unwrap().window_start = expired;
            state.next_cleanup_at = Some(expired + RATE_WINDOW);
        }
        buckets.charge(10_000u32, 10).unwrap();
        assert_eq!(buckets.cleanup_scans(), 1);
        assert_eq!(
            buckets
                .state
                .lock()
                .unwrap_or_else(|error| error.into_inner())
                .buckets
                .len(),
            2
        );
        for key in 10_001..20_000u32 {
            assert!(matches!(
                buckets.charge(key, 10),
                Err(DirectoryError::RateLimited)
            ));
        }
        assert_eq!(buckets.cleanup_scans(), 1);
    }

    #[test]
    fn response_bytes_are_charged_without_consuming_request_counts() {
        let source: IpAddr = "192.0.2.1".parse().unwrap();
        let limits = DirectoryLimits {
            global_requests_per_minute: 1,
            source_requests_per_minute: 1,
            global_response_bytes_per_minute: 100,
            source_response_bytes_per_minute: 100,
            ..DirectoryLimits::default()
        };
        let limiter = RequestLimiter::new(limits);
        limiter.charge_egress(Some(source), 50).unwrap();
        limiter.charge_request(source).unwrap();
    }

    #[test]
    fn response_bytes_fail_closed_at_global_and_source_budgets() {
        let source: IpAddr = "192.0.2.1".parse().unwrap();
        let global = RequestLimiter::new(DirectoryLimits {
            global_response_bytes_per_minute: 4,
            source_response_bytes_per_minute: 100,
            ..DirectoryLimits::default()
        });
        assert!(matches!(
            global.charge_egress(Some(source), 5),
            Err(DirectoryError::RateLimited)
        ));

        let by_source = RequestLimiter::new(DirectoryLimits {
            global_response_bytes_per_minute: 100,
            source_response_bytes_per_minute: 4,
            ..DirectoryLimits::default()
        });
        assert!(matches!(
            by_source.charge_egress(Some(source), 5),
            Err(DirectoryError::RateLimited)
        ));
    }

    #[test]
    fn request_and_mutation_concurrency_reject_instead_of_queueing() {
        let state = DirectoryServer::new(
            Arc::new(Directory::in_memory().unwrap()),
            None,
            DirectoryHttpConfig::direct()
                .with_limits(DirectoryLimits {
                    max_concurrent_requests: 1,
                    max_concurrent_mutations: 1,
                    ..DirectoryLimits::default()
                })
                .unwrap(),
        );
        let request = state.try_request().unwrap();
        assert!(matches!(
            state.try_request(),
            Err(DirectoryError::Overloaded)
        ));
        drop(request);
        assert!(state.try_request().is_ok());

        let mutation = state.try_mutation().unwrap();
        assert!(matches!(
            state.try_mutation(),
            Err(DirectoryError::Overloaded)
        ));
        drop(mutation);
        assert!(state.try_mutation().is_ok());
    }

    #[tokio::test]
    async fn request_permit_follows_response_until_body_drop() {
        let state = DirectoryServer::new(
            Arc::new(Directory::in_memory().unwrap()),
            None,
            DirectoryHttpConfig::direct()
                .with_limits(DirectoryLimits {
                    max_concurrent_requests: 1,
                    ..DirectoryLimits::default()
                })
                .unwrap(),
        );
        let app = with_server_layers(
            Router::new().route("/bounded", get(|| async { "ok" })),
            state,
        );
        let request = || {
            let mut request = axum::http::Request::builder()
                .uri("/bounded")
                .body(Body::empty())
                .unwrap();
            request
                .extensions_mut()
                .insert(ConnectInfo(socket("127.0.0.1:1234")));
            request
        };

        let first = app.clone().oneshot(request()).await.unwrap();
        assert_eq!(first.status(), StatusCode::OK);
        let rejected = app.clone().oneshot(request()).await.unwrap();
        assert_eq!(rejected.status(), StatusCode::SERVICE_UNAVAILABLE);
        drop(first);
        let admitted = app.oneshot(request()).await.unwrap();
        assert_eq!(admitted.status(), StatusCode::OK);
    }

    #[tokio::test]
    async fn health_checks_share_request_admission_through_response_drop() {
        let state = DirectoryServer::new(
            Arc::new(Directory::in_memory().unwrap()),
            None,
            DirectoryHttpConfig::direct()
                .with_limits(DirectoryLimits {
                    max_concurrent_requests: 1,
                    global_requests_per_minute: 10,
                    ..DirectoryLimits::default()
                })
                .unwrap(),
        );
        let app = with_server_layers(
            Router::new().route("/health", get(|| async { "ok" })),
            state,
        );
        let request = || {
            axum::http::Request::builder()
                .uri("/health")
                .body(Body::empty())
                .unwrap()
        };

        let first = app.clone().oneshot(request()).await.unwrap();
        assert_eq!(first.status(), StatusCode::OK);
        let rejected = app.clone().oneshot(request()).await.unwrap();
        assert_eq!(rejected.status(), StatusCode::SERVICE_UNAVAILABLE);
        drop(first);
        let admitted = app.oneshot(request()).await.unwrap();
        assert_eq!(admitted.status(), StatusCode::OK);
    }

    #[tokio::test]
    async fn health_checks_charge_the_global_request_budget() {
        let state = DirectoryServer::new(
            Arc::new(Directory::in_memory().unwrap()),
            None,
            DirectoryHttpConfig::direct()
                .with_limits(DirectoryLimits {
                    max_concurrent_requests: 2,
                    global_requests_per_minute: 1,
                    ..DirectoryLimits::default()
                })
                .unwrap(),
        );
        let app = with_server_layers(
            Router::new().route("/health", get(|| async { "ok" })),
            state,
        );
        let request = || {
            axum::http::Request::builder()
                .uri("/health")
                .body(Body::empty())
                .unwrap()
        };

        let first = app.clone().oneshot(request()).await.unwrap();
        assert_eq!(first.status(), StatusCode::OK);
        let limited = app.oneshot(request()).await.unwrap();
        assert_eq!(limited.status(), StatusCode::TOO_MANY_REQUESTS);
    }

    #[tokio::test]
    async fn incomplete_headers_are_closed_by_the_transport_deadline() {
        let limits = DirectoryLimits {
            header_timeout_ms: 100,
            request_timeout_ms: 300,
            response_timeout_ms: 200,
            ..DirectoryLimits::default()
        };
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        let (stop, stopped) = tokio::sync::watch::channel(false);
        let app = Router::new().route("/", get(|| async { "ok" }));
        let task = tokio::spawn(serve_http(listener, app, limits, stopped));

        let mut stream = tokio::net::TcpStream::connect(address).await.unwrap();
        stream
            .write_all(b"GET / HTTP/1.1\r\nHost: localhost\r\n")
            .await
            .unwrap();
        let mut byte = [0u8; 1];
        let closed = tokio::time::timeout(Duration::from_secs(60), stream.read(&mut byte))
            .await
            .expect("incomplete headers must not retain a directory connection");
        assert!(matches!(closed, Ok(0) | Err(_)));

        let _ = stop.send(true);
        task.await.unwrap().unwrap();
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn stalled_reader_is_reaped_before_it_can_release_request_admission() {
        let limits = DirectoryLimits {
            max_concurrent_connections: 4,
            max_concurrent_requests: 1,
            header_timeout_ms: 100,
            request_timeout_ms: 300,
            response_timeout_ms: 200,
            ..DirectoryLimits::default()
        };
        let state = DirectoryServer::new(
            Arc::new(Directory::in_memory().unwrap()),
            None,
            DirectoryHttpConfig::direct().with_limits(limits).unwrap(),
        );
        let first_poll = Arc::new(tokio::sync::Notify::new());
        let response_first_poll = Arc::clone(&first_poll);
        let app = with_server_layers(
            Router::new()
                .route(
                    "/large",
                    get(move || {
                        let first_poll = Arc::clone(&response_first_poll);
                        async move { Body::new(BackpressuredTestBody::new(first_poll)) }
                    }),
                )
                .route("/small", get(|| async { "ok" })),
            Arc::clone(&state),
        );
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        let (stop, stopped) = tokio::sync::watch::channel(false);
        let task = tokio::spawn(serve_http(listener, app, limits, stopped));

        let mut stalled = tokio::net::TcpStream::connect(address).await.unwrap();
        stalled
            .write_all(b"GET /large HTTP/1.1\r\nHost: localhost\r\nConnection: close\r\n\r\n")
            .await
            .unwrap();
        tokio::time::timeout(Duration::from_secs(60), first_poll.notified())
            .await
            .expect("large response body must reach the transport");
        assert_eq!(state.requests.available_permits(), 0);
        let client = reqwest::Client::new();
        let overloaded = client
            .get(format!("http://{address}/small"))
            .send()
            .await
            .unwrap();
        assert_eq!(overloaded.status(), StatusCode::SERVICE_UNAVAILABLE);
        drop(overloaded);

        tokio::time::timeout(Duration::from_secs(60), async {
            loop {
                // A transport error here is part of what is being waited for, not a failure: the
                // server is reaping the stalled reader while this polls it, and on Windows an
                // in-flight connection aborts (os error 10053) rather than answering. The property
                // under test is that admission is released *eventually*, so a dropped connection
                // is retried like any other not-yet-ready answer.
                let Ok(response) = client.get(format!("http://{address}/small")).send().await
                else {
                    tokio::time::sleep(Duration::from_millis(10)).await;
                    continue;
                };
                let status = response.status();
                drop(response);
                if status == StatusCode::OK {
                    break;
                }
                assert_eq!(status, StatusCode::SERVICE_UNAVAILABLE);
                tokio::time::sleep(Duration::from_millis(10)).await;
            }
        })
        .await
        .expect("stalled response must release request admission by its transport deadline");

        drop(stalled);
        let _ = stop.send(true);
        task.await.unwrap().unwrap();
    }

    #[test]
    fn unsafe_runtime_limits_are_rejected() {
        assert!(DirectoryHttpConfig::direct()
            .with_limits(DirectoryLimits {
                request_timeout_ms: 5 * 60 * 1_000 + 1,
                ..DirectoryLimits::default()
            })
            .is_err());
        assert!(DirectoryHttpConfig::direct()
            .with_limits(DirectoryLimits {
                max_rate_keys: 0,
                ..DirectoryLimits::default()
            })
            .is_err());
    }

    #[tokio::test]
    async fn readiness_fails_closed_without_registry_publication_proof() {
        let directory = Arc::new(Directory::in_memory().unwrap());
        directory.mark_probe_sweep(now()).unwrap();
        let state = DirectoryServer::new(directory, None, DirectoryHttpConfig::direct());
        assert!(matches!(
            readiness(State(state)).await,
            Err(DirectoryError::NotReady)
        ));
    }

    #[tokio::test]
    async fn readiness_from_a_trusted_proxy_address_needs_no_forwarded_header() {
        let directory = Arc::new(Directory::in_memory().unwrap());
        directory.mark_probe_sweep(now()).unwrap();
        let config = DirectoryHttpConfig::with_trusted_proxies(vec!["127.0.0.1".parse().unwrap()])
            .unwrap()
            .with_limits(DirectoryLimits {
                global_requests_per_minute: 1,
                ..DirectoryLimits::default()
            })
            .unwrap();
        let app = build_router(DirectoryServer::new(directory, None, config));
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let base = format!("http://{}", listener.local_addr().unwrap());
        let server = tokio::spawn(async move {
            axum::serve(
                listener,
                app.into_make_service_with_connect_info::<SocketAddr>(),
            )
            .await
            .unwrap();
        });

        let client = reqwest::Client::new();
        assert_eq!(
            client
                .get(format!("{base}/ready"))
                .send()
                .await
                .unwrap()
                .status(),
            StatusCode::SERVICE_UNAVAILABLE
        );
        assert_eq!(
            client
                .get(format!("{base}/ready"))
                .send()
                .await
                .unwrap()
                .status(),
            StatusCode::TOO_MANY_REQUESTS
        );
        server.abort();
    }

    #[tokio::test]
    async fn measurement_endpoint_pages_every_retained_record() {
        let directory = Arc::new(Directory::in_memory().unwrap());
        let current = now();
        directory.submit(entry(), current).unwrap();
        for _ in 0..501 {
            directory
                .record_probe(
                    &ProbeResult {
                        endpoint: "https://loft.example".into(),
                        at: current,
                        reachable: true,
                        stored_and_returned: true,
                        utilization: 0.1,
                        retention_age_secs: None,
                        retention_ok: None,
                        detail: None,
                    },
                    current,
                )
                .unwrap();
        }
        directory.mark_probe_sweep(current).unwrap();
        let key = directory.signing_public_key();
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let base = format!("http://{}", listener.local_addr().unwrap());
        let app = router(directory);
        let server = tokio::spawn(async move {
            axum::serve(
                listener,
                app.into_make_service_with_connect_info::<SocketAddr>(),
            )
            .await
            .unwrap();
        });
        let client = reqwest::Client::new();
        let mut cursor = None;
        let mut count = 0;
        loop {
            let mut request = client
                .get(format!("{base}/v1/probe"))
                .query(&[("endpoint", "https://loft.example"), ("limit", "200")]);
            if let Some(value) = cursor {
                request = request.query(&[("cursor", value)]);
            }
            let document: ProbeDocument = request.send().await.unwrap().json().await.unwrap();
            document.verify(&key).unwrap();
            count += document.probes.len();
            if !document.more {
                break;
            }
            cursor = document.next_cursor;
        }
        assert_eq!(count, 501);
        server.abort();
    }
}
