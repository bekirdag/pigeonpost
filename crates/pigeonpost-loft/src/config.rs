//! Loft configuration.
//!
//! Defaults are the ones `docs/node.md` promises: `pigeonpost install` with no flags produces a
//! working, bounded loft. Every number here is deliberately modest — a loft that surprises its
//! operator by filling a shared production box is a loft nobody runs twice.

use std::net::IpAddr;

/// Maximum serialized v3 wrap accepted by the default loft.
///
/// The envelope writer uses compact ciphertext strings, and a regression test exercises a full
/// 64 KiB, maximally JSON-escaped plaintext through the real HTTP surface. Two MiB leaves bounded
/// framing headroom while matching the directory's advertised node ceiling.
pub const DEFAULT_MAX_EVENT_BYTES: usize = crate::wire::MAX_EVENT_BYTES;

/// Whole publish request ceiling. The request adds a fixed wrapper and an optional fixed-size
/// capability presentation around an event, so 64 KiB is deliberately generous bounded headroom.
pub const DEFAULT_MAX_REQUEST_BYTES: usize = DEFAULT_MAX_EVENT_BYTES + 64 * 1024;

/// Absolute public-API ceilings. Configuration is validated before any semaphore or
/// attacker-keyed map is allocated, so an untrusted or mistyped configuration cannot turn startup
/// into an allocation spike or a Tokio semaphore panic.
pub const MAX_CAPACITY_BYTES: u64 = 1024 * 1024 * 1024 * 1024 * 1024;
pub const MAX_RETENTION_DAYS: u64 = crate::wire::MAX_RETENTION_DAYS;
pub const MAX_REQUEST_BYTES: usize = DEFAULT_MAX_REQUEST_BYTES;
pub const MAX_FETCH_LIMIT: usize = 500;
pub const MAX_FETCH_RESPONSE_BYTES: usize = 8 * 1024 * 1024;
pub const MAX_RATE_LIMIT_PER_MINUTE: u32 = 1_000_000;
pub const MAX_GLOBAL_REQUESTS_PER_MINUTE: u32 = 10_000_000;
pub const MAX_RATE_BYTES_PER_MINUTE: u64 = 1024 * 1024 * 1024 * 1024;
pub const MAX_CONCURRENT_CONNECTIONS: usize = 4_096;
pub const MAX_CONCURRENT_REQUESTS: usize = 4_096;
pub const MAX_BLOCKING_OPERATIONS: usize = 256;
pub const MAX_LIMITER_KEYS: usize = 65_536;
pub const MAX_REQUEST_TIMEOUT_SECS: u64 = 300;
pub const MAX_HEADER_TIMEOUT_SECS: u64 = 300;
pub const MAX_RESPONSE_TIMEOUT_SECS: u64 = 300;
pub const MAX_TRACE_TIMEOUT_MS: u64 = 30_000;
pub const MAX_SWEEP_BATCH: usize = 10_000;
pub const MAX_SWEEP_INTERVAL_SECS: u64 = 86_400;
pub const MAX_TRUSTED_PROXIES: usize = 64;

#[derive(Debug, Clone)]
pub struct LoftConfig {
    /// This loft's public key. Clients bind fetch proofs and token presentations to it.
    pub pubkey: [u8; 32],

    /// Exact canonical public origin. Fetch credentials bind this independently of the loft key,
    /// so one hostile endpoint cannot claim another loft's key and relay harvested credentials.
    pub origin: String,

    /// Advertised capacity — a budget, not free disk (`docs/capacity.md`). 20 GB serves well
    /// over ten thousand agents at 30-day retention.
    pub capacity_bytes: u64,

    pub retention_days: u64,

    /// Per-event ceiling. Attachments are out of scope; inline payload size is the one variable
    /// that multiplies every capacity number.
    pub max_event_bytes: usize,

    /// Whole-request ceiling, checked before the body is buffered.
    pub max_request_bytes: usize,

    pub default_fetch_limit: usize,
    pub max_fetch_limit: usize,
    /// Conservative serialized page budget. The handler derives a count ceiling from the maximum
    /// stored event size before asking SQLite for blobs.
    pub max_fetch_response_bytes: usize,

    /// Publishes per recipient per minute. Blunt, cheap, and independent of sender identity —
    /// which is the only kind of limit a loft can apply.
    pub rate_limit_per_minute: u32,

    /// Admission budgets shared by every request. These prevent rotating attacker-controlled
    /// recipient keys from bypassing the recipient bucket.
    pub global_requests_per_minute: u32,
    pub global_bytes_per_minute: u64,
    pub source_requests_per_minute: u32,
    pub source_bytes_per_minute: u64,
    pub recipient_bytes_per_minute: u64,

    /// Hard bounds on accepted connections, in-flight work, and attacker-controlled limiter
    /// cardinality. Connection admission is non-queueing above the kernel listen backlog.
    pub max_concurrent_connections: usize,
    pub max_concurrent_requests: usize,
    pub max_blocking_operations: usize,
    pub max_limiter_keys: usize,

    /// Timer-backed incomplete-header deadline, end-to-end handler deadline, response-body
    /// lifetime, and the shorter fail-closed trace handoff deadline.
    pub header_timeout_secs: u64,
    pub request_timeout_secs: u64,
    pub response_timeout_secs: u64,
    pub trace_timeout_ms: u64,

    /// Reverse proxies whose RFC 7239 `Forwarded` chain may supply the trace source. Every other
    /// peer's forwarding headers are ignored. Forwarded hops must include the source port.
    pub trusted_proxies: Vec<IpAddr>,

    /// Retention sweep batch. Small so the writer lock is never held long.
    pub sweep_batch: usize,
    pub sweep_interval_secs: u64,
}

impl LoftConfig {
    pub fn new(pubkey: [u8; 32], origin: impl Into<String>) -> Self {
        LoftConfig {
            pubkey,
            origin: origin.into(),
            capacity_bytes: 20 * 1024 * 1024 * 1024,
            retention_days: 30,
            max_event_bytes: DEFAULT_MAX_EVENT_BYTES,
            max_request_bytes: DEFAULT_MAX_REQUEST_BYTES,
            default_fetch_limit: 100,
            max_fetch_limit: 500,
            max_fetch_response_bytes: 8 * 1024 * 1024,
            rate_limit_per_minute: 120,
            global_requests_per_minute: 2_400,
            global_bytes_per_minute: 128 * 1024 * 1024,
            source_requests_per_minute: 600,
            source_bytes_per_minute: 32 * 1024 * 1024,
            recipient_bytes_per_minute: 16 * 1024 * 1024,
            max_concurrent_connections: 256,
            max_concurrent_requests: 128,
            max_blocking_operations: 8,
            max_limiter_keys: 4_096,
            header_timeout_secs: 5,
            request_timeout_secs: 15,
            response_timeout_secs: 15,
            trace_timeout_ms: 250,
            trusted_proxies: Vec::new(),
            sweep_batch: 500,
            sweep_interval_secs: 300,
        }
    }

    pub fn with_capacity_bytes(mut self, bytes: u64) -> Self {
        self.capacity_bytes = bytes;
        self
    }

    pub fn with_retention_days(mut self, days: u64) -> Self {
        self.retention_days = days;
        self
    }

    pub fn with_rate_limit(mut self, per_minute: u32) -> Self {
        self.rate_limit_per_minute = per_minute;
        self
    }

    /// Override the shared admission limits. Primarily useful for small deployments and tests.
    pub fn with_global_rate_limit(mut self, requests: u32, bytes: u64) -> Self {
        self.global_requests_per_minute = requests;
        self.global_bytes_per_minute = bytes;
        self
    }

    pub fn with_source_rate_limit(mut self, requests: u32, bytes: u64) -> Self {
        self.source_requests_per_minute = requests;
        self.source_bytes_per_minute = bytes;
        self
    }

    pub fn with_concurrency_limit(mut self, requests: usize) -> Self {
        self.max_concurrent_requests = requests;
        self
    }

    pub fn with_trusted_proxies(mut self, proxies: impl IntoIterator<Item = IpAddr>) -> Self {
        self.trusted_proxies = proxies.into_iter().collect();
        self
    }
}
