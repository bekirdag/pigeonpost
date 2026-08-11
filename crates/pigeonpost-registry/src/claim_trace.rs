//! Purpose-separated sealed evidence for successful handle claims.
//!
//! Network origin and provider identity are deliberately written to different directories under
//! different daily epoch keys. The only join value is a fresh random commitment sealed into both
//! records. This online module can seal and publicly verify segments; it cannot decrypt them.

use std::any::Any;
use std::io::{Read, Write};
use std::net::{IpAddr, SocketAddr};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, AtomicU8, Ordering};
use std::sync::{mpsc, Arc, Mutex};
use std::thread::JoinHandle;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

#[cfg(not(unix))]
use std::fs::File;
#[cfg(any(test, not(unix)))]
use std::fs::{self, OpenOptions};

use ed25519_dalek::SigningKey;
pub use pigeonpost_compliance_format::TraceCapturePolicy as ClaimCapturePolicy;
use pigeonpost_compliance_format::{
    trace_epoch_contains, trace_epoch_end_ms as canonical_trace_epoch_end_ms, validate_trace_epoch,
    ComplianceKeyId, CompliancePurpose, Jurisdiction, TraceRetentionPolicy, COMPLIANCE_KEY_ID_LEN,
    TRACE_EPOCH_DURATION_MS,
};
use pigeonpost_compliance_seal::{
    epoch_manifest_path, publish_epoch_manifest, recover_segment,
    required_identity_trace_storage_bytes, required_network_trace_storage_bytes, verify_segment,
    EpochManifest, EpochSealingKey, EpochSegmentEntry, IdentityProvider, IdentityTraceRecord,
    NetworkOperation, Recovery, SegmentWriter, TraceIp, TraceRecord, TraceStorageBudget,
    TraceWriterLease, EPOCH_MANIFEST_ENTRY_LEN, EPOCH_MANIFEST_FIXED_LEN,
    IDENTITY_TRACE_FRAME_BYTES, MAX_EPOCH_MANIFEST_SEGMENTS, MAX_SEGMENT_RECORDS,
    NETWORK_TRACE_FRAME_BYTES, SEGMENT_FOOTER_LEN, SEGMENT_HEADER_LEN, TRACE_LIVE_KEY_BYTES,
};
#[cfg(test)]
use pigeonpost_compliance_seal::{
    read_epoch_manifest_for_signer, DEFAULT_TRACE_STORAGE_BYTES, MIN_TRACE_STORAGE_BYTES,
    TRACE_TERMINAL_RESERVE_BYTES,
};
use rand_core::{OsRng, RngCore};
use sha2::{Digest, Sha256};
use zeroize::{Zeroize, Zeroizing};

#[cfg(unix)]
use pigeonpost_unix_custody::{
    CustodyError, FilePolicy, GuardedDir, GuardedFile, LeafName, NormalizedPath, OpenAccess,
};

const LIVE_KEY_MAGIC: &[u8; 8] = b"PPREGKEY";
const LIVE_KEY_VERSION: u8 = 1;
const LIVE_KEY_LEN: usize = TRACE_LIVE_KEY_BYTES as usize;
const LIVE_KEY_NAME: &str = "live-key-v1";
const MAX_CLOCK_SKEW_MS: u64 = 5 * 60 * 1_000;
const CLAIM_TRACE_QUEUE_CAPACITY: usize = 64;
const CLAIM_TRACE_BATCH_MAX: usize = 32;
const CLAIM_TRACE_BATCH_WINDOW: Duration = Duration::from_millis(2);
const CLAIM_TRACE_ROLLOVER_CLOCK_POLL: Duration = Duration::from_secs(1);
const WORKER_RUNNING: u8 = 0;
const WORKER_SHUTTING_DOWN: u8 = 1;
const WORKER_STOPPED: u8 = 2;
const WORKER_POISONED: u8 = 3;
static TEMP_COUNTER: AtomicU64 = AtomicU64::new(0);

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ResolvedClaimTraceKey {
    pub key_id: ComplianceKeyId,
    pub public_key: [u8; 32],
    pub not_before_ms: u64,
    pub not_after_ms: u64,
}

/// Cache-only lookup. Implementations must refresh witnessed registry state in a supervised task;
/// a registration request must never cause network I/O to discover its custody keys.
pub trait ClaimTraceKeyResolver: Send + Sync {
    fn readiness(&self, now_ms: u64) -> Result<(), ClaimTraceError>;

    fn resolve_trace_key(
        &self,
        purpose: CompliancePurpose,
        jurisdiction: Jurisdiction,
        at_ms: u64,
    ) -> Result<Option<ResolvedClaimTraceKey>, ClaimTraceError>;
}

#[derive(Clone)]
pub struct ClaimTraceInput {
    pub timestamp_ms: u64,
    pub source: SocketAddr,
    pub provider: IdentityProvider,
    pub provider_subject: String,
}

impl core::fmt::Debug for ClaimTraceInput {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        f.debug_struct("ClaimTraceInput")
            .field("timestamp_ms", &self.timestamp_ms)
            .field("source", &"<withheld>")
            .field("provider", &self.provider)
            .field("provider_subject", &"<withheld>")
            .finish()
    }
}

impl Drop for ClaimTraceInput {
    fn drop(&mut self) {
        self.timestamp_ms = 0;
        self.source = SocketAddr::from(([0, 0, 0, 0], 0));
        self.provider_subject.zeroize();
    }
}

#[derive(Debug, thiserror::Error, Clone, Copy, PartialEq, Eq)]
pub enum ClaimTraceError {
    #[error("claim trace unavailable")]
    Unavailable,
    #[error("claim trace capacity exhausted")]
    Capacity,
}

/// Release-time capacity promised by a claim-trace sink.
///
/// One UTC epoch is one canonical trace-key day. Public identity-provider serving independently
/// recomputes both purpose-separated storage requirements from this complete plan.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ClaimTraceCapacity {
    pub policy: TraceRetentionPolicy,
    pub records_per_minute: u32,
    pub utc_epochs: u64,
    pub max_records_per_segment: u32,
    pub network_logical_limit_bytes: u64,
    pub identity_logical_limit_bytes: u64,
}

/// Joinable durable acknowledgement returned by the supervised claim-trace worker.
#[derive(Debug)]
pub struct ClaimTraceReceipt {
    acknowledged: tokio::sync::oneshot::Receiver<Result<(), ClaimTraceError>>,
    status: Option<Arc<WorkerStatus>>,
}

impl ClaimTraceReceipt {
    pub async fn wait(self) -> Result<(), ClaimTraceError> {
        match self.acknowledged.await {
            Ok(result) => result,
            Err(_) => {
                if let Some(status) = self.status {
                    status.poison();
                }
                Err(ClaimTraceError::Unavailable)
            }
        }
    }

    fn ready(result: Result<(), ClaimTraceError>) -> Self {
        let (acknowledged, receipt) = tokio::sync::oneshot::channel();
        let _ = acknowledged.send(result);
        Self {
            acknowledged: receipt,
            status: None,
        }
    }

    fn blocking_wait(self) -> Result<(), ClaimTraceError> {
        match self.acknowledged.blocking_recv() {
            Ok(result) => result,
            Err(_) => {
                if let Some(status) = self.status {
                    status.poison();
                }
                Err(ClaimTraceError::Unavailable)
            }
        }
    }
}

pub trait ClaimTraceSink: Send + Sync + Any {
    /// Return the validated storage plan this sink can sustain. The default deliberately reports
    /// no contract so an ad-hoc or legacy sink cannot become a public identity-provider sink.
    fn capacity_contract(&self) -> Option<ClaimTraceCapacity> {
        None
    }

    fn readiness(&self, now_ms: u64) -> Result<(), ClaimTraceError>;
    fn capture(&self, input: ClaimTraceInput) -> Result<(), ClaimTraceError>;
    /// Submit without creating a detached blocking task. Implementations with a supervised worker
    /// override this; small in-memory/test sinks retain a ready acknowledgement by default.
    fn submit(&self, input: ClaimTraceInput) -> Result<ClaimTraceReceipt, ClaimTraceError> {
        Ok(ClaimTraceReceipt::ready(self.capture(input)))
    }
    fn shutdown(&self, timestamp_ms: u64) -> Result<(), ClaimTraceError>;
}

/// Fail-closed default used until an operator supplies independent witnessed keys and stores.
#[derive(Debug, Default)]
pub struct UnconfiguredClaimTraceSink;

impl ClaimTraceSink for UnconfiguredClaimTraceSink {
    fn readiness(&self, _now_ms: u64) -> Result<(), ClaimTraceError> {
        Err(ClaimTraceError::Unavailable)
    }

    fn capture(&self, _input: ClaimTraceInput) -> Result<(), ClaimTraceError> {
        Err(ClaimTraceError::Unavailable)
    }

    fn shutdown(&self, _timestamp_ms: u64) -> Result<(), ClaimTraceError> {
        Ok(())
    }
}

#[derive(Debug, Clone)]
pub struct SealedClaimTraceConfig {
    pub network_directory: PathBuf,
    pub identity_directory: PathBuf,
    pub network_max_storage_bytes: u64,
    pub identity_max_storage_bytes: u64,
    pub jurisdiction: Jurisdiction,
    pub node_id: [u8; 32],
    pub capture_policy: ClaimCapturePolicy,
    /// Selected standing-policy retention. Preservation-only EU capture and the test
    /// jurisdiction carry no standing retention value.
    pub retention_days: Option<u64>,
    pub max_records_per_segment: u32,
    /// Maximum claim records admitted in one fixed one-minute registry window.
    pub planned_records_per_minute: u32,
    /// Number of canonical UTC trace-key epochs provisioned independently in each purpose store.
    pub capacity_utc_epochs: u64,
}

struct LiveKeyState {
    key_id: ComplianceKeyId,
    segment_index: u32,
    secret: [u8; 32],
}

impl Drop for LiveKeyState {
    fn drop(&mut self) {
        self.secret.zeroize();
    }
}

impl core::fmt::Debug for LiveKeyState {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        f.debug_struct("LiveKeyState")
            .field("key_id", &self.key_id)
            .field("segment_index", &self.segment_index)
            .field("secret", &"<withheld>")
            .finish()
    }
}

struct ActiveSegment {
    writer: SegmentWriter,
    last_record_ms: u64,
}

impl core::fmt::Debug for ActiveSegment {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        f.debug_struct("ActiveSegment")
            .field("writer", &self.writer)
            .field("last_record_ms", &self.last_record_ms)
            .finish()
    }
}

#[derive(Debug, Default)]
struct PurposeState {
    live: Option<LiveKeyState>,
    active: Option<ActiveSegment>,
}

#[derive(Debug, Default)]
struct DualState {
    network: PurposeState,
    identity: PurposeState,
    poisoned: bool,
}

#[derive(Debug, Clone, Copy)]
struct PersistentTracePlatform {
    supported_persistent_target: bool,
}

impl PersistentTracePlatform {
    const fn current() -> Self {
        Self {
            supported_persistent_target: cfg!(any(target_os = "linux", target_os = "macos")),
        }
    }

    #[cfg(test)]
    const fn unsupported_for_test() -> Self {
        Self {
            supported_persistent_target: false,
        }
    }
}

#[derive(Debug, Clone, Copy)]
enum WorkerClock {
    System,
    #[cfg(test)]
    Test(&'static AtomicU64),
    #[cfg(test)]
    Panic,
}

impl WorkerClock {
    fn now_ms(self) -> u64 {
        match self {
            Self::System => now_ms(),
            #[cfg(test)]
            Self::Test(current_ms) => current_ms.load(Ordering::Acquire),
            #[cfg(test)]
            Self::Panic => panic!("unsupported platform must not sample its clock"),
        }
    }
}

#[derive(Debug, Clone, Copy)]
struct WorkerOptions {
    queue_capacity: usize,
    batch_max: usize,
    batch_window: Duration,
    rollover_clock_poll: Duration,
    clock: WorkerClock,
    enforce_wall_clock: bool,
}

impl Default for WorkerOptions {
    fn default() -> Self {
        Self {
            queue_capacity: CLAIM_TRACE_QUEUE_CAPACITY,
            batch_max: CLAIM_TRACE_BATCH_MAX,
            batch_window: CLAIM_TRACE_BATCH_WINDOW,
            rollover_clock_poll: CLAIM_TRACE_ROLLOVER_CLOCK_POLL,
            clock: WorkerClock::System,
            enforce_wall_clock: true,
        }
    }
}

struct CaptureCommand {
    input: ClaimTraceInput,
    acknowledged: tokio::sync::oneshot::Sender<Result<(), ClaimTraceError>>,
}

enum ClaimTraceCommand {
    Capture(CaptureCommand),
    Shutdown {
        timestamp_ms: u64,
        acknowledged: mpsc::SyncSender<Result<(), ClaimTraceError>>,
    },
    #[cfg(test)]
    Crash,
}

#[derive(Debug)]
struct WorkerStatus {
    lifecycle: AtomicU8,
    admission: Mutex<()>,
    last_admitted_ms: AtomicU64,
    #[cfg(test)]
    durable_sync_batches: AtomicU64,
    #[cfg(test)]
    enqueued_captures: AtomicU64,
    #[cfg(test)]
    fail_before_sync: std::sync::atomic::AtomicBool,
    #[cfg(test)]
    fail_identity_sync: std::sync::atomic::AtomicBool,
    #[cfg(test)]
    panic_before_sync: std::sync::atomic::AtomicBool,
}

impl Default for WorkerStatus {
    fn default() -> Self {
        Self {
            lifecycle: AtomicU8::new(WORKER_RUNNING),
            admission: Mutex::new(()),
            last_admitted_ms: AtomicU64::new(0),
            #[cfg(test)]
            durable_sync_batches: AtomicU64::new(0),
            #[cfg(test)]
            enqueued_captures: AtomicU64::new(0),
            #[cfg(test)]
            fail_before_sync: std::sync::atomic::AtomicBool::new(false),
            #[cfg(test)]
            fail_identity_sync: std::sync::atomic::AtomicBool::new(false),
            #[cfg(test)]
            panic_before_sync: std::sync::atomic::AtomicBool::new(false),
        }
    }
}

impl WorkerStatus {
    fn poison(&self) {
        self.lifecycle.store(WORKER_POISONED, Ordering::Release);
    }

    fn is_running(&self) -> bool {
        self.lifecycle.load(Ordering::Acquire) == WORKER_RUNNING
    }
}

struct SealedClaimTraceInner {
    config: SealedClaimTraceConfig,
    writer_leases: [TraceWriterLease; 2],
    #[cfg(unix)]
    network_directory: GuardedDir,
    #[cfg(unix)]
    identity_directory: GuardedDir,
    network_storage_budget: TraceStorageBudget,
    identity_storage_budget: TraceStorageBudget,
    resolver: Arc<dyn ClaimTraceKeyResolver>,
    network_signer: SigningKey,
    identity_signer: SigningKey,
    state: Mutex<DualState>,
    enforce_wall_clock: bool,
}

#[derive(Clone, Copy)]
struct TraceDirectory<'a> {
    path: &'a Path,
    #[cfg(unix)]
    guard: &'a GuardedDir,
}

/// Online-only pair writer. No object in this type can decrypt a closed segment.
pub struct SealedClaimTraceSink {
    inner: Arc<SealedClaimTraceInner>,
    sender: mpsc::SyncSender<ClaimTraceCommand>,
    status: Arc<WorkerStatus>,
    worker: Mutex<Option<JoinHandle<()>>>,
}

impl core::fmt::Debug for SealedClaimTraceSink {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        let state = self
            .inner
            .state
            .lock()
            .unwrap_or_else(|error| error.into_inner());
        f.debug_struct("SealedClaimTraceSink")
            .field("network_directory", &"<withheld>")
            .field("identity_directory", &"<withheld>")
            .field("jurisdiction", &self.inner.config.jurisdiction)
            .field("node_id", &self.inner.config.node_id)
            .field("capture_policy", &self.inner.config.capture_policy)
            .field("network_active", &state.network.active.is_some())
            .field("identity_active", &state.identity.active.is_some())
            .field("poisoned", &state.poisoned)
            .field(
                "worker_state",
                &self.status.lifecycle.load(Ordering::Acquire),
            )
            .finish()
    }
}

impl SealedClaimTraceSink {
    pub fn new(
        config: SealedClaimTraceConfig,
        resolver: Arc<dyn ClaimTraceKeyResolver>,
        network_signing_seed: [u8; 32],
        identity_signing_seed: [u8; 32],
    ) -> Result<Self, ClaimTraceError> {
        Self::new_with_options(
            config,
            resolver,
            network_signing_seed,
            identity_signing_seed,
            WorkerOptions::default(),
        )
    }

    fn new_with_options(
        config: SealedClaimTraceConfig,
        resolver: Arc<dyn ClaimTraceKeyResolver>,
        network_signing_seed: [u8; 32],
        identity_signing_seed: [u8; 32],
        options: WorkerOptions,
    ) -> Result<Self, ClaimTraceError> {
        Self::new_with_options_for_platform(
            PersistentTracePlatform::current(),
            config,
            resolver,
            network_signing_seed,
            identity_signing_seed,
            options,
        )
    }

    fn new_with_options_for_platform(
        platform: PersistentTracePlatform,
        config: SealedClaimTraceConfig,
        resolver: Arc<dyn ClaimTraceKeyResolver>,
        network_signing_seed: [u8; 32],
        identity_signing_seed: [u8; 32],
        options: WorkerOptions,
    ) -> Result<Self, ClaimTraceError> {
        require_supported_persistent_trace_platform(platform)?;
        #[cfg(unix)]
        let mut config = config;
        validate_config(&config)?;
        if options.queue_capacity == 0
            || options.batch_max == 0
            || options.rollover_clock_poll.is_zero()
        {
            return Err(ClaimTraceError::Unavailable);
        }
        if network_signing_seed == [0u8; 32]
            || identity_signing_seed == [0u8; 32]
            || network_signing_seed == identity_signing_seed
        {
            return Err(ClaimTraceError::Unavailable);
        }
        let network_signing_seed = Zeroizing::new(network_signing_seed);
        let identity_signing_seed = Zeroizing::new(identity_signing_seed);
        #[cfg(unix)]
        let [network_directory, identity_directory] = {
            let directories =
                secure_separate_directories(&config.network_directory, &config.identity_directory)?;
            config.network_directory = directories[0].absolute_path().to_path_buf();
            config.identity_directory = directories[1].absolute_path().to_path_buf();
            directories
        };
        #[cfg(not(unix))]
        secure_separate_directories(&config.network_directory, &config.identity_directory)?;
        let network_ref = TraceDirectory {
            path: &config.network_directory,
            #[cfg(unix)]
            guard: &network_directory,
        };
        let identity_ref = TraceDirectory {
            path: &config.identity_directory,
            #[cfg(unix)]
            guard: &identity_directory,
        };
        let writer_leases = acquire_ordered_writer_leases(network_ref, identity_ref)?;
        let network_storage_budget =
            TraceStorageBudget::new(&config.network_directory, config.network_max_storage_bytes)
                .map_err(|_| ClaimTraceError::Unavailable)?;
        let identity_storage_budget = TraceStorageBudget::new(
            &config.identity_directory,
            config.identity_max_storage_bytes,
        )
        .map_err(|_| ClaimTraceError::Unavailable)?;
        let network_signer = SigningKey::from_bytes(&network_signing_seed);
        let identity_signer = SigningKey::from_bytes(&identity_signing_seed);
        let inner = Arc::new(SealedClaimTraceInner {
            config,
            writer_leases,
            #[cfg(unix)]
            network_directory,
            #[cfg(unix)]
            identity_directory,
            network_storage_budget,
            identity_storage_budget,
            resolver,
            network_signer,
            identity_signer,
            state: Mutex::new(DualState::default()),
            enforce_wall_clock: options.enforce_wall_clock,
        });
        inner.recover(options.clock.now_ms())?;

        let (sender, receiver) = mpsc::sync_channel(options.queue_capacity);
        let status = Arc::new(WorkerStatus::default());
        let worker_inner = Arc::clone(&inner);
        let worker_status = Arc::clone(&status);
        let worker = std::thread::Builder::new()
            .name("pigeonpost-claim-trace-commit".into())
            .spawn(move || {
                let outcome = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                    run_worker(worker_inner, receiver, &worker_status, options)
                }));
                match outcome {
                    Ok(Ok(())) => worker_status
                        .lifecycle
                        .store(WORKER_STOPPED, Ordering::Release),
                    Ok(Err(())) | Err(_) => worker_status.poison(),
                }
            })
            .map_err(|_| ClaimTraceError::Unavailable)?;
        Ok(Self {
            inner,
            sender,
            status,
            worker: Mutex::new(Some(worker)),
        })
    }

    fn submit_inner(&self, input: ClaimTraceInput) -> Result<ClaimTraceReceipt, ClaimTraceError> {
        self.submit_inner_at(input, None)
    }

    fn submit_inner_at(
        &self,
        mut input: ClaimTraceInput,
        admission_now_ms: Option<u64>,
    ) -> Result<ClaimTraceReceipt, ClaimTraceError> {
        let (acknowledged, receipt) = tokio::sync::oneshot::channel();
        {
            let _admission = self
                .status
                .admission
                .lock()
                .unwrap_or_else(|error| error.into_inner());
            if !self.status.is_running() {
                return Err(ClaimTraceError::Unavailable);
            }
            if self.inner.enforce_wall_clock {
                let admitted_ms = admission_now_ms.unwrap_or_else(now_ms);
                let previous = self.status.last_admitted_ms.load(Ordering::Acquire);
                if admitted_ms == 0 || admitted_ms < previous {
                    // Never invent a monotonic timestamp when the wall clock moved backwards.
                    // Refuse this admission without poisoning an otherwise healthy worker.
                    return Err(ClaimTraceError::Unavailable);
                }
                input.timestamp_ms = admitted_ms;
                self.status
                    .last_admitted_ms
                    .store(admitted_ms, Ordering::Release);
            }
            if !self
                .inner
                .config
                .capture_policy
                .captures(input.timestamp_ms)
            {
                return Ok(ClaimTraceReceipt::ready(Ok(())));
            }
            self.sender
                .try_send(ClaimTraceCommand::Capture(CaptureCommand {
                    input,
                    acknowledged,
                }))
                .map_err(|_| ClaimTraceError::Unavailable)?;
            #[cfg(test)]
            self.status.enqueued_captures.fetch_add(1, Ordering::AcqRel);
        }
        Ok(ClaimTraceReceipt {
            acknowledged: receipt,
            status: Some(Arc::clone(&self.status)),
        })
    }

    #[cfg(test)]
    fn submit_at_for_test(
        &self,
        input: ClaimTraceInput,
        admission_now_ms: u64,
    ) -> Result<ClaimTraceReceipt, ClaimTraceError> {
        self.submit_inner_at(input, Some(admission_now_ms))
    }

    fn shutdown_worker(&self, timestamp_ms: u64) -> Result<(), ClaimTraceError> {
        let (acknowledged, result) = mpsc::sync_channel(1);
        {
            let _admission = self
                .status
                .admission
                .lock()
                .unwrap_or_else(|error| error.into_inner());
            match self.status.lifecycle.load(Ordering::Acquire) {
                WORKER_RUNNING => self
                    .status
                    .lifecycle
                    .store(WORKER_SHUTTING_DOWN, Ordering::Release),
                WORKER_STOPPED => return Ok(()),
                _ => return Err(ClaimTraceError::Unavailable),
            }
            if self
                .sender
                .send(ClaimTraceCommand::Shutdown {
                    timestamp_ms,
                    acknowledged,
                })
                .is_err()
            {
                self.status.poison();
                return Err(ClaimTraceError::Unavailable);
            }
        }
        let outcome = result.recv().unwrap_or(Err(ClaimTraceError::Unavailable));
        let joined = self
            .worker
            .lock()
            .unwrap_or_else(|error| error.into_inner())
            .take()
            .is_none_or(|worker| worker.join().is_ok());
        if outcome.is_err() || !joined {
            self.status.poison();
            return Err(ClaimTraceError::Unavailable);
        }
        outcome
    }

    #[cfg(test)]
    fn crash_for_test(&self) {
        {
            let _admission = self
                .status
                .admission
                .lock()
                .unwrap_or_else(|error| error.into_inner());
            assert!(self.status.is_running());
            self.status
                .lifecycle
                .store(WORKER_SHUTTING_DOWN, Ordering::Release);
            self.sender.send(ClaimTraceCommand::Crash).unwrap();
        }
        let worker = self
            .worker
            .lock()
            .unwrap_or_else(|error| error.into_inner())
            .take()
            .expect("claim trace worker exists");
        assert!(worker.join().is_ok());
        assert_eq!(
            self.status.lifecycle.load(Ordering::Acquire),
            WORKER_POISONED
        );
    }
}

impl SealedClaimTraceInner {
    fn directory(&self, purpose: CompliancePurpose) -> Result<TraceDirectory<'_>, ClaimTraceError> {
        match purpose {
            CompliancePurpose::NetworkTrace => Ok(TraceDirectory {
                path: &self.config.network_directory,
                #[cfg(unix)]
                guard: &self.network_directory,
            }),
            CompliancePurpose::IdentityTrace => Ok(TraceDirectory {
                path: &self.config.identity_directory,
                #[cfg(unix)]
                guard: &self.identity_directory,
            }),
            CompliancePurpose::Attribution => Err(ClaimTraceError::Unavailable),
        }
    }

    fn writer_context(
        &self,
        purpose: CompliancePurpose,
    ) -> Result<PurposeWriterContext<'_>, ClaimTraceError> {
        let (storage_budget, signer) = match purpose {
            CompliancePurpose::NetworkTrace => (&self.network_storage_budget, &self.network_signer),
            CompliancePurpose::IdentityTrace => {
                (&self.identity_storage_budget, &self.identity_signer)
            }
            CompliancePurpose::Attribution => return Err(ClaimTraceError::Unavailable),
        };
        Ok(PurposeWriterContext {
            config: &self.config,
            directory: self.directory(purpose)?,
            purpose,
            storage_budget,
            signer,
            resolver: self.resolver.as_ref(),
        })
    }

    fn recover(&self, current_ms: u64) -> Result<(), ClaimTraceError> {
        self.assert_writer_leases()?;
        let network_directory = self.directory(CompliancePurpose::NetworkTrace)?;
        let identity_directory = self.directory(CompliancePurpose::IdentityTrace)?;
        let network = recover_purpose(
            &self.config,
            CompliancePurpose::NetworkTrace,
            network_directory,
            &self.network_storage_budget,
            &self.network_signer,
            &self.resolver,
            current_ms,
        )?;
        let identity = recover_purpose(
            &self.config,
            CompliancePurpose::IdentityTrace,
            identity_directory,
            &self.identity_storage_budget,
            &self.identity_signer,
            &self.resolver,
            current_ms,
        )?;
        self.network_storage_budget
            .reconcile(&self.config.network_directory)
            .map_err(|_| ClaimTraceError::Unavailable)?;
        self.identity_storage_budget
            .reconcile(&self.config.identity_directory)
            .map_err(|_| ClaimTraceError::Unavailable)?;
        let mut state = self.state.lock().unwrap_or_else(|error| error.into_inner());
        state.network = network;
        state.identity = identity;
        Ok(())
    }

    fn capture_buffered(&self, input: &mut ClaimTraceInput) -> Result<bool, ClaimTraceError> {
        self.assert_writer_leases()?;
        if !self.config.capture_policy.captures(input.timestamp_ms) {
            return Ok(false);
        }
        // Production timestamps were sampled under the queue admission mutex. Rechecking the
        // wall clock here would reintroduce a race; only validate the record's intrinsic fields.
        validate_input(input, false)?;
        self.resolver.readiness(now_ms())?;
        let network_key = self.resolve_key(CompliancePurpose::NetworkTrace, input.timestamp_ms)?;
        let identity_key =
            self.resolve_key(CompliancePurpose::IdentityTrace, input.timestamp_ms)?;

        let mut nonce = [0u8; 32];
        OsRng.fill_bytes(&mut nonce);
        let correlation_id: [u8; 32] = Sha256::digest(nonce).into();
        nonce.zeroize();
        let network = TraceRecord {
            jurisdiction: self.config.jurisdiction,
            operation: NetworkOperation::Claim,
            timestamp_ms: input.timestamp_ms,
            node_id: self.config.node_id,
            source_ip: TraceIp::from(input.source.ip()),
            source_port: input.source.port(),
            event_id: None,
            recipient: None,
            owner: None,
            size_bytes: 0,
            correlation_id: Some(correlation_id),
        };
        let identity = IdentityTraceRecord {
            jurisdiction: self.config.jurisdiction,
            timestamp_ms: input.timestamp_ms,
            node_id: self.config.node_id,
            correlation_id,
            provider: input.provider,
            provider_subject: core::mem::take(&mut input.provider_subject),
        };
        network.encode().map_err(|_| ClaimTraceError::Unavailable)?;
        identity
            .encode()
            .map_err(|_| ClaimTraceError::Unavailable)?;

        let mut state = self.state.lock().unwrap_or_else(|error| error.into_inner());
        if state.poisoned {
            return Err(ClaimTraceError::Unavailable);
        }
        let network_bytes = next_capture_headroom_bytes(
            &self.config,
            CompliancePurpose::NetworkTrace,
            self.directory(CompliancePurpose::NetworkTrace)?,
            &state.network,
            &network_key,
            NETWORK_TRACE_FRAME_BYTES,
        )?;
        let identity_bytes = next_capture_headroom_bytes(
            &self.config,
            CompliancePurpose::IdentityTrace,
            self.directory(CompliancePurpose::IdentityTrace)?,
            &state.identity,
            &identity_key,
            IDENTITY_TRACE_FRAME_BYTES,
        )?;
        if !self
            .network_storage_budget
            .has_normal_headroom(network_bytes)
            || !self
                .identity_storage_budget
                .has_normal_headroom(identity_bytes)
        {
            return Err(ClaimTraceError::Capacity);
        }
        let result = (|| {
            append_network(
                self.writer_context(CompliancePurpose::NetworkTrace)?,
                &mut state.network,
                &network_key,
                &network,
            )?;
            append_identity(
                self.writer_context(CompliancePurpose::IdentityTrace)?,
                &mut state.identity,
                &identity_key,
                &identity,
            )?;
            Ok(true)
        })();
        if matches!(result, Err(ClaimTraceError::Unavailable)) {
            state.poisoned = true;
        }
        result
    }

    fn sync_active(&self, _status: &WorkerStatus) -> Result<(), ClaimTraceError> {
        self.assert_writer_leases()?;
        let mut state = self.state.lock().unwrap_or_else(|error| error.into_inner());
        if state.poisoned {
            return Err(ClaimTraceError::Unavailable);
        }
        let result = (|| {
            state
                .network
                .active
                .as_mut()
                .ok_or(ClaimTraceError::Unavailable)?
                .writer
                .sync_data()
                .map_err(|_| ClaimTraceError::Unavailable)?;
            #[cfg(test)]
            if _status.fail_identity_sync.swap(false, Ordering::AcqRel) {
                return Err(ClaimTraceError::Unavailable);
            }
            state
                .identity
                .active
                .as_mut()
                .ok_or(ClaimTraceError::Unavailable)?
                .writer
                .sync_data()
                .map_err(|_| ClaimTraceError::Unavailable)
        })();
        if result.is_err() {
            state.poisoned = true;
        }
        result
    }

    fn resolve_key(
        &self,
        purpose: CompliancePurpose,
        timestamp_ms: u64,
    ) -> Result<ResolvedClaimTraceKey, ClaimTraceError> {
        let resolved = self
            .resolver
            .resolve_trace_key(purpose, self.config.jurisdiction, timestamp_ms)?
            .ok_or(ClaimTraceError::Unavailable)?;
        validate_resolved_key(&resolved, purpose, self.config.jurisdiction, timestamp_ms)?;
        Ok(resolved)
    }

    fn readiness(&self, current_ms: u64) -> Result<(), ClaimTraceError> {
        {
            let state = self.state.lock().unwrap_or_else(|error| error.into_inner());
            if state.poisoned {
                return Err(ClaimTraceError::Unavailable);
            }
        }
        if !self.config.capture_policy.captures(current_ms) {
            return Ok(());
        }
        self.resolver.readiness(current_ms)?;
        let network_key = self.resolve_key(CompliancePurpose::NetworkTrace, current_ms)?;
        let identity_key = self.resolve_key(CompliancePurpose::IdentityTrace, current_ms)?;
        let state = self.state.lock().unwrap_or_else(|error| error.into_inner());
        if state.poisoned {
            return Err(ClaimTraceError::Unavailable);
        }
        let network_bytes = next_capture_headroom_bytes(
            &self.config,
            CompliancePurpose::NetworkTrace,
            self.directory(CompliancePurpose::NetworkTrace)?,
            &state.network,
            &network_key,
            NETWORK_TRACE_FRAME_BYTES,
        )?;
        let identity_bytes = next_capture_headroom_bytes(
            &self.config,
            CompliancePurpose::IdentityTrace,
            self.directory(CompliancePurpose::IdentityTrace)?,
            &state.identity,
            &identity_key,
            IDENTITY_TRACE_FRAME_BYTES,
        )?;
        if !self
            .network_storage_budget
            .has_normal_headroom(network_bytes)
            || !self
                .identity_storage_budget
                .has_normal_headroom(identity_bytes)
        {
            return Err(ClaimTraceError::Capacity);
        }
        Ok(())
    }

    fn next_rollover_deadline(&self) -> Result<Option<u64>, ClaimTraceError> {
        let state = self.state.lock().unwrap_or_else(|error| error.into_inner());
        if state.poisoned
            || (state.network.live.is_none() && state.network.active.is_some())
            || (state.identity.live.is_none() && state.identity.active.is_some())
        {
            return Err(ClaimTraceError::Unavailable);
        }
        let network = state
            .network
            .live
            .as_ref()
            .map(|live| epoch_end_ms(&live.key_id))
            .transpose()?;
        let identity = state
            .identity
            .live
            .as_ref()
            .map(|live| epoch_end_ms(&live.key_id))
            .transpose()?;
        Ok([network, identity].into_iter().flatten().min())
    }

    /// Close either expired purpose from the pair's single writer thread. A purpose that has not
    /// reached its own canonical boundary remains untouched even if the peer purpose closes first.
    fn rollover_expired(&self, current_ms: u64) -> Result<(), ClaimTraceError> {
        self.assert_writer_leases()?;
        let mut state = self.state.lock().unwrap_or_else(|error| error.into_inner());
        if state.poisoned {
            return Err(ClaimTraceError::Unavailable);
        }
        let result = (|| {
            rollover_expired_live(
                self.writer_context(CompliancePurpose::NetworkTrace)?,
                &mut state.network,
                current_ms,
            )?;
            rollover_expired_live(
                self.writer_context(CompliancePurpose::IdentityTrace)?,
                &mut state.identity,
                current_ms,
            )
        })();
        if result.is_err() {
            state.poisoned = true;
        }
        result
    }

    fn shutdown(&self, timestamp_ms: u64) -> Result<(), ClaimTraceError> {
        self.assert_writer_leases()?;
        let mut state = self.state.lock().unwrap_or_else(|error| error.into_inner());
        if state.poisoned {
            return Err(ClaimTraceError::Unavailable);
        }
        finalize_purpose(
            self.directory(CompliancePurpose::NetworkTrace)?,
            &mut state.network,
            &self.network_storage_budget,
            &self.network_signer,
            timestamp_ms,
        )?;
        finalize_purpose(
            self.directory(CompliancePurpose::IdentityTrace)?,
            &mut state.identity,
            &self.identity_storage_budget,
            &self.identity_signer,
            timestamp_ms,
        )?;
        destroy_expired_live(
            self.writer_context(CompliancePurpose::NetworkTrace)?,
            &mut state.network,
            timestamp_ms,
        )?;
        destroy_expired_live(
            self.writer_context(CompliancePurpose::IdentityTrace)?,
            &mut state.identity,
            timestamp_ms,
        )
    }

    fn assert_writer_leases(&self) -> Result<(), ClaimTraceError> {
        self.writer_leases.iter().try_for_each(|lease| {
            lease
                .assert_stable()
                .map_err(|_| ClaimTraceError::Unavailable)
        })
    }
}

impl ClaimTraceSink for SealedClaimTraceSink {
    fn capacity_contract(&self) -> Option<ClaimTraceCapacity> {
        Some(ClaimTraceCapacity {
            policy: TraceRetentionPolicy {
                jurisdiction: self.inner.config.jurisdiction,
                capture: self.inner.config.capture_policy,
                retention_days: self.inner.config.retention_days,
            },
            records_per_minute: self.inner.config.planned_records_per_minute,
            utc_epochs: self.inner.config.capacity_utc_epochs,
            max_records_per_segment: self.inner.config.max_records_per_segment,
            network_logical_limit_bytes: self.inner.config.network_max_storage_bytes,
            identity_logical_limit_bytes: self.inner.config.identity_max_storage_bytes,
        })
    }

    fn readiness(&self, current_ms: u64) -> Result<(), ClaimTraceError> {
        if !self.status.is_running() {
            return Err(ClaimTraceError::Unavailable);
        }
        self.inner.readiness(current_ms)
    }

    fn capture(&self, input: ClaimTraceInput) -> Result<(), ClaimTraceError> {
        self.submit_inner(input)?.blocking_wait()
    }

    fn submit(&self, input: ClaimTraceInput) -> Result<ClaimTraceReceipt, ClaimTraceError> {
        self.submit_inner(input)
    }

    fn shutdown(&self, timestamp_ms: u64) -> Result<(), ClaimTraceError> {
        self.shutdown_worker(timestamp_ms)
    }
}

impl Drop for SealedClaimTraceSink {
    fn drop(&mut self) {
        if self.status.is_running() {
            let _ = self.shutdown_worker(now_ms());
        }
        if let Some(worker) = self
            .worker
            .lock()
            .unwrap_or_else(|error| error.into_inner())
            .take()
        {
            let _ = worker.join();
        }
    }
}

fn receive_next_command(
    inner: &SealedClaimTraceInner,
    receiver: &mpsc::Receiver<ClaimTraceCommand>,
    options: WorkerOptions,
) -> Result<ClaimTraceCommand, ()> {
    loop {
        if !options.enforce_wall_clock {
            return receiver.recv().map_err(|_| ());
        }
        let Some(epoch_end_ms) = inner.next_rollover_deadline().map_err(|_| ())? else {
            return receiver.recv().map_err(|_| ());
        };
        let current_ms = options.clock.now_ms();
        if current_ms >= epoch_end_ms {
            inner.rollover_expired(current_ms).map_err(|_| ())?;
            continue;
        }
        let until_boundary = Duration::from_millis(epoch_end_ms.saturating_sub(current_ms));
        let wait = until_boundary.min(options.rollover_clock_poll);
        match receiver.recv_timeout(wait) {
            Ok(command) => return Ok(command),
            Err(mpsc::RecvTimeoutError::Timeout) => {
                inner
                    .rollover_expired(options.clock.now_ms())
                    .map_err(|_| ())?;
            }
            Err(mpsc::RecvTimeoutError::Disconnected) => return Err(()),
        }
    }
}

fn run_worker(
    inner: Arc<SealedClaimTraceInner>,
    receiver: mpsc::Receiver<ClaimTraceCommand>,
    status: &WorkerStatus,
    options: WorkerOptions,
) -> Result<(), ()> {
    loop {
        let first = receive_next_command(&inner, &receiver, options)?;
        match first {
            ClaimTraceCommand::Capture(first) => {
                let mut batch =
                    Vec::with_capacity(options.batch_max.min(options.queue_capacity + 1));
                batch.push(first);
                let deadline = Instant::now() + options.batch_window;
                let mut shutdown = None;
                while batch.len() < options.batch_max {
                    let remaining = deadline.saturating_duration_since(Instant::now());
                    let next = if remaining.is_zero() {
                        receiver.try_recv().map_err(|error| match error {
                            mpsc::TryRecvError::Empty => mpsc::RecvTimeoutError::Timeout,
                            mpsc::TryRecvError::Disconnected => {
                                mpsc::RecvTimeoutError::Disconnected
                            }
                        })
                    } else {
                        receiver.recv_timeout(remaining)
                    };
                    match next {
                        Ok(ClaimTraceCommand::Capture(command)) => batch.push(command),
                        Ok(ClaimTraceCommand::Shutdown {
                            timestamp_ms,
                            acknowledged,
                        }) => {
                            shutdown = Some((timestamp_ms, acknowledged));
                            break;
                        }
                        #[cfg(test)]
                        Ok(ClaimTraceCommand::Crash) => return Err(()),
                        Err(mpsc::RecvTimeoutError::Timeout) => break,
                        Err(mpsc::RecvTimeoutError::Disconnected) => return Err(()),
                    }
                }
                // A request canceled before work begins cannot leave detached post-timeout work.
                batch.retain(|command| !command.acknowledged.is_closed());
                let outcome = if batch.is_empty() {
                    Ok(())
                } else {
                    process_batch(&inner, &mut batch, status)
                };
                match outcome {
                    Ok(()) => {
                        for command in batch {
                            let _ = command.acknowledged.send(Ok(()));
                        }
                    }
                    Err(ClaimTraceError::Capacity) => {
                        // Storage exhaustion is an availability condition, not writer corruption.
                        // Earlier complete pairs in this batch have already been synced, but none
                        // of the registrations may proceed, so reject the whole batch and retain
                        // the worker for an orderly reserve-backed shutdown.
                        for command in batch {
                            let _ = command.acknowledged.send(Err(ClaimTraceError::Capacity));
                        }
                    }
                    Err(ClaimTraceError::Unavailable) => {
                        status.poison();
                        for command in batch {
                            let _ = command.acknowledged.send(Err(ClaimTraceError::Unavailable));
                        }
                        if let Some((_, acknowledged)) = shutdown {
                            let _ = acknowledged.send(Err(ClaimTraceError::Unavailable));
                        }
                        return Err(());
                    }
                }
                if let Some((timestamp_ms, acknowledged)) = shutdown {
                    let outcome = inner.shutdown(timestamp_ms);
                    let failed = outcome.is_err();
                    if failed {
                        status.poison();
                    }
                    let _ = acknowledged.send(outcome);
                    return if failed { Err(()) } else { Ok(()) };
                }
            }
            ClaimTraceCommand::Shutdown {
                timestamp_ms,
                acknowledged,
            } => {
                let outcome = inner.shutdown(timestamp_ms);
                let failed = outcome.is_err();
                if failed {
                    status.poison();
                }
                let _ = acknowledged.send(outcome);
                return if failed { Err(()) } else { Ok(()) };
            }
            #[cfg(test)]
            ClaimTraceCommand::Crash => return Err(()),
        }
    }
}

fn process_batch(
    inner: &SealedClaimTraceInner,
    batch: &mut [CaptureCommand],
    _status: &WorkerStatus,
) -> Result<(), ClaimTraceError> {
    let mut appended = false;
    for command in batch {
        match inner.capture_buffered(&mut command.input) {
            Ok(wrote) => appended |= wrote,
            Err(ClaimTraceError::Capacity) => {
                sync_appended(inner, appended, _status)?;
                return Err(ClaimTraceError::Capacity);
            }
            Err(ClaimTraceError::Unavailable) => return Err(ClaimTraceError::Unavailable),
        }
    }
    sync_appended(inner, appended, _status)
}

fn sync_appended(
    inner: &SealedClaimTraceInner,
    appended: bool,
    _status: &WorkerStatus,
) -> Result<(), ClaimTraceError> {
    if !appended {
        return Ok(());
    }
    #[cfg(test)]
    if _status.panic_before_sync.swap(false, Ordering::AcqRel) {
        panic!("injected claim trace worker panic before durable sync");
    }
    #[cfg(test)]
    if _status.fail_before_sync.swap(false, Ordering::AcqRel) {
        return Err(ClaimTraceError::Unavailable);
    }
    inner.sync_active(_status)?;
    #[cfg(test)]
    _status.durable_sync_batches.fetch_add(1, Ordering::AcqRel);
    Ok(())
}

fn next_capture_headroom_bytes(
    _config: &SealedClaimTraceConfig,
    _purpose: CompliancePurpose,
    directory: TraceDirectory<'_>,
    state: &PurposeState,
    resolved: &ResolvedClaimTraceKey,
    frame_bytes: u64,
) -> Result<u64, ClaimTraceError> {
    if state.live.is_none() && state.active.is_some() {
        return Err(ClaimTraceError::Unavailable);
    }
    let needs_live_key = state.live.is_none();
    let changes_epoch = state
        .live
        .as_ref()
        .is_some_and(|live| live.key_id != resolved.key_id);
    let active_is_full = state
        .active
        .as_ref()
        .is_some_and(|active| active.writer.record_count() >= active.writer.header().max_records());
    let needs_header = needs_live_key || changes_epoch || state.active.is_none() || active_is_full;
    let ordinary_bytes = frame_bytes
        .checked_add(if needs_live_key {
            LIVE_KEY_LEN as u64
        } else {
            0
        })
        .and_then(|bytes| {
            bytes.checked_add(if needs_header {
                SEGMENT_HEADER_LEN as u64
            } else {
                0
            })
        })
        .ok_or(ClaimTraceError::Unavailable)?;
    let Some(live) = state.live.as_ref() else {
        return Ok(ordinary_bytes);
    };
    if !changes_epoch {
        return ordinary_bytes
            .checked_add(if active_is_full {
                SEGMENT_FOOTER_LEN as u64
            } else {
                0
            })
            .ok_or(ClaimTraceError::Unavailable);
    }

    let mut terminal_bytes = if state.active.is_some() {
        SEGMENT_FOOTER_LEN as u64
    } else {
        0
    };
    let manifest_path = epoch_manifest_path(directory.path, &live.key_id)
        .map_err(|_| ClaimTraceError::Unavailable)?;
    if !trace_entry_exists(directory, &manifest_path)? {
        let segment_count = live
            .segment_index
            .checked_add(u32::from(state.active.is_some()))
            .filter(|count| *count <= MAX_EPOCH_MANIFEST_SEGMENTS)
            .ok_or(ClaimTraceError::Unavailable)?;
        let manifest_bytes = (segment_count as u64)
            .checked_mul(EPOCH_MANIFEST_ENTRY_LEN as u64)
            .and_then(|entries| entries.checked_add(EPOCH_MANIFEST_FIXED_LEN as u64))
            .ok_or(ClaimTraceError::Unavailable)?;
        terminal_bytes = terminal_bytes
            .checked_add(manifest_bytes)
            .ok_or(ClaimTraceError::Unavailable)?;
    }
    ordinary_bytes
        .checked_add(terminal_bytes)
        .ok_or(ClaimTraceError::Unavailable)
}

#[derive(Clone, Copy)]
struct PurposeWriterContext<'a> {
    config: &'a SealedClaimTraceConfig,
    directory: TraceDirectory<'a>,
    purpose: CompliancePurpose,
    storage_budget: &'a TraceStorageBudget,
    signer: &'a SigningKey,
    resolver: &'a dyn ClaimTraceKeyResolver,
}

fn append_network(
    context: PurposeWriterContext<'_>,
    state: &mut PurposeState,
    resolved: &ResolvedClaimTraceKey,
    record: &TraceRecord,
) -> Result<(), ClaimTraceError> {
    ensure_writer(context, state, resolved, record.timestamp_ms)?;
    context
        .storage_budget
        .charge_normal(NETWORK_TRACE_FRAME_BYTES)
        .map_err(|_| ClaimTraceError::Capacity)?;
    match state
        .active
        .as_mut()
        .ok_or(ClaimTraceError::Unavailable)?
        .writer
        .append_network_buffered(record)
    {
        Ok(_) => {}
        Err(pigeonpost_compliance_seal::SealError::SegmentFull) => {
            context
                .storage_budget
                .release(NETWORK_TRACE_FRAME_BYTES)
                .map_err(|_| ClaimTraceError::Unavailable)?;
            finalize_purpose(
                context.directory,
                state,
                context.storage_budget,
                context.signer,
                record.timestamp_ms,
            )?;
            ensure_writer(context, state, resolved, record.timestamp_ms)?;
            context
                .storage_budget
                .charge_normal(NETWORK_TRACE_FRAME_BYTES)
                .map_err(|_| ClaimTraceError::Capacity)?;
            state
                .active
                .as_mut()
                .ok_or(ClaimTraceError::Unavailable)?
                .writer
                .append_network_buffered(record)
                .map_err(|_| ClaimTraceError::Unavailable)?;
        }
        Err(_) => return Err(ClaimTraceError::Unavailable),
    }
    let active = state.active.as_mut().ok_or(ClaimTraceError::Unavailable)?;
    active.last_record_ms = active.last_record_ms.max(record.timestamp_ms);
    Ok(())
}

fn append_identity(
    context: PurposeWriterContext<'_>,
    state: &mut PurposeState,
    resolved: &ResolvedClaimTraceKey,
    record: &IdentityTraceRecord,
) -> Result<(), ClaimTraceError> {
    ensure_writer(context, state, resolved, record.timestamp_ms)?;
    context
        .storage_budget
        .charge_normal(IDENTITY_TRACE_FRAME_BYTES)
        .map_err(|_| ClaimTraceError::Capacity)?;
    match state
        .active
        .as_mut()
        .ok_or(ClaimTraceError::Unavailable)?
        .writer
        .append_identity_buffered(record)
    {
        Ok(_) => {}
        Err(pigeonpost_compliance_seal::SealError::SegmentFull) => {
            context
                .storage_budget
                .release(IDENTITY_TRACE_FRAME_BYTES)
                .map_err(|_| ClaimTraceError::Unavailable)?;
            finalize_purpose(
                context.directory,
                state,
                context.storage_budget,
                context.signer,
                record.timestamp_ms,
            )?;
            ensure_writer(context, state, resolved, record.timestamp_ms)?;
            context
                .storage_budget
                .charge_normal(IDENTITY_TRACE_FRAME_BYTES)
                .map_err(|_| ClaimTraceError::Capacity)?;
            state
                .active
                .as_mut()
                .ok_or(ClaimTraceError::Unavailable)?
                .writer
                .append_identity_buffered(record)
                .map_err(|_| ClaimTraceError::Unavailable)?;
        }
        Err(_) => return Err(ClaimTraceError::Unavailable),
    }
    let active = state.active.as_mut().ok_or(ClaimTraceError::Unavailable)?;
    active.last_record_ms = active.last_record_ms.max(record.timestamp_ms);
    Ok(())
}

fn ensure_writer(
    context: PurposeWriterContext<'_>,
    state: &mut PurposeState,
    resolved: &ResolvedClaimTraceKey,
    timestamp_ms: u64,
) -> Result<(), ClaimTraceError> {
    let PurposeWriterContext {
        config,
        directory,
        purpose,
        storage_budget,
        signer,
        resolver,
    } = context;
    if state
        .live
        .as_ref()
        .is_some_and(|live| live.key_id != resolved.key_id)
    {
        let closing_key = state
            .live
            .as_ref()
            .ok_or(ClaimTraceError::Unavailable)?
            .key_id;
        let closing_epoch_end = epoch_end_ms(&closing_key)?;
        if closing_key.epoch_start_ms > resolved.key_id.epoch_start_ms
            || timestamp_ms < closing_epoch_end
            || resolved.key_id.epoch_start_ms < closing_epoch_end
        {
            return Err(ClaimTraceError::Unavailable);
        }
        finalize_purpose(
            directory,
            state,
            storage_budget,
            signer,
            closing_epoch_end.saturating_sub(1),
        )?;
        publish_terminal_manifest(
            config,
            purpose,
            directory,
            state,
            storage_budget,
            signer,
            resolver,
        )?;
        destroy_live_key(directory, state, storage_budget)?;
    }
    if state.live.is_none() {
        let mut secret = [0u8; 32];
        while secret == [0u8; 32] {
            OsRng.fill_bytes(&mut secret);
        }
        let live = LiveKeyState {
            key_id: resolved.key_id,
            segment_index: 0,
            secret,
        };
        storage_budget
            .charge_normal(LIVE_KEY_LEN as u64)
            .map_err(|_| ClaimTraceError::Capacity)?;
        persist_live_key(directory, &live)?;
        state.live = Some(live);
    }
    if state.active.is_none() {
        let live = state.live.as_ref().ok_or(ClaimTraceError::Unavailable)?;
        if live.key_id != resolved.key_id || live.segment_index >= MAX_EPOCH_MANIFEST_SEGMENTS {
            return Err(ClaimTraceError::Unavailable);
        }
        let path = segment_path(directory.path, &live.key_id, live.segment_index)?;
        storage_budget
            .charge_normal(SEGMENT_HEADER_LEN as u64)
            .map_err(|_| ClaimTraceError::Capacity)?;
        let writer = SegmentWriter::create(
            path,
            EpochSealingKey::from_bytes(live.key_id, live.secret)
                .map_err(|_| ClaimTraceError::Unavailable)?,
            &resolved.public_key,
            signer.verifying_key().to_bytes(),
            timestamp_ms,
            config.max_records_per_segment,
        )
        .map_err(|_| ClaimTraceError::Unavailable)?;
        state.active = Some(ActiveSegment {
            writer,
            last_record_ms: timestamp_ms,
        });
    }
    Ok(())
}

fn finalize_purpose(
    directory: TraceDirectory<'_>,
    state: &mut PurposeState,
    storage_budget: &TraceStorageBudget,
    signer: &SigningKey,
    requested_close_ms: u64,
) -> Result<(), ClaimTraceError> {
    let Some(active) = state.active.take() else {
        return Ok(());
    };
    let key_id = active.writer.header().key_id();
    let close_ms = requested_close_ms
        .max(active.writer.header().opened_at_ms())
        .max(active.last_record_ms)
        .min(epoch_end_ms(&key_id)?.saturating_sub(1));
    storage_budget
        .charge_terminal(SEGMENT_FOOTER_LEN as u64)
        .map_err(|_| ClaimTraceError::Unavailable)?;
    active
        .writer
        .finalize_durable(close_ms, signer)
        .map_err(|_| ClaimTraceError::Unavailable)?;
    let live = state.live.as_mut().ok_or(ClaimTraceError::Unavailable)?;
    live.segment_index = live
        .segment_index
        .checked_add(1)
        .ok_or(ClaimTraceError::Unavailable)?;
    persist_live_key(directory, live)
}

fn recover_purpose(
    config: &SealedClaimTraceConfig,
    purpose: CompliancePurpose,
    directory: TraceDirectory<'_>,
    storage_budget: &TraceStorageBudget,
    signer: &SigningKey,
    resolver: &Arc<dyn ClaimTraceKeyResolver>,
    current_ms: u64,
) -> Result<PurposeState, ClaimTraceError> {
    let Some(mut live) = load_live_key(directory)? else {
        return Ok(PurposeState::default());
    };
    if live.key_id.purpose != purpose
        || live.key_id.jurisdiction != config.jurisdiction
        || canonical_trace_epoch_end_ms(&live.key_id).is_err()
        || live.secret == [0u8; 32]
        || current_ms < live.key_id.epoch_start_ms
    {
        return Err(ClaimTraceError::Unavailable);
    }
    let final_path = segment_path(directory.path, &live.key_id, live.segment_index)?;
    let open_path = open_path_for(&final_path);
    if trace_entry_exists(directory, &final_path)? && trace_entry_exists(directory, &open_path)? {
        return Err(ClaimTraceError::Unavailable);
    }
    let mut active = None;
    let expected_commitment: [u8; 32] = Sha256::digest(live.secret).into();
    if trace_entry_exists(directory, &open_path)? {
        let key = EpochSealingKey::from_bytes(live.key_id, live.secret)
            .map_err(|_| ClaimTraceError::Unavailable)?;
        match recover_segment(&final_path, key).map_err(|_| ClaimTraceError::Unavailable)? {
            Recovery::Resumed(writer) => {
                if writer.header().signer_public_key() != signer.verifying_key().to_bytes() {
                    return Err(ClaimTraceError::Unavailable);
                }
                active = Some(ActiveSegment {
                    last_record_ms: writer.header().opened_at_ms(),
                    writer,
                });
            }
            Recovery::Finalized(verified) => {
                if verified.header.key_id() != live.key_id
                    || verified.header.signer_public_key() != signer.verifying_key().to_bytes()
                    || verified.header.wrapped_epoch_key().epoch_key_commitment()
                        != expected_commitment
                {
                    return Err(ClaimTraceError::Unavailable);
                }
                live.segment_index = live
                    .segment_index
                    .checked_add(1)
                    .ok_or(ClaimTraceError::Unavailable)?;
                persist_live_key(directory, &live)?;
            }
        }
    } else if trace_entry_exists(directory, &final_path)? {
        let verified = verify_segment(&final_path).map_err(|_| ClaimTraceError::Unavailable)?;
        if verified.header.key_id() != live.key_id
            || verified.header.signer_public_key() != signer.verifying_key().to_bytes()
            || verified.header.wrapped_epoch_key().epoch_key_commitment() != expected_commitment
        {
            return Err(ClaimTraceError::Unavailable);
        }
        live.segment_index = live
            .segment_index
            .checked_add(1)
            .ok_or(ClaimTraceError::Unavailable)?;
        persist_live_key(directory, &live)?;
    }
    let mut state = PurposeState {
        live: Some(live),
        active,
    };
    let recovered_epoch_end = epoch_end_ms(
        &state
            .live
            .as_ref()
            .ok_or(ClaimTraceError::Unavailable)?
            .key_id,
    )?;
    if current_ms >= recovered_epoch_end {
        finalize_purpose(
            directory,
            &mut state,
            storage_budget,
            signer,
            recovered_epoch_end.saturating_sub(1),
        )?;
        publish_terminal_manifest(
            config,
            purpose,
            directory,
            &state,
            storage_budget,
            signer,
            resolver.as_ref(),
        )?;
        destroy_live_key(directory, &mut state, storage_budget)?;
    }
    Ok(state)
}

fn rollover_expired_live(
    context: PurposeWriterContext<'_>,
    state: &mut PurposeState,
    timestamp_ms: u64,
) -> Result<(), ClaimTraceError> {
    let Some(epoch_end) = state
        .live
        .as_ref()
        .map(|live| epoch_end_ms(&live.key_id))
        .transpose()?
    else {
        return if state.active.is_none() {
            Ok(())
        } else {
            Err(ClaimTraceError::Unavailable)
        };
    };
    if timestamp_ms < epoch_end {
        return Ok(());
    }
    finalize_purpose(
        context.directory,
        state,
        context.storage_budget,
        context.signer,
        epoch_end.saturating_sub(1),
    )?;
    destroy_expired_live(context, state, timestamp_ms)
}

fn destroy_expired_live(
    context: PurposeWriterContext<'_>,
    state: &mut PurposeState,
    timestamp_ms: u64,
) -> Result<(), ClaimTraceError> {
    if state
        .live
        .as_ref()
        .is_some_and(|live| timestamp_ms >= epoch_end_ms(&live.key_id).unwrap_or(u64::MAX))
    {
        publish_terminal_manifest(
            context.config,
            context.purpose,
            context.directory,
            state,
            context.storage_budget,
            context.signer,
            context.resolver,
        )?;
        destroy_live_key(context.directory, state, context.storage_budget)?;
    }
    Ok(())
}

fn publish_terminal_manifest(
    config: &SealedClaimTraceConfig,
    purpose: CompliancePurpose,
    directory: TraceDirectory<'_>,
    state: &PurposeState,
    storage_budget: &TraceStorageBudget,
    signer: &SigningKey,
    resolver: &dyn ClaimTraceKeyResolver,
) -> Result<(), ClaimTraceError> {
    if state.active.is_some() {
        return Err(ClaimTraceError::Unavailable);
    }
    let live = state.live.as_ref().ok_or(ClaimTraceError::Unavailable)?;
    if live.key_id.purpose != purpose || live.segment_index > MAX_EPOCH_MANIFEST_SEGMENTS {
        return Err(ClaimTraceError::Unavailable);
    }
    let expected_signer = signer.verifying_key().to_bytes();
    let expected_commitment: [u8; 32] = Sha256::digest(live.secret).into();
    let mut custody_digest = None;
    let mut segments = Vec::with_capacity(live.segment_index as usize);
    for index in 0..live.segment_index {
        let path = segment_path(directory.path, &live.key_id, index)?;
        let segment = verify_segment(path).map_err(|_| ClaimTraceError::Unavailable)?;
        if segment.header.key_id() != live.key_id
            || segment.header.signer_public_key() != expected_signer
            || segment.header.wrapped_epoch_key().epoch_key_commitment() != expected_commitment
        {
            return Err(ClaimTraceError::Unavailable);
        }
        let digest = segment.header.wrapped_epoch_key().compliance_key_digest();
        if custody_digest.is_some_and(|expected| expected != digest) {
            return Err(ClaimTraceError::Unavailable);
        }
        custody_digest = Some(digest);
        segments.push(
            EpochSegmentEntry::from_verified(index, &segment)
                .map_err(|_| ClaimTraceError::Unavailable)?,
        );
    }
    let custody_digest = match custody_digest {
        Some(digest) => digest,
        None => {
            let resolved = resolver
                .resolve_trace_key(purpose, config.jurisdiction, live.key_id.epoch_start_ms)?
                .ok_or(ClaimTraceError::Unavailable)?;
            validate_resolved_key(
                &resolved,
                purpose,
                config.jurisdiction,
                live.key_id.epoch_start_ms,
            )?;
            if resolved.key_id != live.key_id {
                return Err(ClaimTraceError::Unavailable);
            }
            Sha256::digest(resolved.public_key).into()
        }
    };
    let manifest = EpochManifest::new_signed(
        live.key_id,
        config.node_id,
        custody_digest,
        expected_commitment,
        segments,
        signer,
    )
    .map_err(|_| ClaimTraceError::Unavailable)?;
    let path = epoch_manifest_path(directory.path, &live.key_id)
        .map_err(|_| ClaimTraceError::Unavailable)?;
    if !trace_entry_exists(directory, &path)? {
        let manifest_bytes = manifest
            .encode()
            .map_err(|_| ClaimTraceError::Unavailable)?;
        storage_budget
            .charge_terminal(u64::try_from(manifest_bytes.len()).unwrap_or(u64::MAX))
            .map_err(|_| ClaimTraceError::Unavailable)?;
    }
    publish_epoch_manifest(path, &manifest).map_err(|_| ClaimTraceError::Unavailable)
}

fn destroy_live_key(
    directory: TraceDirectory<'_>,
    state: &mut PurposeState,
    storage_budget: &TraceStorageBudget,
) -> Result<(), ClaimTraceError> {
    if state.active.is_some() {
        return Err(ClaimTraceError::Unavailable);
    }
    #[cfg(unix)]
    {
        let name = live_key_name()?;
        if let Some(opened) = directory
            .guard
            .open_file_optional(
                &name,
                OpenAccess::ReadOnly,
                FilePolicy::private_exact(LIVE_KEY_LEN as u64),
            )
            .map_err(map_custody_error)?
        {
            directory
                .guard
                .unlink_file(opened)
                .map_err(map_custody_error)?;
        }
    }
    #[cfg(not(unix))]
    {
        let path = directory.path.join(LIVE_KEY_NAME);
        let opened = open_live_key(&path)?;
        match (opened.as_ref(), fs::remove_file(&path)) {
            (Some(_), Ok(())) => {}
            (None, Err(error)) if error.kind() == std::io::ErrorKind::NotFound => {}
            _ => return Err(ClaimTraceError::Unavailable),
        }
        sync_parent(directory.path)?;
    }
    state.live = None;
    storage_budget
        .release(LIVE_KEY_LEN as u64)
        .map_err(|_| ClaimTraceError::Unavailable)?;
    Ok(())
}

#[cfg(test)]
std::thread_local! {
    static CONFIG_VALIDATION_CALLS: std::cell::Cell<u32> = const { std::cell::Cell::new(0) };
}

fn validate_config(config: &SealedClaimTraceConfig) -> Result<(), ClaimTraceError> {
    #[cfg(test)]
    CONFIG_VALIDATION_CALLS.with(|calls| calls.set(calls.get() + 1));
    if config.network_directory.as_os_str().is_empty()
        || config.identity_directory.as_os_str().is_empty()
        || config.node_id == [0u8; 32]
        || config.max_records_per_segment == 0
        || config.max_records_per_segment > MAX_SEGMENT_RECORDS
    {
        return Err(ClaimTraceError::Unavailable);
    }
    let required_epochs = TraceRetentionPolicy {
        jurisdiction: config.jurisdiction,
        capture: config.capture_policy,
        retention_days: config.retention_days,
    }
    .required_capacity_epochs()
    .map_err(|_| ClaimTraceError::Unavailable)?;
    if config.capacity_utc_epochs < required_epochs {
        return Err(ClaimTraceError::Capacity);
    }
    let network_required = required_network_trace_storage_bytes(
        config.planned_records_per_minute,
        config.capacity_utc_epochs,
        config.max_records_per_segment,
    )
    .map_err(|_| ClaimTraceError::Unavailable)?;
    let identity_required = required_identity_trace_storage_bytes(
        config.planned_records_per_minute,
        config.capacity_utc_epochs,
        config.max_records_per_segment,
    )
    .map_err(|_| ClaimTraceError::Unavailable)?;
    if config.network_max_storage_bytes < network_required
        || config.identity_max_storage_bytes < identity_required
    {
        return Err(ClaimTraceError::Capacity);
    }
    Ok(())
}

fn require_supported_persistent_trace_platform(
    platform: PersistentTracePlatform,
) -> Result<(), ClaimTraceError> {
    if platform.supported_persistent_target {
        Ok(())
    } else {
        Err(ClaimTraceError::Unavailable)
    }
}

fn validate_input(
    input: &ClaimTraceInput,
    enforce_wall_clock: bool,
) -> Result<(), ClaimTraceError> {
    let current = now_ms();
    if input.timestamp_ms == 0
        || (enforce_wall_clock
            && (input.timestamp_ms > current.saturating_add(MAX_CLOCK_SKEW_MS)
                || current > input.timestamp_ms.saturating_add(MAX_CLOCK_SKEW_MS)))
        || input.source.port() == 0
        || invalid_ip(input.source.ip())
    {
        return Err(ClaimTraceError::Unavailable);
    }
    Ok(())
}

fn invalid_ip(ip: IpAddr) -> bool {
    ip.is_unspecified() || ip.is_multicast()
}

fn validate_resolved_key(
    resolved: &ResolvedClaimTraceKey,
    purpose: CompliancePurpose,
    jurisdiction: Jurisdiction,
    timestamp_ms: u64,
) -> Result<(), ClaimTraceError> {
    let epoch_start = day_start_ms(timestamp_ms);
    if resolved.key_id.purpose != purpose
        || resolved.key_id.jurisdiction != jurisdiction
        || resolved.key_id.epoch_start_ms != epoch_start
        || validate_trace_epoch(
            &resolved.key_id,
            resolved.not_before_ms,
            resolved.not_after_ms,
        )
        .is_err()
        || resolved.public_key == [0u8; 32]
        || trace_epoch_contains(&resolved.key_id, timestamp_ms) != Ok(true)
    {
        return Err(ClaimTraceError::Unavailable);
    }
    Ok(())
}

#[cfg(unix)]
fn secure_separate_directories(
    network: &Path,
    identity: &Path,
) -> Result<[GuardedDir; 2], ClaimTraceError> {
    let network_path = NormalizedPath::new(network).map_err(map_custody_error)?;
    let identity_path = NormalizedPath::new(identity).map_err(map_custody_error)?;
    if network_path == identity_path
        || network_path.as_path().starts_with(identity_path.as_path())
        || identity_path.as_path().starts_with(network_path.as_path())
    {
        return Err(ClaimTraceError::Unavailable);
    }
    let network = GuardedDir::create_private(network_path).map_err(map_custody_error)?;
    let identity = GuardedDir::create_private(identity_path).map_err(map_custody_error)?;
    if network.identity() == identity.identity()
        || network
            .absolute_path()
            .starts_with(identity.absolute_path())
        || identity
            .absolute_path()
            .starts_with(network.absolute_path())
    {
        return Err(ClaimTraceError::Unavailable);
    }
    network.verify_named().map_err(map_custody_error)?;
    identity.verify_named().map_err(map_custody_error)?;
    Ok([network, identity])
}

#[cfg(not(unix))]
fn secure_separate_directories(network: &Path, identity: &Path) -> Result<(), ClaimTraceError> {
    secure_directory(network)?;
    secure_directory(identity)?;
    let network = fs::canonicalize(network).map_err(|_| ClaimTraceError::Unavailable)?;
    let identity = fs::canonicalize(identity).map_err(|_| ClaimTraceError::Unavailable)?;
    if network == identity || network.starts_with(&identity) || identity.starts_with(&network) {
        return Err(ClaimTraceError::Unavailable);
    }
    Ok(())
}

fn acquire_ordered_writer_leases(
    network: TraceDirectory<'_>,
    identity: TraceDirectory<'_>,
) -> Result<[TraceWriterLease; 2], ClaimTraceError> {
    #[cfg(unix)]
    {
        network.guard.verify_named().map_err(map_custody_error)?;
        identity.guard.verify_named().map_err(map_custody_error)?;
    }
    let mut directories = [network.path.to_path_buf(), identity.path.to_path_buf()];
    #[cfg(not(unix))]
    for directory in &mut directories {
        *directory = fs::canonicalize(&*directory).map_err(|_| ClaimTraceError::Unavailable)?;
    }
    directories.sort();
    let first =
        TraceWriterLease::acquire(&directories[0]).map_err(|_| ClaimTraceError::Unavailable)?;
    let second =
        TraceWriterLease::acquire(&directories[1]).map_err(|_| ClaimTraceError::Unavailable)?;
    #[cfg(unix)]
    {
        network.guard.verify_named().map_err(map_custody_error)?;
        identity.guard.verify_named().map_err(map_custody_error)?;
    }
    Ok([first, second])
}

#[cfg(not(unix))]
fn secure_directory(path: &Path) -> Result<(), ClaimTraceError> {
    fs::create_dir_all(path).map_err(|_| ClaimTraceError::Unavailable)?;
    let metadata = fs::symlink_metadata(path).map_err(|_| ClaimTraceError::Unavailable)?;
    if metadata.file_type().is_symlink() || !metadata.is_dir() {
        return Err(ClaimTraceError::Unavailable);
    }
    Ok(())
}

fn day_start_ms(timestamp_ms: u64) -> u64 {
    timestamp_ms - timestamp_ms % TRACE_EPOCH_DURATION_MS
}

fn epoch_end_ms(key_id: &ComplianceKeyId) -> Result<u64, ClaimTraceError> {
    canonical_trace_epoch_end_ms(key_id).map_err(|_| ClaimTraceError::Unavailable)
}

fn segment_path(
    directory: &Path,
    key_id: &ComplianceKeyId,
    segment_index: u32,
) -> Result<PathBuf, ClaimTraceError> {
    let encoded = key_id.encode().map_err(|_| ClaimTraceError::Unavailable)?;
    Ok(directory.join(format!(
        "{}-{}-{segment_index:08}.pptrace",
        purpose_name(key_id.purpose)?,
        hex(&encoded)
    )))
}

fn purpose_name(purpose: CompliancePurpose) -> Result<&'static str, ClaimTraceError> {
    match purpose {
        CompliancePurpose::NetworkTrace => Ok("network"),
        CompliancePurpose::IdentityTrace => Ok("identity"),
        CompliancePurpose::Attribution => Err(ClaimTraceError::Unavailable),
    }
}

fn open_path_for(final_path: &Path) -> PathBuf {
    let mut value = final_path.as_os_str().to_os_string();
    value.push(".open");
    PathBuf::from(value)
}

#[cfg(unix)]
fn live_key_name() -> Result<LeafName, ClaimTraceError> {
    LeafName::new(LIVE_KEY_NAME).map_err(map_custody_error)
}

#[cfg(unix)]
fn trace_leaf(directory: TraceDirectory<'_>, path: &Path) -> Result<LeafName, ClaimTraceError> {
    if path.parent() != Some(directory.path) {
        return Err(ClaimTraceError::Unavailable);
    }
    path.file_name()
        .ok_or(ClaimTraceError::Unavailable)
        .and_then(|name| LeafName::new(name).map_err(map_custody_error))
}

fn trace_entry_exists(directory: TraceDirectory<'_>, path: &Path) -> Result<bool, ClaimTraceError> {
    #[cfg(unix)]
    {
        directory.guard.verify_named().map_err(map_custody_error)?;
        let name = trace_leaf(directory, path)?;
        directory
            .guard
            .entry_metadata(&name)
            .map(|metadata| metadata.is_some())
            .map_err(map_custody_error)
    }
    #[cfg(not(unix))]
    {
        let _ = directory;
        Ok(path.exists())
    }
}

#[cfg(unix)]
fn load_live_key(directory: TraceDirectory<'_>) -> Result<Option<LiveKeyState>, ClaimTraceError> {
    directory.guard.verify_named().map_err(map_custody_error)?;
    let name = live_key_name()?;
    let Some(mut file) = directory
        .guard
        .open_file_optional(
            &name,
            OpenAccess::ReadOnly,
            FilePolicy::private_exact(LIVE_KEY_LEN as u64),
        )
        .map_err(map_custody_error)?
    else {
        return Ok(None);
    };
    let opened = file.metadata().map_err(map_custody_error)?;
    let mut bytes = Vec::with_capacity(LIVE_KEY_LEN + 1);
    (&mut file)
        .take((LIVE_KEY_LEN + 1) as u64)
        .read_to_end(&mut bytes)
        .map_err(|_| ClaimTraceError::Unavailable)?;
    let final_metadata = file.metadata().map_err(map_custody_error)?;
    if bytes.len() != LIVE_KEY_LEN || opened != final_metadata {
        bytes.zeroize();
        return Err(ClaimTraceError::Unavailable);
    }
    if file.verify_named().is_err() {
        bytes.zeroize();
        return Err(ClaimTraceError::Unavailable);
    }
    decode_live_key(&mut bytes)
}

#[cfg(all(test, unix))]
fn load_live_key_at(path: &Path) -> Result<Option<LiveKeyState>, ClaimTraceError> {
    let parent = path.parent().ok_or(ClaimTraceError::Unavailable)?;
    let guard = GuardedDir::open_existing(
        parent,
        pigeonpost_unix_custody::DirPolicy::private_mutable(),
    )
    .map_err(map_custody_error)?;
    if path.file_name() != Some(std::ffi::OsStr::new(LIVE_KEY_NAME)) {
        return Err(ClaimTraceError::Unavailable);
    }
    load_live_key(TraceDirectory {
        path: guard.absolute_path(),
        guard: &guard,
    })
}

#[cfg(not(unix))]
fn load_live_key(directory: TraceDirectory<'_>) -> Result<Option<LiveKeyState>, ClaimTraceError> {
    let path = directory.path.join(LIVE_KEY_NAME);
    let Some(file) = open_live_key(&path)? else {
        return Ok(None);
    };
    let mut bytes = Vec::with_capacity(LIVE_KEY_LEN + 1);
    (&file)
        .take((LIVE_KEY_LEN + 1) as u64)
        .read_to_end(&mut bytes)
        .map_err(|_| ClaimTraceError::Unavailable)?;
    if !named_live_key_matches(&file, &path) {
        bytes.zeroize();
        return Err(ClaimTraceError::Unavailable);
    }
    decode_live_key(&mut bytes)
}

#[cfg(all(test, not(unix)))]
fn load_live_key_at(path: &Path) -> Result<Option<LiveKeyState>, ClaimTraceError> {
    let parent = path.parent().ok_or(ClaimTraceError::Unavailable)?;
    load_live_key(TraceDirectory { path: parent })
}

fn decode_live_key(bytes: &mut Vec<u8>) -> Result<Option<LiveKeyState>, ClaimTraceError> {
    let decoded = (|| {
        if bytes.len() != LIVE_KEY_LEN
            || &bytes[..8] != LIVE_KEY_MAGIC
            || bytes[8] != LIVE_KEY_VERSION
        {
            return Err(ClaimTraceError::Unavailable);
        }
        let key_id = ComplianceKeyId::decode(&bytes[9..9 + COMPLIANCE_KEY_ID_LEN])
            .map_err(|_| ClaimTraceError::Unavailable)?;
        let cursor = 9 + COMPLIANCE_KEY_ID_LEN;
        let segment_index = u32::from_be_bytes(
            bytes[cursor..cursor + 4]
                .try_into()
                .map_err(|_| ClaimTraceError::Unavailable)?,
        );
        let mut secret = [0u8; 32];
        secret.copy_from_slice(&bytes[cursor + 4..]);
        if secret == [0u8; 32] {
            return Err(ClaimTraceError::Unavailable);
        }
        Ok(Some(LiveKeyState {
            key_id,
            segment_index,
            secret,
        }))
    })();
    bytes.zeroize();
    decoded
}

#[cfg(windows)]
fn open_live_key(path: &Path) -> Result<Option<File>, ClaimTraceError> {
    use std::os::windows::fs::{MetadataExt, OpenOptionsExt};

    const FILE_ATTRIBUTE_REPARSE_POINT: u32 = 0x0000_0400;
    const FILE_FLAG_OPEN_REPARSE_POINT: u32 = 0x0020_0000;
    let mut options = OpenOptions::new();
    options
        .read(true)
        .custom_flags(FILE_FLAG_OPEN_REPARSE_POINT);
    let file = match options.open(path) {
        Ok(file) => file,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(_) => return Err(ClaimTraceError::Unavailable),
    };
    let metadata = file.metadata().map_err(|_| ClaimTraceError::Unavailable)?;
    if !metadata.is_file()
        || metadata.len() != LIVE_KEY_LEN as u64
        || metadata.file_attributes() & FILE_ATTRIBUTE_REPARSE_POINT != 0
    {
        return Err(ClaimTraceError::Unavailable);
    }
    Ok(Some(file))
}

#[cfg(not(any(unix, windows)))]
fn open_live_key(path: &Path) -> Result<Option<File>, ClaimTraceError> {
    let metadata = match fs::symlink_metadata(path) {
        Ok(metadata) => metadata,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(_) => return Err(ClaimTraceError::Unavailable),
    };
    if metadata.file_type().is_symlink()
        || !metadata.is_file()
        || metadata.len() != LIVE_KEY_LEN as u64
    {
        return Err(ClaimTraceError::Unavailable);
    }
    File::open(path)
        .map(Some)
        .map_err(|_| ClaimTraceError::Unavailable)
}

#[cfg(not(unix))]
fn named_live_key_matches(file: &File, path: &Path) -> bool {
    file.metadata().is_ok_and(|metadata| metadata.is_file())
        && fs::symlink_metadata(path)
            .is_ok_and(|metadata| !metadata.file_type().is_symlink() && metadata.is_file())
}

fn encode_live_key(live: &LiveKeyState) -> Result<[u8; LIVE_KEY_LEN], ClaimTraceError> {
    let mut bytes = [0u8; LIVE_KEY_LEN];
    bytes[..8].copy_from_slice(LIVE_KEY_MAGIC);
    bytes[8] = LIVE_KEY_VERSION;
    bytes[9..9 + COMPLIANCE_KEY_ID_LEN].copy_from_slice(
        &live
            .key_id
            .encode()
            .map_err(|_| ClaimTraceError::Unavailable)?,
    );
    let cursor = 9 + COMPLIANCE_KEY_ID_LEN;
    bytes[cursor..cursor + 4].copy_from_slice(&live.segment_index.to_be_bytes());
    bytes[cursor + 4..].copy_from_slice(&live.secret);
    Ok(bytes)
}

#[cfg(unix)]
fn persist_live_key(
    directory: TraceDirectory<'_>,
    live: &LiveKeyState,
) -> Result<(), ClaimTraceError> {
    let mut bytes = encode_live_key(live)?;
    let result = (|| {
        directory.guard.verify_named().map_err(map_custody_error)?;
        let destination = live_key_name()?;
        let (temp_name, mut temporary) = create_live_key_temp(directory.guard)?;
        let cleanup = match directory.guard.open_file(
            &temp_name,
            OpenAccess::ReadOnly,
            FilePolicy::private(LIVE_KEY_LEN as u64),
        ) {
            Ok(cleanup) => cleanup,
            Err(error) => {
                let _ = directory.guard.unlink_file(temporary);
                return Err(map_custody_error(error));
            }
        };
        let publication = (|| {
            temporary
                .write_all(&bytes)
                .map_err(|_| ClaimTraceError::Unavailable)?;
            temporary.sync_all().map_err(map_custody_error)?;
            if temporary.metadata().map_err(map_custody_error)?.len != LIVE_KEY_LEN as u64 {
                return Err(ClaimTraceError::Unavailable);
            }
            temporary.verify_named().map_err(map_custody_error)?;
            let existing = directory
                .guard
                .open_file_optional(
                    &destination,
                    OpenAccess::ReadOnly,
                    FilePolicy::private_exact(LIVE_KEY_LEN as u64),
                )
                .map_err(map_custody_error)?;
            if let Some(existing) = existing.as_ref() {
                existing.verify_named().map_err(map_custody_error)?;
            }
            let published = match existing {
                Some(_) => directory
                    .guard
                    .rename_replace(temporary, directory.guard, &destination),
                None => {
                    directory
                        .guard
                        .publish_no_replace(temporary, directory.guard, &destination)
                }
            }
            .map_err(map_custody_error)?;
            if published.metadata().map_err(map_custody_error)?.len != LIVE_KEY_LEN as u64 {
                return Err(ClaimTraceError::Unavailable);
            }
            published.verify_named().map_err(map_custody_error)?;
            let reopened = directory
                .guard
                .open_file(
                    &destination,
                    OpenAccess::ReadOnly,
                    FilePolicy::private_exact(LIVE_KEY_LEN as u64),
                )
                .map_err(map_custody_error)?;
            if reopened.identity() != published.identity() {
                return Err(ClaimTraceError::Unavailable);
            }
            reopened.verify_named().map_err(map_custody_error)
        })();
        if publication.is_err() {
            let _ = directory.guard.unlink_file(cleanup);
        }
        publication
    })();
    bytes.zeroize();
    result
}

#[cfg(unix)]
fn create_live_key_temp(
    directory: &GuardedDir,
) -> Result<(LeafName, GuardedFile), ClaimTraceError> {
    for _ in 0..16 {
        let name = LeafName::new(format!(
            ".live-key.{}.{}.tmp",
            std::process::id(),
            TEMP_COUNTER.fetch_add(1, Ordering::Relaxed)
        ))
        .map_err(map_custody_error)?;
        match directory.create_file(&name, FilePolicy::private(LIVE_KEY_LEN as u64)) {
            Ok(file) => return Ok((name, file)),
            Err(CustodyError::AlreadyExists) => continue,
            Err(error) => return Err(map_custody_error(error)),
        }
    }
    Err(ClaimTraceError::Unavailable)
}

#[cfg(unix)]
fn map_custody_error(_error: CustodyError) -> ClaimTraceError {
    ClaimTraceError::Unavailable
}

#[cfg(not(unix))]
fn persist_live_key(
    directory: TraceDirectory<'_>,
    live: &LiveKeyState,
) -> Result<(), ClaimTraceError> {
    let mut bytes = encode_live_key(live)?;
    let path = directory.path.join(LIVE_KEY_NAME);
    let parent = path.parent().ok_or(ClaimTraceError::Unavailable)?;
    let temp = parent.join(format!(
        ".live-key.{}.{}.tmp",
        std::process::id(),
        TEMP_COUNTER.fetch_add(1, Ordering::Relaxed)
    ));
    let mut options = OpenOptions::new();
    options.write(true).create_new(true);
    let mut file = options
        .open(&temp)
        .map_err(|_| ClaimTraceError::Unavailable)?;
    let result = (|| {
        file.write_all(&bytes)?;
        file.sync_all()?;
        match open_live_key(&path) {
            Ok(Some(_)) | Ok(None) => {}
            Err(_) => return Err(std::io::Error::other("unsafe live-key path")),
        }
        fs::rename(&temp, &path)?;
        if !named_live_key_matches(&file, &path) {
            return Err(std::io::Error::other("live key changed during publication"));
        }
        File::open(parent)?.sync_all()
    })();
    bytes.zeroize();
    if result.is_err() {
        let _ = fs::remove_file(&temp);
        return Err(ClaimTraceError::Unavailable);
    }
    Ok(())
}

#[cfg(not(unix))]
fn sync_parent(path: &Path) -> Result<(), ClaimTraceError> {
    let _ = path;
    Ok(())
}

fn hex(bytes: &[u8]) -> String {
    bytes.iter().map(|byte| format!("{byte:02x}")).collect()
}

fn now_ms() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|duration| u64::try_from(duration.as_millis()).unwrap_or(u64::MAX))
        .unwrap_or(0)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ensure_private_test_directory(path: &Path) {
        fs::create_dir_all(path).unwrap();
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            fs::set_permissions(path, fs::Permissions::from_mode(0o700)).unwrap();
        }
    }

    fn create_private_test_file(path: &Path, len: u64) {
        let mut options = OpenOptions::new();
        options.write(true).create_new(true);
        #[cfg(unix)]
        {
            use std::os::unix::fs::OpenOptionsExt;
            options.mode(0o600);
        }
        options.open(path).unwrap().set_len(len).unwrap();
    }

    #[derive(Debug)]
    struct FixedResolver {
        network: ResolvedClaimTraceKey,
        identity: Option<ResolvedClaimTraceKey>,
    }

    impl ClaimTraceKeyResolver for FixedResolver {
        fn readiness(&self, _now_ms: u64) -> Result<(), ClaimTraceError> {
            Ok(())
        }

        fn resolve_trace_key(
            &self,
            purpose: CompliancePurpose,
            jurisdiction: Jurisdiction,
            at_ms: u64,
        ) -> Result<Option<ResolvedClaimTraceKey>, ClaimTraceError> {
            let key = match purpose {
                CompliancePurpose::NetworkTrace => Some(self.network),
                CompliancePurpose::IdentityTrace => self.identity,
                CompliancePurpose::Attribution => None,
            };
            Ok(key.filter(|key| {
                key.key_id.jurisdiction == jurisdiction
                    && key.not_before_ms <= at_ms
                    && at_ms < key.not_after_ms
            }))
        }
    }

    fn fixture(
        root: &tempfile::TempDir,
        identity_key: bool,
    ) -> (SealedClaimTraceConfig, Arc<dyn ClaimTraceKeyResolver>, u64) {
        let timestamp = now_ms();
        let epoch = day_start_ms(timestamp);
        let network = ResolvedClaimTraceKey {
            key_id: ComplianceKeyId::new(
                CompliancePurpose::NetworkTrace,
                Jurisdiction::Test,
                [3; 32],
                epoch,
                1,
            ),
            public_key: [8; 32],
            not_before_ms: epoch,
            not_after_ms: epoch + TRACE_EPOCH_DURATION_MS,
        };
        let identity = identity_key.then_some(ResolvedClaimTraceKey {
            key_id: ComplianceKeyId::new(
                CompliancePurpose::IdentityTrace,
                Jurisdiction::Test,
                [4; 32],
                epoch,
                1,
            ),
            public_key: [9; 32],
            not_before_ms: epoch,
            not_after_ms: epoch + TRACE_EPOCH_DURATION_MS,
        });
        (
            SealedClaimTraceConfig {
                network_directory: root.path().join("network"),
                identity_directory: root.path().join("identity"),
                network_max_storage_bytes: DEFAULT_TRACE_STORAGE_BYTES,
                identity_max_storage_bytes: DEFAULT_TRACE_STORAGE_BYTES,
                jurisdiction: Jurisdiction::Test,
                node_id: [5; 32],
                capture_policy: ClaimCapturePolicy::Standing,
                retention_days: None,
                max_records_per_segment: 10,
                planned_records_per_minute: 1,
                capacity_utc_epochs: 1,
            },
            Arc::new(FixedResolver { network, identity }),
            timestamp,
        )
    }

    #[test]
    fn persistent_trace_platform_matches_the_linux_macos_release_matrix() {
        assert_eq!(
            require_supported_persistent_trace_platform(PersistentTracePlatform::current()).is_ok(),
            cfg!(any(target_os = "linux", target_os = "macos"))
        );
    }

    #[test]
    fn injected_unsupported_platform_precedes_config_clock_and_directory_effects() {
        let root = tempfile::tempdir().unwrap();
        let (mut config, resolver, _) = fixture(&root, true);
        let requested_parent = root.path().join("unsupported-claim-trace");
        config.network_directory = requested_parent.join("network");
        config.identity_directory = requested_parent.join("identity");
        config.node_id = [0; 32];
        config.planned_records_per_minute = 0;
        CONFIG_VALIDATION_CALLS.with(|calls| calls.set(0));

        assert!(matches!(
            SealedClaimTraceSink::new_with_options_for_platform(
                PersistentTracePlatform::unsupported_for_test(),
                config,
                resolver,
                [0; 32],
                [0; 32],
                WorkerOptions {
                    queue_capacity: 0,
                    batch_max: 0,
                    rollover_clock_poll: Duration::ZERO,
                    clock: WorkerClock::Panic,
                    ..WorkerOptions::default()
                },
            ),
            Err(ClaimTraceError::Unavailable)
        ));
        assert_eq!(CONFIG_VALIDATION_CALLS.with(|calls| calls.get()), 0);
        assert!(!requested_parent.exists());
    }

    fn constructor_accepts(configure: impl FnOnce(&mut SealedClaimTraceConfig)) -> bool {
        let root = tempfile::tempdir().unwrap();
        let (mut config, resolver, _) = fixture(&root, true);
        configure(&mut config);
        SealedClaimTraceSink::new(config, resolver, [6; 32], [7; 32]).is_ok()
    }

    #[cfg_attr(
        not(any(target_os = "linux", target_os = "macos")),
        ignore = "persistent regulatory trace is supported only on Linux and macOS"
    )]
    #[test]
    fn standing_retention_runway_is_enforced_at_the_sink_boundary() {
        assert!(!constructor_accepts(|config| {
            config.jurisdiction = Jurisdiction::Us;
            config.retention_days = None;
            config.capacity_utc_epochs = 31;
        }));
        assert!(!constructor_accepts(|config| {
            config.jurisdiction = Jurisdiction::Us;
            config.retention_days = Some(30);
            config.capacity_utc_epochs = 30;
        }));
        assert!(constructor_accepts(|config| {
            config.jurisdiction = Jurisdiction::Us;
            config.retention_days = Some(30);
            config.capacity_utc_epochs = 31;
        }));
        assert!(constructor_accepts(|config| {
            config.jurisdiction = Jurisdiction::Us;
            config.retention_days = Some(30);
            config.capacity_utc_epochs = 32;
        }));

        assert!(!constructor_accepts(|config| {
            config.jurisdiction = Jurisdiction::Tr;
            config.retention_days = Some(364);
            config.capacity_utc_epochs = 365;
        }));
        assert!(!constructor_accepts(|config| {
            config.jurisdiction = Jurisdiction::Tr;
            config.retention_days = Some(365);
            config.capacity_utc_epochs = 365;
        }));
        assert!(constructor_accepts(|config| {
            config.jurisdiction = Jurisdiction::Tr;
            config.retention_days = Some(365);
            config.capacity_utc_epochs = 366;
        }));
        assert!(constructor_accepts(|config| {
            config.jurisdiction = Jurisdiction::Tr;
            config.retention_days = Some(730);
            config.capacity_utc_epochs = 731;
        }));
        assert!(!constructor_accepts(|config| {
            config.jurisdiction = Jurisdiction::Tr;
            config.retention_days = Some(731);
            config.capacity_utc_epochs = 732;
        }));
        assert!(!constructor_accepts(|config| {
            config.retention_days = Some(1);
        }));
    }

    #[cfg_attr(
        not(any(target_os = "linux", target_os = "macos")),
        ignore = "persistent regulatory trace is supported only on Linux and macOS"
    )]
    #[test]
    fn preservation_runway_is_enforced_at_the_sink_boundary() {
        let preservation = ClaimCapturePolicy::Preservation {
            starts_at_ms: TRACE_EPOCH_DURATION_MS - 1,
            expires_at_ms: TRACE_EPOCH_DURATION_MS + 1,
        };
        assert!(!constructor_accepts(|config| {
            config.jurisdiction = Jurisdiction::Eu;
            config.capture_policy = preservation;
            config.capacity_utc_epochs = 1;
        }));
        assert!(constructor_accepts(|config| {
            config.jurisdiction = Jurisdiction::Eu;
            config.capture_policy = preservation;
            config.capacity_utc_epochs = 2;
        }));
        assert!(constructor_accepts(|config| {
            config.jurisdiction = Jurisdiction::Eu;
            config.capture_policy = preservation;
            config.capacity_utc_epochs = 3;
        }));
        assert!(!constructor_accepts(|config| {
            config.jurisdiction = Jurisdiction::Eu;
            config.capture_policy = preservation;
            config.retention_days = Some(1);
            config.capacity_utc_epochs = 2;
        }));
    }

    /// Preserve the tiny effective headroom used by storage-boundary tests while satisfying the
    /// same conservative advertised-rate sizing contract required in production. Padding is an
    /// ordinary accounted file, so the writer sees exactly `effective_*` bytes of logical budget.
    fn provision_effective_capacity(
        config: &mut SealedClaimTraceConfig,
        effective_network_bytes: u64,
        effective_identity_bytes: u64,
    ) {
        let network_required = required_network_trace_storage_bytes(
            config.planned_records_per_minute,
            config.capacity_utc_epochs,
            config.max_records_per_segment,
        )
        .unwrap();
        let identity_required = required_identity_trace_storage_bytes(
            config.planned_records_per_minute,
            config.capacity_utc_epochs,
            config.max_records_per_segment,
        )
        .unwrap();
        assert!(network_required >= effective_network_bytes);
        assert!(identity_required >= effective_identity_bytes);
        ensure_private_test_directory(&config.network_directory);
        ensure_private_test_directory(&config.identity_directory);
        create_private_test_file(
            &config.network_directory.join("capacity-test-padding"),
            network_required - effective_network_bytes,
        );
        create_private_test_file(
            &config.identity_directory.join("capacity-test-padding"),
            identity_required - effective_identity_bytes,
        );
        config.network_max_storage_bytes = network_required;
        config.identity_max_storage_bytes = identity_required;
    }

    fn segment_in(directory: &Path) -> PathBuf {
        fs::read_dir(directory)
            .unwrap()
            .map(|entry| entry.unwrap().path())
            .find(|path| path.extension().is_some_and(|value| value == "pptrace"))
            .unwrap()
    }

    fn claim(timestamp_ms: u64, event: u16) -> ClaimTraceInput {
        ClaimTraceInput {
            timestamp_ms,
            source: SocketAddr::from(([192, 0, 2, 10], 4_000 + event)),
            provider: IdentityProvider::Oauth2,
            provider_subject: format!("github:{event}"),
        }
    }

    fn wait(receipt: ClaimTraceReceipt) -> Result<(), ClaimTraceError> {
        receipt
            .acknowledged
            .blocking_recv()
            .unwrap_or(Err(ClaimTraceError::Unavailable))
    }

    fn directory_file_bytes(directory: &Path) -> u64 {
        fs::read_dir(directory)
            .unwrap()
            .map(|entry| entry.unwrap().metadata().unwrap().len())
            .sum()
    }

    fn open_segment_in(directory: &Path) -> PathBuf {
        fs::read_dir(directory)
            .unwrap()
            .map(|entry| entry.unwrap().path())
            .find(|path| path.to_string_lossy().ends_with(".pptrace.open"))
            .unwrap()
    }

    #[cfg(unix)]
    fn directory_snapshot(directory: &Path) -> Vec<(String, Vec<u8>)> {
        let mut files = fs::read_dir(directory)
            .unwrap()
            .map(|entry| {
                let entry = entry.unwrap();
                (
                    entry.file_name().into_string().unwrap(),
                    fs::read(entry.path()).unwrap(),
                )
            })
            .collect::<Vec<_>>();
        files.sort_by(|left, right| left.0.cmp(&right.0));
        files
    }

    #[cfg_attr(
        not(any(target_os = "linux", target_os = "macos")),
        ignore = "persistent regulatory trace is supported only on Linux and macOS"
    )]
    #[test]
    fn one_claim_writes_two_purpose_separated_segments() {
        let root = tempfile::tempdir().unwrap();
        let (config, resolver, timestamp) = fixture(&root, true);
        let sink = SealedClaimTraceSink::new(config.clone(), resolver, [6; 32], [7; 32]).unwrap();
        sink.capture(ClaimTraceInput {
            timestamp_ms: timestamp,
            source: "192.0.2.10:4567".parse().unwrap(),
            provider: IdentityProvider::Oauth2,
            provider_subject: "github:123456".into(),
        })
        .unwrap();
        sink.shutdown(timestamp).unwrap();

        let network = verify_segment(segment_in(&config.network_directory)).unwrap();
        let identity = verify_segment(segment_in(&config.identity_directory)).unwrap();
        assert_eq!(
            network.header.key_id().purpose,
            CompliancePurpose::NetworkTrace
        );
        assert_eq!(
            identity.header.key_id().purpose,
            CompliancePurpose::IdentityTrace
        );
        assert_ne!(
            network.header.wrapped_epoch_key().epoch_key_commitment(),
            identity.header.wrapped_epoch_key().epoch_key_commitment()
        );
        assert_eq!(network.footer.record_count(), 1);
        assert_eq!(identity.footer.record_count(), 1);
    }

    #[cfg_attr(
        not(any(target_os = "linux", target_os = "macos")),
        ignore = "persistent regulatory trace is supported only on Linux and macOS"
    )]
    #[test]
    fn sealed_capacity_contract_validates_each_purpose_budget_independently() {
        let root = tempfile::tempdir().unwrap();
        let (mut config, resolver, timestamp) = fixture(&root, true);
        config.planned_records_per_minute = 7;
        config.capacity_utc_epochs = 3;
        let network_required = required_network_trace_storage_bytes(
            config.planned_records_per_minute,
            config.capacity_utc_epochs,
            config.max_records_per_segment,
        )
        .unwrap();
        let identity_required = required_identity_trace_storage_bytes(
            config.planned_records_per_minute,
            config.capacity_utc_epochs,
            config.max_records_per_segment,
        )
        .unwrap();

        config.network_max_storage_bytes = network_required - 1;
        config.identity_max_storage_bytes = identity_required;
        assert_eq!(
            SealedClaimTraceSink::new(config.clone(), Arc::clone(&resolver), [6; 32], [7; 32],)
                .unwrap_err(),
            ClaimTraceError::Capacity
        );

        config.network_max_storage_bytes = network_required;
        config.identity_max_storage_bytes = identity_required - 1;
        assert_eq!(
            SealedClaimTraceSink::new(config.clone(), Arc::clone(&resolver), [6; 32], [7; 32],)
                .unwrap_err(),
            ClaimTraceError::Capacity
        );

        config.identity_max_storage_bytes = identity_required;
        let sink = SealedClaimTraceSink::new(config, resolver, [6; 32], [7; 32]).unwrap();
        assert_eq!(
            sink.capacity_contract(),
            Some(ClaimTraceCapacity {
                policy: TraceRetentionPolicy {
                    jurisdiction: Jurisdiction::Test,
                    capture: ClaimCapturePolicy::Standing,
                    retention_days: None,
                },
                records_per_minute: 7,
                utc_epochs: 3,
                max_records_per_segment: 10,
                network_logical_limit_bytes: network_required,
                identity_logical_limit_bytes: identity_required,
            })
        );
        sink.shutdown(timestamp).unwrap();
    }

    #[cfg_attr(
        not(any(target_os = "linux", target_os = "macos")),
        ignore = "persistent regulatory trace is supported only on Linux and macOS"
    )]
    #[test]
    fn exhausted_pair_rejects_the_whole_batch_without_poison_and_reserve_closes_it() {
        let root = tempfile::tempdir().unwrap();
        let (mut config, resolver, timestamp) = fixture(&root, true);
        provision_effective_capacity(
            &mut config,
            MIN_TRACE_STORAGE_BYTES,
            MIN_TRACE_STORAGE_BYTES,
        );
        let sink = SealedClaimTraceSink::new_with_options(
            config.clone(),
            Arc::clone(&resolver),
            [6; 32],
            [7; 32],
            WorkerOptions {
                batch_max: 2,
                batch_window: Duration::from_millis(100),
                enforce_wall_clock: false,
                ..WorkerOptions::default()
            },
        )
        .unwrap();

        assert!(sink.readiness(timestamp).is_ok());
        let first = sink.submit(claim(timestamp, 1)).unwrap();
        let second = sink.submit(claim(timestamp + 1, 2)).unwrap();
        assert_eq!(wait(first), Err(ClaimTraceError::Capacity));
        assert_eq!(wait(second), Err(ClaimTraceError::Capacity));
        assert_eq!(sink.status.durable_sync_batches.load(Ordering::Acquire), 1);
        assert!(sink.status.is_running());
        assert_eq!(
            sink.readiness(timestamp + 2),
            Err(ClaimTraceError::Capacity)
        );

        let network_key = load_live_key_at(&config.network_directory.join(LIVE_KEY_NAME))
            .unwrap()
            .unwrap()
            .key_id;
        let identity_key = load_live_key_at(&config.identity_directory.join(LIVE_KEY_NAME))
            .unwrap()
            .unwrap()
            .key_id;
        assert_eq!(
            sink.inner.identity_storage_budget.used() + TRACE_TERMINAL_RESERVE_BYTES,
            config.identity_max_storage_bytes
        );
        assert_eq!(
            sink.inner.network_storage_budget.used(),
            directory_file_bytes(&config.network_directory)
        );
        assert_eq!(
            sink.inner.identity_storage_budget.used(),
            directory_file_bytes(&config.identity_directory)
        );

        sink.crash_for_test();
        drop(sink);
        let sink = SealedClaimTraceSink::new_with_options(
            config.clone(),
            resolver,
            [6; 32],
            [7; 32],
            WorkerOptions {
                enforce_wall_clock: false,
                ..WorkerOptions::default()
            },
        )
        .unwrap();
        assert!(sink.status.is_running());
        assert_eq!(
            sink.readiness(timestamp + 2),
            Err(ClaimTraceError::Capacity)
        );

        let epoch_end = day_start_ms(timestamp) + TRACE_EPOCH_DURATION_MS;
        sink.shutdown(epoch_end).unwrap();
        assert!(!config.network_directory.join(LIVE_KEY_NAME).exists());
        assert!(!config.identity_directory.join(LIVE_KEY_NAME).exists());
        assert!(epoch_manifest_path(&config.network_directory, &network_key)
            .unwrap()
            .exists());
        assert!(
            epoch_manifest_path(&config.identity_directory, &identity_key)
                .unwrap()
                .exists()
        );
        assert_eq!(
            sink.inner.network_storage_budget.used(),
            directory_file_bytes(&config.network_directory)
        );
        assert_eq!(
            sink.inner.identity_storage_budget.used(),
            directory_file_bytes(&config.identity_directory)
        );
        assert!(sink.inner.network_storage_budget.used() <= config.network_max_storage_bytes);
        assert!(sink.inner.identity_storage_budget.used() <= config.identity_max_storage_bytes);
        assert_eq!(
            verify_segment(segment_in(&config.network_directory))
                .unwrap()
                .footer
                .record_count(),
            1
        );
        assert_eq!(
            verify_segment(segment_in(&config.identity_directory))
                .unwrap()
                .footer
                .record_count(),
            1
        );
    }

    #[cfg_attr(
        not(any(target_os = "linux", target_os = "macos")),
        ignore = "persistent regulatory trace is supported only on Linux and macOS"
    )]
    #[test]
    fn segment_full_precharge_is_rolled_back_before_the_replacement_frame() {
        let root = tempfile::tempdir().unwrap();
        let (mut config, resolver, timestamp) = fixture(&root, true);
        config.max_records_per_segment = 1;
        let effective_network_bytes = TRACE_TERMINAL_RESERVE_BYTES
            + TRACE_LIVE_KEY_BYTES
            + 2 * SEGMENT_HEADER_LEN as u64
            + 2 * NETWORK_TRACE_FRAME_BYTES
            + SEGMENT_FOOTER_LEN as u64;
        let effective_identity_bytes = TRACE_TERMINAL_RESERVE_BYTES
            + TRACE_LIVE_KEY_BYTES
            + 2 * SEGMENT_HEADER_LEN as u64
            + 2 * IDENTITY_TRACE_FRAME_BYTES
            + SEGMENT_FOOTER_LEN as u64;
        provision_effective_capacity(
            &mut config,
            effective_network_bytes,
            effective_identity_bytes,
        );
        let sink = SealedClaimTraceSink::new_with_options(
            config.clone(),
            resolver,
            [6; 32],
            [7; 32],
            WorkerOptions {
                enforce_wall_clock: false,
                ..WorkerOptions::default()
            },
        )
        .unwrap();
        sink.capture(claim(timestamp, 1)).unwrap();
        assert!(sink.readiness(timestamp + 1).is_ok());
        sink.capture(claim(timestamp + 1, 2)).unwrap();
        assert_eq!(
            sink.readiness(timestamp + 2),
            Err(ClaimTraceError::Capacity)
        );

        assert_eq!(
            sink.inner.network_storage_budget.used(),
            directory_file_bytes(&config.network_directory)
        );
        assert_eq!(
            sink.inner.identity_storage_budget.used(),
            directory_file_bytes(&config.identity_directory)
        );
        sink.shutdown(timestamp + 2).unwrap();
        let closed_segments = |directory: &Path| {
            fs::read_dir(directory)
                .unwrap()
                .map(|entry| entry.unwrap().path())
                .filter(|path| path.extension().is_some_and(|value| value == "pptrace"))
                .count()
        };
        assert_eq!(closed_segments(&config.network_directory), 2);
        assert_eq!(closed_segments(&config.identity_directory), 2);
        assert_eq!(
            sink.inner.network_storage_budget.used(),
            directory_file_bytes(&config.network_directory)
        );
        assert_eq!(
            sink.inner.identity_storage_budget.used(),
            directory_file_bytes(&config.identity_directory)
        );
    }

    #[cfg_attr(
        not(any(target_os = "linux", target_os = "macos")),
        ignore = "persistent regulatory trace is supported only on Linux and macOS"
    )]
    #[test]
    fn recovery_reconciles_truncated_partial_frame_bytes_for_both_purposes() {
        let root = tempfile::tempdir().unwrap();
        let (config, resolver, timestamp) = fixture(&root, true);
        let sink = SealedClaimTraceSink::new_with_options(
            config.clone(),
            Arc::clone(&resolver),
            [6; 32],
            [7; 32],
            WorkerOptions {
                enforce_wall_clock: false,
                ..WorkerOptions::default()
            },
        )
        .unwrap();
        sink.capture(claim(timestamp, 1)).unwrap();
        sink.crash_for_test();
        let network_open = open_segment_in(&config.network_directory);
        let identity_open = open_segment_in(&config.identity_directory);
        drop(sink);
        OpenOptions::new()
            .append(true)
            .open(&network_open)
            .unwrap()
            .write_all(&[0xA5; 7])
            .unwrap();
        OpenOptions::new()
            .append(true)
            .open(&identity_open)
            .unwrap()
            .write_all(&[0x5A; 11])
            .unwrap();
        let inflated_network = directory_file_bytes(&config.network_directory);
        let inflated_identity = directory_file_bytes(&config.identity_directory);

        let restarted = SealedClaimTraceSink::new_with_options(
            config.clone(),
            resolver,
            [6; 32],
            [7; 32],
            WorkerOptions {
                enforce_wall_clock: false,
                ..WorkerOptions::default()
            },
        )
        .unwrap();
        assert_eq!(
            restarted.inner.network_storage_budget.used(),
            directory_file_bytes(&config.network_directory)
        );
        assert_eq!(
            restarted.inner.identity_storage_budget.used(),
            directory_file_bytes(&config.identity_directory)
        );
        assert!(restarted.inner.network_storage_budget.used() < inflated_network);
        assert!(restarted.inner.identity_storage_budget.used() < inflated_identity);
        restarted.shutdown(timestamp + 1).unwrap();
    }

    #[cfg(unix)]
    #[test]
    fn dual_writer_leases_deny_reverse_overlap_and_preserve_acknowledged_records() {
        let root = tempfile::tempdir().unwrap();
        let (config, resolver, timestamp) = fixture(&root, true);
        let sink =
            SealedClaimTraceSink::new(config.clone(), Arc::clone(&resolver), [6; 32], [7; 32])
                .unwrap();
        sink.capture(claim(timestamp, 1)).unwrap();

        let network_before = directory_snapshot(&config.network_directory);
        let identity_before = directory_snapshot(&config.identity_directory);
        let mut reversed = config.clone();
        std::mem::swap(
            &mut reversed.network_directory,
            &mut reversed.identity_directory,
        );
        assert!(
            SealedClaimTraceSink::new(reversed, Arc::clone(&resolver), [6; 32], [7; 32],).is_err()
        );
        assert_eq!(
            directory_snapshot(&config.network_directory),
            network_before
        );
        assert_eq!(
            directory_snapshot(&config.identity_directory),
            identity_before
        );

        sink.capture(claim(timestamp.saturating_add(1), 2)).unwrap();
        sink.crash_for_test();
        drop(sink);

        let restarted =
            SealedClaimTraceSink::new(config.clone(), Arc::clone(&resolver), [6; 32], [7; 32])
                .unwrap();
        restarted.shutdown(timestamp.saturating_add(2)).unwrap();
        assert_eq!(
            verify_segment(segment_in(&config.network_directory))
                .unwrap()
                .footer
                .record_count(),
            2
        );
        assert_eq!(
            verify_segment(segment_in(&config.identity_directory))
                .unwrap()
                .footer
                .record_count(),
            2
        );

        let disjoint_root = tempfile::tempdir().unwrap();
        let (disjoint_config, disjoint_resolver, _) = fixture(&disjoint_root, true);
        let disjoint =
            SealedClaimTraceSink::new(disjoint_config, disjoint_resolver, [6; 32], [7; 32])
                .unwrap();
        drop(disjoint);

        drop(restarted);
        SealedClaimTraceSink::new(config, resolver, [6; 32], [7; 32]).unwrap();
    }

    #[cfg(unix)]
    #[test]
    fn ordered_partial_acquisition_releases_the_free_directory() {
        let root = tempfile::tempdir().unwrap();
        let (mut held_config, resolver, _) = fixture(&root, true);
        held_config.network_directory = root.path().join("z-held");
        held_config.identity_directory = root.path().join("zz-held");
        let held =
            SealedClaimTraceSink::new(held_config.clone(), Arc::clone(&resolver), [6; 32], [7; 32])
                .unwrap();

        let free_directory = root.path().join("a-free");
        let overlapping = SealedClaimTraceConfig {
            network_directory: free_directory.clone(),
            identity_directory: held_config.network_directory,
            ..held_config
        };
        assert!(SealedClaimTraceSink::new(overlapping, resolver, [6; 32], [7; 32],).is_err());

        let released = TraceWriterLease::acquire(&free_directory).unwrap();
        released.assert_stable().unwrap();
        drop(released);
        drop(held);
    }

    #[cfg(unix)]
    #[test]
    fn purpose_directories_reject_same_and_nested_paths_before_creation() {
        let root = tempfile::tempdir().unwrap();
        let (mut same_config, same_resolver, _) = fixture(&root, true);
        let same = root.path().join("same-purpose-store");
        same_config.network_directory = same.clone();
        same_config.identity_directory = same.clone();
        assert!(SealedClaimTraceSink::new(same_config, same_resolver, [6; 32], [7; 32]).is_err());
        assert!(!same.exists());

        let (mut nested_config, nested_resolver, _) = fixture(&root, true);
        let network = root.path().join("nested-purpose-store").join("network");
        nested_config.network_directory = network.clone();
        nested_config.identity_directory = network.join("identity");
        assert!(
            SealedClaimTraceSink::new(nested_config, nested_resolver, [6; 32], [7; 32]).is_err()
        );
        assert!(!root.path().join("nested-purpose-store").exists());
    }

    #[cfg_attr(
        not(any(target_os = "linux", target_os = "macos")),
        ignore = "persistent regulatory trace is supported only on Linux and macOS"
    )]
    #[test]
    fn concurrent_claims_share_group_sync_and_shutdown_drains_both_purposes() {
        const RECORDS: usize = 8;

        let root = tempfile::tempdir().unwrap();
        let (mut config, resolver, timestamp) = fixture(&root, true);
        config.max_records_per_segment = 32;
        let sink = Arc::new(
            SealedClaimTraceSink::new_with_options(
                config.clone(),
                resolver,
                [6; 32],
                [7; 32],
                WorkerOptions {
                    queue_capacity: RECORDS,
                    batch_max: RECORDS,
                    batch_window: Duration::from_millis(50),
                    rollover_clock_poll: CLAIM_TRACE_ROLLOVER_CLOCK_POLL,
                    clock: WorkerClock::System,
                    enforce_wall_clock: true,
                },
            )
            .unwrap(),
        );
        let start = Arc::new(std::sync::Barrier::new(RECORDS + 1));
        let mut captures = Vec::new();
        for index in 0..RECORDS {
            let sink = Arc::clone(&sink);
            let start = Arc::clone(&start);
            captures.push(std::thread::spawn(move || {
                start.wait();
                sink.capture(claim(timestamp + index as u64, index as u16))
            }));
        }
        start.wait();
        for capture in captures {
            capture.join().unwrap().unwrap();
        }
        let batches = sink.status.durable_sync_batches.load(Ordering::Acquire);
        assert!(batches > 0 && batches < RECORDS as u64);
        sink.shutdown(timestamp + RECORDS as u64).unwrap();

        let network = verify_segment(segment_in(&config.network_directory)).unwrap();
        let identity = verify_segment(segment_in(&config.identity_directory)).unwrap();
        assert_eq!(network.footer.record_count(), RECORDS as u32);
        assert_eq!(identity.footer.record_count(), RECORDS as u32);
    }

    #[cfg_attr(
        not(any(target_os = "linux", target_os = "macos")),
        ignore = "persistent regulatory trace is supported only on Linux and macOS"
    )]
    #[test]
    fn bounded_queue_rejects_saturation_and_canceled_receipts_do_not_run_later() {
        struct BlockingResolver {
            inner: Arc<dyn ClaimTraceKeyResolver>,
            entered: Arc<std::sync::atomic::AtomicBool>,
            release: Arc<std::sync::atomic::AtomicBool>,
        }

        impl ClaimTraceKeyResolver for BlockingResolver {
            fn readiness(&self, now_ms: u64) -> Result<(), ClaimTraceError> {
                self.entered.store(true, Ordering::Release);
                while !self.release.load(Ordering::Acquire) {
                    std::thread::yield_now();
                }
                self.inner.readiness(now_ms)
            }

            fn resolve_trace_key(
                &self,
                purpose: CompliancePurpose,
                jurisdiction: Jurisdiction,
                at_ms: u64,
            ) -> Result<Option<ResolvedClaimTraceKey>, ClaimTraceError> {
                self.inner.resolve_trace_key(purpose, jurisdiction, at_ms)
            }
        }

        let root = tempfile::tempdir().unwrap();
        let (config, resolver, timestamp) = fixture(&root, true);
        let entered = Arc::new(std::sync::atomic::AtomicBool::new(false));
        let release = Arc::new(std::sync::atomic::AtomicBool::new(false));
        let sink = SealedClaimTraceSink::new_with_options(
            config.clone(),
            Arc::new(BlockingResolver {
                inner: resolver,
                entered: Arc::clone(&entered),
                release: Arc::clone(&release),
            }),
            [6; 32],
            [7; 32],
            WorkerOptions {
                queue_capacity: 1,
                batch_max: 1,
                batch_window: Duration::ZERO,
                rollover_clock_poll: CLAIM_TRACE_ROLLOVER_CLOCK_POLL,
                clock: WorkerClock::System,
                enforce_wall_clock: true,
            },
        )
        .unwrap();
        let first = sink.submit(claim(timestamp, 1)).unwrap();
        while !entered.load(Ordering::Acquire) {
            std::thread::yield_now();
        }
        let canceled = sink.submit(claim(timestamp + 1, 2)).unwrap();
        assert!(sink.submit(claim(timestamp + 2, 3)).is_err());
        assert_eq!(sink.status.enqueued_captures.load(Ordering::Acquire), 2);
        drop(canceled);
        release.store(true, Ordering::Release);
        wait(first).unwrap();
        sink.shutdown(timestamp + 3).unwrap();
        assert_eq!(
            verify_segment(segment_in(&config.network_directory))
                .unwrap()
                .footer
                .record_count(),
            1
        );
        assert_eq!(
            verify_segment(segment_in(&config.identity_directory))
                .unwrap()
                .footer
                .record_count(),
            1
        );
    }

    #[cfg_attr(
        not(any(target_os = "linux", target_os = "macos")),
        ignore = "persistent regulatory trace is supported only on Linux and macOS"
    )]
    #[test]
    fn second_purpose_sync_failure_never_acknowledges_and_poisons_readiness() {
        let root = tempfile::tempdir().unwrap();
        let (config, resolver, timestamp) = fixture(&root, true);
        let sink = SealedClaimTraceSink::new(config, resolver, [6; 32], [7; 32]).unwrap();
        sink.status
            .fail_identity_sync
            .store(true, Ordering::Release);
        assert!(sink.capture(claim(timestamp, 1)).is_err());
        assert_eq!(sink.status.durable_sync_batches.load(Ordering::Acquire), 0);
        assert!(sink.readiness(timestamp).is_err());
        assert!(sink.capture(claim(timestamp + 1, 2)).is_err());
    }

    #[cfg_attr(
        not(any(target_os = "linux", target_os = "macos")),
        ignore = "persistent regulatory trace is supported only on Linux and macOS"
    )]
    #[test]
    fn worker_panic_disconnects_receipt_and_poisons_readiness() {
        let root = tempfile::tempdir().unwrap();
        let (config, resolver, timestamp) = fixture(&root, true);
        let sink = SealedClaimTraceSink::new(config, resolver, [6; 32], [7; 32]).unwrap();
        sink.status.panic_before_sync.store(true, Ordering::Release);
        assert!(sink.capture(claim(timestamp, 1)).is_err());
        assert_eq!(sink.status.durable_sync_batches.load(Ordering::Acquire), 0);
        assert!(sink.readiness(timestamp).is_err());
    }

    #[cfg_attr(
        not(any(target_os = "linux", target_os = "macos")),
        ignore = "persistent regulatory trace is supported only on Linux and macOS"
    )]
    #[test]
    fn epoch_rollover_publishes_separate_terminal_manifests() {
        #[derive(Debug)]
        struct DailyResolver {
            keys: [ResolvedClaimTraceKey; 4],
        }

        impl ClaimTraceKeyResolver for DailyResolver {
            fn readiness(&self, _now_ms: u64) -> Result<(), ClaimTraceError> {
                Ok(())
            }

            fn resolve_trace_key(
                &self,
                purpose: CompliancePurpose,
                jurisdiction: Jurisdiction,
                at_ms: u64,
            ) -> Result<Option<ResolvedClaimTraceKey>, ClaimTraceError> {
                Ok(self.keys.iter().copied().find(|key| {
                    key.key_id.purpose == purpose
                        && key.key_id.jurisdiction == jurisdiction
                        && key.not_before_ms <= at_ms
                        && at_ms < key.not_after_ms
                }))
            }
        }

        const FIRST_EPOCH: u64 = 19_000 * TRACE_EPOCH_DURATION_MS;
        let second_epoch = FIRST_EPOCH + TRACE_EPOCH_DURATION_MS;
        let resolved = |purpose, authority, epoch, public_key| {
            let key_id =
                ComplianceKeyId::new(purpose, Jurisdiction::Test, [authority; 32], epoch, 1);
            ResolvedClaimTraceKey {
                key_id,
                public_key: [public_key; 32],
                not_before_ms: epoch,
                not_after_ms: epoch + TRACE_EPOCH_DURATION_MS,
            }
        };
        let first_network = resolved(CompliancePurpose::NetworkTrace, 31, FIRST_EPOCH, 8);
        let first_identity = resolved(CompliancePurpose::IdentityTrace, 32, FIRST_EPOCH, 9);
        let second_network = resolved(CompliancePurpose::NetworkTrace, 33, second_epoch, 10);
        let second_identity = resolved(CompliancePurpose::IdentityTrace, 34, second_epoch, 11);
        let root = tempfile::tempdir().unwrap();
        let manifest_bytes = EPOCH_MANIFEST_FIXED_LEN as u64 + EPOCH_MANIFEST_ENTRY_LEN as u64;
        let mut config = SealedClaimTraceConfig {
            network_directory: root.path().join("network"),
            identity_directory: root.path().join("identity"),
            network_max_storage_bytes: TRACE_TERMINAL_RESERVE_BYTES
                + TRACE_LIVE_KEY_BYTES
                + 2 * SEGMENT_HEADER_LEN as u64
                + 2 * NETWORK_TRACE_FRAME_BYTES
                + SEGMENT_FOOTER_LEN as u64
                + manifest_bytes,
            identity_max_storage_bytes: TRACE_TERMINAL_RESERVE_BYTES
                + TRACE_LIVE_KEY_BYTES
                + 2 * SEGMENT_HEADER_LEN as u64
                + 2 * IDENTITY_TRACE_FRAME_BYTES
                + SEGMENT_FOOTER_LEN as u64
                + manifest_bytes,
            jurisdiction: Jurisdiction::Test,
            node_id: [5; 32],
            capture_policy: ClaimCapturePolicy::Standing,
            retention_days: None,
            max_records_per_segment: 10,
            planned_records_per_minute: 1,
            capacity_utc_epochs: 2,
        };
        let effective_network_bytes = config.network_max_storage_bytes;
        let effective_identity_bytes = config.identity_max_storage_bytes;
        provision_effective_capacity(
            &mut config,
            effective_network_bytes,
            effective_identity_bytes,
        );
        let sink = SealedClaimTraceSink::new_with_options(
            config.clone(),
            Arc::new(DailyResolver {
                keys: [
                    first_network,
                    first_identity,
                    second_network,
                    second_identity,
                ],
            }),
            [6; 32],
            [7; 32],
            WorkerOptions {
                enforce_wall_clock: false,
                ..WorkerOptions::default()
            },
        )
        .unwrap();
        sink.capture(claim(FIRST_EPOCH + 100, 1)).unwrap();
        assert!(sink.readiness(second_epoch + 100).is_ok());
        sink.capture(claim(second_epoch + 100, 2)).unwrap();
        assert_eq!(
            sink.readiness(second_epoch + 101),
            Err(ClaimTraceError::Capacity)
        );
        sink.shutdown(second_epoch + 200).unwrap();

        let network_segment = verify_segment(
            segment_path(&config.network_directory, &first_network.key_id, 0).unwrap(),
        )
        .unwrap();
        let identity_segment = verify_segment(
            segment_path(&config.identity_directory, &first_identity.key_id, 0).unwrap(),
        )
        .unwrap();
        let network_manifest = read_epoch_manifest_for_signer(
            epoch_manifest_path(&config.network_directory, &first_network.key_id).unwrap(),
            SigningKey::from_bytes(&[6; 32]).verifying_key().to_bytes(),
        )
        .unwrap();
        let identity_manifest = read_epoch_manifest_for_signer(
            epoch_manifest_path(&config.identity_directory, &first_identity.key_id).unwrap(),
            SigningKey::from_bytes(&[7; 32]).verifying_key().to_bytes(),
        )
        .unwrap();
        assert_eq!(network_manifest.total_records(), 1);
        assert_eq!(identity_manifest.total_records(), 1);
        assert_ne!(
            network_manifest.epoch_key_commitment(),
            identity_manifest.epoch_key_commitment()
        );
        let mut network_complete = network_manifest.verifier();
        network_complete.verify_next(&network_segment).unwrap();
        network_complete.finish().unwrap();
        let mut identity_complete = identity_manifest.verifier();
        identity_complete.verify_next(&identity_segment).unwrap();
        identity_complete.finish().unwrap();
        assert!(
            !epoch_manifest_path(&config.network_directory, &second_network.key_id)
                .unwrap()
                .exists()
        );
        assert!(
            !epoch_manifest_path(&config.identity_directory, &second_identity.key_id)
                .unwrap()
                .exists()
        );
    }

    #[cfg_attr(
        not(any(target_os = "linux", target_os = "macos")),
        ignore = "persistent regulatory trace is supported only on Linux and macOS"
    )]
    #[test]
    fn idle_utc_boundary_closes_both_purpose_stores() {
        let root = tempfile::tempdir().unwrap();
        let (config, resolver, timestamp) = fixture(&root, true);
        let manual_now = Box::leak(Box::new(AtomicU64::new(timestamp)));
        let sink = SealedClaimTraceSink::new_with_options(
            config.clone(),
            resolver,
            [6; 32],
            [7; 32],
            WorkerOptions {
                rollover_clock_poll: Duration::from_millis(1),
                clock: WorkerClock::Test(manual_now),
                ..WorkerOptions::default()
            },
        )
        .unwrap();

        sink.submit_at_for_test(claim(timestamp, 91), timestamp)
            .unwrap()
            .blocking_wait()
            .unwrap();
        let network_live_path = config.network_directory.join(LIVE_KEY_NAME);
        let identity_live_path = config.identity_directory.join(LIVE_KEY_NAME);
        let network_live = load_live_key_at(&network_live_path).unwrap().unwrap();
        let identity_live = load_live_key_at(&identity_live_path).unwrap().unwrap();
        let network_manifest_path =
            epoch_manifest_path(&config.network_directory, &network_live.key_id).unwrap();
        let identity_manifest_path =
            epoch_manifest_path(&config.identity_directory, &identity_live.key_id).unwrap();
        assert!(!network_manifest_path.exists());
        assert!(!identity_manifest_path.exists());

        let epoch_end = epoch_end_ms(&network_live.key_id).unwrap();
        assert_eq!(epoch_end, epoch_end_ms(&identity_live.key_id).unwrap());
        manual_now.store(epoch_end, Ordering::Release);
        let wait_deadline = Instant::now() + Duration::from_secs(2);
        while (!network_manifest_path.exists()
            || !identity_manifest_path.exists()
            || network_live_path.exists()
            || identity_live_path.exists())
            && Instant::now() < wait_deadline
        {
            std::thread::sleep(Duration::from_millis(1));
        }

        assert!(network_manifest_path.exists());
        assert!(identity_manifest_path.exists());
        assert!(!network_live_path.exists());
        assert!(!identity_live_path.exists());
        let network_manifest = read_epoch_manifest_for_signer(
            network_manifest_path,
            SigningKey::from_bytes(&[6; 32]).verifying_key().to_bytes(),
        )
        .unwrap();
        let identity_manifest = read_epoch_manifest_for_signer(
            identity_manifest_path,
            SigningKey::from_bytes(&[7; 32]).verifying_key().to_bytes(),
        )
        .unwrap();
        assert_eq!(network_manifest.total_records(), 1);
        assert_eq!(identity_manifest.total_records(), 1);
        assert!(sink.status.is_running());
        sink.shutdown(epoch_end).unwrap();
    }

    #[cfg_attr(
        not(any(target_os = "linux", target_os = "macos")),
        ignore = "persistent regulatory trace is supported only on Linux and macOS"
    )]
    #[test]
    fn admission_time_orders_reverse_midnight_callers_and_backward_clock_does_not_poison() {
        #[derive(Debug)]
        struct DailyResolver {
            keys: [ResolvedClaimTraceKey; 4],
        }

        impl ClaimTraceKeyResolver for DailyResolver {
            fn readiness(&self, _now_ms: u64) -> Result<(), ClaimTraceError> {
                Ok(())
            }

            fn resolve_trace_key(
                &self,
                purpose: CompliancePurpose,
                jurisdiction: Jurisdiction,
                at_ms: u64,
            ) -> Result<Option<ResolvedClaimTraceKey>, ClaimTraceError> {
                Ok(self.keys.iter().copied().find(|key| {
                    key.key_id.purpose == purpose
                        && key.key_id.jurisdiction == jurisdiction
                        && key.not_before_ms <= at_ms
                        && at_ms < key.not_after_ms
                }))
            }
        }

        const FIRST_EPOCH: u64 = 19_000 * TRACE_EPOCH_DURATION_MS;
        let second_epoch = FIRST_EPOCH + TRACE_EPOCH_DURATION_MS;
        let resolved = |purpose, authority, epoch, public_key| ResolvedClaimTraceKey {
            key_id: ComplianceKeyId::new(purpose, Jurisdiction::Test, [authority; 32], epoch, 1),
            public_key: [public_key; 32],
            not_before_ms: epoch,
            not_after_ms: epoch + TRACE_EPOCH_DURATION_MS,
        };
        let first_network = resolved(CompliancePurpose::NetworkTrace, 61, FIRST_EPOCH, 8);
        let first_identity = resolved(CompliancePurpose::IdentityTrace, 62, FIRST_EPOCH, 9);
        let second_network = resolved(CompliancePurpose::NetworkTrace, 63, second_epoch, 10);
        let second_identity = resolved(CompliancePurpose::IdentityTrace, 64, second_epoch, 11);
        let root = tempfile::tempdir().unwrap();
        let config = SealedClaimTraceConfig {
            network_directory: root.path().join("network"),
            identity_directory: root.path().join("identity"),
            network_max_storage_bytes: DEFAULT_TRACE_STORAGE_BYTES,
            identity_max_storage_bytes: DEFAULT_TRACE_STORAGE_BYTES,
            jurisdiction: Jurisdiction::Test,
            node_id: [5; 32],
            capture_policy: ClaimCapturePolicy::Standing,
            retention_days: None,
            max_records_per_segment: 10,
            planned_records_per_minute: 1,
            capacity_utc_epochs: 2,
        };
        let sink = SealedClaimTraceSink::new_with_options(
            config.clone(),
            Arc::new(DailyResolver {
                keys: [
                    first_network,
                    first_identity,
                    second_network,
                    second_identity,
                ],
            }),
            [6; 32],
            [7; 32],
            WorkerOptions {
                enforce_wall_clock: true,
                ..WorkerOptions::default()
            },
        )
        .unwrap();

        // Caller timestamps are intentionally inverted. The truthful admission samples are not:
        // the first request enters immediately before midnight, the second immediately after it.
        wait(
            sink.submit_at_for_test(claim(second_epoch + 500, 1), second_epoch - 1)
                .unwrap(),
        )
        .unwrap();
        let enqueued = sink.status.enqueued_captures.load(Ordering::Acquire);
        assert!(sink
            .submit_at_for_test(claim(FIRST_EPOCH + 500, 2), second_epoch - 2)
            .is_err());
        assert_eq!(
            sink.status.enqueued_captures.load(Ordering::Acquire),
            enqueued
        );
        assert!(sink.status.is_running());
        assert!(sink.readiness(second_epoch - 1).is_ok());

        wait(
            sink.submit_at_for_test(claim(FIRST_EPOCH + 501, 3), second_epoch)
                .unwrap(),
        )
        .unwrap();
        assert!(sink.status.is_running());
        assert!(sink.readiness(second_epoch).is_ok());
        sink.shutdown(second_epoch + 1).unwrap();

        let first = verify_segment(
            segment_path(&config.network_directory, &first_network.key_id, 0).unwrap(),
        )
        .unwrap();
        let second = verify_segment(
            segment_path(&config.network_directory, &second_network.key_id, 0).unwrap(),
        )
        .unwrap();
        assert_eq!(first.header.opened_at_ms(), second_epoch - 1);
        assert_eq!(second.header.opened_at_ms(), second_epoch);
        assert_eq!(first.footer.record_count(), 1);
        assert_eq!(second.footer.record_count(), 1);
        let manifest = read_epoch_manifest_for_signer(
            epoch_manifest_path(&config.network_directory, &first_network.key_id).unwrap(),
            SigningKey::from_bytes(&[6; 32]).verifying_key().to_bytes(),
        )
        .unwrap();
        let mut complete = manifest.verifier();
        complete.verify_next(&first).unwrap();
        complete.finish().unwrap();
    }

    #[cfg_attr(
        not(any(target_os = "linux", target_os = "macos")),
        ignore = "persistent regulatory trace is supported only on Linux and macOS"
    )]
    #[test]
    fn restart_idempotently_converges_both_terminal_markers_before_key_destruction() {
        const CLOSED_EPOCH: u64 = 19_000 * TRACE_EPOCH_DURATION_MS;
        let root = tempfile::tempdir().unwrap();
        let network = ResolvedClaimTraceKey {
            key_id: ComplianceKeyId::new(
                CompliancePurpose::NetworkTrace,
                Jurisdiction::Test,
                [51; 32],
                CLOSED_EPOCH,
                1,
            ),
            public_key: [8; 32],
            not_before_ms: CLOSED_EPOCH,
            not_after_ms: CLOSED_EPOCH + TRACE_EPOCH_DURATION_MS,
        };
        let identity = ResolvedClaimTraceKey {
            key_id: ComplianceKeyId::new(
                CompliancePurpose::IdentityTrace,
                Jurisdiction::Test,
                [52; 32],
                CLOSED_EPOCH,
                1,
            ),
            public_key: [9; 32],
            not_before_ms: CLOSED_EPOCH,
            not_after_ms: CLOSED_EPOCH + TRACE_EPOCH_DURATION_MS,
        };
        let resolver: Arc<dyn ClaimTraceKeyResolver> = Arc::new(FixedResolver {
            network,
            identity: Some(identity),
        });
        let config = SealedClaimTraceConfig {
            network_directory: root.path().join("network"),
            identity_directory: root.path().join("identity"),
            network_max_storage_bytes: DEFAULT_TRACE_STORAGE_BYTES,
            identity_max_storage_bytes: DEFAULT_TRACE_STORAGE_BYTES,
            jurisdiction: Jurisdiction::Test,
            node_id: [5; 32],
            capture_policy: ClaimCapturePolicy::Standing,
            retention_days: None,
            max_records_per_segment: 10,
            planned_records_per_minute: 1,
            capacity_utc_epochs: 1,
        };
        let sink = SealedClaimTraceSink::new_with_options(
            config.clone(),
            Arc::clone(&resolver),
            [6; 32],
            [7; 32],
            WorkerOptions {
                enforce_wall_clock: false,
                ..WorkerOptions::default()
            },
        )
        .unwrap();
        sink.capture(claim(CLOSED_EPOCH + 100, 1)).unwrap();
        {
            let mut state = sink
                .inner
                .state
                .lock()
                .unwrap_or_else(|error| error.into_inner());
            let close = CLOSED_EPOCH + TRACE_EPOCH_DURATION_MS - 1;
            finalize_purpose(
                sink.inner
                    .directory(CompliancePurpose::NetworkTrace)
                    .unwrap(),
                &mut state.network,
                &sink.inner.network_storage_budget,
                &sink.inner.network_signer,
                close,
            )
            .unwrap();
            finalize_purpose(
                sink.inner
                    .directory(CompliancePurpose::IdentityTrace)
                    .unwrap(),
                &mut state.identity,
                &sink.inner.identity_storage_budget,
                &sink.inner.identity_signer,
                close,
            )
            .unwrap();
            publish_terminal_manifest(
                &config,
                CompliancePurpose::NetworkTrace,
                sink.inner
                    .directory(CompliancePurpose::NetworkTrace)
                    .unwrap(),
                &state.network,
                &sink.inner.network_storage_budget,
                &sink.inner.network_signer,
                resolver.as_ref(),
            )
            .unwrap();
            publish_terminal_manifest(
                &config,
                CompliancePurpose::IdentityTrace,
                sink.inner
                    .directory(CompliancePurpose::IdentityTrace)
                    .unwrap(),
                &state.identity,
                &sink.inner.identity_storage_budget,
                &sink.inner.identity_signer,
                resolver.as_ref(),
            )
            .unwrap();
        }
        let network_manifest =
            epoch_manifest_path(&config.network_directory, &network.key_id).unwrap();
        let identity_manifest =
            epoch_manifest_path(&config.identity_directory, &identity.key_id).unwrap();
        let network_bytes = fs::read(&network_manifest).unwrap();
        let identity_bytes = fs::read(&identity_manifest).unwrap();
        sink.crash_for_test();
        drop(sink);

        let restarted = SealedClaimTraceSink::new_with_options(
            config.clone(),
            resolver,
            [6; 32],
            [7; 32],
            WorkerOptions {
                enforce_wall_clock: false,
                ..WorkerOptions::default()
            },
        )
        .unwrap();
        assert!(!config.network_directory.join(LIVE_KEY_NAME).exists());
        assert!(!config.identity_directory.join(LIVE_KEY_NAME).exists());
        assert_eq!(fs::read(network_manifest).unwrap(), network_bytes);
        assert_eq!(fs::read(identity_manifest).unwrap(), identity_bytes);
        restarted.shutdown(now_ms()).unwrap();
    }

    #[cfg_attr(
        not(any(target_os = "linux", target_os = "macos")),
        ignore = "persistent regulatory trace is supported only on Linux and macOS"
    )]
    #[test]
    fn missing_either_purpose_key_fails_readiness_and_capture() {
        let root = tempfile::tempdir().unwrap();
        let (config, resolver, timestamp) = fixture(&root, false);
        let sink = SealedClaimTraceSink::new(config, resolver, [6; 32], [7; 32]).unwrap();
        assert!(sink.readiness(timestamp).is_err());
        assert!(sink
            .capture(ClaimTraceInput {
                timestamp_ms: timestamp,
                source: "192.0.2.10:4567".parse().unwrap(),
                provider: IdentityProvider::Oauth2,
                provider_subject: "github:123456".into(),
            })
            .is_err());
    }

    #[test]
    fn resolved_claim_trace_keys_reject_noncanonical_daily_intervals() {
        let timestamp = now_ms();
        let start = day_start_ms(timestamp);
        let mut key = ResolvedClaimTraceKey {
            key_id: ComplianceKeyId::new(
                CompliancePurpose::IdentityTrace,
                Jurisdiction::Test,
                [4; 32],
                start,
                1,
            ),
            public_key: [9; 32],
            not_before_ms: start,
            not_after_ms: start + 2 * TRACE_EPOCH_DURATION_MS,
        };
        assert!(matches!(
            validate_resolved_key(
                &key,
                CompliancePurpose::IdentityTrace,
                Jurisdiction::Test,
                timestamp,
            ),
            Err(ClaimTraceError::Unavailable)
        ));
        key.key_id.epoch_start_ms = start + 1;
        key.not_before_ms = start + 1;
        key.not_after_ms = start + 1 + TRACE_EPOCH_DURATION_MS;
        assert!(matches!(
            validate_resolved_key(
                &key,
                CompliancePurpose::IdentityTrace,
                Jurisdiction::Test,
                timestamp,
            ),
            Err(ClaimTraceError::Unavailable)
        ));
    }

    #[test]
    fn claim_inputs_never_expose_network_or_identity_values_through_debug() {
        let input = ClaimTraceInput {
            timestamp_ms: now_ms(),
            source: "192.0.2.99:6543".parse().unwrap(),
            provider: IdentityProvider::Oauth2,
            provider_subject: "github:private-subject".into(),
        };
        let debug = format!("{input:?}");
        assert!(!debug.contains("192.0.2.99"));
        assert!(!debug.contains("6543"));
        assert!(!debug.contains("private-subject"));
    }

    #[cfg(unix)]
    #[test]
    fn claim_trace_rejects_linked_live_keys_and_linked_directories() {
        use std::os::unix::fs::symlink;

        let root = tempfile::tempdir().unwrap();
        let (config, resolver, timestamp) = fixture(&root, true);
        let sink =
            SealedClaimTraceSink::new(config.clone(), Arc::clone(&resolver), [6; 32], [7; 32])
                .unwrap();
        sink.capture(ClaimTraceInput {
            timestamp_ms: timestamp,
            source: "192.0.2.10:4567".parse().unwrap(),
            provider: IdentityProvider::Oauth2,
            provider_subject: "github:123456".into(),
        })
        .unwrap();
        drop(sink);

        let network_live = config.network_directory.join(LIVE_KEY_NAME);
        let alias = config.network_directory.join("live-key-alias");
        fs::hard_link(&network_live, &alias).unwrap();
        assert!(
            SealedClaimTraceSink::new(config.clone(), Arc::clone(&resolver), [6; 32], [7; 32])
                .is_err()
        );
        assert!(
            network_live.exists(),
            "refusal must not unlink the named key"
        );
        fs::remove_file(&alias).unwrap();

        let linked_root = tempfile::tempdir().unwrap();
        let actual_network = linked_root.path().join("actual-network");
        fs::create_dir(&actual_network).unwrap();
        let network_link = linked_root.path().join("network-link");
        symlink(&actual_network, &network_link).unwrap();
        let linked_config = SealedClaimTraceConfig {
            network_directory: network_link,
            identity_directory: linked_root.path().join("identity"),
            ..config
        };
        assert!(SealedClaimTraceSink::new(linked_config, resolver, [6; 32], [7; 32]).is_err());
    }

    #[cfg(unix)]
    #[test]
    fn claim_trace_rejects_world_readable_live_key_without_rewriting_it() {
        use std::os::unix::fs::PermissionsExt;

        let root = tempfile::tempdir().unwrap();
        let (config, resolver, timestamp) = fixture(&root, true);
        let sink =
            SealedClaimTraceSink::new(config.clone(), Arc::clone(&resolver), [6; 32], [7; 32])
                .unwrap();
        sink.capture(ClaimTraceInput {
            timestamp_ms: timestamp,
            source: "192.0.2.10:4567".parse().unwrap(),
            provider: IdentityProvider::Oauth2,
            provider_subject: "github:123456".into(),
        })
        .unwrap();
        drop(sink);

        let live = config.network_directory.join(LIVE_KEY_NAME);
        fs::set_permissions(&live, fs::Permissions::from_mode(0o644)).unwrap();
        assert!(SealedClaimTraceSink::new(config, resolver, [6; 32], [7; 32]).is_err());
        assert_eq!(
            fs::metadata(live).unwrap().permissions().mode() & 0o777,
            0o644
        );
    }

    #[cfg(unix)]
    #[test]
    fn claim_trace_rejects_intermediate_symlinks_and_mutable_ancestors_without_creation() {
        use std::os::unix::fs::{symlink, PermissionsExt};

        let root = tempfile::tempdir().unwrap();
        let actual = root.path().join("actual");
        ensure_private_test_directory(&actual);
        let alias = root.path().join("alias");
        symlink(&actual, &alias).unwrap();
        let (mut linked, resolver, _) = fixture(&root, true);
        linked.network_directory = alias.join("network");
        assert!(
            SealedClaimTraceSink::new(linked, Arc::clone(&resolver), [6; 32], [7; 32]).is_err()
        );
        assert!(!actual.join("network").exists());

        let mutable = root.path().join("mutable");
        ensure_private_test_directory(&mutable);
        fs::set_permissions(&mutable, fs::Permissions::from_mode(0o770)).unwrap();
        let (mut unsafe_config, _, _) = fixture(&root, true);
        unsafe_config.network_directory = mutable.join("network");
        unsafe_config.identity_directory = mutable.join("identity");
        assert!(SealedClaimTraceSink::new(unsafe_config, resolver, [6; 32], [7; 32]).is_err());
        assert!(!mutable.join("network").exists());
        assert!(!mutable.join("identity").exists());
    }

    #[cfg(unix)]
    #[test]
    fn claim_trace_parent_replacement_fails_before_trace_mutation() {
        let root = tempfile::tempdir().unwrap();
        let (config, resolver, timestamp) = fixture(&root, true);
        let sink = SealedClaimTraceSink::new(config.clone(), resolver, [6; 32], [7; 32]).unwrap();
        let moved = root.path().join("network-retained");
        fs::rename(&config.network_directory, &moved).unwrap();
        ensure_private_test_directory(&config.network_directory);

        assert_eq!(
            sink.capture(claim(timestamp, 44)),
            Err(ClaimTraceError::Unavailable)
        );
        assert_eq!(fs::read_dir(&config.network_directory).unwrap().count(), 0);
        assert!(!moved.join(LIVE_KEY_NAME).exists());
        assert!(fs::read_dir(&config.identity_directory)
            .unwrap()
            .all(|entry| {
                entry
                    .unwrap()
                    .file_name()
                    .to_string_lossy()
                    .contains("writer")
            }));
    }

    #[cfg(unix)]
    #[test]
    fn live_key_publication_recovers_from_temp_collision_and_preserves_unsafe_destination() {
        use std::os::unix::fs::{FileTypeExt, OpenOptionsExt};
        use std::process::Command;

        let root = tempfile::tempdir().unwrap();
        let (config, resolver, timestamp) = fixture(&root, true);
        let sink = SealedClaimTraceSink::new(config.clone(), resolver, [6; 32], [7; 32]).unwrap();
        let directory = sink
            .inner
            .directory(CompliancePurpose::NetworkTrace)
            .unwrap();
        let key_id = ComplianceKeyId::new(
            CompliancePurpose::NetworkTrace,
            Jurisdiction::Test,
            [81; 32],
            day_start_ms(timestamp),
            1,
        );
        let live = LiveKeyState {
            key_id,
            segment_index: 7,
            secret: [82; 32],
        };
        let colliding_counter = TEMP_COUNTER.load(Ordering::Relaxed);
        let collision = directory.path.join(format!(
            ".live-key.{}.{}.tmp",
            std::process::id(),
            colliding_counter
        ));
        let mut collision_options = OpenOptions::new();
        collision_options.write(true).create_new(true).mode(0o600);
        collision_options
            .open(&collision)
            .unwrap()
            .write_all(b"collision")
            .unwrap();
        let collision_name = collision.file_name().unwrap().to_os_string();
        persist_live_key(directory, &live).unwrap();
        assert_eq!(fs::read(&collision).unwrap(), b"collision");
        let loaded = load_live_key(directory).unwrap().unwrap();
        assert_eq!(loaded.key_id, key_id);
        assert_eq!(loaded.segment_index, 7);
        assert!(!config.identity_directory.join(LIVE_KEY_NAME).exists());

        fs::remove_file(config.network_directory.join(LIVE_KEY_NAME)).unwrap();
        let unsafe_destination = config.network_directory.join(LIVE_KEY_NAME);
        assert!(Command::new("mkfifo")
            .arg(&unsafe_destination)
            .status()
            .unwrap()
            .success());
        assert_eq!(
            persist_live_key(directory, &live),
            Err(ClaimTraceError::Unavailable)
        );
        assert!(fs::symlink_metadata(&unsafe_destination)
            .unwrap()
            .file_type()
            .is_fifo());
        let unexpected_temps = fs::read_dir(&config.network_directory)
            .unwrap()
            .filter_map(Result::ok)
            .filter(|entry| {
                entry
                    .file_name()
                    .to_string_lossy()
                    .starts_with(".live-key.")
                    && entry.file_name() != collision_name
            })
            .map(|entry| entry.file_name())
            .collect::<Vec<_>>();
        assert!(unexpected_temps.is_empty(), "{unexpected_temps:?}");
    }
}
