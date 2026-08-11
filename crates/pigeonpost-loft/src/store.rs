//! Versioned, transactional loft storage.
//!
//! SQLite is synchronous. The methods here remain synchronous so another backend can implement the
//! same contract, while every async caller crosses the crate's bounded `spawn_blocking` boundary.
//! Capacity, dedupe, insert, counters, and the policy snapshot CAS are one `BEGIN IMMEDIATE`
//! transaction; no handler performs a check-then-write sequence. File-backed acknowledgements use
//! SQLite `synchronous=FULL`, so returning success means the committed WAL frame crossed SQLite's
//! power-loss durability boundary rather than merely reaching the operating-system page cache.

use std::any::Any;
use std::path::Path;
#[cfg(unix)]
use std::path::PathBuf;
use std::sync::Mutex;
use std::time::{SystemTime, UNIX_EPOCH};

use pigeonpost_compliance_format::{ComplianceKeyId, CompliancePurpose};
use pigeonpost_compliance_seal::{WrappedEpochKey, WRAPPED_EPOCH_KEY_LEN};
use pigeonpost_core::{
    envelope::Wrap,
    policy::{
        RecipientPolicy, LEGACY_RECIPIENT_POLICY_VERSION, PREVIOUS_RECIPIENT_POLICY_VERSION,
        RECIPIENT_POLICY_VERSION,
    },
    record::{AgentRecord, RotationRecord, AGENT_RECORD_VERSION},
    Address,
};
#[cfg(unix)]
use pigeonpost_unix_custody::{
    CustodyError, FilePolicy, GuardedDir, GuardedFile, LeafName, NormalizedPath, OpenAccess,
};
use rusqlite::{
    params, Connection, OpenFlags, OptionalExtension, Transaction, TransactionBehavior,
};
#[cfg(windows)]
use windows_database_custody::{DatabaseCustody, WindowsDatabasePreparation};

use crate::error::{LoftError, Result};

pub const CURRENT_SCHEMA_VERSION: u32 = 6;
const V0_1_0_SCHEMA: &str = include_str!("../tests/fixtures/v0_1_0_loft.sql");
/// Conservative accounting for row/index/WAL overhead in addition to the serialized envelope.
pub const EVENT_STORAGE_OVERHEAD: u64 = 256;
/// Conservative accounting for one durable routing/policy row and its indexes.
pub const CONTROL_STORAGE_OVERHEAD: u64 = 256;
/// At most five percent of a loft's advertised durable budget may be occupied or reserved by
/// attacker-creatable routing and policy records. Event rows may borrow unused control space, but
/// control rows can never consume the remaining event partition.
pub const CONTROL_STORAGE_DIVISOR: u64 = 20;
/// Every newly admitted agent reserves enough of the control partition for its one immutable
/// rotation. The reservation transfers to the exact rotation row and any unused remainder is
/// released, so a full loft cannot strand a previously admitted identity.
pub const ROTATION_STORAGE_RESERVATION: u64 = 4 * 1024;
/// Trace-producing admissions use UTC-aligned durable fixed windows so process restart cannot
/// grant extra capture budget beyond the storage planner's per-day bound.
pub const TRACE_ADMISSION_WINDOW_MS: u64 = 60_000;
/// One durable reservation amortizes at most this many trace-admission commits. Reserved but
/// unused slots remain spent after a crash or restart.
const TRACE_ADMISSION_BATCH_SIZE: u64 = 64;
/// A predecessor schema had no durable trace counter, so an in-place upgrade must conservatively
/// treat the migration minute as fully consumed. Every supported runtime limit fits in `u32`.
const TRACE_ADMISSION_MIGRATION_CEILING: u64 = u32::MAX as u64;
#[cfg(any(unix, windows))]
const MAX_SQLITE_FILE_BYTES: u64 = 1 << 40;

#[derive(Debug, Clone)]
pub struct StoredEvent {
    pub cursor: u64,
    pub wrap: Wrap,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct StorageStats {
    /// Total durable logical usage, including control-row reservations.
    pub bytes_used: u64,
    pub event_bytes_used: u64,
    pub control_bytes_used: u64,
    pub control_bytes_reserved: u64,
    pub event_count: u64,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[repr(u8)]
pub enum TraceSegmentState {
    Open = 1,
    Closed = 2,
}

/// Public custody metadata for one append-only trace segment. The encrypted segment bytes remain
/// on disk; SQLite stores only the information required to inventory, recover, and disclose it.
#[derive(Clone, PartialEq, Eq)]
pub struct TraceSegmentMetadata {
    pub segment_id: [u8; 32],
    pub key_id: ComplianceKeyId,
    pub opened_at_ms: u64,
    pub closed_at_ms: Option<u64>,
    pub relative_path: String,
    pub wrapped_key: [u8; WRAPPED_EPOCH_KEY_LEN],
    pub record_count: Option<u32>,
    pub first_hash: Option<[u8; 32]>,
    pub final_hash: Option<[u8; 32]>,
    pub state: TraceSegmentState,
}

impl core::fmt::Debug for TraceSegmentMetadata {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        f.debug_struct("TraceSegmentMetadata")
            .field("segment_id", &self.segment_id)
            .field("key_id", &self.key_id)
            .field("opened_at_ms", &self.opened_at_ms)
            .field("closed_at_ms", &self.closed_at_ms)
            .field("relative_path", &"<withheld>")
            .field("wrapped_key", &"<withheld>")
            .field("record_count", &self.record_count)
            .field("first_hash", &self.first_hash)
            .field("final_hash", &self.final_hash)
            .field("state", &self.state)
            .finish()
    }
}

/// Separate interface so a trace sink cannot read or mutate message rows.
pub trait TraceSegmentCatalog: Send + Sync {
    /// Idempotently record an open or closed segment. Immutable-field conflicts and attempts to
    /// move a closed segment back to open fail closed.
    fn record_trace_segment(&self, metadata: &TraceSegmentMetadata) -> Result<()>;
}

pub trait LoftStore: Send + Sync + Any {
    /// Atomically assert the policy snapshot, absorb duplicates, enforce capacity, insert, and
    /// update counters. `expected_policy_seq = None` means no registered policy was observed.
    fn admit(
        &self,
        wrap: &Wrap,
        id: &[u8; 32],
        stored_at: u64,
        expires_at: u64,
        capacity_bytes: u64,
        expected_policy_seq: Option<u64>,
    ) -> Result<bool>;

    fn fetch(&self, recipient: &[u8; 32], cursor: u64, limit: usize) -> Result<Vec<StoredEvent>>;
    fn policy(&self, pubkey: &[u8; 32]) -> Result<Option<RecipientPolicy>>;

    /// Signature verification, monotonic comparison, and write are one transaction.
    fn put_policy(&self, policy: &RecipientPolicy, capacity_bytes: u64) -> Result<()>;

    fn agent_record(&self, address: &str) -> Result<Option<AgentRecord>>;
    fn put_agent_record(
        &self,
        address: &str,
        record: &AgentRecord,
        capacity_bytes: u64,
    ) -> Result<()>;
    /// Look up the immutable transition published at an old key address.
    fn rotation_record(&self, _address: &str) -> Result<Option<RotationRecord>> {
        Err(LoftError::Configuration("rotation storage unavailable"))
    }
    /// Verify and atomically publish one transition. Exact retries are idempotent; a different
    /// record for the same old address is equivocation and fails closed.
    fn put_rotation_record(
        &self,
        _address: &str,
        _record: &RotationRecord,
        _now_secs: u64,
        _capacity_bytes: u64,
    ) -> Result<bool> {
        Err(LoftError::Configuration("rotation storage unavailable"))
    }
    fn sweep_expired(&self, now: u64, batch: usize) -> Result<usize>;
    /// Remove deleted-event frames from the WAL after a completed retention pass.
    fn retention_checkpoint(&self) -> Result<()>;
    fn stats(&self) -> Result<StorageStats>;
    fn health_check(&self) -> Result<()>;

    /// Whether this backend durably preserves the trace-producing global rate window across
    /// process restart. An enabled trace sink fails startup when this remains false.
    fn supports_durable_trace_admission(&self) -> bool {
        false
    }

    /// Whether this exact backend instance is a restart-persistent production adapter. Public
    /// serving checks this only after also requiring the concrete audited `SqliteStore` type, so a
    /// custom implementation cannot promote itself by overriding the method.
    fn supports_public_durable_trace_admission(&self) -> bool {
        false
    }

    /// Consume one trace-producing admission from a durable reservation in the UTC-aligned minute
    /// containing `timestamp_ms`. Rejections and later trace failures are intentionally not
    /// refunded; unused reserved slots may be conservatively burned by restart.
    fn charge_trace_admission(&self, _timestamp_ms: u64, _limit: u32) -> Result<()> {
        Err(LoftError::Configuration(
            "durable trace admission unavailable",
        ))
    }

    fn bytes_used(&self) -> Result<u64> {
        Ok(self.stats()?.bytes_used)
    }

    fn event_count(&self) -> Result<u64> {
        Ok(self.stats()?.event_count)
    }
}

pub struct SqliteStore {
    conn: Mutex<Connection>,
    trace_admission_batch: Mutex<TraceAdmissionBatch>,
    // Fields are dropped in declaration order. Keep the SQLite connection first so every native
    // SQLite handle closes before the retained filesystem custody handles release their names.
    #[cfg(any(unix, windows))]
    custody: Option<DatabaseCustody>,
}

#[derive(Debug, Default)]
struct TraceAdmissionBatch {
    /// Highest UTC-aligned minute observed by this process, including a newer durable minute
    /// written by a peer process. Local reserve can never move this watermark backward.
    highest_requested_window_start_ms: u64,
    /// Lexicographic high-water mark of the durable singleton. This survives lease exhaustion and
    /// runtime-limit changes, so a same-minute database rollback cannot be re-reserved.
    durable_high_water: Option<(u64, u64)>,
    lease: Option<TraceAdmissionLease>,
}

#[derive(Debug, Clone, Copy)]
struct TraceAdmissionLease {
    window_start_ms: u64,
    limit: u32,
    remaining: u64,
    /// Durable admission count after this lease was committed. A lower persisted value indicates
    /// rollback or replacement of the database and invalidates the in-memory reserve.
    reserved_through: u64,
}

fn validate_trace_admission_observation(
    batch: &mut TraceAdmissionBatch,
    observed: (u64, u64),
) -> Result<()> {
    if batch
        .durable_high_water
        .is_some_and(|high_water| observed < high_water)
    {
        batch.lease = None;
        return Err(LoftError::TraceUnavailable);
    }
    if batch
        .durable_high_water
        .is_none_or(|high_water| observed > high_water)
    {
        batch.durable_high_water = Some(observed);
    }
    Ok(())
}

impl SqliteStore {
    pub fn open(path: &str) -> Result<Self> {
        require_supported_persistent_store()?;

        #[cfg(unix)]
        {
            let custody = DatabaseCustody::open_or_create(Path::new(path))?;
            let sqlite_path = custody.path.clone();
            let flags = OpenFlags::SQLITE_OPEN_READ_WRITE
                | OpenFlags::SQLITE_OPEN_NO_MUTEX
                | OpenFlags::SQLITE_OPEN_NOFOLLOW;
            custody.verify_main_named()?;
            let conn = Connection::open_with_flags(&sqlite_path, flags)?;
            custody.verify_sqlite_connection(&conn)?;

            let mut store = Self::init(conn)?;
            custody.verify_all_named()?;
            store.custody = Some(custody);
            Ok(store)
        }

        #[cfg(windows)]
        {
            Self::open_windows_after_custody_check(path, || {})
        }

        #[cfg(not(any(unix, windows)))]
        {
            let _ = path;
            Err(unsupported_persistent_store_error())
        }
    }

    pub fn in_memory() -> Result<Self> {
        Self::init(Connection::open_in_memory()?)
    }

    #[cfg(windows)]
    fn open_windows_after_custody_check<F>(path: &str, before_sqlite_open: F) -> Result<Self>
    where
        F: FnOnce(),
    {
        let preparation = WindowsDatabasePreparation::open_or_create(Path::new(path))?;
        let sqlite_path = preparation.path.clone();
        let flags = OpenFlags::SQLITE_OPEN_READ_WRITE
            | OpenFlags::SQLITE_OPEN_NO_MUTEX
            | OpenFlags::SQLITE_OPEN_NOFOLLOW;
        preparation.verify_main_named()?;
        before_sqlite_open();
        let conn = Connection::open_with_flags(&sqlite_path, flags)?;
        preparation.verify_sqlite_connection(&conn)?;

        let mut store = Self::init(conn)?;
        let custody = preparation.finish()?;
        custody.verify_all_named()?;
        store.custody = Some(custody);
        Ok(store)
    }

    fn init(mut conn: Connection) -> Result<Self> {
        conn.pragma_update(None, "journal_mode", "WAL")?;
        // A publish response is a durability promise: NORMAL-mode WAL may lose the last committed
        // transaction after power loss even though the database remains consistent. FULL adds the
        // commit sync needed before the handler can acknowledge admission.
        conn.pragma_update(None, "synchronous", "FULL")?;
        conn.pragma_update(None, "busy_timeout", 5_000)?;
        conn.pragma_update(None, "foreign_keys", "ON")?;
        conn.pragma_update(None, "secure_delete", "ON")?;
        conn.pragma_update(None, "journal_size_limit", 0)?;
        migrate(&mut conn)?;
        Ok(Self {
            conn: Mutex::new(conn),
            trace_admission_batch: Mutex::new(TraceAdmissionBatch::default()),
            #[cfg(any(unix, windows))]
            custody: None,
        })
    }

    fn lock(&self) -> std::sync::MutexGuard<'_, Connection> {
        self.conn.lock().unwrap_or_else(|e| e.into_inner())
    }

    #[cfg(test)]
    fn schema_version(&self) -> u32 {
        self.lock()
            .pragma_query_value(None, "user_version", |row| row.get(0))
            .unwrap()
    }

    #[cfg(test)]
    pub(crate) fn trace_segment_state(&self, segment_id: &[u8; 32]) -> Option<(i64, Option<i64>)> {
        self.lock()
            .query_row(
                "SELECT state, record_count FROM trace_segments WHERE segment_id = ?1",
                params![segment_id.as_slice()],
                |row| Ok((row.get(0)?, row.get(1)?)),
            )
            .optional()
            .unwrap()
    }
}

fn require_supported_persistent_store() -> Result<()> {
    require_supported_persistent_store_for(cfg!(any(
        target_os = "linux",
        target_os = "macos",
        windows
    )))
}

fn require_supported_persistent_store_for(supported: bool) -> Result<()> {
    if supported {
        Ok(())
    } else {
        Err(unsupported_persistent_store_error())
    }
}

fn unsupported_persistent_store_error() -> LoftError {
    LoftError::Io(std::io::Error::new(
        std::io::ErrorKind::Unsupported,
        "persistent loft storage is supported only on Linux, macOS, and Windows",
    ))
}

fn migrate(conn: &mut Connection) -> Result<()> {
    let raw_version: i64 = conn.pragma_query_value(None, "user_version", |row| row.get(0))?;
    let found = u32::try_from(raw_version)
        .map_err(|_| LoftError::Configuration("invalid loft database schema version"))?;
    if found > CURRENT_SCHEMA_VERSION {
        return Err(LoftError::UnsupportedSchema {
            found,
            supported: CURRENT_SCHEMA_VERSION,
        });
    }

    // A genuinely empty database has admitted no pre-counter requests. Any predecessor schema,
    // including the exact unversioned 0.1.0 layout, may already have served the current minute and
    // must not receive a second full window during this in-place upgrade.
    let cool_down_trace_admission = found > 0 || !schema_snapshot(conn)?.is_empty();

    if found > 0 {
        validate_loft_schema(conn, found)?;
        validate_loft_invariants(conn, found)?;
    }

    for target in (found + 1)..=CURRENT_SCHEMA_VERSION {
        let tx = conn.transaction_with_behavior(TransactionBehavior::Immediate)?;
        if target == 1 {
            migrate_to_v1(&tx)?;
        } else {
            validate_loft_schema(&tx, target - 1)?;
            validate_loft_invariants(&tx, target - 1)?;
            apply_loft_migration(&tx, target)?;
            if target == 6 && cool_down_trace_admission {
                cool_down_migration_minute(&tx)?;
            }
        }
        validate_loft_schema(&tx, target)?;
        validate_loft_invariants(&tx, target)?;
        tx.pragma_update(None, "user_version", target)?;
        tx.commit()?;
    }
    Ok(())
}

/// Adopt only the exact schema emitted by 0.1.0, or create it in a genuinely empty database.
fn migrate_to_v1(tx: &Transaction<'_>) -> Result<()> {
    let actual = schema_snapshot(tx)?;
    if actual.is_empty() {
        tx.execute_batch(V0_1_0_SCHEMA)?;
    } else if actual != expected_loft_schema(1)? {
        return Err(LoftError::Configuration(
            "unrecognized unversioned loft database schema",
        ));
    }
    Ok(())
}

fn apply_loft_migration(tx: &Transaction<'_>, target: u32) -> Result<()> {
    match target {
        2 => migrate_to_v2(tx),
        3 => migrate_to_v3(tx),
        4 => migrate_to_v4(tx),
        5 => migrate_to_v5(tx),
        6 => migrate_to_v6(tx),
        _ => unreachable!("migration target is bounded and version 1 is handled separately"),
    }
}

fn migrate_to_v6(tx: &Transaction<'_>) -> Result<()> {
    tx.execute_batch(
        r#"
        CREATE TABLE trace_admission (
            singleton       INTEGER PRIMARY KEY CHECK (singleton = 1),
            window_start_ms INTEGER NOT NULL CHECK (
                window_start_ms >= 0 AND window_start_ms % 60000 = 0
            ),
            admitted        INTEGER NOT NULL CHECK (admitted >= 0)
        );
        INSERT INTO trace_admission (singleton, window_start_ms, admitted) VALUES (1, 0, 0);
        "#,
    )?;
    Ok(())
}

fn cool_down_migration_minute(tx: &Transaction<'_>) -> Result<()> {
    let now_ms = unix_millis();
    let window_start_ms = now_ms - (now_ms % TRACE_ADMISSION_WINDOW_MS);
    let updated = tx.execute(
        "UPDATE trace_admission SET window_start_ms = ?1, admitted = ?2 WHERE singleton = 1",
        params![
            to_i64(window_start_ms)?,
            to_i64(TRACE_ADMISSION_MIGRATION_CEILING)?
        ],
    )?;
    if updated != 1 {
        return Err(LoftError::Configuration(
            "durable trace admission singleton is missing",
        ));
    }
    Ok(())
}

fn migrate_to_v2(tx: &Transaction<'_>) -> Result<()> {
    tx.execute_batch(
        r#"
        ALTER TABLE recipient_policy ADD COLUMN policy_version INTEGER NOT NULL DEFAULT 1;
        ALTER TABLE recipient_policy ADD COLUMN updated_at INTEGER NOT NULL DEFAULT 0;
        ALTER TABLE agent_records ADD COLUMN updated_at INTEGER NOT NULL DEFAULT 0;
        CREATE INDEX events_by_recipient_stored_at ON events (recipient, stored_at);
        CREATE TABLE storage_stats (
            singleton   INTEGER PRIMARY KEY CHECK (singleton = 1),
            bytes_used  INTEGER NOT NULL CHECK (bytes_used >= 0),
            event_count INTEGER NOT NULL CHECK (event_count >= 0)
        );
        "#,
    )?;
    tx.execute(
        "UPDATE events SET size = size + ?1",
        params![to_i64(EVENT_STORAGE_OVERHEAD)?],
    )?;
    tx.execute(
        "INSERT INTO storage_stats (singleton, bytes_used, event_count)
         SELECT 1, COALESCE(SUM(size), 0), COUNT(*) FROM events",
        [],
    )?;

    // The signed BLOB is authoritative. Validate every deployed row and derive cached columns
    // from it rather than trusting the legacy shadow seq.
    let mut stmt = tx.prepare("SELECT pubkey, policy FROM recipient_policy")?;
    let rows = stmt.query_map([], |row| {
        Ok((row.get::<_, Vec<u8>>(0)?, row.get::<_, Vec<u8>>(1)?))
    })?;
    let mut policies = Vec::new();
    for row in rows {
        policies.push(row?);
    }
    drop(stmt);
    for (pubkey, blob) in policies {
        let policy: RecipientPolicy = serde_json::from_slice(&blob)?;
        policy.verify(None)?;
        if policy.pubkey.as_slice() != pubkey.as_slice()
            || !matches!(
                policy.version,
                LEGACY_RECIPIENT_POLICY_VERSION
                    | PREVIOUS_RECIPIENT_POLICY_VERSION
                    | RECIPIENT_POLICY_VERSION
            )
        {
            return Err(pigeonpost_core::Error::MalformedEnvelope("stored policy mismatch").into());
        }
        tx.execute(
            "UPDATE recipient_policy SET seq = ?1, policy_version = ?2 WHERE pubkey = ?3",
            params![to_i64(policy.seq)?, i64::from(policy.version), pubkey],
        )?;
    }
    Ok(())
}

fn migrate_to_v3(tx: &Transaction<'_>) -> Result<()> {
    tx.execute_batch(
        r#"
        CREATE TABLE trace_segments (
            segment_id    BLOB PRIMARY KEY CHECK (length(segment_id) = 32),
            key_id        BLOB NOT NULL CHECK (length(key_id) = 47),
            purpose       INTEGER NOT NULL,
            jurisdiction  INTEGER NOT NULL,
            opened_at     INTEGER NOT NULL CHECK (opened_at > 0),
            closed_at     INTEGER,
            path          TEXT NOT NULL UNIQUE,
            wrapped_key   BLOB NOT NULL CHECK (length(wrapped_key) = 216),
            record_count  INTEGER,
            first_hash    BLOB,
            final_hash    BLOB,
            state         INTEGER NOT NULL CHECK (state IN (1, 2)),
            CHECK (
                (state = 1 AND closed_at IS NULL AND record_count IS NULL
                    AND first_hash IS NULL AND final_hash IS NULL)
                OR
                (state = 2 AND closed_at >= opened_at AND record_count IS NOT NULL
                    AND record_count >= 0 AND first_hash IS NOT NULL
                    AND length(first_hash) = 32 AND final_hash IS NOT NULL
                    AND length(final_hash) = 32)
            )
        );
        CREATE INDEX trace_segments_by_key ON trace_segments (key_id, opened_at);
        CREATE INDEX trace_segments_by_state ON trace_segments (state, opened_at);
        "#,
    )?;
    Ok(())
}

fn migrate_to_v4(tx: &Transaction<'_>) -> Result<()> {
    tx.execute_batch(
        r#"
        CREATE TABLE rotation_records (
            from_address TEXT PRIMARY KEY,
            from_pubkey  BLOB NOT NULL UNIQUE CHECK (length(from_pubkey) = 32),
            to_address   TEXT NOT NULL,
            to_pubkey    BLOB NOT NULL CHECK (length(to_pubkey) = 32),
            seq          INTEGER NOT NULL CHECK (seq > 0),
            activated_at INTEGER NOT NULL CHECK (activated_at > 0),
            grace_until INTEGER NOT NULL CHECK (grace_until > activated_at),
            record       BLOB NOT NULL,
            stored_at    INTEGER NOT NULL CHECK (stored_at > 0)
        );
        CREATE INDEX rotation_records_by_target ON rotation_records (to_address);
        "#,
    )?;
    Ok(())
}

fn migrate_to_v5(tx: &Transaction<'_>) -> Result<()> {
    tx.execute_batch(
        r#"
        ALTER TABLE recipient_policy ADD COLUMN accounted_size INTEGER NOT NULL DEFAULT 0
            CHECK (accounted_size >= 0);
        ALTER TABLE agent_records ADD COLUMN accounted_size INTEGER NOT NULL DEFAULT 0
            CHECK (accounted_size >= 0);
        ALTER TABLE agent_records ADD COLUMN rotation_reservation INTEGER NOT NULL DEFAULT 0
            CHECK (rotation_reservation >= 0);
        ALTER TABLE rotation_records ADD COLUMN accounted_size INTEGER NOT NULL DEFAULT 0
            CHECK (accounted_size >= 0);
        ALTER TABLE storage_stats ADD COLUMN control_bytes INTEGER NOT NULL DEFAULT 0
            CHECK (control_bytes >= 0);
        ALTER TABLE storage_stats ADD COLUMN control_reserved INTEGER NOT NULL DEFAULT 0
            CHECK (control_reserved >= 0);
        "#,
    )?;

    let overhead = to_i64(CONTROL_STORAGE_OVERHEAD)?;
    let reservation = to_i64(ROTATION_STORAGE_RESERVATION)?;
    tx.execute(
        "UPDATE recipient_policy
         SET accounted_size = length(pubkey) + length(policy) + ?1",
        params![overhead],
    )?;
    tx.execute(
        "UPDATE agent_records
         SET accounted_size = length(address) + length(record) + ?1,
             rotation_reservation = CASE
                 WHEN EXISTS(
                     SELECT 1 FROM rotation_records
                     WHERE rotation_records.from_address = agent_records.address
                 ) THEN 0 ELSE ?2 END",
        params![overhead, reservation],
    )?;
    tx.execute(
        "UPDATE rotation_records
         SET accounted_size = length(from_address) + length(to_address) + length(record) + ?1",
        params![overhead],
    )?;
    tx.execute(
        "UPDATE storage_stats
         SET control_bytes =
                 COALESCE((SELECT SUM(accounted_size) FROM recipient_policy), 0)
               + COALESCE((SELECT SUM(accounted_size) FROM agent_records), 0)
               + COALESCE((SELECT SUM(accounted_size) FROM rotation_records), 0),
             control_reserved =
                 COALESCE((SELECT SUM(rotation_reservation) FROM agent_records), 0)
         WHERE singleton = 1",
        [],
    )?;
    Ok(())
}

type SchemaObject = (String, String, String, String);

fn validate_loft_schema(conn: &Connection, version: u32) -> Result<()> {
    if schema_snapshot(conn)? != expected_loft_schema(version)? {
        return Err(LoftError::Configuration(
            "loft database schema does not match its declared version",
        ));
    }
    Ok(())
}

fn expected_loft_schema(version: u32) -> Result<Vec<SchemaObject>> {
    debug_assert!((1..=CURRENT_SCHEMA_VERSION).contains(&version));
    let mut reference = Connection::open_in_memory()?;
    reference.execute_batch(V0_1_0_SCHEMA)?;
    for target in 2..=version {
        let tx = reference.transaction_with_behavior(TransactionBehavior::Immediate)?;
        apply_loft_migration(&tx, target)?;
        tx.commit()?;
    }
    schema_snapshot(&reference)
}

fn schema_snapshot(conn: &Connection) -> Result<Vec<SchemaObject>> {
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
            canonical_schema_sql(&row.get::<_, String>(3)?),
        ))
    })?;
    rows.collect::<std::result::Result<Vec<_>, _>>()
        .map_err(Into::into)
}

/// SQLite preserves much of the spelling used to create a table. Compare the semantics we care
/// about while tolerating whitespace/comments and column order changes caused by `ALTER TABLE`.
fn canonical_schema_sql(sql: &str) -> String {
    let uncommented = sql
        .lines()
        .map(|line| line.split_once("--").map_or(line, |(before, _)| before))
        .collect::<String>();
    let normalized = normalize_sql(&uncommented);
    if !normalized.starts_with("createtable") {
        return normalized;
    }

    let Some(open) = uncommented.find('(') else {
        return normalized;
    };
    let Some(close) = uncommented.rfind(')') else {
        return normalized;
    };
    if close <= open {
        return normalized;
    }
    let mut definitions = split_sql_definitions(&uncommented[(open + 1)..close]);
    definitions.sort();
    format!(
        "{}({})",
        normalize_sql(&uncommented[..open]),
        definitions.join(",")
    )
}

fn split_sql_definitions(body: &str) -> Vec<String> {
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
            '\'' | '"' | '`' => quote = Some(character),
            '[' => quote = Some(']'),
            '(' => depth += 1,
            ')' => depth = depth.saturating_sub(1),
            ',' if depth == 0 => {
                definitions.push(normalize_sql(&body[start..offset]));
                start = offset + character.len_utf8();
            }
            _ => {}
        }
    }
    definitions.push(normalize_sql(&body[start..]));
    definitions
}

fn normalize_sql(sql: &str) -> String {
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
            '\'' | '"' | '`' => {
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

fn validate_loft_invariants(conn: &Connection, version: u32) -> Result<()> {
    let overhead = if version == 1 {
        0
    } else {
        i64::try_from(EVENT_STORAGE_OVERHEAD)
            .map_err(|_| LoftError::Configuration("loft accounting constant is invalid"))?
    };
    let invalid_event: bool = conn.query_row(
        "SELECT EXISTS(
             SELECT 1 FROM events
             WHERE typeof(cursor) <> 'integer' OR cursor <= 0
                OR length(id) <> 32 OR length(recipient) <> 32
                OR typeof(stored_at) <> 'integer' OR stored_at < 0
                OR typeof(expires_at) <> 'integer' OR expires_at < 0
                OR typeof(size) <> 'integer' OR size < 0
                OR size <> length(blob) + ?1
         )",
        params![overhead],
        |row| row.get(0),
    )?;
    if invalid_event {
        return Err(LoftError::Configuration(
            "loft database contains invalid event accounting",
        ));
    }

    if version >= 2 {
        validate_policy_cache(conn)?;
        let stored: Option<(i64, i64)> = conn
            .query_row(
                "SELECT bytes_used, event_count FROM storage_stats WHERE singleton = 1",
                [],
                |row| Ok((row.get(0)?, row.get(1)?)),
            )
            .optional()?;
        let rows: i64 =
            conn.query_row("SELECT COUNT(*) FROM storage_stats", [], |row| row.get(0))?;
        let computed: (i64, i64) = conn.query_row(
            "SELECT COALESCE(SUM(size), 0), COUNT(*) FROM events",
            [],
            |row| Ok((row.get(0)?, row.get(1)?)),
        )?;
        if rows != 1 || stored != Some(computed) {
            return Err(LoftError::Configuration(
                "loft storage counters do not match persisted events",
            ));
        }
    }
    if version >= 5 {
        let invalid_control_accounting: bool = conn.query_row(
            "SELECT EXISTS(
                 SELECT 1 FROM recipient_policy
                 WHERE typeof(accounted_size) <> 'integer'
                    OR accounted_size <> length(pubkey) + length(policy) + ?1
                 UNION ALL
                 SELECT 1 FROM agent_records
                 WHERE typeof(accounted_size) <> 'integer'
                    OR accounted_size <> length(address) + length(record) + ?1
                    OR typeof(rotation_reservation) <> 'integer'
                    OR rotation_reservation <> CASE
                        WHEN EXISTS(
                            SELECT 1 FROM rotation_records
                            WHERE rotation_records.from_address = agent_records.address
                        ) THEN 0 ELSE ?2 END
                 UNION ALL
                 SELECT 1 FROM rotation_records
                 WHERE typeof(accounted_size) <> 'integer'
                    OR accounted_size <>
                        length(from_address) + length(to_address) + length(record) + ?1
             )",
            params![
                to_i64(CONTROL_STORAGE_OVERHEAD)?,
                to_i64(ROTATION_STORAGE_RESERVATION)?,
            ],
            |row| row.get(0),
        )?;
        if invalid_control_accounting {
            return Err(LoftError::Configuration(
                "loft database contains invalid control storage accounting",
            ));
        }
        let stored: (i64, i64) = conn.query_row(
            "SELECT control_bytes, control_reserved FROM storage_stats WHERE singleton = 1",
            [],
            |row| Ok((row.get(0)?, row.get(1)?)),
        )?;
        let computed: (i64, i64) = conn.query_row(
            "SELECT
                 COALESCE((SELECT SUM(accounted_size) FROM recipient_policy), 0)
                   + COALESCE((SELECT SUM(accounted_size) FROM agent_records), 0)
                   + COALESCE((SELECT SUM(accounted_size) FROM rotation_records), 0),
                 COALESCE((SELECT SUM(rotation_reservation) FROM agent_records), 0)",
            [],
            |row| Ok((row.get(0)?, row.get(1)?)),
        )?;
        if stored != computed {
            return Err(LoftError::Configuration(
                "loft control storage counters do not match persisted records",
            ));
        }
    }
    if version >= 6 {
        let rows: i64 =
            conn.query_row("SELECT COUNT(*) FROM trace_admission", [], |row| row.get(0))?;
        let counter: Option<(i64, i64)> = conn
            .query_row(
                "SELECT window_start_ms, admitted
                 FROM trace_admission WHERE singleton = 1",
                [],
                |row| Ok((row.get(0)?, row.get(1)?)),
            )
            .optional()?;
        if rows != 1
            || counter.is_none_or(|(window_start_ms, admitted)| {
                window_start_ms < 0
                    || window_start_ms % TRACE_ADMISSION_WINDOW_MS as i64 != 0
                    || admitted < 0
            })
        {
            return Err(LoftError::Configuration(
                "loft trace admission counter is invalid",
            ));
        }
    }
    Ok(())
}

fn validate_policy_cache(conn: &Connection) -> Result<()> {
    let mut statement = conn.prepare(
        "SELECT pubkey, seq, policy_version, policy FROM recipient_policy ORDER BY pubkey",
    )?;
    let rows = statement.query_map([], |row| {
        Ok((
            row.get::<_, Vec<u8>>(0)?,
            row.get::<_, i64>(1)?,
            row.get::<_, i64>(2)?,
            row.get::<_, Vec<u8>>(3)?,
        ))
    })?;
    for row in rows {
        let (pubkey, sequence, version, blob) = row?;
        let policy: RecipientPolicy = serde_json::from_slice(&blob)?;
        policy.verify(None)?;
        if pubkey.as_slice() != policy.pubkey.as_slice()
            || sequence != to_i64(policy.seq)?
            || version != i64::from(policy.version)
            || !matches!(
                policy.version,
                LEGACY_RECIPIENT_POLICY_VERSION
                    | PREVIOUS_RECIPIENT_POLICY_VERSION
                    | RECIPIENT_POLICY_VERSION
            )
        {
            return Err(LoftError::Configuration(
                "loft recipient policy cache is inconsistent",
            ));
        }
    }
    Ok(())
}

enum PolicyExpectation {
    #[cfg(test)]
    Ignore,
    Exact(Option<u64>),
}

fn control_capacity_bytes(capacity_bytes: u64) -> u64 {
    capacity_bytes / CONTROL_STORAGE_DIVISOR
}

fn accounted_size(blob_len: usize, identifier_bytes: usize) -> Result<u64> {
    u64::try_from(blob_len)
        .ok()
        .and_then(|size| size.checked_add(u64::try_from(identifier_bytes).ok()?))
        .and_then(|size| size.checked_add(CONTROL_STORAGE_OVERHEAD))
        .ok_or_else(|| pigeonpost_core::Error::TooLarge.into())
}

fn control_counters(tx: &Transaction<'_>) -> Result<(u64, u64, u64)> {
    let (event_bytes, control_bytes, control_reserved): (i64, i64, i64) = tx.query_row(
        "SELECT bytes_used, control_bytes, control_reserved
         FROM storage_stats WHERE singleton = 1",
        [],
        |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
    )?;
    let convert = |value| -> Result<u64> {
        u64::try_from(value).map_err(|_| {
            LoftError::from(pigeonpost_core::Error::MalformedEnvelope(
                "negative storage counter",
            ))
        })
    };
    Ok((
        convert(event_bytes)?,
        convert(control_bytes)?,
        convert(control_reserved)?,
    ))
}

fn replace_control_charge(
    tx: &Transaction<'_>,
    capacity_bytes: u64,
    old_accounted: u64,
    new_accounted: u64,
    old_reserved: u64,
    new_reserved: u64,
) -> Result<()> {
    let (event_bytes, control_bytes, control_reserved) = control_counters(tx)?;
    let next_control = control_bytes
        .checked_sub(old_accounted)
        .and_then(|value| value.checked_add(new_accounted))
        .ok_or(pigeonpost_core::Error::MalformedEnvelope(
            "control storage counter mismatch",
        ))?;
    let next_reserved = control_reserved
        .checked_sub(old_reserved)
        .and_then(|value| value.checked_add(new_reserved))
        .ok_or(pigeonpost_core::Error::MalformedEnvelope(
            "control reservation counter mismatch",
        ))?;
    let next_control_total = next_control
        .checked_add(next_reserved)
        .ok_or(pigeonpost_core::Error::TooLarge)?;
    if next_control_total > control_capacity_bytes(capacity_bytes)
        || event_bytes
            .checked_add(next_control_total)
            .is_none_or(|total| total > capacity_bytes)
    {
        return Err(LoftError::AtCapacity);
    }

    let updated = tx.execute(
        "UPDATE storage_stats SET control_bytes = ?1, control_reserved = ?2
         WHERE singleton = 1",
        params![to_i64(next_control)?, to_i64(next_reserved)?],
    )?;
    if updated != 1 {
        return Err(pigeonpost_core::Error::MalformedEnvelope("storage counter missing").into());
    }
    Ok(())
}

impl SqliteStore {
    fn insert(
        &self,
        wrap: &Wrap,
        id: &[u8; 32],
        stored_at: u64,
        expires_at: u64,
        capacity_bytes: u64,
        expected: PolicyExpectation,
    ) -> Result<bool> {
        let blob = serde_json::to_vec(wrap)?;
        let accounted = u64::try_from(blob.len())
            .unwrap_or(u64::MAX)
            .saturating_add(EVENT_STORAGE_OVERHEAD);
        let mut conn = self.lock();
        let tx = conn.transaction_with_behavior(TransactionBehavior::Immediate)?;

        // Duplicate publish is idempotent even when the loft has since filled.
        let duplicate: bool = tx.query_row(
            "SELECT EXISTS(SELECT 1 FROM events WHERE id = ?1)",
            params![id.as_slice()],
            |row| row.get(0),
        )?;
        if duplicate {
            tx.commit()?;
            return Ok(false);
        }

        match expected {
            PolicyExpectation::Exact(expected) => {
                let blob: Option<Vec<u8>> = tx
                    .query_row(
                        "SELECT policy FROM recipient_policy WHERE pubkey = ?1",
                        params![wrap.recipient.as_slice()],
                        |row| row.get(0),
                    )
                    .optional()?;
                let actual = match blob {
                    Some(blob) => Some(decode_policy(&blob)?.seq),
                    None => None,
                };
                if actual != expected {
                    return Err(LoftError::PolicyChanged);
                }
            }
            #[cfg(test)]
            PolicyExpectation::Ignore => {}
        }

        let (event_bytes, control_bytes, control_reserved) = control_counters(&tx)?;
        let used = event_bytes
            .checked_add(control_bytes)
            .and_then(|value| value.checked_add(control_reserved))
            .ok_or(pigeonpost_core::Error::TooLarge)?;
        if accounted > capacity_bytes.saturating_sub(used) {
            return Err(LoftError::AtCapacity);
        }

        tx.execute(
            "INSERT INTO events (id, recipient, stored_at, expires_at, size, blob)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
            params![
                id.as_slice(),
                wrap.recipient.as_slice(),
                to_i64(stored_at)?,
                to_i64(expires_at)?,
                to_i64(accounted)?,
                blob
            ],
        )?;
        let updated = tx.execute(
            "UPDATE storage_stats
             SET bytes_used = bytes_used + ?1, event_count = event_count + 1
             WHERE singleton = 1",
            params![to_i64(accounted)?],
        )?;
        if updated != 1 {
            return Err(
                pigeonpost_core::Error::MalformedEnvelope("storage counter missing").into(),
            );
        }
        tx.commit()?;
        Ok(true)
    }
}

impl LoftStore for SqliteStore {
    fn admit(
        &self,
        wrap: &Wrap,
        id: &[u8; 32],
        stored_at: u64,
        expires_at: u64,
        capacity_bytes: u64,
        expected_policy_seq: Option<u64>,
    ) -> Result<bool> {
        wrap.verify_public()?;
        if id != &wrap.id() {
            return Err(pigeonpost_core::Error::MalformedEnvelope("event id mismatch").into());
        }
        self.insert(
            wrap,
            id,
            stored_at,
            expires_at,
            capacity_bytes,
            PolicyExpectation::Exact(expected_policy_seq),
        )
    }

    fn fetch(&self, recipient: &[u8; 32], cursor: u64, limit: usize) -> Result<Vec<StoredEvent>> {
        let conn = self.lock();
        let mut stmt = conn.prepare_cached(
            "SELECT cursor, blob FROM events
             WHERE recipient = ?1 AND cursor > ?2
             ORDER BY cursor ASC LIMIT ?3",
        )?;
        let rows = stmt.query_map(
            params![recipient.as_slice(), to_i64(cursor)?, to_i64(limit as u64)?],
            |row| Ok((row.get::<_, i64>(0)?, row.get::<_, Vec<u8>>(1)?)),
        )?;
        let mut out = Vec::new();
        for row in rows {
            let (cursor, blob) = row?;
            out.push(StoredEvent {
                cursor: u64::try_from(cursor).map_err(|_| {
                    pigeonpost_core::Error::MalformedEnvelope("negative stored cursor")
                })?,
                wrap: serde_json::from_slice(&blob)?,
            });
        }
        Ok(out)
    }

    fn policy(&self, pubkey: &[u8; 32]) -> Result<Option<RecipientPolicy>> {
        let conn = self.lock();
        let row: Option<(i64, i64, Vec<u8>)> = conn
            .query_row(
                "SELECT seq, policy_version, policy FROM recipient_policy WHERE pubkey = ?1",
                params![pubkey.as_slice()],
                |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
            )
            .optional()?;
        match row {
            Some((seq, version, blob)) => {
                let policy = decode_policy(&blob)?;
                if to_i64(policy.seq)? != seq || i64::from(policy.version) != version {
                    return Err(pigeonpost_core::Error::MalformedEnvelope(
                        "stored policy mismatch",
                    )
                    .into());
                }
                Ok(Some(policy))
            }
            None => Ok(None),
        }
    }

    fn put_policy(&self, policy: &RecipientPolicy, capacity_bytes: u64) -> Result<()> {
        if policy.version != RECIPIENT_POLICY_VERSION {
            return Err(
                pigeonpost_core::Error::MalformedEnvelope("v3 policy required for write").into(),
            );
        }
        let blob = serde_json::to_vec(policy)?;
        let new_accounted = accounted_size(blob.len(), policy.pubkey.len())?;
        let mut conn = self.lock();
        let tx = conn.transaction_with_behavior(TransactionBehavior::Immediate)?;
        let existing: Option<(Vec<u8>, i64)> = tx
            .query_row(
                "SELECT policy, accounted_size FROM recipient_policy WHERE pubkey = ?1",
                params![policy.pubkey.as_slice()],
                |row| Ok((row.get(0)?, row.get(1)?)),
            )
            .optional()?;
        if existing
            .as_ref()
            .is_some_and(|(existing, _)| existing == &blob)
        {
            tx.commit()?;
            return Ok(());
        }
        let last_seq = existing
            .as_ref()
            .map(|(blob, _)| decode_policy(blob))
            .transpose()?
            .map(|p| p.seq);
        policy.verify(last_seq)?;
        let old_accounted = existing
            .as_ref()
            .map(|(_, size)| u64::try_from(*size))
            .transpose()
            .map_err(|_| pigeonpost_core::Error::MalformedEnvelope("negative storage counter"))?
            .unwrap_or(0);
        replace_control_charge(&tx, capacity_bytes, old_accounted, new_accounted, 0, 0)?;
        let changed = tx.execute(
            "INSERT INTO recipient_policy
                (pubkey, seq, policy, policy_version, updated_at, accounted_size)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6)
             ON CONFLICT(pubkey) DO UPDATE SET
                seq = excluded.seq,
                policy = excluded.policy,
                policy_version = excluded.policy_version,
                updated_at = excluded.updated_at,
                accounted_size = excluded.accounted_size",
            params![
                policy.pubkey.as_slice(),
                to_i64(policy.seq)?,
                blob,
                i64::from(policy.version),
                to_i64(unix_seconds())?,
                to_i64(new_accounted)?,
            ],
        )?;
        if changed != 1 {
            return Err(pigeonpost_core::Error::MalformedEnvelope("policy write failed").into());
        }
        tx.commit()?;
        Ok(())
    }

    fn agent_record(&self, address: &str) -> Result<Option<AgentRecord>> {
        let conn = self.lock();
        let blob: Option<Vec<u8>> = conn
            .query_row(
                "SELECT record FROM agent_records WHERE address = ?1",
                params![address],
                |row| row.get(0),
            )
            .optional()?;
        blob.map(|bytes| serde_json::from_slice(&bytes))
            .transpose()
            .map_err(Into::into)
    }

    fn put_agent_record(
        &self,
        address: &str,
        record: &AgentRecord,
        capacity_bytes: u64,
    ) -> Result<()> {
        let address = Address::parse(address)?;
        if record.version != AGENT_RECORD_VERSION {
            return Err(pigeonpost_core::Error::MalformedEnvelope(
                "v2 agent record required for write",
            )
            .into());
        }
        record.verify(&address)?;
        let blob = serde_json::to_vec(record)?;
        let new_accounted = accounted_size(blob.len(), address.as_str().len())?;
        let mut conn = self.lock();
        let tx = conn.transaction_with_behavior(TransactionBehavior::Immediate)?;
        let existing: Option<(Vec<u8>, i64, i64)> = tx
            .query_row(
                "SELECT record, accounted_size, rotation_reservation
                 FROM agent_records WHERE address = ?1",
                params![address.as_str()],
                |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
            )
            .optional()?;
        if let Some((existing_blob, _, _)) = existing.as_ref() {
            if existing_blob == &blob {
                tx.commit()?;
                return Ok(());
            }
            let rotated: bool = tx.query_row(
                "SELECT EXISTS(
                    SELECT 1 FROM rotation_records WHERE from_address = ?1
                 )",
                params![address.as_str()],
                |row| row.get(0),
            )?;
            if rotated {
                // The transition fixes the exact source sequence it advances. Allowing any later
                // record at the retired address would let a compromised old key hide the chain.
                return Err(pigeonpost_core::Error::StaleSequence.into());
            }
            let prior: AgentRecord = serde_json::from_slice(existing_blob)?;
            prior.verify(&address)?;
            if prior.successor_hash != record.successor_hash {
                return Err(pigeonpost_core::Error::SuccessorMismatch.into());
            }
            if record.seq <= prior.seq {
                return Err(pigeonpost_core::Error::StaleSequence.into());
            }
        }
        let (old_accounted, reservation) = existing
            .as_ref()
            .map(|(_, accounted, reserved)| {
                Ok::<_, pigeonpost_core::Error>((
                    u64::try_from(*accounted).map_err(|_| {
                        pigeonpost_core::Error::MalformedEnvelope("negative storage counter")
                    })?,
                    u64::try_from(*reserved).map_err(|_| {
                        pigeonpost_core::Error::MalformedEnvelope("negative storage counter")
                    })?,
                ))
            })
            .transpose()?
            .unwrap_or((0, ROTATION_STORAGE_RESERVATION));
        replace_control_charge(
            &tx,
            capacity_bytes,
            old_accounted,
            new_accounted,
            if existing.is_some() { reservation } else { 0 },
            reservation,
        )?;
        let changed = tx.execute(
            "INSERT INTO agent_records
                (address, seq, record, updated_at, accounted_size, rotation_reservation)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6)
             ON CONFLICT(address) DO UPDATE SET
                seq = excluded.seq,
                record = excluded.record,
                updated_at = excluded.updated_at,
                accounted_size = excluded.accounted_size
             WHERE excluded.seq > agent_records.seq",
            params![
                address.as_str(),
                to_i64(record.seq)?,
                blob,
                to_i64(unix_seconds())?,
                to_i64(new_accounted)?,
                to_i64(reservation)?,
            ],
        )?;
        if changed == 0 {
            return Err(pigeonpost_core::Error::StaleSequence.into());
        }
        tx.commit()?;
        Ok(())
    }

    fn rotation_record(&self, address: &str) -> Result<Option<RotationRecord>> {
        let address = Address::parse(address)?;
        let conn = self.lock();
        let row: Option<(Vec<u8>, Vec<u8>)> = conn
            .query_row(
                "SELECT rotations.record, agents.record
                 FROM rotation_records AS rotations
                 JOIN agent_records AS agents ON agents.address = rotations.from_address
                 WHERE rotations.from_address = ?1",
                params![address.as_str()],
                |row| Ok((row.get(0)?, row.get(1)?)),
            )
            .optional()?;
        let Some((rotation_blob, agent_blob)) = row else {
            return Ok(None);
        };
        let agent: AgentRecord = serde_json::from_slice(&agent_blob)?;
        agent.verify(&address)?;
        let rotation: RotationRecord = serde_json::from_slice(&rotation_blob)?;
        rotation.verify_source_address(&address)?;
        rotation.verify(
            &agent.successor_commitment(),
            agent.seq,
            rotation.activated_at,
        )?;
        Ok(Some(rotation))
    }

    fn put_rotation_record(
        &self,
        address: &str,
        record: &RotationRecord,
        now_secs: u64,
        capacity_bytes: u64,
    ) -> Result<bool> {
        let address = Address::parse(address)?;
        record.verify_source_address(&address)?;
        let blob = serde_json::to_vec(record)?;
        let target = record.target_address()?;
        let new_accounted = accounted_size(
            blob.len(),
            address.as_str().len().saturating_add(target.as_str().len()),
        )?;
        let mut conn = self.lock();
        let tx = conn.transaction_with_behavior(TransactionBehavior::Immediate)?;

        let existing: Option<Vec<u8>> = tx
            .query_row(
                "SELECT record FROM rotation_records WHERE from_address = ?1",
                params![address.as_str()],
                |row| row.get(0),
            )
            .optional()?;
        if let Some(existing) = existing {
            if existing == blob {
                tx.commit()?;
                return Ok(false);
            }
            return Err(pigeonpost_core::Error::StaleSequence.into());
        }

        let agent_row: Option<(Vec<u8>, i64)> = tx
            .query_row(
                "SELECT record, rotation_reservation FROM agent_records WHERE address = ?1",
                params![address.as_str()],
                |row| Ok((row.get(0)?, row.get(1)?)),
            )
            .optional()?;
        let (agent_blob, reservation) = agent_row.ok_or(
            pigeonpost_core::Error::MalformedEnvelope("rotation source record missing"),
        )?;
        let reservation = u64::try_from(reservation)
            .map_err(|_| pigeonpost_core::Error::MalformedEnvelope("negative storage counter"))?;
        if reservation == 0 || new_accounted > reservation {
            return Err(LoftError::AtCapacity);
        }
        let agent: AgentRecord = serde_json::from_slice(&agent_blob)?;
        agent.verify(&address)?;
        record.verify(&agent.successor_commitment(), agent.seq, now_secs)?;

        replace_control_charge(&tx, capacity_bytes, 0, new_accounted, reservation, 0)?;
        let reservation_updated = tx.execute(
            "UPDATE agent_records SET rotation_reservation = 0
             WHERE address = ?1 AND rotation_reservation = ?2",
            params![address.as_str(), to_i64(reservation)?],
        )?;
        if reservation_updated != 1 {
            return Err(
                pigeonpost_core::Error::MalformedEnvelope("rotation reservation mismatch").into(),
            );
        }

        tx.execute(
            "INSERT INTO rotation_records
                (from_address, from_pubkey, to_address, to_pubkey, seq, activated_at,
                 grace_until, record, stored_at, accounted_size)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10)",
            params![
                address.as_str(),
                record.from_pubkey.as_slice(),
                target.as_str(),
                record.to_pubkey.as_slice(),
                to_i64(record.seq)?,
                to_i64(record.activated_at)?,
                to_i64(record.grace_until)?,
                blob,
                to_i64(now_secs)?,
                to_i64(new_accounted)?,
            ],
        )?;
        tx.commit()?;
        Ok(true)
    }

    fn sweep_expired(&self, now: u64, batch: usize) -> Result<usize> {
        if batch == 0 {
            return Ok(0);
        }
        let mut conn = self.lock();
        let tx = conn.transaction_with_behavior(TransactionBehavior::Immediate)?;
        let (count, bytes): (i64, i64) = tx.query_row(
            "SELECT COUNT(*), COALESCE(SUM(size), 0) FROM (
                 SELECT size FROM events WHERE expires_at <= ?1 ORDER BY expires_at LIMIT ?2
             )",
            params![to_i64(now)?, to_i64(batch as u64)?],
            |row| Ok((row.get(0)?, row.get(1)?)),
        )?;
        if count == 0 {
            tx.commit()?;
            return Ok(0);
        }
        let removed = tx.execute(
            "DELETE FROM events WHERE cursor IN (
                 SELECT cursor FROM events WHERE expires_at <= ?1 ORDER BY expires_at LIMIT ?2
             )",
            params![to_i64(now)?, to_i64(batch as u64)?],
        )?;
        if i64::try_from(removed).ok() != Some(count) {
            return Err(
                pigeonpost_core::Error::MalformedEnvelope("retention count mismatch").into(),
            );
        }
        let updated = tx.execute(
            "UPDATE storage_stats SET
                bytes_used = bytes_used - ?1,
                event_count = event_count - ?2
             WHERE singleton = 1 AND bytes_used >= ?1 AND event_count >= ?2",
            params![bytes, count],
        )?;
        if updated != 1 {
            return Err(
                pigeonpost_core::Error::MalformedEnvelope("storage counter mismatch").into(),
            );
        }
        tx.commit()?;
        Ok(removed)
    }

    fn retention_checkpoint(&self) -> Result<()> {
        let conn = self.lock();
        let (busy, _log_frames, _checkpointed): (i64, i64, i64) =
            conn.query_row("PRAGMA wal_checkpoint(TRUNCATE)", [], |row| {
                Ok((row.get(0)?, row.get(1)?, row.get(2)?))
            })?;
        if busy != 0 {
            return Err(LoftError::NotReady);
        }
        Ok(())
    }

    fn stats(&self) -> Result<StorageStats> {
        let conn = self.lock();
        let (event_bytes, control_bytes, control_reserved, count): (i64, i64, i64, i64) = conn
            .query_row(
                "SELECT bytes_used, control_bytes, control_reserved, event_count
             FROM storage_stats WHERE singleton = 1",
                [],
                |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?, row.get(3)?)),
            )?;
        let event_bytes_used = u64::try_from(event_bytes)
            .map_err(|_| pigeonpost_core::Error::MalformedEnvelope("negative storage counter"))?;
        let control_bytes_used = u64::try_from(control_bytes)
            .map_err(|_| pigeonpost_core::Error::MalformedEnvelope("negative storage counter"))?;
        let control_bytes_reserved = u64::try_from(control_reserved)
            .map_err(|_| pigeonpost_core::Error::MalformedEnvelope("negative storage counter"))?;
        Ok(StorageStats {
            bytes_used: event_bytes_used
                .checked_add(control_bytes_used)
                .and_then(|value| value.checked_add(control_bytes_reserved))
                .ok_or(pigeonpost_core::Error::TooLarge)?,
            event_bytes_used,
            control_bytes_used,
            control_bytes_reserved,
            event_count: u64::try_from(count).map_err(|_| {
                pigeonpost_core::Error::MalformedEnvelope("negative storage counter")
            })?,
        })
    }

    fn supports_durable_trace_admission(&self) -> bool {
        true
    }

    fn supports_public_durable_trace_admission(&self) -> bool {
        #[cfg(unix)]
        {
            self.custody.is_some()
        }
        #[cfg(not(unix))]
        {
            false
        }
    }

    fn charge_trace_admission(&self, timestamp_ms: u64, limit: u32) -> Result<()> {
        if timestamp_ms == 0 || limit == 0 {
            return Err(LoftError::Configuration(
                "invalid durable trace admission input",
            ));
        }
        let window_start_ms = timestamp_ms - (timestamp_ms % TRACE_ADMISSION_WINDOW_MS);
        let mut batch = self
            .trace_admission_batch
            .lock()
            .unwrap_or_else(|error| error.into_inner());
        if window_start_ms < batch.highest_requested_window_start_ms {
            return Err(LoftError::TraceUnavailable);
        }
        if window_start_ms > batch.highest_requested_window_start_ms {
            batch.highest_requested_window_start_ms = window_start_ms;
            batch.lease = None;
        }

        let mut conn = self.lock();
        // Even a locally reserved slot performs a read-only validation. This detects a peer
        // process that has already advanced the durable minute and preserves rollback failure
        // without paying for another FULL-synchronous commit.
        let observed = load_trace_admission(&conn)?;
        validate_trace_admission_observation(&mut batch, observed)?;
        if observed.0 > batch.highest_requested_window_start_ms {
            batch.highest_requested_window_start_ms = observed.0;
            batch.lease = None;
        }
        if window_start_ms < observed.0 {
            return Err(LoftError::TraceUnavailable);
        }

        let lease_is_usable = batch.lease.is_some_and(|lease| {
            lease.window_start_ms == window_start_ms && lease.limit == limit && lease.remaining > 0
        });
        if lease_is_usable {
            let lease = batch
                .lease
                .as_mut()
                .expect("usable trace admission lease exists");
            if observed.0 != window_start_ms || observed.1 < lease.reserved_through {
                batch.lease = None;
                return Err(LoftError::TraceUnavailable);
            }
            lease.remaining -= 1;
            return Ok(());
        }
        // A changed runtime limit, exhausted lease, or rollover burns any unused local slots. The
        // durable high-water mark remains charged, so lowering a limit always fails closed.
        batch.lease = None;

        let tx = conn.transaction_with_behavior(TransactionBehavior::Immediate)?;
        let (stored_window, stored_admitted) = load_trace_admission(&tx)?;
        validate_trace_admission_observation(&mut batch, (stored_window, stored_admitted))?;
        if window_start_ms < stored_window {
            batch.highest_requested_window_start_ms = stored_window;
            return Err(LoftError::TraceUnavailable);
        }
        let admitted = if window_start_ms > stored_window {
            0
        } else {
            stored_admitted
        };
        let available = u64::from(limit)
            .checked_sub(admitted)
            .ok_or(LoftError::RateLimited)?;
        if available == 0 {
            return Err(LoftError::RateLimited);
        }
        let reserved = available.min(TRACE_ADMISSION_BATCH_SIZE);
        let next_admitted = admitted
            .checked_add(reserved)
            .ok_or(LoftError::TraceUnavailable)?;
        let updated = tx.execute(
            "UPDATE trace_admission
             SET window_start_ms = ?1, admitted = ?2
             WHERE singleton = 1 AND window_start_ms = ?3 AND admitted = ?4",
            params![
                to_i64(window_start_ms)?,
                to_i64(next_admitted)?,
                to_i64(stored_window)?,
                to_i64(stored_admitted)?,
            ],
        )?;
        if updated != 1 {
            return Err(LoftError::TraceUnavailable);
        }
        tx.commit()?;
        // Publish the process-local remainder only after the FULL-synchronous reservation commit.
        // The current call consumes one of the already durable slots.
        batch.lease = Some(TraceAdmissionLease {
            window_start_ms,
            limit,
            remaining: reserved - 1,
            reserved_through: next_admitted,
        });
        batch.durable_high_water = Some((window_start_ms, next_admitted));
        Ok(())
    }

    fn health_check(&self) -> Result<()> {
        #[cfg(any(unix, windows))]
        if let Some(custody) = &self.custody {
            custody.verify_all_named()?;
        }
        let conn = self.lock();
        let value: i64 = conn.query_row("SELECT 1", [], |row| row.get(0))?;
        if value != 1 {
            return Err(LoftError::NotReady);
        }
        Ok(())
    }
}

impl TraceSegmentCatalog for SqliteStore {
    fn record_trace_segment(&self, metadata: &TraceSegmentMetadata) -> Result<()> {
        validate_trace_segment_metadata(metadata)?;
        let key_id = metadata
            .key_id
            .encode()
            .map_err(|_| LoftError::TraceMetadata)?;
        let purpose = i64::from(u8::from(metadata.key_id.purpose));
        let jurisdiction = i64::from(u8::from(metadata.key_id.jurisdiction));
        let opened_at = to_i64(metadata.opened_at_ms)?;
        let closed_at = metadata.closed_at_ms.map(to_i64).transpose()?;
        let record_count = metadata.record_count.map(i64::from);
        let state = i64::from(metadata.state as u8);
        let mut conn = self.lock();
        let tx = conn.transaction_with_behavior(TransactionBehavior::Immediate)?;

        let changed = tx.execute(
            "INSERT INTO trace_segments
                (segment_id, key_id, purpose, jurisdiction, opened_at, closed_at, path,
                 wrapped_key, record_count, first_hash, final_hash, state)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12)
             ON CONFLICT(segment_id) DO UPDATE SET
                closed_at = excluded.closed_at,
                record_count = excluded.record_count,
                first_hash = excluded.first_hash,
                final_hash = excluded.final_hash,
                state = excluded.state
             WHERE trace_segments.key_id = excluded.key_id
               AND trace_segments.purpose = excluded.purpose
               AND trace_segments.jurisdiction = excluded.jurisdiction
               AND trace_segments.opened_at = excluded.opened_at
               AND trace_segments.path = excluded.path
               AND trace_segments.wrapped_key = excluded.wrapped_key
               AND (
                    (trace_segments.state = 1 AND excluded.state IN (1, 2))
                    OR
                    (trace_segments.state = 2 AND excluded.state = 2
                     AND trace_segments.closed_at = excluded.closed_at
                     AND trace_segments.record_count = excluded.record_count
                     AND trace_segments.first_hash = excluded.first_hash
                     AND trace_segments.final_hash = excluded.final_hash)
               )",
            params![
                metadata.segment_id.as_slice(),
                key_id.as_slice(),
                purpose,
                jurisdiction,
                opened_at,
                closed_at,
                metadata.relative_path,
                metadata.wrapped_key.as_slice(),
                record_count,
                metadata.first_hash.as_ref().map(<[u8; 32]>::as_slice),
                metadata.final_hash.as_ref().map(<[u8; 32]>::as_slice),
                state,
            ],
        )?;
        if changed != 1 {
            return Err(LoftError::TraceMetadata);
        }
        tx.commit()?;
        Ok(())
    }
}

fn validate_trace_segment_metadata(metadata: &TraceSegmentMetadata) -> Result<()> {
    let path = Path::new(&metadata.relative_path);
    if metadata.segment_id == [0u8; 32]
        || metadata.relative_path.is_empty()
        || metadata.relative_path.len() > 255
        || metadata.relative_path.contains('\0')
        || path.file_name().and_then(|name| name.to_str()) != Some(metadata.relative_path.as_str())
        || path.components().count() != 1
        || !matches!(
            metadata.key_id.purpose,
            CompliancePurpose::NetworkTrace | CompliancePurpose::IdentityTrace
        )
        || metadata.opened_at_ms < metadata.key_id.epoch_start_ms
        || metadata.opened_at_ms
            >= metadata
                .key_id
                .epoch_start_ms
                .checked_add(pigeonpost_compliance_seal::TRACE_EPOCH_DURATION_MS)
                .ok_or(LoftError::TraceMetadata)?
    {
        return Err(LoftError::TraceMetadata);
    }
    let wrapped =
        WrappedEpochKey::decode(&metadata.wrapped_key).map_err(|_| LoftError::TraceMetadata)?;
    if wrapped.key_id() != metadata.key_id {
        return Err(LoftError::TraceMetadata);
    }
    match metadata.state {
        TraceSegmentState::Open => {
            if metadata.closed_at_ms.is_some()
                || metadata.record_count.is_some()
                || metadata.first_hash.is_some()
                || metadata.final_hash.is_some()
            {
                return Err(LoftError::TraceMetadata);
            }
        }
        TraceSegmentState::Closed => {
            let closed_at = metadata.closed_at_ms.ok_or(LoftError::TraceMetadata)?;
            if closed_at < metadata.opened_at_ms
                || closed_at
                    >= metadata
                        .key_id
                        .epoch_start_ms
                        .checked_add(pigeonpost_compliance_seal::TRACE_EPOCH_DURATION_MS)
                        .ok_or(LoftError::TraceMetadata)?
                || metadata.record_count.is_none()
                || metadata.first_hash.is_none()
                || metadata.final_hash.is_none()
            {
                return Err(LoftError::TraceMetadata);
            }
        }
    }
    Ok(())
}

fn decode_policy(blob: &[u8]) -> Result<RecipientPolicy> {
    let policy: RecipientPolicy = serde_json::from_slice(blob)?;
    policy.verify(None)?;
    Ok(policy)
}

fn load_trace_admission(conn: &Connection) -> Result<(u64, u64)> {
    let (window_start_ms, admitted): (i64, i64) = conn.query_row(
        "SELECT window_start_ms, admitted FROM trace_admission WHERE singleton = 1",
        [],
        |row| Ok((row.get(0)?, row.get(1)?)),
    )?;
    Ok((
        u64::try_from(window_start_ms)
            .map_err(|_| LoftError::Configuration("invalid durable trace admission counter"))?,
        u64::try_from(admitted)
            .map_err(|_| LoftError::Configuration("invalid durable trace admission counter"))?,
    ))
}

fn to_i64(value: u64) -> Result<i64> {
    i64::try_from(value).map_err(|_| pigeonpost_core::Error::TooLarge.into())
}

fn unix_millis() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|duration| duration.as_millis().min(u128::from(u64::MAX)) as u64)
        .unwrap_or(0)
}

fn unix_seconds() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|duration| duration.as_secs())
        .unwrap_or(0)
}

#[cfg(unix)]
struct DatabaseCustody {
    directory: GuardedDir,
    file: GuardedFile,
    file_name: LeafName,
    path: PathBuf,
    sidecar_names: [LeafName; 2],
    journal_name: LeafName,
    retained_sidecars: Mutex<[Option<GuardedFile>; 2]>,
}

#[cfg(unix)]
impl DatabaseCustody {
    fn open_or_create(requested: &Path) -> Result<Self> {
        // Parse the complete spelling before creating a directory. In particular, a rejected
        // `..` or oversized component must not leave a partially created custody tree behind.
        let normalized = NormalizedPath::new(requested).map_err(map_custody_error)?;
        let file_name = normalized.as_path().file_name().ok_or_else(|| {
            map_custody_error(CustodyError::InvalidPath("database path must name a file"))
        })?;
        let file_name = LeafName::new(file_name).map_err(map_custody_error)?;
        let parent = normalized.as_path().parent().ok_or_else(|| {
            map_custody_error(CustodyError::InvalidPath("database path has no parent"))
        })?;
        let directory = GuardedDir::create_private(parent).map_err(map_custody_error)?;
        let path = directory.absolute_path().join(file_name.as_os_str());
        let sidecar_names = [
            suffixed_leaf(&file_name, "-wal")?,
            suffixed_leaf(&file_name, "-shm")?,
        ];
        let journal_name = suffixed_leaf(&file_name, "-journal")?;

        // Refuse hostile leftovers before creating a new main file. SQLite's bundled Unix VFS also
        // uses O_NOFOLLOW for WAL, SHM, and journal opens; the private parent closes the remaining
        // replacement race to other local users.
        for sidecar in &sidecar_names {
            directory
                .validate_file(sidecar, sqlite_file_policy())
                .map_err(map_custody_error)?;
        }
        directory
            .validate_file(&journal_name, sqlite_file_policy())
            .map_err(map_custody_error)?;
        let file = directory
            .open_or_create_file(&file_name, OpenAccess::ReadWrite, sqlite_file_policy())
            .map_err(map_custody_error)?;
        let custody = Self {
            directory,
            file,
            file_name,
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
        self.directory.verify_named().map_err(map_custody_error)?;
        self.file.verify_named().map_err(map_custody_error)?;
        let named = self
            .directory
            .validate_file(&self.file_name, sqlite_file_policy())
            .map_err(map_custody_error)?
            .ok_or_else(|| map_custody_error(CustodyError::NotFound))?;
        if named.identity != self.file.identity() {
            return Err(map_custody_error(CustodyError::UnsafeFile(
                "database name no longer identifies retained main file",
            )));
        }
        Ok(())
    }

    fn verify_sqlite_connection(&self, conn: &Connection) -> Result<()> {
        if conn.path().map(Path::new) != Some(self.path.as_path()) {
            return Err(map_custody_error(CustodyError::UnsafeFile(
                "SQLite reports a different main database path",
            )));
        }
        self.verify_main_named()?;
        Ok(())
    }

    fn verify_sidecars(&self, require_wal_and_shm: bool) -> Result<()> {
        let mut retained = self
            .retained_sidecars
            .lock()
            .unwrap_or_else(|error| error.into_inner());
        for (name, retained_file) in self.sidecar_names.iter().zip(retained.iter_mut()) {
            if let Some(file) = retained_file {
                file.verify_named().map_err(map_custody_error)?;
                continue;
            }
            match self
                .directory
                .open_file_optional(name, OpenAccess::ReadOnly, sqlite_file_policy())
                .map_err(map_custody_error)?
            {
                Some(file) => *retained_file = Some(file),
                None if require_wal_and_shm => {
                    return Err(map_custody_error(CustodyError::UnsafeFile(
                        "required SQLite WAL or SHM sidecar is missing",
                    )));
                }
                None => {}
            }
        }
        // A rollback journal is transient in WAL mode, so validate it whenever present but do not
        // retain it as a required long-lived object.
        self.directory
            .validate_file(&self.journal_name, sqlite_file_policy())
            .map_err(map_custody_error)?;
        Ok(())
    }

    fn verify_all_named(&self) -> Result<()> {
        self.verify_main_named()?;
        self.verify_sidecars(true)
    }
}

#[cfg(all(unix, test))]
fn sidecar_path(path: &Path, suffix: &str) -> PathBuf {
    let mut value = path.as_os_str().to_os_string();
    value.push(suffix);
    value.into()
}

#[cfg(unix)]
fn sqlite_file_policy() -> FilePolicy {
    FilePolicy::private(MAX_SQLITE_FILE_BYTES)
}

#[cfg(unix)]
fn suffixed_leaf(name: &LeafName, suffix: &str) -> Result<LeafName> {
    let mut value = name.as_os_str().to_os_string();
    value.push(suffix);
    LeafName::new(value).map_err(map_custody_error)
}

#[cfg(unix)]
fn map_custody_error(error: CustodyError) -> LoftError {
    let error = match error {
        CustodyError::Io(error) if custody_io_is_policy_failure(&error) => {
            std::io::Error::new(std::io::ErrorKind::PermissionDenied, error)
        }
        CustodyError::Io(error) => error,
        error => std::io::Error::new(std::io::ErrorKind::PermissionDenied, error),
    };
    LoftError::Io(error)
}

#[cfg(unix)]
fn custody_io_is_policy_failure(error: &std::io::Error) -> bool {
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
mod windows_database_custody {
    use std::ffi::OsString;
    use std::fs::{File, OpenOptions};
    use std::io;
    use std::os::windows::fs::{MetadataExt, OpenOptionsExt};
    use std::path::{Component, Path, PathBuf, Prefix};

    use rusqlite::Connection;
    use winapi_util::file::{information, typ};
    use windows_permissions::constants::{
        AccessRights, AceType, SeObjectType, SecurityInformation,
    };
    use windows_permissions::{wrappers, LocalBox, SecurityDescriptor, Sid};

    use super::{LoftError, Result, MAX_SQLITE_FILE_BYTES};

    const FILE_ATTRIBUTE_DIRECTORY: u32 = 0x0000_0010;
    const FILE_ATTRIBUTE_REPARSE_POINT: u32 = 0x0000_0400;
    const FILE_FLAG_BACKUP_SEMANTICS: u32 = 0x0200_0000;
    const FILE_FLAG_OPEN_REPARSE_POINT: u32 = 0x0020_0000;
    const FILE_ADD_SUBDIRECTORY: u32 = 0x0000_0004;
    const GENERIC_READ: u32 = 0x8000_0000;
    const GENERIC_WRITE: u32 = 0x4000_0000;
    const READ_CONTROL: u32 = 0x0002_0000;
    const WRITE_DAC: u32 = 0x0004_0000;
    const WRITE_OWNER: u32 = 0x0008_0000;
    const FILE_SHARE_READ: u32 = 0x0000_0001;
    const FILE_SHARE_WRITE: u32 = 0x0000_0002;
    const MAX_PATH_COMPONENTS: usize = 128;

    #[derive(Debug)]
    struct GuardedParent {
        path: PathBuf,
        name: Option<OsString>,
        file: File,
        identity: pigeonpost_windows_custody::FileIdentity,
        guards_target_name: bool,
    }

    /// Retained no-delete-share handles for every ancestor of one exact normalized name.
    #[derive(Debug)]
    struct ParentGuard {
        target: PathBuf,
        components: Vec<GuardedParent>,
    }

    /// A private regular file and the retained ancestor chain that binds its exact name.
    #[derive(Debug)]
    struct RetainedPrivateFile {
        path: PathBuf,
        parents: ParentGuard,
        file: File,
    }

    impl RetainedPrivateFile {
        fn verify(&self) -> io::Result<()> {
            verify_private_handle(&self.file, &self.path, false)?;
            verify_same_named_object(&self.file, &self.path, false)?;
            self.parents.verify()
        }

        fn len(&self) -> io::Result<u64> {
            self.verify()?;
            Ok(self.file.metadata()?.len())
        }
    }

    impl ParentGuard {
        fn acquire(path: &Path) -> io::Result<Self> {
            let target = normalized_absolute(path)?;
            let parent = target
                .parent()
                .ok_or_else(|| custody_error(path, "has no parent directory"))?;
            let (anchor_path, names) = split_absolute_parent(parent)?;
            let anchor_guards_target = names.is_empty();
            let anchor = pigeonpost_windows_custody::lock_directory(open_root_anchor(
                &anchor_path,
                anchor_guards_target,
            )?)?;
            verify_parent_descriptor(anchor.file(), &anchor_path, anchor_guards_target)?;
            let (anchor_file, anchor_identity) = anchor.into_parts();
            let name_count = names.len();
            let mut components = Vec::with_capacity(name_count + 1);
            let mut component_path = anchor_path;
            components.push(GuardedParent {
                path: component_path.clone(),
                name: None,
                file: anchor_file,
                identity: anchor_identity,
                guards_target_name: anchor_guards_target,
            });
            for (index, name) in names.into_iter().enumerate() {
                let guards_target_name = index + 1 == name_count;
                let preceding = components
                    .last()
                    .ok_or_else(|| custody_error(path, "has no root anchor"))?;
                let locked = if guards_target_name {
                    pigeonpost_windows_custody::open_directory_for_child(&preceding.file, &name)?
                } else {
                    pigeonpost_windows_custody::open_directory(&preceding.file, &name)?
                };
                component_path.push(&name);
                verify_parent_descriptor(locked.file(), &component_path, guards_target_name)?;
                let (file, identity) = locked.into_parts();
                components.push(GuardedParent {
                    path: component_path.clone(),
                    name: Some(name),
                    file,
                    identity,
                    guards_target_name,
                });
            }
            if components.is_empty() {
                return Err(custody_error(path, "has no guardable parent directory"));
            }
            Ok(Self { target, components })
        }

        fn target(&self) -> &Path {
            &self.target
        }

        fn immediate_parent(&self) -> io::Result<&File> {
            self.components
                .last()
                .map(|component| &component.file)
                .ok_or_else(|| custody_error(&self.target, "has no guarded parent directory"))
        }

        fn verify(&self) -> io::Result<()> {
            for (index, component) in self.components.iter().enumerate() {
                verify_disk_object(&component.file, &component.path, true)?;
                let reopened = if let Some(name) = &component.name {
                    let preceding = index
                        .checked_sub(1)
                        .and_then(|preceding| self.components.get(preceding))
                        .ok_or_else(|| {
                            custody_error(&component.path, "has no retained preceding ancestor")
                        })?;
                    if component.guards_target_name {
                        pigeonpost_windows_custody::open_directory_for_child(&preceding.file, name)?
                    } else {
                        pigeonpost_windows_custody::open_directory(&preceding.file, name)?
                    }
                } else {
                    pigeonpost_windows_custody::lock_directory(open_root_anchor(
                        &component.path,
                        component.guards_target_name,
                    )?)?
                };
                if reopened.identity() != component.identity {
                    return Err(custody_error(
                        &component.path,
                        "changed while custody checks were running",
                    ));
                }
                verify_parent_descriptor(
                    &component.file,
                    &component.path,
                    component.guards_target_name,
                )?;
            }
            Ok(())
        }

        fn verify_private_parent(&self) -> io::Result<()> {
            let parent = self
                .components
                .last()
                .ok_or_else(|| custody_error(&self.target, "has no guarded parent directory"))?;
            verify_private_handle(&parent.file, &parent.path, true)
        }
    }

    pub(super) struct WindowsDatabasePreparation {
        pub(super) path: PathBuf,
        main: RetainedPrivateFile,
        wal_path: PathBuf,
        shm_path: PathBuf,
        journal_path: PathBuf,
        preexisting_wal: Option<RetainedPrivateFile>,
        preexisting_shm: Option<RetainedPrivateFile>,
        journal_preexisted: bool,
    }

    impl WindowsDatabasePreparation {
        pub(super) fn open_or_create(requested: &Path) -> Result<Self> {
            let path = normalized_absolute(requested)?;
            if path.file_name().is_none() {
                return Err(custody_error(&path, "must name a database file").into());
            }
            let parent = path
                .parent()
                .ok_or_else(|| custody_error(&path, "has no parent directory"))?;
            let wal_path = sidecar_path(&path, "-wal")?;
            let shm_path = sidecar_path(&path, "-shm")?;
            let journal_path = sidecar_path(&path, "-journal")?;
            // Parse every derived name before the first directory creation side effect.
            secure_or_create_private_directory(parent)?;

            // Validate hostile leftovers before SQLite can parse, truncate, recover, or replace
            // them. Existing WAL/SHM handles remain held across SQLite initialization so the exact
            // validated objects cannot be rebound between this check and SQLite's own open.
            let preexisting_wal = retain_private_file_optional(&wal_path, false)?;
            validate_retained_optional(preexisting_wal.as_ref())?;
            let preexisting_shm = retain_private_file_optional(&shm_path, false)?;
            validate_retained_optional(preexisting_shm.as_ref())?;
            let journal = retain_private_file_optional(&journal_path, false)?;
            validate_retained_optional(journal.as_ref())?;
            let journal_preexisted = journal.is_some();
            drop(journal);

            let (main, _created) = retain_or_create_private_file(&path)?;
            validate_retained_file(&main)?;
            Ok(Self {
                path,
                main,
                wal_path,
                shm_path,
                journal_path,
                preexisting_wal,
                preexisting_shm,
                journal_preexisted,
            })
        }

        pub(super) fn verify_main_named(&self) -> Result<()> {
            validate_retained_file(&self.main)
        }

        pub(super) fn verify_sqlite_connection(&self, conn: &Connection) -> Result<()> {
            if conn.path().map(Path::new) != Some(self.path.as_path()) {
                return Err(custody_error(
                    &self.path,
                    "SQLite reports a different main database path",
                )
                .into());
            }
            // The retained main file and complete ancestor chain omit delete sharing before SQLite
            // receives this exact normalized path, so a second spelling cannot be substituted.
            self.verify_main_named()
        }

        pub(super) fn finish(mut self) -> Result<DatabaseCustody> {
            if self.journal_preexisted {
                validate_retained_optional(
                    retain_private_file_optional(&self.journal_path, false)?.as_ref(),
                )?;
            } else {
                harden_optional_subsystem_file(&self.journal_path)?;
            }

            let wal = match self.preexisting_wal.take() {
                Some(file) => file,
                None => retain_and_protect_subsystem_file(&self.wal_path)
                    .map_err(|error| required_sidecar_error(error, &self.wal_path, "WAL"))?,
            };
            let shm = match self.preexisting_shm.take() {
                Some(file) => file,
                None => retain_and_protect_subsystem_file(&self.shm_path)
                    .map_err(|error| required_sidecar_error(error, &self.shm_path, "SHM"))?,
            };
            validate_retained_file(&wal)?;
            validate_retained_file(&shm)?;
            let custody = DatabaseCustody {
                main: self.main,
                wal,
                shm,
                journal_path: self.journal_path,
            };
            custody.verify_all_named()?;
            Ok(custody)
        }
    }

    pub(super) struct DatabaseCustody {
        main: RetainedPrivateFile,
        wal: RetainedPrivateFile,
        shm: RetainedPrivateFile,
        journal_path: PathBuf,
    }

    impl DatabaseCustody {
        pub(super) fn verify_all_named(&self) -> Result<()> {
            validate_retained_file(&self.main)?;
            validate_retained_file(&self.wal)?;
            validate_retained_file(&self.shm)?;
            validate_retained_optional(
                retain_private_file_optional(&self.journal_path, false)?.as_ref(),
            )
        }
    }

    fn sidecar_path(path: &Path, suffix: &str) -> io::Result<PathBuf> {
        let name = path
            .file_name()
            .ok_or_else(|| custody_error(path, "must name a database file"))?;
        let mut sidecar_name = name.to_os_string();
        sidecar_name.push(suffix);
        pigeonpost_windows_custody::validate_component(&sidecar_name)?;
        let parent = path
            .parent()
            .ok_or_else(|| custody_error(path, "has no parent directory"))?;
        let sidecar = parent.join(sidecar_name);
        if normalized_absolute(&sidecar)? != sidecar {
            return Err(custody_error(
                &sidecar,
                "sidecar path is not exactly normalized",
            ));
        }
        Ok(sidecar)
    }

    fn validate_retained_optional(file: Option<&RetainedPrivateFile>) -> Result<()> {
        if let Some(file) = file {
            validate_retained_file(file)?;
        }
        Ok(())
    }

    fn validate_retained_file(file: &RetainedPrivateFile) -> Result<()> {
        file.verify()?;
        if file.len()? > MAX_SQLITE_FILE_BYTES {
            return Err(custody_error(&file.path, "exceeds the supported size bound").into());
        }
        Ok(())
    }

    fn harden_optional_subsystem_file(path: &Path) -> Result<()> {
        match retain_and_protect_subsystem_file(path) {
            Ok(file) => validate_retained_file(&file),
            Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(()),
            Err(error) => Err(error.into()),
        }
    }

    fn required_sidecar_error(error: io::Error, path: &Path, sidecar: &str) -> LoftError {
        if error.kind() == io::ErrorKind::NotFound {
            custody_error(path, &format!("required {sidecar} sidecar is missing")).into()
        } else {
            error.into()
        }
    }

    fn secure_or_create_private_directory(path: &Path) -> io::Result<()> {
        let path = normalized_absolute(path)?;
        secure_private_directory_recursive(&path, 0)
    }

    fn secure_private_directory_recursive(path: &Path, depth: usize) -> io::Result<()> {
        if depth >= MAX_PATH_COMPONENTS {
            return Err(custody_error(
                path,
                "has too many missing directory components",
            ));
        }
        match secure_private_directory_once(path) {
            Ok(()) => Ok(()),
            Err(error) if error.kind() == io::ErrorKind::NotFound => {
                let parent = path
                    .parent()
                    .filter(|parent| parent.file_name().is_some())
                    .ok_or(error)?;
                secure_private_directory_recursive(parent, depth + 1)?;
                secure_private_directory_once(path)
            }
            Err(error) => Err(error),
        }
    }

    fn secure_private_directory_once(path: &Path) -> io::Result<()> {
        let parents = ParentGuard::acquire(path)?;
        let path = parents.target();
        let name = path
            .file_name()
            .ok_or_else(|| custody_error(path, "has no final directory name"))?;
        let locked = match pigeonpost_windows_custody::create_private_directory(
            parents.immediate_parent()?,
            name,
        )? {
            pigeonpost_windows_custody::CreateDirectory::Created(directory) => directory,
            pigeonpost_windows_custody::CreateDirectory::AlreadyExists => {
                pigeonpost_windows_custody::open_directory(parents.immediate_parent()?, name)?
            }
        };
        let identity = locked.identity();
        verify_private_handle(locked.file(), path, true)?;
        let reopened =
            pigeonpost_windows_custody::open_directory(parents.immediate_parent()?, name)?;
        if reopened.identity() != identity {
            return Err(custody_error(
                path,
                "changed while custody checks were running",
            ));
        }
        parents.verify()
    }

    fn guard_private_parent(path: &Path) -> io::Result<ParentGuard> {
        let guard = ParentGuard::acquire(path)?;
        guard.verify_private_parent()?;
        guard.verify()?;
        Ok(guard)
    }

    fn retain_private_file_optional(
        path: &Path,
        writable: bool,
    ) -> io::Result<Option<RetainedPrivateFile>> {
        let parents = guard_private_parent(path)?;
        let path = parents.target().to_path_buf();
        let file = match open_existing_file(&path, writable) {
            Ok(file) => file,
            Err(error) if error.kind() == io::ErrorKind::NotFound => {
                parents.verify()?;
                return Ok(None);
            }
            Err(error) => return Err(error),
        };
        let retained = RetainedPrivateFile {
            path,
            parents,
            file,
        };
        retained.verify()?;
        Ok(Some(retained))
    }

    fn retain_or_create_private_file(path: &Path) -> io::Result<(RetainedPrivateFile, bool)> {
        let parents = guard_private_parent(path)?;
        let path = parents.target().to_path_buf();
        let mut create = OpenOptions::new();
        create
            .read(true)
            .write(true)
            .create_new(true)
            .access_mode(GENERIC_READ | GENERIC_WRITE | READ_CONTROL | WRITE_DAC | WRITE_OWNER)
            .share_mode(FILE_SHARE_READ | FILE_SHARE_WRITE)
            .custom_flags(FILE_FLAG_OPEN_REPARSE_POINT);
        let (mut file, created) = match create.open(&path) {
            Ok(file) => (file, true),
            Err(error) if error.kind() == io::ErrorKind::AlreadyExists => {
                (open_existing_file(&path, true)?, false)
            }
            Err(error) => return Err(error),
        };
        if created {
            protect_private_file(&mut file, &path)?;
            file.sync_all()?;
        }
        let retained = RetainedPrivateFile {
            path,
            parents,
            file,
        };
        retained.verify()?;
        Ok((retained, created))
    }

    /// Harden a file SQLite created after all pre-existing names were rejected, then retain it.
    fn retain_and_protect_subsystem_file(path: &Path) -> io::Result<RetainedPrivateFile> {
        let parents = guard_private_parent(path)?;
        let path = parents.target().to_path_buf();
        let mut options = OpenOptions::new();
        options
            .read(true)
            .write(true)
            .access_mode(GENERIC_READ | GENERIC_WRITE | READ_CONTROL | WRITE_DAC | WRITE_OWNER)
            .share_mode(FILE_SHARE_READ | FILE_SHARE_WRITE)
            .custom_flags(FILE_FLAG_OPEN_REPARSE_POINT);
        let mut file = options.open(&path)?;
        verify_disk_object(&file, &path, false)?;
        if information(&file).map_err(other)?.number_of_links() != 1 {
            return Err(custody_error(&path, "must have exactly one hard link"));
        }
        verify_same_named_object(&file, &path, false)?;
        parents.verify()?;
        protect_private_file(&mut file, &path)?;
        file.sync_all()?;
        let retained = RetainedPrivateFile {
            path,
            parents,
            file,
        };
        retained.verify()?;
        Ok(retained)
    }

    fn protect_private_file(file: &mut File, path: &Path) -> io::Result<()> {
        let current = windows_permissions::utilities::current_process_sid().map_err(other)?;
        let descriptor = private_descriptor(&current, false)?;
        let dacl = descriptor
            .dacl()
            .ok_or_else(|| custody_error(path, "private descriptor has no DACL"))?;
        wrappers::SetSecurityInfo(
            file,
            SeObjectType::SE_FILE_OBJECT,
            SecurityInformation::Owner
                | SecurityInformation::Dacl
                | SecurityInformation::ProtectedDacl,
            Some(&current),
            None,
            Some(dacl),
            None,
        )
        .map_err(other)?;
        verify_private_handle(file, path, false)
    }

    fn open_existing_file(path: &Path, writable: bool) -> io::Result<File> {
        let mut options = OpenOptions::new();
        options
            .read(true)
            .share_mode(FILE_SHARE_READ | FILE_SHARE_WRITE)
            .custom_flags(FILE_FLAG_OPEN_REPARSE_POINT);
        if writable {
            options
                .write(true)
                .access_mode(GENERIC_READ | GENERIC_WRITE | READ_CONTROL);
        } else {
            options.access_mode(GENERIC_READ | READ_CONTROL);
        }
        options.open(path)
    }

    fn open_directory_readonly(path: &Path) -> io::Result<File> {
        let mut options = OpenOptions::new();
        options
            .read(true)
            .access_mode(GENERIC_READ | READ_CONTROL)
            .share_mode(FILE_SHARE_READ | FILE_SHARE_WRITE)
            .custom_flags(FILE_FLAG_OPEN_REPARSE_POINT | FILE_FLAG_BACKUP_SEMANTICS);
        options.open(path)
    }

    fn open_root_anchor(path: &Path, can_create_child: bool) -> io::Result<File> {
        let mut options = OpenOptions::new();
        let mut access = GENERIC_READ | READ_CONTROL;
        if can_create_child {
            access |= FILE_ADD_SUBDIRECTORY;
        }
        options
            .read(true)
            .access_mode(access)
            .share_mode(FILE_SHARE_READ | FILE_SHARE_WRITE)
            .custom_flags(FILE_FLAG_OPEN_REPARSE_POINT | FILE_FLAG_BACKUP_SEMANTICS);
        options.open(path)
    }

    fn split_absolute_parent(path: &Path) -> io::Result<(PathBuf, Vec<OsString>)> {
        let mut anchor = PathBuf::new();
        let mut names = Vec::new();
        for component in path.components() {
            match component {
                Component::Prefix(_) | Component::RootDir => anchor.push(component.as_os_str()),
                Component::Normal(name) => names.push(name.to_os_string()),
                Component::CurDir | Component::ParentDir => {
                    return Err(custody_error(
                        path,
                        "contains a non-normalized ancestor component",
                    ));
                }
            }
        }
        if !anchor.is_absolute() {
            return Err(custody_error(path, "has no local volume/root anchor"));
        }
        Ok((anchor, names))
    }

    fn private_descriptor(
        current: &Sid,
        directory: bool,
    ) -> io::Result<LocalBox<SecurityDescriptor>> {
        let ace_flags = if directory { "OICI" } else { "" };
        format!("O:{current}D:P(A;{ace_flags};FA;;;{current})")
            .parse()
            .map_err(other)
    }

    fn security_descriptor(file: &File) -> io::Result<LocalBox<SecurityDescriptor>> {
        wrappers::GetSecurityInfo(
            file,
            SeObjectType::SE_FILE_OBJECT,
            SecurityInformation::Owner | SecurityInformation::Dacl,
        )
        .map_err(other)
    }

    fn verify_private_handle(file: &File, path: &Path, directory: bool) -> io::Result<()> {
        verify_disk_object(file, path, directory)?;
        let info = information(file).map_err(other)?;
        if !directory && info.number_of_links() != 1 {
            return Err(custody_error(path, "must have exactly one hard link"));
        }
        let descriptor = security_descriptor(file)?;
        let current = windows_permissions::utilities::current_process_sid().map_err(other)?;
        if descriptor.owner() != Some(&*current) {
            return Err(custody_error(path, "must be owned by the current user"));
        }
        let dacl = descriptor
            .dacl()
            .ok_or_else(|| custody_error(path, "must have a non-null private DACL"))?;
        if dacl.len() != 1 {
            return Err(custody_error(
                path,
                "must grant access only to the current user",
            ));
        }
        let ace = dacl
            .get_ace(0)
            .ok_or_else(|| custody_error(path, "private DACL is malformed"))?;
        let inheritance_is_private = !directory
            || ace.flags().contains(
                windows_permissions::constants::AceFlags::ObjectInherit
                    | windows_permissions::constants::AceFlags::ContainerInherit,
            );
        if ace.ace_type() != AceType::ACCESS_ALLOWED_ACE_TYPE
            || ace.sid() != Some(&*current)
            || !ace.mask().contains(AccessRights::FileAllAccess)
            || !inheritance_is_private
        {
            return Err(custody_error(
                path,
                "must grant full access only to the current user",
            ));
        }
        let sddl = wrappers::ConvertSecurityDescriptorToStringSecurityDescriptor(
            &descriptor,
            SecurityInformation::Owner | SecurityInformation::Dacl,
        )
        .map_err(other)?;
        if !sddl.to_string_lossy().contains("D:P") {
            return Err(custody_error(path, "must have a protected DACL"));
        }
        Ok(())
    }

    fn verify_disk_object(file: &File, path: &Path, directory: bool) -> io::Result<()> {
        let metadata = file.metadata()?;
        let attributes = metadata.file_attributes();
        let is_directory = attributes & FILE_ATTRIBUTE_DIRECTORY != 0;
        if attributes & FILE_ATTRIBUTE_REPARSE_POINT != 0
            || is_directory != directory
            || !typ(file).map_err(other)?.is_disk()
        {
            let expected = if directory {
                "directory"
            } else {
                "regular file"
            };
            return Err(custody_error(
                path,
                &format!("must be a disk {expected}, not a reparse point"),
            ));
        }
        Ok(())
    }

    fn verify_same_named_object(file: &File, path: &Path, directory: bool) -> io::Result<()> {
        let named = if directory {
            open_directory_readonly(path)?
        } else {
            open_existing_file(path, false)?
        };
        verify_disk_object(&named, path, directory)?;
        let opened_identity = pigeonpost_windows_custody::file_identity(file)?;
        let named_identity = pigeonpost_windows_custody::file_identity(&named)?;
        let named_info = information(&named).map_err(other)?;
        if opened_identity != named_identity || (!directory && named_info.number_of_links() != 1) {
            return Err(custody_error(
                path,
                "changed while custody checks were running",
            ));
        }
        Ok(())
    }

    fn normalized_absolute(path: &Path) -> io::Result<PathBuf> {
        if path.as_os_str().is_empty() {
            return Err(custody_error(path, "must not be empty"));
        }
        let first = path.components().next();
        if matches!(first, Some(Component::Prefix(_))) && !path.has_root() {
            return Err(custody_error(path, "must not be drive-relative"));
        }
        if path.has_root() && !matches!(first, Some(Component::Prefix(_))) {
            return Err(custody_error(path, "must include an explicit drive prefix"));
        }
        let input = if path.is_absolute() {
            path.to_path_buf()
        } else {
            std::env::current_dir()?.join(path)
        };
        let mut normalized = PathBuf::new();
        let mut components = 0_usize;
        for component in input.components() {
            match component {
                Component::CurDir => {}
                Component::ParentDir => {
                    return Err(custody_error(
                        path,
                        "must not contain a parent-directory component",
                    ));
                }
                Component::Prefix(prefix) => match prefix.kind() {
                    Prefix::Disk(_) | Prefix::VerbatimDisk(_) => {
                        normalized.push(prefix.as_os_str())
                    }
                    _ => {
                        return Err(custody_error(
                            path,
                            "must use a local disk path, not UNC or a device namespace",
                        ));
                    }
                },
                Component::RootDir => normalized.push(component.as_os_str()),
                Component::Normal(part) => {
                    components = components.saturating_add(1);
                    if components > MAX_PATH_COMPONENTS {
                        return Err(custody_error(path, "has too many path components"));
                    }
                    pigeonpost_windows_custody::validate_component(part)
                        .map_err(|error| custody_error(path, &error.to_string()))?;
                    normalized.push(part);
                }
            }
        }
        if !normalized.is_absolute() {
            return Err(custody_error(
                path,
                "must resolve to an absolute local disk path",
            ));
        }
        let encoded = normalized
            .to_str()
            .ok_or_else(|| custody_error(path, "must be losslessly Unicode on Windows"))?;
        if encoded.contains('\0') || encoded.encode_utf16().count() > 32_767 {
            return Err(custody_error(
                path,
                "must contain no embedded NUL and fit the Windows path limit",
            ));
        }
        Ok(normalized)
    }

    fn verify_parent_descriptor(
        directory: &File,
        path: &Path,
        guards_target_name: bool,
    ) -> io::Result<()> {
        let descriptor = security_descriptor(directory)?;
        let current = windows_permissions::utilities::current_process_sid().map_err(other)?;
        let owner = descriptor
            .owner()
            .ok_or_else(|| custody_error(path, "parent component has no owner"))?;
        if !trusted_principal(owner, &current) {
            return Err(custody_error(
                path,
                "parent component has an untrusted owner",
            ));
        }
        let dacl = descriptor
            .dacl()
            .ok_or_else(|| custody_error(path, "parent component has a null DACL"))?;
        for index in 0..dacl.len() {
            let ace = dacl
                .get_ace(index)
                .ok_or_else(|| custody_error(path, "parent DACL is malformed"))?;
            if ace
                .flags()
                .contains(windows_permissions::constants::AceFlags::InheritOnly)
                || !is_allow_ace(ace.ace_type())
            {
                continue;
            }
            let sid = ace
                .sid()
                .ok_or_else(|| custody_error(path, "parent allow ACE has no SID"))?;
            if !trusted_principal(sid, &current)
                && dangerous_parent_rights(ace.mask(), guards_target_name)
            {
                return Err(custody_error(
                    path,
                    "parent grants mutation rights to an untrusted principal",
                ));
            }
        }
        Ok(())
    }

    fn is_allow_ace(ace_type: AceType) -> bool {
        matches!(
            ace_type,
            AceType::ACCESS_ALLOWED_ACE_TYPE
                | AceType::ACCESS_ALLOWED_CALLBACK_ACE_TYPE
                | AceType::ACCESS_ALLOWED_CALLBACK_OBJECT_ACE_TYPE
                | AceType::ACCESS_ALLOWED_OBJECT_ACE_TYPE
        )
    }

    fn dangerous_parent_rights(rights: AccessRights, guards_target_name: bool) -> bool {
        rights.intersects(
            AccessRights::GenericAll
                | AccessRights::GenericWrite
                | AccessRights::Delete
                | AccessRights::WriteDac
                | AccessRights::WriteOwner
                | AccessRights::Bit4
                | AccessRights::Bit6
                | AccessRights::Bit8,
        ) || (guards_target_name && rights.intersects(AccessRights::Bit1 | AccessRights::Bit2))
    }

    fn trusted_principal(sid: &Sid, current: &Sid) -> bool {
        sid == current
            || matches!(
                sid.to_string().as_str(),
                // LocalSystem, BUILTIN\\Administrators, and Windows Modules Installer.
                "S-1-5-18"
                    | "S-1-5-32-544"
                    | "S-1-5-80-956008885-3418522649-1831038044-1853292631-2271478464"
            )
    }

    fn custody_error(path: &Path, reason: &str) -> io::Error {
        io::Error::new(
            io::ErrorKind::PermissionDenied,
            format!("loft storage {} {reason}", path.display()),
        )
    }

    fn other(error: impl std::fmt::Display) -> io::Error {
        io::Error::other(error.to_string())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use pigeonpost_compliance_format::{ComplianceKeyId, Jurisdiction};
    use pigeonpost_compliance_seal::EpochSealingKey;
    use pigeonpost_core::{envelope, keys::SuccessorCommitment, Identity};
    use std::sync::{Arc, Barrier};

    #[test]
    fn persistent_store_platform_guard_is_fail_closed() {
        assert!(matches!(
            require_supported_persistent_store_for(false),
            Err(LoftError::Io(error)) if error.kind() == std::io::ErrorKind::Unsupported
        ));
        require_supported_persistent_store_for(true).unwrap();
    }

    #[cfg(not(any(target_os = "linux", target_os = "macos", windows)))]
    #[test]
    fn unsupported_persistent_store_rejects_before_path_creation() {
        let root = tempfile::tempdir().unwrap();
        let database = root.path().join("must-not-exist/loft.db");
        assert!(matches!(
            SqliteStore::open(database.to_str().unwrap()),
            Err(LoftError::Io(error)) if error.kind() == std::io::ErrorKind::Unsupported
        ));
        assert!(!database.parent().unwrap().exists());
    }

    fn wrap_for(to: &Identity, body: &str) -> Wrap {
        let from = Identity::from_seed([1; 32]);
        envelope::wrap(&from, &to.verifying_key(), body, 1_786_105_721).unwrap()
    }

    fn private_database_path() -> (tempfile::TempDir, std::path::PathBuf) {
        let directory = tempfile::tempdir().unwrap();
        #[cfg(windows)]
        {
            let path = directory.path().join("private/loft.db");
            drop(WindowsDatabasePreparation::open_or_create(&path).unwrap());
            (directory, path)
        }
        #[cfg(not(windows))]
        {
            #[cfg(unix)]
            {
                use std::os::unix::fs::PermissionsExt;
                std::fs::set_permissions(directory.path(), std::fs::Permissions::from_mode(0o700))
                    .unwrap();
            }
            let file = tempfile::NamedTempFile::new_in(directory.path()).unwrap();
            let (_file, path) = file.keep().unwrap();
            (directory, path)
        }
    }

    fn deployed_v1_policy(owner: &Identity, seq: u64) -> RecipientPolicy {
        let pubkey = owner.verifying_key().to_bytes();
        let mut payload = b"pigeonpost/recipient-policy/v1".to_vec();
        payload.extend_from_slice(&pubkey);
        payload.extend_from_slice(&0u32.to_le_bytes());
        payload.push(0);
        payload.extend_from_slice(&0u32.to_le_bytes());
        payload.extend_from_slice(&seq.to_le_bytes());
        payload.push(0);
        RecipientPolicy {
            version: LEGACY_RECIPIENT_POLICY_VERSION,
            pubkey,
            pow_min: 0,
            token_required: false,
            token_hashes: Vec::new(),
            attribution_required: false,
            attribution_requirement: None,
            seq,
            signature: owner.sign(&payload).to_bytes(),
        }
    }

    fn create_loft_schema(conn: &mut Connection, version: u32) {
        conn.execute_batch(V0_1_0_SCHEMA).unwrap();
        for target in 2..=version {
            let tx = conn
                .transaction_with_behavior(TransactionBehavior::Immediate)
                .unwrap();
            apply_loft_migration(&tx, target).unwrap();
            tx.commit().unwrap();
        }
        conn.pragma_update(None, "user_version", version).unwrap();
    }

    fn open_trace_metadata() -> TraceSegmentMetadata {
        let key_id = ComplianceKeyId::new(
            CompliancePurpose::NetworkTrace,
            Jurisdiction::Test,
            [3; 32],
            86_400_000,
            1,
        );
        let epoch = EpochSealingKey::from_bytes(key_id, [4; 32]).unwrap();
        let wrapped_key = WrappedEpochKey::wrap(&epoch, &[9; 32])
            .unwrap()
            .encode()
            .unwrap();
        TraceSegmentMetadata {
            segment_id: [5; 32],
            key_id,
            opened_at_ms: 86_400_001,
            closed_at_ms: None,
            relative_path: "segment-0001.pptrace".to_owned(),
            wrapped_key,
            record_count: None,
            first_hash: None,
            final_hash: None,
            state: TraceSegmentState::Open,
        }
    }

    #[test]
    fn stores_fetches_deduplicates_and_tracks_accounted_bytes() {
        let store = SqliteStore::in_memory().unwrap();
        let bob = Identity::from_seed([2; 32]);
        let wrap = wrap_for(&bob, "once");
        assert!(store
            .admit(&wrap, &wrap.id(), 100, 200, u64::MAX, None)
            .unwrap());
        assert!(!store
            .admit(&wrap, &wrap.id(), 100, 200, u64::MAX, None)
            .unwrap());
        let events = store.fetch(&bob.verifying_key().to_bytes(), 0, 10).unwrap();
        assert_eq!(events.len(), 1);
        assert_eq!(store.event_count().unwrap(), 1);
        assert!(store.bytes_used().unwrap() >= EVENT_STORAGE_OVERHEAD);
    }

    #[test]
    fn persistent_admissions_use_full_durability_and_survive_reopen() {
        let temp = tempfile::tempdir().unwrap();
        let path = temp.path().join("private-storage/loft.db");
        let bob = Identity::from_seed([2; 32]);
        let wrap = wrap_for(&bob, "durable acknowledgement");
        let id = wrap.id();

        {
            let store = SqliteStore::open(path.to_str().unwrap()).unwrap();
            let synchronous: i64 = store
                .conn
                .lock()
                .unwrap_or_else(|error| error.into_inner())
                .pragma_query_value(None, "synchronous", |row| row.get(0))
                .unwrap();
            assert_eq!(synchronous, 2, "SQLite FULL is numeric synchronous level 2");
            assert!(store.admit(&wrap, &id, 100, 200, u64::MAX, None).unwrap());
        }

        let reopened = SqliteStore::open(path.to_str().unwrap()).unwrap();
        let fetched = reopened
            .fetch(bob.verifying_key().as_bytes(), 0, 10)
            .unwrap();
        assert_eq!(fetched.len(), 1);
        assert_eq!(fetched[0].wrap.id(), id);
    }

    #[test]
    fn only_owner_custodied_file_storage_claims_public_restart_durability() {
        let memory = SqliteStore::in_memory().unwrap();
        assert!(!memory.supports_public_durable_trace_admission());

        let temp = tempfile::tempdir().unwrap();
        let path = temp.path().join("private-storage/loft.db");
        let file = SqliteStore::open(path.to_str().unwrap()).unwrap();
        #[cfg(unix)]
        assert!(file.supports_public_durable_trace_admission());
        #[cfg(not(unix))]
        assert!(!file.supports_public_durable_trace_admission());
    }

    #[test]
    fn trace_admission_limit_survives_restart_and_rejects_clock_rollback() {
        let temp = tempfile::tempdir().unwrap();
        let path = temp.path().join("private-storage/loft.db");
        const WINDOW: u64 = 120_000;
        {
            let store = SqliteStore::open(path.to_str().unwrap()).unwrap();
            assert!(store.supports_durable_trace_admission());
            store.charge_trace_admission(WINDOW + 1, 2).unwrap();
            store.charge_trace_admission(WINDOW + 59_999, 2).unwrap();
            assert!(matches!(
                store.charge_trace_admission(WINDOW + 2, 2),
                Err(LoftError::RateLimited)
            ));
        }

        let reopened = SqliteStore::open(path.to_str().unwrap()).unwrap();
        assert!(matches!(
            reopened.charge_trace_admission(WINDOW + 3, 2),
            Err(LoftError::RateLimited)
        ));
        reopened
            .charge_trace_admission(WINDOW + TRACE_ADMISSION_WINDOW_MS, 2)
            .unwrap();
        assert!(matches!(
            reopened.charge_trace_admission(WINDOW, 2),
            Err(LoftError::TraceUnavailable)
        ));
    }

    #[test]
    fn trace_admission_reservations_amortize_commits_and_enforce_nonmultiple_limit() {
        let store = SqliteStore::in_memory().unwrap();
        const WINDOW: u64 = 180_000;
        let limit = u32::try_from(TRACE_ADMISSION_BATCH_SIZE * 2 + 2).unwrap();

        store.charge_trace_admission(WINDOW + 1, limit).unwrap();
        assert_eq!(
            load_trace_admission(&store.lock()).unwrap(),
            (WINDOW, TRACE_ADMISSION_BATCH_SIZE)
        );
        for _ in 1..TRACE_ADMISSION_BATCH_SIZE {
            store.charge_trace_admission(WINDOW + 2, limit).unwrap();
        }
        assert_eq!(
            load_trace_admission(&store.lock()).unwrap(),
            (WINDOW, TRACE_ADMISSION_BATCH_SIZE)
        );

        store.charge_trace_admission(WINDOW + 3, limit).unwrap();
        assert_eq!(
            load_trace_admission(&store.lock()).unwrap(),
            (WINDOW, TRACE_ADMISSION_BATCH_SIZE * 2)
        );
        for _ in 1..TRACE_ADMISSION_BATCH_SIZE {
            store.charge_trace_admission(WINDOW + 4, limit).unwrap();
        }
        store.charge_trace_admission(WINDOW + 5, limit).unwrap();
        assert_eq!(
            load_trace_admission(&store.lock()).unwrap(),
            (WINDOW, u64::from(limit))
        );
        store.charge_trace_admission(WINDOW + 6, limit).unwrap();
        assert!(matches!(
            store.charge_trace_admission(WINDOW + 7, limit),
            Err(LoftError::RateLimited)
        ));
    }

    #[test]
    fn trace_admission_unused_reserve_burns_across_restart() {
        let temp = tempfile::tempdir().unwrap();
        let path = temp.path().join("private-storage/loft.db");
        const WINDOW: u64 = 240_000;
        let limit = u32::try_from(TRACE_ADMISSION_BATCH_SIZE + 1).unwrap();
        {
            let store = SqliteStore::open(path.to_str().unwrap()).unwrap();
            store.charge_trace_admission(WINDOW + 1, limit).unwrap();
            assert_eq!(
                load_trace_admission(&store.lock()).unwrap(),
                (WINDOW, TRACE_ADMISSION_BATCH_SIZE)
            );
        }

        let reopened = SqliteStore::open(path.to_str().unwrap()).unwrap();
        reopened.charge_trace_admission(WINDOW + 2, limit).unwrap();
        assert_eq!(
            load_trace_admission(&reopened.lock()).unwrap(),
            (WINDOW, u64::from(limit))
        );
        assert!(matches!(
            reopened.charge_trace_admission(WINDOW + 3, limit),
            Err(LoftError::RateLimited)
        ));
    }

    #[test]
    fn trace_admission_batches_are_exact_across_process_connections() {
        let temp = tempfile::tempdir().unwrap();
        let path = temp.path().join("private-storage/loft.db");
        let first = SqliteStore::open(path.to_str().unwrap()).unwrap();
        let second = SqliteStore::open(path.to_str().unwrap()).unwrap();
        const WINDOW: u64 = 300_000;
        let limit = u32::try_from(TRACE_ADMISSION_BATCH_SIZE * 2 + 2).unwrap();

        first.charge_trace_admission(WINDOW + 1, limit).unwrap();
        second.charge_trace_admission(WINDOW + 1, limit).unwrap();
        for _ in 1..TRACE_ADMISSION_BATCH_SIZE {
            first.charge_trace_admission(WINDOW + 2, limit).unwrap();
            second.charge_trace_admission(WINDOW + 2, limit).unwrap();
        }
        first.charge_trace_admission(WINDOW + 3, limit).unwrap();
        assert!(matches!(
            second.charge_trace_admission(WINDOW + 3, limit),
            Err(LoftError::RateLimited)
        ));
        first.charge_trace_admission(WINDOW + 4, limit).unwrap();
        assert!(matches!(
            first.charge_trace_admission(WINDOW + 5, limit),
            Err(LoftError::RateLimited)
        ));

        // A peer advancing the durable minute invalidates old local remainder immediately.
        second
            .charge_trace_admission(WINDOW + TRACE_ADMISSION_WINDOW_MS, limit)
            .unwrap();
        assert!(matches!(
            first.charge_trace_admission(WINDOW + 59_999, limit),
            Err(LoftError::TraceUnavailable)
        ));
    }

    #[test]
    fn trace_admission_local_watermark_rejects_rollback_and_lower_limit() {
        let store = SqliteStore::in_memory().unwrap();
        const WINDOW: u64 = 360_000;
        let limit = u32::try_from(TRACE_ADMISSION_BATCH_SIZE * 2).unwrap();
        store.charge_trace_admission(WINDOW + 1, limit).unwrap();
        assert!(matches!(
            store.charge_trace_admission(WINDOW - 1, limit),
            Err(LoftError::TraceUnavailable)
        ));
        store.charge_trace_admission(WINDOW + 2, limit).unwrap();
        assert!(matches!(
            store.charge_trace_admission(WINDOW + 3, 1),
            Err(LoftError::RateLimited)
        ));

        store
            .charge_trace_admission(WINDOW + TRACE_ADMISSION_WINDOW_MS, limit)
            .unwrap();
        assert!(matches!(
            store.charge_trace_admission(WINDOW + 59_999, limit),
            Err(LoftError::TraceUnavailable)
        ));
    }

    #[test]
    fn trace_admission_high_water_survives_exhaustion_and_limit_change() {
        const WINDOW: u64 = 420_000;
        let limit = u32::try_from(TRACE_ADMISSION_BATCH_SIZE * 2).unwrap();

        let exhausted = SqliteStore::in_memory().unwrap();
        for _ in 0..TRACE_ADMISSION_BATCH_SIZE {
            exhausted.charge_trace_admission(WINDOW + 1, limit).unwrap();
        }
        exhausted
            .lock()
            .execute(
                "UPDATE trace_admission SET admitted = admitted - 1 WHERE singleton = 1",
                [],
            )
            .unwrap();
        assert!(matches!(
            exhausted.charge_trace_admission(WINDOW + 2, limit),
            Err(LoftError::TraceUnavailable)
        ));

        let reconfigured = SqliteStore::in_memory().unwrap();
        reconfigured
            .charge_trace_admission(WINDOW + 1, limit)
            .unwrap();
        assert!(matches!(
            reconfigured.charge_trace_admission(WINDOW + 2, 1),
            Err(LoftError::RateLimited)
        ));
        reconfigured
            .lock()
            .execute(
                "UPDATE trace_admission SET admitted = 0 WHERE singleton = 1",
                [],
            )
            .unwrap();
        assert!(matches!(
            reconfigured.charge_trace_admission(WINDOW + 3, limit),
            Err(LoftError::TraceUnavailable)
        ));
    }

    #[test]
    fn schema_v5_upgrade_cools_down_only_the_migration_minute() {
        let (_directory, path) = private_database_path();
        let mut conn = Connection::open(&path).unwrap();
        create_loft_schema(&mut conn, 5);
        drop(conn);

        let store = SqliteStore::open(path.to_str().unwrap()).unwrap();
        let (migration_window, admitted) = load_trace_admission(&store.lock()).unwrap();
        assert_eq!(admitted, TRACE_ADMISSION_MIGRATION_CEILING);
        assert!(matches!(
            store.charge_trace_admission(migration_window + 1, 1),
            Err(LoftError::RateLimited)
        ));
        store
            .charge_trace_admission(migration_window + TRACE_ADMISSION_WINDOW_MS, 1)
            .unwrap();

        let fresh = SqliteStore::in_memory().unwrap();
        assert_eq!(load_trace_admission(&fresh.lock()).unwrap(), (0, 0));
    }

    #[cfg(unix)]
    #[test]
    fn ordinary_umask_keeps_database_and_sqlite_sidecars_owner_only() {
        let current = std::env::current_exe().unwrap();
        let status = std::process::Command::new("/bin/sh")
            .arg("-c")
            .arg(
                "umask 022; exec \"$0\" --ignored --exact \
                 store::tests::ordinary_umask_storage_child --test-threads=1",
            )
            .arg(current)
            .status()
            .unwrap();
        assert!(status.success());
    }

    #[cfg(unix)]
    #[test]
    #[ignore = "invoked in a child process with an explicit ordinary umask"]
    fn ordinary_umask_storage_child() {
        use std::os::unix::fs::MetadataExt;

        let temp = tempfile::tempdir().unwrap();
        let directory = temp.path().join("storage");
        let path = directory.join("loft.db");
        let store = SqliteStore::open(path.to_str().unwrap()).unwrap();
        store.health_check().unwrap();

        assert_eq!(std::fs::metadata(&directory).unwrap().mode() & 0o777, 0o700);
        for protected in [
            path.clone(),
            sidecar_path(&path, "-wal"),
            sidecar_path(&path, "-shm"),
        ] {
            let metadata = std::fs::metadata(&protected).unwrap_or_else(|error| {
                panic!(
                    "missing protected SQLite file {}: {error}",
                    protected.display()
                )
            });
            assert_eq!(metadata.mode() & 0o777, 0o600, "{}", protected.display());
            assert_eq!(metadata.nlink(), 1, "{}", protected.display());
        }
    }

    #[cfg(unix)]
    #[test]
    fn unsafe_preexisting_database_directory_and_file_are_refused() {
        use std::os::unix::fs::PermissionsExt;

        let temp = tempfile::tempdir().unwrap();
        let directory = temp.path().join("storage");
        std::fs::create_dir(&directory).unwrap();
        std::fs::set_permissions(&directory, std::fs::Permissions::from_mode(0o755)).unwrap();
        let path = directory.join("loft.db");
        assert!(matches!(
            SqliteStore::open(path.to_str().unwrap()),
            Err(LoftError::Io(error)) if error.kind() == std::io::ErrorKind::PermissionDenied
        ));
        assert!(!path.exists());

        std::fs::set_permissions(&directory, std::fs::Permissions::from_mode(0o700)).unwrap();
        std::fs::write(&path, []).unwrap();
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o644)).unwrap();
        assert!(matches!(
            SqliteStore::open(path.to_str().unwrap()),
            Err(LoftError::Io(error)) if error.kind() == std::io::ErrorKind::PermissionDenied
        ));
    }

    #[cfg(unix)]
    #[test]
    fn intermediate_symlink_and_mutable_ancestor_are_refused_without_side_effects() {
        use std::os::unix::fs::{symlink, PermissionsExt};

        let temp = tempfile::tempdir().unwrap();
        let outside = temp.path().join("outside");
        std::fs::create_dir(&outside).unwrap();
        std::fs::set_permissions(&outside, std::fs::Permissions::from_mode(0o700)).unwrap();
        let linked = temp.path().join("linked");
        symlink(&outside, &linked).unwrap();
        let through_link = linked.join("new-private/loft.db");
        assert!(SqliteStore::open(through_link.to_str().unwrap()).is_err());
        assert!(!outside.join("new-private").exists());

        let mutable = temp.path().join("mutable");
        std::fs::create_dir(&mutable).unwrap();
        std::fs::set_permissions(&mutable, std::fs::Permissions::from_mode(0o770)).unwrap();
        let through_mutable = mutable.join("new-private/loft.db");
        assert!(SqliteStore::open(through_mutable.to_str().unwrap()).is_err());
        assert!(!mutable.join("new-private").exists());
    }

    #[cfg(unix)]
    #[test]
    fn unsafe_preexisting_sqlite_sidecars_are_refused_before_main_file_creation() {
        use std::os::unix::fs::{symlink, PermissionsExt};

        let temp = tempfile::tempdir().unwrap();
        let directory = temp.path().join("storage");
        std::fs::create_dir(&directory).unwrap();
        std::fs::set_permissions(&directory, std::fs::Permissions::from_mode(0o700)).unwrap();
        let path = directory.join("loft.db");
        let outside = temp.path().join("outside");
        std::fs::write(&outside, []).unwrap();
        let wal = sidecar_path(&path, "-wal");
        symlink(&outside, &wal).unwrap();
        assert!(SqliteStore::open(path.to_str().unwrap()).is_err());
        assert!(!path.exists());

        std::fs::remove_file(&wal).unwrap();
        let shm = sidecar_path(&path, "-shm");
        std::fs::write(&shm, []).unwrap();
        std::fs::set_permissions(&shm, std::fs::Permissions::from_mode(0o644)).unwrap();
        assert!(matches!(
            SqliteStore::open(path.to_str().unwrap()),
            Err(LoftError::Io(error)) if error.kind() == std::io::ErrorKind::PermissionDenied
        ));
        assert!(!path.exists());
    }

    #[cfg(unix)]
    #[test]
    fn hardlinked_database_and_sidecar_files_are_refused() {
        use std::os::unix::fs::PermissionsExt;

        let temp = tempfile::tempdir().unwrap();
        let directory = temp.path().join("storage");
        std::fs::create_dir(&directory).unwrap();
        std::fs::set_permissions(&directory, std::fs::Permissions::from_mode(0o700)).unwrap();
        let path = directory.join("loft.db");

        std::fs::write(&path, []).unwrap();
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o600)).unwrap();
        std::fs::hard_link(&path, temp.path().join("database-copy")).unwrap();
        assert!(matches!(
            SqliteStore::open(path.to_str().unwrap()),
            Err(LoftError::Io(error)) if error.kind() == std::io::ErrorKind::PermissionDenied
        ));

        std::fs::remove_file(temp.path().join("database-copy")).unwrap();
        std::fs::remove_file(&path).unwrap();
        let wal = sidecar_path(&path, "-wal");
        std::fs::write(&wal, []).unwrap();
        std::fs::set_permissions(&wal, std::fs::Permissions::from_mode(0o600)).unwrap();
        std::fs::hard_link(&wal, temp.path().join("wal-copy")).unwrap();
        assert!(matches!(
            SqliteStore::open(path.to_str().unwrap()),
            Err(LoftError::Io(error)) if error.kind() == std::io::ErrorKind::PermissionDenied
        ));
        assert!(!path.exists());
    }

    #[cfg(unix)]
    #[test]
    fn health_check_rejects_post_open_database_or_parent_replacement() {
        use std::os::unix::fs::PermissionsExt;

        let temp = tempfile::tempdir().unwrap();
        let directory = temp.path().join("storage");
        let path = directory.join("loft.db");
        let store = SqliteStore::open(path.to_str().unwrap()).unwrap();
        store.health_check().unwrap();

        let moved_database = directory.join("loft.db.original");
        std::fs::rename(&path, &moved_database).unwrap();
        std::fs::write(&path, []).unwrap();
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o600)).unwrap();
        assert!(matches!(
            store.health_check(),
            Err(LoftError::Io(error)) if error.kind() == std::io::ErrorKind::PermissionDenied
        ));
        drop(store);

        std::fs::remove_file(&path).unwrap();
        std::fs::rename(&moved_database, &path).unwrap();
        let store = SqliteStore::open(path.to_str().unwrap()).unwrap();
        let moved_directory = temp.path().join("storage.original");
        std::fs::rename(&directory, &moved_directory).unwrap();
        std::fs::create_dir(&directory).unwrap();
        std::fs::set_permissions(&directory, std::fs::Permissions::from_mode(0o700)).unwrap();
        std::fs::write(&path, []).unwrap();
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o600)).unwrap();
        assert!(matches!(
            store.health_check(),
            Err(LoftError::Io(error)) if error.kind() == std::io::ErrorKind::PermissionDenied
        ));
    }

    #[cfg(unix)]
    #[test]
    fn health_check_rejects_replaced_or_missing_wal_and_shm() {
        use std::os::unix::fs::PermissionsExt;

        for suffix in ["-wal", "-shm"] {
            let temp = tempfile::tempdir().unwrap();
            let path = temp.path().join("storage/loft.db");
            let store = SqliteStore::open(path.to_str().unwrap()).unwrap();
            store.health_check().unwrap();

            let sidecar = sidecar_path(&path, suffix);
            let original = sidecar_path(&path, &format!("{suffix}.original"));
            std::fs::rename(&sidecar, &original).unwrap();
            assert!(
                store.health_check().is_err(),
                "missing {suffix} was accepted"
            );
            std::fs::write(&sidecar, []).unwrap();
            std::fs::set_permissions(&sidecar, std::fs::Permissions::from_mode(0o600)).unwrap();
            assert!(matches!(
                store.health_check(),
                Err(LoftError::Io(error))
                    if error.kind() == std::io::ErrorKind::PermissionDenied
            ));
        }
    }

    #[cfg(windows)]
    fn windows_sidecar_path(path: &std::path::Path, suffix: &str) -> std::path::PathBuf {
        let mut value = path.as_os_str().to_os_string();
        value.push(suffix);
        value.into()
    }

    #[cfg(windows)]
    #[test]
    fn windows_unsafe_main_and_sidecars_are_rejected_before_sqlite_mutation() {
        for suffix in ["", "-wal", "-shm", "-journal"] {
            let (_directory, path) = private_database_path();
            let candidate = windows_sidecar_path(&path, suffix);
            if suffix.is_empty() {
                std::fs::remove_file(&path).unwrap();
            }
            std::fs::write(&candidate, b"unprotected-custody-sentinel").unwrap();

            let mut sqlite_open_reached = false;
            assert!(
                SqliteStore::open_windows_after_custody_check(path.to_str().unwrap(), || {
                    sqlite_open_reached = true;
                })
                .is_err(),
                "accepted unsafe pre-existing {suffix:?}"
            );
            assert!(
                !sqlite_open_reached,
                "SQLite saw unsafe {suffix:?} before custody rejected it"
            );
            assert_eq!(
                std::fs::read(&candidate).unwrap(),
                b"unprotected-custody-sentinel",
                "SQLite mutated unsafe {suffix:?} before custody rejected it"
            );
        }
    }

    #[cfg(windows)]
    #[test]
    fn windows_database_rejects_a_different_sqlite_connection_path() {
        let (_directory, path) = private_database_path();
        let preparation = WindowsDatabasePreparation::open_or_create(&path).unwrap();
        let unrelated = Connection::open_in_memory().unwrap();

        assert!(matches!(
            preparation.verify_sqlite_connection(&unrelated),
            Err(LoftError::Io(error))
                if error.kind() == std::io::ErrorKind::PermissionDenied
                    && error.to_string().contains("SQLite reports a different main database path")
        ));
    }

    #[cfg(windows)]
    #[test]
    fn windows_hardlinked_and_reparse_sidecars_are_rejected() {
        let (_directory, path) = private_database_path();
        let wal = windows_sidecar_path(&path, "-wal");
        std::fs::hard_link(&path, &wal).unwrap();
        let mut sqlite_open_reached = false;
        assert!(
            SqliteStore::open_windows_after_custody_check(path.to_str().unwrap(), || {
                sqlite_open_reached = true;
            })
            .is_err()
        );
        assert!(!sqlite_open_reached);
        assert!(wal.exists());

        let (_directory, path) = private_database_path();
        let target = path.parent().unwrap().join("target.bin");
        std::fs::write(&target, b"target").unwrap();
        let shm = windows_sidecar_path(&path, "-shm");
        if std::os::windows::fs::symlink_file(&target, &shm).is_ok() {
            let mut sqlite_open_reached = false;
            assert!(
                SqliteStore::open_windows_after_custody_check(path.to_str().unwrap(), || {
                    sqlite_open_reached = true;
                })
                .is_err()
            );
            assert!(!sqlite_open_reached);
            assert_eq!(std::fs::read(&target).unwrap(), b"target");
        }
    }

    #[cfg(windows)]
    #[test]
    fn windows_ambiguous_and_reparse_ancestors_are_rejected_without_effects() {
        let root = tempfile::tempdir().unwrap();
        let attempted = root
            .path()
            .join("would-be-created")
            .join("..")
            .join("escaped")
            .join("loft.db");
        assert!(SqliteStore::open(attempted.to_str().unwrap()).is_err());
        assert!(!root.path().join("would-be-created").exists());
        assert!(!root.path().join("escaped").exists());

        let oversized_sidecar_name = format!("{}.db", "a".repeat(247));
        let oversized_parent = root.path().join("oversized-derived-name");
        let oversized = oversized_parent.join(oversized_sidecar_name);
        assert!(SqliteStore::open(oversized.to_str().unwrap()).is_err());
        assert!(
            !oversized_parent.exists(),
            "a rejected derived SQLite name created its parent"
        );

        let outside = root.path().join("outside");
        std::fs::create_dir(&outside).unwrap();
        let linked = root.path().join("linked");
        let junction = std::process::Command::new("cmd.exe")
            .args(["/C", "mklink", "/J"])
            .arg(&linked)
            .arg(&outside)
            .output()
            .unwrap();
        assert!(
            junction.status.success(),
            "failed to create test junction: {}",
            String::from_utf8_lossy(&junction.stderr)
        );
        let through_link = linked.join("new-private/loft.db");
        assert!(SqliteStore::open(through_link.to_str().unwrap()).is_err());
        assert!(!outside.join("new-private").exists());
    }

    #[cfg(windows)]
    #[test]
    fn windows_database_and_ancestor_names_remain_locked_until_connection_shutdown() {
        let (_directory, path) = private_database_path();
        let store = SqliteStore::open(path.to_str().unwrap()).unwrap();
        store.health_check().unwrap();

        for suffix in ["", "-wal", "-shm"] {
            let source = windows_sidecar_path(&path, suffix);
            assert!(source.exists(), "SQLite did not create required {suffix:?}");
            let destination = windows_sidecar_path(&source, ".moved");
            assert!(
                std::fs::rename(&source, &destination).is_err(),
                "{suffix:?} remained replaceable while SqliteStore was alive"
            );
        }
        let parent = path.parent().unwrap();
        let moved_parent = parent.with_extension("moved");
        assert!(
            std::fs::rename(parent, &moved_parent).is_err(),
            "database parent remained replaceable while SqliteStore was alive"
        );

        drop(store);
        let moved_main = windows_sidecar_path(&path, ".after-close");
        std::fs::rename(&path, moved_main).unwrap();
    }

    #[test]
    fn capacity_and_insert_are_atomic_under_concurrency() {
        let store = Arc::new(SqliteStore::in_memory().unwrap());
        let bob = Identity::from_seed([2; 32]);
        let first = wrap_for(&bob, "first");
        let second = wrap_for(&bob, "second");
        let one_event_capacity =
            serde_json::to_vec(&first).unwrap().len() as u64 + EVENT_STORAGE_OVERHEAD;
        let barrier = Arc::new(Barrier::new(3));
        let mut joins = Vec::new();
        for wrap in [first, second] {
            let store = Arc::clone(&store);
            let barrier = Arc::clone(&barrier);
            joins.push(std::thread::spawn(move || {
                barrier.wait();
                store.admit(&wrap, &wrap.id(), 100, 200, one_event_capacity, None)
            }));
        }
        barrier.wait();
        let results: Vec<_> = joins.into_iter().map(|join| join.join().unwrap()).collect();
        assert_eq!(results.iter().filter(|result| result.is_ok()).count(), 1);
        assert_eq!(store.event_count().unwrap(), 1);
        assert!(store.bytes_used().unwrap() <= one_event_capacity);
    }

    #[test]
    fn duplicate_is_idempotent_even_after_capacity_is_full() {
        let store = SqliteStore::in_memory().unwrap();
        let bob = Identity::from_seed([2; 32]);
        let wrap = wrap_for(&bob, "once");
        let capacity = serde_json::to_vec(&wrap).unwrap().len() as u64 + EVENT_STORAGE_OVERHEAD;
        assert!(store
            .admit(&wrap, &wrap.id(), 100, 200, capacity, None)
            .unwrap());
        assert!(!store.admit(&wrap, &wrap.id(), 100, 200, 0, None).unwrap());
    }

    #[test]
    fn legacy_v2_rows_remain_fetchable_but_runtime_admission_is_separate() {
        let store = SqliteStore::in_memory().unwrap();
        let bob = Identity::from_seed([2; 32]);
        let mut legacy = wrap_for(&bob, "stored before rollout");
        legacy.version = pigeonpost_core::envelope::LEGACY_ENVELOPE_VERSION;
        store
            .insert(
                &legacy,
                &legacy.id(),
                100,
                200,
                u64::MAX,
                PolicyExpectation::Ignore,
            )
            .unwrap();
        let fetched = store.fetch(bob.verifying_key().as_bytes(), 0, 1).unwrap();
        assert_eq!(
            fetched[0].wrap.version,
            pigeonpost_core::envelope::LEGACY_ENVELOPE_VERSION
        );
    }

    #[test]
    fn policy_signature_sequence_and_write_are_one_cas() {
        let store = Arc::new(SqliteStore::in_memory().unwrap());
        let owner = Identity::from_seed([3; 32]);
        store
            .put_policy(&RecipientPolicy::new(&owner, 0, false, vec![], 1), u64::MAX)
            .unwrap();
        let two = RecipientPolicy::new(&owner, 2, false, vec![], 2);
        let three = RecipientPolicy::new(&owner, 3, false, vec![], 3);
        let barrier = Arc::new(Barrier::new(3));
        let mut joins = Vec::new();
        for policy in [two, three] {
            let store = Arc::clone(&store);
            let barrier = Arc::clone(&barrier);
            joins.push(std::thread::spawn(move || {
                barrier.wait();
                store.put_policy(&policy, u64::MAX)
            }));
        }
        barrier.wait();
        for join in joins {
            let _ = join.join().unwrap();
        }
        assert_eq!(
            store
                .policy(owner.verifying_key().as_bytes())
                .unwrap()
                .unwrap()
                .seq,
            3
        );
    }

    #[test]
    fn control_rows_are_bounded_and_exact_policy_retries_cost_zero() {
        let store = SqliteStore::in_memory().unwrap();
        let capacity = 100_000;
        let first_owner = Identity::from_seed([10; 32]);
        let first = RecipientPolicy::new(&first_owner, 0, false, vec![], 1);
        store.put_policy(&first, capacity).unwrap();
        let after_first = store.stats().unwrap();
        store.put_policy(&first, capacity).unwrap();
        assert_eq!(store.stats().unwrap(), after_first);

        let mut rejected = false;
        for seed in 11u8..=100 {
            let owner = Identity::from_seed([seed; 32]);
            let policy = RecipientPolicy::new(&owner, 0, false, vec![], 1);
            match store.put_policy(&policy, capacity) {
                Ok(()) => {}
                Err(LoftError::AtCapacity) => {
                    rejected = true;
                    break;
                }
                Err(error) => panic!("unexpected control admission error: {error}"),
            }
        }
        let stats = store.stats().unwrap();
        assert!(rejected);
        assert_eq!(stats.event_bytes_used, 0);
        assert_eq!(stats.control_bytes_reserved, 0);
        assert!(stats.control_bytes_used <= capacity / CONTROL_STORAGE_DIVISOR);
        assert_eq!(stats.bytes_used, stats.control_bytes_used);
    }

    #[test]
    fn policy_updates_charge_only_the_transactional_size_delta() {
        let store = SqliteStore::in_memory().unwrap();
        let capacity = 1_000_000;
        let owner = Identity::from_seed([101; 32]);
        let small = RecipientPolicy::new(&owner, 0, false, vec![], 1);
        store.put_policy(&small, capacity).unwrap();
        let small_stats = store.stats().unwrap();

        let large = RecipientPolicy::new(&owner, 0, false, vec![[7; 32]; 64], 2);
        store.put_policy(&large, capacity).unwrap();
        let large_stats = store.stats().unwrap();
        let expected = accounted_size(
            serde_json::to_vec(&large).unwrap().len(),
            large.pubkey.len(),
        )
        .unwrap();
        assert!(large_stats.control_bytes_used > small_stats.control_bytes_used);
        assert_eq!(large_stats.control_bytes_used, expected);
        assert_eq!(large_stats.control_bytes_reserved, 0);
    }

    #[test]
    fn admitted_agent_can_rotate_after_events_fill_the_remaining_capacity() {
        let store = SqliteStore::in_memory().unwrap();
        let capacity = 120_000;
        let operating = Identity::from_seed([102; 32]);
        let successor = Identity::from_seed([103; 32]);
        let next = Identity::from_seed([104; 32]);
        let address = operating.address().to_string();
        let pinned = SuccessorCommitment::for_key(&successor.verifying_key());
        store
            .put_agent_record(
                &address,
                &AgentRecord::new(&operating, &pinned, 1, vec![]),
                capacity,
            )
            .unwrap();

        let recipient = Identity::from_seed([105; 32]);
        let mut filled = false;
        for index in 0..1_000 {
            let wrap = wrap_for(&recipient, &format!("fill-{index}"));
            match store.admit(&wrap, &wrap.id(), 100, 200, capacity, None) {
                Ok(true) => {}
                Err(LoftError::AtCapacity) => {
                    filled = true;
                    break;
                }
                result => panic!("unexpected event admission result: {result:?}"),
            }
        }
        assert!(filled);
        let before = store.stats().unwrap();
        assert_eq!(before.control_bytes_reserved, ROTATION_STORAGE_RESERVATION);

        let rotation = RotationRecord::new(
            &operating,
            &successor,
            &SuccessorCommitment::for_key(&next.verifying_key()),
            2,
            1_000,
        )
        .unwrap();
        assert!(store
            .put_rotation_record(&address, &rotation, 1_000, capacity)
            .unwrap());
        let after = store.stats().unwrap();
        assert_eq!(after.control_bytes_reserved, 0);
        assert!(after.bytes_used <= capacity);
        assert!(after.bytes_used <= before.bytes_used);
    }

    #[test]
    fn control_accounting_reconciles_across_restart() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("private-storage/loft.db");
        let capacity = 1_000_000;
        let owner = Identity::from_seed([106; 32]);
        let successor = Identity::from_seed([107; 32]);
        let address = owner.address().to_string();
        let expected = {
            let store = SqliteStore::open(path.to_str().unwrap()).unwrap();
            store
                .put_policy(&RecipientPolicy::new(&owner, 0, false, vec![], 1), capacity)
                .unwrap();
            store
                .put_agent_record(
                    &address,
                    &AgentRecord::new(
                        &owner,
                        &SuccessorCommitment::for_key(&successor.verifying_key()),
                        1,
                        vec![],
                    ),
                    capacity,
                )
                .unwrap();
            store.stats().unwrap()
        };
        let reopened = SqliteStore::open(path.to_str().unwrap()).unwrap();
        assert_eq!(reopened.stats().unwrap(), expected);
    }

    #[test]
    fn legacy_policy_is_read_only() {
        let store = SqliteStore::in_memory().unwrap();
        let owner = Identity::from_seed([3; 32]);
        assert!(store
            .put_policy(&deployed_v1_policy(&owner, 1), u64::MAX)
            .is_err());
        assert!(store
            .put_policy(&RecipientPolicy::new(&owner, 0, false, vec![], 1), u64::MAX,)
            .is_ok());
    }

    #[test]
    fn admission_detects_a_concurrent_policy_change() {
        let store = SqliteStore::in_memory().unwrap();
        let bob = Identity::from_seed([2; 32]);
        let wrap = wrap_for(&bob, "mail");
        store
            .put_policy(&RecipientPolicy::new(&bob, 0, false, vec![], 1), u64::MAX)
            .unwrap();
        assert!(matches!(
            store.admit(&wrap, &wrap.id(), 1, 2, u64::MAX, None),
            Err(LoftError::PolicyChanged)
        ));
    }

    #[test]
    fn sweep_is_bounded_and_updates_counters_in_the_same_transaction() {
        let store = SqliteStore::in_memory().unwrap();
        let bob = Identity::from_seed([2; 32]);
        for index in 0..4 {
            let wrap = wrap_for(&bob, &format!("old{index}"));
            store
                .admit(&wrap, &wrap.id(), 100, 200, u64::MAX, None)
                .unwrap();
        }
        let fresh = wrap_for(&bob, "fresh");
        store
            .admit(&fresh, &fresh.id(), 100, 10_000, u64::MAX, None)
            .unwrap();
        assert_eq!(store.sweep_expired(500, 2).unwrap(), 2);
        assert_eq!(store.event_count().unwrap(), 3);
        assert_eq!(store.sweep_expired(500, 100).unwrap(), 2);
        assert_eq!(store.event_count().unwrap(), 1);
    }

    #[test]
    fn agent_records_never_go_backwards() {
        let store = SqliteStore::in_memory().unwrap();
        let agent = Identity::from_seed([4; 32]);
        let commitment =
            SuccessorCommitment::for_key(&Identity::from_seed([5; 32]).verifying_key());
        let address = agent.address().to_string();
        let old = AgentRecord::new(&agent, &commitment, 1, vec!["https://a.example".into()]);
        let new = AgentRecord::new(&agent, &commitment, 2, vec!["https://b.example".into()]);
        store.put_agent_record(&address, &old, u64::MAX).unwrap();
        store.put_agent_record(&address, &new, u64::MAX).unwrap();
        assert!(store.put_agent_record(&address, &old, u64::MAX).is_err());
        assert_eq!(store.agent_record(&address).unwrap().unwrap().seq, 2);
    }

    #[test]
    fn agent_record_successor_pin_cannot_change_at_the_same_address() {
        let store = SqliteStore::in_memory().unwrap();
        let agent = Identity::from_seed([4; 32]);
        let original = SuccessorCommitment::for_key(&Identity::from_seed([5; 32]).verifying_key());
        let hostile = SuccessorCommitment::for_key(&Identity::from_seed([6; 32]).verifying_key());
        let address = agent.address().to_string();
        store
            .put_agent_record(
                &address,
                &AgentRecord::new(&agent, &original, 1, vec!["https://a.example".into()]),
                u64::MAX,
            )
            .unwrap();

        assert!(matches!(
            store.put_agent_record(
                &address,
                &AgentRecord::new(&agent, &hostile, 2, vec!["https://a.example".into()]),
                u64::MAX,
            ),
            Err(LoftError::Core(pigeonpost_core::Error::SuccessorMismatch))
        ));
    }

    #[test]
    fn rotation_storage_is_verified_immutable_and_idempotent() {
        let store = SqliteStore::in_memory().unwrap();
        let operating = Identity::from_seed([4; 32]);
        let successor = Identity::from_seed([5; 32]);
        let next = Identity::from_seed([6; 32]);
        let pinned = SuccessorCommitment::for_key(&successor.verifying_key());
        let address = operating.address().to_string();
        store
            .put_agent_record(
                &address,
                &AgentRecord::new(&operating, &pinned, 3, vec!["https://a.example".into()]),
                u64::MAX,
            )
            .unwrap();
        let rotation = RotationRecord::new(
            &operating,
            &successor,
            &SuccessorCommitment::for_key(&next.verifying_key()),
            4,
            1_000,
        )
        .unwrap();

        assert!(store
            .put_rotation_record(&address, &rotation, 1_000, u64::MAX)
            .unwrap());
        assert!(!store
            .put_rotation_record(&address, &rotation, 1_001, u64::MAX)
            .unwrap());
        assert_eq!(
            store.rotation_record(&address).unwrap(),
            Some(rotation.clone())
        );
        assert!(matches!(
            store.put_agent_record(
                &address,
                &AgentRecord::new(&operating, &pinned, 5, vec!["https://a.example".into()]),
                u64::MAX,
            ),
            Err(LoftError::Core(pigeonpost_core::Error::StaleSequence))
        ));

        let different_next = Identity::from_seed([7; 32]);
        let equivocation = RotationRecord::new(
            &operating,
            &successor,
            &SuccessorCommitment::for_key(&different_next.verifying_key()),
            4,
            1_000,
        )
        .unwrap();
        assert!(matches!(
            store.put_rotation_record(&address, &equivocation, 1_001, u64::MAX),
            Err(LoftError::Core(pigeonpost_core::Error::StaleSequence))
        ));
        assert_eq!(store.rotation_record(&address).unwrap(), Some(rotation));
    }

    #[test]
    fn rotation_storage_rejects_missing_source_replay_future_and_wrong_commitment() {
        let store = SqliteStore::in_memory().unwrap();
        let operating = Identity::from_seed([4; 32]);
        let successor = Identity::from_seed([5; 32]);
        let next = Identity::from_seed([6; 32]);
        let rotation = RotationRecord::new(
            &operating,
            &successor,
            &SuccessorCommitment::for_key(&next.verifying_key()),
            1,
            10_000,
        )
        .unwrap();
        let address = operating.address().to_string();
        assert!(store
            .put_rotation_record(&address, &rotation, 10_000, u64::MAX)
            .is_err());

        let wrong = Identity::from_seed([8; 32]);
        let wrong_pin = SuccessorCommitment::for_key(&wrong.verifying_key());
        store
            .put_agent_record(
                &address,
                &AgentRecord::new(&operating, &wrong_pin, 0, vec![]),
                u64::MAX,
            )
            .unwrap();
        assert!(matches!(
            store.put_rotation_record(&address, &rotation, 10_000, u64::MAX),
            Err(LoftError::Core(pigeonpost_core::Error::SuccessorMismatch))
        ));

        let other_store = SqliteStore::in_memory().unwrap();
        let pinned = SuccessorCommitment::for_key(&successor.verifying_key());
        other_store
            .put_agent_record(
                &address,
                &AgentRecord::new(&operating, &pinned, 0, vec![]),
                u64::MAX,
            )
            .unwrap();
        assert!(matches!(
            other_store.put_rotation_record(&address, &rotation, 9_000, u64::MAX),
            Err(LoftError::Core(pigeonpost_core::Error::StaleTimestamp))
        ));
        let skipped = RotationRecord::new(
            &operating,
            &successor,
            &SuccessorCommitment::for_key(&next.verifying_key()),
            2,
            10_000,
        )
        .unwrap();
        assert!(matches!(
            other_store.put_rotation_record(&address, &skipped, 10_000, u64::MAX),
            Err(LoftError::Core(pigeonpost_core::Error::StaleSequence))
        ));
    }

    #[test]
    fn trace_segment_catalog_is_idempotent_and_never_reopens_closed_segments() {
        let store = SqliteStore::in_memory().unwrap();
        let open = open_trace_metadata();
        store.record_trace_segment(&open).unwrap();
        store.record_trace_segment(&open).unwrap();

        let mut closed = open.clone();
        closed.closed_at_ms = Some(86_400_002);
        closed.record_count = Some(1);
        closed.first_hash = Some([6; 32]);
        closed.final_hash = Some([7; 32]);
        closed.state = TraceSegmentState::Closed;
        store.record_trace_segment(&closed).unwrap();
        store.record_trace_segment(&closed).unwrap();
        assert_eq!(
            store.trace_segment_state(&closed.segment_id),
            Some((i64::from(TraceSegmentState::Closed as u8), Some(1)))
        );

        assert!(matches!(
            store.record_trace_segment(&open),
            Err(LoftError::TraceMetadata)
        ));
        let mut conflicting = closed;
        conflicting.relative_path = "different.pptrace".to_owned();
        assert!(matches!(
            store.record_trace_segment(&conflicting),
            Err(LoftError::TraceMetadata)
        ));
    }

    #[test]
    fn deployed_unversioned_schema_migrates_transactionally() {
        let (_directory, path) = private_database_path();
        // Build the exact deployed tables without setting user_version; open must run both steps.
        let conn = Connection::open(&path).unwrap();
        conn.execute_batch(V0_1_0_SCHEMA).unwrap();

        let bob = Identity::from_seed([2; 32]);
        let wrap = wrap_for(&bob, "deployed mail");
        let wrap_blob = serde_json::to_vec(&wrap).unwrap();
        conn.execute(
            "INSERT INTO events (id, recipient, stored_at, expires_at, size, blob)
             VALUES (?1, ?2, 10, 20, ?3, ?4)",
            params![
                wrap.id().as_slice(),
                wrap.recipient.as_slice(),
                wrap_blob.len() as i64,
                wrap_blob
            ],
        )
        .unwrap();
        let policy = deployed_v1_policy(&bob, 7);
        let mut policy_json = serde_json::to_value(&policy).unwrap();
        policy_json.as_object_mut().unwrap().remove("version");
        conn.execute(
            "INSERT INTO recipient_policy (pubkey, seq, policy) VALUES (?1, ?2, ?3)",
            params![
                policy.pubkey.as_slice(),
                policy.seq as i64,
                serde_json::to_vec(&policy_json).unwrap()
            ],
        )
        .unwrap();
        drop(conn);
        let store = SqliteStore::open(path.to_str().unwrap()).unwrap();
        assert_eq!(store.schema_version(), CURRENT_SCHEMA_VERSION);
        assert_eq!(store.event_count().unwrap(), 1);
        assert_eq!(
            store
                .policy(bob.verifying_key().as_bytes())
                .unwrap()
                .unwrap()
                .version,
            LEGACY_RECIPIENT_POLICY_VERSION
        );
        assert_eq!(
            store
                .fetch(bob.verifying_key().as_bytes(), 0, 10)
                .unwrap()
                .len(),
            1
        );
        store
            .put_policy(&RecipientPolicy::new(&bob, 1, false, vec![], 8), u64::MAX)
            .unwrap();
        assert_eq!(
            store
                .policy(bob.verifying_key().as_bytes())
                .unwrap()
                .unwrap()
                .version,
            RECIPIENT_POLICY_VERSION
        );
        drop(store);
        let reopened = SqliteStore::open(path.to_str().unwrap()).unwrap();
        assert_eq!(reopened.schema_version(), CURRENT_SCHEMA_VERSION);
        assert_eq!(reopened.event_count().unwrap(), 1);
    }

    #[test]
    fn newer_schema_is_refused() {
        let (_directory, path) = private_database_path();
        let conn = Connection::open(&path).unwrap();
        conn.execute_batch(
            "CREATE TABLE sentinel (value TEXT NOT NULL);
                            INSERT INTO sentinel VALUES ('kept');",
        )
        .unwrap();
        conn.pragma_update(None, "user_version", CURRENT_SCHEMA_VERSION + 1)
            .unwrap();
        drop(conn);
        assert!(matches!(
            SqliteStore::open(path.to_str().unwrap()),
            Err(LoftError::UnsupportedSchema { .. })
        ));
        let conn = Connection::open(&path).unwrap();
        let version: u32 = conn
            .pragma_query_value(None, "user_version", |row| row.get(0))
            .unwrap();
        let value: String = conn
            .query_row("SELECT value FROM sentinel", [], |row| row.get(0))
            .unwrap();
        assert_eq!(version, CURRENT_SCHEMA_VERSION + 1);
        assert_eq!(value, "kept");
    }

    #[test]
    fn pristine_loft_database_initializes_but_unknown_v0_is_untouched() {
        let (_pristine_directory, pristine_path) = private_database_path();
        let store = SqliteStore::open(pristine_path.to_str().unwrap()).unwrap();
        assert_eq!(store.schema_version(), CURRENT_SCHEMA_VERSION);
        drop(store);

        for ddl in [
            "CREATE TABLE events (cursor INTEGER PRIMARY KEY); INSERT INTO events VALUES (7);",
            "CREATE TABLE operator_data (value TEXT NOT NULL);
             INSERT INTO operator_data VALUES ('kept');",
        ] {
            let (_directory, path) = private_database_path();
            let conn = Connection::open(&path).unwrap();
            conn.execute_batch(ddl).unwrap();
            drop(conn);
            assert!(matches!(
                SqliteStore::open(path.to_str().unwrap()),
                Err(LoftError::Configuration(_))
            ));
            let conn = Connection::open(&path).unwrap();
            let version: u32 = conn
                .pragma_query_value(None, "user_version", |row| row.get(0))
                .unwrap();
            let tables: i64 = conn
                .query_row(
                    "SELECT COUNT(*) FROM sqlite_schema
                     WHERE type = 'table' AND name NOT LIKE 'sqlite_%'",
                    [],
                    |row| row.get(0),
                )
                .unwrap();
            assert_eq!(version, 0);
            assert_eq!(tables, 1);
        }
    }

    #[test]
    fn every_known_loft_schema_migrates_step_by_step() {
        for version in 1..=CURRENT_SCHEMA_VERSION {
            let (_directory, path) = private_database_path();
            let mut conn = Connection::open(&path).unwrap();
            create_loft_schema(&mut conn, version);
            drop(conn);

            let store = SqliteStore::open(path.to_str().unwrap()).unwrap();
            assert_eq!(
                store.schema_version(),
                CURRENT_SCHEMA_VERSION,
                "failed from schema {version}"
            );
        }
    }

    #[test]
    fn malformed_declared_loft_schema_is_refused_untouched() {
        let (_directory, path) = private_database_path();
        let mut conn = Connection::open(&path).unwrap();
        create_loft_schema(&mut conn, CURRENT_SCHEMA_VERSION);
        conn.execute_batch("ALTER TABLE storage_stats ADD COLUMN rogue TEXT;")
            .unwrap();
        conn.execute(
            "INSERT INTO agent_records (address, seq, record)
             VALUES ('sentinel', 1, X'01')",
            [],
        )
        .unwrap();
        drop(conn);

        assert!(matches!(
            SqliteStore::open(path.to_str().unwrap()),
            Err(LoftError::Configuration(_))
        ));
        let conn = Connection::open(&path).unwrap();
        let version: u32 = conn
            .pragma_query_value(None, "user_version", |row| row.get(0))
            .unwrap();
        let rows: i64 = conn
            .query_row("SELECT COUNT(*) FROM agent_records", [], |row| row.get(0))
            .unwrap();
        assert_eq!(version, CURRENT_SCHEMA_VERSION);
        assert_eq!(rows, 1);
    }

    #[test]
    fn corrupt_deployed_policy_rolls_back_the_v2_migration() {
        let (_directory, path) = private_database_path();
        let conn = Connection::open(&path).unwrap();
        conn.execute_batch(V0_1_0_SCHEMA).unwrap();
        conn.execute(
            "INSERT INTO recipient_policy (pubkey, seq, policy) VALUES (?1, 1, ?2)",
            params![[1u8; 32].as_slice(), b"not-json".as_slice()],
        )
        .unwrap();
        drop(conn);

        assert!(SqliteStore::open(path.to_str().unwrap()).is_err());
        let conn = Connection::open(&path).unwrap();
        let version: u32 = conn
            .pragma_query_value(None, "user_version", |row| row.get(0))
            .unwrap();
        assert_eq!(
            version, 1,
            "failed v2 migration must not mark itself complete"
        );
        let has_v2_column: bool = conn
            .prepare("PRAGMA table_info(recipient_policy)")
            .unwrap()
            .query_map([], |row| row.get::<_, String>(1))
            .unwrap()
            .any(|name| name.unwrap() == "policy_version");
        assert!(
            !has_v2_column,
            "failed migration must roll back schema changes"
        );
    }
}
