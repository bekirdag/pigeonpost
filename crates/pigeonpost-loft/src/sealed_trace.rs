//! Durable online-only adapter from normalized loft trace facts to sealed daily segments.
//!
//! The live epoch key is the only trace decryption material present on a node. It is kept in a
//! `0600` recovery record for the current UTC day, wrapped into every segment header for offline
//! custody, and deleted after rollover. This package links only the online `compliance-seal` crate;
//! it has no custody or unseal dependency.

use std::io::{Read, Write};
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
pub use pigeonpost_compliance_format::TraceCapturePolicy as CapturePolicy;
use pigeonpost_compliance_format::{
    trace_epoch_contains, trace_epoch_end_ms as canonical_trace_epoch_end_ms, validate_trace_epoch,
    ComplianceKeyId, CompliancePurpose, Jurisdiction, TraceRetentionPolicy, COMPLIANCE_KEY_ID_LEN,
    TRACE_EPOCH_DURATION_MS,
};
#[cfg(test)]
use pigeonpost_compliance_seal::read_epoch_manifest_for_signer;
#[cfg(test)]
use pigeonpost_compliance_seal::DEFAULT_TRACE_STORAGE_BYTES;
use pigeonpost_compliance_seal::{
    epoch_manifest_path, publish_epoch_manifest, recover_segment,
    required_network_trace_storage_bytes, verify_segment, EpochManifest, EpochSealingKey,
    EpochSegmentEntry, NetworkOperation, Recovery, SegmentFooter, SegmentHeader, SegmentWriter,
    TraceIp, TraceRecord, TraceStorageBudget, TraceWriterLease, EPOCH_MANIFEST_ENTRY_LEN,
    EPOCH_MANIFEST_FIXED_LEN, MAX_EPOCH_MANIFEST_SEGMENTS, MAX_SEGMENT_RECORDS,
    NETWORK_TRACE_FRAME_BYTES, SEGMENT_FOOTER_LEN, SEGMENT_HEADER_LEN, TRACE_LIVE_KEY_BYTES,
};
use rand_core::{OsRng, RngCore};
use sha2::{Digest, Sha256};
use zeroize::{Zeroize, Zeroizing};

#[cfg(unix)]
use pigeonpost_unix_custody::{
    CustodyError, FilePolicy, GuardedDir, GuardedFile, LeafName, OpenAccess,
};

use crate::attribution::AttributionResolutionError;
use crate::store::{TraceSegmentCatalog, TraceSegmentMetadata, TraceSegmentState};
use crate::trace::{TraceCapacity, TraceInput, TraceOperation, TraceSink, TraceSinkError};

const LIVE_KEY_MAGIC: &[u8; 8] = b"PPLIVEK\0";
const LIVE_KEY_VERSION: u8 = 1;
const LIVE_KEY_LEN: usize = TRACE_LIVE_KEY_BYTES as usize;
const LIVE_KEY_NAME: &str = "network-trace-live-key-v1";
const TRACE_QUEUE_CAPACITY: usize = 64;
const TRACE_BATCH_MAX: usize = 32;
const TRACE_BATCH_WINDOW: Duration = Duration::from_millis(2);
const TRACE_ROLLOVER_CLOCK_POLL: Duration = Duration::from_secs(1);
const WORKER_RUNNING: u8 = 0;
const WORKER_SHUTTING_DOWN: u8 = 1;
const WORKER_STOPPED: u8 = 2;
const WORKER_POISONED: u8 = 3;
static TEMP_COUNTER: AtomicU64 = AtomicU64::new(0);

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ResolvedTraceKey {
    pub key_id: ComplianceKeyId,
    pub public_key: [u8; 32],
    pub not_before_ms: u64,
    pub not_after_ms: u64,
}

/// Cache-only lookup used when opening a new trace epoch. Implementations must not perform network
/// I/O in this method; registry refresh belongs in a supervised background task.
pub trait TraceKeyResolver: Send + Sync {
    fn readiness(&self, now_ms: u64) -> Result<(), AttributionResolutionError>;

    fn resolve_trace_key(
        &self,
        purpose: CompliancePurpose,
        jurisdiction: Jurisdiction,
        at_ms: u64,
    ) -> Result<Option<ResolvedTraceKey>, AttributionResolutionError>;
}

#[derive(Debug, Clone)]
pub struct SealedTraceConfig {
    pub directory: PathBuf,
    pub jurisdiction: Jurisdiction,
    pub node_id: [u8; 32],
    pub capture_policy: CapturePolicy,
    /// Legal standing-retention choice. US is fixed at 30 days, TR is operator-selected from
    /// 365..=730, and EU preservation forbids a standing-retention value.
    pub retention_days: Option<u64>,
    /// Maximum trace-producing admissions per independently aligned 60-second window.
    pub planned_records_per_minute: u32,
    /// Number of UTC trace epochs this append-only online store is planned to carry. Standing
    /// duties include the current open epoch in addition to retained closed history.
    pub capacity_utc_epochs: u64,
    pub max_records_per_segment: u32,
    /// Independent logical cap for this online trace directory. Ordinary frames cannot consume
    /// the protected terminal-manifest reserve inside this budget.
    pub max_storage_bytes: u64,
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
struct RuntimeState {
    live: Option<LiveKeyState>,
    active: Option<ActiveSegment>,
    poisoned: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct StorageTransition {
    terminal_bytes: u64,
    released_bytes: u64,
    normal_bytes: u64,
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
    enforce_capacity_plan: bool,
}

impl Default for WorkerOptions {
    fn default() -> Self {
        Self {
            queue_capacity: TRACE_QUEUE_CAPACITY,
            batch_max: TRACE_BATCH_MAX,
            batch_window: TRACE_BATCH_WINDOW,
            rollover_clock_poll: TRACE_ROLLOVER_CLOCK_POLL,
            clock: WorkerClock::System,
            enforce_wall_clock: true,
            enforce_capacity_plan: true,
        }
    }
}

struct CaptureCommand {
    input: TraceInput,
    acknowledged: mpsc::SyncSender<Result<(), TraceSinkError>>,
}

enum TraceCommand {
    Capture(CaptureCommand),
    Shutdown {
        timestamp_ms: u64,
        acknowledged: mpsc::SyncSender<Result<(), TraceSinkError>>,
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

struct SealedTraceInner {
    config: SealedTraceConfig,
    writer_lease: TraceWriterLease,
    #[cfg(unix)]
    directory: GuardedDir,
    resolver: Arc<dyn TraceKeyResolver>,
    catalog: Arc<dyn TraceSegmentCatalog>,
    signer: SigningKey,
    storage_budget: TraceStorageBudget,
    state: Mutex<RuntimeState>,
    enforce_wall_clock: bool,
}

/// Production trace sink: crash-recoverable, daily, purpose-scoped, and fail closed.
pub struct SealedTraceSink {
    inner: Arc<SealedTraceInner>,
    sender: mpsc::SyncSender<TraceCommand>,
    status: Arc<WorkerStatus>,
    worker: Mutex<Option<JoinHandle<()>>>,
}

impl core::fmt::Debug for SealedTraceSink {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        let state = self
            .inner
            .state
            .lock()
            .unwrap_or_else(|error| error.into_inner());
        f.debug_struct("SealedTraceSink")
            .field("directory", &"<withheld>")
            .field("jurisdiction", &self.inner.config.jurisdiction)
            .field("node_id", &self.inner.config.node_id)
            .field("capture_policy", &self.inner.config.capture_policy)
            .field("has_live_key", &state.live.is_some())
            .field("has_open_segment", &state.active.is_some())
            .field(
                "worker_state",
                &self.status.lifecycle.load(Ordering::Acquire),
            )
            .finish()
    }
}

impl SealedTraceSink {
    pub fn new(
        config: SealedTraceConfig,
        resolver: Arc<dyn TraceKeyResolver>,
        catalog: Arc<dyn TraceSegmentCatalog>,
        segment_signing_seed: [u8; 32],
    ) -> Result<Self, TraceSinkError> {
        Self::new_with_options(
            config,
            resolver,
            catalog,
            segment_signing_seed,
            WorkerOptions::default(),
        )
    }

    fn new_with_options(
        config: SealedTraceConfig,
        resolver: Arc<dyn TraceKeyResolver>,
        catalog: Arc<dyn TraceSegmentCatalog>,
        segment_signing_seed: [u8; 32],
        options: WorkerOptions,
    ) -> Result<Self, TraceSinkError> {
        Self::new_with_options_for_platform(
            PersistentTracePlatform::current(),
            config,
            resolver,
            catalog,
            segment_signing_seed,
            options,
        )
    }

    fn new_with_options_for_platform(
        platform: PersistentTracePlatform,
        config: SealedTraceConfig,
        resolver: Arc<dyn TraceKeyResolver>,
        catalog: Arc<dyn TraceSegmentCatalog>,
        segment_signing_seed: [u8; 32],
        options: WorkerOptions,
    ) -> Result<Self, TraceSinkError> {
        require_supported_persistent_trace_platform(platform)?;
        #[cfg(unix)]
        let mut config = config;
        validate_config(&config)?;
        if options.enforce_capacity_plan {
            validate_capacity_plan(&config)?;
        }
        if options.queue_capacity == 0
            || options.batch_max == 0
            || options.rollover_clock_poll.is_zero()
        {
            return Err(TraceSinkError::Unavailable);
        }
        if segment_signing_seed == [0u8; 32] {
            return Err(TraceSinkError::Unavailable);
        }
        let segment_signing_seed = Zeroizing::new(segment_signing_seed);
        #[cfg(unix)]
        let directory = {
            let directory = secure_directory(&config.directory)?;
            config.directory = directory.absolute_path().to_path_buf();
            directory
        };
        #[cfg(not(unix))]
        secure_directory(&config.directory)?;
        let writer_lease = TraceWriterLease::acquire(&config.directory)
            .map_err(|_| TraceSinkError::Unavailable)?;
        let storage_budget = TraceStorageBudget::new(&config.directory, config.max_storage_bytes)
            .map_err(|_| TraceSinkError::Unavailable)?;
        let signer = SigningKey::from_bytes(&segment_signing_seed);
        let inner = Arc::new(SealedTraceInner {
            config,
            writer_lease,
            #[cfg(unix)]
            directory,
            resolver,
            catalog,
            signer,
            storage_budget,
            state: Mutex::new(RuntimeState::default()),
            enforce_wall_clock: options.enforce_wall_clock,
        });
        inner.recover_live_state(options.clock.now_ms())?;
        inner
            .storage_budget
            .reconcile(&inner.config.directory)
            .map_err(|_| TraceSinkError::Unavailable)?;

        let (sender, receiver) = mpsc::sync_channel(options.queue_capacity);
        let status = Arc::new(WorkerStatus::default());
        let worker_inner = Arc::clone(&inner);
        let worker_status = Arc::clone(&status);
        let worker = std::thread::Builder::new()
            .name("pigeonpost-trace-commit".into())
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
            .map_err(|_| TraceSinkError::Unavailable)?;
        Ok(Self {
            inner,
            sender,
            status,
            worker: Mutex::new(Some(worker)),
        })
    }

    fn shutdown_worker(&self, timestamp_ms: u64) -> Result<(), TraceSinkError> {
        let (acknowledged, result) = mpsc::sync_channel(1);
        {
            // This lock makes the lifecycle transition and the final queue insertion one atomic
            // admission decision. No capture can slip behind the shutdown command.
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
                _ => return Err(TraceSinkError::Unavailable),
            }
            if self
                .sender
                .send(TraceCommand::Shutdown {
                    timestamp_ms,
                    acknowledged,
                })
                .is_err()
            {
                self.status.poison();
                return Err(TraceSinkError::Unavailable);
            }
        }
        let outcome = result.recv().unwrap_or(Err(TraceSinkError::Unavailable));
        let joined = self
            .worker
            .lock()
            .unwrap_or_else(|error| error.into_inner())
            .take()
            .is_none_or(|worker| worker.join().is_ok());
        if outcome.is_err() || !joined {
            self.status.poison();
            return Err(TraceSinkError::Unavailable);
        }
        outcome
    }

    fn capture_inner(
        &self,
        mut input: TraceInput,
        admission_now_ms: Option<u64>,
    ) -> Result<(), TraceSinkError> {
        let (acknowledged, result) = mpsc::sync_channel(1);
        {
            let _admission = self
                .status
                .admission
                .lock()
                .unwrap_or_else(|error| error.into_inner());
            if !self.status.is_running() {
                return Err(TraceSinkError::Unavailable);
            }
            if self.inner.enforce_wall_clock {
                let admitted_ms = admission_now_ms.unwrap_or_else(now_ms);
                let previous = self.status.last_admitted_ms.load(Ordering::Acquire);
                if admitted_ms == 0 || admitted_ms < previous {
                    // Refuse a backward clock step; clamping it would falsify retained evidence.
                    // The worker itself remains healthy for a later non-decreasing admission.
                    return Err(TraceSinkError::Unavailable);
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
                return Ok(());
            }
            self.sender
                .try_send(TraceCommand::Capture(CaptureCommand {
                    input,
                    acknowledged,
                }))
                .map_err(|_| TraceSinkError::Unavailable)?;
            #[cfg(test)]
            self.status.enqueued_captures.fetch_add(1, Ordering::AcqRel);
        }
        result.recv().unwrap_or_else(|_| {
            self.status.poison();
            Err(TraceSinkError::Unavailable)
        })
    }

    fn readiness_at(&self, current_ms: u64) -> Result<(), TraceSinkError> {
        if !self.status.is_running() {
            return Err(TraceSinkError::Unavailable);
        }
        if !self.inner.config.capture_policy.captures(current_ms) {
            return Ok(());
        }
        let resolved = self.inner.resolve_key(current_ms, current_ms)?;
        let state = self
            .inner
            .state
            .lock()
            .unwrap_or_else(|error| error.into_inner());
        if state.poisoned {
            return Err(TraceSinkError::Unavailable);
        }
        self.inner
            .ensure_storage_headroom_locked(&state, &resolved, current_ms)
            .map_err(|_| TraceSinkError::Unavailable)
    }

    #[cfg(test)]
    fn capture_at_for_test(
        &self,
        input: TraceInput,
        admission_now_ms: u64,
    ) -> Result<(), TraceSinkError> {
        self.capture_inner(input, Some(admission_now_ms))
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
            self.sender.send(TraceCommand::Crash).unwrap();
        }
        let worker = self
            .worker
            .lock()
            .unwrap_or_else(|error| error.into_inner())
            .take()
            .expect("trace worker exists");
        assert!(worker.join().is_ok());
        assert_eq!(
            self.status.lifecycle.load(Ordering::Acquire),
            WORKER_POISONED
        );
    }
}

impl SealedTraceInner {
    fn recover_live_state(&self, current_ms: u64) -> Result<(), TraceSinkError> {
        self.assert_writer_lease()?;
        let Some(mut live) = self.load_live_key()? else {
            return Ok(());
        };
        validate_live_key(&self.config, &live)?;
        let final_path = segment_path(&self.config.directory, &live.key_id, live.segment_index)?;
        let open_path = open_path_for(&final_path);
        if self.entry_exists(&final_path)? && self.entry_exists(&open_path)? {
            return Err(TraceSinkError::Unavailable);
        }

        let mut active = None;
        if self.entry_exists(&open_path)? {
            let epoch_key = EpochSealingKey::from_bytes(live.key_id, live.secret)
                .map_err(|_| TraceSinkError::Unavailable)?;
            match recover_segment(&final_path, epoch_key)
                .map_err(|_| TraceSinkError::Unavailable)?
            {
                Recovery::Resumed(writer) => {
                    if writer.header().signer_public_key() != self.signer.verifying_key().to_bytes()
                    {
                        return Err(TraceSinkError::Unavailable);
                    }
                    self.catalog
                        .record_trace_segment(&open_metadata(writer.header(), &final_path)?)
                        .map_err(|_| TraceSinkError::Unavailable)?;
                    active = Some(ActiveSegment {
                        last_record_ms: writer.header().opened_at_ms(),
                        writer,
                    });
                }
                Recovery::Finalized(verified) => {
                    self.catalog
                        .record_trace_segment(&closed_metadata(
                            &verified.header,
                            &verified.footer,
                            &final_path,
                        )?)
                        .map_err(|_| TraceSinkError::Unavailable)?;
                    live.segment_index = live
                        .segment_index
                        .checked_add(1)
                        .ok_or(TraceSinkError::Unavailable)?;
                    self.persist_live_key(&live)?;
                }
            }
        } else if self.entry_exists(&final_path)? {
            let verified = verify_segment(&final_path).map_err(|_| TraceSinkError::Unavailable)?;
            self.catalog
                .record_trace_segment(&closed_metadata(
                    &verified.header,
                    &verified.footer,
                    &final_path,
                )?)
                .map_err(|_| TraceSinkError::Unavailable)?;
            live.segment_index = live
                .segment_index
                .checked_add(1)
                .ok_or(TraceSinkError::Unavailable)?;
            self.persist_live_key(&live)?;
        }

        let epoch_end = epoch_end_ms(&live.key_id)?;
        if current_ms < live.key_id.epoch_start_ms {
            return Err(TraceSinkError::Unavailable);
        }
        let mut state = self.state.lock().unwrap_or_else(|error| error.into_inner());
        state.live = Some(live);
        state.active = active;
        if current_ms >= epoch_end {
            self.finalize_active_locked(&mut state, epoch_end.saturating_sub(1))?;
            self.publish_terminal_manifest_locked(&state)?;
            self.destroy_live_key_locked(&mut state)?;
        }
        Ok(())
    }

    fn capture_buffered(&self, input: TraceInput) -> Result<bool, TraceSinkError> {
        self.assert_writer_lease()?;
        if !self.config.capture_policy.captures(input.timestamp_ms) {
            return Ok(false);
        }
        // Production timestamps were sampled under queue admission. Checking the wall clock again
        // in the worker would create a second ordering boundary; validate only the intrinsic form.
        if input.timestamp_ms == 0 {
            return Err(TraceSinkError::Unavailable);
        }
        let resolved = self.resolve_key(input.timestamp_ms, now_ms())?;
        let mut state = self.state.lock().unwrap_or_else(|error| error.into_inner());
        if state.poisoned {
            return Err(TraceSinkError::Unavailable);
        }
        let result = (|| {
            self.ensure_storage_headroom_locked(&state, &resolved, input.timestamp_ms)?;
            self.ensure_writer_locked(&mut state, input.timestamp_ms, &resolved)?;
            let record = trace_record(&self.config, input)?;
            self.storage_budget
                .charge_normal(NETWORK_TRACE_FRAME_BYTES)
                .map_err(|_| TraceSinkError::Capacity)?;
            let append = state
                .active
                .as_mut()
                .ok_or(TraceSinkError::Unavailable)?
                .writer
                .append_network_buffered(&record);
            match append {
                Ok(_) => {}
                Err(pigeonpost_compliance_seal::SealError::SegmentFull) => {
                    self.storage_budget
                        .release(NETWORK_TRACE_FRAME_BYTES)
                        .map_err(|_| TraceSinkError::Unavailable)?;
                    self.finalize_active_locked(&mut state, input.timestamp_ms)?;
                    self.ensure_writer_locked(&mut state, input.timestamp_ms, &resolved)?;
                    self.storage_budget
                        .charge_normal(NETWORK_TRACE_FRAME_BYTES)
                        .map_err(|_| TraceSinkError::Capacity)?;
                    state
                        .active
                        .as_mut()
                        .ok_or(TraceSinkError::Unavailable)?
                        .writer
                        .append_network_buffered(&record)
                        .map_err(|_| TraceSinkError::Unavailable)?;
                }
                Err(_) => return Err(TraceSinkError::Unavailable),
            }
            let active = state.active.as_mut().ok_or(TraceSinkError::Unavailable)?;
            active.last_record_ms = active.last_record_ms.max(input.timestamp_ms);
            Ok(true)
        })();
        if result
            .as_ref()
            .is_err_and(|error| !matches!(error, TraceSinkError::Capacity))
        {
            state.poisoned = true;
        }
        result
    }

    fn sync_active(&self) -> Result<(), TraceSinkError> {
        self.assert_writer_lease()?;
        let mut state = self.state.lock().unwrap_or_else(|error| error.into_inner());
        if state.poisoned {
            return Err(TraceSinkError::Unavailable);
        }
        let result = state
            .active
            .as_mut()
            .ok_or(TraceSinkError::Unavailable)?
            .writer
            .sync_data()
            .map_err(|_| TraceSinkError::Unavailable);
        if result.is_err() {
            state.poisoned = true;
        }
        result
    }

    fn sync_partial_batch_after_capacity(&self) -> Result<(), TraceSinkError> {
        self.assert_writer_lease()?;
        let mut state = self.state.lock().unwrap_or_else(|error| error.into_inner());
        if state.poisoned {
            return Err(TraceSinkError::Unavailable);
        }
        match state.active.as_mut() {
            Some(active) => active
                .writer
                .sync_data()
                .map_err(|_| TraceSinkError::Unavailable),
            None => Ok(()),
        }
    }

    fn resolve_key(
        &self,
        timestamp_ms: u64,
        readiness_ms: u64,
    ) -> Result<ResolvedTraceKey, TraceSinkError> {
        self.resolver
            .readiness(readiness_ms)
            .map_err(|_| TraceSinkError::Unavailable)?;
        let resolved = self
            .resolver
            .resolve_trace_key(
                CompliancePurpose::NetworkTrace,
                self.config.jurisdiction,
                timestamp_ms,
            )
            .map_err(|_| TraceSinkError::Unavailable)?
            .ok_or(TraceSinkError::Unavailable)?;
        validate_resolved_key(&resolved, self.config.jurisdiction, timestamp_ms)?;
        Ok(resolved)
    }

    fn ensure_storage_headroom_locked(
        &self,
        state: &RuntimeState,
        resolved: &ResolvedTraceKey,
        timestamp_ms: u64,
    ) -> Result<(), TraceSinkError> {
        let transition = self.storage_transition_locked(state, resolved, timestamp_ms)?;
        if self.storage_budget.has_transition_headroom(
            transition.terminal_bytes,
            transition.released_bytes,
            transition.normal_bytes,
        ) {
            Ok(())
        } else {
            Err(TraceSinkError::Capacity)
        }
    }

    fn storage_transition_locked(
        &self,
        state: &RuntimeState,
        resolved: &ResolvedTraceKey,
        timestamp_ms: u64,
    ) -> Result<StorageTransition, TraceSinkError> {
        let frame = NETWORK_TRACE_FRAME_BYTES;
        let header = SEGMENT_HEADER_LEN as u64;
        let footer = SEGMENT_FOOTER_LEN as u64;
        let live_key = TRACE_LIVE_KEY_BYTES;

        let Some(live) = state.live.as_ref() else {
            if state.active.is_some() {
                return Err(TraceSinkError::Unavailable);
            }
            return Ok(StorageTransition {
                terminal_bytes: 0,
                released_bytes: 0,
                normal_bytes: live_key
                    .checked_add(header)
                    .and_then(|bytes| bytes.checked_add(frame))
                    .ok_or(TraceSinkError::Unavailable)?,
            });
        };

        if live.key_id == resolved.key_id {
            let (terminal_bytes, normal_bytes) = match state.active.as_ref() {
                Some(active)
                    if active.writer.record_count() == self.config.max_records_per_segment =>
                {
                    (footer, header.checked_add(frame))
                }
                Some(active)
                    if active.writer.record_count() < self.config.max_records_per_segment =>
                {
                    (0, Some(frame))
                }
                Some(_) => return Err(TraceSinkError::Unavailable),
                None => (0, header.checked_add(frame)),
            };
            return Ok(StorageTransition {
                terminal_bytes,
                released_bytes: 0,
                normal_bytes: normal_bytes.ok_or(TraceSinkError::Unavailable)?,
            });
        }

        if live.key_id.epoch_start_ms > resolved.key_id.epoch_start_ms {
            return Err(TraceSinkError::Unavailable);
        }
        let closing_epoch_end = epoch_end_ms(&live.key_id)?;
        if timestamp_ms < closing_epoch_end || resolved.key_id.epoch_start_ms < closing_epoch_end {
            return Err(TraceSinkError::Unavailable);
        }
        let segment_count = live
            .segment_index
            .checked_add(u32::from(state.active.is_some()))
            .ok_or(TraceSinkError::Unavailable)?;
        if segment_count > MAX_EPOCH_MANIFEST_SEGMENTS {
            return Err(TraceSinkError::Unavailable);
        }
        let manifest_bytes = u64::try_from(EPOCH_MANIFEST_FIXED_LEN)
            .ok()
            .and_then(|fixed| {
                u64::try_from(EPOCH_MANIFEST_ENTRY_LEN)
                    .ok()?
                    .checked_mul(u64::from(segment_count))?
                    .checked_add(fixed)
            })
            .ok_or(TraceSinkError::Unavailable)?;
        let terminal_bytes = manifest_bytes
            .checked_add(if state.active.is_some() { footer } else { 0 })
            .ok_or(TraceSinkError::Unavailable)?;
        let normal_bytes = live_key
            .checked_add(header)
            .and_then(|bytes| bytes.checked_add(frame))
            .ok_or(TraceSinkError::Unavailable)?;
        Ok(StorageTransition {
            terminal_bytes,
            released_bytes: live_key,
            normal_bytes,
        })
    }

    fn ensure_writer_locked(
        &self,
        state: &mut RuntimeState,
        timestamp_ms: u64,
        resolved: &ResolvedTraceKey,
    ) -> Result<(), TraceSinkError> {
        if state
            .live
            .as_ref()
            .is_some_and(|live| live.key_id != resolved.key_id)
        {
            if state
                .live
                .as_ref()
                .is_some_and(|live| live.key_id.epoch_start_ms > resolved.key_id.epoch_start_ms)
            {
                // Concurrent callers can arrive out of timestamp order at midnight. Never reopen
                // a destroyed prior epoch or overwrite its segment namespace; reject that request
                // before its protected mutation instead.
                return Err(TraceSinkError::Unavailable);
            }
            let closing_epoch_end = epoch_end_ms(
                &state
                    .live
                    .as_ref()
                    .ok_or(TraceSinkError::Unavailable)?
                    .key_id,
            )?;
            if timestamp_ms < closing_epoch_end
                || resolved.key_id.epoch_start_ms < closing_epoch_end
            {
                // A key-generation change inside an open day is not a terminal epoch. Refuse it
                // instead of publishing a misleading completeness marker.
                return Err(TraceSinkError::Unavailable);
            }
            self.finalize_active_locked(state, closing_epoch_end.saturating_sub(1))?;
            self.publish_terminal_manifest_locked(state)?;
            self.destroy_live_key_locked(state)?;
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
            self.storage_budget
                .charge_normal(LIVE_KEY_LEN as u64)
                .map_err(|_| TraceSinkError::Capacity)?;
            self.persist_live_key(&live)?;
            state.live = Some(live);
        }
        if state.active.is_none() {
            let live = state.live.as_ref().ok_or(TraceSinkError::Unavailable)?;
            if live.key_id != resolved.key_id {
                return Err(TraceSinkError::Unavailable);
            }
            let path = segment_path(&self.config.directory, &live.key_id, live.segment_index)?;
            self.storage_budget
                .charge_normal(SEGMENT_HEADER_LEN as u64)
                .map_err(|_| TraceSinkError::Capacity)?;
            let writer = SegmentWriter::create(
                &path,
                EpochSealingKey::from_bytes(live.key_id, live.secret)
                    .map_err(|_| TraceSinkError::Unavailable)?,
                &resolved.public_key,
                self.signer.verifying_key().to_bytes(),
                timestamp_ms,
                self.config.max_records_per_segment,
            )
            .map_err(|_| TraceSinkError::Unavailable)?;
            self.catalog
                .record_trace_segment(&open_metadata(writer.header(), &path)?)
                .map_err(|_| TraceSinkError::Unavailable)?;
            state.active = Some(ActiveSegment {
                writer,
                last_record_ms: timestamp_ms,
            });
        }
        Ok(())
    }

    fn finalize_active_locked(
        &self,
        state: &mut RuntimeState,
        requested_close_ms: u64,
    ) -> Result<(), TraceSinkError> {
        let Some(active) = state.active.take() else {
            return Ok(());
        };
        let key_id = active.writer.header().key_id();
        let header = active.writer.header().clone();
        let segment_index = state
            .live
            .as_ref()
            .ok_or(TraceSinkError::Unavailable)?
            .segment_index;
        let path = segment_path(&self.config.directory, &key_id, segment_index)?;
        let close_ms = requested_close_ms
            .max(active.writer.header().opened_at_ms())
            .max(active.last_record_ms)
            .min(epoch_end_ms(&key_id)?.saturating_sub(1));
        self.storage_budget
            .charge_terminal(SEGMENT_FOOTER_LEN as u64)
            .map_err(|_| TraceSinkError::Unavailable)?;
        let footer = active
            .writer
            .finalize_durable(close_ms, &self.signer)
            .map_err(|_| TraceSinkError::Unavailable)?;
        self.catalog
            .record_trace_segment(&closed_metadata(&header, &footer, &path)?)
            .map_err(|_| TraceSinkError::Unavailable)?;
        let live = state.live.as_mut().ok_or(TraceSinkError::Unavailable)?;
        live.segment_index = live
            .segment_index
            .checked_add(1)
            .ok_or(TraceSinkError::Unavailable)?;
        self.persist_live_key(live)
    }

    fn destroy_live_key_locked(&self, state: &mut RuntimeState) -> Result<(), TraceSinkError> {
        if state.active.is_some() || state.live.is_none() {
            return Err(TraceSinkError::Unavailable);
        }
        #[cfg(unix)]
        {
            let name = live_key_name()?;
            let opened = self
                .directory
                .open_file(
                    &name,
                    OpenAccess::ReadOnly,
                    FilePolicy::private_exact(LIVE_KEY_LEN as u64),
                )
                .map_err(map_custody_error)?;
            self.directory
                .unlink_file(opened)
                .map_err(map_custody_error)?;
        }
        #[cfg(not(unix))]
        {
            let path = self.live_key_path();
            let _opened_key = open_live_key(&path)?.ok_or(TraceSinkError::Unavailable)?;
            fs::remove_file(&path).map_err(|_| TraceSinkError::Unavailable)?;
            sync_parent(&self.config.directory)?;
        }
        state.live = None;
        self.storage_budget
            .release(LIVE_KEY_LEN as u64)
            .map_err(|_| TraceSinkError::Unavailable)?;
        Ok(())
    }

    fn publish_terminal_manifest_locked(&self, state: &RuntimeState) -> Result<(), TraceSinkError> {
        if state.active.is_some() {
            return Err(TraceSinkError::Unavailable);
        }
        let live = state.live.as_ref().ok_or(TraceSinkError::Unavailable)?;
        if live.segment_index > MAX_EPOCH_MANIFEST_SEGMENTS {
            return Err(TraceSinkError::Unavailable);
        }
        let expected_signer = self.signer.verifying_key().to_bytes();
        let expected_commitment: [u8; 32] = Sha256::digest(live.secret).into();
        let mut custody_digest = None;
        let mut segments = Vec::with_capacity(live.segment_index as usize);
        for index in 0..live.segment_index {
            let path = segment_path(&self.config.directory, &live.key_id, index)?;
            let segment = verify_segment(path).map_err(|_| TraceSinkError::Unavailable)?;
            if segment.header.key_id() != live.key_id
                || segment.header.signer_public_key() != expected_signer
                || segment.header.wrapped_epoch_key().epoch_key_commitment() != expected_commitment
            {
                return Err(TraceSinkError::Unavailable);
            }
            let digest = segment.header.wrapped_epoch_key().compliance_key_digest();
            if custody_digest.is_some_and(|expected| expected != digest) {
                return Err(TraceSinkError::Unavailable);
            }
            custody_digest = Some(digest);
            segments.push(
                EpochSegmentEntry::from_verified(index, &segment)
                    .map_err(|_| TraceSinkError::Unavailable)?,
            );
        }
        let custody_digest = match custody_digest {
            Some(digest) => digest,
            None => {
                // This only occurs if a process died after persisting the live key but before
                // opening its first segment. Resolve the witnessed historical key rather than
                // inventing custody metadata, and retain the live key if it is unavailable.
                let resolved = self
                    .resolver
                    .resolve_trace_key(
                        CompliancePurpose::NetworkTrace,
                        self.config.jurisdiction,
                        live.key_id.epoch_start_ms,
                    )
                    .map_err(|_| TraceSinkError::Unavailable)?
                    .ok_or(TraceSinkError::Unavailable)?;
                validate_resolved_key(
                    &resolved,
                    self.config.jurisdiction,
                    live.key_id.epoch_start_ms,
                )?;
                if resolved.key_id != live.key_id {
                    return Err(TraceSinkError::Unavailable);
                }
                Sha256::digest(resolved.public_key).into()
            }
        };
        let manifest = EpochManifest::new_signed(
            live.key_id,
            self.config.node_id,
            custody_digest,
            expected_commitment,
            segments,
            &self.signer,
        )
        .map_err(|_| TraceSinkError::Unavailable)?;
        let path = epoch_manifest_path(&self.config.directory, &live.key_id)
            .map_err(|_| TraceSinkError::Unavailable)?;
        if !self.entry_exists(&path)? {
            let manifest_bytes = manifest.encode().map_err(|_| TraceSinkError::Unavailable)?;
            self.storage_budget
                .charge_terminal(u64::try_from(manifest_bytes.len()).unwrap_or(u64::MAX))
                .map_err(|_| TraceSinkError::Unavailable)?;
        }
        publish_epoch_manifest(path, &manifest).map_err(|_| TraceSinkError::Unavailable)
    }

    #[cfg(not(unix))]
    fn live_key_path(&self) -> PathBuf {
        self.config.directory.join(LIVE_KEY_NAME)
    }

    fn load_live_key(&self) -> Result<Option<LiveKeyState>, TraceSinkError> {
        #[cfg(unix)]
        {
            load_live_key_guarded(&self.directory)
        }
        #[cfg(not(unix))]
        {
            load_live_key(&self.live_key_path())
        }
    }

    fn persist_live_key(&self, live: &LiveKeyState) -> Result<(), TraceSinkError> {
        #[cfg(unix)]
        {
            persist_live_key_guarded(&self.directory, live)
        }
        #[cfg(not(unix))]
        {
            persist_live_key(&self.live_key_path(), live)
        }
    }

    fn entry_exists(&self, path: &Path) -> Result<bool, TraceSinkError> {
        #[cfg(unix)]
        {
            let name = guarded_leaf(&self.directory, path)?;
            self.directory.verify_named().map_err(map_custody_error)?;
            self.directory
                .entry_metadata(&name)
                .map(|metadata| metadata.is_some())
                .map_err(map_custody_error)
        }
        #[cfg(not(unix))]
        {
            Ok(path.exists())
        }
    }

    fn next_rollover_deadline(&self) -> Result<Option<u64>, TraceSinkError> {
        let state = self.state.lock().unwrap_or_else(|error| error.into_inner());
        if state.poisoned || (state.live.is_none() && state.active.is_some()) {
            return Err(TraceSinkError::Unavailable);
        }
        state
            .live
            .as_ref()
            .map(|live| epoch_end_ms(&live.key_id))
            .transpose()
    }

    /// Close an expired epoch from the writer thread even when no post-boundary capture arrives.
    /// This is intentionally owned by the same thread as append/finalize so rollover cannot race a
    /// durable acknowledgement or coordinated shutdown.
    fn rollover_expired(&self, current_ms: u64) -> Result<(), TraceSinkError> {
        self.assert_writer_lease()?;
        let mut state = self.state.lock().unwrap_or_else(|error| error.into_inner());
        if state.poisoned {
            return Err(TraceSinkError::Unavailable);
        }
        let Some(epoch_end) = state
            .live
            .as_ref()
            .map(|live| epoch_end_ms(&live.key_id))
            .transpose()?
        else {
            if state.active.is_some() {
                state.poisoned = true;
                return Err(TraceSinkError::Unavailable);
            }
            return Ok(());
        };
        if current_ms < epoch_end {
            return Ok(());
        }
        let result = (|| {
            self.finalize_active_locked(&mut state, epoch_end.saturating_sub(1))?;
            self.publish_terminal_manifest_locked(&state)?;
            self.destroy_live_key_locked(&mut state)
        })();
        if result.is_err() {
            state.poisoned = true;
        }
        result
    }

    fn shutdown(&self, timestamp_ms: u64) -> Result<(), TraceSinkError> {
        self.assert_writer_lease()?;
        let mut state = self.state.lock().unwrap_or_else(|error| error.into_inner());
        if state.poisoned {
            return Err(TraceSinkError::Unavailable);
        }
        self.finalize_active_locked(&mut state, timestamp_ms)?;
        if state
            .live
            .as_ref()
            .is_some_and(|live| timestamp_ms >= epoch_end_ms(&live.key_id).unwrap_or(u64::MAX))
        {
            self.publish_terminal_manifest_locked(&state)?;
            self.destroy_live_key_locked(&mut state)?;
        }
        Ok(())
    }

    fn assert_writer_lease(&self) -> Result<(), TraceSinkError> {
        self.writer_lease
            .assert_stable()
            .map_err(|_| TraceSinkError::Unavailable)
    }
}

fn receive_next_command(
    inner: &SealedTraceInner,
    receiver: &mpsc::Receiver<TraceCommand>,
    options: WorkerOptions,
) -> Result<TraceCommand, ()> {
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
    inner: Arc<SealedTraceInner>,
    receiver: mpsc::Receiver<TraceCommand>,
    status: &WorkerStatus,
    options: WorkerOptions,
) -> Result<(), ()> {
    loop {
        let first = receive_next_command(&inner, &receiver, options)?;
        match first {
            TraceCommand::Capture(first) => {
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
                        Ok(TraceCommand::Capture(command)) => batch.push(command),
                        Ok(TraceCommand::Shutdown {
                            timestamp_ms,
                            acknowledged,
                        }) => {
                            shutdown = Some((timestamp_ms, acknowledged));
                            break;
                        }
                        #[cfg(test)]
                        Ok(TraceCommand::Crash) => return Err(()),
                        Err(mpsc::RecvTimeoutError::Timeout) => break,
                        Err(mpsc::RecvTimeoutError::Disconnected) => return Err(()),
                    }
                }

                match process_batch(&inner, &batch, status) {
                    Ok(()) => {}
                    Err(TraceSinkError::Capacity) => {
                        if inner.sync_partial_batch_after_capacity().is_err() {
                            status.poison();
                            for command in batch {
                                let _ = command.acknowledged.send(Err(TraceSinkError::Unavailable));
                            }
                            if let Some((_, acknowledged)) = shutdown {
                                let _ = acknowledged.send(Err(TraceSinkError::Unavailable));
                            }
                            return Err(());
                        }
                        for command in batch {
                            let _ = command.acknowledged.send(Err(TraceSinkError::Capacity));
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
                        continue;
                    }
                    Err(_) => {
                        status.poison();
                        for command in batch {
                            let _ = command.acknowledged.send(Err(TraceSinkError::Unavailable));
                        }
                        if let Some((_, acknowledged)) = shutdown {
                            let _ = acknowledged.send(Err(TraceSinkError::Unavailable));
                        }
                        return Err(());
                    }
                }
                // This is the only success-acknowledgement site. `process_batch` has already
                // synced the exact frames represented by every command in this vector.
                for command in batch {
                    let _ = command.acknowledged.send(Ok(()));
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
            TraceCommand::Shutdown {
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
            TraceCommand::Crash => return Err(()),
        }
    }
}

fn process_batch(
    inner: &SealedTraceInner,
    batch: &[CaptureCommand],
    _status: &WorkerStatus,
) -> Result<(), TraceSinkError> {
    let mut appended = false;
    for command in batch {
        appended |= inner.capture_buffered(command.input)?;
    }
    if !appended {
        return Ok(());
    }
    #[cfg(test)]
    if _status.panic_before_sync.swap(false, Ordering::AcqRel) {
        panic!("injected trace worker panic before durable sync");
    }
    #[cfg(test)]
    if _status.fail_before_sync.swap(false, Ordering::AcqRel) {
        return Err(TraceSinkError::Unavailable);
    }
    inner.sync_active()?;
    #[cfg(test)]
    _status.durable_sync_batches.fetch_add(1, Ordering::AcqRel);
    Ok(())
}

impl TraceSink for SealedTraceSink {
    fn readiness(&self) -> Result<(), TraceSinkError> {
        self.readiness_at(now_ms())
    }

    fn capacity_contract(&self) -> Option<TraceCapacity> {
        Some(TraceCapacity {
            policy: TraceRetentionPolicy {
                jurisdiction: self.inner.config.jurisdiction,
                capture: self.inner.config.capture_policy,
                retention_days: self.inner.config.retention_days,
            },
            records_per_minute: self.inner.config.planned_records_per_minute,
            utc_epochs: self.inner.config.capacity_utc_epochs,
            max_records_per_segment: self.inner.config.max_records_per_segment,
            logical_limit_bytes: self.inner.config.max_storage_bytes,
        })
    }

    fn capture(&self, input: TraceInput) -> Result<(), TraceSinkError> {
        self.capture_inner(input, None)
    }

    fn shutdown(&self, timestamp_ms: u64) -> Result<(), TraceSinkError> {
        self.shutdown_worker(timestamp_ms)
    }
}

impl Drop for SealedTraceSink {
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

fn trace_record(
    config: &SealedTraceConfig,
    input: TraceInput,
) -> Result<TraceRecord, TraceSinkError> {
    let (operation, event_id, recipient, owner, size_bytes, correlation_id) = match input.operation
    {
        TraceOperation::Publish {
            event_id,
            recipient,
            size_bytes,
        } => (
            NetworkOperation::Publish,
            Some(event_id),
            Some(recipient),
            None,
            size_bytes,
            None,
        ),
        TraceOperation::Fetch { owner } => {
            (NetworkOperation::Fetch, None, None, Some(owner), 0, None)
        }
        TraceOperation::PutAgent => (NetworkOperation::PutAgent, None, None, None, 0, None),
        TraceOperation::Claim { correlation_id } => (
            NetworkOperation::Claim,
            None,
            None,
            None,
            0,
            Some(correlation_id),
        ),
    };
    let record = TraceRecord {
        jurisdiction: config.jurisdiction,
        operation,
        timestamp_ms: input.timestamp_ms,
        node_id: config.node_id,
        source_ip: TraceIp::from(input.connected_source.ip()),
        source_port: input.connected_source.port(),
        event_id,
        recipient,
        owner,
        size_bytes,
        correlation_id,
    };
    record.encode().map_err(|_| TraceSinkError::Unavailable)?;
    Ok(record)
}

#[cfg(test)]
std::thread_local! {
    static CONFIG_VALIDATION_CALLS: std::cell::Cell<u32> = const { std::cell::Cell::new(0) };
}

fn validate_config(config: &SealedTraceConfig) -> Result<(), TraceSinkError> {
    #[cfg(test)]
    CONFIG_VALIDATION_CALLS.with(|calls| calls.set(calls.get() + 1));
    if config.directory.as_os_str().is_empty()
        || config.node_id == [0u8; 32]
        || config.planned_records_per_minute == 0
        || config.capacity_utc_epochs == 0
        || config.max_records_per_segment == 0
        || config.max_records_per_segment > MAX_SEGMENT_RECORDS
    {
        return Err(TraceSinkError::Unavailable);
    }
    let required_epochs = TraceRetentionPolicy {
        jurisdiction: config.jurisdiction,
        capture: config.capture_policy,
        retention_days: config.retention_days,
    }
    .required_capacity_epochs()
    .map_err(|_| TraceSinkError::Unavailable)?;
    if config.capacity_utc_epochs < required_epochs {
        return Err(TraceSinkError::Unavailable);
    }
    Ok(())
}

fn require_supported_persistent_trace_platform(
    platform: PersistentTracePlatform,
) -> Result<(), TraceSinkError> {
    if platform.supported_persistent_target {
        Ok(())
    } else {
        Err(TraceSinkError::Unavailable)
    }
}

fn validate_capacity_plan(config: &SealedTraceConfig) -> Result<(), TraceSinkError> {
    let required_bytes = required_network_trace_storage_bytes(
        config.planned_records_per_minute,
        config.capacity_utc_epochs,
        config.max_records_per_segment,
    )
    .map_err(|_| TraceSinkError::Unavailable)?;
    if required_bytes > config.max_storage_bytes {
        return Err(TraceSinkError::Unavailable);
    }
    Ok(())
}

fn validate_resolved_key(
    resolved: &ResolvedTraceKey,
    jurisdiction: Jurisdiction,
    timestamp_ms: u64,
) -> Result<(), TraceSinkError> {
    let epoch_start = day_start_ms(timestamp_ms);
    if resolved.key_id.purpose != CompliancePurpose::NetworkTrace
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
        return Err(TraceSinkError::Unavailable);
    }
    Ok(())
}

fn validate_live_key(
    config: &SealedTraceConfig,
    live: &LiveKeyState,
) -> Result<(), TraceSinkError> {
    if live.key_id.purpose != CompliancePurpose::NetworkTrace
        || live.key_id.jurisdiction != config.jurisdiction
        || canonical_trace_epoch_end_ms(&live.key_id).is_err()
        || live.secret == [0u8; 32]
    {
        return Err(TraceSinkError::Unavailable);
    }
    Ok(())
}

fn day_start_ms(timestamp_ms: u64) -> u64 {
    timestamp_ms - (timestamp_ms % TRACE_EPOCH_DURATION_MS)
}

fn epoch_end_ms(key_id: &ComplianceKeyId) -> Result<u64, TraceSinkError> {
    canonical_trace_epoch_end_ms(key_id).map_err(|_| TraceSinkError::Unavailable)
}

fn segment_path(
    directory: &Path,
    key_id: &ComplianceKeyId,
    segment_index: u32,
) -> Result<PathBuf, TraceSinkError> {
    let encoded = key_id.encode().map_err(|_| TraceSinkError::Unavailable)?;
    Ok(directory.join(format!(
        "network-{}-{segment_index:08}.pptrace",
        hex(&encoded)
    )))
}

fn open_path_for(final_path: &Path) -> PathBuf {
    let mut name = final_path.as_os_str().to_os_string();
    name.push(".open");
    PathBuf::from(name)
}

fn open_metadata(
    header: &SegmentHeader,
    final_path: &Path,
) -> Result<TraceSegmentMetadata, TraceSinkError> {
    segment_metadata(header, None, final_path)
}

fn closed_metadata(
    header: &SegmentHeader,
    footer: &SegmentFooter,
    final_path: &Path,
) -> Result<TraceSegmentMetadata, TraceSinkError> {
    segment_metadata(header, Some(footer), final_path)
}

fn segment_metadata(
    header: &SegmentHeader,
    footer: Option<&SegmentFooter>,
    final_path: &Path,
) -> Result<TraceSegmentMetadata, TraceSinkError> {
    let relative_path = final_path
        .file_name()
        .and_then(|name| name.to_str())
        .filter(|name| !name.is_empty())
        .ok_or(TraceSinkError::Unavailable)?
        .to_owned();
    let wrapped_key = header
        .wrapped_epoch_key()
        .encode()
        .map_err(|_| TraceSinkError::Unavailable)?;
    Ok(TraceSegmentMetadata {
        segment_id: header.segment_id(),
        key_id: header.key_id(),
        opened_at_ms: header.opened_at_ms(),
        closed_at_ms: footer.map(SegmentFooter::closed_at_ms),
        relative_path,
        wrapped_key,
        record_count: footer.map(SegmentFooter::record_count),
        first_hash: footer.map(SegmentFooter::first_record_hash),
        final_hash: footer.map(SegmentFooter::final_chain_hash),
        state: if footer.is_some() {
            TraceSegmentState::Closed
        } else {
            TraceSegmentState::Open
        },
    })
}

#[cfg(unix)]
fn live_key_name() -> Result<LeafName, TraceSinkError> {
    LeafName::new(LIVE_KEY_NAME).map_err(map_custody_error)
}

#[cfg(unix)]
fn guarded_leaf(directory: &GuardedDir, path: &Path) -> Result<LeafName, TraceSinkError> {
    if path.parent() != Some(directory.absolute_path()) {
        return Err(TraceSinkError::Unavailable);
    }
    path.file_name()
        .ok_or(TraceSinkError::Unavailable)
        .and_then(|name| LeafName::new(name).map_err(map_custody_error))
}

#[cfg(unix)]
fn load_live_key_guarded(directory: &GuardedDir) -> Result<Option<LiveKeyState>, TraceSinkError> {
    directory.verify_named().map_err(map_custody_error)?;
    let name = live_key_name()?;
    let Some(mut file) = directory
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
        .map_err(|_| TraceSinkError::Unavailable)?;
    let final_metadata = file.metadata().map_err(map_custody_error)?;
    if bytes.len() != LIVE_KEY_LEN || opened != final_metadata {
        bytes.zeroize();
        return Err(TraceSinkError::Unavailable);
    }
    if file.verify_named().is_err() {
        bytes.zeroize();
        return Err(TraceSinkError::Unavailable);
    }
    decode_live_key(&mut bytes)
}

#[cfg(all(test, unix))]
fn load_live_key_at(path: &Path) -> Result<Option<LiveKeyState>, TraceSinkError> {
    let parent = path.parent().ok_or(TraceSinkError::Unavailable)?;
    let directory = GuardedDir::open_existing(
        parent,
        pigeonpost_unix_custody::DirPolicy::private_mutable(),
    )
    .map_err(map_custody_error)?;
    if path.file_name() != Some(std::ffi::OsStr::new(LIVE_KEY_NAME)) {
        return Err(TraceSinkError::Unavailable);
    }
    load_live_key_guarded(&directory)
}

#[cfg(all(test, not(unix)))]
fn load_live_key_at(path: &Path) -> Result<Option<LiveKeyState>, TraceSinkError> {
    load_live_key(path)
}

#[cfg(not(unix))]
fn load_live_key(path: &Path) -> Result<Option<LiveKeyState>, TraceSinkError> {
    let Some(file) = open_live_key(path)? else {
        return Ok(None);
    };
    let mut bytes = Vec::with_capacity(LIVE_KEY_LEN + 1);
    (&file)
        .take((LIVE_KEY_LEN + 1) as u64)
        .read_to_end(&mut bytes)
        .map_err(|_| TraceSinkError::Unavailable)?;
    if !named_live_key_matches(&file, path) {
        bytes.zeroize();
        return Err(TraceSinkError::Unavailable);
    }
    decode_live_key(&mut bytes)
}

fn decode_live_key(bytes: &mut Vec<u8>) -> Result<Option<LiveKeyState>, TraceSinkError> {
    let decoded = (|| {
        if bytes.len() != LIVE_KEY_LEN
            || &bytes[..8] != LIVE_KEY_MAGIC
            || bytes[8] != LIVE_KEY_VERSION
        {
            return Err(TraceSinkError::Unavailable);
        }
        let key_id = ComplianceKeyId::decode(&bytes[9..9 + COMPLIANCE_KEY_ID_LEN])
            .map_err(|_| TraceSinkError::Unavailable)?;
        let cursor = 9 + COMPLIANCE_KEY_ID_LEN;
        let segment_index = u32::from_be_bytes(
            bytes[cursor..cursor + 4]
                .try_into()
                .map_err(|_| TraceSinkError::Unavailable)?,
        );
        let mut secret = [0u8; 32];
        secret.copy_from_slice(&bytes[cursor + 4..]);
        if secret == [0u8; 32] {
            return Err(TraceSinkError::Unavailable);
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
fn open_live_key(path: &Path) -> Result<Option<File>, TraceSinkError> {
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
        Err(_) => return Err(TraceSinkError::Unavailable),
    };
    let metadata = file.metadata().map_err(|_| TraceSinkError::Unavailable)?;
    if !metadata.is_file()
        || metadata.len() != LIVE_KEY_LEN as u64
        || metadata.file_attributes() & FILE_ATTRIBUTE_REPARSE_POINT != 0
    {
        return Err(TraceSinkError::Unavailable);
    }
    Ok(Some(file))
}

#[cfg(not(any(unix, windows)))]
fn open_live_key(path: &Path) -> Result<Option<File>, TraceSinkError> {
    let metadata = match fs::symlink_metadata(path) {
        Ok(metadata) => metadata,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(_) => return Err(TraceSinkError::Unavailable),
    };
    if metadata.file_type().is_symlink()
        || !metadata.is_file()
        || metadata.len() != LIVE_KEY_LEN as u64
    {
        return Err(TraceSinkError::Unavailable);
    }
    File::open(path)
        .map(Some)
        .map_err(|_| TraceSinkError::Unavailable)
}

#[cfg(not(unix))]
fn named_live_key_matches(file: &File, path: &Path) -> bool {
    file.metadata().is_ok_and(|metadata| metadata.is_file())
        && fs::symlink_metadata(path)
            .is_ok_and(|metadata| !metadata.file_type().is_symlink() && metadata.is_file())
}

fn encode_live_key(live: &LiveKeyState) -> Result<[u8; LIVE_KEY_LEN], TraceSinkError> {
    let mut bytes = [0u8; LIVE_KEY_LEN];
    bytes[..8].copy_from_slice(LIVE_KEY_MAGIC);
    bytes[8] = LIVE_KEY_VERSION;
    bytes[9..9 + COMPLIANCE_KEY_ID_LEN].copy_from_slice(
        &live
            .key_id
            .encode()
            .map_err(|_| TraceSinkError::Unavailable)?,
    );
    let cursor = 9 + COMPLIANCE_KEY_ID_LEN;
    bytes[cursor..cursor + 4].copy_from_slice(&live.segment_index.to_be_bytes());
    bytes[cursor + 4..].copy_from_slice(&live.secret);
    Ok(bytes)
}

#[cfg(unix)]
fn persist_live_key_guarded(
    directory: &GuardedDir,
    live: &LiveKeyState,
) -> Result<(), TraceSinkError> {
    let mut bytes = encode_live_key(live)?;
    let result = (|| {
        directory.verify_named().map_err(map_custody_error)?;
        let destination = live_key_name()?;
        let (temp_name, mut temporary) = create_live_key_temp(directory)?;
        let cleanup = match directory.open_file(
            &temp_name,
            OpenAccess::ReadOnly,
            FilePolicy::private(LIVE_KEY_LEN as u64),
        ) {
            Ok(cleanup) => cleanup,
            Err(error) => {
                let _ = directory.unlink_file(temporary);
                return Err(map_custody_error(error));
            }
        };
        let publication = (|| {
            temporary
                .write_all(&bytes)
                .map_err(|_| TraceSinkError::Unavailable)?;
            temporary.sync_all().map_err(map_custody_error)?;
            if temporary.metadata().map_err(map_custody_error)?.len != LIVE_KEY_LEN as u64 {
                return Err(TraceSinkError::Unavailable);
            }
            temporary.verify_named().map_err(map_custody_error)?;
            let existing = directory
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
                Some(_) => directory.rename_replace(temporary, directory, &destination),
                None => directory.publish_no_replace(temporary, directory, &destination),
            }
            .map_err(map_custody_error)?;
            if published.metadata().map_err(map_custody_error)?.len != LIVE_KEY_LEN as u64 {
                return Err(TraceSinkError::Unavailable);
            }
            published.verify_named().map_err(map_custody_error)?;
            let reopened = directory
                .open_file(
                    &destination,
                    OpenAccess::ReadOnly,
                    FilePolicy::private_exact(LIVE_KEY_LEN as u64),
                )
                .map_err(map_custody_error)?;
            if reopened.identity() != published.identity() {
                return Err(TraceSinkError::Unavailable);
            }
            reopened.verify_named().map_err(map_custody_error)
        })();
        if publication.is_err() {
            let _ = directory.unlink_file(cleanup);
        }
        publication
    })();
    bytes.zeroize();
    result
}

#[cfg(unix)]
fn create_live_key_temp(directory: &GuardedDir) -> Result<(LeafName, GuardedFile), TraceSinkError> {
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
    Err(TraceSinkError::Unavailable)
}

#[cfg(unix)]
fn map_custody_error(_error: CustodyError) -> TraceSinkError {
    TraceSinkError::Unavailable
}

#[cfg(not(unix))]
fn persist_live_key(path: &Path, live: &LiveKeyState) -> Result<(), TraceSinkError> {
    let mut bytes = encode_live_key(live)?;
    let parent = path.parent().ok_or(TraceSinkError::Unavailable)?;
    let temp = parent.join(format!(
        ".live-key.{}.{}.tmp",
        std::process::id(),
        TEMP_COUNTER.fetch_add(1, Ordering::Relaxed)
    ));
    let mut options = OpenOptions::new();
    options.write(true).create_new(true);
    let mut file = options
        .open(&temp)
        .map_err(|_| TraceSinkError::Unavailable)?;
    let result = (|| {
        file.write_all(&bytes)?;
        file.sync_all()?;
        match open_live_key(path) {
            Ok(Some(_)) | Ok(None) => {}
            Err(_) => return Err(std::io::Error::other("unsafe live key path")),
        }
        fs::rename(&temp, path)?;
        if !named_live_key_matches(&file, path) {
            return Err(std::io::Error::other("live key changed during publication"));
        }
        File::open(parent)?.sync_all()
    })();
    bytes.zeroize();
    if result.is_err() {
        let _ = fs::remove_file(&temp);
        return Err(TraceSinkError::Unavailable);
    }
    Ok(())
}

#[cfg(unix)]
fn secure_directory(path: &Path) -> Result<GuardedDir, TraceSinkError> {
    GuardedDir::create_private(path).map_err(map_custody_error)
}

#[cfg(not(unix))]
fn secure_directory(path: &Path) -> Result<(), TraceSinkError> {
    fs::create_dir_all(path).map_err(|_| TraceSinkError::Unavailable)?;
    let metadata = fs::symlink_metadata(path).map_err(|_| TraceSinkError::Unavailable)?;
    if metadata.file_type().is_symlink() || !metadata.is_dir() {
        return Err(TraceSinkError::Unavailable);
    }
    Ok(())
}

#[cfg(not(unix))]
fn sync_parent(path: &Path) -> Result<(), TraceSinkError> {
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
    use crate::store::SqliteStore;

    fn private_test_directory(dir: &tempfile::TempDir, name: &str) -> PathBuf {
        let path = dir.path().join(name);
        fs::create_dir(&path).unwrap();
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            fs::set_permissions(&path, fs::Permissions::from_mode(0o700)).unwrap();
        }
        path
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
    struct FixedTraceKey {
        key: ResolvedTraceKey,
    }

    impl TraceKeyResolver for FixedTraceKey {
        fn readiness(&self, _now_ms: u64) -> Result<(), AttributionResolutionError> {
            Ok(())
        }

        fn resolve_trace_key(
            &self,
            purpose: CompliancePurpose,
            jurisdiction: Jurisdiction,
            at_ms: u64,
        ) -> Result<Option<ResolvedTraceKey>, AttributionResolutionError> {
            Ok((self.key.key_id.purpose == purpose
                && self.key.key_id.jurisdiction == jurisdiction
                && self.key.not_before_ms <= at_ms
                && at_ms < self.key.not_after_ms)
                .then_some(self.key))
        }
    }

    fn fixture(
        dir: &tempfile::TempDir,
    ) -> (
        SealedTraceConfig,
        Arc<dyn TraceKeyResolver>,
        Arc<SqliteStore>,
        [u8; 32],
        u64,
    ) {
        let timestamp = now_ms();
        let start = day_start_ms(timestamp);
        let key_id = ComplianceKeyId::new(
            CompliancePurpose::NetworkTrace,
            Jurisdiction::Test,
            [3; 32],
            start,
            1,
        );
        let resolver: Arc<dyn TraceKeyResolver> = Arc::new(FixedTraceKey {
            key: ResolvedTraceKey {
                key_id,
                public_key: [9; 32],
                not_before_ms: start,
                not_after_ms: start + TRACE_EPOCH_DURATION_MS,
            },
        });
        (
            SealedTraceConfig {
                directory: private_test_directory(dir, "trace"),
                jurisdiction: Jurisdiction::Test,
                node_id: [5; 32],
                capture_policy: CapturePolicy::Standing,
                retention_days: None,
                planned_records_per_minute: 1,
                capacity_utc_epochs: 1,
                max_records_per_segment: 10,
                max_storage_bytes: DEFAULT_TRACE_STORAGE_BYTES,
            },
            resolver,
            Arc::new(SqliteStore::in_memory().unwrap()),
            [7; 32],
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
        let (mut config, resolver, catalog, _, _) = fixture(&root);
        let requested_parent = root.path().join("unsupported-trace");
        config.directory = requested_parent.join("segments");
        config.node_id = [0; 32];
        config.planned_records_per_minute = 0;
        CONFIG_VALIDATION_CALLS.with(|calls| calls.set(0));

        assert!(matches!(
            SealedTraceSink::new_with_options_for_platform(
                PersistentTracePlatform::unsupported_for_test(),
                config,
                resolver,
                catalog,
                [0; 32],
                WorkerOptions {
                    queue_capacity: 0,
                    batch_max: 0,
                    rollover_clock_poll: Duration::ZERO,
                    clock: WorkerClock::Panic,
                    ..WorkerOptions::default()
                },
            ),
            Err(TraceSinkError::Unavailable)
        ));
        assert_eq!(CONFIG_VALIDATION_CALLS.with(|calls| calls.get()), 0);
        assert!(!requested_parent.exists());
    }

    fn new_without_capacity_plan_for_boundary_test(
        config: SealedTraceConfig,
        resolver: Arc<dyn TraceKeyResolver>,
        catalog: Arc<dyn TraceSegmentCatalog>,
        seed: [u8; 32],
    ) -> SealedTraceSink {
        SealedTraceSink::new_with_options(
            config,
            resolver,
            catalog,
            seed,
            WorkerOptions {
                enforce_capacity_plan: false,
                ..WorkerOptions::default()
            },
        )
        .unwrap()
    }

    fn publish(timestamp_ms: u64, event: u8) -> TraceInput {
        TraceInput {
            timestamp_ms,
            connected_source: "192.0.2.10:4567".parse().unwrap(),
            operation: TraceOperation::Publish {
                event_id: [event; 32],
                recipient: [6; 32],
                size_bytes: 512,
            },
        }
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
    fn capture_is_sealed_and_publicly_verifiable() {
        let dir = tempfile::tempdir().unwrap();
        let (config, resolver, store, seed, timestamp) = fixture(&dir);
        let catalog: Arc<dyn TraceSegmentCatalog> = store.clone();
        let sink = SealedTraceSink::new(config.clone(), resolver, catalog, seed).unwrap();
        sink.capture(publish(timestamp, 1)).unwrap();
        sink.shutdown(timestamp).unwrap();

        let live = load_live_key_at(&config.directory.join(LIVE_KEY_NAME))
            .unwrap()
            .unwrap();
        let path = segment_path(&config.directory, &live.key_id, 0).unwrap();
        let verified = verify_segment(path).unwrap();
        assert_eq!(verified.footer.record_count(), 1);
        assert_eq!(verified.header.key_id(), live.key_id);
        assert_eq!(
            store.trace_segment_state(&verified.header.segment_id()),
            Some((i64::from(TraceSegmentState::Closed as u8), Some(1)))
        );
    }

    #[cfg_attr(
        not(any(target_os = "linux", target_os = "macos")),
        ignore = "persistent regulatory trace is supported only on Linux and macOS"
    )]
    #[test]
    fn storage_exhaustion_refuses_capture_but_preserves_terminal_shutdown_reserve() {
        let dir = tempfile::tempdir().unwrap();
        let (mut config, resolver, store, seed, timestamp) = fixture(&dir);
        config.max_storage_bytes = pigeonpost_compliance_seal::MIN_TRACE_STORAGE_BYTES;
        let catalog: Arc<dyn TraceSegmentCatalog> = store.clone();
        let sink = new_without_capacity_plan_for_boundary_test(
            config.clone(),
            Arc::clone(&resolver),
            catalog,
            seed,
        );

        sink.capture(publish(timestamp, 1)).unwrap();
        assert!(matches!(
            sink.capture(publish(timestamp.saturating_add(1), 2)),
            Err(TraceSinkError::Capacity)
        ));
        assert!(matches!(sink.readiness(), Err(TraceSinkError::Unavailable)));

        // Capacity is not corruption. Crash recovery may reopen the exhausted normal partition,
        // and the reserved footer space can still close the durably acknowledged prefix.
        sink.crash_for_test();
        drop(sink);
        let catalog: Arc<dyn TraceSegmentCatalog> = store;
        let restarted =
            new_without_capacity_plan_for_boundary_test(config.clone(), resolver, catalog, seed);
        assert!(matches!(
            restarted.readiness(),
            Err(TraceSinkError::Unavailable)
        ));
        let live = load_live_key_at(&config.directory.join(LIVE_KEY_NAME))
            .unwrap()
            .unwrap();
        restarted
            .shutdown(epoch_end_ms(&live.key_id).unwrap())
            .unwrap();
        assert!(!config.directory.join(LIVE_KEY_NAME).exists());
        let verified =
            verify_segment(segment_path(&config.directory, &live.key_id, 0).unwrap()).unwrap();
        assert_eq!(verified.footer.record_count(), 1);
        let manifest = read_epoch_manifest_for_signer(
            epoch_manifest_path(&config.directory, &live.key_id).unwrap(),
            SigningKey::from_bytes(&seed).verifying_key().to_bytes(),
        )
        .unwrap();
        let mut completeness = manifest.verifier();
        completeness.verify_next(&verified).unwrap();
        completeness.finish().unwrap();
    }

    #[cfg_attr(
        not(any(target_os = "linux", target_os = "macos")),
        ignore = "persistent regulatory trace is supported only on Linux and macOS"
    )]
    #[test]
    fn fresh_capture_preflight_accounts_for_live_key_header_frame_and_terminal_reserve() {
        let dir = tempfile::tempdir().unwrap();
        let (mut config, resolver, store, seed, timestamp) = fixture(&dir);
        let historical_bytes = pigeonpost_compliance_seal::MIN_TRACE_STORAGE_BYTES;
        create_private_test_file(
            &config.directory.join("historical-sealed-trace"),
            historical_bytes,
        );
        let fresh_transition = pigeonpost_compliance_seal::TRACE_TERMINAL_RESERVE_BYTES
            + TRACE_LIVE_KEY_BYTES
            + SEGMENT_HEADER_LEN as u64
            + NETWORK_TRACE_FRAME_BYTES;
        config.max_storage_bytes = historical_bytes + fresh_transition - 1;
        let catalog: Arc<dyn TraceSegmentCatalog> = store;
        let sink =
            new_without_capacity_plan_for_boundary_test(config.clone(), resolver, catalog, seed);

        assert!(matches!(
            sink.readiness_at(timestamp),
            Err(TraceSinkError::Unavailable)
        ));
        assert!(matches!(
            sink.capture_at_for_test(publish(timestamp, 1), timestamp),
            Err(TraceSinkError::Capacity)
        ));
        let state = sink
            .inner
            .state
            .lock()
            .unwrap_or_else(|error| error.into_inner());
        assert!(state.live.is_none());
        assert!(state.active.is_none());
        assert!(!config.directory.join(LIVE_KEY_NAME).exists());
        drop(state);
        sink.shutdown(timestamp).unwrap();
    }

    #[cfg_attr(
        not(any(target_os = "linux", target_os = "macos")),
        ignore = "persistent regulatory trace is supported only on Linux and macOS"
    )]
    #[test]
    fn segment_rollover_preflight_fails_before_finalizing_the_acknowledged_segment() {
        let dir = tempfile::tempdir().unwrap();
        let (mut config, resolver, store, seed, timestamp) = fixture(&dir);
        config.max_records_per_segment = 1;
        let historical_bytes = pigeonpost_compliance_seal::MIN_TRACE_STORAGE_BYTES;
        create_private_test_file(
            &config.directory.join("historical-sealed-trace"),
            historical_bytes,
        );
        let fresh_transition = pigeonpost_compliance_seal::TRACE_TERMINAL_RESERVE_BYTES
            + TRACE_LIVE_KEY_BYTES
            + SEGMENT_HEADER_LEN as u64
            + NETWORK_TRACE_FRAME_BYTES;
        config.max_storage_bytes = historical_bytes + fresh_transition;
        let catalog: Arc<dyn TraceSegmentCatalog> = store;
        let sink =
            new_without_capacity_plan_for_boundary_test(config.clone(), resolver, catalog, seed);

        sink.capture_at_for_test(publish(timestamp, 1), timestamp)
            .unwrap();
        assert!(matches!(
            sink.readiness_at(timestamp.saturating_add(1)),
            Err(TraceSinkError::Unavailable)
        ));
        assert!(matches!(
            sink.capture_at_for_test(
                publish(timestamp.saturating_add(1), 2),
                timestamp.saturating_add(1),
            ),
            Err(TraceSinkError::Capacity)
        ));
        let state = sink
            .inner
            .state
            .lock()
            .unwrap_or_else(|error| error.into_inner());
        assert_eq!(state.live.as_ref().unwrap().segment_index, 0);
        assert_eq!(state.active.as_ref().unwrap().writer.record_count(), 1);
        drop(state);
        sink.shutdown(timestamp.saturating_add(1)).unwrap();
        let live = load_live_key_at(&config.directory.join(LIVE_KEY_NAME))
            .unwrap()
            .unwrap();
        let verified =
            verify_segment(segment_path(&config.directory, &live.key_id, 0).unwrap()).unwrap();
        assert_eq!(verified.footer.record_count(), 1);
    }

    #[cfg_attr(
        not(any(target_os = "linux", target_os = "macos")),
        ignore = "persistent regulatory trace is supported only on Linux and macOS"
    )]
    #[test]
    fn epoch_rollover_preflight_fails_before_terminal_manifest_or_key_destruction() {
        #[derive(Debug)]
        struct TwoEpochKeys([ResolvedTraceKey; 2]);

        impl TraceKeyResolver for TwoEpochKeys {
            fn readiness(&self, _now_ms: u64) -> Result<(), AttributionResolutionError> {
                Ok(())
            }

            fn resolve_trace_key(
                &self,
                purpose: CompliancePurpose,
                jurisdiction: Jurisdiction,
                at_ms: u64,
            ) -> Result<Option<ResolvedTraceKey>, AttributionResolutionError> {
                Ok(self.0.iter().copied().find(|key| {
                    key.key_id.purpose == purpose
                        && key.key_id.jurisdiction == jurisdiction
                        && key.not_before_ms <= at_ms
                        && at_ms < key.not_after_ms
                }))
            }
        }

        const FIRST_EPOCH: u64 = 19_000 * TRACE_EPOCH_DURATION_MS;
        let second_epoch = FIRST_EPOCH + TRACE_EPOCH_DURATION_MS;
        let first_key = ComplianceKeyId::new(
            CompliancePurpose::NetworkTrace,
            Jurisdiction::Test,
            [71; 32],
            FIRST_EPOCH,
            1,
        );
        let second_key = ComplianceKeyId::new(
            CompliancePurpose::NetworkTrace,
            Jurisdiction::Test,
            [72; 32],
            second_epoch,
            1,
        );
        let resolved = |key_id: ComplianceKeyId| ResolvedTraceKey {
            key_id,
            public_key: [9; 32],
            not_before_ms: key_id.epoch_start_ms,
            not_after_ms: key_id.epoch_start_ms + TRACE_EPOCH_DURATION_MS,
        };
        let dir = tempfile::tempdir().unwrap();
        let historical_bytes = pigeonpost_compliance_seal::MIN_TRACE_STORAGE_BYTES;
        let trace_directory = private_test_directory(&dir, "trace");
        create_private_test_file(
            &trace_directory.join("historical-sealed-trace"),
            historical_bytes,
        );
        let fresh_transition = pigeonpost_compliance_seal::TRACE_TERMINAL_RESERVE_BYTES
            + TRACE_LIVE_KEY_BYTES
            + SEGMENT_HEADER_LEN as u64
            + NETWORK_TRACE_FRAME_BYTES;
        let config = SealedTraceConfig {
            directory: trace_directory,
            jurisdiction: Jurisdiction::Test,
            node_id: [5; 32],
            capture_policy: CapturePolicy::Standing,
            retention_days: None,
            planned_records_per_minute: 1,
            capacity_utc_epochs: 1,
            max_records_per_segment: 10,
            max_storage_bytes: historical_bytes + fresh_transition,
        };
        let store = Arc::new(SqliteStore::in_memory().unwrap());
        let catalog: Arc<dyn TraceSegmentCatalog> = store;
        let sink = SealedTraceSink::new_with_options(
            config.clone(),
            Arc::new(TwoEpochKeys([resolved(first_key), resolved(second_key)])),
            catalog,
            [7; 32],
            WorkerOptions {
                enforce_wall_clock: false,
                enforce_capacity_plan: false,
                ..WorkerOptions::default()
            },
        )
        .unwrap();

        sink.capture(publish(FIRST_EPOCH + 100, 1)).unwrap();
        assert!(matches!(
            sink.readiness_at(second_epoch + 100),
            Err(TraceSinkError::Unavailable)
        ));
        assert!(matches!(
            sink.capture(publish(second_epoch + 100, 2)),
            Err(TraceSinkError::Capacity)
        ));
        let state = sink
            .inner
            .state
            .lock()
            .unwrap_or_else(|error| error.into_inner());
        assert_eq!(state.live.as_ref().unwrap().key_id, first_key);
        assert_eq!(state.live.as_ref().unwrap().segment_index, 0);
        assert_eq!(state.active.as_ref().unwrap().writer.record_count(), 1);
        assert!(!epoch_manifest_path(&config.directory, &first_key)
            .unwrap()
            .exists());
        drop(state);
        sink.shutdown(FIRST_EPOCH + 200).unwrap();
        assert!(config.directory.join(LIVE_KEY_NAME).exists());
        let verified =
            verify_segment(segment_path(&config.directory, &first_key, 0).unwrap()).unwrap();
        assert_eq!(verified.footer.record_count(), 1);
    }

    #[cfg_attr(
        not(any(target_os = "linux", target_os = "macos")),
        ignore = "persistent regulatory trace is supported only on Linux and macOS"
    )]
    #[test]
    fn restart_recovers_the_partial_segment_without_dropping_a_record() {
        let dir = tempfile::tempdir().unwrap();
        let (config, resolver, store, seed, timestamp) = fixture(&dir);
        {
            let catalog: Arc<dyn TraceSegmentCatalog> = store.clone();
            let sink =
                SealedTraceSink::new(config.clone(), Arc::clone(&resolver), catalog, seed).unwrap();
            sink.capture(publish(timestamp, 1)).unwrap();
            // Simulated process crash: stop the worker without its coordinated finalization path,
            // leaving the durably acknowledged frame in `.open` beside the live key.
            sink.crash_for_test();
        }
        let catalog: Arc<dyn TraceSegmentCatalog> = store.clone();
        let sink = SealedTraceSink::new(config.clone(), resolver, catalog, seed).unwrap();
        sink.capture(publish(timestamp.saturating_add(1), 2))
            .unwrap();
        sink.shutdown(timestamp.saturating_add(1)).unwrap();
        let live = load_live_key_at(&config.directory.join(LIVE_KEY_NAME))
            .unwrap()
            .unwrap();
        let verified =
            verify_segment(segment_path(&config.directory, &live.key_id, 0).unwrap()).unwrap();
        assert_eq!(verified.footer.record_count(), 2);
    }

    #[cfg(unix)]
    #[test]
    fn writer_lease_denies_a_second_sink_without_mutation_and_preserves_acks() {
        let dir = tempfile::tempdir().unwrap();
        let (config, resolver, store, seed, timestamp) = fixture(&dir);
        let catalog: Arc<dyn TraceSegmentCatalog> = store.clone();
        let sink =
            SealedTraceSink::new(config.clone(), Arc::clone(&resolver), catalog, seed).unwrap();
        sink.capture(publish(timestamp, 1)).unwrap();

        let before = directory_snapshot(&config.directory);
        let competing_catalog: Arc<dyn TraceSegmentCatalog> = store.clone();
        assert!(SealedTraceSink::new(
            config.clone(),
            Arc::clone(&resolver),
            competing_catalog,
            seed,
        )
        .is_err());
        assert_eq!(directory_snapshot(&config.directory), before);

        sink.capture(publish(timestamp.saturating_add(1), 2))
            .unwrap();
        sink.crash_for_test();
        drop(sink);

        let restart_catalog: Arc<dyn TraceSegmentCatalog> = store.clone();
        let restarted =
            SealedTraceSink::new(config.clone(), Arc::clone(&resolver), restart_catalog, seed)
                .unwrap();
        restarted.shutdown(timestamp.saturating_add(2)).unwrap();
        let live = load_live_key_at(&config.directory.join(LIVE_KEY_NAME))
            .unwrap()
            .unwrap();
        let verified =
            verify_segment(segment_path(&config.directory, &live.key_id, 0).unwrap()).unwrap();
        assert_eq!(verified.footer.record_count(), 2);

        let disjoint_dir = tempfile::tempdir().unwrap();
        let (disjoint_config, disjoint_resolver, disjoint_store, disjoint_seed, _) =
            fixture(&disjoint_dir);
        let disjoint_catalog: Arc<dyn TraceSegmentCatalog> = disjoint_store;
        let disjoint = SealedTraceSink::new(
            disjoint_config,
            disjoint_resolver,
            disjoint_catalog,
            disjoint_seed,
        )
        .unwrap();
        drop(disjoint);

        drop(restarted);
        let reopen_catalog: Arc<dyn TraceSegmentCatalog> = store;
        SealedTraceSink::new(config, resolver, reopen_catalog, seed).unwrap();
    }

    #[cfg(unix)]
    #[test]
    fn unsafe_writer_lease_is_rejected_before_live_state_mutation() {
        use std::os::unix::fs::{OpenOptionsExt, PermissionsExt};

        let dir = tempfile::tempdir().unwrap();
        let (config, resolver, store, seed, _) = fixture(&dir);
        secure_directory(&config.directory).unwrap();
        let lease_path = config
            .directory
            .join(pigeonpost_compliance_seal::TRACE_WRITER_LEASE_NAME);
        OpenOptions::new()
            .read(true)
            .write(true)
            .create_new(true)
            .mode(0o644)
            .open(&lease_path)
            .unwrap();
        let catalog: Arc<dyn TraceSegmentCatalog> = store;
        assert!(SealedTraceSink::new(config.clone(), resolver, catalog, seed).is_err());
        assert_eq!(
            fs::metadata(&lease_path).unwrap().permissions().mode() & 0o777,
            0o644
        );
        assert!(!config.directory.join(LIVE_KEY_NAME).exists());
    }

    #[cfg_attr(
        not(any(target_os = "linux", target_os = "macos")),
        ignore = "persistent regulatory trace is supported only on Linux and macOS"
    )]
    #[test]
    fn concurrent_captures_share_a_durable_sync_and_all_verify_after_shutdown() {
        const RECORDS: usize = 8;

        let dir = tempfile::tempdir().unwrap();
        let (mut config, resolver, store, seed, timestamp) = fixture(&dir);
        config.max_records_per_segment = 32;
        let catalog: Arc<dyn TraceSegmentCatalog> = store;
        let sink = Arc::new(
            SealedTraceSink::new_with_options(
                config.clone(),
                resolver,
                catalog,
                seed,
                WorkerOptions {
                    queue_capacity: RECORDS,
                    batch_max: RECORDS,
                    batch_window: Duration::from_millis(50),
                    rollover_clock_poll: TRACE_ROLLOVER_CLOCK_POLL,
                    clock: WorkerClock::System,
                    enforce_wall_clock: true,
                    enforce_capacity_plan: true,
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
                sink.capture(publish(timestamp + index as u64, index as u8))
            }));
        }
        start.wait();
        for capture in captures {
            capture.join().unwrap().unwrap();
        }
        let batches = sink.status.durable_sync_batches.load(Ordering::Acquire);
        assert!(batches > 0);
        assert!(
            batches < RECORDS as u64,
            "{RECORDS} acknowledged records used {batches} data-sync batches"
        );
        sink.shutdown(timestamp + RECORDS as u64).unwrap();

        let live = load_live_key_at(&config.directory.join(LIVE_KEY_NAME))
            .unwrap()
            .unwrap();
        let verified =
            verify_segment(segment_path(&config.directory, &live.key_id, 0).unwrap()).unwrap();
        assert_eq!(verified.footer.record_count(), RECORDS as u32);
        assert_eq!(verified.frames.len(), RECORDS);
    }

    #[cfg_attr(
        not(any(target_os = "linux", target_os = "macos")),
        ignore = "persistent regulatory trace is supported only on Linux and macOS"
    )]
    #[test]
    fn bounded_queue_rejects_excess_work_while_the_worker_is_stalled() {
        #[derive(Debug)]
        struct BlockingTraceKey {
            key: ResolvedTraceKey,
            entered: Arc<std::sync::atomic::AtomicBool>,
            release: Arc<std::sync::atomic::AtomicBool>,
        }

        impl TraceKeyResolver for BlockingTraceKey {
            fn readiness(&self, _now_ms: u64) -> Result<(), AttributionResolutionError> {
                self.entered.store(true, Ordering::Release);
                while !self.release.load(Ordering::Acquire) {
                    std::thread::yield_now();
                }
                Ok(())
            }

            fn resolve_trace_key(
                &self,
                purpose: CompliancePurpose,
                jurisdiction: Jurisdiction,
                at_ms: u64,
            ) -> Result<Option<ResolvedTraceKey>, AttributionResolutionError> {
                Ok((self.key.key_id.purpose == purpose
                    && self.key.key_id.jurisdiction == jurisdiction
                    && self.key.not_before_ms <= at_ms
                    && at_ms < self.key.not_after_ms)
                    .then_some(self.key))
            }
        }

        let dir = tempfile::tempdir().unwrap();
        let (config, resolver, store, seed, timestamp) = fixture(&dir);
        let key = resolver
            .resolve_trace_key(
                CompliancePurpose::NetworkTrace,
                Jurisdiction::Test,
                timestamp,
            )
            .unwrap()
            .unwrap();
        let entered = Arc::new(std::sync::atomic::AtomicBool::new(false));
        let release = Arc::new(std::sync::atomic::AtomicBool::new(false));
        let catalog: Arc<dyn TraceSegmentCatalog> = store;
        let sink = Arc::new(
            SealedTraceSink::new_with_options(
                config,
                Arc::new(BlockingTraceKey {
                    key,
                    entered: Arc::clone(&entered),
                    release: Arc::clone(&release),
                }),
                catalog,
                seed,
                WorkerOptions {
                    queue_capacity: 1,
                    batch_max: 1,
                    batch_window: Duration::ZERO,
                    rollover_clock_poll: TRACE_ROLLOVER_CLOCK_POLL,
                    clock: WorkerClock::System,
                    enforce_wall_clock: true,
                    enforce_capacity_plan: true,
                },
            )
            .unwrap(),
        );
        let first_sink = Arc::clone(&sink);
        let first = std::thread::spawn(move || first_sink.capture(publish(timestamp, 1)));
        while !entered.load(Ordering::Acquire) {
            std::thread::yield_now();
        }
        let second_sink = Arc::clone(&sink);
        let second = std::thread::spawn(move || second_sink.capture(publish(timestamp + 1, 2)));
        while sink.status.enqueued_captures.load(Ordering::Acquire) < 2 {
            std::thread::yield_now();
        }
        assert!(matches!(
            sink.capture(publish(timestamp + 2, 3)),
            Err(TraceSinkError::Unavailable)
        ));
        assert_eq!(sink.status.enqueued_captures.load(Ordering::Acquire), 2);

        let shutdown_sink = Arc::clone(&sink);
        let shutdown =
            std::thread::spawn(move || shutdown_sink.shutdown(timestamp.saturating_add(3)));
        release.store(true, Ordering::Release);
        first.join().unwrap().unwrap();
        second.join().unwrap().unwrap();
        shutdown.join().unwrap().unwrap();
    }

    #[cfg_attr(
        not(any(target_os = "linux", target_os = "macos")),
        ignore = "persistent regulatory trace is supported only on Linux and macOS"
    )]
    #[test]
    fn injected_sync_failure_never_acknowledges_and_poisons_the_sink() {
        let dir = tempfile::tempdir().unwrap();
        let (config, resolver, store, seed, timestamp) = fixture(&dir);
        let catalog: Arc<dyn TraceSegmentCatalog> = store;
        let sink = new_without_capacity_plan_for_boundary_test(config, resolver, catalog, seed);
        sink.status.fail_before_sync.store(true, Ordering::Release);

        assert!(matches!(
            sink.capture(publish(timestamp, 1)),
            Err(TraceSinkError::Unavailable)
        ));
        assert_eq!(sink.status.durable_sync_batches.load(Ordering::Acquire), 0);
        assert!(matches!(sink.readiness(), Err(TraceSinkError::Unavailable)));
        assert!(matches!(
            sink.capture(publish(timestamp + 1, 2)),
            Err(TraceSinkError::Unavailable)
        ));
    }

    #[cfg_attr(
        not(any(target_os = "linux", target_os = "macos")),
        ignore = "persistent regulatory trace is supported only on Linux and macOS"
    )]
    #[test]
    fn worker_panic_disconnects_the_ack_and_poisons_readiness() {
        let dir = tempfile::tempdir().unwrap();
        let (config, resolver, store, seed, timestamp) = fixture(&dir);
        let catalog: Arc<dyn TraceSegmentCatalog> = store;
        let sink = SealedTraceSink::new(config, resolver, catalog, seed).unwrap();
        sink.status.panic_before_sync.store(true, Ordering::Release);

        assert!(matches!(
            sink.capture(publish(timestamp, 1)),
            Err(TraceSinkError::Unavailable)
        ));
        assert_eq!(sink.status.durable_sync_batches.load(Ordering::Acquire), 0);
        assert!(matches!(sink.readiness(), Err(TraceSinkError::Unavailable)));
    }

    #[cfg_attr(
        not(any(target_os = "linux", target_os = "macos")),
        ignore = "persistent regulatory trace is supported only on Linux and macOS"
    )]
    #[test]
    fn segment_full_rollover_and_shutdown_publish_every_acknowledged_record() {
        let dir = tempfile::tempdir().unwrap();
        let (mut config, resolver, store, seed, timestamp) = fixture(&dir);
        config.max_records_per_segment = 2;
        let catalog: Arc<dyn TraceSegmentCatalog> = store;
        let sink = SealedTraceSink::new(config.clone(), resolver, catalog, seed).unwrap();
        for event in 0..5 {
            sink.capture(publish(timestamp + event, event as u8))
                .unwrap();
        }
        sink.shutdown(timestamp + 5).unwrap();

        let live = load_live_key_at(&config.directory.join(LIVE_KEY_NAME))
            .unwrap()
            .unwrap();
        assert_eq!(live.segment_index, 3);
        let counts = (0..3)
            .map(|index| {
                verify_segment(segment_path(&config.directory, &live.key_id, index).unwrap())
                    .unwrap()
                    .footer
                    .record_count()
            })
            .collect::<Vec<_>>();
        assert_eq!(counts, vec![2, 2, 1]);
    }

    #[cfg_attr(
        not(any(target_os = "linux", target_os = "macos")),
        ignore = "persistent regulatory trace is supported only on Linux and macOS"
    )]
    #[test]
    fn epoch_rollover_closes_the_old_day_before_acknowledging_the_new_day() {
        #[derive(Debug)]
        struct DailyTraceKeys {
            keys: [ResolvedTraceKey; 2],
        }

        impl TraceKeyResolver for DailyTraceKeys {
            fn readiness(&self, _now_ms: u64) -> Result<(), AttributionResolutionError> {
                Ok(())
            }

            fn resolve_trace_key(
                &self,
                purpose: CompliancePurpose,
                jurisdiction: Jurisdiction,
                at_ms: u64,
            ) -> Result<Option<ResolvedTraceKey>, AttributionResolutionError> {
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
        let first_key = ComplianceKeyId::new(
            CompliancePurpose::NetworkTrace,
            Jurisdiction::Test,
            [31; 32],
            FIRST_EPOCH,
            1,
        );
        let second_key = ComplianceKeyId::new(
            CompliancePurpose::NetworkTrace,
            Jurisdiction::Test,
            [32; 32],
            second_epoch,
            2,
        );
        let resolved = |key_id: ComplianceKeyId| ResolvedTraceKey {
            key_id,
            public_key: [9; 32],
            not_before_ms: key_id.epoch_start_ms,
            not_after_ms: key_id.epoch_start_ms + TRACE_EPOCH_DURATION_MS,
        };
        let dir = tempfile::tempdir().unwrap();
        let store = Arc::new(SqliteStore::in_memory().unwrap());
        let catalog: Arc<dyn TraceSegmentCatalog> = store;
        let config = SealedTraceConfig {
            directory: private_test_directory(&dir, "trace"),
            jurisdiction: Jurisdiction::Test,
            node_id: [5; 32],
            capture_policy: CapturePolicy::Standing,
            retention_days: None,
            planned_records_per_minute: 1,
            capacity_utc_epochs: 1,
            max_records_per_segment: 10,
            max_storage_bytes: DEFAULT_TRACE_STORAGE_BYTES,
        };
        let sink = SealedTraceSink::new_with_options(
            config.clone(),
            Arc::new(DailyTraceKeys {
                keys: [resolved(first_key), resolved(second_key)],
            }),
            catalog,
            [7; 32],
            WorkerOptions {
                queue_capacity: 4,
                batch_max: 4,
                batch_window: Duration::ZERO,
                rollover_clock_poll: TRACE_ROLLOVER_CLOCK_POLL,
                clock: WorkerClock::System,
                // Synthetic aligned days let this test cross midnight deterministically.
                enforce_wall_clock: false,
                enforce_capacity_plan: true,
            },
        )
        .unwrap();

        sink.capture(publish(FIRST_EPOCH + 100, 1)).unwrap();
        sink.capture(publish(second_epoch + 100, 2)).unwrap();
        sink.shutdown(second_epoch + 200).unwrap();

        let first =
            verify_segment(segment_path(&config.directory, &first_key, 0).unwrap()).unwrap();
        let second =
            verify_segment(segment_path(&config.directory, &second_key, 0).unwrap()).unwrap();
        assert_eq!(first.footer.record_count(), 1);
        assert_eq!(second.footer.record_count(), 1);
        assert!(first.footer.closed_at_ms() < second.header.opened_at_ms());
        let first_manifest = read_epoch_manifest_for_signer(
            epoch_manifest_path(&config.directory, &first_key).unwrap(),
            SigningKey::from_bytes(&[7; 32]).verifying_key().to_bytes(),
        )
        .unwrap();
        assert_eq!(first_manifest.key_id(), first_key);
        assert_eq!(first_manifest.total_segments(), 1);
        assert_eq!(first_manifest.total_records(), 1);
        let mut completeness = first_manifest.verifier();
        completeness.verify_next(&first).unwrap();
        completeness.finish().unwrap();
        assert!(!epoch_manifest_path(&config.directory, &second_key)
            .unwrap()
            .exists());
        let live = load_live_key_at(&config.directory.join(LIVE_KEY_NAME))
            .unwrap()
            .unwrap();
        assert_eq!(live.key_id, second_key);
        assert_eq!(live.segment_index, 1);
    }

    #[cfg_attr(
        not(any(target_os = "linux", target_os = "macos")),
        ignore = "persistent regulatory trace is supported only on Linux and macOS"
    )]
    #[test]
    fn idle_utc_boundary_publishes_manifest_and_destroys_live_key() {
        let dir = tempfile::tempdir().unwrap();
        let (config, resolver, store, seed, timestamp) = fixture(&dir);
        let manual_now = Box::leak(Box::new(AtomicU64::new(timestamp)));
        let catalog: Arc<dyn TraceSegmentCatalog> = store;
        let sink = SealedTraceSink::new_with_options(
            config.clone(),
            resolver,
            catalog,
            seed,
            WorkerOptions {
                rollover_clock_poll: Duration::from_millis(1),
                clock: WorkerClock::Test(manual_now),
                enforce_capacity_plan: false,
                ..WorkerOptions::default()
            },
        )
        .unwrap();

        sink.capture_at_for_test(publish(timestamp, 91), timestamp)
            .unwrap();
        let live_path = config.directory.join(LIVE_KEY_NAME);
        let live = load_live_key_at(&live_path).unwrap().unwrap();
        let manifest_path = epoch_manifest_path(&config.directory, &live.key_id).unwrap();
        assert!(!manifest_path.exists());

        let epoch_end = epoch_end_ms(&live.key_id).unwrap();
        manual_now.store(epoch_end, Ordering::Release);
        let wait_deadline = Instant::now() + Duration::from_secs(60);
        while (!manifest_path.exists() || live_path.exists()) && Instant::now() < wait_deadline {
            std::thread::sleep(Duration::from_millis(1));
        }

        assert!(manifest_path.exists());
        assert!(!live_path.exists());
        let manifest = read_epoch_manifest_for_signer(
            manifest_path,
            SigningKey::from_bytes(&seed).verifying_key().to_bytes(),
        )
        .unwrap();
        assert_eq!(manifest.total_segments(), 1);
        assert_eq!(manifest.total_records(), 1);
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
        struct DailyTraceKeys {
            keys: [ResolvedTraceKey; 2],
        }

        impl TraceKeyResolver for DailyTraceKeys {
            fn readiness(&self, _now_ms: u64) -> Result<(), AttributionResolutionError> {
                Ok(())
            }

            fn resolve_trace_key(
                &self,
                purpose: CompliancePurpose,
                jurisdiction: Jurisdiction,
                at_ms: u64,
            ) -> Result<Option<ResolvedTraceKey>, AttributionResolutionError> {
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
        let first_key = ComplianceKeyId::new(
            CompliancePurpose::NetworkTrace,
            Jurisdiction::Test,
            [61; 32],
            FIRST_EPOCH,
            1,
        );
        let second_key = ComplianceKeyId::new(
            CompliancePurpose::NetworkTrace,
            Jurisdiction::Test,
            [62; 32],
            second_epoch,
            1,
        );
        let resolved = |key_id: ComplianceKeyId| ResolvedTraceKey {
            key_id,
            public_key: [9; 32],
            not_before_ms: key_id.epoch_start_ms,
            not_after_ms: key_id.epoch_start_ms + TRACE_EPOCH_DURATION_MS,
        };
        let dir = tempfile::tempdir().unwrap();
        let store = Arc::new(SqliteStore::in_memory().unwrap());
        let catalog: Arc<dyn TraceSegmentCatalog> = store;
        let config = SealedTraceConfig {
            directory: private_test_directory(&dir, "trace"),
            jurisdiction: Jurisdiction::Test,
            node_id: [5; 32],
            capture_policy: CapturePolicy::Standing,
            retention_days: None,
            planned_records_per_minute: 1,
            capacity_utc_epochs: 1,
            max_records_per_segment: 10,
            max_storage_bytes: DEFAULT_TRACE_STORAGE_BYTES,
        };
        let sink = SealedTraceSink::new_with_options(
            config.clone(),
            Arc::new(DailyTraceKeys {
                keys: [resolved(first_key), resolved(second_key)],
            }),
            catalog,
            [7; 32],
            WorkerOptions {
                enforce_wall_clock: true,
                ..WorkerOptions::default()
            },
        )
        .unwrap();

        // Caller timestamps arrive in reverse epoch order; the retained timestamp is the truthful
        // serialized admission sample, so epoch state still moves strictly forward.
        sink.capture_at_for_test(publish(second_epoch + 500, 1), second_epoch - 1)
            .unwrap();
        let enqueued = sink.status.enqueued_captures.load(Ordering::Acquire);
        assert!(sink
            .capture_at_for_test(publish(FIRST_EPOCH + 500, 2), second_epoch - 2)
            .is_err());
        assert_eq!(
            sink.status.enqueued_captures.load(Ordering::Acquire),
            enqueued
        );
        assert!(sink.status.is_running());
        assert!(sink.readiness_at(second_epoch - 1).is_ok());

        sink.capture_at_for_test(publish(FIRST_EPOCH + 501, 3), second_epoch)
            .unwrap();
        assert!(sink.status.is_running());
        assert!(sink.readiness_at(second_epoch).is_ok());
        sink.shutdown(second_epoch + 1).unwrap();

        let first =
            verify_segment(segment_path(&config.directory, &first_key, 0).unwrap()).unwrap();
        let second =
            verify_segment(segment_path(&config.directory, &second_key, 0).unwrap()).unwrap();
        assert_eq!(first.header.opened_at_ms(), second_epoch - 1);
        assert_eq!(second.header.opened_at_ms(), second_epoch);
        assert_eq!(first.footer.record_count(), 1);
        assert_eq!(second.footer.record_count(), 1);
        let manifest = read_epoch_manifest_for_signer(
            epoch_manifest_path(&config.directory, &first_key).unwrap(),
            SigningKey::from_bytes(&[7; 32]).verifying_key().to_bytes(),
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
    fn restart_converges_an_existing_terminal_manifest_before_destroying_the_live_key() {
        const CLOSED_EPOCH: u64 = 19_000 * TRACE_EPOCH_DURATION_MS;

        let key_id = ComplianceKeyId::new(
            CompliancePurpose::NetworkTrace,
            Jurisdiction::Test,
            [41; 32],
            CLOSED_EPOCH,
            1,
        );
        let resolved = ResolvedTraceKey {
            key_id,
            public_key: [9; 32],
            not_before_ms: CLOSED_EPOCH,
            not_after_ms: CLOSED_EPOCH + TRACE_EPOCH_DURATION_MS,
        };
        let resolver: Arc<dyn TraceKeyResolver> = Arc::new(FixedTraceKey { key: resolved });
        let dir = tempfile::tempdir().unwrap();
        let config = SealedTraceConfig {
            directory: private_test_directory(&dir, "trace"),
            jurisdiction: Jurisdiction::Test,
            node_id: [5; 32],
            capture_policy: CapturePolicy::Standing,
            retention_days: None,
            planned_records_per_minute: 1,
            capacity_utc_epochs: 1,
            max_records_per_segment: 10,
            max_storage_bytes: DEFAULT_TRACE_STORAGE_BYTES,
        };
        let store = Arc::new(SqliteStore::in_memory().unwrap());
        let catalog: Arc<dyn TraceSegmentCatalog> = store.clone();
        let sink = SealedTraceSink::new_with_options(
            config.clone(),
            Arc::clone(&resolver),
            catalog,
            [7; 32],
            WorkerOptions {
                enforce_wall_clock: false,
                ..WorkerOptions::default()
            },
        )
        .unwrap();
        sink.capture(publish(CLOSED_EPOCH + 100, 1)).unwrap();
        {
            let mut state = sink
                .inner
                .state
                .lock()
                .unwrap_or_else(|error| error.into_inner());
            sink.inner
                .finalize_active_locked(&mut state, CLOSED_EPOCH + TRACE_EPOCH_DURATION_MS - 1)
                .unwrap();
            sink.inner.publish_terminal_manifest_locked(&state).unwrap();
            // Simulate loss after the atomic terminal marker but before live-key unlink.
        }
        let manifest_path = epoch_manifest_path(&config.directory, &key_id).unwrap();
        let first_bytes = fs::read(&manifest_path).unwrap();
        assert!(config.directory.join(LIVE_KEY_NAME).exists());
        sink.crash_for_test();
        drop(sink);

        let catalog: Arc<dyn TraceSegmentCatalog> = store;
        let restarted = SealedTraceSink::new_with_options(
            config.clone(),
            resolver,
            catalog,
            [7; 32],
            WorkerOptions {
                enforce_wall_clock: false,
                ..WorkerOptions::default()
            },
        )
        .unwrap();
        assert!(!config.directory.join(LIVE_KEY_NAME).exists());
        assert_eq!(fs::read(&manifest_path).unwrap(), first_bytes);
        let manifest = read_epoch_manifest_for_signer(
            manifest_path,
            SigningKey::from_bytes(&[7; 32]).verifying_key().to_bytes(),
        )
        .unwrap();
        assert_eq!(
            manifest.epoch_end_ms(),
            CLOSED_EPOCH + TRACE_EPOCH_DURATION_MS
        );
        restarted.shutdown(now_ms()).unwrap();
    }

    #[cfg_attr(
        not(any(target_os = "linux", target_os = "macos")),
        ignore = "persistent regulatory trace is supported only on Linux and macOS"
    )]
    #[test]
    fn eu_nodes_do_not_capture_outside_the_preservation_window() {
        let dir = tempfile::tempdir().unwrap();
        let (mut config, resolver, store, seed, timestamp) = fixture(&dir);
        config.jurisdiction = Jurisdiction::Eu;
        config.capture_policy = CapturePolicy::Preservation {
            starts_at_ms: timestamp.saturating_add(10_000),
            expires_at_ms: timestamp.saturating_add(20_000),
        };
        config.capacity_utc_epochs = 2;
        let catalog: Arc<dyn TraceSegmentCatalog> = store;
        let sink = SealedTraceSink::new(config, resolver, catalog, seed).unwrap();
        sink.capture(publish(timestamp, 1)).unwrap();
        assert!(sink
            .inner
            .state
            .lock()
            .unwrap_or_else(|error| error.into_inner())
            .live
            .is_none());
    }

    #[cfg_attr(
        not(any(target_os = "linux", target_os = "macos")),
        ignore = "persistent regulatory trace is supported only on Linux and macOS"
    )]
    #[test]
    fn inactive_eu_preservation_remains_ready_when_historical_storage_is_full() {
        let dir = tempfile::tempdir().unwrap();
        let (mut config, resolver, store, seed, timestamp) = fixture(&dir);
        config.jurisdiction = Jurisdiction::Eu;
        config.capture_policy = CapturePolicy::Preservation {
            starts_at_ms: timestamp.saturating_add(10_000),
            expires_at_ms: timestamp.saturating_add(20_000),
        };
        config.capacity_utc_epochs = 2;
        config.max_storage_bytes = pigeonpost_compliance_seal::MIN_TRACE_STORAGE_BYTES;
        create_private_test_file(
            &config.directory.join("historical-sealed-trace"),
            config.max_storage_bytes,
        );
        let catalog: Arc<dyn TraceSegmentCatalog> = store;
        let sink = new_without_capacity_plan_for_boundary_test(config, resolver, catalog, seed);

        assert!(sink.readiness_at(timestamp).is_ok());
        sink.capture(publish(timestamp, 1)).unwrap();
        assert!(sink
            .inner
            .state
            .lock()
            .unwrap_or_else(|error| error.into_inner())
            .live
            .is_none());
        sink.shutdown(timestamp).unwrap();
    }

    #[cfg_attr(
        not(any(target_os = "linux", target_os = "macos")),
        ignore = "persistent regulatory trace is supported only on Linux and macOS"
    )]
    #[test]
    fn active_policy_is_not_ready_without_a_current_network_key() {
        #[derive(Debug)]
        struct MissingKey;

        impl TraceKeyResolver for MissingKey {
            fn readiness(&self, _now_ms: u64) -> Result<(), AttributionResolutionError> {
                Ok(())
            }

            fn resolve_trace_key(
                &self,
                _purpose: CompliancePurpose,
                _jurisdiction: Jurisdiction,
                _at_ms: u64,
            ) -> Result<Option<ResolvedTraceKey>, AttributionResolutionError> {
                Ok(None)
            }
        }

        let dir = tempfile::tempdir().unwrap();
        let (config, _, store, seed, _) = fixture(&dir);
        let catalog: Arc<dyn TraceSegmentCatalog> = store;
        let sink = SealedTraceSink::new(config, Arc::new(MissingKey), catalog, seed).unwrap();
        assert!(matches!(sink.readiness(), Err(TraceSinkError::Unavailable)));
    }

    #[test]
    fn resolved_trace_keys_reject_noncanonical_daily_intervals() {
        let timestamp = now_ms();
        let start = day_start_ms(timestamp);
        let mut key = ResolvedTraceKey {
            key_id: ComplianceKeyId::new(
                CompliancePurpose::NetworkTrace,
                Jurisdiction::Test,
                [3; 32],
                start,
                1,
            ),
            public_key: [9; 32],
            not_before_ms: start,
            not_after_ms: start + 2 * TRACE_EPOCH_DURATION_MS,
        };
        assert!(matches!(
            validate_resolved_key(&key, Jurisdiction::Test, timestamp),
            Err(TraceSinkError::Unavailable)
        ));
        key.key_id.epoch_start_ms = start + 1;
        key.not_before_ms = start + 1;
        key.not_after_ms = start + 1 + TRACE_EPOCH_DURATION_MS;
        assert!(matches!(
            validate_resolved_key(&key, Jurisdiction::Test, timestamp),
            Err(TraceSinkError::Unavailable)
        ));
    }

    #[cfg(unix)]
    #[test]
    fn live_key_rejects_symlinks_hardlinks_and_untracked_destruction() {
        use std::os::unix::fs::symlink;

        let dir = tempfile::tempdir().unwrap();
        let (config, resolver, store, seed, timestamp) = fixture(&dir);
        let catalog: Arc<dyn TraceSegmentCatalog> = store;
        let sink = SealedTraceSink::new(config.clone(), resolver, catalog, seed).unwrap();
        sink.capture(publish(timestamp, 1)).unwrap();
        sink.shutdown(timestamp).unwrap();

        let live_path = config.directory.join(LIVE_KEY_NAME);
        let alias = config.directory.join("live-key.alias");
        fs::hard_link(&live_path, &alias).unwrap();
        assert!(matches!(
            load_live_key_at(&live_path),
            Err(TraceSinkError::Unavailable)
        ));
        let mut state = sink.inner.state.lock().unwrap();
        assert!(matches!(
            sink.inner.destroy_live_key_locked(&mut state),
            Err(TraceSinkError::Unavailable)
        ));
        drop(state);
        fs::remove_file(&alias).unwrap();

        let target = config.directory.join("live-key.target");
        fs::rename(&live_path, &target).unwrap();
        symlink(&target, &live_path).unwrap();
        assert!(matches!(
            load_live_key_at(&live_path),
            Err(TraceSinkError::Unavailable)
        ));
    }

    #[cfg(unix)]
    #[test]
    fn trace_store_rejects_intermediate_symlinks_and_mutable_ancestors_without_creation() {
        use std::os::unix::fs::{symlink, PermissionsExt};

        let root = tempfile::tempdir().unwrap();
        let actual = root.path().join("actual");
        fs::create_dir(&actual).unwrap();
        fs::set_permissions(&actual, fs::Permissions::from_mode(0o700)).unwrap();
        let alias = root.path().join("alias");
        symlink(&actual, &alias).unwrap();
        let (mut linked, resolver, store, seed, _) = fixture(&root);
        fs::remove_dir(&linked.directory).unwrap();
        linked.directory = alias.join("trace");
        let catalog: Arc<dyn TraceSegmentCatalog> = store;
        assert!(SealedTraceSink::new(linked, Arc::clone(&resolver), catalog, seed).is_err());
        assert!(!actual.join("trace").exists());

        let mutable = root.path().join("mutable");
        fs::create_dir(&mutable).unwrap();
        fs::set_permissions(&mutable, fs::Permissions::from_mode(0o770)).unwrap();
        let (mut unsafe_config, _, store, seed, _) = fixture(&root);
        fs::remove_dir(&unsafe_config.directory).unwrap();
        unsafe_config.directory = mutable.join("trace");
        let catalog: Arc<dyn TraceSegmentCatalog> = store;
        assert!(SealedTraceSink::new(unsafe_config, resolver, catalog, seed).is_err());
        assert!(!mutable.join("trace").exists());
    }

    #[cfg(unix)]
    #[test]
    fn retained_trace_parent_replacement_fails_before_capture_mutation() {
        let root = tempfile::tempdir().unwrap();
        let (config, resolver, store, seed, timestamp) = fixture(&root);
        let catalog: Arc<dyn TraceSegmentCatalog> = store;
        let sink = SealedTraceSink::new(config.clone(), resolver, catalog, seed).unwrap();
        let moved = root.path().join("trace-retained");
        fs::rename(&config.directory, &moved).unwrap();
        fs::create_dir(&config.directory).unwrap();
        use std::os::unix::fs::PermissionsExt;
        fs::set_permissions(&config.directory, fs::Permissions::from_mode(0o700)).unwrap();

        assert!(matches!(
            sink.capture_at_for_test(publish(timestamp, 90), timestamp),
            Err(TraceSinkError::Unavailable)
        ));
        assert_eq!(fs::read_dir(&config.directory).unwrap().count(), 0);
        assert!(!moved.join(LIVE_KEY_NAME).exists());
    }

    #[cfg(unix)]
    #[test]
    fn live_key_publication_recovers_from_temp_collision_and_preserves_fifo_destination() {
        use std::os::unix::fs::{FileTypeExt, OpenOptionsExt};
        use std::process::Command;

        let root = tempfile::tempdir().unwrap();
        let (config, resolver, store, seed, timestamp) = fixture(&root);
        let catalog: Arc<dyn TraceSegmentCatalog> = store;
        let sink = SealedTraceSink::new(config.clone(), resolver, catalog, seed).unwrap();
        let key_id = ComplianceKeyId::new(
            CompliancePurpose::NetworkTrace,
            Jurisdiction::Test,
            [91; 32],
            day_start_ms(timestamp),
            1,
        );
        let live = LiveKeyState {
            key_id,
            segment_index: 4,
            secret: [92; 32],
        };
        let colliding_counter = TEMP_COUNTER.load(Ordering::Relaxed);
        let collision = sink.inner.directory.absolute_path().join(format!(
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

        persist_live_key_guarded(&sink.inner.directory, &live).unwrap();
        assert_eq!(fs::read(&collision).unwrap(), b"collision");
        let loaded = load_live_key_guarded(&sink.inner.directory)
            .unwrap()
            .unwrap();
        assert_eq!(loaded.key_id, key_id);
        assert_eq!(loaded.segment_index, 4);

        fs::remove_file(config.directory.join(LIVE_KEY_NAME)).unwrap();
        let destination = config.directory.join(LIVE_KEY_NAME);
        assert!(Command::new("mkfifo")
            .arg(&destination)
            .status()
            .unwrap()
            .success());
        assert!(matches!(
            persist_live_key_guarded(&sink.inner.directory, &live),
            Err(TraceSinkError::Unavailable)
        ));
        assert!(fs::symlink_metadata(&destination)
            .unwrap()
            .file_type()
            .is_fifo());
        let unexpected_temps = fs::read_dir(&config.directory)
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
