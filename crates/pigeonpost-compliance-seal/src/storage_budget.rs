//! Fixed logical budgets for online regulatory trace stores.
//!
//! A writer owns its directory through [`crate::TraceWriterLease`], so one startup scan followed
//! by exact fixed-width charges is both bounded and race-free for remote inputs. The final reserve
//! is unavailable to ordinary frames and remains large enough to close a segment and publish the
//! maximum canonical terminal epoch manifest.

use std::path::Path;
use std::sync::atomic::{AtomicU64, Ordering};

use pigeonpost_compliance_format::COMPLIANCE_KEY_ID_LEN;
#[cfg(unix)]
use pigeonpost_unix_custody::{CustodyError, DirPolicy, FilePolicy, GuardedDir, OpenAccess};

use crate::segment::{require_persistent_writer_platform, PersistentWriterPlatform};
use crate::{
    Result, SealError, EPOCH_MANIFEST_ENTRY_LEN, EPOCH_MANIFEST_FIXED_LEN,
    IDENTITY_TRACE_RECORD_LEN, MAX_EPOCH_MANIFEST_BYTES, MAX_EPOCH_MANIFEST_SEGMENTS,
    MAX_SEGMENT_RECORDS, SEGMENT_FOOTER_LEN, SEGMENT_HEADER_LEN, TRACE_RECORD_LEN,
};

const FRAME_FIXED_BYTES: u64 = 8 + 24 + 16 + 32;
/// Any UTC day can overlap at most 1,441 independently aligned 60-second admission windows.
pub const TRACE_RATE_WINDOWS_PER_UTC_DAY: u64 = 24 * 60 + 1;
/// Exact on-disk bytes appended for one network trace frame.
pub const NETWORK_TRACE_FRAME_BYTES: u64 = TRACE_RECORD_LEN as u64 + FRAME_FIXED_BYTES;
/// Exact on-disk bytes appended for one identity trace frame.
pub const IDENTITY_TRACE_FRAME_BYTES: u64 = IDENTITY_TRACE_RECORD_LEN as u64 + FRAME_FIXED_BYTES;
/// Exact final-file bytes for the online epoch-key recovery record shared by the trace writers.
pub const TRACE_LIVE_KEY_BYTES: u64 = (8 + 1 + COMPLIANCE_KEY_ID_LEN + 4 + 32) as u64;
/// Space ordinary capture may never consume. It covers the maximum terminal manifest, the active
/// segment footer, atomic temp-file overlap for the live key, and filesystem metadata slack.
pub const TRACE_TERMINAL_RESERVE_BYTES: u64 =
    MAX_EPOCH_MANIFEST_BYTES + SEGMENT_FOOTER_LEN as u64 + 64 * 1024;
/// Smallest useful configured budget: reserve plus the live recovery key, one segment header, and
/// the larger fixed-width frame. A configuration at this boundary can therefore capture once.
pub const MIN_TRACE_STORAGE_BYTES: u64 = TRACE_TERMINAL_RESERVE_BYTES
    + TRACE_LIVE_KEY_BYTES
    + SEGMENT_HEADER_LEN as u64
    + IDENTITY_TRACE_FRAME_BYTES;
/// Production constructor default. Operators should place each store on its own at-least-this-big
/// hard-quota filesystem or volume; the application budget remains an independent fail-closed cap.
pub const DEFAULT_TRACE_STORAGE_BYTES: u64 = 10 * 1024 * 1024 * 1024;
/// Audited public-API ceiling for one online purpose directory (one pebibyte).
pub const MAX_TRACE_STORAGE_BYTES: u64 = 1024 * 1024 * 1024 * 1024 * 1024;
#[cfg(unix)]
const MAX_TRACE_DIRECTORY_ENTRIES: usize = 1_000_000;

/// Conservative append-only capacity needed for a network-trace purpose directory. The estimate
/// includes every admitted fixed-width frame, segment headers/footers, exact-size daily terminal
/// manifests, the live recovery key, and the protected terminal reserve. It intentionally assumes
/// one extra rate-limit window at UTC-day boundaries.
pub fn required_network_trace_storage_bytes(
    records_per_minute: u32,
    capacity_days: u64,
    max_records_per_segment: u32,
) -> Result<u64> {
    required_trace_storage_bytes(
        NETWORK_TRACE_FRAME_BYTES,
        records_per_minute,
        capacity_days,
        max_records_per_segment,
    )
}

/// Conservative append-only capacity needed for an identity-trace purpose directory. See
/// [`required_network_trace_storage_bytes`] for the accounted artifacts and boundary assumption.
pub fn required_identity_trace_storage_bytes(
    records_per_minute: u32,
    capacity_days: u64,
    max_records_per_segment: u32,
) -> Result<u64> {
    required_trace_storage_bytes(
        IDENTITY_TRACE_FRAME_BYTES,
        records_per_minute,
        capacity_days,
        max_records_per_segment,
    )
}

fn required_trace_storage_bytes(
    frame_bytes: u64,
    records_per_minute: u32,
    capacity_days: u64,
    max_records_per_segment: u32,
) -> Result<u64> {
    if records_per_minute == 0
        || capacity_days == 0
        || max_records_per_segment == 0
        || max_records_per_segment > MAX_SEGMENT_RECORDS
    {
        return Err(SealError::StorageLimit);
    }
    let records_per_day = u64::from(records_per_minute)
        .checked_mul(TRACE_RATE_WINDOWS_PER_UTC_DAY)
        .ok_or(SealError::StorageLimit)?;
    let segment_size = u64::from(max_records_per_segment);
    let segments_per_day = records_per_day
        .checked_add(segment_size - 1)
        .ok_or(SealError::StorageLimit)?
        / segment_size;
    if segments_per_day > u64::from(MAX_EPOCH_MANIFEST_SEGMENTS) {
        return Err(SealError::StorageLimit);
    }
    let manifest_bytes = u64::try_from(EPOCH_MANIFEST_FIXED_LEN)
        .ok()
        .and_then(|fixed| {
            u64::try_from(EPOCH_MANIFEST_ENTRY_LEN)
                .ok()?
                .checked_mul(segments_per_day)?
                .checked_add(fixed)
        })
        .ok_or(SealError::StorageLimit)?;
    let daily_bytes = frame_bytes
        .checked_mul(records_per_day)
        .and_then(|frames| {
            (SEGMENT_HEADER_LEN as u64 + SEGMENT_FOOTER_LEN as u64)
                .checked_mul(segments_per_day)?
                .checked_add(frames)
        })
        .and_then(|segments_and_frames| segments_and_frames.checked_add(manifest_bytes))
        .ok_or(SealError::StorageLimit)?;
    let required = daily_bytes
        .checked_mul(capacity_days)
        .and_then(|history| history.checked_add(TRACE_LIVE_KEY_BYTES))
        .and_then(|history| history.checked_add(TRACE_TERMINAL_RESERVE_BYTES))
        .ok_or(SealError::StorageLimit)?;
    if required > MAX_TRACE_STORAGE_BYTES {
        return Err(SealError::StorageLimit);
    }
    Ok(required.max(MIN_TRACE_STORAGE_BYTES))
}

pub struct TraceStorageBudget {
    limit: u64,
    used: AtomicU64,
    #[cfg(unix)]
    directory: GuardedDir,
}

impl core::fmt::Debug for TraceStorageBudget {
    fn fmt(&self, formatter: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        formatter
            .debug_struct("TraceStorageBudget")
            .field("limit", &self.limit)
            .field("used", &self.used())
            .field("terminal_reserve", &TRACE_TERMINAL_RESERVE_BYTES)
            .finish()
    }
}

impl TraceStorageBudget {
    pub fn new(directory: &Path, limit: u64) -> Result<Self> {
        Self::new_for_platform(PersistentWriterPlatform::current(), directory, limit)
    }

    fn new_for_platform(
        platform: PersistentWriterPlatform,
        directory: &Path,
        limit: u64,
    ) -> Result<Self> {
        require_persistent_writer_platform(platform)?;
        if !(MIN_TRACE_STORAGE_BYTES..=MAX_TRACE_STORAGE_BYTES).contains(&limit) {
            return Err(SealError::StorageLimit);
        }
        #[cfg(unix)]
        let directory = GuardedDir::open_existing(directory, DirPolicy::private_mutable())
            .map_err(map_budget_custody_error)?;
        #[cfg(unix)]
        let used = directory_usage(&directory)?;
        #[cfg(not(unix))]
        let used = directory_usage(directory)?;
        let budget = Self {
            limit,
            used: AtomicU64::new(used),
            #[cfg(unix)]
            directory,
        };
        if used > limit {
            return Err(SealError::StorageLimit);
        }
        Ok(budget)
    }

    pub fn used(&self) -> u64 {
        self.used.load(Ordering::Acquire)
    }

    pub fn limit(&self) -> u64 {
        self.limit
    }

    pub fn has_normal_headroom(&self, bytes: u64) -> bool {
        self.has_transition_headroom(0, 0, bytes)
    }

    /// Check a complete ordered transition without mutating accounting: terminal artifacts are
    /// written first, then `released_bytes` are securely removed, then ordinary artifacts are
    /// created while preserving a fresh terminal reserve for the new active state.
    pub fn has_transition_headroom(
        &self,
        terminal_bytes: u64,
        released_bytes: u64,
        normal_bytes: u64,
    ) -> bool {
        let Some(terminal_peak) = self.used().checked_add(terminal_bytes) else {
            return false;
        };
        if terminal_peak > self.limit {
            return false;
        }
        terminal_peak
            .checked_sub(released_bytes)
            .and_then(|used| used.checked_add(normal_bytes))
            .and_then(|used| used.checked_add(TRACE_TERMINAL_RESERVE_BYTES))
            .is_some_and(|required| required <= self.limit)
    }

    /// Reserve bytes for an ordinary header, live-key artifact, or trace frame.
    pub fn charge_normal(&self, bytes: u64) -> Result<()> {
        self.charge(bytes, TRACE_TERMINAL_RESERVE_BYTES)
    }

    /// Consume the protected reserve only for a footer or terminal manifest.
    pub fn charge_terminal(&self, bytes: u64) -> Result<()> {
        self.charge(bytes, 0)
    }

    pub fn release(&self, bytes: u64) -> Result<()> {
        self.used
            .fetch_update(Ordering::AcqRel, Ordering::Acquire, |used| {
                used.checked_sub(bytes)
            })
            .map(|_| ())
            .map_err(|_| SealError::StorageLimit)
    }

    /// Reconcile after crash recovery may have truncated or finalized an open segment.
    pub fn reconcile(&self, directory: &Path) -> Result<()> {
        self.reconcile_for_platform(PersistentWriterPlatform::current(), directory)
    }

    fn reconcile_for_platform(
        &self,
        platform: PersistentWriterPlatform,
        directory: &Path,
    ) -> Result<()> {
        require_persistent_writer_platform(platform)?;
        #[cfg(unix)]
        let used = {
            let supplied = GuardedDir::open_existing(directory, DirPolicy::private_mutable())
                .map_err(map_budget_custody_error)?;
            if supplied.identity() != self.directory.identity() {
                return Err(SealError::StorageLimit);
            }
            self.directory
                .verify_named()
                .map_err(map_budget_custody_error)?;
            let used = directory_usage(&self.directory)?;
            self.directory
                .verify_named()
                .map_err(map_budget_custody_error)?;
            used
        };
        #[cfg(not(unix))]
        let used = directory_usage(directory)?;
        if used > self.limit {
            return Err(SealError::StorageLimit);
        }
        self.used.store(used, Ordering::Release);
        Ok(())
    }

    fn charge(&self, bytes: u64, protected: u64) -> Result<()> {
        self.used
            .fetch_update(Ordering::AcqRel, Ordering::Acquire, |used| {
                used.checked_add(bytes).and_then(|next| {
                    next.checked_add(protected)
                        .filter(|required| *required <= self.limit)
                        .map(|_| next)
                })
            })
            .map(|_| ())
            .map_err(|_| SealError::StorageLimit)
    }
}

#[cfg(unix)]
fn directory_usage(directory: &GuardedDir) -> Result<u64> {
    let mut total = 0u64;
    let entries = directory
        .entries_bounded(MAX_TRACE_DIRECTORY_ENTRIES)
        .map_err(map_budget_custody_error)?;
    for entry in entries {
        let entry = entry.map_err(map_budget_custody_error)?;
        let file = directory
            .open_file(
                &entry.name,
                OpenAccess::ReadOnly,
                FilePolicy::private(MAX_TRACE_STORAGE_BYTES),
            )
            .map_err(map_budget_custody_error)?;
        let metadata = file.metadata().map_err(map_budget_custody_error)?;
        if metadata.identity != entry.metadata.identity {
            return Err(SealError::StorageLimit);
        }
        total = total
            .checked_add(metadata.len)
            .ok_or(SealError::StorageLimit)?;
        file.verify_named().map_err(map_budget_custody_error)?;
    }
    Ok(total)
}

#[cfg(not(unix))]
fn directory_usage(directory: &Path) -> Result<u64> {
    let mut total = 0u64;
    for entry in std::fs::read_dir(directory)? {
        let entry = entry?;
        let metadata = std::fs::symlink_metadata(entry.path())?;
        if !metadata.file_type().is_file() {
            return Err(SealError::StorageLimit);
        }
        total = total
            .checked_add(metadata.len())
            .ok_or(SealError::StorageLimit)?;
    }
    Ok(total)
}

#[cfg(unix)]
fn map_budget_custody_error(error: CustodyError) -> SealError {
    match error {
        CustodyError::Io(error) => SealError::Io(error),
        CustodyError::InvalidPath(_)
        | CustodyError::UnsafeAncestor(_)
        | CustodyError::UnsafeDirectory(_)
        | CustodyError::UnsafeFile(_)
        | CustodyError::NotFound
        | CustodyError::AlreadyExists
        | CustodyError::LimitExceeded(_)
        | CustodyError::Unsupported(_) => SealError::StorageLimit,
    }
}

#[cfg(test)]
mod tests {
    #[cfg(unix)]
    use std::fs::OpenOptions;
    #[cfg(unix)]
    use std::io::Write;
    #[cfg(unix)]
    use std::os::unix::fs::{OpenOptionsExt, PermissionsExt};
    #[cfg(unix)]
    use std::process::Command;

    use super::*;

    fn assert_unsupported(result: Result<TraceStorageBudget>) {
        assert!(matches!(
            result,
            Err(SealError::Io(error)) if error.kind() == std::io::ErrorKind::Unsupported
        ));
    }

    #[test]
    fn unsupported_platform_rejects_budget_before_directory_access_or_validation() {
        let root = tempfile::tempdir().unwrap();
        let missing = root.path().join("must-not-exist");
        assert_unsupported(TraceStorageBudget::new_for_platform(
            PersistentWriterPlatform::unsupported_for_test(),
            &missing,
            0,
        ));
        assert!(!missing.exists());
    }

    #[cfg(unix)]
    #[test]
    fn unsupported_platform_reconcile_does_not_change_accounting() {
        let directory = tempfile::tempdir().unwrap();
        make_private(directory.path());
        let budget = TraceStorageBudget::new(directory.path(), MIN_TRACE_STORAGE_BYTES).unwrap();
        let before = budget.used();
        assert!(matches!(
            budget.reconcile_for_platform(
                PersistentWriterPlatform::unsupported_for_test(),
                directory.path(),
            ),
            Err(SealError::Io(error)) if error.kind() == std::io::ErrorKind::Unsupported
        ));
        assert_eq!(budget.used(), before);
    }

    #[cfg(unix)]
    fn make_private(directory: &Path) {
        std::fs::set_permissions(directory, std::fs::Permissions::from_mode(0o700)).unwrap();
    }

    #[cfg(not(unix))]
    fn make_private(_directory: &Path) {}

    #[cfg(unix)]
    fn write_private(path: &Path, bytes: &[u8]) {
        let mut file = OpenOptions::new()
            .write(true)
            .create_new(true)
            .mode(0o600)
            .open(path)
            .unwrap();
        file.write_all(bytes).unwrap();
    }

    #[cfg(not(unix))]
    fn write_private(path: &Path, bytes: &[u8]) {
        std::fs::write(path, bytes).unwrap();
    }

    #[cfg_attr(
        not(any(target_os = "linux", target_os = "macos")),
        ignore = "persistent compliance operations are supported only on Linux and macOS"
    )]
    #[test]
    fn normal_charges_cannot_consume_terminal_reserve() {
        let directory = tempfile::tempdir().unwrap();
        make_private(directory.path());
        let limit = MIN_TRACE_STORAGE_BYTES;
        let budget = TraceStorageBudget::new(directory.path(), limit).unwrap();
        budget.charge_normal(TRACE_LIVE_KEY_BYTES).unwrap();
        budget.charge_normal(SEGMENT_HEADER_LEN as u64).unwrap();
        budget.charge_normal(IDENTITY_TRACE_FRAME_BYTES).unwrap();
        assert!(matches!(
            budget.charge_normal(1),
            Err(SealError::StorageLimit)
        ));
        budget
            .charge_terminal(TRACE_TERMINAL_RESERVE_BYTES)
            .unwrap();
        assert_eq!(budget.used(), limit);
    }

    #[test]
    fn capacity_planning_accounts_for_rate_boundaries_segments_and_manifests() {
        let records = TRACE_RATE_WINDOWS_PER_UTC_DAY;
        let manifest = EPOCH_MANIFEST_FIXED_LEN as u64 + EPOCH_MANIFEST_ENTRY_LEN as u64;
        let expected = NETWORK_TRACE_FRAME_BYTES * records
            + SEGMENT_HEADER_LEN as u64
            + SEGMENT_FOOTER_LEN as u64
            + manifest
            + TRACE_LIVE_KEY_BYTES
            + TRACE_TERMINAL_RESERVE_BYTES;
        assert_eq!(
            required_network_trace_storage_bytes(1, 1, records as u32).unwrap(),
            expected.max(MIN_TRACE_STORAGE_BYTES)
        );
        assert!(
            required_identity_trace_storage_bytes(1, 2, records as u32).unwrap()
                > required_identity_trace_storage_bytes(1, 1, records as u32).unwrap()
        );
    }

    #[test]
    fn capacity_planning_rejects_an_epoch_that_cannot_be_manifested() {
        assert!(matches!(
            required_network_trace_storage_bytes(46, 1, 1),
            Err(SealError::StorageLimit)
        ));

        let maximum_rate = u64::from(MAX_EPOCH_MANIFEST_SEGMENTS)
            .checked_mul(u64::from(MAX_SEGMENT_RECORDS))
            .unwrap()
            / TRACE_RATE_WINDOWS_PER_UTC_DAY;
        let maximum_rate = u32::try_from(maximum_rate).unwrap();
        assert!(
            required_network_trace_storage_bytes(maximum_rate, 1, MAX_SEGMENT_RECORDS,).is_ok()
        );
        assert!(matches!(
            required_network_trace_storage_bytes(maximum_rate + 1, 1, MAX_SEGMENT_RECORDS,),
            Err(SealError::StorageLimit)
        ));
    }

    #[cfg_attr(
        not(any(target_os = "linux", target_os = "macos")),
        ignore = "persistent compliance operations are supported only on Linux and macOS"
    )]
    #[test]
    fn exhausted_store_can_reopen_for_recovery_without_new_capture_headroom() {
        let directory = tempfile::tempdir().unwrap();
        make_private(directory.path());
        let limit = MIN_TRACE_STORAGE_BYTES;
        write_private(
            &directory.path().join("accounted"),
            &vec![0u8; limit as usize],
        );
        let budget = TraceStorageBudget::new(directory.path(), limit).unwrap();
        assert_eq!(budget.used(), limit);
        assert!(!budget.has_normal_headroom(1));
        assert!(matches!(
            budget.charge_normal(1),
            Err(SealError::StorageLimit)
        ));
    }

    #[cfg_attr(
        not(any(target_os = "linux", target_os = "macos")),
        ignore = "persistent compliance operations are supported only on Linux and macOS"
    )]
    #[test]
    fn transition_preflight_accounts_for_terminal_peak_release_and_fresh_reserve() {
        let directory = tempfile::tempdir().unwrap();
        make_private(directory.path());
        let limit = MIN_TRACE_STORAGE_BYTES + 1_000;
        let budget = TraceStorageBudget::new(directory.path(), limit).unwrap();
        let used = limit - TRACE_TERMINAL_RESERVE_BYTES - 500;
        budget.charge_normal(used).unwrap();
        assert!(budget.has_transition_headroom(200, used, 300));
        assert!(!budget.has_transition_headroom(600, 0, 1));
        assert!(!budget.has_transition_headroom(0, used + 1, 1));
    }

    #[cfg(unix)]
    #[test]
    fn reconciliation_rejects_directory_replacement_without_redirecting_accounting() {
        let root = tempfile::tempdir().unwrap();
        make_private(root.path());
        let directory = root.path().join("trace");
        std::fs::create_dir(&directory).unwrap();
        make_private(&directory);
        write_private(&directory.join("accounted"), b"1234567");
        let budget = TraceStorageBudget::new(&directory, MIN_TRACE_STORAGE_BYTES).unwrap();
        assert_eq!(budget.used(), 7);

        let displaced = root.path().join("displaced-trace");
        std::fs::rename(&directory, &displaced).unwrap();
        std::fs::create_dir(&directory).unwrap();
        make_private(&directory);
        let replacement = directory.join("replacement");
        write_private(&replacement, b"must-not-be-counted");

        assert!(matches!(
            budget.reconcile(&directory),
            Err(SealError::StorageLimit)
        ));
        assert_eq!(budget.used(), 7);
        assert_eq!(std::fs::read(replacement).unwrap(), b"must-not-be-counted");
    }

    #[cfg(unix)]
    #[test]
    fn budget_scan_rejects_hardlinks_and_fifos_without_blocking() {
        let root = tempfile::tempdir().unwrap();
        make_private(root.path());

        let hardlink_directory = root.path().join("hardlinks");
        std::fs::create_dir(&hardlink_directory).unwrap();
        make_private(&hardlink_directory);
        let original = hardlink_directory.join("original");
        write_private(&original, b"accounted");
        std::fs::hard_link(&original, hardlink_directory.join("alias")).unwrap();
        assert!(matches!(
            TraceStorageBudget::new(&hardlink_directory, MIN_TRACE_STORAGE_BYTES),
            Err(SealError::StorageLimit)
        ));

        let fifo_directory = root.path().join("fifo");
        std::fs::create_dir(&fifo_directory).unwrap();
        make_private(&fifo_directory);
        let fifo = fifo_directory.join("entry");
        assert!(Command::new("mkfifo")
            .arg(&fifo)
            .status()
            .unwrap()
            .success());
        assert!(matches!(
            TraceStorageBudget::new(&fifo_directory, MIN_TRACE_STORAGE_BYTES),
            Err(SealError::StorageLimit)
        ));
    }

    #[cfg(unix)]
    #[test]
    fn budget_scan_streams_past_the_former_collecting_limit() {
        let directory = tempfile::tempdir().unwrap();
        make_private(directory.path());
        for index in 0..=16_384 {
            write_private(&directory.path().join(format!("entry-{index}")), b"");
        }
        let budget = TraceStorageBudget::new(directory.path(), MIN_TRACE_STORAGE_BYTES).unwrap();
        assert_eq!(budget.used(), 0);
    }
}
