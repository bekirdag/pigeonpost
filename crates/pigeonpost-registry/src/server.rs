//! Bounded, cacheable registry HTTP surface.

use std::collections::{HashMap, VecDeque};
use std::convert::Infallible;
use std::future::Future;
use std::io;
use std::net::{IpAddr, SocketAddr};
use std::pin::Pin;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::task::{Context, Poll};
use std::time::{Duration, Instant};

use axum::body::{Body, Bytes};
use axum::extract::{ConnectInfo, Path, Query, Request, State};
use axum::http::header::{
    CACHE_CONTROL, CONTENT_DISPOSITION, CONTENT_TYPE, ETAG, FORWARDED, IF_NONE_MATCH,
};
use axum::http::{HeaderMap, HeaderValue, Response, StatusCode};
use axum::middleware::{self, Next};
use axum::response::IntoResponse;
use axum::routing::{get, post};
use axum::{Json, Router};
use ed25519_dalek::{Signature, VerifyingKey};
use http_body::{Body as HttpBody, Frame, SizeHint};
use hyper::body::Incoming;
use hyper_util::rt::{TokioExecutor, TokioIo, TokioTimer};
use hyper_util::server::conn::auto::Builder as ConnectionBuilder;
use hyper_util::service::TowerToHyperService;
use pigeonpost_compliance_format::{
    ComplianceKeyId, CompliancePurpose, Jurisdiction, COMPLIANCE_KEY_ID_LEN,
};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use tokio::io::{AsyncRead, AsyncWrite, ReadBuf};
use tokio::sync::{OwnedSemaphorePermit, Semaphore};
use tokio_stream::wrappers::ReceiverStream;
use tower::ServiceExt;

use crate::directory_publisher::{
    mutation_request_bytes, DirectoryMutationOperation, DIRECTORY_PUBLISHER_KEY_HEADER,
    DIRECTORY_PUBLISHER_SIGNATURE_HEADER, MAX_DIRECTORY_MUTATION_BODY_BYTES,
};
use crate::entry::{ComplianceKeyPublish, DirectoryAdd, DirectoryRemove, LogEntry};
use crate::error::{RegistryError, Result};
use crate::handle::Handle;
use crate::identity::ProofPayload;
use crate::registry::{
    hex, ComplianceKeyQuery, HandleBindingOperation, IdentityChallengeProvider,
    LoggedComplianceKey, LoggedDirectoryMutation, ProofBundle, Registry, WitnessPublicationStatus,
};
use crate::AUDIT_DUMP_SEGMENT_ENTRIES;

const READ_CACHE: &str = "public, max-age=60, must-revalidate";
const CHECKPOINT_CACHE: &str = "public, max-age=30, must-revalidate";
const COMPLIANCE_CACHE: &str = "public, max-age=0, must-revalidate";
const DUMP_CACHE: &str = "public, max-age=300, must-revalidate";
const RANGE_DUMP_CACHE: &str = "public, max-age=31536000, immutable";
const NO_STORE: &str = "no-store";
const STREAM_PAGE: u64 = 256;
const MAX_ENTRIES_PAGE: u64 = 1_000;
const MAX_FULL_DUMP_STREAMS: usize = 1;
const DUMP_STREAM_IDLE_TIMEOUT: Duration = Duration::from_secs(10);
const DUMP_STREAM_TOTAL_TIMEOUT: Duration = Duration::from_secs(120);
const MAX_AUDIT_DUMP_LINE_BYTES: u64 = 64 * 1024;
const MAX_AUDIT_DUMP_RESPONSE_BYTES: u64 = 32 * 1024 * 1024;
/// Non-streaming endpoints are deliberately much smaller in normal operation. This ceiling keeps
/// a malformed or unexpectedly large convenience projection from becoming unaccounted egress.
const MAX_GENERAL_RESPONSE_BYTES: u64 = 64 * 1024 * 1024;
const RATE_WINDOW: Duration = Duration::from_secs(60);
const MAX_SOURCE_EXPIRATIONS_PER_CHARGE: usize = 256;
const DEFAULT_REQUEST_TIMEOUT: Duration = Duration::from_secs(30);
const MAX_REQUEST_TIMEOUT: Duration = Duration::from_secs(5 * 60);
const SERVER_DRAIN_TIMEOUT: Duration = Duration::from_secs(30);
pub const MAX_REGISTRY_CONCURRENT_REQUESTS: usize = 4_096;
pub const MAX_REGISTRY_CONCURRENT_CONNECTIONS: usize = 4_096;
pub const MAX_REGISTRY_BLOCKING_OPERATIONS: usize = 256;
pub const MAX_REGISTRY_DUMP_STREAMS: usize = 64;
pub const MAX_REGISTRY_BLOCKING_TIMEOUT_MS: u64 = 5 * 60 * 1_000;
pub const MAX_REGISTRY_HEADER_TIMEOUT_MS: u64 = 5 * 60 * 1_000;
pub const MAX_REGISTRY_GLOBAL_REQUESTS_PER_MINUTE: u32 = 10_000_000;
pub const MAX_REGISTRY_RESPONSE_BYTES_PER_MINUTE: u64 = 1 << 40;
pub const MAX_REGISTRY_SOURCE_REQUESTS_PER_MINUTE: u32 = 1_000_000;
pub const MAX_REGISTRY_SOURCE_KEYS: usize = 65_536;

#[derive(Debug, Clone, Copy)]
struct DedicatedResponseEgress;

struct ResponsePermitLease {
    permit: Mutex<Option<OwnedSemaphorePermit>>,
}

impl ResponsePermitLease {
    fn new(permit: OwnedSemaphorePermit) -> Arc<Self> {
        Arc::new(Self {
            permit: Mutex::new(Some(permit)),
        })
    }

    fn release(&self) {
        self.permit
            .lock()
            .unwrap_or_else(|error| error.into_inner())
            .take();
    }
}

/// Couples an admission lane to downstream body ownership rather than handler completion.
///
/// The absolute deadline is polled as part of the body itself, so EOF/drop destroys its timer and
/// leaves no per-response task alive. The connection lifetime remains the backstop when a peer
/// stops causing body polls. Full mirror dumps intentionally omit an absolute deadline; their
/// dedicated one-stream lane and transport idle-progress deadline are the SDS-defined bound.
struct PermittedResponseBody {
    inner: Body,
    lease: Arc<ResponsePermitLease>,
    response_bytes: Option<Arc<ResponseByteLimiter>>,
    deadline: Option<Pin<Box<tokio::time::Sleep>>>,
    remaining_bytes: Option<u64>,
    done: bool,
}

impl PermittedResponseBody {
    fn new(
        inner: Body,
        permit: OwnedSemaphorePermit,
        deadline: Option<tokio::time::Instant>,
        max_bytes: Option<u64>,
        response_bytes: Option<Arc<ResponseByteLimiter>>,
    ) -> Self {
        let lease = ResponsePermitLease::new(permit);
        Self {
            inner,
            lease,
            response_bytes,
            deadline: deadline.map(|deadline| Box::pin(tokio::time::sleep_until(deadline))),
            remaining_bytes: max_bytes,
            done: false,
        }
    }

    fn fail(
        &mut self,
        reason: &'static str,
    ) -> Poll<Option<std::result::Result<Frame<Bytes>, axum::Error>>> {
        self.done = true;
        self.inner = Body::empty();
        self.lease.release();
        Poll::Ready(Some(Err(axum::Error::new(std::io::Error::other(reason)))))
    }
}

impl HttpBody for PermittedResponseBody {
    type Data = Bytes;
    type Error = axum::Error;

    fn poll_frame(
        mut self: Pin<&mut Self>,
        cx: &mut Context<'_>,
    ) -> Poll<Option<std::result::Result<Frame<Self::Data>, Self::Error>>> {
        let this = &mut *self;
        if this.done {
            return Poll::Ready(None);
        }
        if this
            .deadline
            .as_mut()
            .is_some_and(|deadline| deadline.as_mut().poll(cx).is_ready())
        {
            return this.fail("registry response body exceeded its deadline");
        }
        match Pin::new(&mut this.inner).poll_frame(cx) {
            Poll::Ready(Some(Ok(frame))) => {
                if let Some(data) = frame.data_ref() {
                    let bytes = u64::try_from(data.len()).unwrap_or(u64::MAX);
                    if let Some(remaining) = &mut this.remaining_bytes {
                        let Some(next) = remaining.checked_sub(bytes) else {
                            return this.fail("registry response body exceeded its byte budget");
                        };
                        *remaining = next;
                    }
                    if this
                        .response_bytes
                        .as_ref()
                        .is_some_and(|limiter| limiter.charge(bytes).is_err())
                    {
                        return this.fail("registry response byte rate exhausted");
                    }
                }
                Poll::Ready(Some(Ok(frame)))
            }
            Poll::Ready(Some(Err(error))) => {
                this.done = true;
                this.lease.release();
                Poll::Ready(Some(Err(error)))
            }
            Poll::Ready(None) => {
                this.done = true;
                this.lease.release();
                Poll::Ready(None)
            }
            Poll::Pending => Poll::Pending,
        }
    }

    fn size_hint(&self) -> SizeHint {
        if self.done {
            SizeHint::with_exact(0)
        } else {
            self.inner.size_hint()
        }
    }

    fn is_end_stream(&self) -> bool {
        // Force Hyper to poll the wrapper through EOF so an already-empty inner body cannot make
        // the admission lease observable past logical response completion.
        self.done
    }
}

fn hold_response_permit(
    response: Response<Body>,
    permit: OwnedSemaphorePermit,
    deadline: Option<tokio::time::Instant>,
    max_bytes: Option<u64>,
) -> Response<Body> {
    hold_response_permit_with_egress(response, permit, deadline, max_bytes, None)
}

fn hold_response_permit_with_egress(
    mut response: Response<Body>,
    permit: OwnedSemaphorePermit,
    deadline: Option<tokio::time::Instant>,
    max_bytes: Option<u64>,
    response_bytes: Option<Arc<ResponseByteLimiter>>,
) -> Response<Body> {
    let body = std::mem::replace(response.body_mut(), Body::empty());
    *response.body_mut() = Body::new(PermittedResponseBody::new(
        body,
        permit,
        deadline,
        max_bytes,
        response_bytes,
    ));
    response
}

/// Enforces write-idle progress below Hyper's body polling boundary. A body producer can finish a
/// short/final chunk while the socket remains blocked; only successful transport writes reset this
/// watchdog, so that case cannot retain a dump lane forever.
struct IdleWriteIo<T> {
    inner: T,
    idle_timeout: Duration,
    deadline: Pin<Box<tokio::time::Sleep>>,
}

impl<T> IdleWriteIo<T> {
    fn new(inner: T, idle_timeout: Duration) -> Self {
        Self {
            inner,
            idle_timeout,
            deadline: Box::pin(tokio::time::sleep(idle_timeout)),
        }
    }

    fn record_progress(&mut self) {
        self.deadline
            .as_mut()
            .reset(tokio::time::Instant::now() + self.idle_timeout);
    }

    fn pending_or_timed_out<R>(&mut self, cx: &mut Context<'_>) -> Poll<io::Result<R>> {
        if self.deadline.as_mut().poll(cx).is_ready() {
            Poll::Ready(Err(io::Error::new(
                io::ErrorKind::TimedOut,
                "registry response transport made no write progress",
            )))
        } else {
            Poll::Pending
        }
    }
}

impl<T: AsyncRead + Unpin> AsyncRead for IdleWriteIo<T> {
    fn poll_read(
        mut self: Pin<&mut Self>,
        cx: &mut Context<'_>,
        buffer: &mut ReadBuf<'_>,
    ) -> Poll<io::Result<()>> {
        Pin::new(&mut self.inner).poll_read(cx, buffer)
    }
}

impl<T: AsyncWrite + Unpin> AsyncWrite for IdleWriteIo<T> {
    fn poll_write(
        mut self: Pin<&mut Self>,
        cx: &mut Context<'_>,
        buffer: &[u8],
    ) -> Poll<io::Result<usize>> {
        match Pin::new(&mut self.inner).poll_write(cx, buffer) {
            Poll::Ready(Ok(written)) => {
                if written > 0 {
                    self.record_progress();
                }
                Poll::Ready(Ok(written))
            }
            Poll::Ready(Err(error)) => Poll::Ready(Err(error)),
            Poll::Pending => self.pending_or_timed_out(cx),
        }
    }

    fn poll_flush(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<io::Result<()>> {
        match Pin::new(&mut self.inner).poll_flush(cx) {
            Poll::Pending => self.pending_or_timed_out(cx),
            ready => ready,
        }
    }

    fn poll_shutdown(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<io::Result<()>> {
        match Pin::new(&mut self.inner).poll_shutdown(cx) {
            Poll::Pending => self.pending_or_timed_out(cx),
            ready => ready,
        }
    }

    fn is_write_vectored(&self) -> bool {
        self.inner.is_write_vectored()
    }

    fn poll_write_vectored(
        mut self: Pin<&mut Self>,
        cx: &mut Context<'_>,
        buffers: &[io::IoSlice<'_>],
    ) -> Poll<io::Result<usize>> {
        match Pin::new(&mut self.inner).poll_write_vectored(cx, buffers) {
            Poll::Ready(Ok(written)) => {
                if written > 0 {
                    self.record_progress();
                }
                Poll::Ready(Ok(written))
            }
            Poll::Ready(Err(error)) => Poll::Ready(Err(error)),
            Poll::Pending => self.pending_or_timed_out(cx),
        }
    }
}

#[derive(Debug, Clone, Copy)]
struct RegistryConnection(SocketAddr);

/// Suspends the ordinary absolute connection deadline only while a query-free mirror dump owns a
/// response body. When that body reaches EOF/error/drop, the HTTP/1.1 connection is closed so the
/// no-cutoff exception cannot turn into an unbounded keep-alive exemption.
struct ConnectionLifetimeState {
    active_full_dumps: AtomicUsize,
    close_after_full_dump: AtomicBool,
    generation: tokio::sync::watch::Sender<u64>,
}

impl ConnectionLifetimeState {
    fn new() -> (Arc<Self>, tokio::sync::watch::Receiver<u64>) {
        let (generation, changed) = tokio::sync::watch::channel(0);
        (
            Arc::new(Self {
                active_full_dumps: AtomicUsize::new(0),
                close_after_full_dump: AtomicBool::new(false),
                generation,
            }),
            changed,
        )
    }

    fn begin_full_dump(self: &Arc<Self>) -> FullDumpConnectionGuard {
        self.active_full_dumps.fetch_add(1, Ordering::AcqRel);
        self.close_after_full_dump.store(true, Ordering::Release);
        self.signal();
        FullDumpConnectionGuard {
            state: Arc::clone(self),
            released: false,
        }
    }

    fn signal(&self) {
        self.generation.send_modify(|generation| {
            *generation = generation.wrapping_add(1);
        });
    }

    fn full_dump_active(&self) -> bool {
        self.active_full_dumps.load(Ordering::Acquire) != 0
    }

    fn close_requested(&self) -> bool {
        self.close_after_full_dump.load(Ordering::Acquire) && !self.full_dump_active()
    }
}

struct FullDumpConnectionGuard {
    state: Arc<ConnectionLifetimeState>,
    released: bool,
}

impl FullDumpConnectionGuard {
    fn release(&mut self) {
        if self.released {
            return;
        }
        self.released = true;
        self.state.active_full_dumps.fetch_sub(1, Ordering::AcqRel);
        self.state.signal();
    }
}

impl Drop for FullDumpConnectionGuard {
    fn drop(&mut self) {
        self.release();
    }
}

struct FullDumpConnectionBody {
    inner: Body,
    guard: Option<FullDumpConnectionGuard>,
}

impl HttpBody for FullDumpConnectionBody {
    type Data = Bytes;
    type Error = axum::Error;

    fn poll_frame(
        mut self: Pin<&mut Self>,
        cx: &mut Context<'_>,
    ) -> Poll<Option<std::result::Result<Frame<Self::Data>, Self::Error>>> {
        match Pin::new(&mut self.inner).poll_frame(cx) {
            Poll::Ready(Some(Ok(frame))) => Poll::Ready(Some(Ok(frame))),
            Poll::Ready(Some(Err(error))) => {
                self.guard.take();
                Poll::Ready(Some(Err(error)))
            }
            Poll::Ready(None) => {
                self.guard.take();
                Poll::Ready(None)
            }
            Poll::Pending => Poll::Pending,
        }
    }

    fn size_hint(&self) -> SizeHint {
        self.inner.size_hint()
    }

    fn is_end_stream(&self) -> bool {
        self.guard.is_none()
    }
}

fn hold_full_dump_connection(
    mut response: Response<Body>,
    guard: FullDumpConnectionGuard,
) -> Response<Body> {
    let body = std::mem::replace(response.body_mut(), Body::empty());
    *response.body_mut() = Body::new(FullDumpConnectionBody {
        inner: body,
        guard: Some(guard),
    });
    response
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct RegistryLimits {
    pub max_concurrent_connections: usize,
    pub max_concurrent_requests: usize,
    pub max_blocking_operations: usize,
    pub max_dump_streams: usize,
    pub blocking_timeout_ms: u64,
    pub header_timeout_ms: u64,
    pub global_requests_per_minute: u32,
    pub global_response_bytes_per_minute: u64,
    pub source_challenges_per_minute: u32,
    pub source_bindings_per_minute: u32,
    pub max_source_keys: usize,
}

impl Default for RegistryLimits {
    fn default() -> Self {
        Self {
            max_concurrent_connections: 128,
            max_concurrent_requests: 64,
            max_blocking_operations: 8,
            max_dump_streams: 4,
            blocking_timeout_ms: 30_000,
            header_timeout_ms: 5_000,
            global_requests_per_minute: 6_000,
            global_response_bytes_per_minute: 256 * 1024 * 1024,
            source_challenges_per_minute: 20,
            // A witnessed directory mutation normally uses one POST to commit the pending leaf and
            // one unchanged POST to collect its final inclusion receipt. Keep the registry's
            // source budget aligned with the directory's default 20 mutations per source.
            source_bindings_per_minute: 40,
            max_source_keys: 4_096,
        }
    }
}

#[derive(Debug, Clone)]
pub struct RegistryHttpConfig {
    trusted_proxies: Vec<IpAddr>,
    directory_publishers: Vec<VerifyingKey>,
    limits: RegistryLimits,
    request_timeout: Duration,
}

impl Default for RegistryHttpConfig {
    fn default() -> Self {
        Self {
            trusted_proxies: Vec::new(),
            directory_publishers: Vec::new(),
            limits: RegistryLimits::default(),
            request_timeout: DEFAULT_REQUEST_TIMEOUT,
        }
    }
}

impl RegistryHttpConfig {
    pub fn direct() -> Self {
        Self::default()
    }

    pub fn with_trusted_proxies(trusted_proxies: Vec<IpAddr>) -> Result<Self> {
        if trusted_proxies.len() > 64
            || trusted_proxies
                .iter()
                .any(|ip| ip.is_unspecified() || ip.is_multicast())
        {
            return Err(RegistryError::InvalidConfiguration(
                "trusted proxy list is invalid".into(),
            ));
        }
        Ok(Self {
            trusted_proxies,
            directory_publishers: Vec::new(),
            limits: RegistryLimits::default(),
            request_timeout: DEFAULT_REQUEST_TIMEOUT,
        })
    }

    pub fn with_limits(mut self, limits: RegistryLimits) -> Result<Self> {
        validate_limits(limits)?;
        self.limits = limits;
        Ok(self)
    }

    /// Pin the directory identities permitted to publish already-admitted loft mutations.
    pub fn with_directory_publishers(
        mut self,
        directory_publishers: Vec<VerifyingKey>,
    ) -> Result<Self> {
        validate_directory_publishers(&directory_publishers)?;
        self.directory_publishers = directory_publishers;
        Ok(self)
    }

    /// Bound the complete HTTP request, including body extraction and handler work.
    pub fn with_request_timeout(mut self, request_timeout: Duration) -> Result<Self> {
        if request_timeout.is_zero() || request_timeout > MAX_REQUEST_TIMEOUT {
            return Err(RegistryError::InvalidConfiguration(
                "registry request timeout is outside the supported bounds".into(),
            ));
        }
        self.request_timeout = request_timeout;
        Ok(self)
    }
}

#[derive(Debug, Clone, Copy)]
struct RateBucket {
    requests: u32,
    window_start: Instant,
}

impl RateBucket {
    fn fresh(now: Instant) -> Self {
        Self {
            requests: 0,
            window_start: now,
        }
    }

    fn charge(&mut self, now: Instant, limit: u32) -> Result<()> {
        if now.duration_since(self.window_start) >= RATE_WINDOW {
            *self = Self::fresh(now);
        }
        if limit != 0 && self.requests >= limit {
            return Err(RegistryError::RateLimited);
        }
        self.requests = self.requests.saturating_add(1);
        Ok(())
    }
}

#[derive(Debug, Clone, Copy)]
struct ByteRateBucket {
    bytes: u64,
    window_start: Instant,
}

impl ByteRateBucket {
    fn fresh(now: Instant) -> Self {
        Self {
            bytes: 0,
            window_start: now,
        }
    }

    fn charge(&mut self, now: Instant, bytes: u64, limit: u64) -> Result<()> {
        if now.duration_since(self.window_start) >= RATE_WINDOW {
            *self = Self::fresh(now);
        }
        let next = self
            .bytes
            .checked_add(bytes)
            .ok_or(RegistryError::RateLimited)?;
        if next > limit {
            return Err(RegistryError::RateLimited);
        }
        self.bytes = next;
        Ok(())
    }
}

#[derive(Debug)]
struct ResponseByteLimiter {
    state: Mutex<ByteRateBucket>,
    limit: u64,
}

impl ResponseByteLimiter {
    fn new(limit: u64) -> Self {
        Self {
            state: Mutex::new(ByteRateBucket::fresh(Instant::now())),
            limit,
        }
    }

    fn charge(&self, bytes: u64) -> Result<()> {
        self.charge_at(bytes, Instant::now())
    }

    fn charge_at(&self, bytes: u64, now: Instant) -> Result<()> {
        self.state
            .lock()
            .unwrap_or_else(|error| error.into_inner())
            .charge(now, bytes, self.limit)
    }
}

#[derive(Debug)]
struct SourceBuckets {
    state: Mutex<SourceBucketState>,
    max_keys: usize,
}

#[derive(Debug, Default)]
struct SourceBucketState {
    buckets: HashMap<IpAddr, RateBucket>,
    expirations: VecDeque<(Instant, IpAddr)>,
    #[cfg(test)]
    cleanup_passes: usize,
}

impl SourceBucketState {
    fn schedule(&mut self, source: IpAddr, window_start: Instant) {
        let expires = window_start
            .checked_add(RATE_WINDOW)
            .unwrap_or(window_start);
        self.expirations.push_back((expires, source));
    }

    fn reclaim_expired(&mut self, now: Instant) {
        if self
            .expirations
            .front()
            .is_none_or(|(expires, _)| *expires > now)
        {
            return;
        }
        #[cfg(test)]
        {
            self.cleanup_passes = self.cleanup_passes.saturating_add(1);
        }
        for _ in 0..MAX_SOURCE_EXPIRATIONS_PER_CHARGE {
            let Some((expires, source)) = self.expirations.front().copied() else {
                break;
            };
            if expires > now {
                break;
            }
            self.expirations.pop_front();
            let remove = self.buckets.get(&source).is_some_and(|bucket| {
                bucket
                    .window_start
                    .checked_add(RATE_WINDOW)
                    .is_none_or(|current_expiry| current_expiry <= now && current_expiry == expires)
            });
            if remove {
                self.buckets.remove(&source);
            }
        }
    }
}

impl SourceBuckets {
    fn new(max_keys: usize) -> Self {
        Self {
            state: Mutex::new(SourceBucketState::default()),
            max_keys,
        }
    }

    fn charge(&self, source: IpAddr, limit: u32) -> Result<()> {
        self.charge_at(source, limit, Instant::now())
    }

    fn charge_at(&self, source: IpAddr, limit: u32, now: Instant) -> Result<()> {
        if limit == 0 {
            return Ok(());
        }
        let mut state = self.state.lock().unwrap_or_else(|error| error.into_inner());
        state.reclaim_expired(now);
        if !state.buckets.contains_key(&source) && state.buckets.len() >= self.max_keys {
            return Err(RegistryError::RateLimited);
        }
        if let Some(bucket) = state.buckets.get_mut(&source) {
            let previous_start = bucket.window_start;
            bucket.charge(now, limit)?;
            if bucket.window_start != previous_start {
                let window_start = bucket.window_start;
                state.schedule(source, window_start);
            }
            return Ok(());
        }
        let mut bucket = RateBucket::fresh(now);
        bucket.charge(now, limit)?;
        state.buckets.insert(source, bucket);
        state.schedule(source, now);
        Ok(())
    }

    #[cfg(test)]
    fn len(&self) -> usize {
        self.state
            .lock()
            .unwrap_or_else(|error| error.into_inner())
            .buckets
            .len()
    }

    #[cfg(test)]
    fn cleanup_passes(&self) -> usize {
        self.state
            .lock()
            .unwrap_or_else(|error| error.into_inner())
            .cleanup_passes
    }

    #[cfg(test)]
    fn queued_expirations(&self) -> usize {
        self.state
            .lock()
            .unwrap_or_else(|error| error.into_inner())
            .expirations
            .len()
    }
}

#[derive(Debug)]
struct RequestLimiter {
    global: Mutex<RateBucket>,
    global_limit: u32,
    response_bytes: Arc<ResponseByteLimiter>,
    challenges: SourceBuckets,
    challenge_limit: u32,
    bindings: SourceBuckets,
    binding_limit: u32,
}

impl RequestLimiter {
    fn new(limits: RegistryLimits) -> Self {
        Self {
            global: Mutex::new(RateBucket::fresh(Instant::now())),
            global_limit: limits.global_requests_per_minute,
            response_bytes: Arc::new(ResponseByteLimiter::new(
                limits.global_response_bytes_per_minute,
            )),
            challenges: SourceBuckets::new(limits.max_source_keys),
            challenge_limit: limits.source_challenges_per_minute,
            bindings: SourceBuckets::new(limits.max_source_keys),
            binding_limit: limits.source_bindings_per_minute,
        }
    }

    fn charge_global(&self) -> Result<()> {
        self.global
            .lock()
            .unwrap_or_else(|error| error.into_inner())
            .charge(Instant::now(), self.global_limit)
    }

    fn charge_response_bytes(&self, bytes: u64) -> Result<()> {
        self.charge_response_bytes_at(bytes, Instant::now())
    }

    fn charge_response_bytes_at(&self, bytes: u64, now: Instant) -> Result<()> {
        self.response_bytes.charge_at(bytes, now)
    }

    fn response_byte_limiter(&self) -> Arc<ResponseByteLimiter> {
        Arc::clone(&self.response_bytes)
    }

    fn charge_challenge(&self, source: IpAddr) -> Result<()> {
        self.challenges.charge(source, self.challenge_limit)
    }

    fn charge_binding(&self, source: IpAddr) -> Result<()> {
        self.bindings.charge(source, self.binding_limit)
    }
}

struct RegistryServer {
    registry: Arc<Registry>,
    config: Arc<RegistryHttpConfig>,
    requests: Arc<Semaphore>,
    blocking: Arc<Semaphore>,
    range_streams: Arc<Semaphore>,
    full_streams: Arc<Semaphore>,
    limiter: RequestLimiter,
    request_timeout: Duration,
    #[cfg(test)]
    serialization_test_barrier: Mutex<Option<Arc<SerializationTestBarrier>>>,
}

#[cfg(test)]
struct SerializationTestBarrier {
    reached: Mutex<Option<tokio::sync::oneshot::Sender<()>>>,
    release: Mutex<std::sync::mpsc::Receiver<()>>,
}

#[cfg(test)]
impl SerializationTestBarrier {
    fn new() -> (
        Arc<Self>,
        tokio::sync::oneshot::Receiver<()>,
        std::sync::mpsc::Sender<()>,
    ) {
        let (reached_tx, reached_rx) = tokio::sync::oneshot::channel();
        let (release_tx, release_rx) = std::sync::mpsc::channel();
        (
            Arc::new(Self {
                reached: Mutex::new(Some(reached_tx)),
                release: Mutex::new(release_rx),
            }),
            reached_rx,
            release_tx,
        )
    }

    fn wait(&self) {
        if let Some(reached) = self
            .reached
            .lock()
            .unwrap_or_else(|error| error.into_inner())
            .take()
        {
            let _ = reached.send(());
        }
        let _ = self
            .release
            .lock()
            .unwrap_or_else(|error| error.into_inner())
            .recv_timeout(Duration::from_secs(1));
    }
}

impl RegistryServer {
    fn new(registry: Arc<Registry>, config: RegistryHttpConfig) -> Result<Self> {
        // Validate before constructing Tokio semaphores: values above Tokio's internal permit
        // ceiling panic, and merely parsing hostile/operator-controlled config must never abort the
        // process.
        validate_limits(config.limits)?;
        Ok(Self {
            registry,
            requests: Arc::new(Semaphore::new(config.limits.max_concurrent_requests)),
            blocking: Arc::new(Semaphore::new(config.limits.max_blocking_operations)),
            range_streams: Arc::new(Semaphore::new(config.limits.max_dump_streams)),
            full_streams: Arc::new(Semaphore::new(MAX_FULL_DUMP_STREAMS)),
            limiter: RequestLimiter::new(config.limits),
            request_timeout: config.request_timeout,
            config: Arc::new(config),
            #[cfg(test)]
            serialization_test_barrier: Mutex::new(None),
        })
    }

    #[cfg(test)]
    fn install_serialization_test_barrier(&self, barrier: Arc<SerializationTestBarrier>) {
        *self
            .serialization_test_barrier
            .lock()
            .unwrap_or_else(|error| error.into_inner()) = Some(barrier);
    }

    #[cfg(test)]
    fn take_serialization_test_barrier(&self) -> Option<Arc<SerializationTestBarrier>> {
        self.serialization_test_barrier
            .lock()
            .unwrap_or_else(|error| error.into_inner())
            .take()
    }

    fn try_enter(&self) -> Result<OwnedSemaphorePermit> {
        Arc::clone(&self.requests)
            .try_acquire_owned()
            .map_err(|_| RegistryError::Overloaded)
    }

    fn try_stream(&self) -> Result<OwnedSemaphorePermit> {
        Arc::clone(&self.range_streams)
            .try_acquire_owned()
            .map_err(|_| RegistryError::Overloaded)
    }

    fn try_full_stream(&self) -> Result<OwnedSemaphorePermit> {
        Arc::clone(&self.full_streams)
            .try_acquire_owned()
            .map_err(|_| RegistryError::Overloaded)
    }

    async fn blocking<T, F>(&self, operation: F) -> Result<T>
    where
        T: Send + 'static,
        F: FnOnce(Arc<Registry>) -> Result<T> + Send + 'static,
    {
        let permit = Arc::clone(&self.blocking)
            .try_acquire_owned()
            .map_err(|_| RegistryError::Overloaded)?;
        let registry = Arc::clone(&self.registry);
        let task = tokio::task::spawn_blocking(move || {
            let _permit = permit;
            operation(registry)
        });
        tokio::time::timeout(
            Duration::from_millis(self.config.limits.blocking_timeout_ms),
            task,
        )
        .await
        .map_err(|_| RegistryError::Overloaded)?
        .map_err(|_| RegistryError::Overloaded)?
    }
}

fn validate_limits(limits: RegistryLimits) -> Result<()> {
    if limits.max_concurrent_connections == 0
        || limits.max_concurrent_requests == 0
        || limits.max_blocking_operations == 0
        || limits.max_dump_streams == 0
        || limits.blocking_timeout_ms == 0
        || limits.header_timeout_ms == 0
        || limits.global_requests_per_minute == 0
        || limits.global_response_bytes_per_minute == 0
        || limits.source_challenges_per_minute == 0
        || limits.source_bindings_per_minute == 0
        || limits.max_source_keys == 0
    {
        return Err(RegistryError::InvalidConfiguration(
            "registry HTTP limits must be nonzero".into(),
        ));
    }
    if limits.max_concurrent_connections > MAX_REGISTRY_CONCURRENT_CONNECTIONS
        || limits.max_concurrent_requests > MAX_REGISTRY_CONCURRENT_REQUESTS
        || limits.max_blocking_operations > MAX_REGISTRY_BLOCKING_OPERATIONS
        || limits.max_dump_streams > MAX_REGISTRY_DUMP_STREAMS
        || limits.blocking_timeout_ms > MAX_REGISTRY_BLOCKING_TIMEOUT_MS
        || limits.header_timeout_ms > MAX_REGISTRY_HEADER_TIMEOUT_MS
        || limits.global_requests_per_minute > MAX_REGISTRY_GLOBAL_REQUESTS_PER_MINUTE
        || limits.global_response_bytes_per_minute > MAX_REGISTRY_RESPONSE_BYTES_PER_MINUTE
        || limits.source_challenges_per_minute > MAX_REGISTRY_SOURCE_REQUESTS_PER_MINUTE
        || limits.source_bindings_per_minute > MAX_REGISTRY_SOURCE_REQUESTS_PER_MINUTE
        || limits.max_source_keys > MAX_REGISTRY_SOURCE_KEYS
    {
        return Err(RegistryError::InvalidConfiguration(
            "registry HTTP limits exceed audited product maxima".into(),
        ));
    }
    Ok(())
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RegisterRequest {
    pub handle: String,
    /// Hex-encoded Ed25519 public key being bound.
    pub pubkey: String,
    /// Signature by that key over the claim payload.
    pub signature: String,
    pub proof: ProofPayload,
}

#[derive(Debug, Serialize)]
pub struct RegisterResponse {
    pub handle: String,
    pub log_index: u64,
    pub appended: bool,
    pub inclusion_proof: InclusionProof,
}

#[derive(Debug, Serialize)]
pub struct ResolveResponse {
    pub handle: String,
    pub pubkey: String,
    pub log_index: u64,
    pub inclusion_proof: InclusionProof,
}

#[derive(Debug, Serialize)]
pub struct DirectoryMutationResponse {
    /// Exact immutable leaf, including the original loft signature.
    pub entry: LogEntry,
    pub log_index: u64,
    pub appended: bool,
    pub inclusion_proof: InclusionProof,
}

#[derive(Debug, Clone, Serialize)]
pub struct InclusionProof {
    pub tree_size: u64,
    pub root: String,
    pub path: Vec<String>,
    /// C2SP signed note. Callers still pin the checkpoint key; this is not self-authenticating.
    pub checkpoint: String,
}

impl From<ProofBundle> for InclusionProof {
    fn from(value: ProofBundle) -> Self {
        Self {
            tree_size: value.size,
            root: hex(&value.root),
            path: value.path.iter().map(|hash| hex(hash)).collect(),
            checkpoint: value.checkpoint,
        }
    }
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ConsistencyQuery {
    pub from: u64,
    pub to: u64,
}

#[derive(Debug, Serialize)]
pub struct ConsistencyResponse {
    pub from: u64,
    pub to: u64,
    pub root: String,
    pub path: Vec<String>,
}

/// Operational publication state. This intentionally exposes counts and freshness only; pending
/// entries remain inaccessible until a witness quorum has cosigned their checkpoint.
#[derive(Debug, Serialize)]
pub struct LogStatusResponse {
    pub ready: bool,
    pub committed_size: u64,
    pub published_size: u64,
    pub lag_entries: u64,
    pub witnessed_at: Option<u64>,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct EntriesQuery {
    #[serde(default)]
    pub from: u64,
    /// Exclusive end. At most 1,000 entries are returned.
    pub to: Option<u64>,
}

#[derive(Debug, Serialize)]
pub struct EntriesResponse {
    pub from: u64,
    pub to: u64,
    pub tree_size: u64,
    pub root: String,
    pub checkpoint: String,
    pub entries: Vec<LogEntry>,
}

#[derive(Debug, Default, Deserialize)]
#[serde(deny_unknown_fields)]
struct DumpQuery {
    from: Option<u64>,
    to: Option<u64>,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ChallengeProvider {
    Github,
    Google,
}

impl From<ChallengeProvider> for IdentityChallengeProvider {
    fn from(value: ChallengeProvider) -> Self {
        match value {
            ChallengeProvider::Github => Self::Github,
            ChallengeProvider::Google => Self::Google,
        }
    }
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ChallengeRequest {
    pub provider: ChallengeProvider,
    /// Exact canonical handle whose claim signature authenticates this challenge request.
    pub handle: String,
    /// Ed25519 key that may consume the challenge, lower-case hex.
    pub pubkey: String,
    /// Signature over the standard handle-claim payload, lower-case hex.
    pub signature: String,
    /// Required for GitHub and must be RFC 7636 S256; forbidden for Google.
    pub pkce_challenge: Option<String>,
}

#[derive(Debug, Serialize)]
pub struct ChallengeResponse {
    pub provider: &'static str,
    /// Use as OAuth `state` for GitHub or OIDC `nonce` for Google.
    pub challenge: String,
    pub expires_at_ms: u64,
    /// Public OAuth/OIDC app identifier. Provider secrets are never returned.
    pub client_id: String,
    /// Fixed provider endpoint. Clients must independently allowlist it before opening a browser.
    pub authorization_endpoint: &'static str,
    pub response_type: &'static str,
    pub response_mode: &'static str,
    pub scopes: Vec<&'static str>,
    pub challenge_parameter: &'static str,
    pub pkce_method: Option<&'static str>,
}

#[derive(Debug, Deserialize, Default)]
#[serde(deny_unknown_fields)]
pub struct ComplianceKeysQuery {
    /// Canonical typed key id encoded as 47 lower-case hex bytes.
    pub key_id: Option<String>,
    pub purpose: Option<CompliancePurpose>,
    pub jurisdiction: Option<Jurisdiction>,
    pub at_ms: Option<u64>,
    #[serde(default)]
    pub include_inactive: bool,
    /// Include each exact log leaf inline so modern clients can verify a projection in one
    /// bounded response. Omitted by default to remain wire-compatible with older clients.
    #[serde(default)]
    pub include_entries: bool,
    /// Return a checkpoint-bearing empty result without scanning the key projection.
    #[serde(default)]
    pub metadata_only: bool,
}

#[derive(Debug, Serialize)]
pub struct ComplianceKeyResponse {
    pub key_id_hex: String,
    pub publication: ComplianceKeyPublish,
    pub log_index: u64,
    pub inclusion_path: Vec<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub entry: Option<LogEntry>,
}

#[derive(Debug, Serialize)]
pub struct ComplianceKeysResponse {
    pub tree_size: u64,
    pub root: String,
    pub checkpoint: String,
    pub keys: Vec<ComplianceKeyResponse>,
}

/// Serve the complete registry surface after proving current independently witnessed readiness.
///
/// Route construction remains private: callers provide an already-bound listener and can only run
/// the fail-closed lifecycle. The listener may be loopback or public, but registration readiness
/// (including claim-trace readiness when identity providers are enabled) must already be current.
/// `supervise` is mandatory and must run witness publication plus any claim-key refresh work until
/// its receiver requests shutdown; an early return or failure stops HTTP admission.
pub async fn serve_witnessed<F, Fut>(
    listener: tokio::net::TcpListener,
    registry: Arc<Registry>,
    config: RegistryHttpConfig,
    stop: tokio::sync::watch::Receiver<bool>,
    supervise: F,
) -> Result<()>
where
    F: FnOnce(tokio::sync::watch::Receiver<bool>) -> Fut + Send + 'static,
    Fut: Future<Output = Result<()>> + Send + 'static,
{
    let validation = if let Err(error) = validate_limits(config.limits) {
        Err(error)
    } else if let Err(error) = validate_directory_publishers(&config.directory_publishers) {
        Err(error)
    } else if config.directory_publishers.is_empty() {
        Err(RegistryError::InvalidConfiguration(
            "witnessed registry service requires at least one pinned directory publisher".into(),
        ))
    } else if registry.witness_policy().is_none() {
        Err(RegistryError::InvalidConfiguration(
            "witnessed registry service requires an independent witness policy".into(),
        ))
    } else if let Err(error) = registry.validate_public_registration_capacity() {
        Err(error)
    } else {
        registry.registration_readiness(now_ms())
    };
    if let Err(error) = validation {
        shutdown_trace(Arc::clone(&registry)).await?;
        return Err(error);
    }
    let limits = config.limits;
    let request_timeout = config.request_timeout;
    serve_supervised_router(
        listener,
        full_router(registry.clone(), config)?,
        registry,
        limits,
        request_timeout,
        stop,
        supervise,
    )
    .await
}

/// Serve an explicitly read-only development registry on an already-bound loopback listener.
///
/// This is the sole normal unwitnessed HTTP mode. It refuses identity-enabled registries and does
/// not mount challenge, registration, rotation, directory-mutation, or operator-write routes.
pub async fn serve_loopback_read_only(
    listener: tokio::net::TcpListener,
    registry: Arc<Registry>,
    config: RegistryHttpConfig,
    stop: tokio::sync::watch::Receiver<bool>,
) -> Result<()> {
    let validation = validate_loopback_listener(&listener)
        .and_then(|()| validate_limits(config.limits))
        .and_then(|()| {
            if registry.registration_enabled() {
                return Err(RegistryError::InvalidConfiguration(
                    "unwitnessed loopback development mode must not enable identity providers"
                        .into(),
                ));
            }
            registry.registration_readiness(now_ms())
        });
    if let Err(error) = validation {
        shutdown_trace(Arc::clone(&registry)).await?;
        return Err(error);
    }
    let limits = config.limits;
    let request_timeout = config.request_timeout;
    serve_router(
        listener,
        read_only_router(registry.clone(), config)?,
        registry,
        limits,
        request_timeout,
        stop,
    )
    .await
}

/// Loopback-only full-surface fixture for crate integration tests.
///
/// Even with `test-utilities` enabled this function never exposes a Router and verifies the actual
/// bound listener before mounting mutation routes.
#[cfg(feature = "test-utilities")]
#[doc(hidden)]
pub async fn serve_loopback_test(
    listener: tokio::net::TcpListener,
    registry: Arc<Registry>,
    config: RegistryHttpConfig,
    stop: tokio::sync::watch::Receiver<bool>,
) -> Result<()> {
    let validation = validate_loopback_test_runtime(&listener, &config);
    if let Err(error) = validation {
        shutdown_trace(Arc::clone(&registry)).await?;
        return Err(error);
    }
    let limits = config.limits;
    let request_timeout = config.request_timeout;
    serve_router(
        listener,
        full_router(registry.clone(), config)?,
        registry,
        limits,
        request_timeout,
        stop,
    )
    .await
}

/// Supervised variant of [`serve_loopback_test`] for whole-process source acceptance tests.
///
/// This remains feature-gated and loopback-only. It lets the source-test CLI exercise the same
/// fail-closed witness and compliance supervisor lifecycle as production without weakening the
/// normal witnessed-serving prohibition on mock identity providers.
#[cfg(feature = "test-utilities")]
#[doc(hidden)]
pub async fn serve_loopback_test_supervised<F, Fut>(
    listener: tokio::net::TcpListener,
    registry: Arc<Registry>,
    config: RegistryHttpConfig,
    stop: tokio::sync::watch::Receiver<bool>,
    supervise: F,
) -> Result<()>
where
    F: FnOnce(tokio::sync::watch::Receiver<bool>) -> Fut + Send + 'static,
    Fut: Future<Output = Result<()>> + Send + 'static,
{
    let validation = validate_loopback_test_runtime(&listener, &config);
    if let Err(error) = validation {
        shutdown_trace(Arc::clone(&registry)).await?;
        return Err(error);
    }
    let limits = config.limits;
    let request_timeout = config.request_timeout;
    serve_supervised_router(
        listener,
        full_router(registry.clone(), config)?,
        registry,
        limits,
        request_timeout,
        stop,
        supervise,
    )
    .await
}

#[cfg(feature = "test-utilities")]
fn validate_loopback_test_runtime(
    listener: &tokio::net::TcpListener,
    config: &RegistryHttpConfig,
) -> Result<()> {
    validate_loopback_listener(listener)
        .and_then(|()| validate_limits(config.limits))
        .and_then(|()| validate_directory_publishers(&config.directory_publishers))
        .and_then(|()| {
            if config.directory_publishers.is_empty() {
                Err(RegistryError::InvalidConfiguration(
                    "loopback mutation fixture requires an explicit directory publisher".into(),
                ))
            } else {
                Ok(())
            }
        })
}

fn validate_loopback_listener(listener: &tokio::net::TcpListener) -> Result<()> {
    if !listener.local_addr()?.ip().is_loopback() {
        return Err(RegistryError::InvalidConfiguration(
            "unwitnessed registry service may listen only on loopback".into(),
        ));
    }
    Ok(())
}

async fn serve_router(
    listener: tokio::net::TcpListener,
    app: Router,
    registry: Arc<Registry>,
    limits: RegistryLimits,
    request_timeout: Duration,
    stop: tokio::sync::watch::Receiver<bool>,
) -> Result<()> {
    let server_result = serve_http(listener, app, limits, request_timeout, stop).await;

    // The trace sink owns a fixed queue and worker. Join it after HTTP admission closes; never
    // detach or time out the blocking join, because accepted claims must reach durable shutdown.
    let trace_result = shutdown_trace(registry).await;
    server_result?;
    trace_result
}

/// Own the Registry transport boundary so connection admission and Hyper parser timers cannot be
/// bypassed by a caller choosing a different serving helper.
async fn serve_http(
    listener: tokio::net::TcpListener,
    app: Router,
    limits: RegistryLimits,
    request_timeout: Duration,
    stop: tokio::sync::watch::Receiver<bool>,
) -> Result<()> {
    let connections = Arc::new(Semaphore::new(limits.max_concurrent_connections));
    let (connection_stop, _) = tokio::sync::watch::channel(false);
    let mut tasks = tokio::task::JoinSet::new();
    let header_timeout = Duration::from_millis(limits.header_timeout_ms);
    // General responses inherit `request_timeout`; immutable product ranges have their explicit
    // 120-second limit. Only a query-free full dump may suspend this ordinary connection bound.
    let connection_lifetime =
        header_timeout.saturating_add(request_timeout.max(DUMP_STREAM_TOTAL_TIMEOUT));
    let stopped = wait_for_stop(stop);
    tokio::pin!(stopped);

    loop {
        tokio::select! {
            () = &mut stopped => break,
            completed = tasks.join_next(), if !tasks.is_empty() => {
                if completed.is_some_and(|result| result.is_err()) {
                    let _ = connection_stop.send(true);
                    tasks.abort_all();
                    while tasks.join_next().await.is_some() {}
                    return Err(RegistryError::RegistryUnavailable);
                }
            }
            accepted = listener.accept() => {
                let (stream, peer) = accepted?;
                let permit = match Arc::clone(&connections).try_acquire_owned() {
                    Ok(permit) => permit,
                    Err(_) => {
                        // Overload is closed at accept; it never becomes a user-space socket queue.
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
                        connection_lifetime,
                        connection_stop,
                    )
                    .await;
                });
            }
        }
    }

    let _ = connection_stop.send(true);
    tokio::time::timeout(SERVER_DRAIN_TIMEOUT, async {
        while let Some(result) = tasks.join_next().await {
            if result.is_err() {
                return Err(RegistryError::RegistryUnavailable);
            }
        }
        Ok(())
    })
    .await
    .map_err(|_| RegistryError::RegistryUnavailable)??;
    Ok(())
}

async fn serve_http_connection(
    stream: tokio::net::TcpStream,
    peer: SocketAddr,
    app: Router,
    header_timeout: Duration,
    connection_lifetime: Duration,
    stop: tokio::sync::watch::Receiver<bool>,
) {
    let (lifetime_state, mut lifetime_changed) = ConnectionLifetimeState::new();
    let tower_service = app.layer(axum::Extension(ConnectInfo(RegistryConnection(peer))));
    let request_lifetime = Arc::clone(&lifetime_state);
    let service = tower::service_fn(move |request: axum::http::Request<Incoming>| {
        let app = tower_service.clone();
        let full_dump = request.uri().path() == "/v1/log/dump"
            && request.uri().query().is_none_or(str::is_empty);
        let guard = full_dump.then(|| request_lifetime.begin_full_dump());
        async move {
            let response = app.oneshot(request.map(Body::new)).await?;
            Ok::<_, Infallible>(match guard {
                Some(guard) => hold_full_dump_connection(response, guard),
                None => response,
            })
        }
    });
    let service = TowerToHyperService::new(service);
    // Query-free dumps may outlive the ordinary absolute connection deadline as long as their
    // socket keeps making write progress. Keep the origin HTTP/1.1-only so writes from another
    // multiplexed stream can never mask a stalled dump. TLS edges may still serve HTTP/2 to clients
    // while using one HTTP/1.1 upstream connection per Registry response.
    let mut builder = ConnectionBuilder::new(TokioExecutor::new()).http1_only();
    builder
        .http1()
        .timer(TokioTimer::new())
        .header_read_timeout(header_timeout)
        .max_headers(100)
        .max_buf_size(64 * 1024);

    let io = IdleWriteIo::new(stream, DUMP_STREAM_IDLE_TIMEOUT);
    let connection = builder
        .serve_connection(TokioIo::new(io), service)
        .into_owned();
    tokio::pin!(connection);
    let lifetime = tokio::time::sleep(connection_lifetime);
    tokio::pin!(lifetime);
    let stopped = wait_for_stop(stop);
    tokio::pin!(stopped);
    let mut lifetime_elapsed = false;

    loop {
        if lifetime_state.close_requested() {
            connection.as_mut().graceful_shutdown();
            let _ = tokio::time::timeout(SERVER_DRAIN_TIMEOUT, &mut connection).await;
            return;
        }
        if lifetime_elapsed && !lifetime_state.full_dump_active() {
            return;
        }
        tokio::select! {
            result = &mut connection => {
                if result.is_err() {
                    // Do not place peer addresses or attacker-controlled parser details in logs.
                    tracing::debug!(kind = "connection", "registry HTTP connection closed");
                }
                return;
            }
            _ = &mut lifetime, if !lifetime_elapsed => {
                lifetime_elapsed = true;
            }
            changed = lifetime_changed.changed() => {
                if changed.is_err() {
                    return;
                }
            }
            () = &mut stopped => {
                connection.as_mut().graceful_shutdown();
                let _ = tokio::time::timeout(SERVER_DRAIN_TIMEOUT, &mut connection).await;
                return;
            }
        }
    }
}

async fn serve_supervised_router<F, Fut>(
    listener: tokio::net::TcpListener,
    app: Router,
    registry: Arc<Registry>,
    limits: RegistryLimits,
    request_timeout: Duration,
    stop: tokio::sync::watch::Receiver<bool>,
    supervise: F,
) -> Result<()>
where
    F: FnOnce(tokio::sync::watch::Receiver<bool>) -> Fut + Send + 'static,
    Fut: Future<Output = Result<()>> + Send + 'static,
{
    let (service_stop, service_stopped) = tokio::sync::watch::channel(false);
    let mut service_task = tokio::spawn(serve_router(
        listener,
        app,
        registry,
        limits,
        request_timeout,
        service_stopped,
    ));
    let (supervisor_stop, supervisor_stopped) = tokio::sync::watch::channel(false);
    let mut supervisor_task = tokio::spawn(supervise(supervisor_stopped));

    enum RuntimeStop {
        Requested,
        Service(std::result::Result<Result<()>, tokio::task::JoinError>),
        Supervisor(std::result::Result<Result<()>, tokio::task::JoinError>),
    }
    let stopped = tokio::select! {
        () = wait_for_stop(stop) => RuntimeStop::Requested,
        result = &mut service_task => RuntimeStop::Service(result),
        result = &mut supervisor_task => RuntimeStop::Supervisor(result),
    };
    let _ = service_stop.send(true);
    let _ = supervisor_stop.send(true);

    match stopped {
        RuntimeStop::Requested => {
            let service = runtime_task_outcome((&mut service_task).await);
            let supervisor = drain_supervisor(&mut supervisor_task).await;
            combine_runtime_results(service, supervisor, false)
        }
        RuntimeStop::Service(result) => {
            let service = runtime_task_outcome(result);
            let supervisor = drain_supervisor(&mut supervisor_task).await;
            combine_runtime_results(service, supervisor, true)
        }
        RuntimeStop::Supervisor(result) => {
            let supervisor = runtime_task_outcome(result);
            let service = runtime_task_outcome((&mut service_task).await);
            combine_runtime_results(supervisor, service, true)
        }
    }
}

fn combine_runtime_results(
    primary: Result<()>,
    secondary: Result<()>,
    early_exit_is_error: bool,
) -> Result<()> {
    match (primary, secondary) {
        (Err(error), _) => Err(error),
        (Ok(()), Err(error)) => Err(error),
        (Ok(()), Ok(())) if early_exit_is_error => Err(RegistryError::RegistryUnavailable),
        (Ok(()), Ok(())) => Ok(()),
    }
}

async fn drain_supervisor(task: &mut tokio::task::JoinHandle<Result<()>>) -> Result<()> {
    match tokio::time::timeout(SERVER_DRAIN_TIMEOUT, &mut *task).await {
        Ok(result) => runtime_task_outcome(result),
        Err(_) => {
            task.abort();
            let _ = task.await;
            Err(RegistryError::RegistryUnavailable)
        }
    }
}

fn runtime_task_outcome(
    result: std::result::Result<Result<()>, tokio::task::JoinError>,
) -> Result<()> {
    result.map_err(|_| RegistryError::RegistryUnavailable)?
}

async fn shutdown_trace(registry: Arc<Registry>) -> Result<()> {
    tokio::task::spawn_blocking(move || registry.shutdown_claim_trace(now_ms()))
        .await
        .map_err(|_| RegistryError::RegistryUnavailable)?
}

async fn wait_for_stop(mut stop: tokio::sync::watch::Receiver<bool>) {
    if *stop.borrow() {
        return;
    }
    while stop.changed().await.is_ok() {
        if *stop.borrow() {
            return;
        }
    }
}

fn read_routes() -> Router<Arc<RegistryServer>> {
    Router::new()
        .route("/health", get(health))
        .route("/v1/resolve/{namespace}/{name}", get(resolve))
        .route("/v1/log/checkpoint", get(checkpoint))
        .route("/v1/log/status", get(log_status))
        .route("/v1/log/consistency", get(consistency))
        .route("/v1/log/entries", get(entries))
        .route("/v1/log/dump", get(dump))
        .route("/v1/compliance-keys", get(compliance_keys))
        .route("/v1/compliance-keys/{key_id}", get(compliance_key))
}

fn with_server_layers(routes: Router<Arc<RegistryServer>>, state: Arc<RegistryServer>) -> Router {
    let middleware_state = Arc::clone(&state);
    routes
        .layer(axum::extract::DefaultBodyLimit::max(
            MAX_DIRECTORY_MUTATION_BODY_BYTES,
        ))
        .layer(middleware::from_fn_with_state(
            middleware_state,
            admission_middleware,
        ))
        .with_state(state)
}

fn read_only_router(registry: Arc<Registry>, config: RegistryHttpConfig) -> Result<Router> {
    let state = Arc::new(RegistryServer::new(registry, config)?);
    Ok(with_server_layers(read_routes(), state))
}

fn full_router(registry: Arc<Registry>, config: RegistryHttpConfig) -> Result<Router> {
    let state = Arc::new(RegistryServer::new(registry, config)?);
    let routes = read_routes()
        .route("/v1/identity/challenge", post(identity_challenge))
        .route("/v1/register", post(register))
        .route("/v1/rotate", post(rotate))
        .route("/v1/directory/add", post(directory_add))
        .route("/v1/directory/remove", post(directory_remove));
    Ok(with_server_layers(routes, state))
}

async fn admission_middleware(
    State(state): State<Arc<RegistryServer>>,
    request: Request,
    next: Next,
) -> Response<Body> {
    let deadline = tokio::time::Instant::now() + state.request_timeout;
    let permit = match state.try_enter().and_then(|permit| {
        state.limiter.charge_global()?;
        Ok(permit)
    }) {
        Ok(permit) => permit,
        Err(error) => return error.into_response(),
    };
    // This middleware is outside both the body-limit layer and route extractors, so one deadline
    // covers a slow request body, handler work, and downstream response-body production. Dump
    // handlers replace this general lane with their separately bounded stream lane.
    let response = tokio::time::timeout_at(deadline, next.run(request)).await;
    let Ok(mut response) = response else {
        drop(permit);
        return RegistryError::Overloaded.into_response();
    };
    if response
        .extensions_mut()
        .remove::<DedicatedResponseEgress>()
        .is_some()
    {
        drop(permit);
        return response;
    }
    if response
        .body()
        .size_hint()
        .upper()
        .is_some_and(|response_bytes| response_bytes > MAX_GENERAL_RESPONSE_BYTES)
    {
        drop(permit);
        return RegistryError::Overloaded.into_response();
    }
    hold_response_permit_with_egress(
        response,
        permit,
        Some(deadline),
        Some(MAX_GENERAL_RESPONSE_BYTES),
        Some(state.limiter.response_byte_limiter()),
    )
}

async fn health(State(state): State<Arc<RegistryServer>>) -> Result<&'static str> {
    state
        .blocking(move |registry| registry.registration_readiness(now_ms()))
        .await?;
    Ok("ok")
}

async fn identity_challenge(
    State(state): State<Arc<RegistryServer>>,
    ConnectInfo(RegistryConnection(connected)): ConnectInfo<RegistryConnection>,
    headers: HeaderMap,
    Json(request): Json<ChallengeRequest>,
) -> Result<Response<Body>> {
    let source = trace_source(&state.config, connected, &headers)?;
    state.limiter.charge_challenge(source.ip())?;
    let provider = request.provider.into();
    let handle = Handle::parse(&request.handle)?;
    let pubkey = parse_hex32(&request.pubkey)
        .ok_or_else(|| RegistryError::MalformedHandle("pubkey must be 32 hex bytes".into()))?;
    let signature = parse_hex64(&request.signature)
        .ok_or_else(|| RegistryError::MalformedHandle("signature must be 64 hex bytes".into()))?;
    let pkce_challenge = request.pkce_challenge;
    let challenge = state
        .blocking(move |registry| {
            registry.issue_identity_challenge(
                provider,
                &handle,
                &pubkey,
                &signature,
                pkce_challenge.as_deref(),
            )
        })
        .await?;
    let authorization = challenge.authorization;
    json_response(
        &ChallengeResponse {
            provider: challenge.provider.as_str(),
            challenge: challenge.value,
            expires_at_ms: challenge.expires_at_ms,
            client_id: authorization.client_id,
            authorization_endpoint: authorization.authorization_endpoint,
            response_type: authorization.response_type,
            response_mode: authorization.response_mode,
            scopes: authorization.scopes,
            challenge_parameter: authorization.challenge_parameter,
            pkce_method: authorization.pkce_method,
        },
        NO_STORE,
        None,
    )
}

async fn register(
    State(state): State<Arc<RegistryServer>>,
    ConnectInfo(RegistryConnection(connected)): ConnectInfo<RegistryConnection>,
    headers: HeaderMap,
    Json(request): Json<RegisterRequest>,
) -> Result<Response<Body>> {
    bind_handle(&state, connected, &headers, request, false).await
}

async fn rotate(
    State(state): State<Arc<RegistryServer>>,
    ConnectInfo(RegistryConnection(connected)): ConnectInfo<RegistryConnection>,
    headers: HeaderMap,
    Json(request): Json<RegisterRequest>,
) -> Result<Response<Body>> {
    bind_handle(&state, connected, &headers, request, true).await
}

async fn directory_add(
    State(state): State<Arc<RegistryServer>>,
    ConnectInfo(RegistryConnection(connected)): ConnectInfo<RegistryConnection>,
    headers: HeaderMap,
    body: Bytes,
) -> Result<Response<Body>> {
    append_directory_mutation(
        &state,
        connected,
        &headers,
        DirectoryMutationOperation::Add,
        body,
        |registry, body| {
            let mutation: DirectoryAdd = serde_json::from_slice(body).map_err(|_| {
                RegistryError::MalformedEntry("invalid directory add mutation".into())
            })?;
            registry.append_directory_add(mutation)
        },
    )
    .await
}

async fn directory_remove(
    State(state): State<Arc<RegistryServer>>,
    ConnectInfo(RegistryConnection(connected)): ConnectInfo<RegistryConnection>,
    headers: HeaderMap,
    body: Bytes,
) -> Result<Response<Body>> {
    append_directory_mutation(
        &state,
        connected,
        &headers,
        DirectoryMutationOperation::Remove,
        body,
        |registry, body| {
            let mutation: DirectoryRemove = serde_json::from_slice(body).map_err(|_| {
                RegistryError::MalformedEntry("invalid directory remove mutation".into())
            })?;
            registry.append_directory_remove(mutation)
        },
    )
    .await
}

async fn append_directory_mutation<F>(
    state: &RegistryServer,
    connected: SocketAddr,
    headers: &HeaderMap,
    authenticated_operation: DirectoryMutationOperation,
    body: Bytes,
    operation: F,
) -> Result<Response<Body>>
where
    F: FnOnce(Arc<Registry>, &[u8]) -> Result<LoggedDirectoryMutation> + Send + 'static,
{
    let source = trace_source(&state.config, connected, headers)?;
    // Source admission precedes allowlist lookup and Ed25519 verification so a forged publisher
    // request cannot spend crypto at the much larger global request ceiling.
    state.limiter.charge_binding(source.ip())?;
    let config = state.config.clone();
    let headers = headers.clone();
    let mutation = state
        .blocking(move |registry| {
            // Keep authorization ahead of JSON decoding while moving both CPU costs into the
            // bounded fail-fast lane shared with the resulting SQLite mutation.
            authenticate_directory_publisher(
                &config,
                registry.origin(),
                &headers,
                authenticated_operation,
                &body,
            )?;
            registry.registration_readiness(now_ms())?;
            operation(registry, &body)
        })
        .await?;
    json_response(
        &DirectoryMutationResponse {
            entry: mutation.entry,
            log_index: mutation.index,
            appended: mutation.appended,
            inclusion_proof: mutation.inclusion.into(),
        },
        NO_STORE,
        None,
    )
}

async fn bind_handle(
    state: &RegistryServer,
    connected: SocketAddr,
    headers: &HeaderMap,
    request: RegisterRequest,
    rotate: bool,
) -> Result<Response<Body>> {
    let source = trace_source(&state.config, connected, headers)?;
    state.limiter.charge_binding(source.ip())?;
    let handle = Handle::parse(&request.handle)?;
    let pubkey = parse_hex32(&request.pubkey)
        .ok_or_else(|| RegistryError::MalformedHandle("pubkey must be 32 hex bytes".into()))?;
    let signature = parse_hex64(&request.signature)
        .ok_or_else(|| RegistryError::MalformedHandle("signature must be 64 hex bytes".into()))?;
    let proof = request.proof;
    let operation = if rotate {
        HandleBindingOperation::Rotate
    } else {
        HandleBindingOperation::Register
    };
    // Every SQLite/readiness phase enters the fail-fast blocking lane. Only provider verification
    // and the supervised claim-trace receipt remain on Tokio, so serialized storage can neither
    // stall an executor worker nor escape the configured blocking concurrency bound.
    let prepared = state
        .blocking(move |registry| {
            registry.prepare_handle_binding(handle, pubkey, signature, proof, source, operation)
        })
        .await?;
    let registration = if prepared.is_recovery() {
        state
            .blocking(move |registry| registry.recover_handle_binding(prepared))
            .await?
    } else {
        let verified = state.registry.verify_handle_binding(prepared).await?;
        let verified = state
            .blocking(move |registry| registry.admit_handle_binding(verified))
            .await?;
        state.registry.capture_handle_binding(&verified).await?;
        state
            .blocking(move |registry| registry.commit_handle_binding(verified))
            .await?
    };
    json_response(
        &RegisterResponse {
            handle: registration.handle,
            log_index: registration.index,
            appended: registration.appended,
            inclusion_proof: registration.inclusion.into(),
        },
        NO_STORE,
        None,
    )
}

async fn resolve(
    State(state): State<Arc<RegistryServer>>,
    Path((namespace, name)): Path<(String, String)>,
    headers: HeaderMap,
) -> Result<Response<Body>> {
    let handle = Handle::new(&namespace, &name)?;
    let handle_path = handle.as_path();
    let resolved = state
        .blocking(move |registry| registry.resolve(&handle))
        .await?;
    let etag = response_etag(
        b"resolve",
        resolved.inclusion.size,
        &resolved.inclusion.root,
        handle_path.as_bytes(),
    );
    if matches_etag(&headers, &etag) {
        return not_modified(&etag, READ_CACHE);
    }
    json_response(
        &ResolveResponse {
            handle: resolved.handle,
            pubkey: resolved.pubkey,
            log_index: resolved.index,
            inclusion_proof: resolved.inclusion.into(),
        },
        READ_CACHE,
        Some(&etag),
    )
}

async fn checkpoint(
    State(state): State<Arc<RegistryServer>>,
    headers: HeaderMap,
) -> Result<Response<Body>> {
    let head = state.blocking(move |registry| registry.head()).await?;
    let etag = response_etag(b"checkpoint", head.size, &head.root, &[]);
    if matches_etag(&headers, &etag) {
        return not_modified(&etag, CHECKPOINT_CACHE);
    }
    bytes_response(
        head.checkpoint.into_bytes(),
        "text/plain; charset=utf-8",
        CHECKPOINT_CACHE,
        Some(&etag),
        None,
    )
}

async fn log_status(State(state): State<Arc<RegistryServer>>) -> Result<Response<Body>> {
    let now_secs = now_ms() / 1_000;
    let (publication, ready) = state
        .blocking(move |registry| {
            let publication = registry.witness_publication_status()?;
            let ready = registry.witness_readiness(now_secs).is_ok();
            Ok((publication, ready))
        })
        .await?;
    json_response(&log_status_response(publication, ready), NO_STORE, None)
}

fn log_status_response(publication: WitnessPublicationStatus, ready: bool) -> LogStatusResponse {
    LogStatusResponse {
        ready,
        committed_size: publication.committed_size,
        published_size: publication.published_size,
        lag_entries: publication.lag_entries,
        witnessed_at: publication.witnessed_at,
    }
}

async fn consistency(
    State(state): State<Arc<RegistryServer>>,
    Query(query): Query<ConsistencyQuery>,
    headers: HeaderMap,
) -> Result<Response<Body>> {
    let from = query.from;
    let to = query.to;
    let (root, path) = state
        .blocking(move |registry| registry.published_consistency_proof_between(from, to))
        .await?;
    let etag = response_etag(b"consistency", to, &root, &query.from.to_be_bytes());
    if matches_etag(&headers, &etag) {
        return not_modified(&etag, READ_CACHE);
    }
    json_response(
        &ConsistencyResponse {
            from: query.from,
            to,
            root: hex(&root),
            path: path.iter().map(|hash| hex(hash)).collect(),
        },
        READ_CACHE,
        Some(&etag),
    )
}

async fn entries(
    State(state): State<Arc<RegistryServer>>,
    Query(query): Query<EntriesQuery>,
    headers: HeaderMap,
) -> Result<Response<Body>> {
    let head = state.blocking(move |registry| registry.head()).await?;
    let requested_to = query
        .to
        .unwrap_or_else(|| query.from.saturating_add(MAX_ENTRIES_PAGE));
    if query.from > requested_to
        || requested_to > head.size
        || requested_to.saturating_sub(query.from) > MAX_ENTRIES_PAGE
    {
        return Err(RegistryError::MalformedEntry(
            "entry range must be ordered, within the checkpoint, and at most 1000 entries".into(),
        ));
    }
    let etag = response_etag(
        b"entries",
        head.size,
        &head.root,
        format!("{}:{requested_to}", query.from).as_bytes(),
    );
    if matches_etag(&headers, &etag) {
        return not_modified(&etag, READ_CACHE);
    }
    let from = query.from;
    #[cfg(test)]
    let serialization_barrier = state.take_serialization_test_barrier();
    let encoded = state
        .blocking(move |registry| {
            let page = if from == requested_to {
                Vec::new()
            } else {
                registry.entries(from, requested_to - from)?
            };
            if page.len() as u64 != requested_to - from {
                return Err(RegistryError::CorruptStorage(
                    "entry range contains a gap".into(),
                ));
            }
            #[cfg(test)]
            if let Some(barrier) = serialization_barrier {
                barrier.wait();
            }
            Ok(serde_json::to_vec(&EntriesResponse {
                from,
                to: requested_to,
                tree_size: head.size,
                root: hex(&head.root),
                checkpoint: head.checkpoint,
                entries: page,
            })?)
        })
        .await?;
    bytes_response(encoded, "application/json", READ_CACHE, Some(&etag), None)
}

struct EncodedDumpPage {
    entries: u64,
    bytes: Vec<u8>,
}

fn read_and_encode_dump_page(
    registry: &Registry,
    from: u64,
    limit: u64,
    exact_range: bool,
) -> Result<EncodedDumpPage> {
    let page = registry.entries(from, limit)?;
    if page.is_empty() {
        return Err(RegistryError::CorruptStorage(
            "registry dump encountered a gap".into(),
        ));
    }
    if page.len() as u64 > limit {
        return Err(RegistryError::CorruptStorage(
            "registry dump exceeded its exact range".into(),
        ));
    }
    let mut bytes = Vec::with_capacity(page.len() * 256);
    for entry in &page {
        let line_start = bytes.len();
        serde_json::to_writer(&mut bytes, entry)?;
        bytes.push(b'\n');
        if exact_range
            && u64::try_from(bytes.len().saturating_sub(line_start)).unwrap_or(u64::MAX)
                > MAX_AUDIT_DUMP_LINE_BYTES
        {
            return Err(RegistryError::MalformedEntry(
                "registry dump line exceeded its byte budget".into(),
            ));
        }
    }
    Ok(EncodedDumpPage {
        entries: page.len() as u64,
        bytes,
    })
}

/// Stream either the complete exit log or one immutable exact range as newline-delimited strict
/// JSON. Query-free behavior is preserved for mirrors and forks; product clients use exact ranges
/// so a CDN key never aliases two different prefixes.
async fn dump(
    State(state): State<Arc<RegistryServer>>,
    Query(query): Query<DumpQuery>,
    headers: HeaderMap,
) -> Result<Response<Body>> {
    let (from, to, etag, cache, disposition, exact_range) = match (query.from, query.to) {
        (None, None) => {
            let head = state.blocking(move |registry| registry.head()).await?;
            (
                0,
                head.size,
                response_etag(b"dump", head.size, &head.root, &[]),
                DUMP_CACHE,
                "attachment; filename=\"pigeonpost-log.ndjson\"".to_owned(),
                false,
            )
        }
        (Some(from), Some(to))
            if from < to && to - from <= AUDIT_DUMP_SEGMENT_ENTRIES =>
        {
            let root = state
                .blocking(move |registry| registry.dump_range_root(from, to))
                .await?;
            (
                from,
                to,
                response_etag(b"dump-range-v1", to, &root, &from.to_le_bytes()),
                RANGE_DUMP_CACHE,
                format!("attachment; filename=\"pigeonpost-log-{from}-{to}.ndjson\""),
                true,
            )
        }
        _ => {
            return Err(RegistryError::MalformedEntry(format!(
                "registry dump range must contain both bounds and at most {AUDIT_DUMP_SEGMENT_ENTRIES} entries"
            )))
        }
    };
    if matches_etag(&headers, &etag) {
        return not_modified(&etag, cache);
    }

    // Product bootstrap ranges and the unbounded mirror/exit stream use independent admission
    // lanes. One slow mirror can neither occupy a range slot nor make product bootstrap return 503.
    let stream_permit = if exact_range {
        state.try_stream()?
    } else {
        state.try_full_stream()?
    };
    let deadline = exact_range.then(|| tokio::time::Instant::now() + DUMP_STREAM_TOTAL_TIMEOUT);
    let (sender, receiver) = tokio::sync::mpsc::channel::<std::io::Result<Bytes>>(4);
    #[cfg(test)]
    let mut serialization_barrier = state.take_serialization_test_barrier();
    tokio::spawn(async move {
        // Exact product ranges have a hard total deadline. The query-free exit stream is complete
        // beyond the product envelope and therefore has only an idle-progress deadline.
        let mut cursor = from;
        let mut emitted_bytes = 0u64;
        while cursor < to {
            let remaining = to - cursor;
            let page_from = cursor;
            let page_limit = remaining.min(STREAM_PAGE);
            #[cfg(test)]
            let page_barrier = serialization_barrier.take();
            let page_future = state.blocking(move |registry| {
                let page =
                    read_and_encode_dump_page(&registry, page_from, page_limit, exact_range)?;
                #[cfg(test)]
                if let Some(barrier) = page_barrier {
                    barrier.wait();
                }
                Ok(page)
            });
            let page_result = match deadline {
                Some(deadline) => match tokio::time::timeout_at(deadline, page_future).await {
                    Ok(result) => result,
                    Err(_) => {
                        let _ = sender.try_send(Err(std::io::Error::new(
                            std::io::ErrorKind::TimedOut,
                            "registry dump exceeded its total deadline",
                        )));
                        return;
                    }
                },
                None => page_future.await,
            };
            let page = match page_result {
                Ok(page) => page,
                Err(error) => {
                    let _ = sender.try_send(Err(std::io::Error::other(error.to_string())));
                    return;
                }
            };
            let chunk_bytes = u64::try_from(page.bytes.len()).unwrap_or(u64::MAX);
            let Some(next_emitted_bytes) = emitted_bytes.checked_add(chunk_bytes) else {
                let _ = sender.try_send(Err(std::io::Error::other(
                    "registry dump byte accounting overflowed",
                )));
                return;
            };
            if exact_range && next_emitted_bytes > MAX_AUDIT_DUMP_RESPONSE_BYTES {
                let _ = sender.try_send(Err(std::io::Error::other(
                    "registry dump exceeded its response byte budget",
                )));
                return;
            }
            if state.limiter.charge_response_bytes(chunk_bytes).is_err() {
                let _ = sender.try_send(Err(std::io::Error::other(
                    "registry response byte rate exhausted",
                )));
                return;
            }
            emitted_bytes = next_emitted_bytes;
            cursor += page.entries;
            if !send_dump_chunk(&sender, Bytes::from(page.bytes), deadline).await {
                return;
            }
        }
    });

    let mut response = response_with_body(
        Body::from_stream(ReceiverStream::new(receiver)),
        "application/x-ndjson",
        cache,
        Some(&etag),
        Some(&disposition),
        StatusCode::OK,
    )?;
    response.extensions_mut().insert(DedicatedResponseEgress);
    Ok(hold_response_permit(
        response,
        stream_permit,
        deadline,
        exact_range.then_some(MAX_AUDIT_DUMP_RESPONSE_BYTES),
    ))
}

async fn send_dump_chunk(
    sender: &tokio::sync::mpsc::Sender<std::io::Result<Bytes>>,
    chunk: Bytes,
    deadline: Option<tokio::time::Instant>,
) -> bool {
    send_dump_chunk_with_idle(sender, chunk, deadline, DUMP_STREAM_IDLE_TIMEOUT).await
}

async fn send_dump_chunk_with_idle(
    sender: &tokio::sync::mpsc::Sender<std::io::Result<Bytes>>,
    chunk: Bytes,
    deadline: Option<tokio::time::Instant>,
    idle_timeout: Duration,
) -> bool {
    let remaining = deadline
        .map(|deadline| deadline.saturating_duration_since(tokio::time::Instant::now()))
        .unwrap_or(idle_timeout);
    if remaining.is_zero() {
        return false;
    }
    matches!(
        tokio::time::timeout(idle_timeout.min(remaining), sender.send(Ok(chunk))).await,
        Ok(Ok(()))
    )
}

async fn compliance_keys(
    State(state): State<Arc<RegistryServer>>,
    Query(query): Query<ComplianceKeysQuery>,
    headers: HeaderMap,
) -> Result<Response<Body>> {
    compliance_response(&state, query, &headers, false).await
}

async fn compliance_key(
    State(state): State<Arc<RegistryServer>>,
    Path(key_id): Path<String>,
    headers: HeaderMap,
) -> Result<Response<Body>> {
    compliance_response(
        &state,
        ComplianceKeysQuery {
            key_id: Some(key_id),
            include_inactive: true,
            include_entries: true,
            metadata_only: false,
            ..Default::default()
        },
        &headers,
        true,
    )
    .await
}

async fn compliance_response(
    state: &RegistryServer,
    query: ComplianceKeysQuery,
    headers: &HeaderMap,
    require_one: bool,
) -> Result<Response<Body>> {
    let key_id = query.key_id.as_deref().map(parse_key_id).transpose()?;
    let registry_query = ComplianceKeyQuery {
        key_id,
        purpose: query.purpose,
        jurisdiction: query.jurisdiction,
        at_ms: query.at_ms,
        include_inactive: query.include_inactive,
        metadata_only: query.metadata_only,
    };
    let include_entries = query.include_entries;
    let conditional_headers = headers.clone();
    #[cfg(test)]
    let serialization_barrier = state.take_serialization_test_barrier();
    let encoded = state
        .blocking(move |registry| {
            let snapshot = registry.compliance_key_set(&registry_query)?;
            let records = snapshot.keys;
            if require_one && records.is_empty() {
                return Err(RegistryError::NotFound);
            }
            let head = snapshot.head;
            let mut query_tag = format!(
                "{:?}:{:?}:{:?}:{:?}:{}:{}",
                query.key_id,
                query.purpose,
                query.jurisdiction,
                query.at_ms,
                query.include_inactive,
                query.metadata_only
            );
            for record in &records {
                query_tag.push(':');
                query_tag.push_str(&record.index.to_string());
            }
            let etag = response_etag(
                b"compliance-keys",
                head.size,
                &head.root,
                query_tag.as_bytes(),
            );
            if matches_etag(&conditional_headers, &etag) {
                return Ok(EncodedComplianceResponse::NotModified { etag });
            }
            #[cfg(test)]
            if let Some(barrier) = serialization_barrier {
                barrier.wait();
            }
            let keys = records
                .into_iter()
                .map(|record| compliance_key_response(record, include_entries))
                .collect::<Result<Vec<_>>>()?;
            let body = serde_json::to_vec(&ComplianceKeysResponse {
                tree_size: head.size,
                root: hex(&head.root),
                checkpoint: head.checkpoint,
                keys,
            })?;
            if u64::try_from(body.len()).unwrap_or(u64::MAX) > MAX_GENERAL_RESPONSE_BYTES {
                return Err(RegistryError::Overloaded);
            }
            Ok(EncodedComplianceResponse::Body { etag, body })
        })
        .await?;
    match encoded {
        EncodedComplianceResponse::NotModified { etag } => not_modified(&etag, COMPLIANCE_CACHE),
        EncodedComplianceResponse::Body { etag, body } => bytes_response(
            body,
            "application/json",
            COMPLIANCE_CACHE,
            Some(&etag),
            None,
        ),
    }
}

enum EncodedComplianceResponse {
    NotModified { etag: String },
    Body { etag: String, body: Vec<u8> },
}

fn compliance_key_response(
    record: LoggedComplianceKey,
    include_entry: bool,
) -> Result<ComplianceKeyResponse> {
    let key_id = record
        .publication
        .key_id
        .encode()
        .map_err(|error| RegistryError::MalformedEntry(error.to_string()))?;
    Ok(ComplianceKeyResponse {
        key_id_hex: hex(&key_id),
        publication: record.publication,
        log_index: record.index,
        inclusion_path: record.inclusion.path.iter().map(|hash| hex(hash)).collect(),
        entry: include_entry.then_some(record.entry),
    })
}

fn parse_key_id(input: &str) -> Result<ComplianceKeyId> {
    let bytes = parse_hex(input, COMPLIANCE_KEY_ID_LEN).ok_or_else(|| {
        RegistryError::MalformedEntry(
            "key_id must be the canonical 47-byte lower-case hex encoding".into(),
        )
    })?;
    ComplianceKeyId::decode(&bytes)
        .map_err(|error| RegistryError::MalformedEntry(error.to_string()))
}

fn json_response<T: Serialize>(
    value: &T,
    cache_control: &'static str,
    etag: Option<&str>,
) -> Result<Response<Body>> {
    bytes_response(
        serde_json::to_vec(value)?,
        "application/json",
        cache_control,
        etag,
        None,
    )
}

fn bytes_response(
    body: Vec<u8>,
    content_type: &'static str,
    cache_control: &'static str,
    etag: Option<&str>,
    disposition: Option<&str>,
) -> Result<Response<Body>> {
    response_with_body(
        Body::from(body),
        content_type,
        cache_control,
        etag,
        disposition,
        StatusCode::OK,
    )
}

fn not_modified(etag: &str, cache_control: &'static str) -> Result<Response<Body>> {
    response_with_body(
        Body::empty(),
        "application/octet-stream",
        cache_control,
        Some(etag),
        None,
        StatusCode::NOT_MODIFIED,
    )
}

fn response_with_body(
    body: Body,
    content_type: &'static str,
    cache_control: &'static str,
    etag: Option<&str>,
    disposition: Option<&str>,
    status: StatusCode,
) -> Result<Response<Body>> {
    let mut response = Response::new(body);
    *response.status_mut() = status;
    let headers = response.headers_mut();
    headers.insert(CACHE_CONTROL, HeaderValue::from_static(cache_control));
    if status != StatusCode::NOT_MODIFIED {
        headers.insert(CONTENT_TYPE, HeaderValue::from_static(content_type));
    }
    if let Some(value) = etag {
        headers.insert(
            ETAG,
            HeaderValue::from_str(value)
                .map_err(|_| RegistryError::Io(std::io::Error::other("invalid generated ETag")))?,
        );
    }
    if let Some(value) = disposition {
        headers.insert(
            CONTENT_DISPOSITION,
            HeaderValue::from_str(value).map_err(|_| {
                RegistryError::Io(std::io::Error::other(
                    "invalid generated content disposition",
                ))
            })?,
        );
    }
    Ok(response)
}

fn response_etag(domain: &[u8], size: u64, root: &[u8; 32], discriminator: &[u8]) -> String {
    let mut hasher = Sha256::new();
    hasher.update(b"pigeonpost/registry-etag/v1\0");
    hasher.update(domain);
    hasher.update(size.to_be_bytes());
    hasher.update(root);
    hasher.update(discriminator);
    format!("\"{}\"", hex(&hasher.finalize()))
}

fn matches_etag(headers: &HeaderMap, etag: &str) -> bool {
    headers
        .get(IF_NONE_MATCH)
        .and_then(|value| value.to_str().ok())
        .is_some_and(|value| {
            value.split(',').map(str::trim).any(|candidate| {
                candidate == "*" || candidate.strip_prefix("W/").unwrap_or(candidate) == etag
            })
        })
}

fn trace_source(
    config: &RegistryHttpConfig,
    connected: SocketAddr,
    headers: &HeaderMap,
) -> Result<SocketAddr> {
    if connected.port() == 0 || connected.ip().is_unspecified() || connected.ip().is_multicast() {
        return Err(RegistryError::ClaimTraceUnavailable);
    }
    if !config.trusted_proxies.contains(&connected.ip()) {
        return Ok(connected);
    }
    if headers.contains_key("x-forwarded-for") {
        return Err(RegistryError::ClaimTraceUnavailable);
    }
    let forwarded = headers
        .get(FORWARDED)
        .and_then(|value| value.to_str().ok())
        .filter(|value| value.len() <= 4_096)
        .ok_or(RegistryError::ClaimTraceUnavailable)?;
    let elements = forwarded.split(',').collect::<Vec<_>>();
    if elements.is_empty() || elements.len() > 32 {
        return Err(RegistryError::ClaimTraceUnavailable);
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
                return Err(RegistryError::ClaimTraceUnavailable);
            }
            let (name, value) = parameter
                .trim()
                .split_once('=')
                .ok_or(RegistryError::ClaimTraceUnavailable)?;
            if name.trim().eq_ignore_ascii_case("for")
                && forwarded_for.replace(value.trim()).is_some()
            {
                return Err(RegistryError::ClaimTraceUnavailable);
            }
        }
        current =
            parse_forwarded_socket(forwarded_for.ok_or(RegistryError::ClaimTraceUnavailable)?)?;
        consumed = true;
    }
    if !consumed {
        return Err(RegistryError::ClaimTraceUnavailable);
    }
    Ok(current)
}

fn parse_forwarded_socket(value: &str) -> Result<SocketAddr> {
    let value = if value.starts_with('"') && value.ends_with('"') && value.len() >= 2 {
        &value[1..value.len() - 1]
    } else if value.contains('"') {
        return Err(RegistryError::ClaimTraceUnavailable);
    } else {
        value
    };
    if value.is_empty()
        || value
            .bytes()
            .any(|byte| byte.is_ascii_control() || byte == b'\\')
    {
        return Err(RegistryError::ClaimTraceUnavailable);
    }
    let address = value
        .parse::<SocketAddr>()
        .map_err(|_| RegistryError::ClaimTraceUnavailable)?;
    if address.port() == 0 || address.ip().is_unspecified() || address.ip().is_multicast() {
        return Err(RegistryError::ClaimTraceUnavailable);
    }
    Ok(address)
}

fn now_ms() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|duration| u64::try_from(duration.as_millis()).unwrap_or(u64::MAX))
        .unwrap_or(0)
}

fn parse_hex32(input: &str) -> Option<[u8; 32]> {
    parse_hex(input, 32)?.try_into().ok()
}

fn parse_hex64(input: &str) -> Option<[u8; 64]> {
    parse_hex(input, 64)?.try_into().ok()
}

fn parse_hex(input: &str, len: usize) -> Option<Vec<u8>> {
    if input.len() != len * 2
        || input
            .bytes()
            .any(|byte| !byte.is_ascii_digit() && !(b'a'..=b'f').contains(&byte))
    {
        return None;
    }
    input
        .as_bytes()
        .chunks_exact(2)
        .map(|chunk| u8::from_str_radix(std::str::from_utf8(chunk).ok()?, 16).ok())
        .collect()
}

fn validate_directory_publishers(publishers: &[VerifyingKey]) -> Result<()> {
    if publishers.len() > 64
        || publishers
            .iter()
            .any(|publisher| publisher.as_bytes() == &[0u8; 32] || publisher.is_weak())
    {
        return Err(RegistryError::InvalidConfiguration(
            "directory publisher allowlist is invalid".into(),
        ));
    }
    for (index, publisher) in publishers.iter().enumerate() {
        if publishers[..index]
            .iter()
            .any(|seen| seen.as_bytes() == publisher.as_bytes())
        {
            return Err(RegistryError::InvalidConfiguration(
                "directory publisher allowlist contains a duplicate key".into(),
            ));
        }
    }
    Ok(())
}

fn authenticate_directory_publisher(
    config: &RegistryHttpConfig,
    registry_origin: &str,
    headers: &HeaderMap,
    operation: DirectoryMutationOperation,
    body: &[u8],
) -> Result<()> {
    let key = unique_header(headers, DIRECTORY_PUBLISHER_KEY_HEADER)
        .and_then(parse_hex32)
        .ok_or(RegistryError::DirectoryPublisherUnauthorized)?;
    let publisher = config
        .directory_publishers
        .iter()
        .find(|publisher| publisher.as_bytes() == &key)
        .ok_or(RegistryError::DirectoryPublisherUnauthorized)?;
    let signature = unique_header(headers, DIRECTORY_PUBLISHER_SIGNATURE_HEADER)
        .and_then(parse_hex64)
        .map(|bytes| Signature::from_bytes(&bytes))
        .ok_or(RegistryError::DirectoryPublisherUnauthorized)?;
    let request = mutation_request_bytes(registry_origin, operation, body)
        .ok_or(RegistryError::DirectoryPublisherUnauthorized)?;
    publisher
        .verify_strict(&request, &signature)
        .map_err(|_| RegistryError::DirectoryPublisherUnauthorized)
}

fn unique_header<'a>(headers: &'a HeaderMap, name: &'static str) -> Option<&'a str> {
    let mut values = headers.get_all(name).iter();
    let value = values.next()?;
    if values.next().is_some() {
        return None;
    }
    value.to_str().ok()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::claim_trace::{
        ClaimTraceCapacity, ClaimTraceError, ClaimTraceInput, ClaimTraceSink,
    };
    use crate::entry::{directory_add_claim_payload, ComplianceKeyStatus};
    use crate::identity::{pkce_s256, IdentityProvider, MockProvider, Subject};
    use crate::registry::CommitTestBarrier;
    use crate::{Checkpoint, RegistrationLimits, WitnessKey, WitnessPolicy, WitnessReceipt};
    use ed25519_dalek::{Signer, SigningKey};
    use pigeonpost_compliance_format::{
        trace_epoch_end_ms, CompliancePurpose, Jurisdiction, TraceCapturePolicy,
        TraceRetentionPolicy, TRACE_EPOCH_DURATION_MS,
    };
    use pigeonpost_compliance_seal::MAX_TRACE_STORAGE_BYTES;

    const TEST_WITNESS_NAME: &str = "witness.test";
    const TEST_PUBLISHER_SEED: [u8; 32] = [45; 32];

    fn publisher_key() -> SigningKey {
        SigningKey::from_bytes(&TEST_PUBLISHER_SEED)
    }

    fn publisher_config() -> RegistryHttpConfig {
        RegistryHttpConfig::direct()
            .with_directory_publishers(vec![publisher_key().verifying_key()])
            .unwrap()
    }

    fn test_trace_policy() -> TraceRetentionPolicy {
        TraceRetentionPolicy {
            jurisdiction: Jurisdiction::Test,
            capture: TraceCapturePolicy::Standing,
            retention_days: None,
        }
    }

    #[derive(Debug)]
    struct ContractClaimTraceSink(Option<ClaimTraceCapacity>);

    impl ClaimTraceSink for ContractClaimTraceSink {
        fn capacity_contract(&self) -> Option<ClaimTraceCapacity> {
            self.0
        }

        fn readiness(&self, _now_ms: u64) -> std::result::Result<(), ClaimTraceError> {
            Ok(())
        }

        fn capture(&self, _input: ClaimTraceInput) -> std::result::Result<(), ClaimTraceError> {
            Ok(())
        }

        fn shutdown(&self, _timestamp_ms: u64) -> std::result::Result<(), ClaimTraceError> {
            Ok(())
        }
    }

    #[derive(Debug)]
    struct DeterministicGithubProvider;

    #[async_trait::async_trait]
    impl IdentityProvider for DeterministicGithubProvider {
        fn namespace(&self) -> &'static str {
            "github"
        }

        fn public_client_id(&self) -> Option<&str> {
            Some("registry-timeout-test-client")
        }

        async fn verify(&self, proof: &ProofPayload) -> Result<Subject> {
            if !matches!(proof, ProofPayload::Github { .. }) {
                return Err(RegistryError::WrongProvider);
            }
            Ok(Subject {
                namespace: "github",
                name: "alice".into(),
                opaque_id: "stable-alice".into(),
            })
        }
    }

    fn test_server(mut limits: RegistryLimits) -> Arc<RegistryServer> {
        if limits.blocking_timeout_ms == 0 {
            limits.blocking_timeout_ms = 1_000;
        }
        let registry = Arc::new(
            Registry::in_memory(crate::RegistryConfig {
                origin: "pigeonpost.dev/registry-test".into(),
                signing_key: SigningKey::from_bytes(&[42; 32]),
                allow_mock_identities: false,
            })
            .unwrap(),
        );
        Arc::new(
            RegistryServer::new(
                registry,
                RegistryHttpConfig::direct().with_limits(limits).unwrap(),
            )
            .unwrap(),
        )
    }

    fn witnessed_test_server(mut limits: RegistryLimits) -> (Arc<RegistryServer>, SigningKey) {
        if limits.blocking_timeout_ms == 0 {
            limits.blocking_timeout_ms = 1_000;
        }
        let witness_key = SigningKey::from_bytes(&[43; 32]);
        let policy = WitnessPolicy::new(
            vec![WitnessKey::new(TEST_WITNESS_NAME, witness_key.verifying_key()).unwrap()],
            1,
            120,
            10,
            0,
        )
        .unwrap();
        let registry = Arc::new(
            Registry::in_memory(crate::RegistryConfig {
                origin: "pigeonpost.dev/registry-witnessed-rate-test".into(),
                signing_key: SigningKey::from_bytes(&[44; 32]),
                allow_mock_identities: false,
            })
            .unwrap()
            .with_witness_policy(policy)
            .unwrap(),
        );
        promote_committed_head(&registry, &witness_key);
        (
            Arc::new(
                RegistryServer::new(registry, publisher_config().with_limits(limits).unwrap())
                    .unwrap(),
            ),
            witness_key,
        )
    }

    fn witnessed_identity_registry(contract: Option<ClaimTraceCapacity>) -> Arc<Registry> {
        let witness_key = SigningKey::from_bytes(&[46; 32]);
        let policy = WitnessPolicy::new(
            vec![WitnessKey::new("identity-witness.test", witness_key.verifying_key()).unwrap()],
            1,
            120,
            10,
            0,
        )
        .unwrap();
        let registry = Arc::new(
            Registry::in_memory(crate::RegistryConfig {
                origin: "pigeonpost.dev/registry-identity-capacity-test".into(),
                signing_key: SigningKey::from_bytes(&[47; 32]),
                allow_mock_identities: true,
            })
            .unwrap()
            .with_provider(Box::new(MockProvider))
            .with_claim_trace(Arc::new(ContractClaimTraceSink(contract)))
            .with_registration_limits(RegistrationLimits {
                global_bindings_per_minute: 2,
                account_bindings_per_minute: 10,
                max_account_keys: 8,
            })
            .unwrap()
            .with_witness_policy(policy)
            .unwrap(),
        );
        let witnessed_at = now_ms() / 1_000;
        let head = registry.committed_head().unwrap();
        let checkpoint = Checkpoint::verify(&head.checkpoint, &registry.verifying_key()).unwrap();
        let note = format!(
            "{}{}",
            head.checkpoint,
            checkpoint
                .cosignature_line("identity-witness.test", &witness_key, witnessed_at)
                .unwrap()
        );
        let receipt: WitnessReceipt = serde_json::from_value(serde_json::json!({
            "version": 1,
            "witness_name": "identity-witness.test",
            "origin": checkpoint.origin,
            "size": checkpoint.size,
            "root": checkpoint.root,
            "note": note,
            "witnessed_at": witnessed_at,
        }))
        .unwrap();
        registry
            .save_witness_receipt(&receipt, witnessed_at)
            .unwrap();
        assert!(registry.promote_witnessed_head(witnessed_at).unwrap());
        registry
    }

    fn promote_committed_head(registry: &Registry, witness_key: &SigningKey) {
        let witnessed_at = now_ms() / 1_000;
        let head = registry.committed_head().unwrap();
        let checkpoint = Checkpoint::verify(&head.checkpoint, &registry.verifying_key()).unwrap();
        let note = format!(
            "{}{}",
            head.checkpoint,
            checkpoint
                .cosignature_line(TEST_WITNESS_NAME, witness_key, witnessed_at)
                .unwrap()
        );
        let receipt: WitnessReceipt = serde_json::from_value(serde_json::json!({
            "version": 1,
            "witness_name": TEST_WITNESS_NAME,
            "origin": checkpoint.origin,
            "size": checkpoint.size,
            "root": checkpoint.root,
            "note": note,
            "witnessed_at": witnessed_at,
        }))
        .unwrap();
        registry
            .save_witness_receipt(&receipt, witnessed_at)
            .unwrap();
        assert!(registry.promote_witnessed_head(witnessed_at).unwrap());
    }

    fn signed_directory_add(key: &SigningKey, endpoint: &str) -> DirectoryAdd {
        let pubkey = hex(key.verifying_key().as_bytes());
        let payload =
            directory_add_claim_payload(endpoint, &pubkey, None, 1, 30, true, 0, 65_536, 1)
                .unwrap();
        DirectoryAdd::authenticated(
            endpoint.into(),
            pubkey,
            None,
            1,
            30,
            true,
            0,
            65_536,
            1,
            hex(&key.sign(&payload).to_bytes()),
        )
        .unwrap()
    }

    fn publisher_headers(
        advertised: &VerifyingKey,
        signer: &SigningKey,
        origin: &str,
        operation: DirectoryMutationOperation,
        body: &[u8],
    ) -> HeaderMap {
        let request = mutation_request_bytes(origin, operation, body).unwrap();
        let mut headers = HeaderMap::new();
        headers.insert(
            DIRECTORY_PUBLISHER_KEY_HEADER,
            HeaderValue::from_str(&hex(advertised.as_bytes())).unwrap(),
        );
        headers.insert(
            DIRECTORY_PUBLISHER_SIGNATURE_HEADER,
            HeaderValue::from_str(&hex(&signer.sign(&request).to_bytes())).unwrap(),
        );
        headers
    }

    async fn response_json(response: Response<Body>) -> serde_json::Value {
        let body = axum::body::to_bytes(response.into_body(), 1024 * 1024)
            .await
            .unwrap();
        serde_json::from_slice(&body).unwrap()
    }

    fn socket(value: &str) -> SocketAddr {
        value.parse().unwrap()
    }

    fn unwitnessed_registry(identity_enabled: bool) -> Arc<Registry> {
        let mut registry = Registry::in_memory(crate::RegistryConfig {
            origin: "pigeonpost.dev/registry-boundary-test".into(),
            signing_key: SigningKey::from_bytes(&[77; 32]),
            allow_mock_identities: identity_enabled,
        })
        .unwrap();
        if identity_enabled {
            registry = registry.with_provider(Box::new(MockProvider));
        }
        Arc::new(registry)
    }

    #[tokio::test]
    async fn non_loopback_unwitnessed_startup_is_rejected_from_the_bound_listener() {
        let listener = tokio::net::TcpListener::bind("0.0.0.0:0").await.unwrap();
        let (_stop, stopped) = tokio::sync::watch::channel(false);
        let error = serve_loopback_read_only(
            listener,
            unwitnessed_registry(false),
            publisher_config(),
            stopped,
        )
        .await
        .unwrap_err();
        assert!(matches!(error, RegistryError::InvalidConfiguration(_)));
    }

    #[cfg(feature = "test-utilities")]
    #[tokio::test]
    async fn test_utility_cannot_bind_a_non_loopback_listener() {
        let listener = tokio::net::TcpListener::bind("0.0.0.0:0").await.unwrap();
        let (_stop, stopped) = tokio::sync::watch::channel(false);
        let error = serve_loopback_test(
            listener,
            unwitnessed_registry(false),
            publisher_config(),
            stopped,
        )
        .await
        .unwrap_err();
        assert!(matches!(error, RegistryError::InvalidConfiguration(_)));
    }

    #[tokio::test]
    async fn identity_enabled_unwitnessed_startup_is_rejected_in_every_normal_mode() {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let (_stop, stopped) = tokio::sync::watch::channel(false);
        let error = serve_loopback_read_only(
            listener,
            unwitnessed_registry(true),
            RegistryHttpConfig::direct(),
            stopped,
        )
        .await
        .unwrap_err();
        assert!(matches!(error, RegistryError::InvalidConfiguration(_)));

        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let (_stop, stopped) = tokio::sync::watch::channel(false);
        let error = serve_witnessed(
            listener,
            unwitnessed_registry(true),
            publisher_config(),
            stopped,
            |_| async { Ok(()) },
        )
        .await
        .unwrap_err();
        assert!(matches!(error, RegistryError::InvalidConfiguration(_)));
    }

    #[tokio::test]
    async fn witnessed_identity_serving_rejects_missing_or_undersized_trace_contracts() {
        for contract in [
            None,
            Some(ClaimTraceCapacity {
                policy: test_trace_policy(),
                records_per_minute: 1,
                utc_epochs: 1,
                max_records_per_segment: 10_000,
                network_logical_limit_bytes: MAX_TRACE_STORAGE_BYTES,
                identity_logical_limit_bytes: MAX_TRACE_STORAGE_BYTES,
            }),
            Some(ClaimTraceCapacity {
                policy: test_trace_policy(),
                records_per_minute: 2,
                utc_epochs: 0,
                max_records_per_segment: 10_000,
                network_logical_limit_bytes: MAX_TRACE_STORAGE_BYTES,
                identity_logical_limit_bytes: MAX_TRACE_STORAGE_BYTES,
            }),
            Some(ClaimTraceCapacity {
                policy: test_trace_policy(),
                records_per_minute: 2,
                utc_epochs: 1,
                max_records_per_segment: 10_000,
                network_logical_limit_bytes: 0,
                identity_logical_limit_bytes: MAX_TRACE_STORAGE_BYTES,
            }),
            Some(ClaimTraceCapacity {
                policy: test_trace_policy(),
                records_per_minute: 2,
                utc_epochs: 1,
                max_records_per_segment: 10_000,
                network_logical_limit_bytes: MAX_TRACE_STORAGE_BYTES,
                identity_logical_limit_bytes: 0,
            }),
            Some(ClaimTraceCapacity {
                policy: test_trace_policy(),
                records_per_minute: 2,
                utc_epochs: 1,
                max_records_per_segment: 0,
                network_logical_limit_bytes: MAX_TRACE_STORAGE_BYTES,
                identity_logical_limit_bytes: MAX_TRACE_STORAGE_BYTES,
            }),
        ] {
            // Loopback remains a public serving boundary because production commonly exposes it
            // through a TLS proxy; it must not weaken the capacity contract.
            let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
            let (_stop, stopped) = tokio::sync::watch::channel(false);
            let error = serve_witnessed(
                listener,
                witnessed_identity_registry(contract),
                publisher_config(),
                stopped,
                |_| async { Ok(()) },
            )
            .await
            .unwrap_err();
            assert!(matches!(error, RegistryError::InvalidConfiguration(_)));
        }

        let exact = witnessed_identity_registry(Some(ClaimTraceCapacity {
            policy: test_trace_policy(),
            records_per_minute: 2,
            utc_epochs: 1,
            max_records_per_segment: 10_000,
            network_logical_limit_bytes: MAX_TRACE_STORAGE_BYTES,
            identity_logical_limit_bytes: MAX_TRACE_STORAGE_BYTES,
        }));
        assert!(exact.validate_registration_capacity_contract().is_ok());
        assert!(exact.validate_public_registration_capacity().is_err());

        let short_runway = witnessed_identity_registry(Some(ClaimTraceCapacity {
            policy: TraceRetentionPolicy {
                jurisdiction: Jurisdiction::Us,
                capture: TraceCapturePolicy::Standing,
                retention_days: Some(30),
            },
            records_per_minute: 2,
            utc_epochs: 30,
            max_records_per_segment: 10_000,
            network_logical_limit_bytes: MAX_TRACE_STORAGE_BYTES,
            identity_logical_limit_bytes: MAX_TRACE_STORAGE_BYTES,
        }));
        assert!(short_runway
            .validate_registration_capacity_contract()
            .is_err());

        let invalid_policy = witnessed_identity_registry(Some(ClaimTraceCapacity {
            policy: TraceRetentionPolicy {
                jurisdiction: Jurisdiction::Us,
                capture: TraceCapturePolicy::Standing,
                retention_days: Some(29),
            },
            records_per_minute: 2,
            utc_epochs: 31,
            max_records_per_segment: 10_000,
            network_logical_limit_bytes: MAX_TRACE_STORAGE_BYTES,
            identity_logical_limit_bytes: MAX_TRACE_STORAGE_BYTES,
        }));
        assert!(invalid_policy
            .validate_registration_capacity_contract()
            .is_err());
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn witnessed_startup_rejects_a_configured_but_unpublished_quorum() {
        let directory = tempfile::tempdir().unwrap();
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;

            let mut permissions = std::fs::metadata(directory.path()).unwrap().permissions();
            permissions.set_mode(0o700);
            std::fs::set_permissions(directory.path(), permissions).unwrap();
        }
        let database = directory.path().join("registry.sqlite3");
        let registry = Registry::open(
            database.to_str().unwrap(),
            crate::RegistryConfig {
                origin: "pigeonpost.dev/registry-stale-witness-test".into(),
                signing_key: SigningKey::from_bytes(&[79; 32]),
                allow_mock_identities: false,
            },
        )
        .unwrap();
        let witness = crate::WitnessKey::new(
            "independent",
            SigningKey::from_bytes(&[80; 32]).verifying_key(),
        )
        .unwrap();
        let policy = crate::WitnessPolicy::new(vec![witness], 1, 600, 30, 0).unwrap();
        let registry = Arc::new(registry.with_witness_policy(policy).unwrap());
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let (_stop, stopped) = tokio::sync::watch::channel(false);
        let error = serve_witnessed(listener, registry, publisher_config(), stopped, |_| async {
            Ok(())
        })
        .await
        .unwrap_err();
        assert!(matches!(error, RegistryError::WitnessUnavailable));
    }

    #[tokio::test]
    async fn witnessed_startup_rejects_an_empty_directory_publisher_allowlist() {
        let (state, _witness_key) = witnessed_test_server(RegistryLimits::default());
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let (_stop, stopped) = tokio::sync::watch::channel(false);
        let error = serve_witnessed(
            listener,
            Arc::clone(&state.registry),
            RegistryHttpConfig::direct(),
            stopped,
            |_| async { Ok(()) },
        )
        .await
        .unwrap_err();
        assert!(matches!(error, RegistryError::InvalidConfiguration(_)));
    }

    #[tokio::test]
    async fn loopback_read_only_mode_mounts_no_mutation_routes() {
        let registry = unwitnessed_registry(false);
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let base = format!("http://{}", listener.local_addr().unwrap());
        let (stop, stopped) = tokio::sync::watch::channel(false);
        let task = tokio::spawn(serve_loopback_read_only(
            listener,
            Arc::clone(&registry),
            RegistryHttpConfig::direct(),
            stopped,
        ));

        let response = reqwest::Client::new()
            .post(format!("{base}/v1/directory/add"))
            .json(&serde_json::json!({}))
            .send()
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::NOT_FOUND);
        assert_eq!(registry.size().unwrap(), 0);

        stop.send(true).unwrap();
        task.await.unwrap().unwrap();
    }

    #[test]
    fn untrusted_peers_cannot_spoof_forwarding_headers() {
        let mut headers = HeaderMap::new();
        headers.insert(FORWARDED, HeaderValue::from_static("for=203.0.113.9:9999"));
        headers.insert("x-forwarded-for", HeaderValue::from_static("203.0.113.10"));
        let connected = socket("198.51.100.7:44321");

        assert_eq!(
            trace_source(&RegistryHttpConfig::direct(), connected, &headers).unwrap(),
            connected
        );
    }

    #[test]
    fn trusted_proxy_chain_preserves_the_exact_client_port() {
        let config = RegistryHttpConfig::with_trusted_proxies(vec![
            "10.0.0.1".parse().unwrap(),
            "10.0.0.2".parse().unwrap(),
        ])
        .unwrap();
        let mut headers = HeaderMap::new();
        headers.insert(
            FORWARDED,
            HeaderValue::from_static("for=\"[2001:db8::42]:4567\";proto=https, for=10.0.0.1:8443"),
        );

        assert_eq!(
            trace_source(&config, socket("10.0.0.2:443"), &headers).unwrap(),
            socket("[2001:db8::42]:4567")
        );
    }

    #[test]
    fn trusted_proxy_input_fails_closed_when_ambiguous_or_portless() {
        let config =
            RegistryHttpConfig::with_trusted_proxies(vec!["10.0.0.1".parse().unwrap()]).unwrap();
        let connected = socket("10.0.0.1:443");

        let mut conflict = HeaderMap::new();
        conflict.insert(FORWARDED, HeaderValue::from_static("for=192.0.2.4:7654"));
        conflict.insert("x-forwarded-for", HeaderValue::from_static("192.0.2.5"));
        assert!(trace_source(&config, connected, &conflict).is_err());

        let mut portless = HeaderMap::new();
        portless.insert(FORWARDED, HeaderValue::from_static("for=192.0.2.4"));
        assert!(trace_source(&config, connected, &portless).is_err());

        let mut duplicate = HeaderMap::new();
        duplicate.insert(
            FORWARDED,
            HeaderValue::from_static("for=192.0.2.4:7654;for=192.0.2.5:7655"),
        );
        assert!(trace_source(&config, connected, &duplicate).is_err());
    }

    #[test]
    fn request_and_source_budgets_fail_closed_with_bounded_cardinality() {
        let limits = RegistryLimits {
            global_requests_per_minute: 2,
            source_challenges_per_minute: 1,
            source_bindings_per_minute: 1,
            max_source_keys: 2,
            ..RegistryLimits::default()
        };
        let limiter = RequestLimiter::new(limits);
        assert!(limiter.charge_global().is_ok());
        assert!(limiter.charge_global().is_ok());
        assert!(matches!(
            limiter.charge_global(),
            Err(RegistryError::RateLimited)
        ));

        let first: IpAddr = "192.0.2.1".parse().unwrap();
        let second: IpAddr = "192.0.2.2".parse().unwrap();
        let third: IpAddr = "192.0.2.3".parse().unwrap();
        assert!(limiter.charge_challenge(first).is_ok());
        assert!(matches!(
            limiter.charge_challenge(first),
            Err(RegistryError::RateLimited)
        ));
        assert!(limiter.charge_challenge(second).is_ok());
        assert!(matches!(
            limiter.charge_challenge(third),
            Err(RegistryError::RateLimited)
        ));
        assert_eq!(limiter.challenges.len(), 2);
    }

    #[test]
    fn global_response_byte_budget_is_exact_and_rolls_only_at_the_window_boundary() {
        let limiter = RequestLimiter::new(RegistryLimits {
            global_response_bytes_per_minute: 5,
            ..RegistryLimits::default()
        });
        let start = Instant::now();
        limiter.charge_response_bytes_at(3, start).unwrap();
        assert!(matches!(
            limiter.charge_response_bytes_at(3, start + Duration::from_secs(1)),
            Err(RegistryError::RateLimited)
        ));
        limiter
            .charge_response_bytes_at(2, start + Duration::from_secs(1))
            .unwrap();
        assert!(matches!(
            limiter.charge_response_bytes_at(1, start + Duration::from_secs(59)),
            Err(RegistryError::RateLimited)
        ));
        limiter
            .charge_response_bytes_at(5, start + RATE_WINDOW)
            .unwrap();
    }

    #[test]
    fn source_limiter_suppresses_full_map_scans_until_expiry_and_bounds_cleanup() {
        let buckets = SourceBuckets::new(2);
        let start = Instant::now();
        buckets
            .charge_at("192.0.2.1".parse().unwrap(), 10, start)
            .unwrap();
        buckets
            .charge_at("192.0.2.2".parse().unwrap(), 10, start)
            .unwrap();

        for suffix in 3..=253 {
            assert!(matches!(
                buckets.charge_at(
                    IpAddr::from([192, 0, 2, suffix]),
                    10,
                    start + Duration::from_secs(1),
                ),
                Err(RegistryError::RateLimited)
            ));
        }
        assert_eq!(buckets.len(), 2);
        assert_eq!(buckets.queued_expirations(), 2);
        assert_eq!(buckets.cleanup_passes(), 0);

        buckets
            .charge_at("198.51.100.1".parse().unwrap(), 10, start + RATE_WINDOW)
            .unwrap();
        assert_eq!(buckets.len(), 1);
        assert_eq!(buckets.queued_expirations(), 1);
        assert_eq!(buckets.cleanup_passes(), 1);
    }

    #[test]
    fn source_limiter_keeps_one_expiration_for_a_long_lived_key() {
        let buckets = SourceBuckets::new(1_000);
        let source = "192.0.2.1".parse().unwrap();
        let start = Instant::now();

        for window in 0..1_000_u32 {
            buckets
                .charge_at(source, 10, start + RATE_WINDOW * window)
                .unwrap();
        }

        assert_eq!(buckets.len(), 1);
        assert_eq!(buckets.queued_expirations(), 1);
        assert_eq!(buckets.cleanup_passes(), 999);
    }

    #[test]
    fn audited_registry_limit_maxima_reject_oversized_allocations_before_construction() {
        let maximum = RegistryLimits {
            max_concurrent_connections: MAX_REGISTRY_CONCURRENT_CONNECTIONS,
            max_concurrent_requests: MAX_REGISTRY_CONCURRENT_REQUESTS,
            max_blocking_operations: MAX_REGISTRY_BLOCKING_OPERATIONS,
            max_dump_streams: MAX_REGISTRY_DUMP_STREAMS,
            blocking_timeout_ms: MAX_REGISTRY_BLOCKING_TIMEOUT_MS,
            header_timeout_ms: MAX_REGISTRY_HEADER_TIMEOUT_MS,
            global_requests_per_minute: MAX_REGISTRY_GLOBAL_REQUESTS_PER_MINUTE,
            global_response_bytes_per_minute: MAX_REGISTRY_RESPONSE_BYTES_PER_MINUTE,
            source_challenges_per_minute: MAX_REGISTRY_SOURCE_REQUESTS_PER_MINUTE,
            source_bindings_per_minute: MAX_REGISTRY_SOURCE_REQUESTS_PER_MINUTE,
            max_source_keys: MAX_REGISTRY_SOURCE_KEYS,
        };
        assert!(validate_limits(maximum).is_ok());

        let mut oversized = maximum;
        oversized.max_concurrent_connections += 1;
        assert!(validate_limits(oversized).is_err());
        oversized = maximum;
        oversized.max_concurrent_requests += 1;
        assert!(validate_limits(oversized).is_err());
        oversized = maximum;
        oversized.max_blocking_operations += 1;
        assert!(validate_limits(oversized).is_err());
        oversized = maximum;
        oversized.max_dump_streams += 1;
        assert!(validate_limits(oversized).is_err());
        oversized = maximum;
        oversized.blocking_timeout_ms += 1;
        assert!(validate_limits(oversized).is_err());
        oversized = maximum;
        oversized.header_timeout_ms += 1;
        assert!(validate_limits(oversized).is_err());
        oversized = maximum;
        oversized.global_requests_per_minute += 1;
        assert!(validate_limits(oversized).is_err());
        oversized = maximum;
        oversized.source_challenges_per_minute += 1;
        assert!(validate_limits(oversized).is_err());
        oversized = maximum;
        oversized.source_bindings_per_minute += 1;
        assert!(validate_limits(oversized).is_err());
        oversized = maximum;
        oversized.max_source_keys += 1;
        assert!(validate_limits(oversized).is_err());
    }

    #[test]
    fn directory_publisher_allowlist_rejects_weak_and_duplicate_keys() {
        let weak_bytes = {
            let mut bytes = [0u8; 32];
            bytes[0] = 1;
            bytes
        };
        let weak = VerifyingKey::from_bytes(&weak_bytes).unwrap();
        assert!(weak.is_weak());
        assert!(RegistryHttpConfig::direct()
            .with_directory_publishers(vec![weak])
            .is_err());

        let publisher = publisher_key().verifying_key();
        assert!(RegistryHttpConfig::direct()
            .with_directory_publishers(vec![publisher, publisher])
            .is_err());
    }

    #[tokio::test]
    async fn directory_mutations_require_exact_origin_scoped_publisher_authorization() {
        let (state, witness_key) = witnessed_test_server(RegistryLimits::default());
        let source = socket("192.0.2.45:4045");
        let publisher = publisher_key();
        let attacker = SigningKey::from_bytes(&[46; 32]);
        let mutation = signed_directory_add(
            &SigningKey::from_bytes(&[47; 32]),
            "https://authorized-loft.example",
        );
        let body = serde_json::to_vec(&mutation).unwrap();
        let origin = state.registry.origin();

        assert!(matches!(
            directory_add(
                State(Arc::clone(&state)),
                ConnectInfo(RegistryConnection(source)),
                HeaderMap::new(),
                Bytes::from(body.clone()),
            )
            .await,
            Err(RegistryError::DirectoryPublisherUnauthorized)
        ));

        let unknown = publisher_headers(
            &attacker.verifying_key(),
            &attacker,
            origin,
            DirectoryMutationOperation::Add,
            &body,
        );
        assert!(matches!(
            directory_add(
                State(Arc::clone(&state)),
                ConnectInfo(RegistryConnection(source)),
                unknown,
                Bytes::from(body.clone()),
            )
            .await,
            Err(RegistryError::DirectoryPublisherUnauthorized)
        ));

        let forged = publisher_headers(
            &publisher.verifying_key(),
            &attacker,
            origin,
            DirectoryMutationOperation::Add,
            &body,
        );
        assert!(matches!(
            directory_add(
                State(Arc::clone(&state)),
                ConnectInfo(RegistryConnection(source)),
                forged,
                Bytes::from(body.clone()),
            )
            .await,
            Err(RegistryError::DirectoryPublisherUnauthorized)
        ));

        let swapped = publisher_headers(
            &publisher.verifying_key(),
            &publisher,
            origin,
            DirectoryMutationOperation::Remove,
            &body,
        );
        assert!(matches!(
            directory_add(
                State(Arc::clone(&state)),
                ConnectInfo(RegistryConnection(source)),
                swapped,
                Bytes::from(body.clone()),
            )
            .await,
            Err(RegistryError::DirectoryPublisherUnauthorized)
        ));

        let wrong_origin = publisher_headers(
            &publisher.verifying_key(),
            &publisher,
            "staging.pigeonpost/registry",
            DirectoryMutationOperation::Add,
            &body,
        );
        assert!(matches!(
            directory_add(
                State(Arc::clone(&state)),
                ConnectInfo(RegistryConnection(source)),
                wrong_origin,
                Bytes::from(body.clone()),
            )
            .await,
            Err(RegistryError::DirectoryPublisherUnauthorized)
        ));
        assert_eq!(state.registry.committed_size().unwrap(), 0);

        let valid = publisher_headers(
            &publisher.verifying_key(),
            &publisher,
            origin,
            DirectoryMutationOperation::Add,
            &body,
        );
        let mut duplicate = valid.clone();
        duplicate.append(
            axum::http::HeaderName::from_static(DIRECTORY_PUBLISHER_KEY_HEADER),
            HeaderValue::from_str(&hex(publisher.verifying_key().as_bytes())).unwrap(),
        );
        assert!(matches!(
            directory_add(
                State(Arc::clone(&state)),
                ConnectInfo(RegistryConnection(source)),
                duplicate,
                Bytes::from(body.clone()),
            )
            .await,
            Err(RegistryError::DirectoryPublisherUnauthorized)
        ));
        assert_eq!(state.registry.committed_size().unwrap(), 0);

        let pending = directory_add(
            State(Arc::clone(&state)),
            ConnectInfo(RegistryConnection(source)),
            valid.clone(),
            Bytes::from(body.clone()),
        )
        .await
        .unwrap();
        let pending = response_json(pending).await;
        assert_eq!(pending["appended"], true);
        assert_eq!(state.registry.committed_size().unwrap(), 1);

        promote_committed_head(&state.registry, &witness_key);
        let retry = directory_add(
            State(Arc::clone(&state)),
            ConnectInfo(RegistryConnection(source)),
            valid,
            Bytes::from(body),
        )
        .await
        .unwrap();
        let retry = response_json(retry).await;
        assert_eq!(retry["appended"], false);
        assert_eq!(state.registry.committed_size().unwrap(), 1);
        assert_eq!(state.registry.size().unwrap(), 1);
    }

    #[tokio::test(flavor = "current_thread")]
    async fn directory_publisher_crypto_is_source_gated_and_blocking_bounded() {
        let (state, _) = witnessed_test_server(RegistryLimits {
            max_blocking_operations: 1,
            source_bindings_per_minute: 1,
            ..RegistryLimits::default()
        });
        let publisher = publisher_key();
        let attacker = SigningKey::from_bytes(&[46; 32]);
        let mutation = signed_directory_add(
            &SigningKey::from_bytes(&[47; 32]),
            "https://bounded-auth.example",
        );
        let body = Bytes::from(serde_json::to_vec(&mutation).unwrap());
        let forged = || {
            publisher_headers(
                &publisher.verifying_key(),
                &attacker,
                state.registry.origin(),
                DirectoryMutationOperation::Add,
                &body,
            )
        };

        let source = socket("192.0.2.90:4090");
        assert!(matches!(
            directory_add(
                State(Arc::clone(&state)),
                ConnectInfo(RegistryConnection(source)),
                forged(),
                body.clone(),
            )
            .await,
            Err(RegistryError::DirectoryPublisherUnauthorized)
        ));
        assert!(matches!(
            directory_add(
                State(Arc::clone(&state)),
                ConnectInfo(RegistryConnection(source)),
                forged(),
                body.clone(),
            )
            .await,
            Err(RegistryError::RateLimited)
        ));

        let held = Arc::clone(&state.blocking).try_acquire_owned().unwrap();
        let heartbeat = tokio::spawn(async {
            tokio::task::yield_now().await;
            true
        });
        assert!(matches!(
            directory_add(
                State(Arc::clone(&state)),
                ConnectInfo(RegistryConnection(socket("192.0.2.91:4091"))),
                forged(),
                body,
            )
            .await,
            Err(RegistryError::Overloaded)
        ));
        assert!(heartbeat.await.unwrap());
        drop(held);
        assert_eq!(state.registry.committed_size().unwrap(), 0);
    }

    #[tokio::test]
    async fn botnet_style_loft_self_signatures_cannot_consume_the_registry_log() {
        let (state, _witness_key) = witnessed_test_server(RegistryLimits {
            global_requests_per_minute: 10_000,
            source_bindings_per_minute: 10_000,
            ..RegistryLimits::default()
        });
        let source = socket("192.0.2.46:4046");
        for seed in (1u8..=128).filter(|seed| *seed != TEST_PUBLISHER_SEED[0]) {
            let loft = SigningKey::from_bytes(&[seed; 32]);
            let mutation = signed_directory_add(&loft, &format!("https://botnet-{seed}.example"));
            let body = serde_json::to_vec(&mutation).unwrap();
            let headers = publisher_headers(
                &loft.verifying_key(),
                &loft,
                state.registry.origin(),
                DirectoryMutationOperation::Add,
                &body,
            );
            assert!(matches!(
                directory_add(
                    State(Arc::clone(&state)),
                    ConnectInfo(RegistryConnection(source)),
                    headers,
                    Bytes::from(body),
                )
                .await,
                Err(RegistryError::DirectoryPublisherUnauthorized)
            ));
        }
        assert_eq!(state.registry.committed_size().unwrap(), 0);
        assert_eq!(state.registry.size().unwrap(), 0);
    }

    #[tokio::test]
    async fn consistency_requires_an_exact_published_ordered_range() {
        let (state, witness_key) = witnessed_test_server(RegistryLimits::default());
        state
            .registry
            .append_directory_add(signed_directory_add(
                &SigningKey::from_bytes(&[115; 32]),
                "https://consistency-one.example",
            ))
            .unwrap();
        state
            .registry
            .append_directory_add(signed_directory_add(
                &SigningKey::from_bytes(&[116; 32]),
                "https://consistency-two.example",
            ))
            .unwrap();

        assert!(matches!(
            consistency(
                State(Arc::clone(&state)),
                Query(ConsistencyQuery { from: 1, to: 2 }),
                HeaderMap::new(),
            )
            .await,
            Err(RegistryError::MalformedEntry(_))
        ));

        promote_committed_head(&state.registry, &witness_key);
        let response = consistency(
            State(Arc::clone(&state)),
            Query(ConsistencyQuery { from: 1, to: 2 }),
            HeaderMap::new(),
        )
        .await
        .unwrap();
        let response = response_json(response).await;
        assert_eq!(response["from"], 1);
        assert_eq!(response["to"], 2);
        assert!(response["path"]
            .as_array()
            .is_some_and(|path| !path.is_empty()));

        for query in [
            ConsistencyQuery { from: 2, to: 1 },
            ConsistencyQuery { from: 1, to: 3 },
            ConsistencyQuery { from: 0, to: 1 },
        ] {
            assert!(matches!(
                consistency(State(Arc::clone(&state)), Query(query), HeaderMap::new(),).await,
                Err(RegistryError::MalformedEntry(_))
            ));
        }
    }

    #[tokio::test]
    async fn default_binding_budget_covers_witnessed_directory_two_phase_mutations() {
        let limits = RegistryLimits::default();
        assert_eq!(limits.source_bindings_per_minute, 40);
        let (state, witness_key) = witnessed_test_server(limits);
        let source = socket("192.0.2.40:4040");

        for mutation_number in 0u8..20 {
            let endpoint = format!("https://loft-{mutation_number}.example");
            let mutation = signed_directory_add(
                &SigningKey::from_bytes(&[mutation_number.saturating_add(1); 32]),
                &endpoint,
            );
            let body = Bytes::from(serde_json::to_vec(&mutation).unwrap());
            let headers = publisher_headers(
                &publisher_key().verifying_key(),
                &publisher_key(),
                state.registry.origin(),
                DirectoryMutationOperation::Add,
                &body,
            );
            let pending = directory_add(
                State(Arc::clone(&state)),
                ConnectInfo(RegistryConnection(source)),
                headers,
                body.clone(),
            )
            .await
            .unwrap();
            let pending = response_json(pending).await;
            assert_eq!(
                pending["log_index"].as_u64(),
                Some(u64::from(mutation_number))
            );
            assert!(
                pending["log_index"].as_u64() >= pending["inclusion_proof"]["tree_size"].as_u64()
            );

            promote_committed_head(&state.registry, &witness_key);

            let headers = publisher_headers(
                &publisher_key().verifying_key(),
                &publisher_key(),
                state.registry.origin(),
                DirectoryMutationOperation::Add,
                &body,
            );
            let final_response = directory_add(
                State(Arc::clone(&state)),
                ConnectInfo(RegistryConnection(source)),
                headers,
                body,
            )
            .await
            .unwrap();
            let final_response = response_json(final_response).await;
            assert_eq!(final_response["appended"].as_bool(), Some(false));
            assert!(
                final_response["log_index"].as_u64()
                    < final_response["inclusion_proof"]["tree_size"].as_u64()
            );
        }

        let overflow = signed_directory_add(
            &SigningKey::from_bytes(&[99; 32]),
            "https://loft-overflow.example",
        );
        let body = Bytes::from(serde_json::to_vec(&overflow).unwrap());
        let headers = publisher_headers(
            &publisher_key().verifying_key(),
            &publisher_key(),
            state.registry.origin(),
            DirectoryMutationOperation::Add,
            &body,
        );
        assert!(matches!(
            directory_add(
                State(Arc::clone(&state)),
                ConnectInfo(RegistryConnection(source)),
                headers,
                body,
            )
            .await,
            Err(RegistryError::RateLimited)
        ));
        assert_eq!(state.registry.committed_size().unwrap(), 20);
        assert_eq!(state.registry.size().unwrap(), 20);
    }

    #[test]
    fn request_concurrency_rejects_instead_of_queueing() {
        let state = test_server(RegistryLimits {
            max_concurrent_requests: 1,
            max_dump_streams: 1,
            ..RegistryLimits::default()
        });
        let first = state.try_enter().unwrap();
        assert!(matches!(state.try_enter(), Err(RegistryError::Overloaded)));
        drop(first);
        assert!(state.try_enter().is_ok());

        let first_stream = state.try_stream().unwrap();
        assert!(matches!(state.try_stream(), Err(RegistryError::Overloaded)));
        let first_full_stream = state.try_full_stream().unwrap();
        assert!(matches!(
            state.try_full_stream(),
            Err(RegistryError::Overloaded)
        ));
        drop(first_stream);
        assert!(state.try_stream().is_ok());
        drop(first_full_stream);
        assert!(state.try_full_stream().is_ok());
    }

    #[tokio::test]
    async fn exact_dump_ranges_are_immutable_and_query_free_dump_remains_complete() {
        let state = test_server(RegistryLimits::default());
        state
            .registry
            .append_directory_add(signed_directory_add(
                &SigningKey::from_bytes(&[104u8; 32]),
                "https://range-one.example",
            ))
            .unwrap();

        assert!(matches!(
            dump(
                State(Arc::clone(&state)),
                Query(DumpQuery {
                    from: Some(0),
                    to: None,
                }),
                HeaderMap::new(),
            )
            .await,
            Err(RegistryError::MalformedEntry(_))
        ));
        assert!(matches!(
            dump(
                State(Arc::clone(&state)),
                Query(DumpQuery {
                    from: Some(0),
                    to: Some(2),
                }),
                HeaderMap::new(),
            )
            .await,
            Err(RegistryError::NotFound)
        ));

        // A mirror occupying the independently bounded full-dump lane cannot consume a product
        // range permit.
        let full_dump_permit = state.try_full_stream().unwrap();
        let first = dump(
            State(Arc::clone(&state)),
            Query(DumpQuery {
                from: Some(0),
                to: Some(1),
            }),
            HeaderMap::new(),
        )
        .await
        .unwrap();
        drop(full_dump_permit);
        assert_eq!(
            first.headers()[CACHE_CONTROL],
            HeaderValue::from_static(RANGE_DUMP_CACHE)
        );
        let first_etag = first.headers()[ETAG].clone();
        let first_body = axum::body::to_bytes(first.into_body(), 1024 * 1024)
            .await
            .unwrap();
        let first_lines = first_body
            .split(|byte| *byte == b'\n')
            .filter(|line| !line.is_empty())
            .collect::<Vec<_>>();
        assert_eq!(first_lines.len(), 1);
        let first_entry: LogEntry = serde_json::from_slice(first_lines[0]).unwrap();
        assert_eq!(first_entry.seq(), 0);

        state
            .registry
            .append_directory_add(signed_directory_add(
                &SigningKey::from_bytes(&[105u8; 32]),
                "https://range-two.example",
            ))
            .unwrap();
        let repeated = dump(
            State(Arc::clone(&state)),
            Query(DumpQuery {
                from: Some(0),
                to: Some(1),
            }),
            HeaderMap::new(),
        )
        .await
        .unwrap();
        assert_eq!(repeated.headers()[ETAG], first_etag);
        let repeated_body = axum::body::to_bytes(repeated.into_body(), 1024 * 1024)
            .await
            .unwrap();
        assert_eq!(repeated_body, first_body);
        let mut conditional_headers = HeaderMap::new();
        conditional_headers.insert(IF_NONE_MATCH, first_etag);
        let not_changed = dump(
            State(Arc::clone(&state)),
            Query(DumpQuery {
                from: Some(0),
                to: Some(1),
            }),
            conditional_headers,
        )
        .await
        .unwrap();
        assert_eq!(not_changed.status(), StatusCode::NOT_MODIFIED);
        assert_eq!(
            not_changed.headers()[CACHE_CONTROL],
            HeaderValue::from_static(RANGE_DUMP_CACHE)
        );

        let complete = dump(
            State(Arc::clone(&state)),
            Query(DumpQuery::default()),
            HeaderMap::new(),
        )
        .await
        .unwrap();
        assert_eq!(
            complete.headers()[CACHE_CONTROL],
            HeaderValue::from_static(DUMP_CACHE)
        );
        let complete_body = axum::body::to_bytes(complete.into_body(), 1024 * 1024)
            .await
            .unwrap();
        assert_eq!(
            complete_body
                .split(|byte| *byte == b'\n')
                .filter(|line| !line.is_empty())
                .count(),
            2
        );
    }

    #[tokio::test(flavor = "current_thread")]
    async fn compliance_projection_serialization_does_not_stall_the_async_executor() {
        let state = test_server(RegistryLimits::default());
        let current = now_ms();
        let start = current - current % TRACE_EPOCH_DURATION_MS;
        let key_id = ComplianceKeyId::new(
            CompliancePurpose::NetworkTrace,
            Jurisdiction::Test,
            [118; 32],
            start,
            1,
        );
        let end = trace_epoch_end_ms(&key_id).unwrap();
        state
            .registry
            .publish_compliance_key(ComplianceKeyPublish {
                key_id,
                public_key: "77".repeat(32),
                not_before_ms: start,
                not_after_ms: end,
                status: ComplianceKeyStatus::Active,
            })
            .unwrap();
        let (barrier, reached, release) = SerializationTestBarrier::new();
        state.install_serialization_test_barrier(barrier);

        let response = tokio::spawn({
            let state = Arc::clone(&state);
            async move {
                compliance_keys(
                    State(state),
                    Query(ComplianceKeysQuery::default()),
                    HeaderMap::new(),
                )
                .await
            }
        });
        tokio::time::timeout(Duration::from_secs(1), reached)
            .await
            .expect("compliance encoding must reach the blocking-lane barrier")
            .unwrap();
        tokio::time::timeout(Duration::from_millis(100), tokio::task::yield_now())
            .await
            .expect("blocking compliance encoding must leave the executor responsive");
        release.send(()).unwrap();
        let response = tokio::time::timeout(Duration::from_secs(1), response)
            .await
            .expect("compliance response must finish after serialization resumes")
            .unwrap()
            .unwrap();
        let bytes = axum::body::to_bytes(response.into_body(), 1024 * 1024)
            .await
            .unwrap();
        assert!(!bytes.is_empty());
    }

    #[tokio::test(flavor = "current_thread")]
    async fn dump_page_serialization_does_not_stall_the_async_executor() {
        let state = test_server(RegistryLimits::default());
        state
            .registry
            .append_directory_add(signed_directory_add(
                &SigningKey::from_bytes(&[117; 32]),
                "https://serialization.example",
            ))
            .unwrap();
        let (barrier, reached, release) = SerializationTestBarrier::new();
        state.install_serialization_test_barrier(barrier);

        let response = dump(
            State(Arc::clone(&state)),
            Query(DumpQuery {
                from: Some(0),
                to: Some(1),
            }),
            HeaderMap::new(),
        )
        .await
        .unwrap();
        let drain =
            tokio::spawn(
                async move { axum::body::to_bytes(response.into_body(), 1024 * 1024).await },
            );
        tokio::time::timeout(Duration::from_secs(1), reached)
            .await
            .expect("dump encoding must reach the blocking-lane barrier")
            .unwrap();
        tokio::time::timeout(Duration::from_millis(100), tokio::task::yield_now())
            .await
            .expect("blocking dump encoding must leave the current-thread executor responsive");
        release.send(()).unwrap();
        let bytes = tokio::time::timeout(Duration::from_secs(1), drain)
            .await
            .expect("dump must finish after serialization resumes")
            .unwrap()
            .unwrap();
        assert!(!bytes.is_empty());
    }

    #[tokio::test]
    async fn stalled_dump_stream_releases_its_permit_at_the_idle_deadline() {
        let state = test_server(RegistryLimits {
            max_dump_streams: 1,
            ..RegistryLimits::default()
        });
        let permit = state.try_stream().unwrap();
        let (sender, _receiver) = tokio::sync::mpsc::channel(1);
        sender
            .send(Ok(Bytes::from_static(b"already buffered")))
            .await
            .unwrap();
        let task = tokio::spawn(async move {
            let _permit = permit;
            send_dump_chunk_with_idle(
                &sender,
                Bytes::from_static(b"blocked"),
                Some(tokio::time::Instant::now() + Duration::from_secs(1)),
                Duration::from_millis(20),
            )
            .await
        });

        assert!(matches!(state.try_stream(), Err(RegistryError::Overloaded)));
        assert!(!tokio::time::timeout(Duration::from_secs(1), task)
            .await
            .expect("idle deadline must end the stream task")
            .unwrap());
        assert!(state.try_stream().is_ok());
    }

    #[tokio::test]
    async fn dump_lanes_follow_downstream_body_eof_or_drop() {
        let state = test_server(RegistryLimits {
            max_dump_streams: 1,
            ..RegistryLimits::default()
        });
        state
            .registry
            .append_directory_add(signed_directory_add(
                &SigningKey::from_bytes(&[106u8; 32]),
                "https://body-lifetime.example",
            ))
            .unwrap();

        let range = dump(
            State(Arc::clone(&state)),
            Query(DumpQuery {
                from: Some(0),
                to: Some(1),
            }),
            HeaderMap::new(),
        )
        .await
        .unwrap();
        tokio::task::yield_now().await;
        assert!(matches!(state.try_stream(), Err(RegistryError::Overloaded)));
        drop(range);
        assert!(state.try_stream().is_ok());

        let full = dump(
            State(Arc::clone(&state)),
            Query(DumpQuery::default()),
            HeaderMap::new(),
        )
        .await
        .unwrap();
        tokio::task::yield_now().await;
        assert!(matches!(
            state.try_full_stream(),
            Err(RegistryError::Overloaded)
        ));
        let bytes = axum::body::to_bytes(full.into_body(), 1024 * 1024)
            .await
            .unwrap();
        assert!(!bytes.is_empty());
        assert!(state.try_full_stream().is_ok());
    }

    #[tokio::test]
    async fn dump_stream_fails_closed_at_the_global_response_byte_rate() {
        let state = test_server(RegistryLimits {
            max_dump_streams: 1,
            global_response_bytes_per_minute: 1,
            ..RegistryLimits::default()
        });
        state
            .registry
            .append_directory_add(signed_directory_add(
                &SigningKey::from_bytes(&[108u8; 32]),
                "https://egress-budget.example",
            ))
            .unwrap();

        let response = dump(
            State(Arc::clone(&state)),
            Query(DumpQuery {
                from: Some(0),
                to: Some(1),
            }),
            HeaderMap::new(),
        )
        .await
        .unwrap();
        assert!(axum::body::to_bytes(response.into_body(), 1024 * 1024)
            .await
            .is_err());
        assert!(state.try_stream().is_ok());
    }

    #[tokio::test]
    async fn response_deadline_and_byte_budget_release_the_body_permit() {
        let timed = Arc::new(Semaphore::new(1));
        let response = hold_response_permit(
            Response::new(Body::from("held")),
            Arc::clone(&timed).try_acquire_owned().unwrap(),
            Some(tokio::time::Instant::now() + Duration::from_millis(20)),
            Some(4),
        );
        assert_eq!(timed.available_permits(), 0);
        tokio::time::sleep(Duration::from_millis(50)).await;
        assert!(axum::body::to_bytes(response.into_body(), 1024)
            .await
            .is_err());
        assert_eq!(timed.available_permits(), 1);

        let budgeted = Arc::new(Semaphore::new(1));
        let response = hold_response_permit(
            Response::new(Body::from("too large")),
            Arc::clone(&budgeted).try_acquire_owned().unwrap(),
            None,
            Some(3),
        );
        assert!(axum::body::to_bytes(response.into_body(), 1024)
            .await
            .is_err());
        assert_eq!(budgeted.available_permits(), 1);
    }

    #[tokio::test]
    async fn dropping_many_deadlined_bodies_releases_every_permit_without_watchdog_tasks() {
        let permits = Arc::new(Semaphore::new(1));
        for _ in 0..10_000 {
            let response = hold_response_permit(
                Response::new(Body::from("held")),
                Arc::clone(&permits).try_acquire_owned().unwrap(),
                Some(tokio::time::Instant::now() + Duration::from_secs(60)),
                Some(4),
            );
            drop(response);
        }
        tokio::task::yield_now().await;
        assert_eq!(permits.available_permits(), 1);
    }

    #[tokio::test]
    async fn transport_write_idle_timeout_catches_a_stalled_final_frame() {
        use tokio::io::AsyncWriteExt;

        let (server, _stalled_reader) = tokio::io::duplex(1);
        let mut transport = IdleWriteIo::new(server, Duration::from_millis(20));
        let error = tokio::time::timeout(Duration::from_secs(1), transport.write_all(b"final"))
            .await
            .expect("transport watchdog must fire")
            .unwrap_err();
        assert_eq!(error.kind(), std::io::ErrorKind::TimedOut);
    }

    #[tokio::test]
    async fn incomplete_headers_are_closed_by_the_timer_backed_transport_deadline() {
        use tokio::io::{AsyncReadExt, AsyncWriteExt};

        let registry = unwitnessed_registry(false);
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        let config = RegistryHttpConfig::direct()
            .with_limits(RegistryLimits {
                header_timeout_ms: 50,
                ..RegistryLimits::default()
            })
            .unwrap();
        let (stop, stopped) = tokio::sync::watch::channel(false);
        let server = tokio::spawn(serve_loopback_read_only(
            listener, registry, config, stopped,
        ));

        let mut peer = tokio::net::TcpStream::connect(address).await.unwrap();
        peer.write_all(b"GET /health HTTP/1.1\r\nHost: registry")
            .await
            .unwrap();
        let mut response = Vec::new();
        let closed =
            tokio::time::timeout(Duration::from_secs(1), peer.read_to_end(&mut response)).await;
        assert!(
            closed.is_ok(),
            "partial headers must not retain a connection"
        );

        stop.send(true).unwrap();
        server.await.unwrap().unwrap();
    }

    #[tokio::test]
    async fn http2_multiplexing_is_rejected_before_full_dump_admission() {
        use tokio::io::{AsyncReadExt, AsyncWriteExt};

        let registry = unwitnessed_registry(false);
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        let (stop, stopped) = tokio::sync::watch::channel(false);
        let server = tokio::spawn(serve_loopback_read_only(
            listener,
            registry,
            RegistryHttpConfig::direct(),
            stopped,
        ));

        let mut h2 = tokio::net::TcpStream::connect(address).await.unwrap();
        h2.write_all(b"PRI * HTTP/2.0\r\n\r\nSM\r\n\r\n")
            .await
            .unwrap();
        h2.shutdown().await.unwrap();
        let mut rejection = Vec::new();
        tokio::time::timeout(Duration::from_secs(1), h2.read_to_end(&mut rejection))
            .await
            .expect("the HTTP/1.1-only origin must close an HTTP/2 preface")
            .unwrap();
        assert!(
            rejection.is_empty() || rejection.starts_with(b"HTTP/1.1"),
            "the Registry origin must never negotiate an HTTP/2 SETTINGS frame"
        );

        let mut http1 = tokio::net::TcpStream::connect(address).await.unwrap();
        http1
            .write_all(b"GET /health HTTP/1.1\r\nHost: registry\r\nConnection: close\r\n\r\n")
            .await
            .unwrap();
        let mut response = Vec::new();
        tokio::time::timeout(Duration::from_secs(1), http1.read_to_end(&mut response))
            .await
            .expect("rejecting HTTP/2 must not damage HTTP/1.1 service")
            .unwrap();
        assert!(response.starts_with(b"HTTP/1.1 200"));

        stop.send(true).unwrap();
        server.await.unwrap().unwrap();
    }

    #[tokio::test]
    async fn accepted_connection_admission_is_fail_fast_and_non_queueing() {
        use tokio::io::{AsyncReadExt, AsyncWriteExt};

        let registry = unwitnessed_registry(false);
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        let config = RegistryHttpConfig::direct()
            .with_limits(RegistryLimits {
                max_concurrent_connections: 1,
                header_timeout_ms: 2_000,
                ..RegistryLimits::default()
            })
            .unwrap();
        let (stop, stopped) = tokio::sync::watch::channel(false);
        let server = tokio::spawn(serve_loopback_read_only(
            listener, registry, config, stopped,
        ));

        let mut occupied = tokio::net::TcpStream::connect(address).await.unwrap();
        occupied
            .write_all(b"GET /health HTTP/1.1\r\nHost: occupied")
            .await
            .unwrap();
        tokio::time::sleep(Duration::from_millis(20)).await;
        let mut excess = tokio::net::TcpStream::connect(address).await.unwrap();
        let mut response = Vec::new();
        let closed = tokio::time::timeout(
            Duration::from_millis(500),
            excess.read_to_end(&mut response),
        )
        .await;
        assert!(
            closed.is_ok(),
            "excess accepted sockets must be closed, not queued"
        );

        drop(occupied);
        stop.send(true).unwrap();
        server.await.unwrap().unwrap();
    }

    #[tokio::test]
    async fn full_dump_guard_suspends_then_closes_the_connection_lifetime() {
        let (state, mut changed) = ConnectionLifetimeState::new();
        let guard = state.begin_full_dump();
        assert!(state.full_dump_active());
        assert!(!state.close_requested());
        drop(guard);
        tokio::time::timeout(Duration::from_secs(1), changed.changed())
            .await
            .unwrap()
            .unwrap();
        assert!(!state.full_dump_active());
        assert!(state.close_requested());
    }

    #[tokio::test]
    async fn slow_reader_holds_general_admission_until_response_drop() {
        let registry = Arc::new(
            Registry::in_memory(crate::RegistryConfig {
                origin: "pigeonpost.dev/registry-response-lifetime-test".into(),
                signing_key: SigningKey::from_bytes(&[107; 32]),
                allow_mock_identities: false,
            })
            .unwrap(),
        );
        let config = RegistryHttpConfig::direct()
            .with_limits(RegistryLimits {
                max_concurrent_requests: 1,
                ..RegistryLimits::default()
            })
            .unwrap()
            .with_request_timeout(Duration::from_secs(2))
            .unwrap();
        let state = Arc::new(RegistryServer::new(registry, config).unwrap());
        let (sender, receiver) =
            tokio::sync::mpsc::channel::<std::result::Result<Bytes, std::io::Error>>(1);
        sender
            .send(Ok(Bytes::from_static(b"first frame")))
            .await
            .unwrap();
        let receiver = Arc::new(Mutex::new(Some(receiver)));
        let app =
            Router::new()
                .route(
                    "/slow-body",
                    get({
                        let receiver = Arc::clone(&receiver);
                        move || {
                            let receiver = receiver
                                .lock()
                                .unwrap_or_else(|error| error.into_inner())
                                .take()
                                .expect("one request expected");
                            async move {
                                Response::new(Body::from_stream(ReceiverStream::new(receiver)))
                            }
                        }
                    }),
                )
                .layer(middleware::from_fn_with_state(
                    Arc::clone(&state),
                    admission_middleware,
                ));
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let endpoint = format!("http://{}/slow-body", listener.local_addr().unwrap());
        let server = tokio::spawn(async move {
            axum::serve(
                listener,
                app.into_make_service_with_connect_info::<SocketAddr>(),
            )
            .await
        });

        let response = reqwest::get(endpoint).await.unwrap();
        assert_eq!(response.status(), StatusCode::OK);
        assert!(matches!(state.try_enter(), Err(RegistryError::Overloaded)));
        drop(response);
        drop(sender);
        tokio::time::timeout(Duration::from_secs(1), async {
            loop {
                if state.requests.available_permits() == 1 {
                    break;
                }
                tokio::task::yield_now().await;
            }
        })
        .await
        .expect("response drop must release the general admission permit");
        server.abort();
    }

    #[tokio::test]
    async fn full_dump_allows_slow_progress_but_releases_a_stalled_reader() {
        let state = test_server(RegistryLimits::default());
        let permit = state.try_full_stream().unwrap();
        let (sender, mut receiver) = tokio::sync::mpsc::channel(1);
        sender.send(Ok(Bytes::from_static(b"first"))).await.unwrap();
        let progressing = tokio::spawn(async move {
            let _permit = permit;
            send_dump_chunk_with_idle(
                &sender,
                Bytes::from_static(b"second"),
                None,
                Duration::from_millis(100),
            )
            .await
        });
        tokio::time::sleep(Duration::from_millis(20)).await;
        assert!(matches!(
            state.try_full_stream(),
            Err(RegistryError::Overloaded)
        ));
        assert!(state.try_stream().is_ok());
        assert_eq!(receiver.recv().await.unwrap().unwrap(), b"first"[..]);
        assert!(progressing.await.unwrap());
        assert_eq!(receiver.recv().await.unwrap().unwrap(), b"second"[..]);
        assert!(state.try_full_stream().is_ok());

        let permit = state.try_full_stream().unwrap();
        let (sender, _stalled_receiver) = tokio::sync::mpsc::channel(1);
        sender
            .send(Ok(Bytes::from_static(b"buffered")))
            .await
            .unwrap();
        let stalled = tokio::spawn(async move {
            let _permit = permit;
            send_dump_chunk_with_idle(
                &sender,
                Bytes::from_static(b"blocked"),
                None,
                Duration::from_millis(20),
            )
            .await
        });
        assert!(!tokio::time::timeout(Duration::from_secs(1), stalled)
            .await
            .expect("idle full-dump reader must be disconnected")
            .unwrap());
        assert!(state.try_full_stream().is_ok());
    }

    #[test]
    fn request_deadline_configuration_is_bounded() {
        assert!(RegistryHttpConfig::direct()
            .with_request_timeout(Duration::ZERO)
            .is_err());
        assert!(RegistryHttpConfig::direct()
            .with_request_timeout(MAX_REQUEST_TIMEOUT + Duration::from_millis(1))
            .is_err());
        assert!(RegistryHttpConfig::direct()
            .with_request_timeout(Duration::from_millis(1))
            .is_ok());
    }

    #[tokio::test]
    async fn total_request_deadline_releases_the_admission_permit() {
        let registry = Arc::new(
            Registry::in_memory(crate::RegistryConfig {
                origin: "pigeonpost.dev/registry-timeout-test".into(),
                signing_key: SigningKey::from_bytes(&[43; 32]),
                allow_mock_identities: false,
            })
            .unwrap(),
        );
        let config = RegistryHttpConfig::direct()
            .with_limits(RegistryLimits {
                max_concurrent_requests: 1,
                ..RegistryLimits::default()
            })
            .unwrap()
            .with_request_timeout(Duration::from_millis(25))
            .unwrap();
        let state = Arc::new(RegistryServer::new(registry, config).unwrap());
        let started = Arc::new(tokio::sync::Notify::new());
        let handler_started = Arc::clone(&started);
        let app = Router::new()
            .route(
                "/slow",
                get(move || {
                    let started = Arc::clone(&handler_started);
                    async move {
                        started.notify_one();
                        tokio::time::sleep(Duration::from_secs(60)).await;
                        "late"
                    }
                }),
            )
            .layer(middleware::from_fn_with_state(
                Arc::clone(&state),
                admission_middleware,
            ));
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let endpoint = format!("http://{}/slow", listener.local_addr().unwrap());
        let server = tokio::spawn(async move {
            axum::serve(
                listener,
                app.into_make_service_with_connect_info::<SocketAddr>(),
            )
            .await
        });
        let request = tokio::spawn(async move { reqwest::get(endpoint).await });

        tokio::time::timeout(Duration::from_secs(1), started.notified())
            .await
            .expect("slow handler must start");
        assert!(matches!(state.try_enter(), Err(RegistryError::Overloaded)));

        let response = tokio::time::timeout(Duration::from_secs(1), request)
            .await
            .expect("request must finish at the configured deadline")
            .unwrap()
            .unwrap();
        assert_eq!(response.status(), StatusCode::SERVICE_UNAVAILABLE);
        assert!(state.try_enter().is_ok());
        server.abort();
    }

    #[test]
    fn log_status_reports_lag_without_exposing_pending_entries() {
        let status = log_status_response(
            WitnessPublicationStatus {
                committed_size: 9,
                published_size: 7,
                lag_entries: 2,
                witnessed_at: Some(123),
            },
            false,
        );
        assert!(!status.ready);
        assert_eq!(status.committed_size, 9);
        assert_eq!(status.published_size, 7);
        assert_eq!(status.lag_entries, 2);
        assert_eq!(status.witnessed_at, Some(123));
    }

    #[tokio::test(flavor = "current_thread")]
    async fn blocking_lane_is_bounded_and_does_not_stall_the_async_executor() {
        let state = test_server(RegistryLimits {
            max_blocking_operations: 1,
            ..RegistryLimits::default()
        });
        let (started_tx, started_rx) = tokio::sync::oneshot::channel();
        let (release_tx, release_rx) = std::sync::mpsc::channel();
        let first_state = Arc::clone(&state);
        let first = tokio::spawn(async move {
            first_state
                .blocking(move |_| {
                    let _ = started_tx.send(());
                    release_rx.recv().map_err(|_| RegistryError::Overloaded)?;
                    Ok(())
                })
                .await
        });
        started_rx.await.unwrap();

        tokio::time::timeout(Duration::from_millis(100), tokio::task::yield_now())
            .await
            .expect("the current-thread executor must remain responsive");
        assert!(matches!(
            state.blocking(move |_| Ok(())).await,
            Err(RegistryError::Overloaded)
        ));

        release_tx.send(()).unwrap();
        first.await.unwrap().unwrap();
    }

    #[tokio::test]
    async fn handle_binding_cannot_bypass_a_saturated_sqlite_lane() {
        let registry = Arc::new(
            Registry::in_memory(crate::RegistryConfig {
                origin: "pigeonpost.dev/registry-binding-lane-test".into(),
                signing_key: SigningKey::from_bytes(&[118; 32]),
                allow_mock_identities: true,
            })
            .unwrap()
            .with_provider(Box::new(MockProvider))
            .with_claim_trace(Arc::new(ContractClaimTraceSink(Some(ClaimTraceCapacity {
                policy: test_trace_policy(),
                records_per_minute: 1,
                utc_epochs: 1,
                max_records_per_segment: 10_000,
                network_logical_limit_bytes: MAX_TRACE_STORAGE_BYTES,
                identity_logical_limit_bytes: MAX_TRACE_STORAGE_BYTES,
            }))))
            .with_registration_limits(RegistrationLimits {
                global_bindings_per_minute: 1,
                account_bindings_per_minute: 1,
                max_account_keys: 1,
            })
            .unwrap(),
        );
        let state = Arc::new(
            RegistryServer::new(
                registry,
                RegistryHttpConfig::direct()
                    .with_limits(RegistryLimits {
                        max_blocking_operations: 1,
                        ..RegistryLimits::default()
                    })
                    .unwrap(),
            )
            .unwrap(),
        );
        let identity = SigningKey::from_bytes(&[119; 32]);
        let pubkey = *identity.verifying_key().as_bytes();
        let handle = Handle::parse("/github/alice").unwrap();
        let signature = identity.sign(&crate::entry::claim_payload(&handle.as_path(), &pubkey));
        let request = RegisterRequest {
            handle: handle.as_path(),
            pubkey: hex(&pubkey),
            signature: hex(&signature.to_bytes()),
            proof: ProofPayload::Mock {
                name: "alice".into(),
            },
        };

        let held = Arc::clone(&state.blocking).try_acquire_owned().unwrap();
        assert!(matches!(
            bind_handle(
                &state,
                socket("127.0.0.1:42001"),
                &HeaderMap::new(),
                request,
                false,
            )
            .await,
            Err(RegistryError::Overloaded)
        ));
        drop(held);
    }

    #[tokio::test(flavor = "current_thread")]
    async fn timed_out_final_commit_recovers_the_exact_atomic_challenge_result() {
        let registration_limits = RegistrationLimits {
            global_bindings_per_minute: 1,
            account_bindings_per_minute: 1,
            max_account_keys: 8,
        };
        let registry = Arc::new(
            Registry::in_memory(crate::RegistryConfig {
                origin: "pigeonpost.dev/registry-atomic-timeout-test".into(),
                signing_key: SigningKey::from_bytes(&[113; 32]),
                allow_mock_identities: false,
            })
            .unwrap()
            .with_provider(Box::new(DeterministicGithubProvider))
            .with_claim_trace(Arc::new(ContractClaimTraceSink(Some(ClaimTraceCapacity {
                policy: test_trace_policy(),
                records_per_minute: 1,
                utc_epochs: 1,
                max_records_per_segment: 10_000,
                network_logical_limit_bytes: MAX_TRACE_STORAGE_BYTES,
                identity_logical_limit_bytes: MAX_TRACE_STORAGE_BYTES,
            }))))
            .with_registration_limits(registration_limits)
            .unwrap(),
        );
        let state = Arc::new(
            RegistryServer::new(
                Arc::clone(&registry),
                RegistryHttpConfig::direct()
                    .with_limits(RegistryLimits {
                        max_blocking_operations: 1,
                        blocking_timeout_ms: 25,
                        ..RegistryLimits::default()
                    })
                    .unwrap(),
            )
            .unwrap(),
        );

        let signing_key = SigningKey::from_bytes(&[114; 32]);
        let pubkey = *signing_key.verifying_key().as_bytes();
        let handle = Handle::parse("/github/alice").unwrap();
        let signature = signing_key.sign(&crate::entry::claim_payload(&handle.as_path(), &pubkey));
        let verifier = "a".repeat(43);
        let challenge = registry
            .issue_identity_challenge(
                IdentityChallengeProvider::Github,
                &handle,
                &pubkey,
                &signature.to_bytes(),
                Some(&pkce_s256(&verifier).unwrap()),
            )
            .unwrap();
        let request = || RegisterRequest {
            handle: handle.as_path(),
            pubkey: hex(&pubkey),
            signature: hex(&signature.to_bytes()),
            proof: ProofPayload::Github {
                code: "one-shot-code".into(),
                code_verifier: verifier.clone(),
                state: challenge.value.clone(),
            },
        };
        let first_request = request();
        let retry_request = request();

        let (barrier, reached, release) = CommitTestBarrier::new();
        registry.install_commit_test_barrier(barrier);
        let first_state = Arc::clone(&state);
        let first = tokio::spawn(async move {
            bind_handle(
                &first_state,
                socket("127.0.0.1:41001"),
                &HeaderMap::new(),
                first_request,
                false,
            )
            .await
        });
        tokio::time::timeout(Duration::from_secs(1), reached)
            .await
            .expect("commit must reach the post-SQLite barrier")
            .unwrap();
        assert!(matches!(
            tokio::time::timeout(Duration::from_secs(1), first)
                .await
                .expect("the blocking adapter must return at its configured timeout")
                .unwrap(),
            Err(RegistryError::Overloaded)
        ));

        release.send(()).unwrap();
        tokio::time::timeout(Duration::from_secs(1), async {
            while state.blocking.available_permits() != 1 {
                tokio::task::yield_now().await;
            }
        })
        .await
        .expect("the detached commit must finish after the barrier is released");
        assert_eq!(registry.committed_size().unwrap(), 1);

        let recovered = bind_handle(
            &state,
            socket("127.0.0.1:41001"),
            &HeaderMap::new(),
            retry_request,
            false,
        )
        .await
        .unwrap();
        let recovered = response_json(recovered).await;
        assert_eq!(recovered["log_index"], 0);
        assert_eq!(recovered["appended"], false);
        assert_eq!(registry.committed_size().unwrap(), 1);
    }
}
