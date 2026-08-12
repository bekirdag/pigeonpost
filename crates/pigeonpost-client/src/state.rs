//! Agent state: one SQLite file per agent.
//!
//! **No daemon.** This is a file the library opens, not a service — which is requirement 7 from
//! `docs/product.md` made concrete. An agent wakes, opens the database, drains, and exits.
//!
//! Everything that must outlive a process lives here: the outbox so a send survives being offline,
//! the cursors so mail is never re-read, and the pinned successor commitments that make a key
//! rotation detectable (`docs/keys.md`).

use pigeonpost_compliance_format::{
    attribution_epoch_contains, validate_compliance_epoch, ComplianceKeyId, CompliancePurpose,
    Jurisdiction,
};
use pigeonpost_core::{
    envelope::{Attribution, Wrap},
    keys,
    policy::ATTRIBUTION_REQUIREMENT_LEN,
    record::validate_loft_list,
    Address, AgentRecord, AttributionRequirement, RotationRecord, Token, UntrustedBody,
};
use pigeonpost_directory::DirectoryDocument;
use pigeonpost_registry::{
    validate_handle_transition, AuditedHandleBinding, AuditedHandleMutation, Checkpoint,
    CheckpointPin, ComplianceAuditState, ComplianceKeyPublish, ComplianceKeyStatus, Handle,
    HandleAuditState, HandleProjectionStore, RegistryClient, RegistryError, RegistryTrust,
    VerifiedComplianceKeys, VerifiedHandle, WitnessKey,
};
#[cfg(unix)]
use pigeonpost_unix_custody::{
    CustodyError, DirPolicy, FilePolicy, GuardedDir, GuardedFile, LeafName, NormalizedPath,
    OpenAccess,
};
use rusqlite::{
    params, Connection, OpenFlags, OptionalExtension, Transaction, TransactionBehavior,
};
use std::cell::RefCell;
use std::collections::{HashMap, HashSet};
#[cfg(unix)]
use std::path::{Path, PathBuf};
#[cfg(unix)]
use std::sync::Mutex;
use std::time::{SystemTime, UNIX_EPOCH};

use crate::error::{ClientError, Result, StorageResource};

const SCHEMA_VERSION: i64 = 15;
const V0_1_0_SCHEMA: &str = include_str!("../tests/fixtures/v0_1_0_state.sql");
const SECONDS_PER_DAY: u64 = 24 * 60 * 60;
/// State-only callers which cannot interrogate `/v1/info` retain the historical bounded default.
pub const DEFAULT_LOFT_RETENTION_DAYS: u64 = 30;
/// Compatibility spelling for the default. Network-configured lofts persist authenticated
/// advertised retention, while removal drains for the smaller of that value and this 30-day cap.
pub const LOFT_DRAIN_GRACE_SECS: u64 = DEFAULT_LOFT_RETENTION_DAYS * SECONDS_PER_DAY;
const MAX_CACHED_COMPLIANCE_KEYS: usize = 4_096;
const MAX_COMPLIANCE_AUDIT_BYTES: usize = 4 * 1024 * 1024;
const MAX_HANDLE_AUDIT_BYTES: usize = 256 * 1024;
const MAX_HANDLE_SUBJECT_BYTES: usize = 512;
const MAX_DEAD_LETTER_RESULTS: usize = 1_000;
const MAX_PUBLIC_LIST_RESULTS: usize = 1_000;
const MAX_OUTBOX_REASON_BYTES: usize = 128;
const MAX_MESSAGE_ID_BYTES: usize = 128;
const MAX_STORED_ADDRESS_BYTES: usize = 256;
const MAX_STORED_LOFT_URL_BYTES: usize = pigeonpost_core::record::MAX_AGENT_RECORD_LOFT_URL_BYTES;
const MAX_OUTBOX_WRAP_BYTES: usize = pigeonpost_loft::MAX_EVENT_BYTES;
const MAX_INBOX_BODY_BYTES_PER_MESSAGE: usize = pigeonpost_core::envelope::MAX_PLAINTEXT;
const MAX_PUBLICATION_TARGETS: usize = 64;
const MAX_PUBLICATION_RECORD_BYTES: usize = 64 * 1024;
/// Active routes plus still-draining replacements. The signed active list remains capped at eight;
/// this larger fixed ceiling prevents replacement churn from making every wake unbounded.
pub const MAX_STORED_LOFT_ROUTES: usize = pigeonpost_core::record::MAX_AGENT_RECORD_LOFTS * 4;
/// Trusted directory refresh and snapshot merging are foreground wake work, so configuration is
/// deliberately small and finite.
pub const MAX_CONFIGURED_DIRECTORIES: usize = 16;
const ACCEPT_ALL_META: &str = "accept_all";
const POW_FLOOR_META: &str = "pow_floor";
const TOKEN_LABELS_META: &str = "token_labels";
const TOKEN_GATE_ENABLED_META: &str = "token_gate_enabled";
const ATTRIBUTION_JURISDICTION_META: &str = "attribution_jurisdiction";
const ATTRIBUTION_REQUIRED_META: &str = "attribution_required";
const SENDER_ATTRIBUTION_REQUIREMENT_META: &str = "sender_attribution_requirement_v1";
const RECIPIENT_ATTRIBUTION_REQUIREMENT_META: &str = "recipient_attribution_requirement_v1";
pub(crate) const MAX_TOKEN_LABEL_BYTES: usize = 128;
const MAX_TOKEN_LABELS_META_BYTES: usize = 32 * 1024;
pub(crate) const MAX_POW_FLOOR: u32 = crate::spam::MAX_SUPPORTED_POW_BITS;
#[cfg(any(unix, windows))]
const MAX_SQLITE_FILE_BYTES: u64 = 1 << 40;

/// New databases start with finite local-state budgets. A migration may raise an individual
/// default only as far as needed to admit already-persisted v12 data, never above the audited hard
/// maximum beside it.
pub const DEFAULT_INBOX_MESSAGE_LIMIT: u64 = 10_000;
pub const DEFAULT_INBOX_BODY_BYTES_LIMIT: u64 = 512 * 1024 * 1024;
pub const DEFAULT_OUTBOX_ROW_LIMIT: u64 = 10_000;
pub const DEFAULT_OUTBOX_PAYLOAD_BYTES_LIMIT: u64 = 2 * 1024 * 1024 * 1024;
pub const MAX_INBOX_MESSAGE_LIMIT: u64 = 1_000_000;
pub const MAX_INBOX_BODY_BYTES_LIMIT: u64 = 64 * 1024 * 1024 * 1024;
pub const MAX_OUTBOX_ROW_LIMIT: u64 = 1_000_000;
pub const MAX_OUTBOX_PAYLOAD_BYTES_LIMIT: u64 = 64 * 1024 * 1024 * 1024;
/// Indefinite replay tombstones use their own fixed lifetime-security budget. They never consume
/// the active inbox page quota and are never evicted automatically.
pub const MAX_INBOX_TOMBSTONES: u64 = 1_000_000;

/// Exact confirmation required before a caller may discard an undelivered outbox debt.
pub const PENDING_OUTBOX_DELETE_CONFIRMATION: &str = "delete-undelivered-pigeonpost-copy";
/// Exact confirmation required before bounded bulk removal may include terminal delivery debt.
pub const FINISHED_OUTBOX_PRUNE_CONFIRMATION: &str = "prune-finished-pigeonpost-metadata";

/// Opaque identifier for exactly one durable outbox copy.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct OutboxRecordId(i64);

impl OutboxRecordId {
    pub fn new(value: i64) -> Result<Self> {
        if value <= 0 {
            return Err(ClientError::Config(
                "outbox record id must be positive".into(),
            ));
        }
        Ok(Self(value))
    }

    pub const fn get(self) -> i64 {
        self.0
    }
}

/// Persisted limits for the two client-owned payload stores.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct StorageLimits {
    pub inbox_messages: u64,
    pub inbox_body_bytes: u64,
    pub outbox_rows: u64,
    pub outbox_payload_bytes: u64,
}

impl Default for StorageLimits {
    fn default() -> Self {
        Self {
            inbox_messages: DEFAULT_INBOX_MESSAGE_LIMIT,
            inbox_body_bytes: DEFAULT_INBOX_BODY_BYTES_LIMIT,
            outbox_rows: DEFAULT_OUTBOX_ROW_LIMIT,
            outbox_payload_bytes: DEFAULT_OUTBOX_PAYLOAD_BYTES_LIMIT,
        }
    }
}

/// Exact O(1) usage counters maintained in the same transaction as every client payload change.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct StorageUsage {
    pub inbox_messages: u64,
    /// Indefinite replay-prevention ids, separate from active `inbox_messages`.
    pub inbox_tombstones: u64,
    pub inbox_body_bytes: u64,
    pub outbox_rows: u64,
    pub outbox_payload_bytes: u64,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct StorageStatus {
    pub limits: StorageLimits,
    pub usage: StorageUsage,
    pub inbox_tombstone_limit: u64,
}

/// A message after it has been opened, as the agent's owner sees it.
#[derive(Clone)]
pub struct StoredMessage {
    pub id: String,
    pub from_pubkey: [u8; 32],
    pub from_address: String,
    pub received_at: u64,
    pub read: bool,
    /// `accepted` or `pending` (`docs/spam.md` layer 4).
    pub state: String,
    /// Recipient-side verification against independently witnessed compliance-key history.
    pub attribution: Attribution,
    pub body: UntrustedBody,
}

impl core::fmt::Debug for StoredMessage {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        // Includes decrypted sender-controlled text and identity metadata.
        f.write_str("StoredMessage(<withheld>)")
    }
}

/// One compliance public key from a durable, witnessed registry snapshot.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CachedComplianceKey {
    pub publication: ComplianceKeyPublish,
    pub public_key: [u8; 32],
    pub log_index: u64,
}

/// One undelivered copy: a message still owed to one specific loft.
#[derive(Clone)]
pub struct OutboxEntry {
    pub row: i64,
    pub message_id: String,
    pub loft_url: String,
    pub wrap: Wrap,
    /// Bearer credential retained only in the private client database. It is converted to a
    /// loft-bound presentation immediately before publish and never sent raw.
    pub token: Option<Token>,
    /// The target was independently authorized by explicit local configuration. Network-sourced
    /// URLs never set this bit, even when their signed spelling is a loopback origin.
    pub allow_local: bool,
    pub attempts: u32,
}

impl core::fmt::Debug for OutboxEntry {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        // Contains a complete queued wrap and, optionally, its bearer token.
        f.write_str("OutboxEntry(<withheld>)")
    }
}

/// Durable delivery state for all loft copies of one logical message.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct DeliveryStatus {
    pub delivered: usize,
    pub queued: usize,
    pub terminal: usize,
}

/// One outbox copy which requires operator attention and is no longer retried automatically.
///
/// `reason` is a bounded code generated by the client. Untrusted server response text is never
/// persisted here.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DeadLetter {
    pub row: OutboxRecordId,
    pub message_id: String,
    pub to_addr: String,
    pub loft_url: String,
    pub attempts: u32,
    pub reason: String,
    pub terminal_at: u64,
}

/// Bounded metadata retained after one outbox copy was accepted by its loft. The wrap and bearer
/// token have already been logically erased and are never exposed here.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CompletedDelivery {
    pub row: OutboxRecordId,
    pub message_id: String,
    pub to_addr: String,
    pub loft_url: String,
    pub attempts: u32,
    pub sent_at: u64,
}

/// Payload-free metadata for one copy that is still eligible for automatic delivery retry.
/// Operators use `row` with the exact-confirmation deletion API; wraps and bearer tokens never
/// cross this inspection surface.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PendingDelivery {
    pub row: OutboxRecordId,
    pub message_id: String,
    pub to_addr: String,
    pub loft_url: String,
    pub attempts: u32,
    pub created_at: u64,
    pub next_attempt_at: u64,
    pub last_error: Option<String>,
}

/// One outbox destination plus the independently established permission to use a numeric
/// loopback origin. Keeping these fields together prevents callers from silently dropping route
/// provenance when they enqueue a durable retry.
#[derive(Debug, Clone, Copy)]
pub struct OutboxRoute<'a> {
    pub loft_url: &'a str,
    pub allow_local: bool,
}

impl<'a> OutboxRoute<'a> {
    pub const fn new(loft_url: &'a str, allow_local: bool) -> Self {
        Self {
            loft_url,
            allow_local,
        }
    }
}

/// Configured loft URL, optional pinned service key, and explicit-local provenance.
pub type ConfiguredLoft = (String, Option<[u8; 32]>, bool);

/// What we learned, and pinned, about another agent's address.
#[derive(Debug, Clone)]
pub struct Resolution {
    pub pubkey: [u8; 32],
    pub successor_hash: [u8; 32],
    pub seq: u64,
    pub lofts: Vec<String>,
    /// Proof-of-work this recipient's lofts demand, from their signed agent record.
    pub pow_min: u32,
    /// Recipient-signed exact attribution scope, or `None` for a legacy/optional recipient.
    pub attribution_requirement: Option<AttributionRequirement>,
}

/// A locally-created transition retained for idempotent publication and recovery routing.
#[derive(Debug, Clone)]
pub struct OwnRotation {
    pub record: RotationRecord,
    pub source_record: AgentRecord,
    pub target_record: AgentRecord,
    pub lofts: Vec<String>,
}

/// One exact destination in a durable record or rotation publication plan.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct PublicationTarget {
    pub url: String,
    pub allow_local: bool,
    pub rendezvous: bool,
    pub completed: bool,
}

impl PublicationTarget {
    pub(crate) fn pending(url: String, allow_local: bool, rendezvous: bool) -> Self {
        Self {
            url,
            allow_local,
            rendezvous,
            completed: false,
        }
    }
}

/// The active identity's exact signed record and its durable placement progress.
#[derive(Debug, Clone)]
pub(crate) struct OwnRecordPublication {
    pub address: Address,
    pub record: AgentRecord,
    pub targets: Vec<PublicationTarget>,
}

/// Durable, locally inspectable health of record and rotation placement.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct PlacementState {
    pub record_seq: Option<u64>,
    pub record_targets: usize,
    pub record_pending: usize,
    pub rendezvous_targets: usize,
    pub rendezvous_pending: usize,
    pub rotation_targets: usize,
    pub rotation_pending: usize,
    pub rotations_without_targets: usize,
    pub configured_directories: usize,
    pub directory_refresh_degraded: bool,
    pub last_attempt_at: Option<u64>,
}

impl PlacementState {
    /// True when any exact publication is unfinished or directory-backed rendezvous placement
    /// cannot currently prove a complete three-target set.
    pub fn degraded(&self) -> bool {
        self.directory_refresh_degraded
            || self.record_pending > 0
            || self.rotation_pending > 0
            || self.rotations_without_targets > 0
            || (self.configured_directories > 0
                && self.record_seq.is_some()
                && self.rendezvous_targets < pigeonpost_directory::TARGET_LOFTS)
    }
}

#[derive(Debug, Clone)]
pub struct DirectoryConfig {
    pub url: String,
    pub signing_key: [u8; 32],
    pub last_generated_at: u64,
    pub etag: Option<String>,
    pub snapshot: Option<DirectoryDocument>,
}

/// Durable registry trust and the newest checkpoint this agent has fully verified.
#[derive(Debug, Clone)]
pub struct RegistryConfiguration {
    pub url: String,
    pub trust: RegistryTrust,
    pub checkpoint: Option<Checkpoint>,
    pub witnessed_at: Option<u64>,
}

pub struct State {
    conn: Connection,
    compliance_cache: RefCell<Option<ValidatedComplianceCache>>,
    handle_cache: RefCell<Option<ValidatedHandleCache>>,
    // Rust drops fields in declaration order, so the SQLite connection closes before custody.
    #[cfg(unix)]
    database_custody: Option<StateDatabaseCustody>,
    #[cfg(windows)]
    database_custody: Option<WindowsStateDatabaseCustody>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct SecuritySettings {
    pub accept_all: bool,
    pub pow_floor: u32,
    pub token_labels: Vec<String>,
    pub token_gate_enabled: bool,
    pub attribution_requirement: Option<AttributionRequirement>,
}

struct ValidatedComplianceCache {
    data_version: i64,
    checkpoint: Checkpoint,
    witnessed_at: u64,
    max_age_secs: u64,
    future_skew_secs: u64,
    keys: Vec<CachedComplianceKey>,
    by_id: HashMap<ComplianceKeyId, usize>,
}

struct ValidatedHandleCache {
    data_version: i64,
    validated_at: u64,
    evidence: Option<ValidatedHandleEvidence>,
}

struct ValidatedHandleEvidence {
    audit_size: u64,
    witnessed_at: u64,
    max_age_secs: u64,
    future_skew_secs: u64,
}

/// Disk-backed provisional projection used while the registry stream is still untrusted.
///
/// SQLite's empty filename creates a private temporary database that is deleted on close. The
/// agent database remains read-only for the whole network operation; only after root verification
/// does `apply_to` copy this bounded local delta under one short write transaction.
struct StagedHandleProjection {
    staged: Connection,
    fresh: bool,
    segment_active: bool,
}

impl StagedHandleProjection {
    fn new(base: &Connection) -> Result<Self> {
        let staged = Connection::open("")?;
        staged.pragma_update(None, "temp_store", "FILE")?;
        staged.execute_batch(
            "CREATE TABLE base_projected (
                 handle TEXT PRIMARY KEY,
                 pubkey BLOB NOT NULL CHECK (length(pubkey) = 32),
                 subject TEXT NOT NULL,
                 log_index INTEGER NOT NULL CHECK (log_index >= 0)
             );
             CREATE TABLE projected (
                 handle TEXT PRIMARY KEY,
                 pubkey BLOB NOT NULL CHECK (length(pubkey) = 32),
                 subject TEXT NOT NULL,
                 log_index INTEGER NOT NULL CHECK (log_index >= 0)
             );
             BEGIN IMMEDIATE;",
        )?;
        let mut statement = base.prepare(
            "SELECT handle, pubkey, subject, log_index
             FROM registry_handle_projection
             ORDER BY handle",
        )?;
        let rows = statement.query_map([], |row| {
            Ok((
                row.get::<_, String>(0)?,
                row.get::<_, Vec<u8>>(1)?,
                row.get::<_, String>(2)?,
                row.get::<_, i64>(3)?,
            ))
        })?;
        for row in rows {
            let (handle, pubkey, subject, log_index) = row?;
            staged.execute(
                "INSERT INTO base_projected (handle, pubkey, subject, log_index)
                 VALUES (?1, ?2, ?3, ?4)",
                params![handle, pubkey, subject, log_index],
            )?;
        }
        Ok(Self {
            staged,
            fresh: false,
            segment_active: false,
        })
    }

    fn projection_error() -> RegistryError {
        RegistryError::InvalidConfiguration("local handle-audit staging failed".into())
    }

    fn current(
        &self,
        handle: &Handle,
    ) -> std::result::Result<Option<AuditedHandleBinding>, RegistryError> {
        let staged: Option<(Vec<u8>, String, i64)> = self
            .staged
            .query_row(
                "SELECT pubkey, subject, log_index FROM projected WHERE handle = ?1",
                params![handle.as_path()],
                |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
            )
            .optional()
            .map_err(|_| Self::projection_error())?;
        let stored = if staged.is_some() || self.fresh {
            staged
        } else {
            self.staged
                .query_row(
                    "SELECT pubkey, subject, log_index
                     FROM base_projected WHERE handle = ?1",
                    params![handle.as_path()],
                    |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
                )
                .optional()
                .map_err(|_| Self::projection_error())?
        };
        stored
            .map(|(pubkey, subject, log_index)| {
                let pubkey: [u8; 32] = pubkey.try_into().map_err(|_| Self::projection_error())?;
                let log_index = u64::try_from(log_index).map_err(|_| Self::projection_error())?;
                AuditedHandleBinding::new(handle, pubkey, subject, log_index)
            })
            .transpose()
    }

    fn apply_to(&self, tx: &Transaction<'_>) -> Result<()> {
        if self.fresh {
            tx.execute("DELETE FROM registry_handle_projection", [])?;
        }
        let mut statement = self
            .staged
            .prepare("SELECT handle, pubkey, subject, log_index FROM projected ORDER BY handle")?;
        let rows = statement.query_map([], |row| {
            Ok((
                row.get::<_, String>(0)?,
                row.get::<_, Vec<u8>>(1)?,
                row.get::<_, String>(2)?,
                row.get::<_, i64>(3)?,
            ))
        })?;
        for row in rows {
            let (handle, pubkey, subject, log_index) = row?;
            tx.execute(
                "INSERT INTO registry_handle_projection (handle, pubkey, subject, log_index)
                 VALUES (?1, ?2, ?3, ?4)
                 ON CONFLICT(handle) DO UPDATE SET
                     pubkey = excluded.pubkey,
                     subject = excluded.subject,
                     log_index = excluded.log_index
                 WHERE excluded.log_index > registry_handle_projection.log_index",
                params![handle, pubkey, subject, log_index],
            )?;
        }
        Ok(())
    }
}

impl HandleProjectionStore for StagedHandleProjection {
    fn reset(&mut self) -> std::result::Result<(), RegistryError> {
        if self.segment_active {
            return Err(Self::projection_error());
        }
        self.staged
            .execute("DELETE FROM projected", [])
            .map_err(|_| Self::projection_error())?;
        self.fresh = true;
        Ok(())
    }

    fn begin_segment(&mut self) -> std::result::Result<(), RegistryError> {
        if self.segment_active {
            return Err(Self::projection_error());
        }
        self.staged
            .execute_batch("SAVEPOINT registry_audit_segment;")
            .map_err(|_| Self::projection_error())?;
        self.segment_active = true;
        Ok(())
    }

    fn commit_segment(&mut self) -> std::result::Result<(), RegistryError> {
        if !self.segment_active {
            return Err(Self::projection_error());
        }
        self.staged
            .execute_batch("RELEASE registry_audit_segment;")
            .map_err(|_| Self::projection_error())?;
        self.segment_active = false;
        Ok(())
    }

    fn rollback_segment(&mut self) -> std::result::Result<(), RegistryError> {
        if !self.segment_active {
            return Err(Self::projection_error());
        }
        self.staged
            .execute_batch(
                "ROLLBACK TO registry_audit_segment;
                 RELEASE registry_audit_segment;",
            )
            .map_err(|_| Self::projection_error())?;
        self.segment_active = false;
        Ok(())
    }

    fn apply(
        &mut self,
        mutation: &AuditedHandleMutation,
    ) -> std::result::Result<(), RegistryError> {
        let handle = Handle::parse(mutation.handle())?;
        let previous = self.current(&handle)?;
        validate_handle_transition(previous.as_ref(), mutation)?;
        self.staged
            .execute(
                "INSERT INTO projected (handle, pubkey, subject, log_index)
                 VALUES (?1, ?2, ?3, ?4)
                 ON CONFLICT(handle) DO UPDATE SET
                     pubkey = excluded.pubkey,
                     subject = excluded.subject,
                     log_index = excluded.log_index",
                params![
                    mutation.handle(),
                    mutation.pubkey().as_slice(),
                    mutation.subject(),
                    i64::try_from(mutation.log_index()).map_err(|_| Self::projection_error())?,
                ],
            )
            .map_err(|_| Self::projection_error())?;
        Ok(())
    }

    fn binding(
        &mut self,
        handle: &Handle,
    ) -> std::result::Result<Option<AuditedHandleBinding>, RegistryError> {
        self.current(handle)
    }
}

impl State {
    pub fn open(path: &std::path::Path) -> Result<Self> {
        Self::open_after_custody_check(path, || {})
    }

    fn open_after_custody_check<F>(path: &std::path::Path, before_sqlite_open: F) -> Result<Self>
    where
        F: FnOnce(),
    {
        crate::keystore::require_supported_persistent_storage()?;

        #[cfg(unix)]
        {
            let custody = StateDatabaseCustody::open_or_create(path)?;
            let sqlite_path = custody.path.clone();
            custody.verify_main_named()?;
            before_sqlite_open();

            let flags = OpenFlags::SQLITE_OPEN_READ_WRITE
                | OpenFlags::SQLITE_OPEN_NO_MUTEX
                | OpenFlags::SQLITE_OPEN_NOFOLLOW;
            let conn = Connection::open_with_flags(&sqlite_path, flags)?;
            custody.verify_sqlite_connection(&conn)?;

            let mut state = Self::init(conn)?;
            custody.verify_all_named()?;
            state.database_custody = Some(custody);
            state.verify_database_custody()?;
            Ok(state)
        }

        #[cfg(windows)]
        {
            let custody = WindowsStateDatabasePreparation::open_or_create(path)?;
            let sqlite_path = custody.path.clone();
            custody.verify_main_named()?;
            before_sqlite_open();

            let flags = OpenFlags::SQLITE_OPEN_READ_WRITE
                | OpenFlags::SQLITE_OPEN_NO_MUTEX
                | OpenFlags::SQLITE_OPEN_NOFOLLOW;
            let conn = Connection::open_with_flags(&sqlite_path, flags)?;
            custody.verify_sqlite_connection(&conn)?;

            let mut state = Self::init(conn)?;
            let custody = custody.finish()?;
            custody.verify_all_named()?;
            state.database_custody = Some(custody);
            state.verify_database_custody()?;
            Ok(state)
        }

        #[cfg(not(any(unix, windows)))]
        {
            let _ = (path, before_sqlite_open);
            Err(crate::keystore::unsupported_persistent_storage_error())
        }
    }

    pub fn in_memory() -> Result<Self> {
        Self::init(Connection::open_in_memory()?)
    }

    fn init(mut conn: Connection) -> Result<Self> {
        conn.pragma_update(None, "journal_mode", "WAL")?;
        conn.pragma_update(None, "synchronous", "FULL")?;
        // Separate agent processes share this WAL, and `synchronous = FULL` fsyncs every commit, so
        // under heavy multi-connection write contention a single write can wait a while for the lock.
        // 30s gives enough headroom to serialize rather than surface a spurious SQLITE_BUSY.
        conn.pragma_update(None, "busy_timeout", 30_000)?;
        // This reduces payload remnants in ordinary SQLite pages, but WAL frames, filesystem
        // snapshots, and backups remain outside SQLite's guarantee. Public documentation therefore
        // promises logical deletion only.
        conn.pragma_update(None, "secure_delete", "FAST")?;

        migrate(&mut conn)?;

        let state = State {
            conn,
            compliance_cache: RefCell::new(None),
            handle_cache: RefCell::new(None),
            #[cfg(any(unix, windows))]
            database_custody: None,
        };
        // Keep this check at the persistence boundary as well as in migration invariants. A caller
        // must never receive a usable State whose signed-policy inputs are malformed.
        state.security_settings()?;
        state.record_publication()?;
        state.prune_expired_own_rotations(
            current_time_secs()?,
            crate::keystore::MAX_LIVE_RETIRED_IDENTITIES,
        )?;
        state.own_rotations()?;
        state.placement_state()?;
        Ok(state)
    }

    #[cfg(unix)]
    fn verify_database_custody(&self) -> Result<()> {
        self.database_custody
            .as_ref()
            .ok_or_else(|| {
                ClientError::Config("persistent state has no filesystem custody".into())
            })?
            .verify_all_named()
    }

    #[cfg(windows)]
    fn verify_database_custody(&self) -> Result<()> {
        self.database_custody
            .as_ref()
            .ok_or_else(|| {
                ClientError::Config("persistent state has no filesystem custody".into())
            })?
            .verify_all_named()
    }

    // ---- meta -----------------------------------------------------------------------------

    pub fn get_meta(&self, key: &str) -> Result<Option<String>> {
        Ok(self
            .conn
            .query_row("SELECT value FROM meta WHERE key = ?1", params![key], |r| {
                r.get(0)
            })
            .optional()?)
    }

    pub fn set_meta(&self, key: &str, value: &str) -> Result<()> {
        self.conn.execute(
            "INSERT INTO meta (key, value) VALUES (?1, ?2)
             ON CONFLICT(key) DO UPDATE SET value = excluded.value",
            params![key, value],
        )?;
        Ok(())
    }

    /// Return persisted storage policy and usage without scanning payload tables.
    pub fn storage_status(&self) -> Result<StorageStatus> {
        load_storage_status(&self.conn)
    }

    /// Replace all storage limits atomically. Limits are finite, cannot exceed the audited hard
    /// maxima, and cannot be lowered below bytes or rows already retained.
    pub fn set_storage_limits(&self, limits: StorageLimits) -> Result<StorageStatus> {
        validate_storage_limits(limits)?;
        let tx = Transaction::new_unchecked(&self.conn, TransactionBehavior::Immediate)?;
        let current = load_storage_status(&tx)?;
        if limits.inbox_messages < current.usage.inbox_messages
            || limits.inbox_body_bytes < current.usage.inbox_body_bytes
            || limits.outbox_rows < current.usage.outbox_rows
            || limits.outbox_payload_bytes < current.usage.outbox_payload_bytes
        {
            return Err(ClientError::Config(
                "storage limits cannot be lower than current usage".into(),
            ));
        }
        tx.execute(
            "UPDATE storage_accounting SET
                 inbox_message_limit = ?1,
                 inbox_body_bytes_limit = ?2,
                 outbox_row_limit = ?3,
                 outbox_payload_bytes_limit = ?4
             WHERE id = 1",
            params![
                sqlite_i64(limits.inbox_messages, "inbox message limit")?,
                sqlite_i64(limits.inbox_body_bytes, "inbox body-byte limit")?,
                sqlite_i64(limits.outbox_rows, "outbox row limit")?,
                sqlite_i64(limits.outbox_payload_bytes, "outbox payload-byte limit")?,
            ],
        )?;
        tx.commit()?;
        self.storage_status()
    }

    pub(crate) fn security_settings(&self) -> Result<SecuritySettings> {
        Ok(SecuritySettings {
            accept_all: self.accept_all()?,
            pow_floor: self.pow_floor()?,
            token_labels: self.token_labels()?,
            token_gate_enabled: self.token_gate_enabled()?,
            attribution_requirement: self.recipient_attribution_requirement()?,
        })
    }

    pub(crate) fn accept_all(&self) -> Result<bool> {
        Ok(
            parse_bool_meta(ACCEPT_ALL_META, self.get_meta(ACCEPT_ALL_META)?.as_deref())?
                .unwrap_or(false),
        )
    }

    pub(crate) fn set_accept_all(&self, value: bool) -> Result<()> {
        self.set_meta(ACCEPT_ALL_META, canonical_bool(value))
    }

    pub(crate) fn pow_floor(&self) -> Result<u32> {
        Ok(parse_pow_floor_meta(self.get_meta(POW_FLOOR_META)?.as_deref())?.unwrap_or(0))
    }

    pub(crate) fn set_pow_floor(&self, value: u32) -> Result<()> {
        if value > MAX_POW_FLOOR {
            return Err(invalid_security_meta(POW_FLOOR_META));
        }
        self.set_meta(POW_FLOOR_META, &value.to_string())
    }

    pub(crate) fn token_labels(&self) -> Result<Vec<String>> {
        Ok(
            parse_token_labels_meta(self.get_meta(TOKEN_LABELS_META)?.as_deref())?
                .unwrap_or_default(),
        )
    }

    pub(crate) fn set_token_labels(&self, labels: &[String]) -> Result<()> {
        validate_token_labels(labels)?;
        self.set_meta(TOKEN_LABELS_META, &serde_json::to_string(labels)?)
    }

    pub(crate) fn token_gate_enabled(&self) -> Result<bool> {
        Ok(parse_bool_meta(
            TOKEN_GATE_ENABLED_META,
            self.get_meta(TOKEN_GATE_ENABLED_META)?.as_deref(),
        )?
        .unwrap_or(false))
    }

    pub(crate) fn set_token_gate_enabled(&self, value: bool) -> Result<()> {
        self.set_meta(TOKEN_GATE_ENABLED_META, canonical_bool(value))
    }

    /// Sequence number for the agent's own published record. Increments on every republish so a
    /// stale record can never overwrite a newer one.
    pub fn next_record_seq(&self) -> Result<u64> {
        self.next_meta_counter("record_seq")
    }

    pub fn ensure_record_seq_at_least(&self, sequence: u64) -> Result<()> {
        self.conn.execute(
            "INSERT INTO meta (key, value) VALUES ('record_seq', ?1)
             ON CONFLICT(key) DO UPDATE SET value = MAX(
                 CAST(meta.value AS INTEGER), CAST(excluded.value AS INTEGER)
             )",
            params![sequence.to_string()],
        )?;
        Ok(())
    }

    /// Sequence number for recipient-policy updates. This is one atomic SQLite statement so two
    /// concurrently waking clients cannot sign the same sequence or move the counter backwards.
    pub fn next_policy_seq(&self) -> Result<u64> {
        self.next_meta_counter("policy_seq")
    }

    fn next_meta_counter(&self, key: &str) -> Result<u64> {
        let next: i64 = self.conn.query_row(
            "INSERT INTO meta (key, value) VALUES (?1, '1')
             ON CONFLICT(key) DO UPDATE
                 SET value = CAST(meta.value AS INTEGER) + 1
             RETURNING CAST(value AS INTEGER)",
            params![key],
            |row| row.get(0),
        )?;
        u64::try_from(next).map_err(|_| ClientError::Config("state counter overflowed".into()))
    }

    // ---- lofts ----------------------------------------------------------------------------

    pub fn add_loft(&self, url: &str, pubkey: Option<[u8; 32]>, now: u64) -> Result<()> {
        self.add_loft_with_retention_and_local_trust(
            url,
            pubkey,
            DEFAULT_LOFT_RETENTION_DAYS,
            now,
            false,
        )
    }

    /// Persist whether an exact-loopback origin was independently authorized by local
    /// configuration. The flag is intentionally orthogonal to URL syntax: callers must prove the
    /// provenance before setting it, and public HTTPS still uses the hardened network path.
    pub fn add_loft_with_local_trust(
        &self,
        url: &str,
        pubkey: Option<[u8; 32]>,
        now: u64,
        allow_local: bool,
    ) -> Result<()> {
        self.add_loft_with_retention_and_local_trust(
            url,
            pubkey,
            DEFAULT_LOFT_RETENTION_DAYS,
            now,
            allow_local,
        )
    }

    /// Persist the exact retention advertised by the authenticated loft endpoint. Removal later
    /// freezes the network-contract drain window derived from this value.
    pub fn add_loft_with_retention_and_local_trust(
        &self,
        url: &str,
        pubkey: Option<[u8; 32]>,
        retention_days: u64,
        now: u64,
        allow_local: bool,
    ) -> Result<()> {
        validate_loft_list(&[url.to_owned()])?;
        validate_loft_retention_days(retention_days)?;
        let retention_days = sqlite_i64(retention_days, "loft advertised retention")?;
        let now = sqlite_i64(now, "loft addition timestamp")?;
        let tx = Transaction::new_unchecked(&self.conn, TransactionBehavior::Immediate)?;
        finalize_expired_lofts_tx(&tx, now)?;
        let existing: Option<(String, i64)> = tx
            .query_row(
                "SELECT state, retention_days FROM lofts WHERE url = ?1",
                params![url],
                |row| Ok((row.get(0)?, row.get(1)?)),
            )
            .optional()?;
        let existing_state = existing.as_ref().map(|(state, _)| state.as_str());
        if let Some((_, stored_retention)) = existing.as_ref() {
            validate_loft_retention_days(sqlite_u64(
                *stored_retention,
                "stored loft advertised retention",
            )?)?;
        }
        if existing_state.is_some_and(|state| state != "active" && state != "draining") {
            return Err(ClientError::Config("stored loft state is malformed".into()));
        }
        let needs_active_slot = existing_state != Some("active");
        let active_full: bool = tx.query_row(
            "SELECT EXISTS(
                 SELECT 1 FROM lofts WHERE state = 'active'
                 ORDER BY url LIMIT 1 OFFSET ?1
             )",
            params![(pigeonpost_core::record::MAX_AGENT_RECORD_LOFTS - 1) as i64],
            |row| row.get(0),
        )?;
        let route_set_full: bool = tx.query_row(
            "SELECT EXISTS(SELECT 1 FROM lofts ORDER BY url LIMIT 1 OFFSET ?1)",
            params![(MAX_STORED_LOFT_ROUTES - 1) as i64],
            |row| row.get(0),
        )?;
        if (needs_active_slot && active_full) || (existing.is_none() && route_set_full) {
            // Expiry cleanup is useful maintenance even when this distinct admission is refused.
            tx.commit()?;
            return Err(ClientError::Core(pigeonpost_core::Error::TooLarge));
        }
        tx.execute(
            "INSERT INTO lofts (url, pubkey, added_at, allow_local, retention_days)
             VALUES (?1, ?2, ?3, ?4, ?5)
             ON CONFLICT(url) DO UPDATE SET
                 pubkey = excluded.pubkey,
                 state = 'active',
                 drain_after = NULL,
                 retention_days = excluded.retention_days,
                 allow_local = MAX(lofts.allow_local, excluded.allow_local)",
            params![
                url,
                pubkey.map(|p| p.to_vec()),
                now,
                i64::from(allow_local),
                retention_days
            ],
        )?;
        if allow_local {
            tx.execute(
                "UPDATE outbox SET allow_local = 1
                 WHERE loft_url = ?1 AND sent_at IS NULL",
                params![url],
            )?;
        }
        tx.commit()?;
        Ok(())
    }

    /// Stop advertising a loft immediately, but retain its read path for a fixed grace interval.
    /// Repeating removal is idempotent and never extends the original deadline.
    pub fn remove_loft(&self, url: &str, now: u64) -> Result<bool> {
        let tx = Transaction::new_unchecked(&self.conn, TransactionBehavior::Immediate)?;
        let retention_days: Option<i64> = tx
            .query_row(
                "SELECT retention_days FROM lofts WHERE url = ?1",
                params![url],
                |row| row.get(0),
            )
            .optional()?;
        if let Some(retention_days) = retention_days {
            let retention_days = sqlite_u64(retention_days, "stored loft advertised retention")?;
            validate_loft_retention_days(retention_days)?;
            let drain_after = retention_days
                .min(DEFAULT_LOFT_RETENTION_DAYS)
                .checked_mul(SECONDS_PER_DAY)
                .and_then(|grace| now.checked_add(grace))
                .and_then(|deadline| i64::try_from(deadline).ok())
                .ok_or_else(|| ClientError::Config("loft drain deadline overflowed".into()))?;
            tx.execute(
                "UPDATE lofts
                 SET state = 'draining', drain_after = COALESCE(drain_after, ?2)
                 WHERE url = ?1",
                params![url, drain_after],
            )?;
        }
        tx.commit()?;
        Ok(retention_days.is_some())
    }

    /// Permanently forget expired drain routes and revoke any local-network authorization they
    /// had lent to durable retries. This is called at agent startup and before every drain query.
    pub fn finalize_expired_lofts(&self, now: u64) -> Result<usize> {
        let now = sqlite_i64(now, "loft drain clock")?;
        let tx = Transaction::new_unchecked(&self.conn, TransactionBehavior::Immediate)?;
        let removed = finalize_expired_lofts_tx(&tx, now)?;
        tx.commit()?;
        Ok(removed)
    }

    pub fn lofts(&self) -> Result<Vec<(String, Option<[u8; 32]>)>> {
        let mut stmt = self.conn.prepare(
            "SELECT url, pubkey, retention_days FROM lofts WHERE state = 'active'
             ORDER BY added_at, url LIMIT ?1",
        )?;
        let rows = stmt.query_map(
            params![(pigeonpost_core::record::MAX_AGENT_RECORD_LOFTS + 1) as i64],
            |row| {
                let url: String = row.get(0)?;
                let pubkey: Option<Vec<u8>> = row.get(1)?;
                let retention_days: i64 = row.get(2)?;
                Ok((url, pubkey, retention_days))
            },
        )?;

        let mut out = Vec::new();
        for row in rows {
            let (url, pubkey, retention_days) = row?;
            validate_loft_retention_days(sqlite_u64(
                retention_days,
                "stored loft advertised retention",
            )?)?;
            let pubkey = pubkey
                .map(|bytes| fixed32(bytes, "stored loft public key"))
                .transpose()?;
            out.push((url, pubkey));
        }
        if out.len() > pigeonpost_core::record::MAX_AGENT_RECORD_LOFTS {
            return Err(ClientError::Config(
                "active loft set exceeds the signed-record bound".into(),
            ));
        }
        Ok(out)
    }

    pub fn lofts_with_local_trust(&self) -> Result<Vec<ConfiguredLoft>> {
        let mut stmt = self.conn.prepare(
            "SELECT url, pubkey, allow_local, retention_days FROM lofts
             WHERE state = 'active' ORDER BY added_at, url LIMIT ?1",
        )?;
        let rows = stmt.query_map(
            params![(pigeonpost_core::record::MAX_AGENT_RECORD_LOFTS + 1) as i64],
            |row| {
                Ok((
                    row.get::<_, String>(0)?,
                    row.get::<_, Option<Vec<u8>>>(1)?,
                    row.get::<_, bool>(2)?,
                    row.get::<_, i64>(3)?,
                ))
            },
        )?;

        let mut out = Vec::new();
        for row in rows {
            let (url, pubkey, allow_local, retention_days) = row?;
            validate_loft_retention_days(sqlite_u64(
                retention_days,
                "stored loft advertised retention",
            )?)?;
            let pubkey = pubkey
                .map(|bytes| fixed32(bytes, "stored loft public key"))
                .transpose()?;
            out.push((url, pubkey, allow_local));
        }
        if out.len() > pigeonpost_core::record::MAX_AGENT_RECORD_LOFTS {
            return Err(ClientError::Config(
                "active loft set exceeds the signed-record bound".into(),
            ));
        }
        Ok(out)
    }

    /// Lofts from which mail must still be collected: active routes plus unexpired replacements.
    /// Expiry cleanup is part of the query so a long-running process cannot retain a route past its
    /// deadline merely because it has not restarted.
    pub fn lofts_for_drain_with_local_trust(&self, now: u64) -> Result<Vec<ConfiguredLoft>> {
        self.finalize_expired_lofts(now)?;
        let now = i64::try_from(now)
            .map_err(|_| ClientError::Config("loft drain clock overflowed".into()))?;
        let mut stmt = self.conn.prepare(
            "SELECT url, pubkey, allow_local, retention_days FROM lofts
             WHERE state = 'active'
                OR (state = 'draining' AND drain_after IS NOT NULL AND drain_after > ?1)
             ORDER BY added_at, url LIMIT ?2",
        )?;
        let rows = stmt.query_map(params![now, (MAX_STORED_LOFT_ROUTES + 1) as i64], |row| {
            Ok((
                row.get::<_, String>(0)?,
                row.get::<_, Option<Vec<u8>>>(1)?,
                row.get::<_, bool>(2)?,
                row.get::<_, i64>(3)?,
            ))
        })?;

        let mut out = Vec::new();
        for row in rows {
            let (url, pubkey, allow_local, retention_days) = row?;
            validate_loft_retention_days(sqlite_u64(
                retention_days,
                "stored loft advertised retention",
            )?)?;
            let pubkey = pubkey
                .map(|bytes| fixed32(bytes, "stored loft public key"))
                .transpose()?;
            out.push((url, pubkey, allow_local));
        }
        if out.len() > MAX_STORED_LOFT_ROUTES {
            return Err(ClientError::Config(
                "active and draining loft set exceeds its fixed bound".into(),
            ));
        }
        Ok(out)
    }

    // ---- trusted directories --------------------------------------------------------------

    /// Returns true only when this call inserted a new pinned directory.
    pub fn add_directory(&self, url: &str, signing_key: &[u8; 32], now: u64) -> Result<bool> {
        let added_at = sqlite_i64(now, "directory addition timestamp")?;
        if url.is_empty() || url.len() > MAX_STORED_LOFT_URL_BYTES {
            return Err(ClientError::Config("directory URL is malformed".into()));
        }
        let tx = Transaction::new_unchecked(&self.conn, TransactionBehavior::Immediate)?;
        let existing: Option<Vec<u8>> = tx
            .query_row(
                "SELECT signing_key FROM directories WHERE url = ?1",
                params![url],
                |row| row.get(0),
            )
            .optional()?;
        if existing
            .as_deref()
            .is_some_and(|known| known != signing_key.as_slice())
        {
            return Err(ClientError::Config(
                "directory signing-key change requires explicit removal first".into(),
            ));
        }
        if existing.is_none() {
            let full: bool = tx.query_row(
                "SELECT EXISTS(
                     SELECT 1 FROM directories ORDER BY url LIMIT 1 OFFSET ?1
                 )",
                params![(MAX_CONFIGURED_DIRECTORIES - 1) as i64],
                |row| row.get(0),
            )?;
            if full {
                return Err(ClientError::Core(pigeonpost_core::Error::TooLarge));
            }
        }
        tx.execute(
            "INSERT INTO directories (url, signing_key, added_at)
             VALUES (?1, ?2, ?3)
             ON CONFLICT(url) DO UPDATE SET enabled = 1",
            params![url, signing_key.as_slice(), added_at],
        )?;
        tx.commit()?;
        Ok(existing.is_none())
    }

    /// Remove one exact directory pin and its cached snapshot. Key rollover intentionally
    /// requires this explicit step before a different key can be added for the same URL.
    pub fn remove_directory(&self, url: &str) -> Result<bool> {
        if url.is_empty() || url.len() > MAX_STORED_LOFT_URL_BYTES {
            return Err(ClientError::Config("directory URL is malformed".into()));
        }
        Ok(self
            .conn
            .execute("DELETE FROM directories WHERE url = ?1", params![url])?
            > 0)
    }

    /// Roll back only a newly-added pin which still has no verified snapshot. A concurrent
    /// successful refresh wins and prevents this cleanup from deleting useful trust state.
    pub(crate) fn remove_uninitialized_directory(
        &self,
        url: &str,
        signing_key: &[u8; 32],
    ) -> Result<bool> {
        Ok(self.conn.execute(
            "DELETE FROM directories
             WHERE url = ?1 AND signing_key = ?2
               AND snapshot IS NULL AND etag IS NULL AND last_generated_at = 0",
            params![url, signing_key.as_slice()],
        )? > 0)
    }

    pub fn directories(&self) -> Result<Vec<DirectoryConfig>> {
        let mut statement = self.conn.prepare(
            "SELECT url, signing_key, last_generated_at, etag, snapshot
             FROM directories WHERE enabled = 1 ORDER BY added_at, url LIMIT ?1",
        )?;
        let rows =
            statement.query_map(params![(MAX_CONFIGURED_DIRECTORIES + 1) as i64], |row| {
                Ok((
                    row.get::<_, String>(0)?,
                    row.get::<_, Vec<u8>>(1)?,
                    row.get::<_, i64>(2)?,
                    row.get::<_, Option<String>>(3)?,
                    row.get::<_, Option<Vec<u8>>>(4)?,
                ))
            })?;
        let mut directories = Vec::new();
        for row in rows {
            let (url, signing_key, generated_at, etag, snapshot) = row?;
            directories.push(DirectoryConfig {
                url,
                signing_key: signing_key
                    .try_into()
                    .map_err(|_| ClientError::Config("stored directory key is malformed".into()))?,
                last_generated_at: u64::try_from(generated_at).map_err(|_| {
                    ClientError::Config("stored directory timestamp is malformed".into())
                })?,
                etag,
                snapshot: snapshot
                    .map(|encoded| serde_json::from_slice(&encoded))
                    .transpose()?,
            });
        }
        if directories.len() > MAX_CONFIGURED_DIRECTORIES {
            return Err(ClientError::Config(
                "configured directory set exceeds its fixed bound".into(),
            ));
        }
        Ok(directories)
    }

    pub fn save_directory_snapshot(
        &self,
        url: &str,
        document: &DirectoryDocument,
        etag: Option<&str>,
    ) -> Result<()> {
        let encoded = serde_json::to_vec(document)?;
        let generated_at = i64::try_from(document.generated_at)
            .map_err(|_| ClientError::Config("directory timestamp overflowed".into()))?;
        let changed = self.conn.execute(
            "UPDATE directories
             SET last_generated_at = ?2, etag = ?3, snapshot = ?4
             WHERE url = ?1 AND enabled = 1
               AND (last_generated_at < ?2
                    OR (last_generated_at = ?2
                        AND (snapshot IS NULL OR snapshot = ?4)))",
            params![url, generated_at, etag, encoded],
        )?;
        if changed == 0 {
            let configured = self
                .conn
                .query_row(
                    "SELECT 1 FROM directories WHERE url = ?1 AND enabled = 1",
                    params![url],
                    |_| Ok(()),
                )
                .optional()?
                .is_some();
            if !configured {
                return Err(ClientError::Config("directory is not configured".into()));
            }
            return Err(ClientError::Core(pigeonpost_core::Error::StaleSequence));
        }
        Ok(())
    }

    // ---- trusted registry ----------------------------------------------------------------

    /// Persist the out-of-band registry trust policy. A configured policy is immutable through
    /// this method: replacing any key, witness, origin, or minimum checkpoint requires an explicit
    /// reset so a network response can never silently rewrite the trust root.
    pub(crate) fn configure_registry(
        &self,
        url: &str,
        trust: &RegistryTrust,
        now: u64,
    ) -> Result<()> {
        if let Some(existing) = self.registry_configuration()? {
            if existing.url == url && same_registry_trust(&existing.trust, trust) {
                return Ok(());
            }
            return Err(ClientError::Config(
                "registry trust change requires an explicit reset".into(),
            ));
        }

        let tx = Transaction::new_unchecked(&self.conn, TransactionBehavior::Immediate)?;
        let minimum = trust.minimum_checkpoint();
        tx.execute(
            "INSERT INTO registry_config
                 (id, url, origin, checkpoint_key, witness_threshold, minimum_size,
                  minimum_root, max_cosignature_age, future_clock_skew, added_at)
             VALUES (1, ?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9)",
            params![
                url,
                trust.expected_origin(),
                trust.checkpoint_key().as_bytes().as_slice(),
                sqlite_i64(trust.witness_threshold() as u64, "witness threshold")?,
                sqlite_i64(minimum.size, "minimum checkpoint size")?,
                minimum.root.as_slice(),
                sqlite_i64(trust.max_cosignature_age_secs(), "maximum cosignature age")?,
                sqlite_i64(trust.future_clock_skew_secs(), "future clock skew")?,
                sqlite_i64(now, "registry configuration timestamp")?,
            ],
        )?;
        for witness in trust.witnesses() {
            tx.execute(
                "INSERT INTO registry_witnesses (name, pubkey) VALUES (?1, ?2)",
                params![witness.name(), witness.key().as_bytes().as_slice()],
            )?;
        }
        tx.commit()?;
        *self.compliance_cache.borrow_mut() = None;
        *self.handle_cache.borrow_mut() = None;
        Ok(())
    }

    pub fn registry_configuration(&self) -> Result<Option<RegistryConfiguration>> {
        type StoredRegistry = (String, String, Vec<u8>, i64, i64, Vec<u8>, i64, i64);
        let stored: Option<StoredRegistry> = self
            .conn
            .query_row(
                "SELECT url, origin, checkpoint_key, witness_threshold, minimum_size,
                        minimum_root, max_cosignature_age, future_clock_skew
                 FROM registry_config WHERE id = 1",
                [],
                |row| {
                    Ok((
                        row.get(0)?,
                        row.get(1)?,
                        row.get(2)?,
                        row.get(3)?,
                        row.get(4)?,
                        row.get(5)?,
                        row.get(6)?,
                        row.get(7)?,
                    ))
                },
            )
            .optional()?;
        let Some((
            url,
            origin,
            checkpoint_key,
            threshold,
            minimum_size,
            minimum_root,
            max_age,
            skew,
        )) = stored
        else {
            return Ok(None);
        };

        let checkpoint_key = fixed32(checkpoint_key, "stored registry checkpoint key")?;
        let minimum_root = fixed32(minimum_root, "stored minimum checkpoint root")?;
        let mut statement = self
            .conn
            .prepare("SELECT name, pubkey FROM registry_witnesses ORDER BY name")?;
        let rows = statement.query_map([], |row| {
            Ok((row.get::<_, String>(0)?, row.get::<_, Vec<u8>>(1)?))
        })?;
        let mut witnesses = Vec::new();
        for row in rows {
            let (name, key) = row?;
            let key = keys::verifying_key_from_bytes(&fixed32(key, "stored registry witness key")?)
                .map_err(|_| {
                    ClientError::Config("stored registry witness key is invalid".into())
                })?;
            witnesses.push(WitnessKey::new(name, key)?);
        }
        let trust = RegistryTrust::new(
            origin.clone(),
            checkpoint_key,
            witnesses,
            usize::try_from(sqlite_u64(threshold, "stored witness threshold")?).map_err(|_| {
                ClientError::Config("stored witness threshold exceeds this platform".into())
            })?,
            CheckpointPin {
                size: sqlite_u64(minimum_size, "stored minimum checkpoint size")?,
                root: minimum_root,
            },
            sqlite_u64(max_age, "stored maximum cosignature age")?,
            sqlite_u64(skew, "stored future clock skew")?,
        )?;

        let pin: Option<(i64, Vec<u8>, Option<i64>)> = self
            .conn
            .query_row(
                "SELECT size, root, witnessed_at FROM registry_pin WHERE id = 1",
                [],
                |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
            )
            .optional()?;
        let (checkpoint, witnessed_at) = match pin {
            Some((size, root, witnessed_at)) => (
                Some(Checkpoint {
                    origin,
                    size: sqlite_u64(size, "stored registry checkpoint size")?,
                    root: fixed32(root, "stored registry checkpoint root")?,
                }),
                witnessed_at
                    .map(|value| sqlite_u64(value, "stored witness timestamp"))
                    .transpose()?,
            ),
            None => (None, None),
        };

        Ok(Some(RegistryConfiguration {
            url,
            trust,
            checkpoint,
            witnessed_at,
        }))
    }

    /// Advance the durable checkpoint only after the caller has verified the full handle leaf,
    /// inclusion path, witness quorum, and consistency proof.
    pub fn save_registry_checkpoint(
        &self,
        checkpoint: &Checkpoint,
        witnessed_at: Option<u64>,
    ) -> Result<()> {
        // Reserve the writer before reading the pin. Separate agent processes share this WAL, so a
        // pre-transaction read followed by a conditional UPSERT can otherwise lose a race while
        // still reporting success to its caller.
        let tx = Transaction::new_unchecked(&self.conn, TransactionBehavior::Immediate)?;
        let configured = self
            .registry_configuration()?
            .ok_or_else(|| ClientError::Config("registry is not configured".into()))?;
        if checkpoint.origin != configured.trust.expected_origin()
            || (configured.trust.witness_threshold() > 0 && witnessed_at.is_none())
        {
            return Err(ClientError::Config(
                "registry checkpoint does not satisfy configured trust".into(),
            ));
        }
        if let Some(known) = configured.checkpoint {
            if checkpoint.size < known.size
                || (checkpoint.size == known.size && checkpoint.root != known.root)
            {
                return Err(ClientError::Core(pigeonpost_core::Error::StaleSequence));
            }
        }
        let changed = tx.execute(
            "INSERT INTO registry_pin (id, size, root, witnessed_at)
             VALUES (1, ?1, ?2, ?3)
             ON CONFLICT(id) DO UPDATE SET
                 size = excluded.size,
                 root = excluded.root,
                 witnessed_at = CASE
                     WHEN excluded.size > registry_pin.size THEN excluded.witnessed_at
                     WHEN registry_pin.witnessed_at IS NULL THEN excluded.witnessed_at
                     WHEN excluded.witnessed_at IS NULL THEN registry_pin.witnessed_at
                     ELSE MAX(registry_pin.witnessed_at, excluded.witnessed_at)
                 END
             WHERE excluded.size > registry_pin.size
                OR (excluded.size = registry_pin.size AND excluded.root = registry_pin.root)",
            params![
                sqlite_i64(checkpoint.size, "registry checkpoint size")?,
                checkpoint.root.as_slice(),
                witnessed_at
                    .map(|value| sqlite_i64(value, "registry witness timestamp"))
                    .transpose()?,
            ],
        )?;
        if changed != 1 {
            return Err(ClientError::Core(pigeonpost_core::Error::StaleSequence));
        }
        tx.commit()?;
        *self.compliance_cache.borrow_mut() = None;
        *self.handle_cache.borrow_mut() = None;
        Ok(())
    }

    /// Persist the sender's explicit agreement to one exact recipient custody scope.
    pub fn set_sender_attribution_requirement(
        &self,
        requirement: Option<AttributionRequirement>,
    ) -> Result<()> {
        let encoded = requirement
            .map(|value| value.encode().map(|bytes| hex_bytes(&bytes)))
            .transpose()?;
        let tx = Transaction::new_unchecked(&self.conn, TransactionBehavior::Immediate)?;
        match encoded {
            Some(encoded) => {
                tx.execute(
                    "INSERT INTO meta (key, value) VALUES (?1, ?2)
                     ON CONFLICT(key) DO UPDATE SET value = excluded.value",
                    params![SENDER_ATTRIBUTION_REQUIREMENT_META, encoded],
                )?;
            }
            None => {
                tx.execute(
                    "DELETE FROM meta WHERE key = ?1",
                    params![SENDER_ATTRIBUTION_REQUIREMENT_META],
                )?;
            }
        }
        // The deployed jurisdiction-only selector cannot express a custodian. Never leave it
        // available as an ambiguous fallback after an exact operator choice.
        tx.execute(
            "DELETE FROM meta WHERE key = ?1",
            params![ATTRIBUTION_JURISDICTION_META],
        )?;
        tx.commit()?;
        Ok(())
    }

    pub fn sender_attribution_requirement(&self) -> Result<Option<AttributionRequirement>> {
        let exact = parse_attribution_requirement_meta(
            SENDER_ATTRIBUTION_REQUIREMENT_META,
            self.get_meta(SENDER_ATTRIBUTION_REQUIREMENT_META)?
                .as_deref(),
        )?;
        if exact.is_some() {
            return Ok(exact);
        }
        if self.get_meta(ATTRIBUTION_JURISDICTION_META)?.is_some() {
            return Err(ClientError::Config(
                "legacy sender attribution jurisdiction lacks a custody authority; reconfigure the exact attribution scope"
                    .into(),
            ));
        }
        Ok(None)
    }

    /// Compatibility accessor. A jurisdiction without an authority can no longer be enabled.
    pub fn set_attribution_jurisdiction(&self, jurisdiction: Option<Jurisdiction>) -> Result<()> {
        match jurisdiction {
            Some(_) => Err(ClientError::Config(
                "an attribution jurisdiction must be paired with a stable custody authority".into(),
            )),
            None => self.set_sender_attribution_requirement(None),
        }
    }

    pub fn attribution_jurisdiction(&self) -> Result<Option<Jurisdiction>> {
        Ok(self
            .sender_attribution_requirement()?
            .map(|requirement| requirement.jurisdiction))
    }

    pub(crate) fn set_recipient_attribution_requirement(
        &self,
        requirement: Option<AttributionRequirement>,
    ) -> Result<()> {
        let encoded = requirement
            .map(|value| value.encode().map(|bytes| hex_bytes(&bytes)))
            .transpose()?;
        let tx = Transaction::new_unchecked(&self.conn, TransactionBehavior::Immediate)?;
        match encoded {
            Some(encoded) => {
                tx.execute(
                    "INSERT INTO meta (key, value) VALUES (?1, ?2)
                     ON CONFLICT(key) DO UPDATE SET value = excluded.value",
                    params![RECIPIENT_ATTRIBUTION_REQUIREMENT_META, encoded],
                )?;
            }
            None => {
                tx.execute(
                    "DELETE FROM meta WHERE key = ?1",
                    params![RECIPIENT_ATTRIBUTION_REQUIREMENT_META],
                )?;
            }
        }
        tx.execute(
            "INSERT INTO meta (key, value) VALUES (?1, ?2)
             ON CONFLICT(key) DO UPDATE SET value = excluded.value",
            params![
                ATTRIBUTION_REQUIRED_META,
                canonical_bool(requirement.is_some())
            ],
        )?;
        tx.commit()?;
        Ok(())
    }

    pub(crate) fn recipient_attribution_requirement(
        &self,
    ) -> Result<Option<AttributionRequirement>> {
        let exact = parse_attribution_requirement_meta(
            RECIPIENT_ATTRIBUTION_REQUIREMENT_META,
            self.get_meta(RECIPIENT_ATTRIBUTION_REQUIREMENT_META)?
                .as_deref(),
        )?;
        let legacy_required = parse_bool_meta(
            ATTRIBUTION_REQUIRED_META,
            self.get_meta(ATTRIBUTION_REQUIRED_META)?.as_deref(),
        )?
        .unwrap_or(false);
        match (exact, legacy_required) {
            (Some(requirement), true) => Ok(Some(requirement)),
            (Some(_), false) => Err(ClientError::Config(
                "recipient attribution requirement conflicts with its legacy marker".into(),
            )),
            (None, true) => Err(ClientError::Config(
                "legacy recipient attribution requirement lacks jurisdiction and custody authority; reconfigure the exact scope"
                    .into(),
            )),
            (None, false) => Ok(None),
        }
    }

    pub(crate) fn set_attribution_required(&self, required: bool) -> Result<()> {
        if required {
            return Err(ClientError::Config(
                "recipient attribution requires an exact jurisdiction and custody authority".into(),
            ));
        }
        self.set_recipient_attribution_requirement(None)
    }

    pub(crate) fn attribution_required(&self) -> Result<bool> {
        Ok(self.recipient_attribution_requirement()?.is_some())
    }

    /// Atomically replace the local key cache and its independently recomputed full-log audit.
    /// Cached rows are unusable without the exact audit state for the same witnessed checkpoint.
    pub fn save_compliance_keys(
        &self,
        verified: &VerifiedComplianceKeys,
        audit: &ComplianceAuditState,
        fetched_at_secs: u64,
    ) -> Result<()> {
        let configured = self
            .registry_configuration()?
            .ok_or_else(|| ClientError::Config("registry is not configured".into()))?;
        let witnessed_at = verified.witnessed_at().ok_or_else(|| {
            ClientError::Config("compliance keys require a witnessed checkpoint".into())
        })?;
        if configured.trust.witness_threshold() == 0
            || verified.checkpoint().origin != configured.trust.expected_origin()
            || verified.keys().len() > MAX_CACHED_COMPLIANCE_KEYS
        {
            return Err(ClientError::Config(
                "compliance-key projection does not satisfy registry trust".into(),
            ));
        }
        audit.validate(configured.trust.expected_origin())?;
        if audit.checkpoint() != *verified.checkpoint()
            || audit.witnessed_at() != witnessed_at
            || audit.keys().len() != verified.keys().len()
            || !audit
                .keys()
                .iter()
                .zip(verified.keys())
                .all(|(audited, key)| {
                    audited.publication() == key.publication()
                        && audited.public_key() == key.public_key()
                        && audited.log_index() == key.log_index()
                })
        {
            return Err(ClientError::Config(
                "compliance-key rows do not match the complete registry audit".into(),
            ));
        }
        if let Some(known) = configured.checkpoint.as_ref() {
            if verified.checkpoint().size < known.size
                || (verified.checkpoint().size == known.size
                    && verified.checkpoint().root != known.root)
            {
                return Err(ClientError::Core(pigeonpost_core::Error::StaleSequence));
            }
        }
        let audit_bytes = serde_json::to_vec(audit)?;
        if audit_bytes.len() > MAX_COMPLIANCE_AUDIT_BYTES {
            return Err(ClientError::Config(
                "compliance registry audit exceeds its durable size bound".into(),
            ));
        }

        let mut rows = Vec::with_capacity(verified.keys().len());
        let mut ids = HashSet::with_capacity(verified.keys().len());
        for key in verified.keys() {
            let publication = key.publication();
            let encoded = publication
                .key_id
                .encode()
                .map_err(|error| ClientError::Config(error.to_string()))?;
            if !ids.insert(encoded)
                || validate_compliance_epoch(
                    &publication.key_id,
                    publication.not_before_ms,
                    publication.not_after_ms,
                )
                .is_err()
                || publication.public_key != hex_bytes(key.public_key())
                || *key.public_key() == [0u8; 32]
                || key.log_index() >= verified.checkpoint().size
            {
                return Err(ClientError::Config(
                    "compliance-key projection contains a malformed row".into(),
                ));
            }
            rows.push((
                encoded,
                serde_json::to_vec(publication)?,
                *key.public_key(),
                key.log_index(),
            ));
        }

        // All definitive pin checks and derived-cache replacement happen under one reserved writer.
        // Validation above deliberately stays outside the lock because it is CPU work over bounded
        // input; the trust and pin are re-read after BEGIN IMMEDIATE before any row changes.
        let tx = Transaction::new_unchecked(&self.conn, TransactionBehavior::Immediate)?;
        let locked = self
            .registry_configuration()?
            .ok_or_else(|| ClientError::Config("registry is not configured".into()))?;
        if locked.trust.witness_threshold() == 0
            || verified.checkpoint().origin != locked.trust.expected_origin()
        {
            return Err(ClientError::Config(
                "compliance-key projection no longer matches registry trust".into(),
            ));
        }
        audit.validate(locked.trust.expected_origin())?;
        if let Some(known) = locked.checkpoint.as_ref() {
            if verified.checkpoint().size < known.size
                || (verified.checkpoint().size == known.size
                    && verified.checkpoint().root != known.root)
            {
                return Err(ClientError::Core(pigeonpost_core::Error::StaleSequence));
            }
        }
        let changed = tx.execute(
            "INSERT INTO registry_pin (id, size, root, witnessed_at)
             VALUES (1, ?1, ?2, ?3)
             ON CONFLICT(id) DO UPDATE SET
                 size = excluded.size,
                 root = excluded.root,
                 witnessed_at = CASE
                     WHEN excluded.size > registry_pin.size THEN excluded.witnessed_at
                     WHEN registry_pin.witnessed_at IS NULL THEN excluded.witnessed_at
                     WHEN excluded.witnessed_at IS NULL THEN registry_pin.witnessed_at
                     ELSE MAX(registry_pin.witnessed_at, excluded.witnessed_at)
                 END
             WHERE excluded.size > registry_pin.size
                OR (excluded.size = registry_pin.size AND excluded.root = registry_pin.root)",
            params![
                sqlite_i64(verified.checkpoint().size, "registry checkpoint size")?,
                verified.checkpoint().root.as_slice(),
                sqlite_i64(witnessed_at, "registry witness timestamp")?,
            ],
        )?;
        if changed != 1 {
            return Err(ClientError::Core(pigeonpost_core::Error::StaleSequence));
        }
        tx.execute("DELETE FROM compliance_keys", [])?;
        for (key_id, publication, public_key, log_index) in rows {
            tx.execute(
                "INSERT INTO compliance_keys
                    (key_id, publication, public_key, log_index, checkpoint_size,
                     witnessed_at, fetched_at)
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)",
                params![
                    key_id.as_slice(),
                    publication,
                    public_key.as_slice(),
                    sqlite_i64(log_index, "compliance-key log index")?,
                    sqlite_i64(verified.checkpoint().size, "compliance checkpoint size")?,
                    sqlite_i64(witnessed_at, "compliance witness timestamp")?,
                    sqlite_i64(fetched_at_secs, "compliance fetch timestamp")?,
                ],
            )?;
        }
        tx.execute(
            "INSERT INTO registry_audit (id, state) VALUES (1, ?1)
             ON CONFLICT(id) DO UPDATE SET state = excluded.state",
            params![audit_bytes],
        )?;
        tx.commit()?;
        *self.compliance_cache.borrow_mut() = None;
        *self.handle_cache.borrow_mut() = None;
        Ok(())
    }

    /// Load and validate the compact audit that authorizes offline compliance-key use.
    pub fn compliance_audit(&self) -> Result<Option<ComplianceAuditState>> {
        let bytes: Option<Vec<u8>> = self
            .conn
            .query_row("SELECT state FROM registry_audit WHERE id = 1", [], |row| {
                row.get(0)
            })
            .optional()?;
        let Some(bytes) = bytes else {
            return Ok(None);
        };
        if bytes.len() > MAX_COMPLIANCE_AUDIT_BYTES {
            return Err(ClientError::Config(
                "stored compliance registry audit exceeds its size bound".into(),
            ));
        }
        let audit: ComplianceAuditState = serde_json::from_slice(&bytes)?;
        let configured = self
            .registry_configuration()?
            .ok_or_else(|| ClientError::Config("registry is not configured".into()))?;
        audit.validate(configured.trust.expected_origin())?;
        let audit_checkpoint = audit.checkpoint();
        if configured.checkpoint.as_ref() != Some(&audit_checkpoint) {
            return Err(ClientError::Config(
                "stored compliance audit does not match the pinned checkpoint".into(),
            ));
        }
        Ok(Some(audit))
    }

    /// Resolve the currently active attribution key for a sender. Cache freshness is measured
    /// against the configured witness policy, never against the network response's own clock.
    pub fn current_attribution_key(
        &self,
        requirement: &AttributionRequirement,
        at_ms: u64,
        now_secs: u64,
    ) -> Result<Option<CachedComplianceKey>> {
        self.ensure_fresh_compliance_cache(now_secs)?;
        let cache = self.compliance_cache.borrow();
        Ok(cache
            .as_ref()
            .ok_or_else(|| ClientError::Config("compliance cache was not initialized".into()))?
            .keys
            .iter()
            .filter(|key| {
                requirement.matches_key_id(&key.publication.key_id)
                    && key.publication.status == ComplianceKeyStatus::Active
                    && attribution_epoch_contains(&key.publication.key_id, at_ms) == Ok(true)
            })
            .max_by_key(|key| {
                (
                    key.publication.key_id.epoch_start_ms,
                    key.publication.key_id.generation,
                    key.log_index,
                )
            })
            .cloned())
    }

    /// Resolve an exact key id while enforcing a required recipient scope.
    ///
    /// Retired keys remain usable for delayed messages whose signed send time falls inside their
    /// published interval; revoked keys never do. Absence from a still-fresh audited prefix is not
    /// proof that a later witnessed prefix lacks the key, so it is a retryable local trust failure
    /// rather than an `Invalid` attribution verdict.
    pub fn attribution_key(
        &self,
        key_id: &ComplianceKeyId,
        now_secs: u64,
    ) -> Result<Option<CachedComplianceKey>> {
        self.ensure_fresh_compliance_cache(now_secs)?;
        let cache = self.compliance_cache.borrow();
        let cache = cache
            .as_ref()
            .ok_or_else(|| ClientError::Config("compliance cache was not initialized".into()))?;
        let key = cache
            .by_id
            .get(key_id)
            .and_then(|index| cache.keys.get(*index))
            .ok_or(ClientError::AttributionTrustUnavailable)?;
        Ok(
            (key.publication.key_id.purpose == CompliancePurpose::Attribution
                && key.publication.status != ComplianceKeyStatus::Revoked)
                .then(|| key.clone()),
        )
    }

    /// Resolve an attribution key for a recipient whose policy does not require attribution.
    ///
    /// Missing or expired witnessed evidence means the block is unverifiable, not that the whole
    /// mailbox must stop. Static configuration, signatures, pinned checkpoints, and stored rows
    /// are still validated and still fail closed: only the freshness window is allowed to degrade
    /// to `None` for this privacy-first receive path.
    pub(crate) fn optional_attribution_key(
        &self,
        key_id: &ComplianceKeyId,
        now_secs: u64,
    ) -> Result<Option<CachedComplianceKey>> {
        let audit_present = self
            .conn
            .query_row("SELECT 1 FROM registry_audit WHERE id = 1", [], |_| Ok(()))
            .optional()?
            .is_some();
        let configured = self.registry_configuration()?;
        if configured.is_none() && !audit_present {
            return Ok(None);
        }
        let configured =
            configured.ok_or_else(|| ClientError::Config("registry is not configured".into()))?;
        if configured.trust.witness_threshold() == 0 {
            return Err(ClientError::Config(
                "compliance keys require a nonzero witness quorum".into(),
            ));
        }
        if !audit_present {
            return Ok(None);
        }
        // Parse and authenticate the complete durable projection before deciding that its witness
        // time is too old to authorize this optional block. Otherwise a stale timestamp could
        // hide a corrupt row indefinitely and turn malformed local state into a silent downgrade.
        self.ensure_validated_compliance_cache()?;
        let cache = self.compliance_cache.borrow();
        let cache = cache
            .as_ref()
            .ok_or_else(|| ClientError::Config("compliance cache was not initialized".into()))?;
        if cache.witnessed_at > now_secs.saturating_add(cache.future_skew_secs)
            || now_secs > cache.witnessed_at.saturating_add(cache.max_age_secs)
        {
            return Ok(None);
        }
        Ok(cache
            .by_id
            .get(key_id)
            .and_then(|index| cache.keys.get(*index))
            .filter(|key| {
                key.publication.key_id.purpose == CompliancePurpose::Attribution
                    && key.publication.status != ComplianceKeyStatus::Revoked
            })
            .cloned())
    }

    fn ensure_fresh_compliance_cache(&self, now_secs: u64) -> Result<()> {
        self.ensure_validated_compliance_cache()?;
        let cache = self.compliance_cache.borrow();
        let cache = cache
            .as_ref()
            .ok_or_else(|| ClientError::Config("compliance cache was not initialized".into()))?;
        if cache.witnessed_at > now_secs.saturating_add(cache.future_skew_secs)
            || now_secs > cache.witnessed_at.saturating_add(cache.max_age_secs)
        {
            return Err(ClientError::Config(
                "cached compliance checkpoint is stale".into(),
            ));
        }
        Ok(())
    }

    fn ensure_validated_compliance_cache(&self) -> Result<()> {
        let data_version: i64 = self
            .conn
            .pragma_query_value(None, "data_version", |row| row.get(0))?;
        let pin: Option<(i64, Vec<u8>, Option<i64>)> = self
            .conn
            .query_row(
                "SELECT size, root, witnessed_at FROM registry_pin WHERE id = 1",
                [],
                |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
            )
            .optional()?;
        if let (Some(cache), Some((size, root, witnessed_at))) =
            (self.compliance_cache.borrow().as_ref(), pin.as_ref())
        {
            let size = sqlite_u64(*size, "stored registry checkpoint size")?;
            let witnessed_at = witnessed_at
                .map(|value| sqlite_u64(value, "stored witness timestamp"))
                .transpose()?;
            if data_version == cache.data_version
                && size == cache.checkpoint.size
                && root.as_slice() == cache.checkpoint.root
                && witnessed_at == Some(cache.witnessed_at)
            {
                return Ok(());
            }
        }
        let loaded = self.load_compliance_cache()?;
        *self.compliance_cache.borrow_mut() = Some(loaded);
        Ok(())
    }

    fn load_compliance_cache(&self) -> Result<ValidatedComplianceCache> {
        let data_version_before: i64 =
            self.conn
                .pragma_query_value(None, "data_version", |row| row.get(0))?;
        let configured = self
            .registry_configuration()?
            .ok_or_else(|| ClientError::Config("registry is not configured".into()))?;
        if configured.trust.witness_threshold() == 0 {
            return Err(ClientError::Config(
                "compliance keys require a nonzero witness quorum".into(),
            ));
        }
        let checkpoint = configured.checkpoint.ok_or_else(|| {
            ClientError::Config("no witnessed compliance checkpoint is cached".into())
        })?;
        let audit = self.compliance_audit()?.ok_or_else(|| {
            ClientError::Config("no complete compliance registry audit is cached".into())
        })?;
        // Re-authenticate the persisted checkpoint at its recorded witness time first. This
        // validates operator/witness signatures and timestamp coherence without letting current
        // freshness short-circuit validation of the durable key rows below.
        let audit_checkpoint = audit.verify_witnesses(&configured.trust, audit.witnessed_at())?;
        if audit_checkpoint.checkpoint != checkpoint
            || audit_checkpoint.witnessed_at != Some(audit.witnessed_at())
        {
            return Err(ClientError::Config(
                "cached compliance audit does not match its witnessed checkpoint".into(),
            ));
        }

        let mut statement = self.conn.prepare(
            "SELECT key_id, publication, public_key, log_index, witnessed_at
             FROM compliance_keys WHERE checkpoint_size = ?1 ORDER BY log_index",
        )?;
        let rows = statement.query_map(
            params![sqlite_i64(checkpoint.size, "compliance checkpoint size")?],
            |row| {
                Ok((
                    row.get::<_, Vec<u8>>(0)?,
                    row.get::<_, Vec<u8>>(1)?,
                    row.get::<_, Vec<u8>>(2)?,
                    row.get::<_, i64>(3)?,
                    row.get::<_, i64>(4)?,
                ))
            },
        )?;

        let mut keys = Vec::new();
        let mut by_id = HashMap::new();
        for row in rows {
            if keys.len() >= MAX_CACHED_COMPLIANCE_KEYS {
                return Err(ClientError::Config(
                    "cached compliance-key projection exceeds its bound".into(),
                ));
            }
            let (encoded_id, publication, public_key, log_index, witnessed_at) = row?;
            let witnessed_at = sqlite_u64(witnessed_at, "cached witness timestamp")?;
            if witnessed_at != audit.witnessed_at() {
                return Err(ClientError::Config(
                    "cached compliance-key row has a mismatched witness timestamp".into(),
                ));
            }
            let publication: ComplianceKeyPublish = serde_json::from_slice(&publication)?;
            let decoded = ComplianceKeyId::decode(&encoded_id)
                .map_err(|error| ClientError::Config(error.to_string()))?;
            let public_key = fixed32(public_key, "cached compliance public key")?;
            if publication.key_id != decoded
                || publication.public_key != hex_bytes(&public_key)
                || validate_compliance_epoch(
                    &publication.key_id,
                    publication.not_before_ms,
                    publication.not_after_ms,
                )
                .is_err()
                || public_key == [0u8; 32]
            {
                return Err(ClientError::Config(
                    "cached compliance-key row is malformed".into(),
                ));
            }
            let index = keys.len();
            if by_id.insert(publication.key_id, index).is_some() {
                return Err(ClientError::Config(
                    "cached compliance keys contain a duplicate id".into(),
                ));
            }
            keys.push(CachedComplianceKey {
                publication,
                public_key,
                log_index: sqlite_u64(log_index, "cached compliance-key log index")?,
            });
        }
        if keys.len() != audit.keys().len()
            || !keys.iter().zip(audit.keys()).all(|(cached, audited)| {
                cached.publication == *audited.publication()
                    && cached.public_key == *audited.public_key()
                    && cached.log_index == audited.log_index()
            })
        {
            return Err(ClientError::Config(
                "cached compliance keys do not match the complete registry audit".into(),
            ));
        }
        let data_version_after: i64 =
            self.conn
                .pragma_query_value(None, "data_version", |row| row.get(0))?;
        if data_version_before != data_version_after {
            return Err(ClientError::Config(
                "cached compliance projection changed during validation".into(),
            ));
        }
        Ok(ValidatedComplianceCache {
            data_version: data_version_after,
            checkpoint,
            witnessed_at: audit.witnessed_at(),
            max_age_secs: configured.trust.max_cosignature_age_secs(),
            future_skew_secs: configured.trust.future_clock_skew_secs(),
            keys,
            by_id,
        })
    }

    /// Load the compact global handle-audit frontier. It may lag the sole durable registry pin
    /// (for example after a compliance refresh), but it may never be ahead or conflict at the same
    /// size.
    pub fn handle_audit(&self) -> Result<Option<HandleAuditState>> {
        let configured = self
            .registry_configuration()?
            .ok_or_else(|| ClientError::Config("registry is not configured".into()))?;
        load_handle_audit(&self.conn, &configured)
    }

    /// Whether `pubkey` is the current key of at least one OIDC-backed handle in the complete,
    /// locally cached registry projection.
    ///
    /// This is deliberately an offline lookup: receiving a message must not disclose its sender
    /// to the registry. Credit is withheld unless the handle audit is exactly at the sole durable
    /// pin and its signed checkpoint still carries a fresh quorum under the configured trust
    /// policy. Missing, lagged, or expired evidence is therefore `false`; malformed durable state
    /// and invalid signatures remain errors instead of being silently downgraded.
    pub fn has_current_verified_handle(&self, pubkey: &[u8; 32], now_secs: u64) -> Result<bool> {
        let data_version: i64 = self
            .conn
            .pragma_query_value(None, "data_version", |row| row.get(0))?;
        let reload = self.handle_cache.borrow().as_ref().is_none_or(|cache| {
            cache.data_version != data_version || now_secs < cache.validated_at
        });
        if reload {
            let loaded = self.load_handle_cache(now_secs)?;
            *self.handle_cache.borrow_mut() = Some(loaded);
        }

        let (cache_data_version, audit_size) = {
            let cache = self.handle_cache.borrow();
            let cache = cache
                .as_ref()
                .ok_or_else(|| ClientError::Config("handle cache initialization failed".into()))?;
            let Some(evidence) = cache.evidence.as_ref() else {
                return Ok(false);
            };
            if evidence.witnessed_at > now_secs.saturating_add(evidence.future_skew_secs)
                || evidence.witnessed_at < now_secs.saturating_sub(evidence.max_age_secs)
            {
                return Ok(false);
            }
            (cache.data_version, evidence.audit_size)
        };

        let row = self
            .conn
            .query_row(
                "SELECT handle, pubkey, subject, log_index
                 FROM registry_handle_projection WHERE pubkey = ?1 LIMIT 1",
                params![pubkey.as_slice()],
                |row| {
                    Ok((
                        row.get::<_, String>(0)?,
                        row.get::<_, Vec<u8>>(1)?,
                        row.get::<_, String>(2)?,
                        row.get::<_, i64>(3)?,
                    ))
                },
            )
            .optional()?;
        if let Some((handle, stored_pubkey, subject, log_index)) = row.as_ref() {
            validate_handle_projection_binding(
                handle,
                stored_pubkey,
                subject,
                *log_index,
                audit_size,
            )?;
        }
        let data_version_after: i64 =
            self.conn
                .pragma_query_value(None, "data_version", |row| row.get(0))?;
        if data_version_after != cache_data_version {
            return Err(ClientError::Config(
                "cached handle projection changed during lookup".into(),
            ));
        }
        Ok(row.is_some())
    }

    fn load_handle_cache(&self, now_secs: u64) -> Result<ValidatedHandleCache> {
        let data_version_before: i64 =
            self.conn
                .pragma_query_value(None, "data_version", |row| row.get(0))?;
        let Some(configured) = self.registry_configuration()? else {
            return self.finish_handle_cache(data_version_before, now_secs, None);
        };
        // A handle tier is meaningful only when a configured strict-majority witness quorum exists.
        // Constructors reject zero, but keep this persistence-boundary guard so legacy or corrupt
        // state can never turn an operator-only claim into reputation.
        if configured.trust.witness_threshold() == 0 {
            return self.finish_handle_cache(data_version_before, now_secs, None);
        }
        let Some(pin) = configured.checkpoint.as_ref() else {
            return self.finish_handle_cache(data_version_before, now_secs, None);
        };
        let Some(audit) = load_handle_audit(&self.conn, &configured)? else {
            return self.finish_handle_cache(data_version_before, now_secs, None);
        };
        if audit.size() != pin.size || audit.root() != &pin.root {
            return self.finish_handle_cache(data_version_before, now_secs, None);
        }

        // First authenticate the note without a clock policy. If this fails, the durable note was
        // malformed or tampered with. Only after that distinction is established may an expired
        // (but otherwise valid) quorum be treated as unavailable evidence.
        let authenticated = Checkpoint::verify_with_witnesses(
            audit.checkpoint_note(),
            configured.trust.checkpoint_key(),
            configured.trust.witnesses(),
            configured.trust.witness_threshold(),
        )?;
        if authenticated != *pin {
            return Err(ClientError::Config(
                "stored handle checkpoint note does not match the registry pin".into(),
            ));
        }
        let fresh = match Checkpoint::verify_with_fresh_witnesses(
            audit.checkpoint_note(),
            configured.trust.checkpoint_key(),
            configured.trust.witnesses(),
            configured.trust.witness_threshold(),
            now_secs,
            configured.trust.max_cosignature_age_secs(),
            configured.trust.future_clock_skew_secs(),
        ) {
            Ok(fresh) => fresh,
            Err(_) => return self.finish_handle_cache(data_version_before, now_secs, None),
        };
        if fresh.checkpoint != *pin
            || fresh.witnessed_at != Some(audit.witnessed_at())
            || configured.witnessed_at != Some(audit.witnessed_at())
        {
            return Err(ClientError::Config(
                "stored handle witness evidence does not match the registry pin".into(),
            ));
        }

        self.finish_handle_cache(
            data_version_before,
            now_secs,
            Some(ValidatedHandleEvidence {
                audit_size: audit.size(),
                witnessed_at: audit.witnessed_at(),
                max_age_secs: configured.trust.max_cosignature_age_secs(),
                future_skew_secs: configured.trust.future_clock_skew_secs(),
            }),
        )
    }

    fn finish_handle_cache(
        &self,
        data_version_before: i64,
        validated_at: u64,
        evidence: Option<ValidatedHandleEvidence>,
    ) -> Result<ValidatedHandleCache> {
        let data_version_after: i64 =
            self.conn
                .pragma_query_value(None, "data_version", |row| row.get(0))?;
        if data_version_before != data_version_after {
            return Err(ClientError::Config(
                "cached handle projection changed during validation".into(),
            ));
        }
        Ok(ValidatedHandleCache {
            data_version: data_version_after,
            validated_at,
            evidence,
        })
    }

    /// Audit the registry's handle history without holding the agent database write lock, then
    /// atomically publish the verified local delta, audit frontier, requested binding, and sole
    /// registry checkpoint pin.
    pub async fn resolve_handle_audited(
        &self,
        client: &RegistryClient,
        handle: &Handle,
        now: u64,
    ) -> Result<(VerifiedHandle, Address)> {
        let before = self
            .registry_configuration()?
            .ok_or_else(|| ClientError::Config("registry is not configured".into()))?;
        let previous = load_handle_audit(&self.conn, &before)?;
        let accepted = before.checkpoint.clone();
        let staged = StagedHandleProjection::new(&self.conn)?;
        let (verified, audit, staged) = client
            .resolve_audited(handle, previous.as_ref(), accepted.as_ref(), now, staged)
            .await?;
        audit.validate(before.trust.expected_origin())?;
        if audit.checkpoint() != *verified.checkpoint()
            || audit.witnessed_at() != verified.witnessed_at().unwrap_or_default()
            || verified.log_index() >= verified.checkpoint().size
        {
            return Err(ClientError::Config(
                "verified handle does not match its complete registry audit".into(),
            ));
        }
        let audit_bytes = serde_json::to_vec(&audit)?;
        if audit_bytes.len() > MAX_HANDLE_AUDIT_BYTES {
            return Err(ClientError::Config(
                "handle registry audit exceeds its persistence bound".into(),
            ));
        }

        // Acquire the writer only after all network I/O and cryptographic work has completed.
        // Re-reading both snapshots under the lock turns concurrent progress into a clean retry
        // instead of merging a delta against a different projection base.
        let tx = Transaction::new_unchecked(&self.conn, TransactionBehavior::Immediate)?;
        let current = self
            .registry_configuration()?
            .ok_or_else(|| ClientError::Config("registry is not configured".into()))?;
        if current.url != before.url
            || !same_registry_trust(&current.trust, &before.trust)
            || current.checkpoint != accepted
            || load_handle_audit(&tx, &current)? != previous
        {
            return Err(ClientError::Core(pigeonpost_core::Error::StaleSequence));
        }

        staged.apply_to(&tx)?;
        let address = save_handle_resolution_tx(&tx, &current, &verified, now)?;
        tx.execute(
            "INSERT INTO registry_handle_audit (id, state) VALUES (1, ?1)
             ON CONFLICT(id) DO UPDATE SET state = excluded.state",
            params![audit_bytes],
        )?;
        tx.commit()?;
        *self.compliance_cache.borrow_mut() = None;
        *self.handle_cache.borrow_mut() = None;
        Ok((verified, address))
    }

    pub fn handle_resolution(&self, handle: &str) -> Result<Option<Address>> {
        let stored: Option<(String, Vec<u8>)> = self
            .conn
            .query_row(
                "SELECT address, pubkey FROM handle_resolutions WHERE handle = ?1",
                params![handle],
                |row| Ok((row.get(0)?, row.get(1)?)),
            )
            .optional()?;
        stored
            .map(|(address, pubkey)| {
                let address = Address::parse(&address)?;
                let key = keys::verifying_key_from_bytes(&fixed32(pubkey, "stored handle key")?)
                    .map_err(|_| ClientError::Config("stored handle key is invalid".into()))?;
                if !address.matches(&key) {
                    return Err(ClientError::Config(
                        "stored handle address does not match its key".into(),
                    ));
                }
                Ok(address)
            })
            .transpose()
    }

    /// Explicitly remove the registry trust root and every handle learned through it.
    pub(crate) fn reset_registry(&self) -> Result<()> {
        let tx = Transaction::new_unchecked(&self.conn, TransactionBehavior::Immediate)?;
        tx.execute("DELETE FROM registry_handle_projection", [])?;
        tx.execute("DELETE FROM registry_handle_audit", [])?;
        tx.execute("DELETE FROM handle_resolutions", [])?;
        tx.execute("DELETE FROM compliance_keys", [])?;
        tx.execute("DELETE FROM registry_audit", [])?;
        tx.execute("DELETE FROM registry_pin", [])?;
        tx.execute("DELETE FROM registry_witnesses", [])?;
        tx.execute("DELETE FROM registry_config", [])?;
        tx.commit()?;
        *self.compliance_cache.borrow_mut() = None;
        *self.handle_cache.borrow_mut() = None;
        Ok(())
    }

    // ---- outbox ---------------------------------------------------------------------------

    pub fn queue(
        &self,
        message_id: &str,
        to_addr: &str,
        route: OutboxRoute<'_>,
        wrap: &Wrap,
        token: Option<&Token>,
        now: u64,
    ) -> Result<()> {
        let encoded_wrap = encode_outbox_payload(message_id, to_addr, route.loft_url, wrap)?;
        let token = token.map(Token::to_hex);
        let timestamp = sqlite_i64(now, "outbox timestamp")?;
        let tx = Transaction::new_unchecked(&self.conn, TransactionBehavior::Immediate)?;
        Self::queue_outbox_tx(
            &tx,
            message_id,
            to_addr,
            route,
            &encoded_wrap,
            token.as_deref(),
            timestamp,
        )?;
        tx.commit()?;
        Ok(())
    }

    /// Durably queue every copy of one outbound message and only then grant its recipient reply
    /// trust, all under the same writer transaction.
    ///
    /// This is the send-path commit point. Failures before it leave neither an outbox debt nor a
    /// false allowlist/score signal; a crash cannot persist just one side of the relationship.
    #[allow(clippy::too_many_arguments)]
    pub fn queue_correspondence(
        &self,
        message_id: &str,
        to_addr: &str,
        routes: &[OutboxRoute<'_>],
        wrap: &Wrap,
        token: Option<&Token>,
        correspondent: &[u8; 32],
        now: u64,
    ) -> Result<()> {
        if routes.is_empty() {
            return Err(ClientError::Undeliverable);
        }
        if routes.len() > pigeonpost_core::record::MAX_AGENT_RECORD_LOFTS {
            return Err(ClientError::Core(pigeonpost_core::Error::TooLarge));
        }
        let first_route = routes
            .first()
            .expect("nonempty routes were checked immediately above");
        let encoded_wrap = encode_outbox_payload(message_id, to_addr, first_route.loft_url, wrap)?;
        for route in &routes[1..] {
            validate_bounded_text(route.loft_url, MAX_STORED_LOFT_URL_BYTES, "outbox loft URL")?;
        }
        let token = token.map(Token::to_hex);
        let timestamp = sqlite_i64(now, "outbox/correspondence timestamp")?;
        let tx = Transaction::new_unchecked(&self.conn, TransactionBehavior::Immediate)?;
        for route in routes {
            Self::queue_outbox_tx(
                &tx,
                message_id,
                to_addr,
                *route,
                &encoded_wrap,
                token.as_deref(),
                timestamp,
            )?;
        }
        let durable_copies: i64 = tx.query_row(
            "SELECT COUNT(*) FROM outbox WHERE message_id = ?1 AND sent_at IS NULL",
            params![message_id],
            |row| row.get(0),
        )?;
        if durable_copies == 0 {
            return Err(ClientError::Config(
                "outbound message did not create a durable queue entry".into(),
            ));
        }

        let newly_allowed = tx.execute(
            "INSERT OR IGNORE INTO allowlist (pubkey, added_at, reason)
             VALUES (?1, ?2, 'corresponded')",
            params![correspondent.as_slice(), timestamp],
        )?;
        if newly_allowed == 1 {
            Self::set_score_floor_tx(&tx, correspondent, crate::spam::SCORE_CORRESPONDED, now)?;
        }
        tx.commit()?;
        Ok(())
    }

    #[allow(clippy::too_many_arguments)]
    fn queue_outbox_tx(
        tx: &Transaction<'_>,
        message_id: &str,
        to_addr: &str,
        route: OutboxRoute<'_>,
        encoded_wrap: &[u8],
        token: Option<&str>,
        timestamp: i64,
    ) -> Result<bool> {
        let existing = tx
            .query_row(
                "SELECT to_addr, wrap, token, allow_local, sent_at, terminal_at
                 FROM outbox WHERE message_id = ?1 AND loft_url = ?2",
                params![message_id, route.loft_url],
                |row| {
                    Ok((
                        row.get::<_, String>(0)?,
                        row.get::<_, Vec<u8>>(1)?,
                        row.get::<_, Option<String>>(2)?,
                        row.get::<_, bool>(3)?,
                        row.get::<_, Option<i64>>(4)?,
                        row.get::<_, Option<i64>>(5)?,
                    ))
                },
            )
            .optional()?;
        if let Some((stored_to, stored_wrap, stored_token, allow_local, sent_at, terminal_at)) =
            existing
        {
            let active = sent_at.is_none() && terminal_at.is_none();
            if stored_to != to_addr
                || (active
                    && (stored_wrap.as_slice() != encoded_wrap || stored_token.as_deref() != token))
            {
                return Err(ClientError::Config(
                    "outbox idempotency key conflicts with different payload metadata".into(),
                ));
            }
            if active && route.allow_local && !allow_local {
                tx.execute(
                    "UPDATE outbox SET allow_local = 1 WHERE message_id = ?1 AND loft_url = ?2",
                    params![message_id, route.loft_url],
                )?;
            }
            return Ok(false);
        }

        let payload_bytes = u64::try_from(encoded_wrap.len() + token.map_or(0, str::len))
            .map_err(|_| ClientError::Config("outbox payload size overflowed".into()))?;
        let status = load_storage_status(tx)?;
        let next_rows = checked_quota_add(
            status.usage.outbox_rows,
            1,
            status.limits.outbox_rows,
            StorageResource::OutboxRows,
        )?;
        let next_payload = checked_quota_add(
            status.usage.outbox_payload_bytes,
            payload_bytes,
            status.limits.outbox_payload_bytes,
            StorageResource::OutboxPayloadBytes,
        )?;
        tx.execute(
            "INSERT INTO outbox
                 (message_id, to_addr, loft_url, wrap, token, allow_local, created_at,
                  next_attempt_at)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?7)",
            params![
                message_id,
                to_addr,
                route.loft_url,
                encoded_wrap,
                token,
                i64::from(route.allow_local),
                timestamp,
            ],
        )?;
        tx.execute(
            "UPDATE storage_accounting
             SET outbox_rows = ?1, outbox_payload_bytes = ?2 WHERE id = 1",
            params![
                sqlite_i64(next_rows, "outbox row usage")?,
                sqlite_i64(next_payload, "outbox payload usage")?
            ],
        )?;
        Ok(true)
    }

    pub fn pending(&self, limit: usize, now: u64) -> Result<Vec<OutboxEntry>> {
        let now = sqlite_i64(now, "outbox retry timestamp")?;
        let limit = sqlite_i64(
            u64::try_from(limit.min(MAX_PUBLIC_LIST_RESULTS))
                .map_err(|_| ClientError::Config("outbox limit exceeds this platform".into()))?,
            "outbox limit",
        )?;
        let mut stmt = self.conn.prepare(
            "SELECT row, message_id, loft_url, wrap, token, allow_local, attempts FROM outbox
             WHERE sent_at IS NULL AND terminal_at IS NULL AND next_attempt_at <= ?1
             ORDER BY next_attempt_at, row LIMIT ?2",
        )?;
        let rows = stmt.query_map(params![now, limit], |row| {
            Ok((
                row.get::<_, i64>(0)?,
                row.get::<_, String>(1)?,
                row.get::<_, String>(2)?,
                row.get::<_, Vec<u8>>(3)?,
                row.get::<_, Option<String>>(4)?,
                row.get::<_, bool>(5)?,
                row.get::<_, i64>(6)?,
            ))
        })?;

        let mut out = Vec::new();
        for row in rows {
            let (row_id, message_id, loft_url, blob, token, allow_local, attempts) = row?;
            let token = token
                .map(|value| {
                    Token::from_hex(&value).ok_or_else(|| {
                        ClientError::Config("outbox contains a malformed capability token".into())
                    })
                })
                .transpose()?;
            out.push(OutboxEntry {
                row: row_id,
                message_id,
                loft_url,
                wrap: serde_json::from_slice(&blob)?,
                token,
                allow_local,
                attempts: sqlite_u32(attempts, "stored outbox attempt count")?,
            });
        }
        Ok(out)
    }

    pub fn mark_sent(&self, row: i64, now: u64) -> Result<()> {
        let sent_at = sqlite_i64(now, "outbox sent timestamp")?;
        let tx = Transaction::new_unchecked(&self.conn, TransactionBehavior::Immediate)?;
        let payload_bytes = unsent_outbox_payload_bytes(&tx, row)?;
        let Some(payload_bytes) = payload_bytes else {
            tx.commit()?;
            return Ok(());
        };
        tx.execute(
            "UPDATE outbox
             SET sent_at = ?2, last_error = NULL, terminal_at = NULL, terminal_reason = NULL,
                 wrap = X'', token = NULL
             WHERE row = ?1 AND sent_at IS NULL",
            params![row, sent_at],
        )?;
        if payload_bytes > 0 {
            uncharge_outbox_payload(&tx, payload_bytes)?;
        }
        tx.commit()?;
        Ok(())
    }

    /// Record a bounded error class and schedule exponential retry with deterministic jitter.
    /// Delayed failures no longer sit at the head of every bounded flush and starve new messages.
    pub fn mark_failed(&self, row: i64, error_class: &str, now: u64) -> Result<()> {
        validate_outbox_reason(error_class)?;
        let attempts: u32 = self.conn.query_row(
            "SELECT attempts FROM outbox WHERE row = ?1",
            params![row],
            |r| r.get(0),
        )?;
        let exponent = attempts.min(10);
        let base = 5u64.saturating_mul(1u64 << exponent).min(3_600);
        let jitter = (row.unsigned_abs() % 17).min(base / 4 + 1);
        let next = now.saturating_add(base).saturating_add(jitter);
        let next = sqlite_i64(next, "outbox retry timestamp")?;
        self.conn.execute(
            "UPDATE outbox
             SET attempts = MIN(attempts + 1, 4294967295),
                 last_error = ?2, next_attempt_at = ?3
             WHERE row = ?1 AND sent_at IS NULL AND terminal_at IS NULL",
            params![row, error_class, next],
        )?;
        Ok(())
    }

    /// Stop automatic retries for a deterministic failure and make the copy operator-visible.
    pub fn mark_terminal(&self, row: i64, reason: &str, now: u64) -> Result<()> {
        validate_outbox_reason(reason)?;
        let terminal_at = sqlite_i64(now, "outbox terminal timestamp")?;
        let tx = Transaction::new_unchecked(&self.conn, TransactionBehavior::Immediate)?;
        let payload_bytes = active_outbox_payload_bytes(&tx, row)?;
        let Some(payload_bytes) = payload_bytes else {
            tx.commit()?;
            return Ok(());
        };
        tx.execute(
            "UPDATE outbox
             SET attempts = MIN(attempts + 1, 4294967295),
                 last_error = ?2,
                 terminal_at = ?3,
                 terminal_reason = ?2,
                 wrap = X'',
                 token = NULL
             WHERE row = ?1 AND sent_at IS NULL AND terminal_at IS NULL",
            params![row, reason, terminal_at],
        )?;
        uncharge_outbox_payload(&tx, payload_bytes)?;
        tx.commit()?;
        Ok(())
    }

    pub fn pending_count(&self) -> Result<u64> {
        let count = self.conn.query_row(
            "SELECT COUNT(*) FROM outbox
             WHERE sent_at IS NULL AND terminal_at IS NULL",
            [],
            |r| r.get::<_, i64>(0),
        )?;
        sqlite_u64(count, "pending outbox count")
    }

    pub fn terminal_count(&self) -> Result<u64> {
        let count = self.conn.query_row(
            "SELECT COUNT(*) FROM outbox
             WHERE sent_at IS NULL AND terminal_at IS NOT NULL",
            [],
            |row| row.get::<_, i64>(0),
        )?;
        sqlite_u64(count, "terminal outbox count")
    }

    /// Return bounded metadata for every retryable copy, including delayed rows. The query never
    /// selects the encrypted wrap or bearer token, so operator surfaces cannot leak either while
    /// presenting the opaque row id needed for an explicit abandonment decision.
    pub fn pending_deliveries(&self, limit: usize) -> Result<Vec<PendingDelivery>> {
        let limit = i64::try_from(limit.min(MAX_PUBLIC_LIST_RESULTS))
            .map_err(|_| ClientError::Config("pending-delivery limit exceeds SQLite".into()))?;
        let mut statement = self.conn.prepare(
            "SELECT row, message_id, to_addr, loft_url, attempts, created_at,
                    next_attempt_at, last_error
             FROM outbox
             WHERE sent_at IS NULL AND terminal_at IS NULL
             ORDER BY created_at, row LIMIT ?1",
        )?;
        let rows = statement.query_map(params![limit], |row| {
            Ok((
                row.get::<_, i64>(0)?,
                row.get::<_, String>(1)?,
                row.get::<_, String>(2)?,
                row.get::<_, String>(3)?,
                row.get::<_, i64>(4)?,
                row.get::<_, i64>(5)?,
                row.get::<_, i64>(6)?,
                row.get::<_, Option<String>>(7)?,
            ))
        })?;
        let mut deliveries = Vec::new();
        for row in rows {
            let (
                row,
                message_id,
                to_addr,
                loft_url,
                attempts,
                created_at,
                next_attempt_at,
                last_error,
            ) = row?;
            if let Some(reason) = last_error.as_deref() {
                validate_outbox_reason(reason)?;
            }
            deliveries.push(PendingDelivery {
                row: OutboxRecordId::new(row)?,
                message_id,
                to_addr,
                loft_url,
                attempts: sqlite_u32(attempts, "stored outbox attempt count")?,
                created_at: sqlite_u64(created_at, "stored outbox creation timestamp")?,
                next_attempt_at: sqlite_u64(next_attempt_at, "stored outbox retry timestamp")?,
                last_error,
            });
        }
        Ok(deliveries)
    }

    /// Return a bounded, deterministic operator view of terminal outbox copies.
    pub fn dead_letters(&self, limit: usize) -> Result<Vec<DeadLetter>> {
        let mut statement = self.conn.prepare(
            "SELECT row, message_id, to_addr, loft_url, attempts, terminal_reason, terminal_at
             FROM outbox
             WHERE sent_at IS NULL AND terminal_at IS NOT NULL
             ORDER BY terminal_at DESC, row DESC LIMIT ?1",
        )?;
        let limit = i64::try_from(limit.min(MAX_DEAD_LETTER_RESULTS))
            .map_err(|_| ClientError::Config("dead-letter limit exceeds SQLite".into()))?;
        let rows = statement.query_map(params![limit], |row| {
            Ok((
                row.get::<_, i64>(0)?,
                row.get::<_, String>(1)?,
                row.get::<_, String>(2)?,
                row.get::<_, String>(3)?,
                row.get::<_, i64>(4)?,
                row.get::<_, String>(5)?,
                row.get::<_, i64>(6)?,
            ))
        })?;
        let mut letters = Vec::new();
        for row in rows {
            let (row, message_id, to_addr, loft_url, attempts, reason, terminal_at) = row?;
            letters.push(DeadLetter {
                row: OutboxRecordId::new(row)?,
                message_id,
                to_addr,
                loft_url,
                attempts: sqlite_u32(attempts, "stored outbox attempt count")?,
                reason,
                terminal_at: sqlite_u64(terminal_at, "stored terminal timestamp")?,
            });
        }
        Ok(letters)
    }

    /// Return a bounded operator view of successful outbox metadata. No payload or credential is
    /// retained after success.
    pub fn completed_deliveries(&self, limit: usize) -> Result<Vec<CompletedDelivery>> {
        let limit = i64::try_from(limit.min(MAX_PUBLIC_LIST_RESULTS))
            .map_err(|_| ClientError::Config("completed-delivery limit exceeds SQLite".into()))?;
        let mut statement = self.conn.prepare(
            "SELECT row, message_id, to_addr, loft_url, attempts, sent_at
             FROM outbox WHERE sent_at IS NOT NULL
             ORDER BY sent_at DESC, row DESC LIMIT ?1",
        )?;
        let rows = statement.query_map(params![limit], |row| {
            Ok((
                row.get::<_, i64>(0)?,
                row.get::<_, String>(1)?,
                row.get::<_, String>(2)?,
                row.get::<_, String>(3)?,
                row.get::<_, i64>(4)?,
                row.get::<_, i64>(5)?,
            ))
        })?;
        let mut deliveries = Vec::new();
        for row in rows {
            let (row, message_id, to_addr, loft_url, attempts, sent_at) = row?;
            deliveries.push(CompletedDelivery {
                row: OutboxRecordId::new(row)?,
                message_id,
                to_addr,
                loft_url,
                attempts: sqlite_u32(attempts, "stored outbox attempt count")?,
                sent_at: sqlite_u64(sent_at, "stored sent timestamp")?,
            });
        }
        Ok(deliveries)
    }

    /// Delete exactly one successful delivery metadata row.
    pub fn delete_completed_delivery(&self, row: OutboxRecordId) -> Result<bool> {
        self.delete_finished_outbox(row, true)
    }

    /// Delete exactly one terminal/dead-letter metadata row.
    pub fn delete_dead_letter(&self, row: OutboxRecordId) -> Result<bool> {
        self.delete_finished_outbox(row, false)
    }

    fn delete_finished_outbox(&self, row: OutboxRecordId, sent: bool) -> Result<bool> {
        let tx = Transaction::new_unchecked(&self.conn, TransactionBehavior::Immediate)?;
        let predicate = if sent {
            "sent_at IS NOT NULL"
        } else {
            "sent_at IS NULL AND terminal_at IS NOT NULL"
        };
        let changed = tx.execute(
            &format!("DELETE FROM outbox WHERE row = ?1 AND {predicate}"),
            params![row.get()],
        )?;
        if changed > 0 {
            uncharge_outbox_rows(&tx, changed as u64)?;
        }
        tx.commit()?;
        Ok(changed > 0)
    }

    /// Explicitly abandon exactly one undelivered copy. This is never called by automatic
    /// maintenance and requires the public confirmation phrase at every invocation.
    pub fn delete_pending_outbox(&self, row: OutboxRecordId, confirmation: &str) -> Result<bool> {
        if confirmation != PENDING_OUTBOX_DELETE_CONFIRMATION {
            return Err(ClientError::Config(
                "pending outbox deletion requires exact confirmation".into(),
            ));
        }
        let tx = Transaction::new_unchecked(&self.conn, TransactionBehavior::Immediate)?;
        let payload_bytes = active_outbox_payload_bytes(&tx, row.get())?;
        let Some(payload_bytes) = payload_bytes else {
            tx.commit()?;
            return Ok(false);
        };
        let changed = tx.execute(
            "DELETE FROM outbox
             WHERE row = ?1 AND sent_at IS NULL AND terminal_at IS NULL",
            params![row.get()],
        )?;
        if changed == 1 {
            uncharge_outbox_payload(&tx, payload_bytes)?;
            uncharge_outbox_rows(&tx, 1)?;
        }
        tx.commit()?;
        Ok(changed == 1)
    }

    /// Delete at most `limit` successful metadata rows older than `before`. Terminal dead letters
    /// require exact operator deletion and inbound message bodies are outside this lifecycle.
    pub fn prune_completed_outbox(&self, before: u64, limit: usize) -> Result<usize> {
        let limit = limit.min(MAX_PUBLIC_LIST_RESULTS);
        if limit == 0 {
            return Ok(0);
        }
        let before = sqlite_i64(before, "completed outbox retention timestamp")?;
        let limit = i64::try_from(limit)
            .map_err(|_| ClientError::Config("outbox prune limit exceeds SQLite".into()))?;
        let tx = Transaction::new_unchecked(&self.conn, TransactionBehavior::Immediate)?;
        let changed = tx.execute(
            "DELETE FROM outbox WHERE row IN (
                 SELECT row FROM outbox
                 WHERE sent_at IS NOT NULL AND sent_at < ?1
                 ORDER BY sent_at, row LIMIT ?2
             )",
            params![before, limit],
        )?;
        if changed > 0 {
            uncharge_outbox_rows(&tx, changed as u64)?;
        }
        tx.commit()?;
        Ok(changed)
    }

    /// Explicitly remove bounded payload-free metadata for successful or terminal copies.
    /// Automatic wake maintenance deliberately calls the completed-only helper above instead.
    pub fn prune_finished_outbox(
        &self,
        before: u64,
        limit: usize,
        confirmation: &str,
    ) -> Result<usize> {
        if confirmation != FINISHED_OUTBOX_PRUNE_CONFIRMATION {
            return Err(ClientError::Config(
                "finished outbox pruning requires exact confirmation".into(),
            ));
        }
        let limit = limit.min(MAX_PUBLIC_LIST_RESULTS);
        if limit == 0 {
            return Ok(0);
        }
        let before = sqlite_i64(before, "finished outbox retention timestamp")?;
        let limit = i64::try_from(limit)
            .map_err(|_| ClientError::Config("outbox prune limit exceeds SQLite".into()))?;
        let tx = Transaction::new_unchecked(&self.conn, TransactionBehavior::Immediate)?;
        let changed = tx.execute(
            "DELETE FROM outbox WHERE row IN (
                 SELECT row FROM outbox
                 WHERE (sent_at IS NOT NULL AND sent_at < ?1)
                    OR (sent_at IS NULL AND terminal_at IS NOT NULL AND terminal_at < ?1)
                 ORDER BY COALESCE(sent_at, terminal_at), row LIMIT ?2
             )",
            params![before, limit],
        )?;
        if changed > 0 {
            uncharge_outbox_rows(&tx, changed as u64)?;
        }
        tx.commit()?;
        Ok(changed)
    }

    /// Delivery status for one logical message. Send reports must not count unrelated backlog
    /// entries that happened to be attempted by the same bounded flush.
    pub fn delivery_status(&self, message_id: &str) -> Result<DeliveryStatus> {
        let (delivered, queued, terminal): (i64, i64, i64) = self.conn.query_row(
            "SELECT COALESCE(SUM(CASE WHEN sent_at IS NOT NULL THEN 1 ELSE 0 END), 0),
                    COALESCE(SUM(CASE WHEN sent_at IS NULL AND terminal_at IS NULL
                                      THEN 1 ELSE 0 END), 0),
                    COALESCE(SUM(CASE WHEN sent_at IS NULL AND terminal_at IS NOT NULL
                                      THEN 1 ELSE 0 END), 0)
             FROM outbox WHERE message_id = ?1",
            params![message_id],
            |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
        )?;
        Ok(DeliveryStatus {
            delivered: sqlite_usize(delivered, "delivered outbox count")?,
            queued: sqlite_usize(queued, "queued outbox count")?,
            terminal: sqlite_usize(terminal, "terminal outbox count")?,
        })
    }

    // ---- cursors --------------------------------------------------------------------------

    pub fn cursor(&self, loft_url: &str, address: &Address) -> Result<u64> {
        let stored = self
            .conn
            .query_row(
                "SELECT cursor FROM cursors WHERE loft_url = ?1 AND address = ?2",
                params![loft_url, address.as_str()],
                |r| r.get::<_, i64>(0),
            )
            .optional()?
            .unwrap_or(0);
        sqlite_u64(stored, "stored loft cursor")
    }

    /// Cursors only move forward: a rewind would re-deliver mail the agent already handled.
    pub fn set_cursor(&self, loft_url: &str, address: &Address, cursor: u64) -> Result<()> {
        let cursor = sqlite_i64(cursor, "loft cursor")?;
        self.conn.execute(
            "INSERT INTO cursors (loft_url, address, cursor) VALUES (?1, ?2, ?3)
             ON CONFLICT(loft_url, address)
             DO UPDATE SET cursor = MAX(cursor, excluded.cursor)",
            params![loft_url, address.as_str(), cursor],
        )?;
        Ok(())
    }

    /// Adopt cursors written by schema v4. They belonged to the operating address at migration
    /// time; keeping the empty sentinel would either replay or skip mail after a rotation.
    pub fn adopt_legacy_cursors(&self, address: &Address) -> Result<()> {
        let tx = self.conn.unchecked_transaction()?;
        tx.execute(
            "INSERT INTO cursors (loft_url, address, cursor)
             SELECT loft_url, ?1, cursor FROM cursors WHERE address = ''
             ON CONFLICT(loft_url, address)
             DO UPDATE SET cursor = MAX(cursor, excluded.cursor)",
            params![address.as_str()],
        )?;
        tx.execute("DELETE FROM cursors WHERE address = ''", [])?;
        tx.commit()?;
        Ok(())
    }

    // ---- messages -------------------------------------------------------------------------

    /// Returns false when this id was already stored — the expected case for the second and third
    /// copies of a message published to several lofts.
    #[allow(clippy::too_many_arguments)]
    pub fn store_message(
        &self,
        id: &str,
        from_pubkey: &[u8; 32],
        from_address: &str,
        received_at: u64,
        body: &UntrustedBody,
        state: &str,
    ) -> Result<bool> {
        self.store_message_with_attribution(
            id,
            from_pubkey,
            from_address,
            received_at,
            body,
            state,
            Attribution::Absent,
        )
    }

    #[allow(clippy::too_many_arguments)]
    pub fn store_message_with_attribution(
        &self,
        id: &str,
        from_pubkey: &[u8; 32],
        from_address: &str,
        received_at: u64,
        body: &UntrustedBody,
        state: &str,
        attribution: Attribution,
    ) -> Result<bool> {
        validate_bounded_text(id, MAX_MESSAGE_ID_BYTES, "message id")?;
        validate_bounded_text(
            from_address,
            MAX_STORED_ADDRESS_BYTES,
            "stored sender address",
        )?;
        let body_bytes = u64::try_from(body.len())
            .map_err(|_| ClientError::Config("message body size overflowed".into()))?;
        if body.len() > MAX_INBOX_BODY_BYTES_PER_MESSAGE {
            return Err(ClientError::Core(pigeonpost_core::Error::TooLarge));
        }
        if !matches!(state, "accepted" | "pending") {
            return Err(ClientError::Config(
                "stored message state must be accepted or pending".into(),
            ));
        }
        let received_at = sqlite_i64(received_at, "message receive timestamp")?;
        let attribution = attribution_as_str(attribution);
        let tx = Transaction::new_unchecked(&self.conn, TransactionBehavior::Immediate)?;
        let existing = tx
            .query_row(
                "SELECT from_pubkey, from_address, body, attribution, state
                 FROM messages WHERE id = ?1",
                params![id],
                |row| {
                    Ok((
                        row.get::<_, Vec<u8>>(0)?,
                        row.get::<_, String>(1)?,
                        row.get::<_, String>(2)?,
                        row.get::<_, String>(3)?,
                        row.get::<_, String>(4)?,
                    ))
                },
            )
            .optional()?;
        if let Some((stored_key, stored_address, stored_body, stored_attribution, stored_state)) =
            existing
        {
            // Deletion retains only this id indefinitely. Every delayed or redundant loft copy is
            // deliberately absorbed so a cursor may commit without resurrecting erased content.
            if stored_state == "deleted" {
                tx.commit()?;
                return Ok(false);
            }
            if stored_key.as_slice() != from_pubkey
                || stored_address != from_address
                || stored_body != body.as_str()
                || stored_attribution != attribution
            {
                return Err(ClientError::Config(
                    "message idempotency key conflicts with different message data".into(),
                ));
            }
            tx.commit()?;
            return Ok(false);
        }

        let status = load_storage_status(&tx)?;
        let next_messages = checked_quota_add(
            status.usage.inbox_messages,
            1,
            status.limits.inbox_messages,
            StorageResource::InboxMessages,
        )?;
        let next_body_bytes = checked_quota_add(
            status.usage.inbox_body_bytes,
            body_bytes,
            status.limits.inbox_body_bytes,
            StorageResource::InboxBodyBytes,
        )?;
        tx.execute(
            "INSERT INTO messages
                 (id, from_pubkey, from_address, received_at, body, state, attribution)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)",
            params![
                id,
                from_pubkey.as_slice(),
                from_address,
                received_at,
                body.as_str(),
                state,
                attribution,
            ],
        )?;
        tx.execute(
            "UPDATE storage_accounting
             SET inbox_messages = ?1, inbox_body_bytes = ?2 WHERE id = 1",
            params![
                sqlite_i64(next_messages, "inbox message usage")?,
                sqlite_i64(next_body_bytes, "inbox body-byte usage")?
            ],
        )?;
        tx.commit()?;
        Ok(true)
    }

    /// Logically erase exactly one inbound message and retain its id as an indefinite replay
    /// tombstone. This operation is never run by automatic maintenance.
    pub fn delete_message(&self, id: &str) -> Result<bool> {
        self.delete_message_at(id, current_time_secs()?)
    }

    fn delete_message_at(&self, id: &str, deleted_at: u64) -> Result<bool> {
        validate_bounded_text(id, MAX_MESSAGE_ID_BYTES, "message id")?;
        let deleted_at = sqlite_i64(deleted_at, "message deletion timestamp")?;
        let tx = Transaction::new_unchecked(&self.conn, TransactionBehavior::Immediate)?;
        let stored = tx
            .query_row(
                "SELECT state, length(CAST(body AS BLOB)) FROM messages WHERE id = ?1",
                params![id],
                |row| Ok((row.get::<_, String>(0)?, row.get::<_, i64>(1)?)),
            )
            .optional()?;
        let Some((state, body_bytes)) = stored else {
            tx.commit()?;
            return Ok(false);
        };
        if state == "deleted" {
            tx.commit()?;
            return Ok(false);
        }
        if !matches!(state.as_str(), "accepted" | "pending") {
            return Err(ClientError::Config(
                "stored message state is malformed".into(),
            ));
        }
        let storage = load_storage_status(&tx)?;
        if storage.usage.inbox_tombstones >= MAX_INBOX_TOMBSTONES {
            return Err(ClientError::StorageLimit(StorageResource::InboxTombstones));
        }
        if storage.usage.inbox_messages == 0
            || sqlite_u64(body_bytes, "stored message body size")? > storage.usage.inbox_body_bytes
        {
            return Err(ClientError::Config(
                "inbox message accounting drifted".into(),
            ));
        }
        tx.execute("DELETE FROM spam_marks WHERE message_id = ?1", params![id])?;
        let changed = tx.execute(
            "UPDATE messages
             SET from_pubkey = X'', from_address = '', received_at = 0, read = 0,
                 body = '', state = 'deleted', attribution = 'absent', deleted_at = ?2
             WHERE id = ?1 AND state <> 'deleted'",
            params![id, deleted_at],
        )?;
        if changed == 1 {
            tombstone_inbox_message(&tx, sqlite_u64(body_bytes, "stored message body size")?)?;
        }
        tx.commit()?;
        Ok(changed == 1)
    }

    /// Messages held for review because their sender is unknown.
    pub fn pending_messages(&self, limit: usize) -> Result<Vec<StoredMessage>> {
        let limit = sqlite_i64(
            u64::try_from(limit.min(MAX_PUBLIC_LIST_RESULTS)).map_err(|_| {
                ClientError::Config("pending-message limit exceeds this platform".into())
            })?,
            "pending-message limit",
        )?;
        let mut stmt = self.conn.prepare(
            "SELECT id, from_pubkey, from_address, received_at, read, body, state, attribution FROM messages
             WHERE state = 'pending' ORDER BY received_at DESC, id LIMIT ?1",
        )?;
        let rows = stmt.query_map(params![limit], |row| {
            Ok((
                row.get::<_, String>(0)?,
                row.get::<_, Vec<u8>>(1)?,
                row.get::<_, String>(2)?,
                row.get::<_, i64>(3)?,
                row.get::<_, i64>(4)?,
                row.get::<_, String>(5)?,
                row.get::<_, String>(6)?,
                row.get::<_, String>(7)?,
            ))
        })?;

        let mut out = Vec::new();
        for row in rows {
            let (id, from_pubkey, from_address, received_at, read, body, state, attribution) = row?;
            out.push(StoredMessage {
                id,
                from_pubkey: fixed32(from_pubkey, "stored sender public key")?,
                from_address,
                received_at: sqlite_u64(received_at, "stored receive timestamp")?,
                read: read != 0,
                state,
                attribution: parse_attribution(&attribution)?,
                body: UntrustedBody::new(body),
            });
        }
        Ok(out)
    }

    /// Accept every pending message from one sender in a single bounded SQL operation.
    ///
    /// This deliberately does not page through the global pending queue: a busy inbox may hold
    /// more messages from other senders than a caller-facing page limit, and allowlisting must not
    /// leave older matching messages stranded behind that window.
    pub fn release_pending_from(&self, pubkey: &[u8; 32]) -> Result<usize> {
        Ok(self.conn.execute(
            "UPDATE messages SET state = 'accepted'
             WHERE state = 'pending' AND from_pubkey = ?1",
            params![pubkey.as_slice()],
        )?)
    }

    pub fn set_message_state(&self, id: &str, state: &str) -> Result<()> {
        validate_bounded_text(id, MAX_MESSAGE_ID_BYTES, "message id")?;
        if !matches!(state, "accepted" | "pending") {
            return Err(ClientError::Config(
                "stored message state must be accepted or pending".into(),
            ));
        }
        let changed = self.conn.execute(
            "UPDATE messages SET state = ?2 WHERE id = ?1 AND state <> 'deleted'",
            params![id, state],
        )?;
        if changed == 0
            && self
                .conn
                .query_row(
                    "SELECT state = 'deleted' FROM messages WHERE id = ?1",
                    params![id],
                    |row| row.get::<_, bool>(0),
                )
                .optional()?
                .unwrap_or(false)
        {
            return Err(ClientError::Config("message has been deleted".into()));
        }
        Ok(())
    }

    // ---- allowlist and scores -------------------------------------------------------------

    pub fn allow(&self, pubkey: &[u8; 32], reason: &str, now: u64) -> Result<()> {
        self.conn.execute(
            "INSERT OR IGNORE INTO allowlist (pubkey, added_at, reason) VALUES (?1, ?2, ?3)",
            params![pubkey.as_slice(), now, reason],
        )?;
        Ok(())
    }

    pub fn disallow(&self, pubkey: &[u8; 32]) -> Result<bool> {
        Ok(self.conn.execute(
            "DELETE FROM allowlist WHERE pubkey = ?1",
            params![pubkey.as_slice()],
        )? > 0)
    }

    pub fn is_allowed(&self, pubkey: &[u8; 32]) -> Result<bool> {
        Ok(self
            .conn
            .query_row(
                "SELECT 1 FROM allowlist WHERE pubkey = ?1",
                params![pubkey.as_slice()],
                |_| Ok(()),
            )
            .optional()?
            .is_some())
    }

    pub fn score(&self, pubkey: &[u8; 32]) -> Result<(i64, u64)> {
        let found = self
            .conn
            .query_row(
                "SELECT score, updated_at FROM scores WHERE pubkey = ?1",
                params![pubkey.as_slice()],
                |row| Ok((row.get::<_, i64>(0)?, row.get::<_, i64>(1)?)),
            )
            .optional()?
            .unwrap_or((0, 0));
        Ok((
            found.0,
            sqlite_u64(found.1, "stored score update timestamp")?,
        ))
    }

    /// Atomically grant reply trust, apply its one-time score floor, and release held messages.
    /// Repeating the action is a no-op unless another action removed the allowlist entry first.
    pub(crate) fn allow_sender(&self, pubkey: &[u8; 32], reason: &str, now: u64) -> Result<usize> {
        let timestamp = sqlite_i64(now, "allowlist timestamp")?;
        let tx = Transaction::new_unchecked(&self.conn, TransactionBehavior::Immediate)?;
        let newly_allowed = tx.execute(
            "INSERT OR IGNORE INTO allowlist (pubkey, added_at, reason) VALUES (?1, ?2, ?3)",
            params![pubkey.as_slice(), timestamp, reason],
        )?;
        if newly_allowed == 1 {
            Self::set_score_floor_tx(&tx, pubkey, crate::spam::SCORE_CORRESPONDED, now)?;
        }
        let released = tx.execute(
            "UPDATE messages SET state = 'accepted'
             WHERE state = 'pending' AND from_pubkey = ?1",
            params![pubkey.as_slice()],
        )?;
        tx.commit()?;
        Ok(released)
    }

    /// Atomically remove reply trust and enforce the block score ceiling.
    pub(crate) fn block_sender(&self, pubkey: &[u8; 32], now: u64) -> Result<i64> {
        let tx = Transaction::new_unchecked(&self.conn, TransactionBehavior::Immediate)?;
        tx.execute(
            "DELETE FROM allowlist WHERE pubkey = ?1",
            params![pubkey.as_slice()],
        )?;
        let score = Self::set_score_ceiling_tx(
            &tx,
            pubkey,
            crate::spam::SCORE_MARKED_SPAM.saturating_mul(2),
            now,
        )?;
        tx.commit()?;
        Ok(score)
    }

    /// Atomically mark one exact message, revoke its sender's reply trust, and apply one penalty.
    /// The durable message-id marker makes retries and concurrent invocations idempotent.
    pub(crate) fn mark_spam(&self, id: &str, expected_pubkey: &[u8; 32], now: u64) -> Result<i64> {
        let timestamp = sqlite_i64(now, "spam-mark timestamp")?;
        let tx = Transaction::new_unchecked(&self.conn, TransactionBehavior::Immediate)?;
        let stored_pubkey = tx
            .query_row(
                "SELECT from_pubkey FROM messages WHERE id = ?1 AND state <> 'deleted'",
                params![id],
                |row| row.get::<_, Vec<u8>>(0),
            )
            .optional()?
            .ok_or_else(|| ClientError::Config("message disappeared before spam marking".into()))?;
        if fixed32(stored_pubkey, "stored sender public key")? != *expected_pubkey {
            return Err(ClientError::Config(
                "message sender changed before spam marking".into(),
            ));
        }
        tx.execute(
            "DELETE FROM allowlist WHERE pubkey = ?1",
            params![expected_pubkey.as_slice()],
        )?;
        tx.execute(
            "UPDATE messages SET state = 'pending' WHERE id = ?1",
            params![id],
        )?;
        let newly_marked = tx.execute(
            "INSERT OR IGNORE INTO spam_marks (message_id, marked_at) VALUES (?1, ?2)",
            params![id, timestamp],
        )?;
        let score = if newly_marked == 1 {
            Self::adjust_score_tx(&tx, expected_pubkey, crate::spam::SCORE_MARKED_SPAM, now)?
        } else {
            Self::effective_score_tx(&tx, expected_pubkey, now)?
        };
        tx.commit()?;
        Ok(score)
    }

    /// Adjust a sender's score, folding in the decay accrued since it was last touched so a
    /// stale value is never resurrected at full strength.
    pub fn adjust_score(&self, pubkey: &[u8; 32], delta: i64, now: u64) -> Result<i64> {
        // Reserve the writer before reading. Separate Agent instances (and separate processes)
        // share this WAL; a read followed by a standalone UPSERT loses increments when two
        // operators act at once.
        let tx = Transaction::new_unchecked(&self.conn, TransactionBehavior::Immediate)?;
        let next = Self::adjust_score_tx(&tx, pubkey, delta, now)?;
        tx.commit()?;
        Ok(next)
    }

    fn effective_score_tx(tx: &Transaction<'_>, pubkey: &[u8; 32], now: u64) -> Result<i64> {
        let found = tx
            .query_row(
                "SELECT score, updated_at FROM scores WHERE pubkey = ?1",
                params![pubkey.as_slice()],
                |row| Ok((row.get::<_, i64>(0)?, row.get::<_, i64>(1)?)),
            )
            .optional()?
            .unwrap_or((0, 0));
        let updated_at = sqlite_u64(found.1, "stored score update timestamp")?;
        Ok(crate::spam::effective_score(found.0, updated_at, now))
    }

    fn write_score_tx(tx: &Transaction<'_>, pubkey: &[u8; 32], score: i64, now: u64) -> Result<()> {
        let updated_at = sqlite_i64(now, "score update timestamp")?;
        tx.execute(
            "INSERT INTO scores (pubkey, score, updated_at) VALUES (?1, ?2, ?3)
             ON CONFLICT(pubkey) DO UPDATE SET score = excluded.score,
                                               updated_at = excluded.updated_at",
            params![pubkey.as_slice(), score, updated_at],
        )?;
        Ok(())
    }

    fn adjust_score_tx(
        tx: &Transaction<'_>,
        pubkey: &[u8; 32],
        delta: i64,
        now: u64,
    ) -> Result<i64> {
        let aged = Self::effective_score_tx(tx, pubkey, now)?;
        let next = aged.saturating_add(delta).clamp(-10_000, 10_000);
        Self::write_score_tx(tx, pubkey, next, now)?;
        Ok(next)
    }

    fn set_score_floor_tx(
        tx: &Transaction<'_>,
        pubkey: &[u8; 32],
        floor: i64,
        now: u64,
    ) -> Result<i64> {
        let aged = Self::effective_score_tx(tx, pubkey, now)?;
        let next = aged.max(floor);
        if next != aged {
            Self::write_score_tx(tx, pubkey, next, now)?;
        }
        Ok(next)
    }

    fn set_score_ceiling_tx(
        tx: &Transaction<'_>,
        pubkey: &[u8; 32],
        ceiling: i64,
        now: u64,
    ) -> Result<i64> {
        let aged = Self::effective_score_tx(tx, pubkey, now)?;
        let next = aged.min(ceiling);
        if next != aged {
            Self::write_score_tx(tx, pubkey, next, now)?;
        }
        Ok(next)
    }

    pub fn messages(&self, unread_only: bool, limit: usize) -> Result<Vec<StoredMessage>> {
        let sql = if unread_only {
            "SELECT id, from_pubkey, from_address, received_at, read, body, state, attribution FROM messages
             WHERE read = 0 AND state = 'accepted' ORDER BY received_at DESC, id LIMIT ?1"
        } else {
            "SELECT id, from_pubkey, from_address, received_at, read, body, state, attribution FROM messages
             WHERE state = 'accepted' ORDER BY received_at DESC, id LIMIT ?1"
        };

        let limit = sqlite_i64(
            u64::try_from(limit.min(MAX_PUBLIC_LIST_RESULTS))
                .map_err(|_| ClientError::Config("message limit exceeds this platform".into()))?,
            "message limit",
        )?;
        let mut stmt = self.conn.prepare(sql)?;
        let rows = stmt.query_map(params![limit], |row| {
            Ok((
                row.get::<_, String>(0)?,
                row.get::<_, Vec<u8>>(1)?,
                row.get::<_, String>(2)?,
                row.get::<_, i64>(3)?,
                row.get::<_, i64>(4)?,
                row.get::<_, String>(5)?,
                row.get::<_, String>(6)?,
                row.get::<_, String>(7)?,
            ))
        })?;

        let mut out = Vec::new();
        for row in rows {
            let (id, from_pubkey, from_address, received_at, read, body, state, attribution) = row?;
            out.push(StoredMessage {
                id,
                from_pubkey: fixed32(from_pubkey, "stored sender public key")?,
                from_address,
                received_at: sqlite_u64(received_at, "stored receive timestamp")?,
                read: read != 0,
                state,
                attribution: parse_attribution(&attribution)?,
                body: UntrustedBody::new(body),
            });
        }
        Ok(out)
    }

    pub fn message(&self, id: &str) -> Result<Option<StoredMessage>> {
        validate_bounded_text(id, MAX_MESSAGE_ID_BYTES, "message id or prefix")?;
        let prefix = escape_like_prefix(id);
        let mut stmt = self.conn.prepare(
            "SELECT id, from_pubkey, from_address, received_at, read, body, state, attribution FROM messages
             WHERE state <> 'deleted' AND (id = ?1 OR id LIKE ?2 ESCAPE '\\')
             ORDER BY id LIMIT 2",
        )?;
        let mut rows = stmt.query(params![id, prefix])?;
        let Some(first) = rows.next()? else {
            return Ok(None);
        };
        let found = (
            first.get::<_, String>(0)?,
            first.get::<_, Vec<u8>>(1)?,
            first.get::<_, String>(2)?,
            first.get::<_, i64>(3)?,
            first.get::<_, i64>(4)?,
            first.get::<_, String>(5)?,
            first.get::<_, String>(6)?,
            first.get::<_, String>(7)?,
        );
        if rows.next()?.is_some() {
            return Err(ClientError::AmbiguousMessage(id.to_owned()));
        }

        let (id, from_pubkey, from_address, received_at, read, body, state, attribution) = found;
        Ok(Some(StoredMessage {
            id,
            from_pubkey: fixed32(from_pubkey, "stored sender public key")?,
            from_address,
            received_at: sqlite_u64(received_at, "stored receive timestamp")?,
            read: read != 0,
            state,
            attribution: parse_attribution(&attribution)?,
            body: UntrustedBody::new(body),
        }))
    }

    pub fn mark_read(&self, id: &str) -> Result<bool> {
        validate_bounded_text(id, MAX_MESSAGE_ID_BYTES, "message id")?;
        Ok(self.conn.execute(
            "UPDATE messages SET read = 1 WHERE id = ?1 AND state <> 'deleted'",
            params![id],
        )? > 0)
    }

    pub fn unread_count(&self) -> Result<u64> {
        let count = self.conn.query_row(
            "SELECT COUNT(*) FROM messages WHERE read = 0 AND state = 'accepted'",
            [],
            |r| r.get::<_, i64>(0),
        )?;
        sqlite_u64(count, "unread message count")
    }

    // ---- resolution cache ----------------------------------------------------------------

    pub fn save_resolution(
        &self,
        address: &Address,
        resolution: &Resolution,
        fetched_at: u64,
    ) -> Result<Resolution> {
        // Network records use unsigned protocol counters while SQLite INTEGER is signed. Perform
        // every narrowing conversion before opening a transaction so failure cannot refresh or
        // partially mutate an otherwise valid cached resolution.
        let sequence = sqlite_i64(resolution.seq, "resolution sequence")?;
        let fetched_at = sqlite_i64(fetched_at, "resolution fetch timestamp")?;
        let pow_min = i64::from(resolution.pow_min);
        let attribution_requirement = resolution
            .attribution_requirement
            .map(|requirement| requirement.encode().map(Vec::from))
            .transpose()?;
        let tx = Transaction::new_unchecked(&self.conn, TransactionBehavior::Immediate)?;
        if let Some(known) = self.resolution(address)? {
            if known.successor_hash != resolution.successor_hash {
                return Err(ClientError::Core(pigeonpost_core::Error::SuccessorMismatch));
            }
            if resolution.seq < known.seq {
                return Err(ClientError::Core(pigeonpost_core::Error::StaleSequence));
            }
            if resolution.seq == known.seq {
                if known.pubkey != resolution.pubkey
                    || known.lofts != resolution.lofts
                    || known.pow_min != resolution.pow_min
                    || known.attribution_requirement != resolution.attribution_requirement
                {
                    return Err(ClientError::Core(pigeonpost_core::Error::StaleSequence));
                }
                tx.execute(
                    "UPDATE resolutions SET fetched_at = ?2 WHERE addr = ?1",
                    params![address.as_str(), fetched_at],
                )?;
                tx.commit()?;
                return Ok(known);
            }
        }

        let lofts = serde_json::to_string(&resolution.lofts)?;
        let changed = tx.execute(
            "INSERT INTO resolutions
                 (addr, pubkey, successor_hash, seq, lofts, fetched_at, pow_min,
                  attribution_requirement)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8)
             ON CONFLICT(addr) DO UPDATE SET pubkey = excluded.pubkey,
                                             seq = excluded.seq,
                                             lofts = excluded.lofts,
                                             fetched_at = excluded.fetched_at,
                                             pow_min = excluded.pow_min,
                                             attribution_requirement = excluded.attribution_requirement
             WHERE excluded.seq > resolutions.seq",
            params![
                address.as_str(),
                resolution.pubkey.as_slice(),
                resolution.successor_hash.as_slice(),
                sequence,
                lofts,
                fetched_at,
                pow_min,
                attribution_requirement
            ],
        )?;
        if changed != 1 {
            return Err(ClientError::Core(pigeonpost_core::Error::StaleSequence));
        }
        tx.commit()?;
        Ok(resolution.clone())
    }

    pub fn resolution(&self, address: &Address) -> Result<Option<Resolution>> {
        let found = self
            .conn
            .query_row(
                "SELECT pubkey, successor_hash, seq, lofts, pow_min, attribution_requirement
                 FROM resolutions
                 WHERE addr = ?1",
                params![address.as_str()],
                |row| {
                    Ok((
                        row.get::<_, Vec<u8>>(0)?,
                        row.get::<_, Vec<u8>>(1)?,
                        row.get::<_, i64>(2)?,
                        row.get::<_, String>(3)?,
                        row.get::<_, i64>(4)?,
                        row.get::<_, Option<Vec<u8>>>(5)?,
                    ))
                },
            )
            .optional()?;

        found
            .map(
                |(pubkey, successor_hash, seq, lofts, pow_min, attribution_requirement)| {
                    Ok(Resolution {
                        pubkey: pubkey.try_into().map_err(|_| {
                            ClientError::Config("stored resolution has malformed pubkey".into())
                        })?,
                        successor_hash: successor_hash.try_into().map_err(|_| {
                            ClientError::Config(
                                "stored resolution has malformed successor commitment".into(),
                            )
                        })?,
                        seq: sqlite_u64(seq, "stored resolution sequence")?,
                        lofts: serde_json::from_str(&lofts)?,
                        pow_min: u32::try_from(pow_min).map_err(|_| {
                            ClientError::Config(
                                "stored resolution proof-of-work floor is malformed".into(),
                            )
                        })?,
                        attribution_requirement: attribution_requirement
                            .map(|encoded| AttributionRequirement::decode(&encoded))
                            .transpose()?,
                    })
                },
            )
            .transpose()
    }

    // ---- own record placement -------------------------------------------------------------

    /// Atomically retain one exact signed record and its complete current target set.
    ///
    /// Completion survives when the signed bytes and URL remain identical. A changed record
    /// invalidates every completion, while a directory-membership change invalidates only newly
    /// introduced targets. This transaction is committed before any publication request starts.
    pub(crate) fn save_record_publication(
        &self,
        address: &Address,
        record: &AgentRecord,
        targets: &[PublicationTarget],
        updated_at: u64,
    ) -> Result<()> {
        record.verify(address)?;
        validate_publication_targets(targets)?;
        let record_json = serde_json::to_vec(record)?;
        if record_json.len() > MAX_PUBLICATION_RECORD_BYTES {
            return Err(ClientError::Config(
                "signed agent record exceeds the publication journal bound".into(),
            ));
        }
        let updated_at = sqlite_i64(updated_at, "record publication timestamp")?;
        let tx = Transaction::new_unchecked(&self.conn, TransactionBehavior::Immediate)?;
        let existing: Option<(String, Vec<u8>)> = tx
            .query_row(
                "SELECT address, record FROM own_record_publication WHERE id = 1",
                [],
                |row| Ok((row.get(0)?, row.get(1)?)),
            )
            .optional()?;
        let same_record = existing
            .as_ref()
            .is_some_and(|(known_address, known_record)| {
                known_address == address.as_str() && known_record == &record_json
            });
        if let Some((known_address, known_record)) = existing.as_ref() {
            if known_address == address.as_str() && known_record != &record_json {
                let known: AgentRecord = serde_json::from_slice(known_record)?;
                if known.seq >= record.seq {
                    return Err(ClientError::Core(pigeonpost_core::Error::StaleSequence));
                }
            }
        }

        let mut completed = HashSet::new();
        let mut merged = targets.to_vec();
        if same_record {
            let mut statement = tx.prepare(
                "SELECT url, allow_local, rendezvous, completed
                 FROM own_record_publication_targets",
            )?;
            let rows = statement.query_map([], publication_target_from_row)?;
            for row in rows {
                let previous = row?;
                if previous.completed {
                    completed.insert(previous.url);
                } else if let Some(current) =
                    merged.iter_mut().find(|target| target.url == previous.url)
                {
                    current.allow_local |= previous.allow_local;
                    current.rendezvous |= previous.rendezvous;
                } else if !previous.allow_local {
                    merged.push(previous);
                }
            }
        }
        validate_publication_targets(&merged)?;

        tx.execute("DELETE FROM own_record_publication_targets", [])?;
        tx.execute("DELETE FROM own_record_publication", [])?;
        tx.execute(
            "INSERT INTO own_record_publication (id, address, record, updated_at)
             VALUES (1, ?1, ?2, ?3)",
            params![address.as_str(), record_json, updated_at],
        )?;
        for target in &merged {
            tx.execute(
                "INSERT INTO own_record_publication_targets
                     (url, allow_local, rendezvous, completed)
                 VALUES (?1, ?2, ?3, ?4)",
                params![
                    target.url,
                    i64::from(target.allow_local),
                    i64::from(target.rendezvous),
                    i64::from(completed.contains(&target.url)),
                ],
            )?;
        }
        tx.commit()?;
        Ok(())
    }

    pub(crate) fn record_publication(&self) -> Result<Option<OwnRecordPublication>> {
        let stored: Option<(String, Vec<u8>)> = self
            .conn
            .query_row(
                "SELECT address, record FROM own_record_publication WHERE id = 1",
                [],
                |row| Ok((row.get(0)?, row.get(1)?)),
            )
            .optional()?;
        let Some((address, record)) = stored else {
            return Ok(None);
        };
        if record.len() > MAX_PUBLICATION_RECORD_BYTES {
            return Err(ClientError::Config(
                "stored agent-record publication exceeds its size bound".into(),
            ));
        }
        let address = Address::parse(&address)?;
        let record: AgentRecord = serde_json::from_slice(&record)?;
        record.verify(&address)?;
        let mut statement = self.conn.prepare(
            "SELECT url, allow_local, rendezvous, completed
             FROM own_record_publication_targets ORDER BY url",
        )?;
        let rows = statement.query_map([], publication_target_from_row)?;
        let targets = rows.collect::<std::result::Result<Vec<_>, _>>()?;
        validate_publication_targets(&targets)?;
        Ok(Some(OwnRecordPublication {
            address,
            record,
            targets,
        }))
    }

    /// Mark success only if the exact signed bytes are still the active plan. A delayed response
    /// from a superseded request can therefore never complete a newer publication accidentally.
    pub(crate) fn mark_record_target_complete(
        &self,
        address: &Address,
        record: &AgentRecord,
        url: &str,
    ) -> Result<bool> {
        let record_json = serde_json::to_vec(record)?;
        let changed = self.conn.execute(
            "UPDATE own_record_publication_targets SET completed = 1
             WHERE url = ?1
               AND EXISTS (
                   SELECT 1 FROM own_record_publication
                   WHERE id = 1 AND address = ?2 AND record = ?3
               )",
            params![url, address.as_str(), record_json],
        )?;
        Ok(changed == 1)
    }

    pub(crate) fn set_placement_health(&self, degraded: bool, attempted_at: u64) -> Result<()> {
        self.conn.execute(
            "UPDATE placement_health
             SET directory_refresh_degraded = ?1, last_attempt_at = ?2
             WHERE id = 1",
            params![
                i64::from(degraded),
                sqlite_i64(attempted_at, "placement maintenance timestamp")?
            ],
        )?;
        Ok(())
    }

    /// Synchronous operator/API status; no network request is hidden behind inspection.
    pub fn placement_state(&self) -> Result<PlacementState> {
        let publication = self.record_publication()?;
        let (record_targets, record_pending, rendezvous_targets, rendezvous_pending) = publication
            .as_ref()
            .map(|publication| {
                (
                    publication.targets.len(),
                    publication
                        .targets
                        .iter()
                        .filter(|target| !target.completed)
                        .count(),
                    publication
                        .targets
                        .iter()
                        .filter(|target| target.rendezvous)
                        .count(),
                    publication
                        .targets
                        .iter()
                        .filter(|target| target.rendezvous && !target.completed)
                        .count(),
                )
            })
            .unwrap_or_default();
        let (rotation_targets, rotation_pending): (i64, i64) = self.conn.query_row(
            "SELECT COUNT(*), COALESCE(SUM(completed = 0), 0)
             FROM own_rotation_publication_targets",
            [],
            |row| Ok((row.get(0)?, row.get(1)?)),
        )?;
        let rotations_without_targets: i64 = self.conn.query_row(
            "SELECT COUNT(*) FROM own_rotations AS rotation
             WHERE NOT EXISTS (
                 SELECT 1 FROM own_rotation_publication_targets AS target
                 WHERE target.from_addr = rotation.from_addr
             )",
            [],
            |row| row.get(0),
        )?;
        let configured_directories: i64 = self.conn.query_row(
            "SELECT COUNT(*) FROM directories WHERE enabled = 1",
            [],
            |row| row.get(0),
        )?;
        let (directory_refresh_degraded, last_attempt_at): (bool, Option<i64>) =
            self.conn.query_row(
                "SELECT directory_refresh_degraded, last_attempt_at
                 FROM placement_health WHERE id = 1",
                [],
                |row| Ok((row.get(0)?, row.get(1)?)),
            )?;
        Ok(PlacementState {
            record_seq: publication.map(|publication| publication.record.seq),
            record_targets,
            record_pending,
            rendezvous_targets,
            rendezvous_pending,
            rotation_targets: usize::try_from(rotation_targets).map_err(|_| {
                ClientError::Config("rotation publication target count overflowed".into())
            })?,
            rotation_pending: usize::try_from(rotation_pending).map_err(|_| {
                ClientError::Config("rotation publication pending count overflowed".into())
            })?,
            rotations_without_targets: usize::try_from(rotations_without_targets)
                .map_err(|_| ClientError::Config("rotation publication count overflowed".into()))?,
            configured_directories: usize::try_from(configured_directories)
                .map_err(|_| ClientError::Config("configured directory count overflowed".into()))?,
            directory_refresh_degraded,
            last_attempt_at: last_attempt_at
                .map(|value| sqlite_u64(value, "placement maintenance timestamp"))
                .transpose()?,
        })
    }

    // ---- rotation chains -----------------------------------------------------------------

    /// Save a fully verified immutable transition learned while resolving a peer.
    pub fn save_rotation(
        &self,
        from: &Address,
        record: &RotationRecord,
        fetched_at: u64,
    ) -> Result<()> {
        record.verify_source_address(from)?;
        let fetched_at = sqlite_i64(fetched_at, "rotation fetch timestamp")?;
        let encoded = serde_json::to_vec(record)?;
        let existing: Option<Vec<u8>> = self
            .conn
            .query_row(
                "SELECT record FROM rotation_chains WHERE from_addr = ?1",
                params![from.as_str()],
                |row| row.get(0),
            )
            .optional()?;
        if let Some(existing) = existing {
            if existing != encoded {
                return Err(ClientError::Core(pigeonpost_core::Error::StaleSequence));
            }
            self.conn.execute(
                "UPDATE rotation_chains SET fetched_at = ?2 WHERE from_addr = ?1",
                params![from.as_str(), fetched_at],
            )?;
            return Ok(());
        }
        self.conn.execute(
            "INSERT INTO rotation_chains (from_addr, to_addr, record, fetched_at)
             VALUES (?1, ?2, ?3, ?4)",
            params![
                from.as_str(),
                record.target_address()?.as_str(),
                encoded,
                fetched_at
            ],
        )?;
        Ok(())
    }

    pub fn rotation(&self, from: &Address) -> Result<Option<RotationRecord>> {
        self.conn
            .query_row(
                "SELECT record FROM rotation_chains WHERE from_addr = ?1",
                params![from.as_str()],
                |row| row.get::<_, Vec<u8>>(0),
            )
            .optional()?
            .map(|encoded| Ok(serde_json::from_slice(&encoded)?))
            .transpose()
    }

    /// Retain a locally-created transition so every wake-up can retry partial publication. The
    /// signed source/rotation pair is immutable; the target set may grow as lofts are added.
    pub fn save_own_rotation(
        &self,
        record: &RotationRecord,
        source_record: &AgentRecord,
        target_record: &AgentRecord,
        lofts: &[String],
    ) -> Result<()> {
        let from = Address::from_pubkey(&pigeonpost_core::keys::verifying_key_from_bytes(
            &record.from_pubkey,
        )?);
        source_record.verify(&from)?;
        record.verify_source_address(&from)?;
        record.verify(
            &source_record.successor_commitment(),
            source_record.seq,
            record.activated_at,
        )?;
        let target = record.target_address()?;
        target_record.verify(&target)?;
        if target_record.pubkey != record.to_pubkey
            || target_record.successor_hash != record.next_successor_hash
            || target_record.seq != record.seq
        {
            return Err(ClientError::Core(pigeonpost_core::Error::SuccessorMismatch));
        }
        let grace_until = sqlite_i64(record.grace_until, "rotation grace deadline")?;
        let record_json = serde_json::to_vec(record)?;
        let source_json = serde_json::to_vec(source_record)?;
        let target_json = serde_json::to_vec(target_record)?;
        let tx = Transaction::new_unchecked(&self.conn, TransactionBehavior::Immediate)?;
        let existing = tx
            .query_row(
                "SELECT record, source_record, target_record
                 FROM own_rotations WHERE from_addr = ?1",
                params![from.as_str()],
                |row| {
                    Ok((
                        row.get::<_, Vec<u8>>(0)?,
                        row.get::<_, Vec<u8>>(1)?,
                        row.get::<_, Vec<u8>>(2)?,
                    ))
                },
            )
            .optional()?;
        if let Some((known_record, known_source, known_target)) = existing.as_ref() {
            if known_record.as_slice() != record_json.as_slice()
                || known_source.as_slice() != source_json.as_slice()
                || known_target.as_slice() != target_json.as_slice()
            {
                return Err(ClientError::Core(pigeonpost_core::Error::StaleSequence));
            }
        } else {
            let full: bool = tx.query_row(
                "SELECT EXISTS(
                     SELECT 1 FROM own_rotations ORDER BY from_addr LIMIT 1 OFFSET ?1
                 )",
                params![(crate::keystore::MAX_LIVE_RETIRED_IDENTITIES - 1) as i64],
                |row| row.get(0),
            )?;
            if full {
                return Err(ClientError::Config(format!(
                    "at most {} local rotation journals may remain in grace",
                    crate::keystore::MAX_LIVE_RETIRED_IDENTITIES
                )));
            }
            tx.execute(
                "INSERT INTO own_rotations
                     (from_addr, record, source_record, target_record, lofts, grace_until)
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
                params![
                    from.as_str(),
                    record_json,
                    source_json,
                    target_json,
                    serde_json::to_string(lofts)?,
                    grace_until,
                ],
            )?;
        }
        let fetched_at = sqlite_i64(record.activated_at, "rotation fetch timestamp")?;
        let known_chain: Option<Vec<u8>> = tx
            .query_row(
                "SELECT record FROM rotation_chains WHERE from_addr = ?1",
                params![from.as_str()],
                |row| row.get(0),
            )
            .optional()?;
        if let Some(known_chain) = known_chain {
            if known_chain != record_json {
                return Err(ClientError::Core(pigeonpost_core::Error::StaleSequence));
            }
            tx.execute(
                "UPDATE rotation_chains SET fetched_at = ?2 WHERE from_addr = ?1",
                params![from.as_str(), fetched_at],
            )?;
        } else {
            tx.execute(
                "INSERT INTO rotation_chains (from_addr, to_addr, record, fetched_at)
                 VALUES (?1, ?2, ?3, ?4)",
                params![
                    from.as_str(),
                    record.target_address()?.as_str(),
                    record_json,
                    fetched_at
                ],
            )?;
        }
        tx.commit()?;
        Ok(())
    }

    /// Replace one immutable rotation bundle's current deterministic target set while preserving
    /// completion for unchanged URLs. The bundle itself is already durable in `own_rotations`.
    pub(crate) fn sync_own_rotation_targets(
        &self,
        from: &Address,
        targets: &[PublicationTarget],
    ) -> Result<()> {
        validate_publication_targets(targets)?;
        let exists: bool = self.conn.query_row(
            "SELECT EXISTS(SELECT 1 FROM own_rotations WHERE from_addr = ?1)",
            params![from.as_str()],
            |row| row.get(0),
        )?;
        if !exists {
            return Err(ClientError::Config(
                "rotation publication has no durable signed bundle".into(),
            ));
        }
        let tx = Transaction::new_unchecked(&self.conn, TransactionBehavior::Immediate)?;
        let mut completed = HashSet::new();
        let mut merged = targets.to_vec();
        {
            let mut statement = tx.prepare(
                "SELECT url, allow_local, rendezvous, completed
                 FROM own_rotation_publication_targets WHERE from_addr = ?1",
            )?;
            let rows = statement.query_map(params![from.as_str()], publication_target_from_row)?;
            for row in rows {
                let previous = row?;
                if previous.completed {
                    completed.insert(previous.url);
                } else if let Some(current) =
                    merged.iter_mut().find(|target| target.url == previous.url)
                {
                    current.allow_local |= previous.allow_local;
                    current.rendezvous |= previous.rendezvous;
                } else if !previous.allow_local {
                    // A directory outage must not erase an exact public retry plan. Explicit-local
                    // authorization is different: once absent from the current plan it is revoked.
                    merged.push(previous);
                }
            }
        }
        validate_publication_targets(&merged)?;
        tx.execute(
            "DELETE FROM own_rotation_publication_targets WHERE from_addr = ?1",
            params![from.as_str()],
        )?;
        for target in &merged {
            tx.execute(
                "INSERT INTO own_rotation_publication_targets
                     (from_addr, url, allow_local, rendezvous, completed)
                 VALUES (?1, ?2, ?3, ?4, ?5)",
                params![
                    from.as_str(),
                    target.url,
                    i64::from(target.allow_local),
                    i64::from(target.rendezvous),
                    i64::from(completed.contains(&target.url)),
                ],
            )?;
        }
        let urls: Vec<&str> = merged.iter().map(|target| target.url.as_str()).collect();
        tx.execute(
            "UPDATE own_rotations SET lofts = ?2 WHERE from_addr = ?1",
            params![from.as_str(), serde_json::to_string(&urls)?],
        )?;
        tx.commit()?;
        Ok(())
    }

    pub(crate) fn pending_rotation_targets(
        &self,
        limit: usize,
    ) -> Result<Vec<(OwnRotation, PublicationTarget)>> {
        let limit = limit.min(MAX_PUBLICATION_TARGETS);
        let mut statement = self.conn.prepare(
            "SELECT rotation.record, rotation.source_record, rotation.target_record,
                    rotation.lofts, target.url, target.allow_local, target.rendezvous,
                    target.completed
             FROM own_rotation_publication_targets AS target
             JOIN own_rotations AS rotation ON rotation.from_addr = target.from_addr
             WHERE target.completed = 0
             ORDER BY rotation.grace_until, target.from_addr, target.url
             LIMIT ?1",
        )?;
        let rows = statement.query_map(params![limit as i64], |row| {
            Ok((
                row.get::<_, Vec<u8>>(0)?,
                row.get::<_, Vec<u8>>(1)?,
                row.get::<_, Vec<u8>>(2)?,
                row.get::<_, String>(3)?,
                publication_target_from_row_offset(row, 4)?,
            ))
        })?;
        let mut pending = Vec::new();
        for row in rows {
            let (record, source_record, target_record, lofts, target) = row?;
            let rotation = OwnRotation {
                record: serde_json::from_slice(&record)?,
                source_record: serde_json::from_slice(&source_record)?,
                target_record: serde_json::from_slice(&target_record)?,
                lofts: serde_json::from_str(&lofts)?,
            };
            validate_own_rotation(&rotation)?;
            pending.push((rotation, target));
        }
        Ok(pending)
    }

    pub(crate) fn mark_rotation_target_complete(&self, from: &Address, url: &str) -> Result<bool> {
        let changed = self.conn.execute(
            "UPDATE own_rotation_publication_targets SET completed = 1
             WHERE from_addr = ?1 AND url = ?2
               AND EXISTS (
                   SELECT 1 FROM own_rotations WHERE from_addr = ?1
               )",
            params![from.as_str(), url],
        )?;
        Ok(changed == 1)
    }

    pub(crate) fn rotation_target_progress(&self, from: &Address) -> Result<(usize, usize)> {
        let (completed, pending): (i64, i64) = self.conn.query_row(
            "SELECT COALESCE(SUM(completed = 1), 0), COALESCE(SUM(completed = 0), 0)
             FROM own_rotation_publication_targets WHERE from_addr = ?1",
            params![from.as_str()],
            |row| Ok((row.get(0)?, row.get(1)?)),
        )?;
        Ok((
            usize::try_from(completed)
                .map_err(|_| ClientError::Config("rotation completed count overflowed".into()))?,
            usize::try_from(pending)
                .map_err(|_| ClientError::Config("rotation pending count overflowed".into()))?,
        ))
    }

    /// Remove a bounded number of locally-created publication journals whose signed grace window
    /// is over, along with the retired address's cursors and its locally-created chain copy.
    /// Remotely learned `rotation_chains` rows have no matching own journal and are never selected.
    pub(crate) fn prune_expired_own_rotations(&self, now: u64, limit: usize) -> Result<usize> {
        let limit = limit.min(crate::keystore::MAX_LIVE_RETIRED_IDENTITIES);
        if limit == 0 {
            return Ok(0);
        }
        let now = sqlite_i64(now, "rotation journal prune timestamp")?;
        let tx = Transaction::new_unchecked(&self.conn, TransactionBehavior::Immediate)?;
        let from_addresses = {
            let mut statement = tx.prepare(
                "SELECT from_addr FROM own_rotations
                 WHERE grace_until <= ?1
                 ORDER BY grace_until, from_addr LIMIT ?2",
            )?;
            let rows = statement
                .query_map(params![now, limit as i64], |row| row.get::<_, String>(0))?
                .collect::<std::result::Result<Vec<_>, _>>()?;
            rows
        };
        for from in &from_addresses {
            tx.execute("DELETE FROM cursors WHERE address = ?1", params![from])?;
            tx.execute(
                "DELETE FROM own_rotation_publication_targets WHERE from_addr = ?1",
                params![from],
            )?;
            tx.execute(
                "DELETE FROM rotation_chains WHERE from_addr = ?1",
                params![from],
            )?;
            tx.execute(
                "DELETE FROM own_rotations WHERE from_addr = ?1 AND grace_until <= ?2",
                params![from, now],
            )?;
        }
        tx.commit()?;
        Ok(from_addresses.len())
    }

    pub fn own_rotations(&self) -> Result<Vec<OwnRotation>> {
        let mut statement = self.conn.prepare(
            "SELECT record, source_record, target_record, lofts
             FROM own_rotations ORDER BY grace_until, from_addr LIMIT ?1",
        )?;
        let rows = statement.query_map(
            params![(crate::keystore::MAX_LIVE_RETIRED_IDENTITIES + 1) as i64],
            |row| {
                Ok((
                    row.get::<_, Vec<u8>>(0)?,
                    row.get::<_, Vec<u8>>(1)?,
                    row.get::<_, Vec<u8>>(2)?,
                    row.get::<_, String>(3)?,
                ))
            },
        )?;
        let mut rotations = Vec::new();
        for row in rows {
            let (record, source_record, target_record, lofts) = row?;
            let rotation = OwnRotation {
                record: serde_json::from_slice(&record)?,
                source_record: serde_json::from_slice(&source_record)?,
                target_record: serde_json::from_slice(&target_record)?,
                lofts: serde_json::from_str(&lofts)?,
            };
            validate_own_rotation(&rotation)?;
            rotations.push(rotation);
        }
        if rotations.len() > crate::keystore::MAX_LIVE_RETIRED_IDENTITIES {
            return Err(ClientError::Config(format!(
                "local rotation journal exceeds its {}-transition bound",
                crate::keystore::MAX_LIVE_RETIRED_IDENTITIES
            )));
        }
        Ok(rotations)
    }

    /// Load the one immutable locally-created transition from an exact source address.
    pub fn own_rotation(&self, from: &Address) -> Result<Option<OwnRotation>> {
        let stored = self
            .conn
            .query_row(
                "SELECT record, source_record, target_record, lofts
                 FROM own_rotations WHERE from_addr = ?1",
                params![from.as_str()],
                |row| {
                    Ok((
                        row.get::<_, Vec<u8>>(0)?,
                        row.get::<_, Vec<u8>>(1)?,
                        row.get::<_, Vec<u8>>(2)?,
                        row.get::<_, String>(3)?,
                    ))
                },
            )
            .optional()?;
        stored
            .map(|(record, source_record, target_record, lofts)| {
                let rotation = OwnRotation {
                    record: serde_json::from_slice(&record)?,
                    source_record: serde_json::from_slice(&source_record)?,
                    target_record: serde_json::from_slice(&target_record)?,
                    lofts: serde_json::from_str(&lofts)?,
                };
                validate_own_rotation(&rotation)?;
                Ok(rotation)
            })
            .transpose()
    }
}

fn finalize_expired_lofts_tx(tx: &Transaction<'_>, now: i64) -> Result<usize> {
    tx.execute(
        "UPDATE outbox SET allow_local = 0
         WHERE sent_at IS NULL AND loft_url IN (
             SELECT url FROM lofts
             WHERE state = 'draining' AND (drain_after IS NULL OR drain_after <= ?1)
         )",
        params![now],
    )?;
    tx.execute(
        "DELETE FROM cursors WHERE loft_url IN (
             SELECT url FROM lofts
             WHERE state = 'draining' AND (drain_after IS NULL OR drain_after <= ?1)
         )",
        params![now],
    )?;
    Ok(tx.execute(
        "DELETE FROM lofts
         WHERE state = 'draining' AND (drain_after IS NULL OR drain_after <= ?1)",
        params![now],
    )?)
}

fn publication_target_from_row(row: &rusqlite::Row<'_>) -> rusqlite::Result<PublicationTarget> {
    publication_target_from_row_offset(row, 0)
}

fn publication_target_from_row_offset(
    row: &rusqlite::Row<'_>,
    offset: usize,
) -> rusqlite::Result<PublicationTarget> {
    Ok(PublicationTarget {
        url: row.get(offset)?,
        allow_local: row.get(offset + 1)?,
        rendezvous: row.get(offset + 2)?,
        completed: row.get(offset + 3)?,
    })
}

fn validate_publication_targets(targets: &[PublicationTarget]) -> Result<()> {
    if targets.len() > MAX_PUBLICATION_TARGETS {
        return Err(ClientError::Config(
            "publication target set exceeds its fixed bound".into(),
        ));
    }
    let mut unique = HashSet::with_capacity(targets.len());
    for target in targets {
        validate_loft_list(std::slice::from_ref(&target.url))?;
        if !unique.insert(target.url.as_str()) {
            return Err(ClientError::Config(
                "publication target set contains a duplicate URL".into(),
            ));
        }
    }
    Ok(())
}

fn validate_own_rotation(rotation: &OwnRotation) -> Result<()> {
    let from = Address::from_pubkey(&pigeonpost_core::keys::verifying_key_from_bytes(
        &rotation.record.from_pubkey,
    )?);
    rotation.source_record.verify(&from)?;
    rotation.record.verify_source_address(&from)?;
    rotation.record.verify(
        &rotation.source_record.successor_commitment(),
        rotation.source_record.seq,
        rotation.record.activated_at,
    )?;
    let target = rotation.record.target_address()?;
    rotation.target_record.verify(&target)?;
    if rotation.target_record.pubkey != rotation.record.to_pubkey
        || rotation.target_record.successor_hash != rotation.record.next_successor_hash
        || rotation.target_record.seq != rotation.record.seq
    {
        return Err(ClientError::Core(pigeonpost_core::Error::SuccessorMismatch));
    }
    validate_publication_targets(
        &rotation
            .lofts
            .iter()
            .cloned()
            .map(|url| PublicationTarget::pending(url, false, false))
            .collect::<Vec<_>>(),
    )
}

fn load_handle_audit(
    conn: &Connection,
    configured: &RegistryConfiguration,
) -> Result<Option<HandleAuditState>> {
    let bytes: Option<Vec<u8>> = conn
        .query_row(
            "SELECT state FROM registry_handle_audit WHERE id = 1",
            [],
            |row| row.get(0),
        )
        .optional()?;
    let projected: bool = conn.query_row(
        "SELECT EXISTS(SELECT 1 FROM registry_handle_projection)",
        [],
        |row| row.get(0),
    )?;
    let Some(bytes) = bytes else {
        if projected {
            return Err(ClientError::Config(
                "handle projection exists without its complete audit".into(),
            ));
        }
        return Ok(None);
    };
    if bytes.len() > MAX_HANDLE_AUDIT_BYTES {
        return Err(ClientError::Config(
            "stored handle registry audit exceeds its size bound".into(),
        ));
    }
    let audit: HandleAuditState = serde_json::from_slice(&bytes)?;
    audit.validate(configured.trust.expected_origin())?;
    let pin = configured.checkpoint.as_ref().ok_or_else(|| {
        ClientError::Config("handle audit exists without a registry checkpoint pin".into())
    })?;
    if audit.size() > pin.size || (audit.size() == pin.size && audit.root() != &pin.root) {
        return Err(ClientError::Config(
            "stored handle audit conflicts with the registry checkpoint pin".into(),
        ));
    }
    Ok(Some(audit))
}

fn validate_handle_projection_binding(
    handle: &str,
    pubkey: &[u8],
    subject: &str,
    log_index: i64,
    audit_size: u64,
) -> Result<()> {
    let parsed = Handle::parse(handle)
        .map_err(|_| ClientError::Config("stored handle projection is malformed".into()))?;
    if parsed.as_path() != handle
        || pubkey.len() != 32
        || subject.is_empty()
        || subject.len() > MAX_HANDLE_SUBJECT_BYTES
        || log_index < 0
        || u64::try_from(log_index).map_or(true, |index| index >= audit_size)
    {
        return Err(ClientError::Config(
            "stored handle projection is malformed".into(),
        ));
    }
    Ok(())
}

fn save_handle_resolution_tx(
    tx: &Transaction<'_>,
    configured: &RegistryConfiguration,
    verified: &VerifiedHandle,
    now: u64,
) -> Result<Address> {
    let key = keys::verifying_key_from_bytes(verified.pubkey())
        .map_err(|_| ClientError::Config("verified handle key is invalid".into()))?;
    let address = Address::from_pubkey(&key);
    let handle = verified.handle().as_path();
    let witnessed_at = verified.witnessed_at();
    if verified.checkpoint().origin != configured.trust.expected_origin()
        || configured.trust.witness_threshold() == 0
        || witnessed_at.is_none()
        || verified.log_index() >= verified.checkpoint().size
    {
        return Err(ClientError::Config(
            "handle checkpoint does not satisfy configured trust".into(),
        ));
    }
    if let Some(known) = configured.checkpoint.as_ref() {
        if verified.checkpoint().size < known.size
            || (verified.checkpoint().size == known.size
                && verified.checkpoint().root != known.root)
        {
            return Err(ClientError::Core(pigeonpost_core::Error::StaleSequence));
        }
    }

    let projected: Option<(Vec<u8>, String, i64)> = tx
        .query_row(
            "SELECT pubkey, subject, log_index
             FROM registry_handle_projection WHERE handle = ?1",
            params![handle],
            |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
        )
        .optional()?;
    let Some((projected_key, projected_subject, projected_index)) = projected else {
        return Err(ClientError::Config(
            "complete handle projection omitted the resolved binding".into(),
        ));
    };
    if projected_key.as_slice() != verified.pubkey()
        || projected_subject != verified.subject()
        || sqlite_u64(projected_index, "projected handle log index")? != verified.log_index()
    {
        return Err(ClientError::Config(
            "resolved handle differs from the complete local projection".into(),
        ));
    }

    let existing: Option<(String, Vec<u8>, i64, i64)> = tx
        .query_row(
            "SELECT address, pubkey, log_index, checkpoint_size
             FROM handle_resolutions WHERE handle = ?1",
            params![handle],
            |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?, row.get(3)?)),
        )
        .optional()?;
    if let Some((known_address, known_key, known_index, known_size)) = existing {
        let known_size = sqlite_u64(known_size, "stored handle checkpoint size")?;
        let known_index = sqlite_u64(known_index, "stored handle log index")?;
        if verified.checkpoint().size < known_size
            || verified.log_index() < known_index
            || (verified.log_index() == known_index
                && (known_address != address.as_str() || known_key.as_slice() != verified.pubkey()))
        {
            return Err(ClientError::Core(pigeonpost_core::Error::StaleSequence));
        }
    }

    let pin_changed = tx.execute(
        "INSERT INTO registry_pin (id, size, root, witnessed_at)
         VALUES (1, ?1, ?2, ?3)
         ON CONFLICT(id) DO UPDATE SET
             size = excluded.size,
             root = excluded.root,
             witnessed_at = CASE
                 WHEN excluded.size > registry_pin.size THEN excluded.witnessed_at
                 WHEN registry_pin.witnessed_at IS NULL THEN excluded.witnessed_at
                 ELSE MAX(registry_pin.witnessed_at, excluded.witnessed_at)
             END
         WHERE excluded.size > registry_pin.size
            OR (excluded.size = registry_pin.size AND excluded.root = registry_pin.root)",
        params![
            sqlite_i64(verified.checkpoint().size, "registry checkpoint size")?,
            verified.checkpoint().root.as_slice(),
            witnessed_at
                .map(|value| sqlite_i64(value, "registry witness timestamp"))
                .transpose()?,
        ],
    )?;
    if pin_changed != 1 {
        return Err(ClientError::Core(pigeonpost_core::Error::StaleSequence));
    }
    let handle_changed = tx.execute(
        "INSERT INTO handle_resolutions
             (handle, address, pubkey, log_index, checkpoint_size, resolved_at)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6)
         ON CONFLICT(handle) DO UPDATE SET
             address = excluded.address,
             pubkey = excluded.pubkey,
             log_index = excluded.log_index,
             checkpoint_size = excluded.checkpoint_size,
             resolved_at = excluded.resolved_at
         WHERE (excluded.log_index > handle_resolutions.log_index
                AND excluded.checkpoint_size > handle_resolutions.checkpoint_size)
            OR (excluded.log_index = handle_resolutions.log_index
                AND excluded.checkpoint_size >= handle_resolutions.checkpoint_size
                AND excluded.address = handle_resolutions.address
                AND excluded.pubkey = handle_resolutions.pubkey)",
        params![
            handle,
            address.as_str(),
            verified.pubkey().as_slice(),
            sqlite_i64(verified.log_index(), "handle log index")?,
            sqlite_i64(verified.checkpoint().size, "handle checkpoint size")?,
            sqlite_i64(now, "handle resolution timestamp")?,
        ],
    )?;
    if handle_changed != 1 {
        return Err(ClientError::Core(pigeonpost_core::Error::StaleSequence));
    }
    Ok(address)
}

fn validate_outbox_reason(reason: &str) -> Result<()> {
    if reason.is_empty()
        || reason.len() > MAX_OUTBOX_REASON_BYTES
        || !reason
            .bytes()
            .all(|byte| byte.is_ascii_lowercase() || byte.is_ascii_digit() || byte == b'_')
    {
        return Err(ClientError::Config(
            "outbox failure reason must be a bounded lowercase code".into(),
        ));
    }
    Ok(())
}

fn validate_bounded_text(value: &str, max_bytes: usize, field: &str) -> Result<()> {
    if value.is_empty() || value.len() > max_bytes || value.chars().any(char::is_control) {
        return Err(ClientError::Config(format!(
            "{field} must be nonempty printable text of at most {max_bytes} bytes"
        )));
    }
    Ok(())
}

fn encode_outbox_payload(
    message_id: &str,
    to_addr: &str,
    loft_url: &str,
    wrap: &Wrap,
) -> Result<Vec<u8>> {
    validate_bounded_text(message_id, MAX_MESSAGE_ID_BYTES, "outbox message id")?;
    validate_bounded_text(
        to_addr,
        MAX_STORED_ADDRESS_BYTES,
        "outbox recipient address",
    )?;
    validate_bounded_text(loft_url, MAX_STORED_LOFT_URL_BYTES, "outbox loft URL")?;
    wrap.verify_public()?;
    let encoded = serde_json::to_vec(wrap)?;
    if encoded.is_empty() || encoded.len() > MAX_OUTBOX_WRAP_BYTES {
        return Err(ClientError::Core(pigeonpost_core::Error::TooLarge));
    }
    Ok(encoded)
}

fn escape_like_prefix(value: &str) -> String {
    let mut escaped = String::with_capacity(value.len().saturating_add(1));
    for character in value.chars() {
        if matches!(character, '%' | '_' | '\\') {
            escaped.push('\\');
        }
        escaped.push(character);
    }
    escaped.push('%');
    escaped
}

fn validate_storage_limits(limits: StorageLimits) -> Result<()> {
    if limits.inbox_messages == 0
        || limits.inbox_messages > MAX_INBOX_MESSAGE_LIMIT
        || limits.inbox_body_bytes == 0
        || limits.inbox_body_bytes > MAX_INBOX_BODY_BYTES_LIMIT
        || limits.outbox_rows == 0
        || limits.outbox_rows > MAX_OUTBOX_ROW_LIMIT
        || limits.outbox_payload_bytes == 0
        || limits.outbox_payload_bytes > MAX_OUTBOX_PAYLOAD_BYTES_LIMIT
    {
        return Err(ClientError::Config(
            "storage limits must be nonzero and within audited hard maxima".into(),
        ));
    }
    Ok(())
}

fn validate_loft_retention_days(retention_days: u64) -> Result<()> {
    if !(1..=pigeonpost_loft::MAX_RETENTION_DAYS).contains(&retention_days) {
        return Err(ClientError::Config(format!(
            "loft retention must be between 1 and {} days",
            pigeonpost_loft::MAX_RETENTION_DAYS
        )));
    }
    Ok(())
}

fn load_storage_status(conn: &Connection) -> Result<StorageStatus> {
    let values: (i64, i64, i64, i64, i64, i64, i64, i64, i64) = conn.query_row(
        "SELECT inbox_message_limit, inbox_body_bytes_limit,
                outbox_row_limit, outbox_payload_bytes_limit,
                inbox_messages, inbox_tombstones, inbox_body_bytes,
                outbox_rows, outbox_payload_bytes
         FROM storage_accounting WHERE id = 1",
        [],
        |row| {
            Ok((
                row.get(0)?,
                row.get(1)?,
                row.get(2)?,
                row.get(3)?,
                row.get(4)?,
                row.get(5)?,
                row.get(6)?,
                row.get(7)?,
                row.get(8)?,
            ))
        },
    )?;
    let limits = StorageLimits {
        inbox_messages: sqlite_u64(values.0, "inbox message limit")?,
        inbox_body_bytes: sqlite_u64(values.1, "inbox body-byte limit")?,
        outbox_rows: sqlite_u64(values.2, "outbox row limit")?,
        outbox_payload_bytes: sqlite_u64(values.3, "outbox payload-byte limit")?,
    };
    validate_storage_limits(limits)?;
    let usage = StorageUsage {
        inbox_messages: sqlite_u64(values.4, "inbox message usage")?,
        inbox_tombstones: sqlite_u64(values.5, "inbox tombstone usage")?,
        inbox_body_bytes: sqlite_u64(values.6, "inbox body-byte usage")?,
        outbox_rows: sqlite_u64(values.7, "outbox row usage")?,
        outbox_payload_bytes: sqlite_u64(values.8, "outbox payload-byte usage")?,
    };
    if usage.inbox_messages > limits.inbox_messages
        || usage.inbox_tombstones > MAX_INBOX_TOMBSTONES
        || usage.inbox_body_bytes > limits.inbox_body_bytes
        || usage.outbox_rows > limits.outbox_rows
        || usage.outbox_payload_bytes > limits.outbox_payload_bytes
    {
        return Err(ClientError::Config(
            "stored storage usage exceeds its configured limit".into(),
        ));
    }
    Ok(StorageStatus {
        limits,
        usage,
        inbox_tombstone_limit: MAX_INBOX_TOMBSTONES,
    })
}

fn checked_quota_add(
    current: u64,
    additional: u64,
    limit: u64,
    resource: StorageResource,
) -> Result<u64> {
    let Some(next) = current.checked_add(additional) else {
        return Err(ClientError::StorageLimit(resource));
    };
    if next > limit {
        return Err(ClientError::StorageLimit(resource));
    }
    Ok(next)
}

fn active_outbox_payload_bytes(conn: &Connection, row: i64) -> Result<Option<u64>> {
    if row <= 0 {
        return Err(ClientError::Config(
            "outbox record id must be positive".into(),
        ));
    }
    conn.query_row(
        "SELECT length(wrap) + CASE WHEN token IS NULL THEN 0
                                    ELSE length(CAST(token AS BLOB)) END
         FROM outbox WHERE row = ?1 AND sent_at IS NULL AND terminal_at IS NULL",
        params![row],
        |record| record.get::<_, i64>(0),
    )
    .optional()?
    .map(|bytes| sqlite_u64(bytes, "outbox payload size"))
    .transpose()
}

fn unsent_outbox_payload_bytes(conn: &Connection, row: i64) -> Result<Option<u64>> {
    if row <= 0 {
        return Err(ClientError::Config(
            "outbox record id must be positive".into(),
        ));
    }
    conn.query_row(
        "SELECT length(wrap) + CASE WHEN token IS NULL THEN 0
                                    ELSE length(CAST(token AS BLOB)) END
         FROM outbox WHERE row = ?1 AND sent_at IS NULL",
        params![row],
        |record| record.get::<_, i64>(0),
    )
    .optional()?
    .map(|bytes| sqlite_u64(bytes, "outbox payload size"))
    .transpose()
}

fn uncharge_outbox_payload(conn: &Connection, bytes: u64) -> Result<()> {
    let status = load_storage_status(conn)?;
    let next = status
        .usage
        .outbox_payload_bytes
        .checked_sub(bytes)
        .ok_or_else(|| ClientError::Config("outbox payload accounting drifted".into()))?;
    conn.execute(
        "UPDATE storage_accounting SET outbox_payload_bytes = ?1 WHERE id = 1",
        params![sqlite_i64(next, "outbox payload usage")?],
    )?;
    Ok(())
}

fn uncharge_outbox_rows(conn: &Connection, rows: u64) -> Result<()> {
    let status = load_storage_status(conn)?;
    let next = status
        .usage
        .outbox_rows
        .checked_sub(rows)
        .ok_or_else(|| ClientError::Config("outbox row accounting drifted".into()))?;
    conn.execute(
        "UPDATE storage_accounting SET outbox_rows = ?1 WHERE id = 1",
        params![sqlite_i64(next, "outbox row usage")?],
    )?;
    Ok(())
}

fn tombstone_inbox_message(conn: &Connection, body_bytes: u64) -> Result<()> {
    let status = load_storage_status(conn)?;
    let next_messages = status
        .usage
        .inbox_messages
        .checked_sub(1)
        .ok_or_else(|| ClientError::Config("inbox message accounting drifted".into()))?;
    let next_tombstones = status
        .usage
        .inbox_tombstones
        .checked_add(1)
        .filter(|next| *next <= MAX_INBOX_TOMBSTONES)
        .ok_or_else(|| ClientError::StorageLimit(StorageResource::InboxTombstones))?;
    let next_body_bytes = status
        .usage
        .inbox_body_bytes
        .checked_sub(body_bytes)
        .ok_or_else(|| ClientError::Config("inbox body accounting drifted".into()))?;
    conn.execute(
        "UPDATE storage_accounting
         SET inbox_messages = ?1, inbox_tombstones = ?2, inbox_body_bytes = ?3 WHERE id = 1",
        params![
            sqlite_i64(next_messages, "inbox message usage")?,
            sqlite_i64(next_tombstones, "inbox tombstone usage")?,
            sqlite_i64(next_body_bytes, "inbox body-byte usage")?
        ],
    )?;
    Ok(())
}

fn migrate(conn: &mut Connection) -> Result<()> {
    migrate_versioned(conn)
}

fn migrate_versioned(conn: &mut Connection) -> Result<()> {
    let version: i64 = conn.pragma_query_value(None, "user_version", |row| row.get(0))?;
    if !(0..=SCHEMA_VERSION).contains(&version) {
        return Err(ClientError::Config(format!(
            "state schema {version} is newer than supported {SCHEMA_VERSION}"
        )));
    }

    if version > 0 {
        validate_client_schema(conn, version)?;
        if version == SCHEMA_VERSION {
            validate_current_client_invariants(conn)?;
        } else {
            validate_client_invariants(conn, version)?;
        }
    }
    for target in (version + 1)..=SCHEMA_VERSION {
        let tx = conn.transaction_with_behavior(TransactionBehavior::Immediate)?;
        if target == 1 {
            migrate_client_to_v1(&tx)?;
        } else {
            validate_client_schema(&tx, target - 1)?;
            validate_client_invariants(&tx, target - 1)?;
            apply_client_migration(&tx, target)?;
        }
        validate_client_schema(&tx, target)?;
        validate_client_invariants(&tx, target)?;
        tx.pragma_update(None, "user_version", target)?;
        tx.commit()?;
    }
    Ok(())
}

/// Current-schema startup must remain O(1) in retained payload volume because an agent opens,
/// wakes, and exits. Exact table recounts and payload walks run once inside migration; ordinary
/// reads and writes validate every row they touch.
fn validate_current_client_invariants(conn: &Connection) -> Result<()> {
    validate_security_meta_values(conn)?;
    let accounting_rows: i64 = conn.query_row(
        "SELECT COUNT(*) FROM storage_accounting WHERE id = 1",
        [],
        |row| row.get(0),
    )?;
    if accounting_rows != 1 {
        return Err(ClientError::Config(
            "client storage accounting singleton is malformed".into(),
        ));
    }
    load_storage_status(conn)?;
    Ok(())
}

/// Adopt only the exact database shape emitted by 0.1.0, or initialize a genuinely empty file.
fn migrate_client_to_v1(tx: &Transaction<'_>) -> Result<()> {
    let actual = client_schema_snapshot(tx)?;
    if actual.is_empty() {
        tx.execute_batch(V0_1_0_SCHEMA)?;
    } else if actual != expected_client_schema(1)? {
        return Err(ClientError::Config(
            "unrecognized unversioned client state schema".into(),
        ));
    }
    Ok(())
}

fn apply_client_migration(tx: &Transaction<'_>, target: i64) -> Result<()> {
    match target {
        2 => migrate_client_to_v2(tx),
        3 => migrate_client_to_v3(tx),
        4 => migrate_client_to_v4(tx),
        5 => migrate_client_to_v5(tx),
        6 => migrate_client_to_v6(tx),
        7 => migrate_client_to_v7(tx),
        8 => migrate_client_to_v8(tx),
        9 => migrate_client_to_v9(tx),
        10 => migrate_client_to_v10(tx),
        11 => migrate_client_to_v11(tx),
        12 => migrate_client_to_v12(tx),
        13 => migrate_client_to_v13(tx),
        14 => migrate_client_to_v14(tx),
        15 => migrate_client_to_v15(tx),
        _ => unreachable!("migration target is bounded and version 1 is handled separately"),
    }
}

fn migrate_client_to_v2(tx: &Transaction<'_>) -> Result<()> {
    tx.execute_batch(
        "ALTER TABLE outbox ADD COLUMN token TEXT;
         ALTER TABLE outbox ADD COLUMN next_attempt_at INTEGER NOT NULL DEFAULT 0;
         CREATE INDEX outbox_ready ON outbox (sent_at, next_attempt_at, row);
         CREATE INDEX messages_inbox ON messages (state, read, received_at);
         CREATE INDEX messages_pending_sender ON messages (state, from_pubkey);",
    )?;
    Ok(())
}

fn migrate_client_to_v3(tx: &Transaction<'_>) -> Result<()> {
    tx.execute_batch(
        "CREATE TABLE directories (
             url TEXT PRIMARY KEY,
             signing_key BLOB NOT NULL,
             added_at INTEGER NOT NULL,
             enabled INTEGER NOT NULL DEFAULT 1,
             last_generated_at INTEGER NOT NULL DEFAULT 0,
             etag TEXT,
             snapshot BLOB
         );",
    )?;
    Ok(())
}

fn migrate_client_to_v4(tx: &Transaction<'_>) -> Result<()> {
    tx.execute_batch(
        "CREATE TABLE registry_config (
             id INTEGER PRIMARY KEY CHECK (id = 1),
             url TEXT NOT NULL,
             origin TEXT NOT NULL,
             checkpoint_key BLOB NOT NULL,
             witness_threshold INTEGER NOT NULL,
             minimum_size INTEGER NOT NULL,
             minimum_root BLOB NOT NULL,
             max_cosignature_age INTEGER NOT NULL,
             future_clock_skew INTEGER NOT NULL,
             added_at INTEGER NOT NULL
         );
         CREATE TABLE registry_witnesses (
             name TEXT PRIMARY KEY,
             pubkey BLOB NOT NULL UNIQUE
         );
         CREATE TABLE registry_pin (
             id INTEGER PRIMARY KEY CHECK (id = 1),
             size INTEGER NOT NULL,
             root BLOB NOT NULL,
             witnessed_at INTEGER
         );
         CREATE TABLE handle_resolutions (
             handle TEXT PRIMARY KEY,
             address TEXT NOT NULL,
             pubkey BLOB NOT NULL,
             log_index INTEGER NOT NULL,
             checkpoint_size INTEGER NOT NULL,
             resolved_at INTEGER NOT NULL
         );",
    )?;
    Ok(())
}

fn migrate_client_to_v5(tx: &Transaction<'_>) -> Result<()> {
    tx.execute_batch(
        "ALTER TABLE lofts ADD COLUMN role TEXT NOT NULL DEFAULT 'primary';
         ALTER TABLE cursors RENAME TO cursors_v4;
         CREATE TABLE cursors (
             loft_url TEXT NOT NULL,
             address TEXT NOT NULL,
             cursor INTEGER NOT NULL,
             PRIMARY KEY (loft_url, address)
         );
         INSERT INTO cursors (loft_url, address, cursor)
             SELECT loft_url, '', cursor FROM cursors_v4;
         DROP TABLE cursors_v4;
         CREATE TABLE rotation_chains (
             from_addr TEXT PRIMARY KEY,
             to_addr TEXT NOT NULL,
             record BLOB NOT NULL,
             fetched_at INTEGER NOT NULL
         );
         CREATE INDEX rotation_chains_target ON rotation_chains (to_addr);
         CREATE TABLE own_rotations (
             from_addr TEXT PRIMARY KEY,
             record BLOB NOT NULL,
             source_record BLOB NOT NULL,
             target_record BLOB NOT NULL,
             lofts TEXT NOT NULL,
             grace_until INTEGER NOT NULL
         );",
    )?;
    Ok(())
}

fn migrate_client_to_v6(tx: &Transaction<'_>) -> Result<()> {
    tx.execute_batch(
        "CREATE TABLE compliance_keys (
             key_id BLOB PRIMARY KEY CHECK (length(key_id) = 47),
             publication BLOB NOT NULL,
             public_key BLOB NOT NULL CHECK (length(public_key) = 32),
             log_index INTEGER NOT NULL,
             checkpoint_size INTEGER NOT NULL,
             witnessed_at INTEGER NOT NULL,
             fetched_at INTEGER NOT NULL
         );
         ALTER TABLE messages
             ADD COLUMN attribution TEXT NOT NULL DEFAULT 'absent';",
    )?;
    Ok(())
}

fn migrate_client_to_v7(tx: &Transaction<'_>) -> Result<()> {
    tx.execute_batch(
        "CREATE TABLE registry_audit (
             id INTEGER PRIMARY KEY CHECK (id = 1),
             state BLOB NOT NULL
         );",
    )?;
    Ok(())
}

fn migrate_client_to_v8(tx: &Transaction<'_>) -> Result<()> {
    tx.execute_batch(
        "ALTER TABLE lofts ADD COLUMN drain_after INTEGER;
         ALTER TABLE lofts ADD COLUMN allow_local
             INTEGER NOT NULL DEFAULT 0 CHECK (allow_local IN (0, 1));
         ALTER TABLE outbox ADD COLUMN allow_local
             INTEGER NOT NULL DEFAULT 0 CHECK (allow_local IN (0, 1));",
    )?;
    Ok(())
}

fn migrate_client_to_v9(tx: &Transaction<'_>) -> Result<()> {
    tx.execute_batch(
        "DROP INDEX outbox_ready;
         ALTER TABLE outbox ADD COLUMN terminal_at INTEGER;
         ALTER TABLE outbox ADD COLUMN terminal_reason TEXT;
         CREATE INDEX outbox_ready
             ON outbox (sent_at, terminal_at, next_attempt_at, row);
         CREATE INDEX outbox_terminal
             ON outbox (terminal_at, row)
             WHERE sent_at IS NULL AND terminal_at IS NOT NULL;",
    )?;
    Ok(())
}

fn migrate_client_to_v10(tx: &Transaction<'_>) -> Result<()> {
    tx.execute_batch(
        "CREATE TABLE registry_handle_audit (
             id INTEGER PRIMARY KEY CHECK (id = 1),
             state BLOB NOT NULL
         );
         CREATE TABLE registry_handle_projection (
             handle TEXT PRIMARY KEY,
             pubkey BLOB NOT NULL CHECK (length(pubkey) = 32),
             subject TEXT NOT NULL,
             log_index INTEGER NOT NULL CHECK (log_index >= 0)
         );",
    )?;
    Ok(())
}

fn migrate_client_to_v11(tx: &Transaction<'_>) -> Result<()> {
    tx.execute_batch(
        "CREATE TABLE spam_marks (
             message_id TEXT PRIMARY KEY,
             marked_at INTEGER NOT NULL CHECK (marked_at >= 0)
         );",
    )?;
    Ok(())
}

fn migrate_client_to_v12(tx: &Transaction<'_>) -> Result<()> {
    tx.execute_batch(
        "CREATE TABLE own_record_publication (
             id INTEGER PRIMARY KEY CHECK (id = 1),
             address TEXT NOT NULL,
             record BLOB NOT NULL CHECK (length(record) BETWEEN 1 AND 65536),
             updated_at INTEGER NOT NULL CHECK (updated_at >= 0)
         );
         CREATE TABLE own_record_publication_targets (
             url TEXT PRIMARY KEY,
             allow_local INTEGER NOT NULL CHECK (allow_local IN (0, 1)),
             rendezvous INTEGER NOT NULL CHECK (rendezvous IN (0, 1)),
             completed INTEGER NOT NULL CHECK (completed IN (0, 1))
         );
         CREATE INDEX own_record_publication_pending
             ON own_record_publication_targets (completed, rendezvous, url);
         CREATE TABLE own_rotation_publication_targets (
             from_addr TEXT NOT NULL,
             url TEXT NOT NULL,
             allow_local INTEGER NOT NULL CHECK (allow_local IN (0, 1)),
             rendezvous INTEGER NOT NULL CHECK (rendezvous IN (0, 1)),
             completed INTEGER NOT NULL CHECK (completed IN (0, 1)),
             PRIMARY KEY (from_addr, url)
         );
         CREATE INDEX own_rotation_publication_pending
             ON own_rotation_publication_targets (completed, from_addr, url);
         CREATE TABLE placement_health (
             id INTEGER PRIMARY KEY CHECK (id = 1),
             directory_refresh_degraded INTEGER NOT NULL
                 CHECK (directory_refresh_degraded IN (0, 1)),
             last_attempt_at INTEGER CHECK (last_attempt_at IS NULL OR last_attempt_at >= 0)
         );
         INSERT INTO placement_health
             (id, directory_refresh_degraded, last_attempt_at)
         VALUES (1, 0, NULL);",
    )?;
    Ok(())
}

fn migrate_client_to_v13(tx: &Transaction<'_>) -> Result<()> {
    // Reject data which could not have been written through the bounded v13 API. This check runs
    // before any payload is changed, and the surrounding migration transaction rolls everything
    // back on failure.
    let max_body_bytes = i64::try_from(MAX_INBOX_BODY_BYTES_PER_MESSAGE)
        .map_err(|_| ClientError::Config("message body ceiling exceeds SQLite".into()))?;
    let max_wrap_bytes = i64::try_from(MAX_OUTBOX_WRAP_BYTES)
        .map_err(|_| ClientError::Config("outbox wrap ceiling exceeds SQLite".into()))?;
    let invalid: bool = tx.query_row(
        "SELECT
             EXISTS(SELECT 1 FROM messages
                    WHERE length(CAST(id AS BLOB)) NOT BETWEEN 1 AND 128
                       OR length(CAST(from_address AS BLOB)) NOT BETWEEN 1 AND 256
                       OR length(CAST(body AS BLOB)) > ?1)
          OR EXISTS(SELECT 1 FROM outbox
                    WHERE length(CAST(message_id AS BLOB)) NOT BETWEEN 1 AND 128
                       OR length(CAST(to_addr AS BLOB)) NOT BETWEEN 1 AND 256
                       OR length(CAST(loft_url AS BLOB)) NOT BETWEEN 1 AND 2048
                       OR typeof(wrap) <> 'blob' OR length(wrap) > ?2
                       OR attempts > 4294967295
                       OR (sent_at IS NULL AND terminal_at IS NULL AND length(wrap) = 0)
                       OR (token IS NOT NULL
                           AND (length(token) <> 64 OR token GLOB '*[^0-9a-f]*'))
                       OR (last_error IS NOT NULL
                           AND (length(CAST(last_error AS BLOB)) NOT BETWEEN 1 AND 128
                                OR last_error GLOB '*[^a-z0-9_]*')))",
        params![max_body_bytes, max_wrap_bytes],
        |row| row.get(0),
    )?;
    if invalid {
        return Err(ClientError::Config(
            "schema v12 client payloads exceed v13 persistence bounds".into(),
        ));
    }

    // Successful and terminal copies retain bounded delivery metadata only. An empty BLOB is the
    // tombstone required by the historical NOT NULL column; it contains no serialized wrap.
    tx.execute(
        "UPDATE outbox SET wrap = X'', token = NULL
         WHERE sent_at IS NOT NULL OR terminal_at IS NOT NULL",
        [],
    )?;

    let (inbox_messages, inbox_body_bytes, outbox_rows, outbox_payload_bytes): (
        i64,
        i64,
        i64,
        i64,
    ) = tx.query_row(
        "SELECT
                 (SELECT COUNT(*) FROM messages),
                 (SELECT COALESCE(SUM(length(CAST(body AS BLOB))), 0) FROM messages),
                 (SELECT COUNT(*) FROM outbox),
                 (SELECT COALESCE(SUM(length(wrap) +
                                      CASE WHEN token IS NULL THEN 0
                                           ELSE length(CAST(token AS BLOB)) END), 0)
                    FROM outbox)",
        [],
        |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?, row.get(3)?)),
    )?;
    if inbox_messages > 1_000_000
        || inbox_body_bytes > 68_719_476_736
        || outbox_rows > 1_000_000
        || outbox_payload_bytes > 68_719_476_736
    {
        return Err(ClientError::Config(
            "schema v12 client payloads exceed v13 hard storage maxima".into(),
        ));
    }

    tx.execute_batch(
        "CREATE TABLE storage_accounting (
             id INTEGER PRIMARY KEY CHECK (id = 1),
             inbox_message_limit INTEGER NOT NULL
                 CHECK (inbox_message_limit BETWEEN 1 AND 1000000),
             inbox_body_bytes_limit INTEGER NOT NULL
                 CHECK (inbox_body_bytes_limit BETWEEN 1 AND 68719476736),
             outbox_row_limit INTEGER NOT NULL
                 CHECK (outbox_row_limit BETWEEN 1 AND 1000000),
             outbox_payload_bytes_limit INTEGER NOT NULL
                 CHECK (outbox_payload_bytes_limit BETWEEN 1 AND 68719476736),
             inbox_messages INTEGER NOT NULL
                 CHECK (inbox_messages BETWEEN 0 AND inbox_message_limit),
             inbox_body_bytes INTEGER NOT NULL
                 CHECK (inbox_body_bytes BETWEEN 0 AND inbox_body_bytes_limit),
             outbox_rows INTEGER NOT NULL
                 CHECK (outbox_rows BETWEEN 0 AND outbox_row_limit),
             outbox_payload_bytes INTEGER NOT NULL
                 CHECK (outbox_payload_bytes BETWEEN 0 AND outbox_payload_bytes_limit)
         );",
    )?;
    tx.execute(
        "INSERT INTO storage_accounting
             (id, inbox_message_limit, inbox_body_bytes_limit,
              outbox_row_limit, outbox_payload_bytes_limit,
              inbox_messages, inbox_body_bytes, outbox_rows, outbox_payload_bytes)
         VALUES (1, MAX(10000, ?1), MAX(536870912, ?2),
                 MAX(10000, ?3), MAX(2147483648, ?4), ?1, ?2, ?3, ?4)",
        params![
            inbox_messages,
            inbox_body_bytes,
            outbox_rows,
            outbox_payload_bytes
        ],
    )?;
    Ok(())
}

fn migrate_client_to_v14(tx: &Transaction<'_>) -> Result<()> {
    // Version 13 trusted this projection only through later whole-table checks. Validate every
    // legacy row once before installing the indexed hot path; a malformed cache must roll the
    // entire migration back rather than become a reputation signal.
    let audit_origin: Option<String> = tx
        .query_row(
            "SELECT origin FROM registry_config WHERE id = 1",
            [],
            |row| row.get(0),
        )
        .optional()?;
    let audit_size = tx
        .query_row(
            "SELECT state FROM registry_handle_audit WHERE id = 1",
            [],
            |row| row.get::<_, Vec<u8>>(0),
        )
        .optional()?
        .map(|bytes| {
            if bytes.is_empty() || bytes.len() > MAX_HANDLE_AUDIT_BYTES {
                return Err(ClientError::Config(
                    "schema v13 handle audit exceeds its persistence bound".into(),
                ));
            }
            let audit: HandleAuditState = serde_json::from_slice(&bytes)?;
            let origin = audit_origin.as_deref().ok_or_else(|| {
                ClientError::Config("schema v13 handle audit exists without registry trust".into())
            })?;
            audit.validate(origin)?;
            Ok(audit.size())
        })
        .transpose()?;
    let mut projection = tx.prepare(
        "SELECT handle, pubkey, subject, log_index
         FROM registry_handle_projection ORDER BY handle",
    )?;
    let rows = projection.query_map([], |row| {
        Ok((
            row.get::<_, String>(0)?,
            row.get::<_, Vec<u8>>(1)?,
            row.get::<_, String>(2)?,
            row.get::<_, i64>(3)?,
        ))
    })?;
    for row in rows {
        let (handle, pubkey, subject, log_index) = row?;
        let parsed = Handle::parse(&handle)
            .map_err(|_| ClientError::Config("schema v13 handle projection is malformed".into()))?;
        if parsed.as_path() != handle
            || pubkey.len() != 32
            || subject.is_empty()
            || subject.len() > MAX_HANDLE_SUBJECT_BYTES
            || log_index < 0
            || audit_size.is_none_or(|size| u64::try_from(log_index).map_or(true, |i| i >= size))
        {
            return Err(ClientError::Config(
                "schema v13 handle projection is malformed".into(),
            ));
        }
    }
    drop(projection);

    tx.execute_batch(
        "ALTER TABLE messages ADD COLUMN deleted_at INTEGER
             CHECK (deleted_at IS NULL OR deleted_at >= 0);
         ALTER TABLE storage_accounting ADD COLUMN inbox_tombstones INTEGER NOT NULL DEFAULT 0
             CHECK (inbox_tombstones BETWEEN 0 AND 1000000);
         ALTER TABLE lofts ADD COLUMN retention_days INTEGER NOT NULL DEFAULT 30
             CHECK (retention_days BETWEEN 1 AND 3650);
         CREATE INDEX registry_handle_projection_pubkey
             ON registry_handle_projection (pubkey);",
    )?;
    Ok(())
}

fn migrate_client_to_v15(tx: &Transaction<'_>) -> Result<()> {
    tx.execute_batch(
        "ALTER TABLE resolutions ADD COLUMN attribution_requirement BLOB
             CHECK (attribution_requirement IS NULL OR
                    (typeof(attribution_requirement) = 'blob' AND
                     length(attribution_requirement) = 34));",
    )?;
    Ok(())
}

type ClientSchemaObject = (String, String, String, String);

fn validate_client_schema(conn: &Connection, version: i64) -> Result<()> {
    if client_schema_snapshot(conn)? != expected_client_schema(version)? {
        return Err(ClientError::Config(format!(
            "client state schema does not match declared version {version}"
        )));
    }
    Ok(())
}

fn expected_client_schema(version: i64) -> Result<Vec<ClientSchemaObject>> {
    debug_assert!((1..=SCHEMA_VERSION).contains(&version));
    let mut reference = Connection::open_in_memory()?;
    reference.execute_batch(V0_1_0_SCHEMA)?;
    for target in 2..=version {
        let tx = reference.transaction_with_behavior(TransactionBehavior::Immediate)?;
        apply_client_migration(&tx, target)?;
        tx.commit()?;
    }
    client_schema_snapshot(&reference)
}

fn client_schema_snapshot(conn: &Connection) -> Result<Vec<ClientSchemaObject>> {
    let mut statement = conn.prepare(
        "SELECT type, name, tbl_name, sql
         FROM sqlite_schema
         WHERE name NOT LIKE 'sqlite_%' AND sql IS NOT NULL
         ORDER BY type, name",
    )?;
    let rows = statement.query_map([], |row| {
        Ok((
            row.get::<_, String>(0)?,
            row.get::<_, String>(1)?,
            row.get::<_, String>(2)?,
            canonical_client_schema_sql(&row.get::<_, String>(3)?),
        ))
    })?;
    rows.collect::<std::result::Result<Vec<_>, _>>()
        .map_err(Into::into)
}

/// SQLite preserves much of the original DDL spelling. Compare exact definitions while
/// tolerating whitespace/comments and the column-order differences produced by ALTER TABLE.
fn canonical_client_schema_sql(sql: &str) -> String {
    let uncommented = sql
        .lines()
        .map(|line| line.split_once("--").map_or(line, |(before, _)| before))
        .collect::<String>();
    let normalized = normalize_client_sql(&uncommented);
    if !normalized.starts_with("createtable") {
        return normalized;
    }
    let (Some(open), Some(close)) = (uncommented.find('('), uncommented.rfind(')')) else {
        return normalized;
    };
    if close <= open {
        return normalized;
    }
    let mut definitions = split_client_sql_definitions(&uncommented[(open + 1)..close]);
    definitions.sort();
    format!(
        "{}({})",
        normalize_client_sql(&uncommented[..open]),
        definitions.join(",")
    )
}

fn split_client_sql_definitions(body: &str) -> Vec<String> {
    let mut definitions = Vec::new();
    let mut start = 0;
    let mut depth = 0usize;
    let mut quote = None;
    for (offset, character) in body.char_indices() {
        if let Some(closing) = quote {
            if character == closing {
                quote = None;
            }
            continue;
        }
        match character {
            '\'' | '"' => quote = Some(character),
            '[' => quote = Some(']'),
            '(' => depth += 1,
            ')' => depth = depth.saturating_sub(1),
            ',' if depth == 0 => {
                definitions.push(normalize_client_sql(&body[start..offset]));
                start = offset + character.len_utf8();
            }
            _ => {}
        }
    }
    definitions.push(normalize_client_sql(&body[start..]));
    definitions
}

fn normalize_client_sql(sql: &str) -> String {
    let mut normalized = String::with_capacity(sql.len());
    let mut quote = None;
    for character in sql.chars() {
        if let Some(closing) = quote {
            normalized.push(character);
            if character == closing {
                quote = None;
            }
            continue;
        }
        match character {
            '\'' | '"' => {
                quote = Some(character);
                normalized.push(character);
            }
            '[' => {
                quote = Some(']');
                normalized.push(character);
            }
            _ if character.is_whitespace() => {}
            _ => normalized.push(character.to_ascii_lowercase()),
        }
    }
    normalized
}

fn validate_client_invariants(conn: &Connection, version: i64) -> Result<()> {
    validate_security_meta_values(conn)?;
    let invalid_base: bool = conn.query_row(
        "SELECT
             EXISTS(SELECT 1 FROM lofts
                    WHERE typeof(added_at) <> 'integer' OR added_at < 0
                       OR state NOT IN ('active', 'draining')
                       OR (pubkey IS NOT NULL
                           AND (typeof(pubkey) <> 'blob' OR length(pubkey) <> 32)))
          OR EXISTS(SELECT 1 FROM outbox
                    WHERE typeof(row) <> 'integer' OR row <= 0
                       OR typeof(attempts) <> 'integer' OR attempts < 0
                       OR typeof(created_at) <> 'integer' OR created_at < 0
                       OR (sent_at IS NOT NULL
                           AND (typeof(sent_at) <> 'integer' OR sent_at < 0)))
          OR EXISTS(SELECT 1 FROM messages
                    WHERE ((state = 'deleted' AND length(from_pubkey) <> 0)
                           OR (state <> 'deleted' AND length(from_pubkey) <> 32))
                       OR typeof(received_at) <> 'integer' OR received_at < 0
                       OR read NOT IN (0, 1)
                       OR state NOT IN ('accepted', 'pending', 'deleted')
                       OR (?1 < 14 AND state = 'deleted'))
          OR EXISTS(SELECT 1 FROM resolutions
                    WHERE length(pubkey) <> 32 OR length(successor_hash) <> 32
                       OR typeof(seq) <> 'integer' OR seq < 0
                       OR typeof(fetched_at) <> 'integer' OR fetched_at < 0
                       OR typeof(pow_min) <> 'integer' OR pow_min < 0)",
        params![version],
        |row| row.get(0),
    )?;
    if invalid_base {
        return Err(ClientError::Config(
            "client state contains malformed persisted rows".into(),
        ));
    }

    if version >= 15 {
        let invalid_requirement: bool = conn.query_row(
            "SELECT EXISTS(SELECT 1 FROM resolutions
                           WHERE attribution_requirement IS NOT NULL
                             AND (typeof(attribution_requirement) <> 'blob'
                                  OR length(attribution_requirement) <> ?1))",
            params![ATTRIBUTION_REQUIREMENT_LEN as i64],
            |row| row.get(0),
        )?;
        if invalid_requirement {
            return Err(ClientError::Config(
                "client state contains a malformed resolution attribution requirement".into(),
            ));
        }
        let mut statement = conn.prepare(
            "SELECT attribution_requirement FROM resolutions
             WHERE attribution_requirement IS NOT NULL",
        )?;
        let encoded = statement.query_map([], |row| row.get::<_, Vec<u8>>(0))?;
        for encoded in encoded {
            if AttributionRequirement::decode(&encoded?).is_err() {
                return Err(ClientError::Config(
                    "client state contains a noncanonical resolution attribution requirement"
                        .into(),
                ));
            }
        }
    }

    if version >= 2 {
        let invalid: bool = conn.query_row(
            "SELECT EXISTS(SELECT 1 FROM outbox
                           WHERE typeof(next_attempt_at) <> 'integer'
                              OR next_attempt_at < 0)",
            [],
            |row| row.get(0),
        )?;
        if invalid {
            return Err(ClientError::Config(
                "client outbox retry state is malformed".into(),
            ));
        }
    }
    if version >= 3 {
        let invalid: bool = conn.query_row(
            "SELECT EXISTS(SELECT 1 FROM directories
                           WHERE length(signing_key) <> 32
                              OR typeof(added_at) <> 'integer' OR added_at < 0
                              OR enabled NOT IN (0, 1)
                              OR typeof(last_generated_at) <> 'integer'
                              OR last_generated_at < 0)",
            [],
            |row| row.get(0),
        )?;
        if invalid {
            return Err(ClientError::Config(
                "client directory cache is malformed".into(),
            ));
        }
    }
    if version >= 4 {
        let invalid: bool = conn.query_row(
            "SELECT
                 EXISTS(SELECT 1 FROM registry_config
                        WHERE id <> 1 OR length(checkpoint_key) <> 32
                           OR length(minimum_root) <> 32
                           OR witness_threshold < 0 OR minimum_size < 0
                           OR max_cosignature_age < 0 OR future_clock_skew < 0
                           OR added_at < 0)
              OR EXISTS(SELECT 1 FROM registry_witnesses
                        WHERE length(pubkey) <> 32)
              OR EXISTS(SELECT 1 FROM registry_pin
                        WHERE id <> 1 OR size < 0 OR length(root) <> 32
                           OR (witnessed_at IS NOT NULL AND witnessed_at < 0))
              OR EXISTS(SELECT 1 FROM handle_resolutions
                        WHERE length(pubkey) <> 32 OR log_index < 0
                           OR checkpoint_size <= log_index OR resolved_at < 0)",
            [],
            |row| row.get(0),
        )?;
        if invalid {
            return Err(ClientError::Config(
                "client registry trust cache is malformed".into(),
            ));
        }
    }
    if version >= 5 {
        let invalid: bool = conn.query_row(
            "SELECT
                 EXISTS(SELECT 1 FROM cursors
                        WHERE typeof(cursor) <> 'integer' OR cursor < 0)
              OR EXISTS(SELECT 1 FROM rotation_chains WHERE fetched_at < 0)
              OR EXISTS(SELECT 1 FROM own_rotations WHERE grace_until < 0)",
            [],
            |row| row.get(0),
        )?;
        if invalid {
            return Err(ClientError::Config(
                "client rotation state is malformed".into(),
            ));
        }
    }
    if version >= 6 {
        let invalid: bool = conn.query_row(
            "SELECT
                 EXISTS(SELECT 1 FROM messages
                        WHERE attribution NOT IN ('absent', 'valid', 'invalid'))
              OR EXISTS(SELECT 1 FROM compliance_keys
                        WHERE length(key_id) <> 47 OR length(public_key) <> 32
                           OR log_index < 0 OR checkpoint_size < 0
                           OR witnessed_at < 0 OR fetched_at < 0)",
            [],
            |row| row.get(0),
        )?;
        if invalid {
            return Err(ClientError::Config(
                "client attribution cache is malformed".into(),
            ));
        }
    }
    if version >= 8 {
        let invalid: bool = conn.query_row(
            "SELECT
                 EXISTS(SELECT 1 FROM lofts
                        WHERE allow_local NOT IN (0, 1)
                           OR (state = 'active' AND drain_after IS NOT NULL)
                           OR (state = 'draining' AND drain_after IS NULL))
              OR EXISTS(SELECT 1 FROM outbox WHERE allow_local NOT IN (0, 1))",
            [],
            |row| row.get(0),
        )?;
        if invalid {
            return Err(ClientError::Config(
                "client loft routing state is malformed".into(),
            ));
        }
    }
    if version >= 9 {
        let invalid: bool = conn.query_row(
            "SELECT EXISTS(SELECT 1 FROM outbox
                           WHERE (terminal_at IS NULL) <> (terminal_reason IS NULL)
                              OR (sent_at IS NOT NULL AND terminal_at IS NOT NULL)
                              OR (terminal_at IS NOT NULL
                                  AND (typeof(terminal_at) <> 'integer'
                                       OR terminal_at < 0
                                       OR length(terminal_reason) = 0
                                       OR length(CAST(terminal_reason AS BLOB)) > 128)))",
            [],
            |row| row.get(0),
        )?;
        if invalid {
            return Err(ClientError::Config(
                "client outbox terminal state is malformed".into(),
            ));
        }
    }
    if version >= 10 {
        let invalid: bool = conn.query_row(
            "SELECT
                 EXISTS(SELECT 1 FROM registry_handle_audit
                        WHERE id <> 1 OR length(state) = 0 OR length(state) > 262144)
              OR EXISTS(SELECT 1 FROM registry_handle_projection
                        WHERE length(handle) = 0 OR length(pubkey) <> 32
                           OR length(subject) = 0 OR log_index < 0)
              OR (EXISTS(SELECT 1 FROM registry_handle_projection)
                  AND NOT EXISTS(SELECT 1 FROM registry_handle_audit WHERE id = 1))",
            [],
            |row| row.get(0),
        )?;
        if invalid {
            return Err(ClientError::Config(
                "client handle-audit projection is malformed".into(),
            ));
        }
    }
    if version >= 11 {
        let invalid: bool = conn.query_row(
            "SELECT EXISTS(SELECT 1 FROM spam_marks AS mark
                           LEFT JOIN messages AS message ON message.id = mark.message_id
                           WHERE message.id IS NULL
                              OR typeof(mark.marked_at) <> 'integer'
                              OR mark.marked_at < 0)",
            [],
            |row| row.get(0),
        )?;
        if invalid {
            return Err(ClientError::Config(
                "client spam-mark state is malformed".into(),
            ));
        }
    }
    if version >= 12 {
        let invalid: bool = conn.query_row(
            "SELECT
                 (SELECT COUNT(*) FROM placement_health) <> 1
              OR EXISTS(SELECT 1 FROM placement_health
                        WHERE id <> 1
                           OR directory_refresh_degraded NOT IN (0, 1)
                           OR (last_attempt_at IS NOT NULL
                               AND (typeof(last_attempt_at) <> 'integer'
                                    OR last_attempt_at < 0)))
              OR EXISTS(SELECT 1 FROM own_record_publication
                        WHERE id <> 1 OR length(address) = 0
                           OR length(record) = 0 OR length(record) > 65536
                           OR typeof(updated_at) <> 'integer' OR updated_at < 0)
              OR (EXISTS(SELECT 1 FROM own_record_publication_targets)
                  AND NOT EXISTS(SELECT 1 FROM own_record_publication WHERE id = 1))
              OR EXISTS(SELECT 1 FROM own_record_publication_targets
                        WHERE length(url) = 0 OR length(CAST(url AS BLOB)) > 2048
                           OR allow_local NOT IN (0, 1)
                           OR rendezvous NOT IN (0, 1)
                           OR completed NOT IN (0, 1))
              OR (SELECT COUNT(*) FROM own_record_publication_targets) > 64
              OR EXISTS(SELECT 1 FROM own_rotation_publication_targets AS target
                        LEFT JOIN own_rotations AS rotation
                          ON rotation.from_addr = target.from_addr
                        WHERE rotation.from_addr IS NULL
                           OR length(target.url) = 0
                           OR length(CAST(target.url AS BLOB)) > 2048
                           OR target.allow_local NOT IN (0, 1)
                           OR target.rendezvous NOT IN (0, 1)
                           OR target.completed NOT IN (0, 1))
              OR EXISTS(
                    SELECT from_addr FROM own_rotation_publication_targets
                    GROUP BY from_addr HAVING COUNT(*) > 64
                 )",
            [],
            |row| row.get(0),
        )?;
        if invalid {
            return Err(ClientError::Config(
                "client publication journal is malformed".into(),
            ));
        }
    }
    if version >= 13 {
        let max_body_bytes = i64::try_from(MAX_INBOX_BODY_BYTES_PER_MESSAGE)
            .map_err(|_| ClientError::Config("message body ceiling exceeds SQLite".into()))?;
        let max_wrap_bytes = i64::try_from(MAX_OUTBOX_WRAP_BYTES)
            .map_err(|_| ClientError::Config("outbox wrap ceiling exceeds SQLite".into()))?;
        let invalid: bool = conn.query_row(
            "SELECT
                 (SELECT COUNT(*) FROM storage_accounting) <> 1
              OR EXISTS(SELECT 1 FROM storage_accounting
                        WHERE id <> 1
                           OR typeof(inbox_message_limit) <> 'integer'
                           OR inbox_message_limit NOT BETWEEN 1 AND 1000000
                           OR typeof(inbox_body_bytes_limit) <> 'integer'
                           OR inbox_body_bytes_limit NOT BETWEEN 1 AND 68719476736
                           OR typeof(outbox_row_limit) <> 'integer'
                           OR outbox_row_limit NOT BETWEEN 1 AND 1000000
                           OR typeof(outbox_payload_bytes_limit) <> 'integer'
                           OR outbox_payload_bytes_limit NOT BETWEEN 1 AND 68719476736
                           OR typeof(inbox_messages) <> 'integer'
                           OR inbox_messages NOT BETWEEN 0 AND inbox_message_limit
                           OR typeof(inbox_body_bytes) <> 'integer'
                           OR inbox_body_bytes NOT BETWEEN 0 AND inbox_body_bytes_limit
                           OR typeof(outbox_rows) <> 'integer'
                           OR outbox_rows NOT BETWEEN 0 AND outbox_row_limit
                           OR typeof(outbox_payload_bytes) <> 'integer'
                           OR outbox_payload_bytes NOT BETWEEN 0 AND outbox_payload_bytes_limit)
              OR EXISTS(SELECT 1 FROM messages
                        WHERE length(CAST(id AS BLOB)) NOT BETWEEN 1 AND 128
                           OR (state <> 'deleted' AND
                               length(CAST(from_address AS BLOB)) NOT BETWEEN 1 AND 256)
                           OR (state = 'deleted' AND length(CAST(from_address AS BLOB)) <> 0)
                           OR length(CAST(body AS BLOB)) > ?1)
              OR EXISTS(SELECT 1 FROM outbox
                        WHERE length(CAST(message_id AS BLOB)) NOT BETWEEN 1 AND 128
                           OR length(CAST(to_addr AS BLOB)) NOT BETWEEN 1 AND 256
                           OR length(CAST(loft_url AS BLOB)) NOT BETWEEN 1 AND 2048
                           OR typeof(wrap) <> 'blob' OR length(wrap) > ?2
                           OR attempts > 4294967295
                           OR ((sent_at IS NULL AND terminal_at IS NULL)
                               <> (length(wrap) > 0))
                           OR ((sent_at IS NOT NULL OR terminal_at IS NOT NULL)
                               AND (length(wrap) <> 0 OR token IS NOT NULL))
                           OR (token IS NOT NULL
                               AND (length(token) <> 64 OR token GLOB '*[^0-9a-f]*'))
                           OR (last_error IS NOT NULL
                               AND (length(CAST(last_error AS BLOB)) NOT BETWEEN 1 AND 128
                                    OR last_error GLOB '*[^a-z0-9_]*')))
              OR (SELECT inbox_messages FROM storage_accounting WHERE id = 1)
                    <> (SELECT COUNT(*) FROM messages WHERE state <> 'deleted')
              OR (SELECT inbox_body_bytes FROM storage_accounting WHERE id = 1)
                    <> (SELECT COALESCE(SUM(length(CAST(body AS BLOB))), 0) FROM messages)
              OR (SELECT outbox_rows FROM storage_accounting WHERE id = 1)
                    <> (SELECT COUNT(*) FROM outbox)
              OR (SELECT outbox_payload_bytes FROM storage_accounting WHERE id = 1)
                    <> (SELECT COALESCE(SUM(length(wrap) +
                                           CASE WHEN token IS NULL THEN 0
                                                ELSE length(CAST(token AS BLOB)) END), 0)
                          FROM outbox)",
            params![max_body_bytes, max_wrap_bytes],
            |row| row.get(0),
        )?;
        if invalid {
            return Err(ClientError::Config(
                "client storage accounting or payload state is malformed".into(),
            ));
        }
        validate_stored_payload_text(conn)?;

        let mut statement = conn.prepare(
            "SELECT wrap, token FROM outbox
             WHERE sent_at IS NULL AND terminal_at IS NULL ORDER BY row",
        )?;
        let rows = statement.query_map([], |row| {
            Ok((row.get::<_, Vec<u8>>(0)?, row.get::<_, Option<String>>(1)?))
        })?;
        for row in rows {
            let (encoded_wrap, token) = row?;
            let _: Wrap = serde_json::from_slice(&encoded_wrap).map_err(|_| {
                ClientError::Config("client outbox contains a malformed serialized wrap".into())
            })?;
            if token
                .as_deref()
                .is_some_and(|value| Token::from_hex(value).is_none())
            {
                return Err(ClientError::Config(
                    "client outbox contains a malformed capability token".into(),
                ));
            }
        }
    }
    if version >= 14 {
        let invalid: bool = conn.query_row(
            "SELECT
                 EXISTS(SELECT 1 FROM storage_accounting
                        WHERE typeof(inbox_tombstones) <> 'integer'
                           OR inbox_tombstones NOT BETWEEN 0 AND 1000000)
              OR EXISTS(SELECT 1 FROM lofts
                        WHERE typeof(retention_days) <> 'integer'
                           OR retention_days NOT BETWEEN 1 AND 3650)
              OR EXISTS(SELECT 1 FROM messages
                        WHERE (state = 'deleted' AND (
                                  typeof(from_pubkey) <> 'blob' OR length(from_pubkey) <> 0
                                  OR from_address <> '' OR received_at <> 0 OR read <> 0
                                  OR body <> '' OR attribution <> 'absent'
                                  OR typeof(deleted_at) <> 'integer' OR deleted_at < 0
                              ))
                           OR (state <> 'deleted' AND deleted_at IS NOT NULL))
              OR EXISTS(SELECT 1 FROM spam_marks AS mark
                        JOIN messages AS message ON message.id = mark.message_id
                        WHERE message.state = 'deleted')
              OR (SELECT inbox_tombstones FROM storage_accounting WHERE id = 1)
                    <> (SELECT COUNT(*) FROM messages WHERE state = 'deleted')",
            [],
            |row| row.get(0),
        )?;
        if invalid {
            return Err(ClientError::Config(
                "client tombstone or loft-retention state is malformed".into(),
            ));
        }
    }
    Ok(())
}

fn validate_stored_payload_text(conn: &Connection) -> Result<()> {
    let mut messages = conn.prepare("SELECT id, from_address, state FROM messages ORDER BY id")?;
    let rows = messages.query_map([], |row| {
        Ok((
            row.get::<_, String>(0)?,
            row.get::<_, String>(1)?,
            row.get::<_, String>(2)?,
        ))
    })?;
    for row in rows {
        let (id, from_address, state) = row?;
        validate_bounded_text(&id, MAX_MESSAGE_ID_BYTES, "stored message id")?;
        if state != "deleted" {
            validate_bounded_text(
                &from_address,
                MAX_STORED_ADDRESS_BYTES,
                "stored sender address",
            )?;
        }
    }
    drop(messages);

    let mut outbox =
        conn.prepare("SELECT message_id, to_addr, loft_url FROM outbox ORDER BY row")?;
    let rows = outbox.query_map([], |row| {
        Ok((
            row.get::<_, String>(0)?,
            row.get::<_, String>(1)?,
            row.get::<_, String>(2)?,
        ))
    })?;
    for row in rows {
        let (message_id, to_addr, loft_url) = row?;
        validate_bounded_text(
            &message_id,
            MAX_MESSAGE_ID_BYTES,
            "stored outbox message id",
        )?;
        validate_bounded_text(
            &to_addr,
            MAX_STORED_ADDRESS_BYTES,
            "stored outbox recipient address",
        )?;
        validate_bounded_text(
            &loft_url,
            MAX_STORED_LOFT_URL_BYTES,
            "stored outbox loft URL",
        )?;
    }
    Ok(())
}

fn validate_security_meta_values(conn: &Connection) -> Result<()> {
    let get = |key: &str| -> Result<Option<String>> {
        Ok(conn
            .query_row(
                "SELECT value FROM meta WHERE key = ?1",
                params![key],
                |row| row.get(0),
            )
            .optional()?)
    };
    parse_bool_meta(ACCEPT_ALL_META, get(ACCEPT_ALL_META)?.as_deref())?;
    parse_pow_floor_meta(get(POW_FLOOR_META)?.as_deref())?;
    parse_token_labels_meta(get(TOKEN_LABELS_META)?.as_deref())?;
    parse_bool_meta(
        TOKEN_GATE_ENABLED_META,
        get(TOKEN_GATE_ENABLED_META)?.as_deref(),
    )?;
    let legacy_required = parse_bool_meta(
        ATTRIBUTION_REQUIRED_META,
        get(ATTRIBUTION_REQUIRED_META)?.as_deref(),
    )?;
    let sender = parse_attribution_requirement_meta(
        SENDER_ATTRIBUTION_REQUIREMENT_META,
        get(SENDER_ATTRIBUTION_REQUIREMENT_META)?.as_deref(),
    )?;
    let recipient = parse_attribution_requirement_meta(
        RECIPIENT_ATTRIBUTION_REQUIREMENT_META,
        get(RECIPIENT_ATTRIBUTION_REQUIREMENT_META)?.as_deref(),
    )?;
    let legacy_jurisdiction = get(ATTRIBUTION_JURISDICTION_META)?
        .map(|value| {
            let parsed: Jurisdiction = serde_json::from_str(&value)
                .map_err(|_| invalid_security_meta(ATTRIBUTION_JURISDICTION_META))?;
            if serde_json::to_string(&parsed).map_err(ClientError::from)? != value {
                return Err(invalid_security_meta(ATTRIBUTION_JURISDICTION_META));
            }
            Ok(parsed)
        })
        .transpose()?;
    if sender.is_some() && legacy_jurisdiction.is_some() {
        return Err(invalid_security_meta(SENDER_ATTRIBUTION_REQUIREMENT_META));
    }
    if recipient.is_some() && legacy_required != Some(true) {
        return Err(invalid_security_meta(
            RECIPIENT_ATTRIBUTION_REQUIREMENT_META,
        ));
    }
    Ok(())
}

fn parse_attribution_requirement_meta(
    key: &str,
    value: Option<&str>,
) -> Result<Option<AttributionRequirement>> {
    value
        .map(|value| {
            if value.len() != ATTRIBUTION_REQUIREMENT_LEN * 2
                || !value
                    .bytes()
                    .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
            {
                return Err(invalid_security_meta(key));
            }
            let mut encoded = [0u8; ATTRIBUTION_REQUIREMENT_LEN];
            for (index, pair) in value.as_bytes().chunks_exact(2).enumerate() {
                encoded[index] = (hex_nibble(pair[0]).ok_or_else(|| invalid_security_meta(key))?
                    << 4)
                    | hex_nibble(pair[1]).ok_or_else(|| invalid_security_meta(key))?;
            }
            AttributionRequirement::decode(&encoded)
                .map(Some)
                .map_err(|_| invalid_security_meta(key))
        })
        .transpose()
        .map(Option::flatten)
}

fn hex_nibble(byte: u8) -> Option<u8> {
    match byte {
        b'0'..=b'9' => Some(byte - b'0'),
        b'a'..=b'f' => Some(byte - b'a' + 10),
        _ => None,
    }
}

fn canonical_bool(value: bool) -> &'static str {
    if value {
        "true"
    } else {
        "false"
    }
}

fn parse_bool_meta(key: &str, value: Option<&str>) -> Result<Option<bool>> {
    value
        .map(|value| match value {
            "true" => Ok(true),
            "false" => Ok(false),
            _ => Err(invalid_security_meta(key)),
        })
        .transpose()
}

fn parse_pow_floor_meta(value: Option<&str>) -> Result<Option<u32>> {
    value
        .map(|value| {
            let parsed = value
                .parse::<u32>()
                .map_err(|_| invalid_security_meta(POW_FLOOR_META))?;
            if parsed > MAX_POW_FLOOR || parsed.to_string() != value {
                return Err(invalid_security_meta(POW_FLOOR_META));
            }
            Ok(parsed)
        })
        .transpose()
}

fn parse_token_labels_meta(value: Option<&str>) -> Result<Option<Vec<String>>> {
    value
        .map(|value| {
            if value.len() > MAX_TOKEN_LABELS_META_BYTES {
                return Err(invalid_security_meta(TOKEN_LABELS_META));
            }
            let labels: Vec<String> = serde_json::from_str(value)
                .map_err(|_| invalid_security_meta(TOKEN_LABELS_META))?;
            validate_token_labels(&labels)?;
            Ok(labels)
        })
        .transpose()
}

fn validate_token_labels(labels: &[String]) -> Result<()> {
    if labels.len() > pigeonpost_core::policy::MAX_TOKENS {
        return Err(invalid_security_meta(TOKEN_LABELS_META));
    }
    let mut unique = HashSet::with_capacity(labels.len());
    for label in labels {
        validate_token_label(label)?;
        if !unique.insert(label.as_str()) {
            return Err(invalid_security_meta(TOKEN_LABELS_META));
        }
    }
    Ok(())
}

pub(crate) fn validate_token_label(label: &str) -> Result<()> {
    if label.is_empty()
        || label.len() > MAX_TOKEN_LABEL_BYTES
        || label.chars().any(char::is_control)
    {
        return Err(ClientError::Config(
            "token labels must be 1-128 printable bytes".into(),
        ));
    }
    Ok(())
}

fn invalid_security_meta(key: &str) -> ClientError {
    ClientError::Config(format!("stored security setting `{key}` is malformed"))
}

fn same_registry_trust(left: &RegistryTrust, right: &RegistryTrust) -> bool {
    left.expected_origin() == right.expected_origin()
        && left.checkpoint_key() == right.checkpoint_key()
        && left.witness_threshold() == right.witness_threshold()
        && left.minimum_checkpoint() == right.minimum_checkpoint()
        && left.max_cosignature_age_secs() == right.max_cosignature_age_secs()
        && left.future_clock_skew_secs() == right.future_clock_skew_secs()
        && left.witnesses().len() == right.witnesses().len()
        && left.witnesses().iter().all(|witness| {
            right
                .witnesses()
                .iter()
                .any(|candidate| candidate == witness)
        })
}

fn fixed32(bytes: Vec<u8>, field: &str) -> Result<[u8; 32]> {
    bytes
        .try_into()
        .map_err(|_| ClientError::Config(format!("{field} is malformed")))
}

fn attribution_as_str(attribution: Attribution) -> &'static str {
    match attribution {
        Attribution::Absent => "absent",
        Attribution::Valid => "valid",
        Attribution::Invalid => "invalid",
    }
}

fn parse_attribution(value: &str) -> Result<Attribution> {
    match value {
        "absent" => Ok(Attribution::Absent),
        "valid" => Ok(Attribution::Valid),
        "invalid" => Ok(Attribution::Invalid),
        _ => Err(ClientError::Config(
            "stored message attribution state is malformed".into(),
        )),
    }
}

fn hex_bytes(bytes: &[u8]) -> String {
    const DIGITS: &[u8; 16] = b"0123456789abcdef";
    let mut out = String::with_capacity(bytes.len() * 2);
    for byte in bytes {
        out.push(DIGITS[(byte >> 4) as usize] as char);
        out.push(DIGITS[(byte & 0x0f) as usize] as char);
    }
    out
}

fn sqlite_i64(value: u64, field: &str) -> Result<i64> {
    i64::try_from(value).map_err(|_| ClientError::Config(format!("{field} overflowed")))
}

fn current_time_secs() -> Result<u64> {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|duration| duration.as_secs())
        .map_err(|_| ClientError::Config("system clock is before the Unix epoch".into()))
}

fn sqlite_u64(value: i64, field: &str) -> Result<u64> {
    u64::try_from(value).map_err(|_| ClientError::Config(format!("{field} is negative")))
}

fn sqlite_u32(value: i64, field: &str) -> Result<u32> {
    u32::try_from(value)
        .map_err(|_| ClientError::Config(format!("{field} is outside the supported range")))
}

fn sqlite_usize(value: i64, field: &str) -> Result<usize> {
    usize::try_from(value)
        .map_err(|_| ClientError::Config(format!("{field} is outside the supported range")))
}

#[cfg(unix)]
struct StateDatabaseCustody {
    directory: GuardedDir,
    main: GuardedFile,
    main_name: LeafName,
    path: PathBuf,
    sidecar_names: [LeafName; 2],
    journal_name: LeafName,
    retained_sidecars: Mutex<[Option<GuardedFile>; 2]>,
}

#[cfg(unix)]
impl StateDatabaseCustody {
    fn open_or_create(requested: &Path) -> Result<Self> {
        let normalized = NormalizedPath::new(requested).map_err(map_state_custody_error)?;
        let main_name = normalized.as_path().file_name().ok_or_else(|| {
            map_state_custody_error(CustodyError::InvalidPath(
                "state database path must name a file",
            ))
        })?;
        let main_name = LeafName::new(main_name).map_err(map_state_custody_error)?;
        let parent = normalized.as_path().parent().ok_or_else(|| {
            map_state_custody_error(CustodyError::InvalidPath(
                "state database path has no parent",
            ))
        })?;
        let directory = match GuardedDir::open_existing(parent, DirPolicy::trusted()) {
            Ok(directory) => directory,
            Err(CustodyError::NotFound) => {
                GuardedDir::create_private(parent).map_err(map_state_custody_error)?
            }
            Err(error) => return Err(map_state_custody_error(error)),
        };
        let path = directory.absolute_path().join(main_name.as_os_str());
        let sidecar_names = [
            state_suffixed_leaf(&main_name, "-wal")?,
            state_suffixed_leaf(&main_name, "-shm")?,
        ];
        let journal_name = state_suffixed_leaf(&main_name, "-journal")?;

        for sidecar in &sidecar_names {
            directory
                .validate_file(sidecar, state_sqlite_file_policy())
                .map_err(map_state_custody_error)?;
        }
        directory
            .validate_file(&journal_name, state_sqlite_file_policy())
            .map_err(map_state_custody_error)?;
        let main = directory
            .open_or_create_file(
                &main_name,
                OpenAccess::ReadWrite,
                state_sqlite_file_policy(),
            )
            .map_err(map_state_custody_error)?;
        let custody = Self {
            directory,
            main,
            main_name,
            path,
            sidecar_names,
            journal_name,
            retained_sidecars: Mutex::new(std::array::from_fn(|_| None)),
        };
        custody.verify_main_named()?;
        custody.verify_sidecars(false)?;
        Ok(custody)
    }

    fn verify_main_named(&self) -> Result<()> {
        self.directory
            .verify_named()
            .map_err(map_state_custody_error)?;
        self.main.verify_named().map_err(map_state_custody_error)?;
        let named = self
            .directory
            .validate_file(&self.main_name, state_sqlite_file_policy())
            .map_err(map_state_custody_error)?
            .ok_or_else(|| map_state_custody_error(CustodyError::NotFound))?;
        if named.identity != self.main.identity() {
            return Err(map_state_custody_error(CustodyError::UnsafeFile(
                "state database name no longer identifies retained main file",
            )));
        }
        Ok(())
    }

    fn verify_sqlite_connection(&self, conn: &Connection) -> Result<()> {
        if conn.path().map(Path::new) != Some(self.path.as_path()) {
            return Err(map_state_custody_error(CustodyError::UnsafeFile(
                "SQLite reports a different state database path",
            )));
        }
        self.verify_main_named()
    }

    fn verify_sidecars(&self, require_wal_and_shm: bool) -> Result<()> {
        let mut retained = self
            .retained_sidecars
            .lock()
            .unwrap_or_else(|error| error.into_inner());
        for (name, retained_file) in self.sidecar_names.iter().zip(retained.iter_mut()) {
            if let Some(file) = retained_file {
                file.verify_named().map_err(map_state_custody_error)?;
                continue;
            }
            match self
                .directory
                .open_file_optional(name, OpenAccess::ReadOnly, state_sqlite_file_policy())
                .map_err(map_state_custody_error)?
            {
                Some(file) => *retained_file = Some(file),
                None if require_wal_and_shm => {
                    return Err(map_state_custody_error(CustodyError::UnsafeFile(
                        "required SQLite WAL or SHM sidecar is missing",
                    )));
                }
                None => {}
            }
        }
        self.directory
            .validate_file(&self.journal_name, state_sqlite_file_policy())
            .map_err(map_state_custody_error)?;
        Ok(())
    }

    fn verify_all_named(&self) -> Result<()> {
        self.verify_main_named()?;
        self.verify_sidecars(true)
    }
}

#[cfg(unix)]
fn state_sqlite_file_policy() -> FilePolicy {
    FilePolicy::private(MAX_SQLITE_FILE_BYTES)
}

#[cfg(unix)]
fn state_suffixed_leaf(name: &LeafName, suffix: &str) -> Result<LeafName> {
    let mut value = name.as_os_str().to_os_string();
    value.push(suffix);
    LeafName::new(value).map_err(map_state_custody_error)
}

#[cfg(unix)]
fn map_state_custody_error(error: CustodyError) -> ClientError {
    match error {
        CustodyError::Io(error) if state_custody_io_is_policy_failure(&error) => {
            ClientError::Config(format!("state database custody failed: {error}"))
        }
        CustodyError::Io(error) => ClientError::Io(error),
        error => ClientError::Config(format!("state database custody failed: {error}")),
    }
}

#[cfg(unix)]
fn state_custody_io_is_policy_failure(error: &std::io::Error) -> bool {
    error.raw_os_error().is_some_and(|raw| {
        [
            rustix::io::Errno::LOOP,
            rustix::io::Errno::ISDIR,
            rustix::io::Errno::NOTDIR,
        ]
        .into_iter()
        .any(|candidate| candidate.raw_os_error() == raw)
    })
}

#[cfg(windows)]
struct WindowsStateDatabasePreparation {
    path: std::path::PathBuf,
    main: crate::keystore::windows_custody::RetainedPrivateFile,
    wal_path: std::path::PathBuf,
    shm_path: std::path::PathBuf,
    journal_path: std::path::PathBuf,
}

#[cfg(windows)]
impl WindowsStateDatabasePreparation {
    fn open_or_create(requested: &std::path::Path) -> Result<Self> {
        let path = crate::keystore::windows_custody::normalized_absolute(requested)?;
        if path.file_name().is_none() {
            return Err(ClientError::Config(
                "state database path must name a file".into(),
            ));
        }
        let parent = path
            .parent()
            .ok_or_else(|| ClientError::Config("state database path has no parent".into()))?;
        crate::keystore::secure_or_create_directory(parent)?;
        let wal_path = windows_state_sidecar_path(&path, "-wal")?;
        let shm_path = windows_state_sidecar_path(&path, "-shm")?;
        let journal_path = windows_state_sidecar_path(&path, "-journal")?;

        // This must precede the SQLite open. A hostile pre-existing sidecar is never handed to
        // SQLite for recovery, parsing, truncation, or replacement before custody rejects it.
        for sidecar in [&wal_path, &shm_path, &journal_path] {
            windows_validate_optional_state_file(sidecar)?;
        }
        let (main, _created) =
            crate::keystore::windows_custody::retain_or_create_private_file(&path)?;
        windows_validate_retained_state_file(&main)?;
        Ok(Self {
            path,
            main,
            wal_path,
            shm_path,
            journal_path,
        })
    }

    fn verify_main_named(&self) -> Result<()> {
        windows_validate_retained_state_file(&self.main)
    }

    fn verify_sqlite_connection(&self, conn: &Connection) -> Result<()> {
        if conn.path().map(std::path::Path::new) != Some(self.path.as_path()) {
            return Err(ClientError::Config(
                "SQLite reports a different state database path".into(),
            ));
        }
        // The retained main-file and ancestor handles omit delete sharing before SQLite receives
        // this exact normalized path, so the name cannot be rebound between the two opens.
        self.verify_main_named()
    }

    fn finish(self) -> Result<WindowsStateDatabaseCustody> {
        windows_harden_optional_subsystem_state_file(&self.journal_path)?;
        let wal =
            crate::keystore::windows_custody::retain_and_protect_subsystem_file(&self.wal_path)
                .map_err(|error| windows_required_sidecar_error(error, "WAL"))?;
        let shm =
            crate::keystore::windows_custody::retain_and_protect_subsystem_file(&self.shm_path)
                .map_err(|error| windows_required_sidecar_error(error, "SHM"))?;
        windows_validate_retained_state_file(&wal)?;
        windows_validate_retained_state_file(&shm)?;
        let custody = WindowsStateDatabaseCustody {
            main: self.main,
            wal,
            shm,
            journal_path: self.journal_path,
        };
        custody.verify_all_named()?;
        Ok(custody)
    }
}

#[cfg(windows)]
struct WindowsStateDatabaseCustody {
    main: crate::keystore::windows_custody::RetainedPrivateFile,
    wal: crate::keystore::windows_custody::RetainedPrivateFile,
    shm: crate::keystore::windows_custody::RetainedPrivateFile,
    journal_path: std::path::PathBuf,
}

#[cfg(windows)]
impl WindowsStateDatabaseCustody {
    fn verify_all_named(&self) -> Result<()> {
        windows_validate_retained_state_file(&self.main)?;
        windows_validate_retained_state_file(&self.wal)?;
        windows_validate_retained_state_file(&self.shm)?;
        windows_validate_optional_state_file(&self.journal_path)
    }
}

#[cfg(windows)]
fn windows_state_sidecar_path(path: &std::path::Path, suffix: &str) -> Result<std::path::PathBuf> {
    let name = path
        .file_name()
        .ok_or_else(|| ClientError::Config("state database path must name a file".into()))?;
    let mut sidecar_name = name.to_os_string();
    sidecar_name.push(suffix);
    let parent = path
        .parent()
        .ok_or_else(|| ClientError::Config("state database path has no parent".into()))?;
    let sidecar = parent.join(sidecar_name);
    let normalized = crate::keystore::windows_custody::normalized_absolute(&sidecar)?;
    if normalized != sidecar {
        return Err(ClientError::Config(
            "state database sidecar path is not exactly normalized".into(),
        ));
    }
    Ok(sidecar)
}

#[cfg(windows)]
fn windows_validate_optional_state_file(path: &std::path::Path) -> Result<()> {
    if let Some(file) = crate::keystore::windows_custody::retain_private_file_optional(path, false)?
    {
        windows_validate_retained_state_file(&file)?;
    }
    Ok(())
}

#[cfg(windows)]
fn windows_harden_optional_subsystem_state_file(path: &std::path::Path) -> Result<()> {
    match crate::keystore::windows_custody::retain_and_protect_subsystem_file(path) {
        Ok(file) => windows_validate_retained_state_file(&file),
        Err(ClientError::Io(error)) if error.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(error) => Err(error),
    }
}

#[cfg(windows)]
fn windows_validate_retained_state_file(
    file: &crate::keystore::windows_custody::RetainedPrivateFile,
) -> Result<()> {
    file.verify()?;
    if file.len()? > MAX_SQLITE_FILE_BYTES {
        return Err(ClientError::Config(format!(
            "state database custody failed: {} exceeds the supported size bound",
            file.path().display()
        )));
    }
    Ok(())
}

#[cfg(windows)]
fn windows_required_sidecar_error(error: ClientError, name: &str) -> ClientError {
    match error {
        ClientError::Io(error) if error.kind() == std::io::ErrorKind::NotFound => {
            ClientError::Config(format!(
                "state database custody failed: required SQLite {name} sidecar is missing"
            ))
        }
        error => error,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use ed25519_dalek::SigningKey;
    use pigeonpost_core::{envelope, Identity};
    use pigeonpost_registry::MerkleLog;

    fn sample_wrap() -> Wrap {
        let a = Identity::from_seed([1; 32]);
        let b = Identity::from_seed([2; 32]);
        envelope::wrap(&a, &b.verifying_key(), "hi", 1_000).unwrap()
    }

    #[test]
    fn debug_output_withholds_decrypted_and_queued_message_material() {
        let stored = StoredMessage {
            id: "stored-id-debug-canary-8642".to_owned(),
            from_pubkey: [0xA8; 32],
            from_address: "/k/sender-debug-canary-8642".to_owned(),
            received_at: 1_234,
            read: false,
            state: "accepted".to_owned(),
            attribution: Attribution::Valid,
            body: UntrustedBody::new("stored-body-debug-canary-8642"),
        };
        assert_eq!(format!("{stored:?}"), "StoredMessage(<withheld>)");

        let queued = OutboxEntry {
            row: 7,
            message_id: "outbox-id-debug-canary-8642".to_owned(),
            loft_url: "https://outbox-debug-canary-8642.example".to_owned(),
            wrap: sample_wrap(),
            token: Some(Token::mint(&[0xB9; 32], "outbox-debug-canary-8642")),
            allow_local: false,
            attempts: 3,
        };
        assert_eq!(format!("{queued:?}"), "OutboxEntry(<withheld>)");
    }

    fn sample_rotation(
        index: u8,
        activated_at: u64,
    ) -> (Address, RotationRecord, AgentRecord, AgentRecord) {
        let outgoing = Identity::from_seed([index; 32]);
        let incoming = Identity::from_seed([index + 64; 32]);
        let next = Identity::from_seed([index + 128; 32]);
        let outgoing_successor = keys::SuccessorCommitment::for_key(&incoming.verifying_key());
        let next_successor = keys::SuccessorCommitment::for_key(&next.verifying_key());
        let source = AgentRecord::new(
            &outgoing,
            &outgoing_successor,
            index as u64,
            vec!["https://own.example".into()],
        );
        let record = RotationRecord::new(
            &outgoing,
            &incoming,
            &next_successor,
            index as u64 + 1,
            activated_at,
        )
        .unwrap();
        let target = AgentRecord::new(
            &incoming,
            &next_successor,
            index as u64 + 1,
            vec!["https://own.example".into()],
        );
        (outgoing.address(), record, source, target)
    }

    fn create_state_schema(conn: &mut Connection, version: i64) {
        conn.execute_batch(V0_1_0_SCHEMA).unwrap();
        for target in 2..=version {
            let tx = conn
                .transaction_with_behavior(TransactionBehavior::Immediate)
                .unwrap();
            apply_client_migration(&tx, target).unwrap();
            tx.commit().unwrap();
        }
        conn.pragma_update(None, "user_version", version).unwrap();
    }

    fn make_state_private(path: &std::path::Path) {
        assert!(path.is_file(), "fixture path must be a regular file");
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o600)).unwrap();
        }
        #[cfg(windows)]
        {
            // SQLite fixtures are deliberately created outside the production open path. On an
            // elevated Windows runner their default owner can therefore be Administrators even
            // though the process token belongs to the runner user. Mark only this test-created
            // subsystem file as current-user private; production continues to reject arbitrary
            // pre-existing objects rather than adopting them.
            drop(
                crate::keystore::windows_custody::retain_and_protect_subsystem_file(path).unwrap(),
            );
        }
    }

    fn private_state_path(
        file_name: impl AsRef<std::path::Path>,
    ) -> (tempfile::TempDir, std::path::PathBuf) {
        let directory = tempfile::tempdir().unwrap();
        #[cfg(windows)]
        {
            // `tempfile` inherits the elevated token's default owner on hosted Windows. Put the
            // database beneath a child created through the real custody boundary so direct
            // `State::open` fixtures exercise the same owner/DACL contract as production.
            let private = directory.path().join("private");
            crate::keystore::secure_or_create_directory(&private).unwrap();
            let path = private.join(file_name);
            (directory, path)
        }
        #[cfg(not(windows))]
        {
            let path = directory.path().join(file_name);
            (directory, path)
        }
    }

    fn assert_non_custody_config_error(result: Result<State>) {
        let error = match result {
            Ok(_) => panic!("malformed schema was accepted"),
            Err(error) => error,
        };
        match error {
            ClientError::Config(message) => assert!(
                !message.contains("custody") && !message.contains("private storage"),
                "schema regression stopped at custody instead of schema validation: {message}"
            ),
            error => panic!("schema regression returned the wrong error class: {error}"),
        }
    }

    #[test]
    fn cursors_never_go_backwards() {
        let state = State::in_memory().unwrap();
        let first = Identity::from_seed([0xA1; 32]).address();
        let second = Identity::from_seed([0xA2; 32]).address();
        state.set_cursor("wss://a", &first, 10).unwrap();
        state.set_cursor("wss://a", &first, 25).unwrap();
        state.set_cursor("wss://a", &first, 5).unwrap();
        state.set_cursor("wss://a", &second, 7).unwrap();

        assert_eq!(
            state.cursor("wss://a", &first).unwrap(),
            25,
            "a rewound cursor would re-deliver mail the agent already handled"
        );
        assert_eq!(state.cursor("wss://a", &second).unwrap(), 7);
    }

    #[test]
    fn outbox_tracks_delivery_per_loft() {
        let state = State::in_memory().unwrap();
        let wrap = sample_wrap();

        state
            .queue(
                "m1",
                "/k/x",
                OutboxRoute::new("wss://a", false),
                &wrap,
                None,
                100,
            )
            .unwrap();
        state
            .queue(
                "m1",
                "/k/x",
                OutboxRoute::new("wss://b", false),
                &wrap,
                None,
                100,
            )
            .unwrap();
        assert_eq!(state.pending_count().unwrap(), 2);

        let pending = state.pending(10, 100).unwrap();
        state.mark_sent(pending[0].row, 200).unwrap();
        assert_eq!(
            state.pending_count().unwrap(),
            1,
            "one loft accepting must not clear the copy owed to the other"
        );
        assert_eq!(
            state.delivery_status("m1").unwrap(),
            DeliveryStatus {
                delivered: 1,
                queued: 1,
                terminal: 0,
            }
        );
    }

    #[test]
    fn queueing_the_same_copy_twice_is_idempotent() {
        let state = State::in_memory().unwrap();
        let wrap = sample_wrap();
        state
            .queue(
                "m1",
                "/k/x",
                OutboxRoute::new("wss://a", false),
                &wrap,
                None,
                100,
            )
            .unwrap();
        state
            .queue(
                "m1",
                "/k/x",
                OutboxRoute::new("wss://a", false),
                &wrap,
                None,
                100,
            )
            .unwrap();
        assert_eq!(state.pending_count().unwrap(), 1);
    }

    #[test]
    fn active_loft_order_is_deterministic_when_timestamps_tie() {
        let state = State::in_memory().unwrap();
        state.add_loft("https://z.example", None, 10).unwrap();
        state.add_loft("https://a.example", None, 10).unwrap();

        assert_eq!(
            state.lofts().unwrap(),
            vec![
                ("https://a.example".to_owned(), None),
                ("https://z.example".to_owned(), None),
            ]
        );
        assert_eq!(
            state
                .lofts_with_local_trust()
                .unwrap()
                .into_iter()
                .map(|(url, _, _)| url)
                .collect::<Vec<_>>(),
            vec!["https://a.example", "https://z.example"]
        );
    }

    #[test]
    fn configured_directory_set_is_bounded_and_exact_readd_is_idempotent() {
        let state = State::in_memory().unwrap();
        for index in 0..MAX_CONFIGURED_DIRECTORIES {
            assert!(state
                .add_directory(
                    &format!("https://directory-{index}.example"),
                    &[index as u8; 32],
                    index as u64,
                )
                .unwrap());
        }
        assert_eq!(
            state.directories().unwrap().len(),
            MAX_CONFIGURED_DIRECTORIES
        );
        assert!(!state
            .add_directory("https://directory-0.example", &[0; 32], 100)
            .unwrap());
        assert!(state
            .add_directory("https://directory-0.example", &[1; 32], 100)
            .is_err());
        assert!(matches!(
            state.add_directory("https://one-too-many.example", &[0xFF; 32], 101),
            Err(ClientError::Core(pigeonpost_core::Error::TooLarge))
        ));
        assert!(state
            .remove_directory("https://directory-0.example")
            .unwrap());
        assert!(!state
            .remove_directory("https://directory-0.example")
            .unwrap());
        assert!(state
            .add_directory("https://directory-0.example", &[0xEE; 32], 102)
            .unwrap());
        assert_eq!(
            state.directories().unwrap().len(),
            MAX_CONFIGURED_DIRECTORIES
        );
    }

    #[test]
    fn replacement_churn_is_bounded_without_rejecting_an_existing_route() {
        let state = State::in_memory().unwrap();
        for index in 0..MAX_STORED_LOFT_ROUTES {
            let url = format!("https://replacement-{index}.example");
            state.add_loft(&url, None, index as u64).unwrap();
            assert!(state.remove_loft(&url, index as u64).unwrap());
        }
        assert_eq!(
            state
                .lofts_for_drain_with_local_trust(MAX_STORED_LOFT_ROUTES as u64)
                .unwrap()
                .len(),
            MAX_STORED_LOFT_ROUTES
        );
        assert!(matches!(
            state.add_loft("https://one-too-many.example", None, 100),
            Err(ClientError::Core(pigeonpost_core::Error::TooLarge))
        ));
        assert_eq!(
            state
                .conn
                .query_row("SELECT COUNT(*) FROM lofts", [], |row| row.get::<_, i64>(0))
                .unwrap(),
            MAX_STORED_LOFT_ROUTES as i64
        );

        // Re-enabling an exact draining route consumes no new slot.
        state
            .add_loft("https://replacement-0.example", None, 101)
            .unwrap();
        assert_eq!(state.lofts().unwrap().len(), 1);
        assert_eq!(
            state
                .conn
                .query_row("SELECT COUNT(*) FROM lofts", [], |row| row.get::<_, i64>(0))
                .unwrap(),
            MAX_STORED_LOFT_ROUTES as i64
        );
    }

    #[test]
    fn loft_replacement_drain_persists_and_revokes_local_trust_only_at_expiry() {
        let (_dir, path) = private_state_path("state.db");
        let loopback = "http://127.0.0.1:7717";
        let wrap = sample_wrap();

        {
            let state = State::open(&path).unwrap();
            state
                .add_loft_with_local_trust(loopback, None, 10, true)
                .unwrap();
            state.add_loft("https://loft.example", None, 11).unwrap();
            state
                .queue(
                    "local",
                    "/k/x",
                    OutboxRoute::new(loopback, true),
                    &wrap,
                    None,
                    12,
                )
                .unwrap();
            let address = Identity::from_seed([0x44; 32]).address();
            state.set_cursor(loopback, &address, 42).unwrap();
        }

        let state = State::open(&path).unwrap();
        let lofts = state.lofts_with_local_trust().unwrap();
        assert_eq!(lofts[0], (loopback.to_owned(), None, true));
        assert_eq!(lofts[1], ("https://loft.example".to_owned(), None, false));
        assert!(state.pending(1, 12).unwrap()[0].allow_local);

        assert!(state.remove_loft(loopback, 20).unwrap());
        assert_eq!(
            state.lofts().unwrap(),
            vec![("https://loft.example".to_owned(), None)]
        );
        assert!(state
            .lofts_for_drain_with_local_trust(20 + LOFT_DRAIN_GRACE_SECS - 1)
            .unwrap()
            .iter()
            .any(|(url, _, allow_local)| url == loopback && *allow_local));
        assert!(state.pending(1, 20).unwrap()[0].allow_local);

        // A repeated command may retry record publication, but must not move the persisted cutoff.
        assert!(state.remove_loft(loopback, 200).unwrap());
        let deadline: i64 = state
            .conn
            .query_row(
                "SELECT drain_after FROM lofts WHERE url = ?1",
                params![loopback],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(deadline, (20 + LOFT_DRAIN_GRACE_SECS) as i64);

        drop(state);
        let state = State::open(&path).unwrap();
        assert!(state
            .lofts_for_drain_with_local_trust(20 + LOFT_DRAIN_GRACE_SECS - 1)
            .unwrap()
            .iter()
            .any(|(url, _, allow_local)| url == loopback && *allow_local));
        assert_eq!(
            state
                .finalize_expired_lofts(20 + LOFT_DRAIN_GRACE_SECS)
                .unwrap(),
            1
        );
        assert!(!state
            .lofts_for_drain_with_local_trust(20 + LOFT_DRAIN_GRACE_SECS)
            .unwrap()
            .iter()
            .any(|(url, _, _)| url == loopback));
        assert!(!state.pending(1, 20).unwrap()[0].allow_local);
        let address = Identity::from_seed([0x44; 32]).address();
        assert_eq!(state.cursor(loopback, &address).unwrap(), 0);
    }

    #[test]
    fn loft_drain_uses_validated_advertised_retention_capped_at_thirty_days() {
        let state = State::in_memory().unwrap();
        let short = "https://short-retention.example";
        state
            .add_loft_with_retention_and_local_trust(short, None, 7, 1, false)
            .unwrap();
        assert!(state.remove_loft(short, 10).unwrap());
        let short_deadline: i64 = state
            .conn
            .query_row(
                "SELECT drain_after FROM lofts WHERE url = ?1",
                params![short],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(short_deadline, (10 + 7 * SECONDS_PER_DAY) as i64);

        let long = "https://long-retention.example";
        state
            .add_loft_with_retention_and_local_trust(
                long,
                None,
                pigeonpost_loft::MAX_RETENTION_DAYS,
                2,
                false,
            )
            .unwrap();
        assert!(state.remove_loft(long, 20).unwrap());
        assert!(state.remove_loft(long, 200).unwrap());
        let long_deadline: i64 = state
            .conn
            .query_row(
                "SELECT drain_after FROM lofts WHERE url = ?1",
                params![long],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(long_deadline, (20 + LOFT_DRAIN_GRACE_SECS) as i64);

        assert!(state
            .add_loft_with_retention_and_local_trust("https://zero.example", None, 0, 3, false)
            .is_err());
        assert!(state
            .add_loft_with_retention_and_local_trust(
                "https://too-long.example",
                None,
                pigeonpost_loft::MAX_RETENTION_DAYS + 1,
                3,
                false,
            )
            .is_err());

        state
            .conn
            .pragma_update(None, "ignore_check_constraints", true)
            .unwrap();
        state
            .conn
            .execute(
                "UPDATE lofts SET retention_days = 0 WHERE url = ?1",
                params![short],
            )
            .unwrap();
        state
            .conn
            .pragma_update(None, "ignore_check_constraints", false)
            .unwrap();
        assert!(state.lofts_for_drain_with_local_trust(11).is_err());
    }

    #[test]
    fn capability_token_round_trips_only_through_private_outbox_state() {
        let state = State::in_memory().unwrap();
        let wrap = sample_wrap();
        let token = Token::mint(&[7; 32], "integration-test");
        state
            .queue(
                "m1",
                "/k/x",
                OutboxRoute::new("wss://a", false),
                &wrap,
                Some(&token),
                100,
            )
            .unwrap();

        let queued = state.pending(1, 100).unwrap();
        assert_eq!(queued[0].token.as_ref(), Some(&token));
        assert!(!format!("{:?}", queued[0].token).contains(&token.to_hex()));
    }

    #[test]
    fn pending_delivery_view_is_payload_free_and_clamped() {
        let state = State::in_memory().unwrap();
        let wrap = sample_wrap();
        let token = Token::mint(&[0xBC; 32], "pending-metadata-secret");
        for index in 0..=MAX_PUBLIC_LIST_RESULTS {
            state
                .queue(
                    &format!("pending-{index:04}"),
                    "/k/recipient",
                    OutboxRoute::new("https://loft.example", false),
                    &wrap,
                    Some(&token),
                    index as u64,
                )
                .unwrap();
        }
        let first_row = state.pending(1, 0).unwrap()[0].row;
        state.mark_failed(first_row, "network", 1).unwrap();

        let pending = state.pending_deliveries(usize::MAX).unwrap();
        assert_eq!(pending.len(), MAX_PUBLIC_LIST_RESULTS);
        assert_eq!(pending[0].message_id, "pending-0000");
        assert_eq!(pending[0].last_error.as_deref(), Some("network"));
        assert!(pending[0].next_attempt_at > pending[0].created_at);
        let rendered = format!("{pending:?}");
        assert!(!rendered.contains(&token.to_hex()));
        assert!(!rendered.contains("ciphertext"));
    }

    #[test]
    fn failures_are_counted_and_stay_pending() {
        let state = State::in_memory().unwrap();
        let wrap = sample_wrap();
        state
            .queue(
                "m1",
                "/k/x",
                OutboxRoute::new("wss://a", false),
                &wrap,
                None,
                100,
            )
            .unwrap();

        let row = state.pending(1, 100).unwrap()[0].row;
        state.mark_failed(row, "network", 100).unwrap();

        assert!(
            state.pending(1, 100).unwrap().is_empty(),
            "retry is delayed"
        );
        let still = state.pending(1, 500).unwrap();
        assert_eq!(still.len(), 1, "a failed send stays owed");
        assert_eq!(still[0].attempts, 1);
    }

    #[test]
    fn terminal_failures_leave_retry_queue_and_remain_operator_visible() {
        let state = State::in_memory().unwrap();
        let wrap = sample_wrap();
        state
            .queue(
                "m1",
                "/k/x",
                OutboxRoute::new("https://loft.example", false),
                &wrap,
                None,
                100,
            )
            .unwrap();

        let row = state.pending(1, 100).unwrap()[0].row;
        state.mark_terminal(row, "http_400", 200).unwrap();

        assert!(state.pending(10, 1_000).unwrap().is_empty());
        assert_eq!(state.pending_count().unwrap(), 0);
        assert_eq!(state.terminal_count().unwrap(), 1);
        assert_eq!(
            state.delivery_status("m1").unwrap(),
            DeliveryStatus {
                delivered: 0,
                queued: 0,
                terminal: 1,
            }
        );
        let letters = state.dead_letters(10).unwrap();
        assert_eq!(letters.len(), 1);
        assert_eq!(letters[0].reason, "http_400");
        assert_eq!(letters[0].attempts, 1);
        assert_eq!(letters[0].terminal_at, 200);

        // Idempotent queueing cannot silently resurrect a terminal delivery.
        state
            .queue(
                "m1",
                "/k/x",
                OutboxRoute::new("https://loft.example", false),
                &wrap,
                None,
                300,
            )
            .unwrap();
        assert_eq!(state.terminal_count().unwrap(), 1);

        // A concurrent success is authoritative and clears terminal state.
        state.mark_sent(row, 400).unwrap();
        assert_eq!(state.terminal_count().unwrap(), 0);
        assert!(state.dead_letters(10).unwrap().is_empty());
        assert_eq!(
            state.delivery_status("m1").unwrap(),
            DeliveryStatus {
                delivered: 1,
                queued: 0,
                terminal: 0,
            }
        );
    }

    #[test]
    fn outbox_reasons_are_codes_not_untrusted_diagnostics() {
        let state = State::in_memory().unwrap();
        let wrap = sample_wrap();
        state
            .queue(
                "m1",
                "/k/x",
                OutboxRoute::new("https://loft.example", false),
                &wrap,
                None,
                100,
            )
            .unwrap();
        let row = state.pending(1, 100).unwrap()[0].row;

        assert!(state.mark_terminal(row, "", 200).is_err());
        assert!(state
            .mark_terminal(row, "HTTP 400: secret body", 200)
            .is_err());
        assert!(state.mark_failed(row, &"x".repeat(129), 200).is_err());
        assert_eq!(state.pending_count().unwrap(), 1);
    }

    #[test]
    fn outbox_quota_is_atomic_and_exact_duplicates_are_free() {
        let state = State::in_memory().unwrap();
        state
            .set_storage_limits(StorageLimits {
                outbox_rows: 1,
                ..StorageLimits::default()
            })
            .unwrap();
        let wrap = sample_wrap();
        let route = OutboxRoute::new("https://loft.example", false);
        state
            .queue("quota-copy", "/k/x", route, &wrap, None, 100)
            .unwrap();
        let first = state.storage_status().unwrap();
        state
            .queue("quota-copy", "/k/x", route, &wrap, None, 100)
            .unwrap();
        assert_eq!(state.storage_status().unwrap(), first);
        assert!(matches!(
            state.queue(
                "another-copy",
                "/k/x",
                OutboxRoute::new("https://other.example", false),
                &wrap,
                None,
                100,
            ),
            Err(ClientError::StorageLimit(StorageResource::OutboxRows))
        ));
        assert_eq!(state.pending_count().unwrap(), 1);

        let rollback = State::in_memory().unwrap();
        rollback
            .set_storage_limits(StorageLimits {
                outbox_rows: 1,
                ..StorageLimits::default()
            })
            .unwrap();
        let correspondent = [0x91; 32];
        assert!(matches!(
            rollback.queue_correspondence(
                "all-or-nothing",
                "/k/x",
                &[
                    OutboxRoute::new("https://one.example", false),
                    OutboxRoute::new("https://two.example", false),
                ],
                &wrap,
                None,
                &correspondent,
                100,
            ),
            Err(ClientError::StorageLimit(StorageResource::OutboxRows))
        ));
        assert_eq!(rollback.pending_count().unwrap(), 0);
        assert_eq!(
            rollback.storage_status().unwrap().usage,
            StorageUsage::default()
        );
        assert!(!rollback.is_allowed(&correspondent).unwrap());
    }

    #[test]
    fn inbox_delete_reclaims_body_quota_but_retains_an_indefinite_id_tombstone() {
        let state = State::in_memory().unwrap();
        state
            .set_storage_limits(StorageLimits {
                inbox_messages: 1,
                inbox_body_bytes: 5,
                ..StorageLimits::default()
            })
            .unwrap();
        let body = UntrustedBody::new("hello");
        assert!(state
            .store_message("one", &[1; 32], "/k/x", 10, &body, "accepted")
            .unwrap());
        assert!(!state
            .store_message("one", &[1; 32], "/k/x", 20, &body, "accepted")
            .unwrap());
        assert_eq!(
            state.storage_status().unwrap().usage,
            StorageUsage {
                inbox_messages: 1,
                inbox_tombstones: 0,
                inbox_body_bytes: 5,
                outbox_rows: 0,
                outbox_payload_bytes: 0,
            }
        );
        assert!(state.delete_message_at("one", 40).unwrap());
        assert!(!state.delete_message("one").unwrap());
        assert!(!state
            .store_message("one", &[9; 32], "/k/replayed", 50, &body, "accepted")
            .unwrap());
        assert!(state.message("one").unwrap().is_none());
        assert!(!state.mark_read("one").unwrap());
        assert!(state.set_message_state("one", "accepted").is_err());
        assert!(state
            .store_message("two", &[2; 32], "/k/y", 30, &body, "accepted")
            .unwrap());
        assert_eq!(
            state.storage_status().unwrap().usage,
            StorageUsage {
                inbox_messages: 1,
                inbox_tombstones: 1,
                inbox_body_bytes: 5,
                outbox_rows: 0,
                outbox_payload_bytes: 0,
            }
        );
        assert_eq!(
            state.storage_status().unwrap().inbox_tombstone_limit,
            MAX_INBOX_TOMBSTONES
        );
        let erased: (
            Vec<u8>,
            String,
            i64,
            i64,
            String,
            String,
            String,
            Option<i64>,
        ) = state
            .conn
            .query_row(
                "SELECT from_pubkey, from_address, received_at, read, body, state,
                        attribution, deleted_at FROM messages WHERE id = 'one'",
                [],
                |row| {
                    Ok((
                        row.get(0)?,
                        row.get(1)?,
                        row.get(2)?,
                        row.get(3)?,
                        row.get(4)?,
                        row.get(5)?,
                        row.get(6)?,
                        row.get(7)?,
                    ))
                },
            )
            .unwrap();
        assert_eq!(
            erased,
            (
                Vec::new(),
                String::new(),
                0,
                0,
                String::new(),
                "deleted".into(),
                "absent".into(),
                Some(40)
            )
        );
    }

    #[test]
    fn tombstone_hard_ceiling_survives_restart_and_fails_before_erasure() {
        let (_dir, path) = private_state_path("state.db");
        let state = State::open(&path).unwrap();
        let body = UntrustedBody::new("still present");
        state
            .store_message("protected", &[3; 32], "/k/source", 9, &body, "accepted")
            .unwrap();
        // Simulate the exact O(1) accounting state of a database containing the fixed maximum;
        // materializing one million inert rows would add no coverage to this admission test.
        state
            .conn
            .execute(
                "UPDATE storage_accounting SET inbox_tombstones = ?1 WHERE id = 1",
                params![MAX_INBOX_TOMBSTONES as i64],
            )
            .unwrap();
        drop(state);

        let reopened = State::open(&path).unwrap();
        assert_eq!(
            reopened.storage_status().unwrap().usage.inbox_tombstones,
            MAX_INBOX_TOMBSTONES
        );
        assert!(matches!(
            reopened.delete_message_at("protected", 10),
            Err(ClientError::StorageLimit(StorageResource::InboxTombstones))
        ));
        let message = reopened.message("protected").unwrap().unwrap();
        assert_eq!(message.body.as_str(), "still present");
        assert_eq!(reopened.storage_status().unwrap().usage.inbox_messages, 1);
    }

    #[test]
    fn inbound_protocol_body_ceiling_is_inclusive_and_shared_with_migration_validation() {
        let state = State::in_memory().unwrap();
        let exact = UntrustedBody::new("x".repeat(MAX_INBOX_BODY_BYTES_PER_MESSAGE));
        assert!(state
            .store_message("exact", &[1; 32], "/k/x", 1, &exact, "accepted")
            .unwrap());
        assert_eq!(
            state.storage_status().unwrap().usage.inbox_body_bytes,
            MAX_INBOX_BODY_BYTES_PER_MESSAGE as u64
        );
        let oversized = UntrustedBody::new("x".repeat(MAX_INBOX_BODY_BYTES_PER_MESSAGE + 1));
        assert!(matches!(
            state.store_message("oversized", &[1; 32], "/k/x", 1, &oversized, "accepted"),
            Err(ClientError::Core(pigeonpost_core::Error::TooLarge))
        ));
    }

    #[test]
    fn pending_outbox_delete_requires_exact_authorization() {
        let state = State::in_memory().unwrap();
        let wrap = sample_wrap();
        state
            .queue(
                "owed",
                "/k/x",
                OutboxRoute::new("https://loft.example", false),
                &wrap,
                None,
                100,
            )
            .unwrap();
        let row = OutboxRecordId::new(state.pending(1, 100).unwrap()[0].row).unwrap();
        assert!(state.delete_pending_outbox(row, "yes").is_err());
        assert_eq!(state.pending_count().unwrap(), 1);
        assert!(state
            .delete_pending_outbox(row, PENDING_OUTBOX_DELETE_CONFIRMATION)
            .unwrap());
        assert_eq!(
            state.storage_status().unwrap().usage,
            StorageUsage::default()
        );
    }

    #[test]
    fn sent_and_terminal_payloads_are_erased_and_metadata_deletes_are_exact() {
        let (_dir, path) = private_state_path("state.db");
        let token = Token::mint(&[0x71; 32], "erasure");
        let wrap = sample_wrap();
        let state = State::open(&path).unwrap();
        for (id, url) in [
            ("successful", "https://sent.example"),
            ("terminal", "https://terminal.example"),
        ] {
            state
                .queue(
                    id,
                    "/k/x",
                    OutboxRoute::new(url, false),
                    &wrap,
                    Some(&token),
                    100,
                )
                .unwrap();
        }
        let rows = state.pending(10, 100).unwrap();
        let sent_row = rows
            .iter()
            .find(|entry| entry.message_id == "successful")
            .unwrap()
            .row;
        let terminal_row = rows
            .iter()
            .find(|entry| entry.message_id == "terminal")
            .unwrap()
            .row;
        state.mark_sent(sent_row, 200).unwrap();
        state.mark_terminal(terminal_row, "http_400", 201).unwrap();
        let retained_payloads: i64 = state
            .conn
            .query_row(
                "SELECT COUNT(*) FROM outbox WHERE length(wrap) <> 0 OR token IS NOT NULL",
                [],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(retained_payloads, 0);
        assert_eq!(
            state.storage_status().unwrap().usage.outbox_payload_bytes,
            0
        );
        drop(state);

        let reopened = State::open(&path).unwrap();
        let completed = reopened.completed_deliveries(10).unwrap();
        let dead = reopened.dead_letters(10).unwrap();
        assert_eq!(completed.len(), 1);
        assert_eq!(dead.len(), 1);
        assert!(reopened
            .delete_completed_delivery(completed[0].row)
            .unwrap());
        assert!(!reopened.delete_dead_letter(completed[0].row).unwrap());
        assert!(reopened.delete_dead_letter(dead[0].row).unwrap());
        assert_eq!(reopened.storage_status().unwrap().usage.outbox_rows, 0);
    }

    #[test]
    fn completed_prune_is_age_and_batch_bounded_and_never_touches_inbox() {
        let state = State::in_memory().unwrap();
        let wrap = sample_wrap();
        for (id, sent_at) in [("old-a", 10), ("old-b", 20), ("new", 100)] {
            let url = format!("https://{id}.example");
            state
                .queue(id, "/k/x", OutboxRoute::new(&url, false), &wrap, None, 1)
                .unwrap();
            let row = state
                .pending(10, 1)
                .unwrap()
                .into_iter()
                .find(|entry| entry.message_id == id)
                .unwrap()
                .row;
            state.mark_sent(row, sent_at).unwrap();
        }
        state
            .queue(
                "operator-visible",
                "/k/x",
                OutboxRoute::new("https://dead.example", false),
                &wrap,
                None,
                1,
            )
            .unwrap();
        let dead_row = state
            .pending(10, 1)
            .unwrap()
            .into_iter()
            .find(|entry| entry.message_id == "operator-visible")
            .unwrap()
            .row;
        state.mark_terminal(dead_row, "http_400", 5).unwrap();
        state
            .store_message(
                "inbound",
                &[1; 32],
                "/k/x",
                1,
                &UntrustedBody::new("keep me"),
                "accepted",
            )
            .unwrap();
        assert_eq!(state.prune_completed_outbox(50, 1).unwrap(), 1);
        assert_eq!(state.completed_deliveries(10).unwrap().len(), 2);
        assert_eq!(state.dead_letters(10).unwrap().len(), 1);
        assert!(state.message("inbound").unwrap().is_some());

        state
            .queue(
                "still-pending",
                "/k/x",
                OutboxRoute::new("https://pending.example", false),
                &wrap,
                None,
                1,
            )
            .unwrap();
        assert!(state.prune_finished_outbox(50, 10, "confirm").is_err());
        assert_eq!(state.dead_letters(10).unwrap().len(), 1);
        // Oldest-first and bounded: the terminal row at 5 is removed before the successful row
        // at 20. Ordinary wake pruning above never touched it.
        assert_eq!(
            state
                .prune_finished_outbox(50, 1, FINISHED_OUTBOX_PRUNE_CONFIRMATION)
                .unwrap(),
            1
        );
        assert!(state.dead_letters(10).unwrap().is_empty());
        assert_eq!(state.completed_deliveries(10).unwrap().len(), 2);
        assert_eq!(
            state
                .prune_finished_outbox(50, 10, FINISHED_OUTBOX_PRUNE_CONFIRMATION)
                .unwrap(),
            1
        );
        // Strict-before leaves the successful row whose timestamp equals the boundary. Pending
        // debt and inbound data are never candidates.
        assert_eq!(
            state
                .prune_finished_outbox(100, 10, FINISHED_OUTBOX_PRUNE_CONFIRMATION)
                .unwrap(),
            0
        );
        assert_eq!(state.completed_deliveries(10).unwrap().len(), 1);
        assert_eq!(state.pending_count().unwrap(), 1);
        assert!(state.message("inbound").unwrap().is_some());
    }

    #[test]
    fn storage_accounting_is_atomic_across_connections() {
        use std::sync::{Arc, Barrier};

        let (_dir, path) = private_state_path("state.db");
        let path = Arc::new(path);
        let state = State::open(&path).unwrap();
        state
            .set_storage_limits(StorageLimits {
                inbox_messages: 4,
                inbox_body_bytes: 4,
                ..StorageLimits::default()
            })
            .unwrap();
        drop(state);

        let barrier = Arc::new(Barrier::new(9));
        let mut workers = Vec::new();
        for index in 0..8 {
            let path = Arc::clone(&path);
            let barrier = Arc::clone(&barrier);
            workers.push(std::thread::spawn(move || {
                let state = State::open(&path).unwrap();
                barrier.wait();
                state.store_message(
                    &format!("concurrent-{index}"),
                    &[index as u8; 32],
                    "/k/x",
                    1,
                    &UntrustedBody::new("x"),
                    "accepted",
                )
            }));
        }
        barrier.wait();
        let mut inserted = 0;
        let mut rejected = 0;
        for worker in workers {
            match worker.join().unwrap() {
                Ok(true) => inserted += 1,
                Err(ClientError::StorageLimit(_)) => rejected += 1,
                other => panic!("unexpected concurrent result: {other:?}"),
            }
        }
        assert_eq!((inserted, rejected), (4, 4));
        let reopened = State::open(&path).unwrap();
        assert_eq!(
            reopened.storage_status().unwrap().usage,
            StorageUsage {
                inbox_messages: 4,
                inbox_tombstones: 0,
                inbox_body_bytes: 4,
                outbox_rows: 0,
                outbox_payload_bytes: 0,
            }
        );
    }

    #[test]
    fn a_failed_copy_cannot_starve_new_outbox_work() {
        let state = State::in_memory().unwrap();
        let wrap = sample_wrap();
        state
            .queue(
                "old",
                "/k/x",
                OutboxRoute::new("wss://a", false),
                &wrap,
                None,
                100,
            )
            .unwrap();
        let old = state.pending(1, 100).unwrap()[0].row;
        state.mark_failed(old, "network", 100).unwrap();
        state
            .queue(
                "new",
                "/k/x",
                OutboxRoute::new("wss://a", false),
                &wrap,
                None,
                101,
            )
            .unwrap();

        let ready = state.pending(1, 101).unwrap();
        assert_eq!(ready[0].message_id, "new");
    }

    #[test]
    fn duplicate_messages_are_absorbed() {
        let state = State::in_memory().unwrap();
        let body = UntrustedBody::new("hello");

        assert!(state
            .store_message("abc", &[1; 32], "/k/x", 100, &body, "accepted")
            .unwrap());
        assert!(
            !state
                .store_message("abc", &[1; 32], "/k/x", 100, &body, "accepted")
                .unwrap(),
            "the same message from a second loft is not a new message"
        );
        assert_eq!(state.unread_count().unwrap(), 1);
    }

    #[test]
    fn attribution_policy_and_verdict_round_trip_without_conflating_states() {
        let state = State::in_memory().unwrap();
        let requirement = AttributionRequirement::new(Jurisdiction::Eu, [0xA5; 32]);
        assert_eq!(state.sender_attribution_requirement().unwrap(), None);
        state
            .set_sender_attribution_requirement(Some(requirement))
            .unwrap();
        assert_eq!(
            state.sender_attribution_requirement().unwrap(),
            Some(requirement)
        );

        let body = UntrustedBody::new("verified");
        state
            .store_message_with_attribution(
                "attributed",
                &[3; 32],
                "/k/sender",
                100,
                &body,
                "accepted",
                Attribution::Valid,
            )
            .unwrap();
        assert_eq!(
            state.message("attributed").unwrap().unwrap().attribution,
            Attribution::Valid
        );

        state.set_sender_attribution_requirement(None).unwrap();
        assert_eq!(state.sender_attribution_requirement().unwrap(), None);
    }

    #[test]
    fn releasing_a_sender_is_not_limited_by_the_pending_page_size() {
        let state = State::in_memory().unwrap();
        let body = UntrustedBody::new("held");
        let sender = [1; 32];

        for n in 0..1_005 {
            state
                .store_message(
                    &format!("sender-{n:04}"),
                    &sender,
                    "/k/sender",
                    n,
                    &body,
                    "pending",
                )
                .unwrap();
        }
        state
            .store_message("other", &[2; 32], "/k/other", 2_000, &body, "pending")
            .unwrap();

        assert_eq!(state.release_pending_from(&sender).unwrap(), 1_005);
        assert!(state
            .pending_messages(10)
            .unwrap()
            .iter()
            .all(|message| { message.from_pubkey != sender }));
        assert_eq!(state.messages(false, 2_000).unwrap().len(), 1_000);
        assert_eq!(state.storage_status().unwrap().usage.inbox_messages, 1_006);
    }

    #[test]
    fn messages_can_be_found_by_id_prefix() {
        let state = State::in_memory().unwrap();
        let body = UntrustedBody::new("hello");
        state
            .store_message("abcdef123456", &[1; 32], "/k/x", 100, &body, "accepted")
            .unwrap();

        assert!(state.message("abcdef").unwrap().is_some());
        assert!(state.message("zzz").unwrap().is_none());
    }

    #[test]
    fn ambiguous_message_prefixes_fail_closed() {
        let state = State::in_memory().unwrap();
        let body = UntrustedBody::new("hello");
        for id in ["abcdef111111", "abcdef222222"] {
            state
                .store_message(id, &[1; 32], "/k/x", 100, &body, "accepted")
                .unwrap();
        }
        assert!(matches!(
            state.message("abcdef"),
            Err(ClientError::AmbiguousMessage(_))
        ));
    }

    #[test]
    fn reading_and_acking_are_separate() {
        let state = State::in_memory().unwrap();
        let body = UntrustedBody::new("hello");
        state
            .store_message("abc", &[1; 32], "/k/x", 100, &body, "accepted")
            .unwrap();

        assert_eq!(state.messages(true, 10).unwrap().len(), 1);
        state.mark_read("abc").unwrap();
        assert_eq!(state.messages(true, 10).unwrap().len(), 0);
        assert_eq!(state.messages(false, 10).unwrap().len(), 1);
    }

    #[test]
    fn record_seq_only_climbs() {
        let state = State::in_memory().unwrap();
        assert_eq!(state.next_record_seq().unwrap(), 1);
        assert_eq!(state.next_record_seq().unwrap(), 2);
        assert_eq!(state.next_record_seq().unwrap(), 3);
        assert_eq!(state.next_policy_seq().unwrap(), 1);
        assert_eq!(state.next_policy_seq().unwrap(), 2);
    }

    #[test]
    fn exact_record_placement_survives_restart_and_preserves_only_matching_completion() {
        let (_dir, path) = private_state_path("state.db");
        let identity = Identity::from_seed([0x91; 32]);
        let successor =
            keys::SuccessorCommitment::for_key(&Identity::from_seed([0x92; 32]).verifying_key());
        let address = identity.address();
        let first = AgentRecord::new(&identity, &successor, 1, vec!["https://own.example".into()]);
        let targets = vec![
            PublicationTarget::pending("https://own.example".into(), false, false),
            PublicationTarget::pending("https://r1.example".into(), false, true),
        ];
        {
            let state = State::open(&path).unwrap();
            state
                .save_record_publication(&address, &first, &targets, 10)
                .unwrap();
            assert!(state
                .mark_record_target_complete(&address, &first, "https://r1.example")
                .unwrap());
        }

        let state = State::open(&path).unwrap();
        let shifted = vec![
            PublicationTarget::pending("https://own.example".into(), false, false),
            PublicationTarget::pending("https://r1.example".into(), false, true),
            PublicationTarget::pending("https://r2.example".into(), false, true),
        ];
        state
            .save_record_publication(&address, &first, &shifted, 20)
            .unwrap();
        let recovered = state.record_publication().unwrap().unwrap();
        assert_eq!(recovered.record, first);
        assert!(
            recovered
                .targets
                .iter()
                .find(|target| target.url == "https://r1.example")
                .unwrap()
                .completed
        );
        assert!(
            !recovered
                .targets
                .iter()
                .find(|target| target.url == "https://r2.example")
                .unwrap()
                .completed
        );

        let second = AgentRecord::new(&identity, &successor, 2, vec!["https://own.example".into()]);
        state
            .save_record_publication(&address, &second, &shifted, 30)
            .unwrap();
        assert!(state
            .record_publication()
            .unwrap()
            .unwrap()
            .targets
            .iter()
            .all(|target| !target.completed));
    }

    #[test]
    fn rotation_target_completion_is_exact_and_restart_safe() {
        let (_dir, path) = private_state_path("state.db");
        let outgoing = Identity::from_seed([0x93; 32]);
        let incoming = Identity::from_seed([0x94; 32]);
        let next = Identity::from_seed([0x95; 32]);
        let outgoing_successor = keys::SuccessorCommitment::for_key(&incoming.verifying_key());
        let next_successor = keys::SuccessorCommitment::for_key(&next.verifying_key());
        let source = AgentRecord::new(
            &outgoing,
            &outgoing_successor,
            4,
            vec!["https://own.example".into()],
        );
        let activated_at = current_time_secs().unwrap();
        let record =
            RotationRecord::new(&outgoing, &incoming, &next_successor, 5, activated_at).unwrap();
        let target = AgentRecord::new(
            &incoming,
            &next_successor,
            5,
            vec!["https://own.example".into()],
        );
        let from = outgoing.address();
        {
            let state = State::open(&path).unwrap();
            state
                .save_own_rotation(&record, &source, &target, &["https://own.example".into()])
                .unwrap();
            state
                .sync_own_rotation_targets(
                    &from,
                    &[
                        PublicationTarget::pending("https://own.example".into(), false, false),
                        PublicationTarget::pending("https://r1.example".into(), false, true),
                    ],
                )
                .unwrap();
            assert!(state
                .mark_rotation_target_complete(&from, "https://own.example")
                .unwrap());
        }

        let state = State::open(&path).unwrap();
        assert_eq!(state.rotation_target_progress(&from).unwrap(), (1, 1));
        let pending = state.pending_rotation_targets(8).unwrap();
        assert_eq!(pending.len(), 1);
        assert_eq!(pending[0].0.record, record);
        assert_eq!(pending[0].1.url, "https://r1.example");
        assert!(state.placement_state().unwrap().degraded());
    }

    #[test]
    fn own_rotation_history_is_capped_and_expiry_prunes_only_local_control_state() {
        let state = State::in_memory().unwrap();
        let base = 1_000_000;
        let mut bundles = Vec::new();
        for index in 1..=crate::keystore::MAX_LIVE_RETIRED_IDENTITIES as u8 {
            let bundle = sample_rotation(index, base + index as u64);
            state
                .save_own_rotation(
                    &bundle.1,
                    &bundle.2,
                    &bundle.3,
                    &["https://own.example".into()],
                )
                .unwrap();
            bundles.push(bundle);
        }
        assert_eq!(
            state.own_rotations().unwrap().len(),
            crate::keystore::MAX_LIVE_RETIRED_IDENTITIES
        );
        let extra = sample_rotation(33, base + 100);
        assert!(matches!(
            state.save_own_rotation(
                &extra.1,
                &extra.2,
                &extra.3,
                &["https://own.example".into()]
            ),
            Err(ClientError::Config(_))
        ));

        let first = &bundles[0];
        let active = Identity::from_seed([0xFE; 32]).address();
        state
            .set_cursor("https://own.example", &first.0, 7)
            .unwrap();
        state.set_cursor("https://own.example", &active, 9).unwrap();
        state
            .sync_own_rotation_targets(
                &first.0,
                &[PublicationTarget::pending(
                    "https://retry.example".into(),
                    false,
                    true,
                )],
            )
            .unwrap();
        let remote = sample_rotation(40, base + 200);
        state
            .save_rotation(&remote.0, &remote.1, remote.1.activated_at)
            .unwrap();

        assert_eq!(
            state
                .prune_expired_own_rotations(
                    first.1.grace_until,
                    crate::keystore::MAX_LIVE_RETIRED_IDENTITIES,
                )
                .unwrap(),
            1
        );
        assert!(state.own_rotation(&first.0).unwrap().is_none());
        assert!(state.rotation(&first.0).unwrap().is_none());
        assert!(state.rotation(&remote.0).unwrap().is_some());
        assert_eq!(state.cursor("https://own.example", &first.0).unwrap(), 0);
        assert_eq!(state.cursor("https://own.example", &active).unwrap(), 9);
        let first_targets: i64 = state
            .conn
            .query_row(
                "SELECT COUNT(*) FROM own_rotation_publication_targets WHERE from_addr = ?1",
                params![first.0.as_str()],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(first_targets, 0);

        // The freed grace slot admits exactly one later transition.
        state
            .save_own_rotation(
                &extra.1,
                &extra.2,
                &extra.3,
                &["https://own.example".into()],
            )
            .unwrap();
        assert_eq!(
            state.own_rotations().unwrap().len(),
            crate::keystore::MAX_LIVE_RETIRED_IDENTITIES
        );
    }

    #[test]
    fn policy_sequence_allocation_is_atomic_across_connections() {
        let (_dir, path) = private_state_path("state.db");
        let path = std::sync::Arc::new(path);
        State::open(&path).unwrap();

        let mut workers = Vec::new();
        for _ in 0..4 {
            let path = path.clone();
            workers.push(std::thread::spawn(move || {
                let state = State::open(&path).unwrap();
                (0..25)
                    .map(|_| state.next_policy_seq().unwrap())
                    .collect::<Vec<_>>()
            }));
        }
        let mut allocated: Vec<u64> = workers
            .into_iter()
            .flat_map(|worker| worker.join().unwrap())
            .collect();
        allocated.sort_unstable();
        assert_eq!(allocated, (1..=100).collect::<Vec<_>>());
    }

    #[test]
    fn a_pinned_successor_commitment_is_immutable() {
        // Trust-on-first-use is only trust-on-*first*-use if later writes cannot quietly move the
        // pin. Everything else about a resolution may be refreshed; this may not.
        let state = State::in_memory().unwrap();
        let addr = Identity::from_seed([9; 32]).address();

        let original = Resolution {
            pubkey: [3; 32],
            successor_hash: [4; 32],
            seq: 1,
            lofts: vec!["wss://a".into()],
            pow_min: 0,
            attribution_requirement: None,
        };
        state.save_resolution(&addr, &original, 100).unwrap();

        let attempted = Resolution {
            successor_hash: [0xEE; 32],
            seq: 2,
            lofts: vec!["wss://attacker".into()],
            ..original
        };
        assert!(state.save_resolution(&addr, &attempted, 200).is_err());

        let stored = state.resolution(&addr).unwrap().unwrap();
        assert_eq!(stored.successor_hash, [4; 32], "the pin must not move");
        assert_eq!(stored.seq, 1, "a hostile update changes nothing");
    }

    #[test]
    fn resolutions_round_trip() {
        let state = State::in_memory().unwrap();
        let addr = Identity::from_seed([9; 32]).address();
        let resolution = Resolution {
            pubkey: [3; 32],
            successor_hash: [4; 32],
            seq: 7,
            lofts: vec!["wss://a".into(), "wss://b".into()],
            pow_min: 0,
            attribution_requirement: None,
        };

        state.save_resolution(&addr, &resolution, 100).unwrap();
        let loaded = state.resolution(&addr).unwrap().unwrap();
        assert_eq!(loaded.pubkey, [3; 32]);
        assert_eq!(loaded.seq, 7);
        assert_eq!(loaded.lofts.len(), 2);
    }

    #[test]
    fn current_open_keeps_constant_work_and_resolution_access_rejects_noncanonical_attribution() {
        let requirement = AttributionRequirement::new(Jurisdiction::Eu, [0xA5; 32]);
        let canonical = requirement.encode().unwrap();
        let mut unknown_version = canonical;
        unknown_version[0] = 0xff;
        let mut unknown_jurisdiction = canonical;
        unknown_jurisdiction[1] = 0x04;
        let mut zero_authority = canonical;
        zero_authority[2..].fill(0);

        for (label, malformed) in [
            ("version", unknown_version),
            ("jurisdiction", unknown_jurisdiction),
            ("authority", zero_authority),
        ] {
            let (_dir, path) = private_state_path(format!("state-{label}.db"));
            let address = Identity::from_seed([0x91; 32]).address();
            let state = State::open(&path).unwrap();
            state
                .save_resolution(
                    &address,
                    &Resolution {
                        pubkey: [3; 32],
                        successor_hash: [4; 32],
                        seq: 1,
                        lofts: vec!["https://loft.example".into()],
                        pow_min: 0,
                        attribution_requirement: Some(requirement),
                    },
                    100,
                )
                .unwrap();
            assert_eq!(
                state
                    .conn
                    .execute(
                        "UPDATE resolutions SET attribution_requirement = ?1 WHERE addr = ?2",
                        params![malformed.as_slice(), address.as_str()],
                    )
                    .unwrap(),
                1
            );
            assert!(AttributionRequirement::decode(&malformed).is_err());
            drop(state);

            let reopened = State::open(&path).unwrap();
            assert!(reopened.resolution(&address).is_err());
        }
    }

    #[test]
    fn unsigned_values_outside_sqlite_integer_range_never_mutate_state() {
        let (_dir, path) = private_state_path("state.db");
        let address = Identity::from_seed([0xA1; 32]).address();
        let state = State::open(&path).unwrap();

        assert!(state
            .set_cursor("https://loft.example", &address, u64::MAX)
            .is_err());
        let resolution = Resolution {
            pubkey: [3; 32],
            successor_hash: [4; 32],
            seq: u64::MAX,
            lofts: vec!["https://loft.example".into()],
            pow_min: 0,
            attribution_requirement: None,
        };
        assert!(state.save_resolution(&address, &resolution, 100).is_err());
        assert_eq!(state.cursor("https://loft.example", &address).unwrap(), 0);
        assert!(state.resolution(&address).unwrap().is_none());
        drop(state);

        let reopened = State::open(&path).unwrap();
        assert_eq!(
            reopened.cursor("https://loft.example", &address).unwrap(),
            0
        );
        assert!(reopened.resolution(&address).unwrap().is_none());
    }

    #[test]
    fn registry_trust_is_immutable_and_checkpoint_pin_never_rewinds() {
        let state = State::in_memory().unwrap();
        let operator = SigningKey::from_bytes(&[41; 32]);
        let witness = SigningKey::from_bytes(&[43; 32]);
        let minimum = CheckpointPin {
            size: 0,
            root: MerkleLog::new().root(),
        };
        let trust = RegistryTrust::new(
            "registry.test/log",
            operator.verifying_key().to_bytes(),
            vec![WitnessKey::new("witness.test", witness.verifying_key()).unwrap()],
            1,
            minimum,
            60,
            5,
        )
        .unwrap();
        state
            .configure_registry("https://registry.test", &trust, 10)
            .unwrap();
        state
            .configure_registry("https://registry.test", &trust, 11)
            .unwrap();

        let replacement = RegistryTrust::new(
            "registry.test/log",
            SigningKey::from_bytes(&[42; 32]).verifying_key().to_bytes(),
            vec![WitnessKey::new("witness.test", witness.verifying_key()).unwrap()],
            1,
            minimum,
            60,
            5,
        )
        .unwrap();
        assert!(state
            .configure_registry("https://registry.test", &replacement, 12)
            .is_err());

        let checkpoint = Checkpoint {
            origin: "registry.test/log".into(),
            size: 7,
            root: [7; 32],
        };
        state
            .save_registry_checkpoint(&checkpoint, Some(100))
            .unwrap();
        let loaded = state.registry_configuration().unwrap().unwrap();
        assert_eq!(loaded.checkpoint, Some(checkpoint.clone()));

        let equivocation = Checkpoint {
            root: [8; 32],
            ..checkpoint
        };
        assert!(state
            .save_registry_checkpoint(&equivocation, Some(101))
            .is_err());

        state.reset_registry().unwrap();
        assert!(state.registry_configuration().unwrap().is_none());
    }

    #[test]
    fn handle_pubkey_lookup_uses_the_index_with_work_independent_of_projection_size() {
        use std::sync::atomic::{AtomicUsize, Ordering};
        use std::sync::Arc;

        let state = State::in_memory().unwrap();
        let tx = state.conn.unchecked_transaction().unwrap();
        for index in 0..16_000u64 {
            let mut pubkey = [0u8; 32];
            pubkey[..8].copy_from_slice(&index.to_be_bytes());
            tx.execute(
                "INSERT INTO registry_handle_projection (handle, pubkey, subject, log_index)
                 VALUES (?1, ?2, ?3, 0)",
                params![
                    format!("/github/h{index:05}"),
                    pubkey.as_slice(),
                    format!("github:subject-{index}"),
                ],
            )
            .unwrap();
        }
        tx.commit().unwrap();

        let plan: Vec<String> = state
            .conn
            .prepare(
                "EXPLAIN QUERY PLAN
                 SELECT handle, pubkey, subject, log_index
                 FROM registry_handle_projection WHERE pubkey = ?1 LIMIT 1",
            )
            .unwrap()
            .query_map(params![[0u8; 32].as_slice()], |row| row.get::<_, String>(3))
            .unwrap()
            .collect::<std::result::Result<_, _>>()
            .unwrap();
        assert!(
            plan.iter().any(|detail| {
                detail.contains("registry_handle_projection_pubkey") && detail.contains("SEARCH")
            }),
            "unexpected query plan: {plan:?}"
        );

        let operations = Arc::new(AtomicUsize::new(0));
        let observed = Arc::clone(&operations);
        state.conn.progress_handler(
            1,
            Some(move || {
                observed.fetch_add(1, Ordering::Relaxed);
                false
            }),
        );
        for key in [[0u8; 32], [0xFFu8; 32]] {
            operations.store(0, Ordering::Relaxed);
            let _: Option<String> = state
                .conn
                .query_row(
                    "SELECT handle FROM registry_handle_projection
                     WHERE pubkey = ?1 LIMIT 1",
                    params![key.as_slice()],
                    |row| row.get(0),
                )
                .optional()
                .unwrap();
            assert!(
                operations.load(Ordering::Relaxed) < 64,
                "indexed handle lookup exceeded its fixed VM-operation budget"
            );
        }
        state.conn.progress_handler(0, None::<fn() -> bool>);
    }

    #[test]
    fn legacy_schema_is_migrated_without_losing_outbox_rows() {
        let (_dir, path) = private_state_path("state.db");
        let conn = Connection::open(&path).unwrap();
        conn.execute_batch(V0_1_0_SCHEMA).unwrap();
        let encoded_wrap = serde_json::to_vec(&sample_wrap()).unwrap();
        conn.execute(
            "INSERT INTO outbox
                 (message_id, to_addr, loft_url, wrap, created_at)
             VALUES ('legacy', '/k/x', 'wss://a', ?1, 123)",
            params![encoded_wrap],
        )
        .unwrap();
        conn.execute(
            "INSERT INTO lofts (url, added_at) VALUES ('wss://a', 123)",
            [],
        )
        .unwrap();
        drop(conn);
        make_state_private(&path);

        let state = State::open(&path).unwrap();
        assert_eq!(state.pending_count().unwrap(), 1);
        let version: i64 = state
            .conn
            .pragma_query_value(None, "user_version", |row| row.get(0))
            .unwrap();
        assert_eq!(version, SCHEMA_VERSION);

        for (table, column) in [
            ("lofts", "role"),
            ("lofts", "drain_after"),
            ("lofts", "allow_local"),
            ("outbox", "token"),
            ("outbox", "next_attempt_at"),
            ("outbox", "allow_local"),
            ("messages", "attribution"),
        ] {
            let mut stmt = state
                .conn
                .prepare(&format!("PRAGMA table_info({table})"))
                .unwrap();
            let names: Vec<String> = stmt
                .query_map([], |row| row.get(1))
                .unwrap()
                .collect::<std::result::Result<_, _>>()
                .unwrap();
            assert!(
                names.iter().any(|name| name == column),
                "missing {table}.{column}"
            );
        }
        let migrated_loft_trust: i64 = state
            .conn
            .query_row(
                "SELECT allow_local FROM lofts WHERE url = 'wss://a'",
                [],
                |row| row.get(0),
            )
            .unwrap();
        let migrated_outbox_trust: i64 = state
            .conn
            .query_row(
                "SELECT allow_local FROM outbox WHERE message_id = 'legacy'",
                [],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(migrated_loft_trust, 0);
        assert_eq!(migrated_outbox_trust, 0);
        state
            .add_directory("https://directory.example", &[7; 32], 123)
            .unwrap();
        assert_eq!(state.directories().unwrap().len(), 1);
        assert!(state
            .add_directory("https://directory.example", &[8; 32], 124)
            .is_err());
    }

    #[test]
    fn pristine_database_is_distinct_from_the_exact_release_schema() {
        let (_dir, path) = private_state_path("state.db");
        Connection::open(&path).unwrap();
        make_state_private(&path);

        let state = State::open(&path).unwrap();
        let version: i64 = state
            .conn
            .pragma_query_value(None, "user_version", |row| row.get(0))
            .unwrap();
        assert_eq!(version, SCHEMA_VERSION);
        validate_client_schema(&state.conn, SCHEMA_VERSION).unwrap();
    }

    #[test]
    fn storage_limit_updates_are_bounded_and_never_undercut_usage() {
        let state = State::in_memory().unwrap();
        state
            .store_message(
                "one",
                &[1; 32],
                "/k/x",
                1,
                &UntrustedBody::new("body"),
                "accepted",
            )
            .unwrap();
        let before = state.storage_status().unwrap();
        assert!(state
            .set_storage_limits(StorageLimits {
                inbox_messages: 0,
                ..before.limits
            })
            .is_err());
        assert!(state
            .set_storage_limits(StorageLimits {
                inbox_messages: MAX_INBOX_MESSAGE_LIMIT + 1,
                ..before.limits
            })
            .is_err());
        assert!(state
            .set_storage_limits(StorageLimits {
                inbox_body_bytes: 3,
                ..before.limits
            })
            .is_err());
        assert_eq!(state.storage_status().unwrap(), before);
    }

    #[test]
    fn v12_migration_accounts_exact_usage_and_erases_finished_payloads() {
        let (_dir, path) = private_state_path("state.db");
        let mut conn = Connection::open(&path).unwrap();
        create_state_schema(&mut conn, 12);
        let encoded = serde_json::to_vec(&sample_wrap()).unwrap();
        conn.execute(
            "INSERT INTO messages
                 (id, from_pubkey, from_address, received_at, body, state, attribution)
             VALUES ('inbound', ?1, '/k/x', 1, 'hello', 'accepted', 'absent')",
            params![[1u8; 32].as_slice()],
        )
        .unwrap();
        conn.execute(
            "INSERT INTO outbox
                 (message_id, to_addr, loft_url, wrap, created_at, next_attempt_at)
             VALUES ('active', '/k/x', 'https://active.example', ?1, 1, 1)",
            params![&encoded],
        )
        .unwrap();
        conn.execute(
            "INSERT INTO outbox
                 (message_id, to_addr, loft_url, wrap, token, created_at,
                  next_attempt_at, sent_at)
             VALUES ('sent', '/k/x', 'https://sent.example', ?1,
                     '0000000000000000000000000000000000000000000000000000000000000000',
                     1, 1, 2)",
            params![&encoded],
        )
        .unwrap();
        drop(conn);
        make_state_private(&path);

        let state = State::open(&path).unwrap();
        let status = state.storage_status().unwrap();
        assert_eq!(
            status.usage,
            StorageUsage {
                inbox_messages: 1,
                inbox_tombstones: 0,
                inbox_body_bytes: 5,
                outbox_rows: 2,
                outbox_payload_bytes: encoded.len() as u64,
            }
        );
        assert_eq!(status.limits, StorageLimits::default());
        let sent_payload: (i64, Option<String>) = state
            .conn
            .query_row(
                "SELECT length(wrap), token FROM outbox WHERE message_id = 'sent'",
                [],
                |row| Ok((row.get(0)?, row.get(1)?)),
            )
            .unwrap();
        assert_eq!(sent_payload, (0, None));
    }

    #[test]
    fn v13_migration_failure_rolls_back_data_and_schema_version() {
        let (_dir, path) = private_state_path("state.db");
        let mut conn = Connection::open(&path).unwrap();
        create_state_schema(&mut conn, 12);
        conn.execute(
            "INSERT INTO messages
                 (id, from_pubkey, from_address, received_at, body, state, attribution)
             VALUES ('oversized', ?1, '/k/x', 1, ?2, 'accepted', 'absent')",
            params![[1u8; 32].as_slice(), "x".repeat(65_537)],
        )
        .unwrap();
        drop(conn);
        make_state_private(&path);

        assert_non_custody_config_error(State::open(&path));
        let conn = Connection::open(&path).unwrap();
        let version: i64 = conn
            .pragma_query_value(None, "user_version", |row| row.get(0))
            .unwrap();
        assert_eq!(version, 12);
        let accounting_exists: bool = conn
            .query_row(
                "SELECT EXISTS(SELECT 1 FROM sqlite_schema
                               WHERE type = 'table' AND name = 'storage_accounting')",
                [],
                |row| row.get(0),
            )
            .unwrap();
        assert!(!accounting_exists);
        assert_eq!(
            conn.query_row("SELECT COUNT(*) FROM messages", [], |row| row
                .get::<_, i64>(0))
                .unwrap(),
            1
        );
    }

    #[test]
    fn v14_rejects_noncanonical_legacy_handle_projection_and_rolls_back() {
        let (_dir, path) = private_state_path("state.db");
        let mut conn = Connection::open(&path).unwrap();
        create_state_schema(&mut conn, 13);
        let root = vec![0u8; 32];
        conn.execute(
            "INSERT INTO registry_config
                 (id, url, origin, checkpoint_key, witness_threshold, minimum_size,
                  minimum_root, max_cosignature_age, future_clock_skew, added_at)
             VALUES (1, 'https://registry.test', 'registry.test/log', ?1, 1, 0, ?2, 60, 5, 0)",
            params![[0x11u8; 32].as_slice(), MerkleLog::new().root().as_slice()],
        )
        .unwrap();
        conn.execute(
            "INSERT INTO registry_pin (id, size, root, witnessed_at) VALUES (1, 1, ?1, 1)",
            params![root.as_slice()],
        )
        .unwrap();
        let audit = serde_json::json!({
            "origin": "registry.test/log",
            "size": 1,
            "root": root,
            "checkpoint_note": "legacy-note",
            "witnessed_at": 1,
            "frontier": { "size": 1, "peaks": [vec![0u8; 32]] }
        });
        conn.execute(
            "INSERT INTO registry_handle_audit (id, state) VALUES (1, ?1)",
            params![serde_json::to_vec(&audit).unwrap()],
        )
        .unwrap();
        conn.execute(
            "INSERT INTO registry_handle_projection (handle, pubkey, subject, log_index)
             VALUES ('/github/UPPER', ?1, 'github:subject', 0)",
            params![[0xA1u8; 32].as_slice()],
        )
        .unwrap();
        drop(conn);
        make_state_private(&path);

        assert_non_custody_config_error(State::open(&path));
        let conn = Connection::open(&path).unwrap();
        let version: i64 = conn
            .pragma_query_value(None, "user_version", |row| row.get(0))
            .unwrap();
        assert_eq!(version, 13);
        let deleted_at: i64 = conn
            .query_row(
                "SELECT COUNT(*) FROM pragma_table_info('messages') WHERE name = 'deleted_at'",
                [],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(deleted_at, 0);
    }

    #[test]
    fn current_open_rejects_a_malformed_accounting_singleton_in_constant_work() {
        let (_dir, path) = private_state_path("state.db");
        let state = State::open(&path).unwrap();
        state
            .conn
            .pragma_update(None, "ignore_check_constraints", true)
            .unwrap();
        state
            .conn
            .execute("UPDATE storage_accounting SET inbox_message_limit = 0", [])
            .unwrap();
        drop(state);

        assert!(matches!(State::open(&path), Err(ClientError::Config(_))));
    }

    #[test]
    fn current_open_does_not_walk_payload_rows_and_validation_occurs_on_access() {
        let (_dir, path) = private_state_path("state.db");
        let state = State::open(&path).unwrap();
        for (id, url) in [
            ("first", "https://first.example"),
            ("corrupt", "https://corrupt.example"),
        ] {
            state
                .queue(
                    id,
                    "/k/x",
                    OutboxRoute::new(url, false),
                    &sample_wrap(),
                    None,
                    1,
                )
                .unwrap();
        }
        let old_bytes: i64 = state
            .conn
            .query_row(
                "SELECT length(wrap) FROM outbox WHERE message_id = 'corrupt'",
                [],
                |row| row.get(0),
            )
            .unwrap();
        state
            .conn
            .execute(
                "UPDATE outbox SET wrap = X'00' WHERE message_id = 'corrupt'",
                [],
            )
            .unwrap();
        state
            .conn
            .execute(
                "UPDATE storage_accounting
                 SET outbox_payload_bytes = outbox_payload_bytes - ?1 + 1",
                params![old_bytes],
            )
            .unwrap();
        drop(state);

        let reopened = State::open(&path).unwrap();
        assert_eq!(reopened.pending(1, 1).unwrap()[0].message_id, "first");
        assert!(matches!(
            reopened.pending(10, 1),
            Err(ClientError::Serialization(_))
        ));
    }

    #[test]
    fn partial_and_unknown_unversioned_schemas_are_refused_untouched() {
        for (ddl, table) in [
            (
                "CREATE TABLE meta (key TEXT PRIMARY KEY, value TEXT NOT NULL);
                 INSERT INTO meta VALUES ('sentinel', 'partial');",
                "meta",
            ),
            (
                "CREATE TABLE operator_data (id INTEGER PRIMARY KEY, value TEXT NOT NULL);
                 INSERT INTO operator_data (value) VALUES ('unknown');",
                "operator_data",
            ),
        ] {
            let (_dir, path) = private_state_path("state.db");
            let conn = Connection::open(&path).unwrap();
            conn.execute_batch(ddl).unwrap();
            drop(conn);
            make_state_private(&path);

            assert_non_custody_config_error(State::open(&path));
            let conn = Connection::open(&path).unwrap();
            let version: i64 = conn
                .pragma_query_value(None, "user_version", |row| row.get(0))
                .unwrap();
            assert_eq!(version, 0);
            let application_objects: i64 = conn
                .query_row(
                    "SELECT COUNT(*) FROM sqlite_schema
                     WHERE type = 'table' AND name NOT LIKE 'sqlite_%'",
                    [],
                    |row| row.get(0),
                )
                .unwrap();
            assert_eq!(application_objects, 1);
            let sentinel_rows: i64 = conn
                .query_row(&format!("SELECT COUNT(*) FROM {table}"), [], |row| {
                    row.get(0)
                })
                .unwrap();
            assert_eq!(sentinel_rows, 1);
        }
    }

    #[test]
    fn every_known_client_schema_migrates_step_by_step() {
        for version in 1..=SCHEMA_VERSION {
            let (_dir, path) = private_state_path(format!("state-v{version}.db"));
            let mut conn = Connection::open(&path).unwrap();
            create_state_schema(&mut conn, version);
            drop(conn);
            make_state_private(&path);

            let state = State::open(&path).unwrap();
            let migrated: i64 = state
                .conn
                .pragma_query_value(None, "user_version", |row| row.get(0))
                .unwrap();
            assert_eq!(migrated, SCHEMA_VERSION, "failed from schema {version}");
            validate_client_schema(&state.conn, SCHEMA_VERSION).unwrap();
        }
    }

    #[test]
    fn v9_terminal_outbox_migration_preserves_rows_and_detects_later_split_state_on_access() {
        let (_dir, path) = private_state_path("state.db");
        let mut conn = Connection::open(&path).unwrap();
        create_state_schema(&mut conn, 8);
        let encoded_wrap = serde_json::to_vec(&sample_wrap()).unwrap();
        conn.execute(
            "INSERT INTO outbox
                 (message_id, to_addr, loft_url, wrap, created_at)
             VALUES ('queued', '/k/x', 'wss://a', ?1, 123)",
            params![encoded_wrap],
        )
        .unwrap();
        drop(conn);
        make_state_private(&path);

        let state = State::open(&path).unwrap();
        let terminal: (Option<i64>, Option<String>) = state
            .conn
            .query_row(
                "SELECT terminal_at, terminal_reason FROM outbox
                 WHERE message_id = 'queued'",
                [],
                |row| Ok((row.get(0)?, row.get(1)?)),
            )
            .unwrap();
        assert_eq!(terminal, (None, None));
        state
            .conn
            .execute(
                "UPDATE outbox SET terminal_at = 456 WHERE message_id = 'queued'",
                [],
            )
            .unwrap();
        drop(state);

        let reopened = State::open(&path).unwrap();
        assert!(reopened.dead_letters(10).is_err());
        drop(reopened);
        let conn = Connection::open(&path).unwrap();
        let version: i64 = conn
            .pragma_query_value(None, "user_version", |row| row.get(0))
            .unwrap();
        let terminal_at: i64 = conn
            .query_row(
                "SELECT terminal_at FROM outbox WHERE message_id = 'queued'",
                [],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(version, SCHEMA_VERSION);
        assert_eq!(terminal_at, 456);
    }

    #[test]
    fn malformed_versioned_schema_and_future_schema_are_untouched() {
        for future in [false, true] {
            let (_dir, path) = private_state_path("state.db");
            let mut conn = Connection::open(&path).unwrap();
            create_state_schema(&mut conn, SCHEMA_VERSION);
            conn.execute(
                "INSERT INTO meta (key, value) VALUES ('sentinel', 'kept')",
                [],
            )
            .unwrap();
            if future {
                conn.pragma_update(None, "user_version", SCHEMA_VERSION + 1)
                    .unwrap();
            } else {
                conn.execute_batch("ALTER TABLE meta ADD COLUMN rogue TEXT;")
                    .unwrap();
            }
            drop(conn);
            make_state_private(&path);

            assert_non_custody_config_error(State::open(&path));
            let conn = Connection::open(&path).unwrap();
            let version: i64 = conn
                .pragma_query_value(None, "user_version", |row| row.get(0))
                .unwrap();
            assert_eq!(
                version,
                if future {
                    SCHEMA_VERSION + 1
                } else {
                    SCHEMA_VERSION
                }
            );
            let value: String = conn
                .query_row("SELECT value FROM meta WHERE key = 'sentinel'", [], |row| {
                    row.get(0)
                })
                .unwrap();
            assert_eq!(value, "kept");
        }
    }

    #[test]
    fn missing_security_settings_keep_documented_defaults() {
        let state = State::in_memory().unwrap();
        assert_eq!(
            state.security_settings().unwrap(),
            SecuritySettings {
                accept_all: false,
                pow_floor: 0,
                token_labels: Vec::new(),
                token_gate_enabled: false,
                attribution_requirement: None,
            }
        );
        assert!(!state.accept_all().unwrap());
        assert_eq!(state.pow_floor().unwrap(), 0);
        assert!(state.token_labels().unwrap().is_empty());
        assert!(!state.token_gate_enabled().unwrap());
        assert!(!state.attribution_required().unwrap());
    }

    #[test]
    fn legacy_boolean_and_jurisdiction_only_attribution_settings_fail_closed_until_reconfigured() {
        let state = State::in_memory().unwrap();
        state.set_meta(ATTRIBUTION_REQUIRED_META, "true").unwrap();
        assert!(matches!(
            state.recipient_attribution_requirement(),
            Err(ClientError::Config(_))
        ));
        let requirement = AttributionRequirement::new(Jurisdiction::Test, [0xB6; 32]);
        state
            .set_recipient_attribution_requirement(Some(requirement))
            .unwrap();
        assert_eq!(
            state.recipient_attribution_requirement().unwrap(),
            Some(requirement)
        );

        state
            .set_meta(ATTRIBUTION_JURISDICTION_META, "\"test\"")
            .unwrap();
        assert!(matches!(
            state.sender_attribution_requirement(),
            Err(ClientError::Config(_))
        ));
        state
            .set_sender_attribution_requirement(Some(requirement))
            .unwrap();
        assert_eq!(
            state.sender_attribution_requirement().unwrap(),
            Some(requirement)
        );
    }

    #[test]
    fn every_security_setting_accessor_rejects_present_corruption() {
        let state = State::in_memory().unwrap();
        let corruptions = [
            (ACCEPT_ALL_META, "TRUE", "false"),
            (POW_FLOOR_META, "01", "0"),
            (TOKEN_LABELS_META, "[\"same\",\"same\"]", "[]"),
            (TOKEN_GATE_ENABLED_META, "1", "false"),
            (ATTRIBUTION_REQUIRED_META, "yes", "false"),
        ];

        for (key, malformed, restored) in corruptions {
            state.set_meta(key, malformed).unwrap();
            let result = match key {
                ACCEPT_ALL_META => state.accept_all().map(|_| ()),
                POW_FLOOR_META => state.pow_floor().map(|_| ()),
                TOKEN_LABELS_META => state.token_labels().map(|_| ()),
                TOKEN_GATE_ENABLED_META => state.token_gate_enabled().map(|_| ()),
                ATTRIBUTION_REQUIRED_META => state.attribution_required().map(|_| ()),
                _ => unreachable!(),
            };
            assert!(
                matches!(result, Err(ClientError::Config(_))),
                "{key} must fail closed"
            );
            assert!(matches!(
                state.security_settings(),
                Err(ClientError::Config(_))
            ));
            assert_eq!(state.get_meta(key).unwrap().as_deref(), Some(malformed));
            state.set_meta(key, restored).unwrap();
        }
    }

    #[test]
    fn security_setting_parsers_enforce_bounds_canonical_text_and_unique_labels() {
        for malformed in ["00", "01", "+1", " 1", "19", "256", "4294967296"] {
            assert!(matches!(
                parse_pow_floor_meta(Some(malformed)),
                Err(ClientError::Config(_))
            ));
        }
        assert_eq!(
            parse_pow_floor_meta(Some("18")).unwrap(),
            Some(crate::spam::MAX_SUPPORTED_POW_BITS)
        );

        let too_many = serde_json::to_string(
            &(0..=pigeonpost_core::policy::MAX_TOKENS)
                .map(|index| format!("label-{index}"))
                .collect::<Vec<_>>(),
        )
        .unwrap();
        for malformed in [
            "{}".to_string(),
            "[\"same\",\"same\"]".to_string(),
            "[\"\"]".to_string(),
            serde_json::to_string(&vec!["x".repeat(MAX_TOKEN_LABEL_BYTES + 1)]).unwrap(),
            too_many,
        ] {
            assert!(matches!(
                parse_token_labels_meta(Some(&malformed)),
                Err(ClientError::Config(_))
            ));
        }
    }

    #[test]
    fn concurrent_score_penalties_are_atomic_and_reach_the_drop_threshold() {
        use std::sync::{Arc, Barrier};

        let (_dir, path) = private_state_path("state.db");
        drop(State::open(&path).unwrap());
        let path = Arc::new(path);
        let barrier = Arc::new(Barrier::new(9));
        let pubkey = [0xD4; 32];
        let now = 50_000;
        let mut workers = Vec::new();
        for _ in 0..8 {
            let path = Arc::clone(&path);
            let barrier = Arc::clone(&barrier);
            workers.push(std::thread::spawn(move || {
                let state = State::open(&path).unwrap();
                barrier.wait();
                state.adjust_score(&pubkey, -10, now)
            }));
        }
        barrier.wait();
        for worker in workers {
            worker.join().unwrap().unwrap();
        }

        let state = State::open(&path).unwrap();
        assert_eq!(state.score(&pubkey).unwrap(), (-80, now));
        assert_eq!(
            crate::spam::decide(
                &crate::spam::SenderContext {
                    allowlisted: false,
                    raw_score: -80,
                    score_updated_at: now,
                    has_handle: false,
                },
                false,
                now,
            ),
            crate::spam::Disposition::Drop
        );
    }

    #[test]
    fn concurrent_allow_is_one_bonus_and_one_atomic_pending_release() {
        use std::sync::{Arc, Barrier};

        let (_dir, path) = private_state_path("state.db");
        let path = Arc::new(path);
        let sender = [0xD5; 32];
        {
            let state = State::open(&path).unwrap();
            let body = UntrustedBody::new("held");
            for index in 0..4 {
                state
                    .store_message(
                        &format!("held-{index}"),
                        &sender,
                        "/k/sender",
                        10 + index,
                        &body,
                        "pending",
                    )
                    .unwrap();
            }
            state.adjust_score(&sender, -500, 100).unwrap();
        }

        let barrier = Arc::new(Barrier::new(9));
        let mut workers = Vec::new();
        for _ in 0..8 {
            let path = Arc::clone(&path);
            let barrier = Arc::clone(&barrier);
            workers.push(std::thread::spawn(move || {
                let state = State::open(&path).unwrap();
                barrier.wait();
                state.allow_sender(&sender, "reviewed", 200)
            }));
        }
        barrier.wait();
        let released: usize = workers
            .into_iter()
            .map(|worker| worker.join().unwrap().unwrap())
            .sum();

        let state = State::open(&path).unwrap();
        assert_eq!(released, 4);
        assert!(state.is_allowed(&sender).unwrap());
        assert_eq!(state.score(&sender).unwrap(), (100, 200));
        assert!(state.pending_messages(10).unwrap().is_empty());
        assert_eq!(state.messages(false, 10).unwrap().len(), 4);
        assert_eq!(state.allow_sender(&sender, "retry", 200).unwrap(), 0);
        assert_eq!(state.score(&sender).unwrap(), (100, 200));
    }

    #[test]
    fn concurrent_correspondence_queue_grants_one_bonus() {
        use std::sync::{Arc, Barrier};

        let (_dir, path) = private_state_path("state.db");
        let path = Arc::new(path);
        drop(State::open(&path).unwrap());
        let correspondent = [0xD6; 32];
        let wrap = Arc::new(sample_wrap());
        let barrier = Arc::new(Barrier::new(9));
        let mut workers = Vec::new();
        for _ in 0..8 {
            let path = Arc::clone(&path);
            let barrier = Arc::clone(&barrier);
            let wrap = Arc::clone(&wrap);
            workers.push(std::thread::spawn(move || {
                let state = State::open(&path).unwrap();
                barrier.wait();
                state.queue_correspondence(
                    "same-outbound",
                    "/k/correspondent",
                    &[OutboxRoute::new("https://loft.example", false)],
                    &wrap,
                    None,
                    &correspondent,
                    300,
                )
            }));
        }
        barrier.wait();
        for worker in workers {
            worker.join().unwrap().unwrap();
        }

        let state = State::open(&path).unwrap();
        assert_eq!(state.pending_count().unwrap(), 1);
        assert!(state.is_allowed(&correspondent).unwrap());
        assert_eq!(state.score(&correspondent).unwrap(), (100, 300));
    }

    #[test]
    fn concurrent_block_is_an_idempotent_score_ceiling() {
        use std::sync::{Arc, Barrier};

        let (_dir, path) = private_state_path("state.db");
        let path = Arc::new(path);
        let sender = [0xD7; 32];
        {
            let state = State::open(&path).unwrap();
            state.allow(&sender, "known", 100).unwrap();
            state.adjust_score(&sender, 500, 100).unwrap();
        }

        let barrier = Arc::new(Barrier::new(9));
        let mut workers = Vec::new();
        for _ in 0..8 {
            let path = Arc::clone(&path);
            let barrier = Arc::clone(&barrier);
            workers.push(std::thread::spawn(move || {
                let state = State::open(&path).unwrap();
                barrier.wait();
                state.block_sender(&sender, 400)
            }));
        }
        barrier.wait();
        for worker in workers {
            assert_eq!(worker.join().unwrap().unwrap(), -80);
        }

        let state = State::open(&path).unwrap();
        assert!(!state.is_allowed(&sender).unwrap());
        assert_eq!(state.score(&sender).unwrap(), (-80, 400));
        assert_eq!(state.block_sender(&sender, 400).unwrap(), -80);
        assert_eq!(state.score(&sender).unwrap(), (-80, 400));
    }

    #[test]
    fn concurrent_spam_mark_penalizes_each_message_exactly_once() {
        use std::sync::{Arc, Barrier};

        let (_dir, path) = private_state_path("state.db");
        let path = Arc::new(path);
        let sender = [0xD8; 32];
        {
            let state = State::open(&path).unwrap();
            let body = UntrustedBody::new("flag me");
            for id in ["spam-one", "spam-two"] {
                state
                    .store_message(id, &sender, "/k/sender", 100, &body, "accepted")
                    .unwrap();
            }
            state.allow(&sender, "known", 100).unwrap();
        }

        let barrier = Arc::new(Barrier::new(9));
        let mut workers = Vec::new();
        for _ in 0..8 {
            let path = Arc::clone(&path);
            let barrier = Arc::clone(&barrier);
            workers.push(std::thread::spawn(move || {
                let state = State::open(&path).unwrap();
                barrier.wait();
                state.mark_spam("spam-one", &sender, 500)
            }));
        }
        barrier.wait();
        for worker in workers {
            assert_eq!(worker.join().unwrap().unwrap(), -40);
        }

        let state = State::open(&path).unwrap();
        assert!(!state.is_allowed(&sender).unwrap());
        assert_eq!(state.score(&sender).unwrap(), (-40, 500));
        assert_eq!(state.mark_spam("spam-one", &sender, 500).unwrap(), -40);
        assert_eq!(state.mark_spam("spam-two", &sender, 500).unwrap(), -80);
        assert_eq!(state.score(&sender).unwrap(), (-80, 500));
        let marked: i64 = state
            .conn
            .query_row("SELECT COUNT(*) FROM spam_marks", [], |row| row.get(0))
            .unwrap();
        assert_eq!(marked, 2);
    }

    #[test]
    fn policy_actions_roll_back_list_message_and_outbox_mutations_on_score_failure() {
        let state = State::in_memory().unwrap();
        let body = UntrustedBody::new("held");
        let sender = [0xD9; 32];
        state
            .store_message("held", &sender, "/k/sender", 100, &body, "pending")
            .unwrap();
        state
            .conn
            .execute(
                "INSERT INTO scores (pubkey, score, updated_at) VALUES (?1, 0, -1)",
                params![sender.as_slice()],
            )
            .unwrap();

        assert!(state.allow_sender(&sender, "reviewed", 600).is_err());
        assert!(!state.is_allowed(&sender).unwrap());
        assert_eq!(state.pending_messages(10).unwrap().len(), 1);

        state.allow(&sender, "known", 600).unwrap();
        state.set_message_state("held", "accepted").unwrap();
        assert!(state.block_sender(&sender, 600).is_err());
        assert!(state.is_allowed(&sender).unwrap());
        assert!(state.mark_spam("held", &sender, 600).is_err());
        assert!(state.is_allowed(&sender).unwrap());
        assert_eq!(
            state.message("held").unwrap().unwrap().state.as_str(),
            "accepted"
        );
        let marked: i64 = state
            .conn
            .query_row("SELECT COUNT(*) FROM spam_marks", [], |row| row.get(0))
            .unwrap();
        assert_eq!(marked, 0);

        let correspondent = [0xDA; 32];
        state
            .conn
            .execute(
                "INSERT INTO scores (pubkey, score, updated_at) VALUES (?1, 0, -1)",
                params![correspondent.as_slice()],
            )
            .unwrap();
        let wrap = sample_wrap();
        assert!(state
            .queue_correspondence(
                "rollback-outbound",
                "/k/correspondent",
                &[OutboxRoute::new("https://loft.example", false)],
                &wrap,
                None,
                &correspondent,
                600,
            )
            .is_err());
        assert_eq!(state.pending_count().unwrap(), 0);
        assert!(!state.is_allowed(&correspondent).unwrap());
    }

    #[test]
    fn opening_rejects_each_malformed_security_setting_without_rewriting_it() {
        for (key, malformed) in [
            (ACCEPT_ALL_META, "TRUE"),
            (POW_FLOOR_META, "257"),
            (TOKEN_LABELS_META, "[\"same\",\"same\"]"),
            (TOKEN_GATE_ENABLED_META, "enabled"),
            (ATTRIBUTION_REQUIRED_META, "required"),
        ] {
            let (_dir, path) = private_state_path("state.db");
            drop(State::open(&path).unwrap());
            let conn = Connection::open(&path).unwrap();
            conn.execute(
                "INSERT INTO meta (key, value) VALUES (?1, ?2)
                 ON CONFLICT(key) DO UPDATE SET value = excluded.value",
                params![key, malformed],
            )
            .unwrap();
            drop(conn);

            assert!(matches!(State::open(&path), Err(ClientError::Config(_))));
            let conn = Connection::open(&path).unwrap();
            let persisted: String = conn
                .query_row(
                    "SELECT value FROM meta WHERE key = ?1",
                    params![key],
                    |row| row.get(0),
                )
                .unwrap();
            assert_eq!(persisted, malformed, "{key} was rewritten on failure");
        }
    }

    #[test]
    fn malformed_persisted_loft_key_never_downgrades_to_unpinned_trust() {
        let state = State::in_memory().unwrap();
        state
            .add_loft("https://loft.example", Some([0xA5; 32]), 1)
            .unwrap();
        state
            .conn
            .execute(
                "UPDATE lofts SET pubkey = X'01' WHERE url = 'https://loft.example'",
                [],
            )
            .unwrap();

        assert!(matches!(state.lofts(), Err(ClientError::Config(_))));
        assert!(matches!(
            state.lofts_with_local_trust(),
            Err(ClientError::Config(_))
        ));
        assert!(matches!(
            state.lofts_for_drain_with_local_trust(2),
            Err(ClientError::Config(_))
        ));
    }

    #[test]
    fn malformed_non_null_loft_keys_are_rejected_on_access_without_rewriting_them() {
        let (_dir, path) = private_state_path("state.db");
        {
            let state = State::open(&path).unwrap();
            state
                .add_loft("https://loft.example", Some([0xA5; 32]), 1)
                .unwrap();
        }
        let conn = Connection::open(&path).unwrap();
        conn.execute(
            "UPDATE lofts SET pubkey = X'0102' WHERE url = 'https://loft.example'",
            [],
        )
        .unwrap();
        drop(conn);

        let reopened = State::open(&path).unwrap();
        assert!(matches!(reopened.lofts(), Err(ClientError::Config(_))));
        drop(reopened);
        let conn = Connection::open(&path).unwrap();
        let persisted: Vec<u8> = conn
            .query_row(
                "SELECT pubkey FROM lofts WHERE url = 'https://loft.example'",
                [],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(persisted, vec![1, 2]);
    }

    #[test]
    fn failed_v5_data_validation_rolls_back_schema_and_version() {
        let (_dir, path) = private_state_path("state.db");
        let mut conn = Connection::open(&path).unwrap();
        create_state_schema(&mut conn, 4);
        conn.execute(
            "INSERT INTO cursors (loft_url, cursor) VALUES ('wss://a', -1)",
            [],
        )
        .unwrap();
        drop(conn);
        make_state_private(&path);

        assert_non_custody_config_error(State::open(&path));
        let conn = Connection::open(&path).unwrap();
        let version: i64 = conn
            .pragma_query_value(None, "user_version", |row| row.get(0))
            .unwrap();
        assert_eq!(version, 4);
        assert!(!column_named(&conn, "cursors", "address"));
        let rotation_table: i64 = conn
            .query_row(
                "SELECT COUNT(*) FROM sqlite_schema WHERE name = 'rotation_chains'",
                [],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(rotation_table, 0);
        let cursor: i64 = conn
            .query_row(
                "SELECT cursor FROM cursors WHERE loft_url = 'wss://a'",
                [],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(cursor, -1);
    }

    fn column_named(conn: &Connection, table: &str, column: &str) -> bool {
        let mut statement = conn
            .prepare(&format!("PRAGMA table_info({table})"))
            .unwrap();
        let found = statement
            .query_map([], |row| row.get::<_, String>(1))
            .unwrap()
            .any(|name| name.unwrap() == column);
        found
    }

    #[test]
    fn v4_cursor_is_adopted_by_the_pre_rotation_address_only() {
        let (_dir, path) = private_state_path("state.db");
        let mut conn = Connection::open(&path).unwrap();
        create_state_schema(&mut conn, 4);
        conn.execute(
            "INSERT INTO cursors (loft_url, cursor) VALUES ('wss://a', 19)",
            [],
        )
        .unwrap();
        drop(conn);
        make_state_private(&path);

        let state = State::open(&path).unwrap();
        let operating = Identity::from_seed([0x81; 32]).address();
        let successor = Identity::from_seed([0x82; 32]).address();
        state.adopt_legacy_cursors(&operating).unwrap();
        assert_eq!(state.cursor("wss://a", &operating).unwrap(), 19);
        assert_eq!(state.cursor("wss://a", &successor).unwrap(), 0);
    }

    #[test]
    fn verified_rotation_state_is_immutable_and_round_trips() {
        let state = State::in_memory().unwrap();
        let outgoing = Identity::from_seed([0x91; 32]);
        let incoming = Identity::from_seed([0x92; 32]);
        let next = Identity::from_seed([0x93; 32]);
        let alternate = Identity::from_seed([0x94; 32]);
        let pinned = pigeonpost_core::keys::SuccessorCommitment::for_key(&incoming.verifying_key());
        let next_pin = pigeonpost_core::keys::SuccessorCommitment::for_key(&next.verifying_key());
        let source = AgentRecord::new(&outgoing, &pinned, 4, vec!["https://a.example".into()]);
        let rotation = RotationRecord::new(&outgoing, &incoming, &next_pin, 5, 1_000).unwrap();
        let target = AgentRecord::new(&incoming, &next_pin, 5, vec!["https://a.example".into()]);

        state
            .save_own_rotation(&rotation, &source, &target, &["https://a.example".into()])
            .unwrap();
        let loaded = state.own_rotations().unwrap();
        assert_eq!(loaded.len(), 1);
        assert_eq!(loaded[0].record, rotation);
        assert_eq!(
            state.rotation(&outgoing.address()).unwrap(),
            Some(rotation.clone())
        );

        let poisoned = RotationRecord::new(
            &outgoing,
            &incoming,
            &pigeonpost_core::keys::SuccessorCommitment::for_key(&alternate.verifying_key()),
            5,
            1_000,
        )
        .unwrap();
        assert!(state
            .save_rotation(&outgoing.address(), &poisoned, 1_001)
            .is_err());
    }

    #[cfg(unix)]
    #[test]
    fn state_database_custody_rejects_insecure_links_and_path_swaps() {
        use std::os::unix::fs::{symlink, MetadataExt, OpenOptionsExt, PermissionsExt};

        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("state.db");
        State::open(&path).unwrap();
        assert_eq!(std::fs::metadata(&path).unwrap().mode() & 0o777, 0o600);

        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o644)).unwrap();
        assert!(matches!(State::open(&path), Err(ClientError::Config(_))));
        assert_eq!(
            std::fs::metadata(&path).unwrap().mode() & 0o777,
            0o644,
            "unsafe existing state is reported, not silently blessed"
        );
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o600)).unwrap();

        let link = dir.path().join("state-link.db");
        symlink(&path, &link).unwrap();
        assert!(matches!(State::open(&link), Err(ClientError::Config(_))));

        let hard_link = dir.path().join("state-hard-link.db");
        std::fs::hard_link(&path, &hard_link).unwrap();
        assert!(matches!(State::open(&path), Err(ClientError::Config(_))));
        std::fs::remove_file(&hard_link).unwrap();

        let symlink_swap_path = dir.path().join("symlink-swap.db");
        State::open(&symlink_swap_path).unwrap();
        let symlink_swap_original = dir.path().join("symlink-swap-original.db");
        let symlink_target = dir.path().join("symlink-target.db");
        State::open(&symlink_target).unwrap();
        assert!(State::open_after_custody_check(&symlink_swap_path, || {
            std::fs::rename(&symlink_swap_path, &symlink_swap_original).unwrap();
            symlink(&symlink_target, &symlink_swap_path).unwrap();
        })
        .is_err());

        let regular_swap_path = dir.path().join("regular-swap.db");
        State::open(&regular_swap_path).unwrap();
        let regular_swap_original = dir.path().join("regular-swap-original.db");
        let replacement = dir.path().join("replacement.db");
        std::fs::OpenOptions::new()
            .read(true)
            .write(true)
            .create_new(true)
            .mode(0o600)
            .open(&replacement)
            .unwrap();
        assert!(State::open_after_custody_check(&regular_swap_path, || {
            std::fs::rename(&regular_swap_path, &regular_swap_original).unwrap();
            std::fs::rename(&replacement, &regular_swap_path).unwrap();
        })
        .is_err());
    }

    #[cfg(unix)]
    #[test]
    fn state_database_rejects_intermediate_symlink_and_mutable_ancestor_without_creation() {
        use std::os::unix::fs::{symlink, PermissionsExt};

        let root = tempfile::tempdir().unwrap();
        let outside = root.path().join("outside");
        std::fs::create_dir(&outside).unwrap();
        std::fs::set_permissions(&outside, std::fs::Permissions::from_mode(0o700)).unwrap();
        let linked = root.path().join("linked");
        symlink(&outside, &linked).unwrap();
        let through_link = linked.join("new-private/state.db");
        assert!(State::open(&through_link).is_err());
        assert!(!outside.join("new-private").exists());

        let mutable = root.path().join("mutable");
        std::fs::create_dir(&mutable).unwrap();
        std::fs::set_permissions(&mutable, std::fs::Permissions::from_mode(0o770)).unwrap();
        let through_mutable = mutable.join("new-private/state.db");
        assert!(State::open(&through_mutable).is_err());
        assert!(!mutable.join("new-private").exists());
    }

    #[cfg(unix)]
    #[test]
    fn retained_state_custody_detects_main_wal_shm_and_parent_replacement() {
        use std::os::unix::fs::PermissionsExt;

        fn replace(path: &std::path::Path) {
            let mut moved = path.as_os_str().to_os_string();
            moved.push(".original");
            std::fs::rename(path, std::path::PathBuf::from(moved)).unwrap();
            std::fs::write(path, []).unwrap();
            std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o600)).unwrap();
        }

        for suffix in ["", "-wal", "-shm"] {
            let root = tempfile::tempdir().unwrap();
            let path = root.path().join("private/state.db");
            let state = State::open(&path).unwrap();
            state.verify_database_custody().unwrap();
            let mut target = path.as_os_str().to_os_string();
            target.push(suffix);
            let target = std::path::PathBuf::from(target);
            replace(&target);
            assert!(
                state.verify_database_custody().is_err(),
                "replaced {suffix}"
            );
        }

        let root = tempfile::tempdir().unwrap();
        let parent = root.path().join("private");
        let path = parent.join("state.db");
        let state = State::open(&path).unwrap();
        let moved_parent = root.path().join("private.original");
        std::fs::rename(&parent, &moved_parent).unwrap();
        std::fs::create_dir(&parent).unwrap();
        std::fs::set_permissions(&parent, std::fs::Permissions::from_mode(0o700)).unwrap();
        std::fs::write(&path, []).unwrap();
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o600)).unwrap();
        assert!(state.verify_database_custody().is_err());
    }

    #[cfg(windows)]
    #[test]
    fn windows_state_database_rejects_hard_links() {
        let root = tempfile::tempdir().unwrap();
        let dir = root.path().join("private");
        crate::keystore::secure_or_create_directory(&dir).unwrap();
        let path = dir.join("state.db");
        drop(State::open(&path).unwrap());

        let second_name = dir.join("state-copy.db");
        std::fs::hard_link(&path, &second_name).unwrap();
        assert!(State::open(&path).is_err());
        std::fs::remove_file(second_name).unwrap();
        State::open(&path).unwrap();
    }

    #[cfg(windows)]
    #[test]
    fn windows_state_database_rejects_a_different_sqlite_connection_path() {
        let root = tempfile::tempdir().unwrap();
        let path = root.path().join("private/state.db");
        let preparation = WindowsStateDatabasePreparation::open_or_create(&path).unwrap();
        let unrelated = Connection::open_in_memory().unwrap();

        assert!(matches!(
            preparation.verify_sqlite_connection(&unrelated),
            Err(ClientError::Config(message))
                if message == "SQLite reports a different state database path"
        ));
    }

    #[cfg(windows)]
    #[test]
    fn windows_state_rejects_unsafe_preexisting_sidecars_before_sqlite_open() {
        use std::io::Write;

        for suffix in ["-wal", "-shm", "-journal"] {
            let root = tempfile::tempdir().unwrap();
            let directory = root.path().join("private");
            crate::keystore::secure_or_create_directory(&directory).unwrap();
            let path = directory.join("state.db");
            let sidecar = windows_state_sidecar_path(&path, suffix).unwrap();
            std::fs::write(&sidecar, b"inherited-but-not-protected").unwrap();
            let mut sqlite_open_reached = false;
            let result = State::open_after_custody_check(&path, || {
                sqlite_open_reached = true;
            });
            assert!(result.is_err(), "unsafe {suffix} sidecar was accepted");
            assert!(
                !sqlite_open_reached,
                "SQLite saw unsafe {suffix} before custody rejected it"
            );

            std::fs::remove_file(&sidecar).unwrap();
            let source = directory.join(format!("source{suffix}"));
            let (mut source_file, parents) =
                crate::keystore::windows_custody::create_new_private_file(&source).unwrap();
            source_file.write_all(b"safe-source").unwrap();
            source_file.sync_all().unwrap();
            parents.verify().unwrap();
            drop(source_file);
            std::fs::hard_link(&source, &sidecar).unwrap();
            sqlite_open_reached = false;
            let result = State::open_after_custody_check(&path, || {
                sqlite_open_reached = true;
            });
            assert!(result.is_err(), "hard-linked {suffix} sidecar was accepted");
            assert!(!sqlite_open_reached);
        }
    }

    #[cfg(windows)]
    #[test]
    fn windows_state_rejects_preexisting_reparse_sidecar_before_sqlite_open() {
        let root = tempfile::tempdir().unwrap();
        let directory = root.path().join("private");
        crate::keystore::secure_or_create_directory(&directory).unwrap();
        let path = directory.join("state.db");
        let target = directory.join("target.bin");
        std::fs::write(&target, b"target").unwrap();
        let sidecar = windows_state_sidecar_path(&path, "-wal").unwrap();
        if std::os::windows::fs::symlink_file(&target, &sidecar).is_ok() {
            let mut sqlite_open_reached = false;
            assert!(State::open_after_custody_check(&path, || {
                sqlite_open_reached = true;
            })
            .is_err());
            assert!(!sqlite_open_reached);
        }
    }

    #[cfg(windows)]
    #[test]
    fn windows_state_retains_no_delete_share_handles_for_main_wal_and_shm() {
        let root = tempfile::tempdir().unwrap();
        let directory = root.path().join("private");
        let path = directory.join("state.db");
        let state = State::open(&path).unwrap();
        state.verify_database_custody().unwrap();

        for suffix in ["", "-wal", "-shm"] {
            let source = if suffix.is_empty() {
                path.clone()
            } else {
                windows_state_sidecar_path(&path, suffix).unwrap()
            };
            let destination = windows_state_sidecar_path(&source, ".moved").unwrap();
            assert!(
                std::fs::rename(&source, &destination).is_err(),
                "{suffix} remained replaceable while State was alive"
            );
        }
    }
}
