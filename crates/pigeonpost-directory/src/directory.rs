//! The directory service.
//!
//! Admission is **open** — any loft may submit, and there is no approval step. Gating admission
//! would recreate the gatekeeper problem, and encryption already bounds a hostile loft to dropping
//! mail (survived by redundancy, detected by self-probing) and observing metadata (which admission
//! control would not fix either, since a patient attacker passes review). Quality is handled after
//! the fact, by probing and client-side de-weighting (`docs/network.md`).

use std::net::IpAddr;
use std::sync::Mutex;

use ed25519_dalek::{Signer, SigningKey};
use pigeonpost_core::network::{is_localhost_name, is_public_network_address};
use pigeonpost_registry::entry::{
    DirectoryAdd as RegistryDirectoryAdd, DirectoryRemove as RegistryDirectoryRemove,
};
use rand_core::OsRng;
use rusqlite::{params, Connection, OpenFlags, OptionalExtension, TransactionBehavior};
use sha2::{Digest, Sha256};
use zeroize::Zeroize;

use crate::document::MAX_DIRECTORY_ENTRIES;
use crate::entry::{DirectoryEntry, DrainAuthorization, Health, LoftState};
use crate::error::{DirectoryError, Result};
use crate::private_store::PrivateDatabase;

/// Consecutive probe failures before a loft stops attracting new agents.
pub const DEGRADE_AFTER_FAILURES: u32 = 3;

/// How long a newly submitted loft must probe clean before it becomes selectable.
pub const PROMOTE_AFTER_SECS: u64 = 24 * 60 * 60;

/// Pending registrations that never establish a clean probation interval stop consuming probe work.
pub const PENDING_EXPIRE_SECS: u64 = 7 * 24 * 60 * 60;

/// How long a degraded loft is kept before it is dropped entirely.
pub const REMOVE_AFTER_SECS: u64 = 72 * 60 * 60;

/// Exact rolling window used by both published uptime and raw measurement evidence.
pub const PROBE_RETENTION_SECS: u64 = 30 * 24 * 60 * 60;

/// Normal liveness-probe cadence.
pub const PROBE_INTERVAL_SECS: u64 = 5 * 60;

/// A cheap unauthenticated submission cannot create an unbounded probation queue.
pub const MAX_PENDING_ENTRIES: usize = 4_096;

/// Public snapshots are intentionally smaller than the client's fixed two-MiB body ceiling.
pub const MAX_PUBLIC_ENTRIES: usize = MAX_DIRECTORY_ENTRIES;

/// One sweep leases a bounded number of due entries so later entries cannot starve.
pub const MAX_PROBE_CANDIDATES: usize = 512;

/// Retention evidence is checked at most once per day for each loft.
pub const RETENTION_CHECK_INTERVAL_SECS: u64 = 24 * 60 * 60;

/// Check just before the exact advertised expiry boundary, not after an honest loft may delete.
pub const RETENTION_CHECK_MARGIN_SECS: u64 = 60 * 60;

const DIRECTORY_SIGNING_SEED: &str = "directory_signing_seed";
const DIRECTORY_SCHEMA_VERSION: u32 = 4;
const REGISTRY_CHECKPOINT: &str = "registry_checkpoint_v1";
const PROBER_HEARTBEAT: &str = "prober_heartbeat_v1";
const MAX_CHECKPOINT_NOTE_BYTES: usize = 64 * 1024;
const MAX_MUTATION_RESERVATIONS: usize = MAX_PENDING_ENTRIES;
const MAX_LOCAL_MUTATION_BYTES: usize = 64 * 1024;
const RESERVATION_DOMAIN: &[u8] = b"pigeonpost/directory-mutation-reservation/v1";
const RESERVATION_ADD: &str = "add";
const RESERVATION_DRAIN: &str = "drain";
const DIRECTORY_SCHEMA_V3_SQL: &str = r#"
    CREATE TABLE IF NOT EXISTS lofts (
        endpoint      TEXT PRIMARY KEY,
        entry         BLOB NOT NULL,
        state         TEXT NOT NULL,
        first_seen    INTEGER NOT NULL,
        last_probe    INTEGER NOT NULL DEFAULT 0,
        fail_streak   INTEGER NOT NULL DEFAULT 0,
        probes_ok     INTEGER NOT NULL DEFAULT 0,
        probes_total  INTEGER NOT NULL DEFAULT 0,
        degraded_at   INTEGER,
        drain_after   INTEGER,
        mutation_sequence INTEGER NOT NULL DEFAULT 0,
        clean_since   INTEGER,
        next_probe_at INTEGER NOT NULL DEFAULT 0,
        ownership_proven INTEGER NOT NULL DEFAULT 0 CHECK (ownership_proven IN (0, 1))
    );

    CREATE TABLE IF NOT EXISTS directory_meta (
        key    TEXT PRIMARY KEY,
        value  BLOB NOT NULL
    );

    CREATE TABLE IF NOT EXISTS probes (
        id        INTEGER PRIMARY KEY AUTOINCREMENT,
        endpoint  TEXT NOT NULL,
        at        INTEGER NOT NULL,
        result    BLOB NOT NULL,
        healthy   INTEGER NOT NULL
    );
    CREATE INDEX IF NOT EXISTS probes_by_endpoint ON probes (endpoint, at);
    CREATE INDEX IF NOT EXISTS probes_by_age ON probes (at);
    CREATE TABLE IF NOT EXISTS retention_canaries (
        endpoint          TEXT PRIMARY KEY,
        recipient_seed    BLOB NOT NULL,
        event_id          BLOB NOT NULL,
        published_at      INTEGER NOT NULL,
        last_checked_at   INTEGER NOT NULL DEFAULT 0
    );
    CREATE TABLE IF NOT EXISTS pending_claims (
        endpoint          TEXT NOT NULL,
        loft_pubkey       TEXT NOT NULL,
        entry             BLOB NOT NULL,
        first_seen        INTEGER NOT NULL,
        mutation_sequence INTEGER NOT NULL DEFAULT 0,
        next_probe_at      INTEGER NOT NULL DEFAULT 0,
        PRIMARY KEY (endpoint, loft_pubkey)
    );
    CREATE INDEX IF NOT EXISTS pending_claims_by_probe_due
        ON pending_claims (next_probe_at, first_seen, endpoint, loft_pubkey);
"#;
const DIRECTORY_SCHEMA_V4_SQL: &str = r#"
    CREATE TABLE IF NOT EXISTS directory_mutation_reservations (
        reservation_id    BLOB NOT NULL UNIQUE CHECK (length(reservation_id) = 32),
        endpoint          TEXT PRIMARY KEY,
        operation         TEXT NOT NULL CHECK (operation IN ('add', 'drain')),
        loft_pubkey       TEXT NOT NULL,
        mutation_sequence INTEGER NOT NULL CHECK (mutation_sequence >= 0),
        canonical_mutation BLOB NOT NULL,
        local_request     BLOB NOT NULL,
        reserved_at       INTEGER NOT NULL CHECK (reserved_at >= 0),
        capacity_slot     INTEGER NOT NULL CHECK (capacity_slot IN (0, 1))
    );
    CREATE INDEX IF NOT EXISTS directory_reservations_by_age
        ON directory_mutation_reservations (reserved_at, reservation_id);
"#;
pub const PROBER_FRESHNESS_SECS: u64 = 3 * PROBE_INTERVAL_SECS;

type LoftProbeStateRow = (String, i64, i64, Option<i64>, Option<i64>, Option<i64>);

pub struct Directory {
    inner: Mutex<Connection>,
    signing_key: SigningKey,
    database_custody: Option<PrivateDatabase>,
}

fn has_persisted_signing_key(conn: &Connection) -> Result<bool> {
    let has_meta: bool = conn.query_row(
        "SELECT EXISTS(SELECT 1 FROM sqlite_schema WHERE type = 'table' AND name = 'directory_meta')",
        [],
        |row| row.get(0),
    )?;
    if !has_meta {
        return Ok(false);
    }
    conn.query_row(
        "SELECT EXISTS(SELECT 1 FROM directory_meta WHERE key = ?1)",
        params![DIRECTORY_SIGNING_SEED],
        |row| row.get(0),
    )
    .map_err(Into::into)
}

/// Last witnessed registry head durably accepted before a local directory mutation committed.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct PersistedRegistryCheckpoint {
    pub version: u8,
    pub origin: String,
    pub size: u64,
    pub root: [u8; 32],
    pub note: String,
    pub witnessed_at: u64,
}

/// Exact, durable pre-admission context returned to the HTTP mutation path.
#[derive(Debug, Clone)]
pub(crate) struct ReservedMutation<T> {
    pub mutation: T,
    pub previous_checkpoint: Option<PersistedRegistryCheckpoint>,
}

/// One exact mutation left behind by cancellation, process failure, or registry ambiguity.
///
/// The recovery supervisor replays only these already-admitted values. Nothing here is projected
/// into `lofts` or `pending_claims` until a witnessed receipt is finalized transactionally.
#[derive(Debug, Clone)]
pub(crate) enum PendingMutation {
    Add {
        entry: DirectoryEntry,
        mutation: RegistryDirectoryAdd,
    },
    Drain {
        authorization: DrainAuthorization,
        mutation: RegistryDirectoryRemove,
    },
}

/// One probe result, signed and published so anyone can recompute the weights we publish.
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ProbeResult {
    pub endpoint: String,
    pub at: u64,
    pub reachable: bool,
    pub stored_and_returned: bool,
    pub utilization: f64,
    /// Age of an existing canary checked during this probe, when a retention check was due.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub retention_age_secs: Option<u64>,
    /// Whether the aged canary was still readable. `None` means this was only a liveness probe.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub retention_ok: Option<bool>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub detail: Option<String>,
}

impl ProbeResult {
    pub fn healthy(&self) -> bool {
        self.reachable && self.stored_and_returned && self.retention_ok != Some(false)
    }
}

pub(crate) struct RetentionCanary {
    pub recipient_seed: [u8; 32],
    pub event_id: [u8; 32],
    pub published_at: u64,
    pub target_age_secs: u64,
}

pub(crate) enum RetentionWork {
    Create,
    Check(RetentionCanary),
}

pub(crate) enum RetentionUpdate {
    Created {
        recipient_seed: [u8; 32],
        event_id: [u8; 32],
        published_at: u64,
    },
    Checked {
        checked_at: u64,
        rotate: bool,
    },
}

pub(crate) struct ProbePage {
    pub probes: Vec<ProbeResult>,
    pub next_cursor: Option<u64>,
    pub more: bool,
}

impl Directory {
    /// Test-only convenience that creates a signing key when the database is new.
    ///
    /// Product code must use [`Self::open_with_signing_key`] for first provisioning and
    /// [`Self::open_existing`] for later restarts. Keeping implicit generation outside a normal
    /// build makes that custody requirement a compile-time boundary rather than an operator hint.
    #[cfg(any(test, feature = "test-utilities"))]
    #[doc(hidden)]
    pub fn open(path: &str) -> Result<Self> {
        Self::open_secured(path, None, true)
    }

    /// Reopen a database that already contains its pinned directory signing key.
    ///
    /// This never creates a database and never generates a key. An empty, legacy, or corrupt
    /// database without a persisted seed is refused before schema migration changes it.
    pub fn open_existing(path: &str) -> Result<Self> {
        Self::open_secured(path, None, false)
    }

    /// Open a directory with a separately provisioned signing key.
    ///
    /// The first call pins the key in the database. Later calls reject a different configured key,
    /// preventing an operator typo from silently changing the public document trust anchor.
    pub fn open_with_signing_key(path: &str, signing_key: SigningKey) -> Result<Self> {
        Self::open_secured(path, Some(signing_key), true)
    }

    pub fn in_memory() -> Result<Self> {
        Self::init(Connection::open_in_memory()?, None, None, true)
    }

    fn open_secured(
        path: &str,
        configured_key: Option<SigningKey>,
        create_if_missing: bool,
    ) -> Result<Self> {
        let path = canonical_database_path(std::path::Path::new(path))?;
        let allow_generated_key = configured_key.is_none() && create_if_missing;
        let custody = if create_if_missing {
            PrivateDatabase::open_or_create(&path)?.0
        } else {
            match PrivateDatabase::open_existing(&path) {
                Ok(custody) => custody,
                Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                    return Err(DirectoryError::SigningKeyNotProvisioned);
                }
                Err(error) => return Err(error.into()),
            }
        };
        let flags = OpenFlags::SQLITE_OPEN_READ_WRITE
            | OpenFlags::SQLITE_OPEN_NO_MUTEX
            | OpenFlags::SQLITE_OPEN_NOFOLLOW;
        let sqlite_path = custody.sqlite_path().to_path_buf();
        #[cfg(any(unix, windows))]
        custody.verify_main_named()?;
        #[cfg(not(any(unix, windows)))]
        custody.verify_named()?;
        let conn = Connection::open_with_flags(&sqlite_path, flags)?;
        #[cfg(any(unix, windows))]
        if conn.path().map(std::path::Path::new) != Some(sqlite_path.as_path()) {
            return Err(DirectoryError::Malformed(
                "SQLite reports a different directory database path".into(),
            ));
        }
        #[cfg(any(unix, windows))]
        custody.verify_main_named()?;
        #[cfg(not(any(unix, windows)))]
        custody.verify_named()?;
        Self::init(conn, configured_key, Some(custody), allow_generated_key)
    }

    fn init(
        mut conn: Connection,
        configured_key: Option<SigningKey>,
        database_custody: Option<PrivateDatabase>,
        allow_generated_key: bool,
    ) -> Result<Self> {
        if configured_key.is_none() && !allow_generated_key && !has_persisted_signing_key(&conn)? {
            return Err(DirectoryError::SigningKeyNotProvisioned);
        }
        conn.pragma_update(None, "journal_mode", "WAL")?;
        if let Some(custody) = database_custody.as_ref() {
            // Merely selecting WAL mode does not materialize WAL/SHM for a clean database. A
            // no-op immediate transaction makes SQLite create its persistent sidecar namespace
            // without changing application data, so custody can retain both names before schema
            // initialization or migration performs a real write.
            conn.execute_batch("BEGIN IMMEDIATE; ROLLBACK;")?;
            custody.verify_named()?;
        }
        // A persistent reservation must survive power loss before its exact leaf is sent to the
        // registry. Likewise, final projection/checkpoint/consumption must be durable before an
        // HTTP success can escape. In-memory test databases have no power-loss contract.
        conn.pragma_update(
            None,
            "synchronous",
            if database_custody.is_some() {
                "FULL"
            } else {
                "NORMAL"
            },
        )?;
        conn.pragma_update(None, "busy_timeout", 5_000)?;

        let schema_version: i64 =
            conn.pragma_query_value(None, "user_version", |row| row.get(0))?;
        let schema_version = u32::try_from(schema_version).map_err(|_| {
            DirectoryError::Malformed("directory schema version is negative or too large".into())
        })?;
        if schema_version > DIRECTORY_SCHEMA_VERSION {
            return Err(DirectoryError::Malformed(format!(
                "directory schema {schema_version} is newer than supported schema {DIRECTORY_SCHEMA_VERSION}"
            )));
        }
        if schema_version == DIRECTORY_SCHEMA_VERSION {
            verify_directory_schema(&conn)?;
        } else {
            // Schema 3 is the immediate predecessor and has a fully specified shape. Verify it
            // before issuing any DDL so an operator-modified or partially applied database is not
            // silently normalized into something that merely resembles schema 4.
            if schema_version == 3 {
                verify_directory_schema_v3(&conn)?;
            }
            let transaction = conn.transaction_with_behavior(TransactionBehavior::Immediate)?;
            ensure_directory_schema_v3(&transaction)?;
            ensure_directory_schema_v4(&transaction)?;
            verify_directory_schema(&transaction)?;
            transaction.pragma_update(None, "user_version", DIRECTORY_SCHEMA_VERSION)?;
            transaction.commit()?;
        }

        let stored_seed: Option<Vec<u8>> = conn
            .query_row(
                "SELECT value FROM directory_meta WHERE key = ?1",
                params![DIRECTORY_SIGNING_SEED],
                |row| row.get(0),
            )
            .optional()?;
        let signing_key = match (stored_seed, configured_key) {
            (Some(mut stored), configured) => {
                if stored.len() != 32 {
                    stored.zeroize();
                    return Err(DirectoryError::Malformed(
                        "persisted directory signing key is invalid".into(),
                    ));
                }
                let mut seed = [0u8; 32];
                seed.copy_from_slice(&stored);
                stored.zeroize();
                let persisted = SigningKey::from_bytes(&seed);
                seed.zeroize();
                if configured
                    .as_ref()
                    .is_some_and(|key| key.verifying_key() != persisted.verifying_key())
                {
                    return Err(DirectoryError::KeyMismatch);
                }
                persisted
            }
            (None, Some(key)) => {
                let mut seed = key.to_bytes();
                let stored = conn.execute(
                    "INSERT INTO directory_meta (key, value) VALUES (?1, ?2)",
                    params![DIRECTORY_SIGNING_SEED, seed.as_slice()],
                );
                seed.zeroize();
                stored?;
                key
            }
            (None, None) if allow_generated_key => {
                let key = SigningKey::generate(&mut OsRng);
                let mut seed = key.to_bytes();
                let stored = conn.execute(
                    "INSERT INTO directory_meta (key, value) VALUES (?1, ?2)",
                    params![DIRECTORY_SIGNING_SEED, seed.as_slice()],
                );
                seed.zeroize();
                stored?;
                key
            }
            (None, None) => return Err(DirectoryError::SigningKeyNotProvisioned),
        };

        if let Some(custody) = database_custody.as_ref() {
            custody.verify_named()?;
        }
        Ok(Directory {
            inner: Mutex::new(conn),
            signing_key,
            database_custody,
        })
    }

    fn lock(&self) -> std::sync::MutexGuard<'_, Connection> {
        self.inner.lock().unwrap_or_else(|e| e.into_inner())
    }

    /// Public trust anchor for signed directory and measurement documents.
    pub fn signing_public_key(&self) -> [u8; 32] {
        self.signing_key.verifying_key().to_bytes()
    }

    pub(crate) fn sign(&self, payload: &[u8]) -> [u8; 64] {
        self.signing_key.sign(payload).to_bytes()
    }

    /// Test-only raw admission. Production submissions use the durable reserve/finalize protocol.
    #[cfg(any(test, feature = "test-utilities"))]
    #[doc(hidden)]
    pub fn submit(&self, entry: DirectoryEntry, now: u64) -> Result<()> {
        let mut conn = self.lock();
        let transaction = conn.transaction_with_behavior(TransactionBehavior::Immediate)?;
        expire_pending(&transaction, now)?;
        expire_pending_claims(&transaction, now)?;
        if reservation_for_endpoint(&transaction, &entry.endpoint)?.is_some() {
            return Err(DirectoryError::Replay);
        }
        apply_submission(&transaction, entry, now, false)?;
        transaction.commit()?;
        Ok(())
    }

    /// Validate the complete local add transition and durably reserve its exact registry leaf
    /// before any external request is attempted.
    pub(crate) fn reserve_add(
        &self,
        entry: &DirectoryEntry,
        now: u64,
    ) -> Result<ReservedMutation<RegistryDirectoryAdd>> {
        let mutation = entry.registry_addition()?;
        let canonical_mutation = bounded_mutation_bytes(&mutation)?;
        let local_request = bounded_local_request(entry)?;
        let reservation_id = reservation_id(RESERVATION_ADD, &canonical_mutation, &local_request);
        let sequence = sql_u64(entry.sequence, "entry sequence")?;
        let reserved_at = sql_u64(now, "reservation time")?;

        let mut conn = self.lock();
        let mut transaction = conn.transaction_with_behavior(TransactionBehavior::Immediate)?;
        expire_pending(&transaction, now)?;
        expire_pending_claims(&transaction, now)?;
        if let Some(row) = reservation_for_endpoint(&transaction, &entry.endpoint)? {
            ensure_exact_reservation(
                &row,
                &reservation_id,
                RESERVATION_ADD,
                &entry.pubkey,
                sequence,
                &canonical_mutation,
                &local_request,
            )?;
            let previous_checkpoint = registry_checkpoint_from(&transaction)?;
            transaction.commit()?;
            return Ok(ReservedMutation {
                mutation,
                previous_checkpoint,
            });
        }
        ensure_reservation_capacity(&transaction)?;
        let pending_before = pending_load(&transaction)?;
        let pending_after = {
            let mut savepoint = transaction.savepoint()?;
            apply_submission(&savepoint, entry.clone(), now, true)?;
            let pending_after = pending_load(&savepoint)?;
            savepoint.rollback()?;
            pending_after
        };
        let capacity_slot = i64::from(pending_after > pending_before);
        transaction.execute(
            "INSERT INTO directory_mutation_reservations
                 (reservation_id, endpoint, operation, loft_pubkey, mutation_sequence,
                  canonical_mutation, local_request, reserved_at, capacity_slot)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9)",
            params![
                reservation_id.as_slice(),
                entry.endpoint,
                RESERVATION_ADD,
                entry.pubkey,
                sequence,
                canonical_mutation,
                local_request,
                reserved_at,
                capacity_slot,
            ],
        )?;
        let previous_checkpoint = registry_checkpoint_from(&transaction)?;
        transaction.commit()?;
        Ok(ReservedMutation {
            mutation,
            previous_checkpoint,
        })
    }

    /// Atomically consume an exact add reservation, project it, and advance the witnessed pin.
    pub(crate) fn finalize_add(
        &self,
        entry: &DirectoryEntry,
        checkpoint: &PersistedRegistryCheckpoint,
    ) -> Result<()> {
        validate_persisted_checkpoint(checkpoint)?;
        let mutation = entry.registry_addition()?;
        let canonical_mutation = bounded_mutation_bytes(&mutation)?;
        let local_request = bounded_local_request(entry)?;
        let reservation_id = reservation_id(RESERVATION_ADD, &canonical_mutation, &local_request);
        let sequence = sql_u64(entry.sequence, "entry sequence")?;
        let mut conn = self.lock();
        let transaction = conn.transaction_with_behavior(TransactionBehavior::Immediate)?;
        match reservation_for_endpoint(&transaction, &entry.endpoint)? {
            Some(row) => {
                ensure_exact_reservation(
                    &row,
                    &reservation_id,
                    RESERVATION_ADD,
                    &entry.pubkey,
                    sequence,
                    &canonical_mutation,
                    &local_request,
                )?;
                let deleted = transaction.execute(
                    "DELETE FROM directory_mutation_reservations WHERE reservation_id = ?1",
                    params![reservation_id.as_slice()],
                )?;
                if deleted != 1 {
                    return Err(DirectoryError::Replay);
                }
                apply_submission(&transaction, entry.clone(), row.reserved_at_u64()?, true)?;
            }
            None => {
                // A different process may have finalized the same reservation while this process
                // was awaiting the witnessed receipt. Missing is idempotent only when the exact
                // local projection is already present; it never authorizes a new transition.
                if !add_is_exactly_applied(&transaction, entry)? {
                    return Err(DirectoryError::Replay);
                }
            }
        }
        accept_registry_checkpoint_in(&transaction, checkpoint)?;
        transaction.commit()?;
        Ok(())
    }

    /// Test-only raw drain. Production drains pass through witnessed registry removal first.
    #[cfg(test)]
    pub(crate) fn drain(&self, authorization: &DrainAuthorization) -> Result<()> {
        let mut conn = self.lock();
        let transaction = conn.transaction_with_behavior(TransactionBehavior::Immediate)?;
        if reservation_for_endpoint(&transaction, &authorization.endpoint)?.is_some() {
            return Err(DirectoryError::Replay);
        }
        apply_drain(&transaction, authorization, false)?;
        transaction.commit()?;
        Ok(())
    }

    pub(crate) fn reserve_drain(
        &self,
        authorization: &DrainAuthorization,
        now: u64,
    ) -> Result<ReservedMutation<RegistryDirectoryRemove>> {
        let local_request = bounded_local_request(authorization)?;
        let reserved_at = sql_u64(now, "reservation time")?;
        let sequence = sql_u64(authorization.sequence, "drain sequence")?;
        let mut conn = self.lock();
        let mut transaction = conn.transaction_with_behavior(TransactionBehavior::Immediate)?;
        let loft_pubkey = drain_loft_pubkey(&transaction, authorization)?;
        let mutation = authorization.registry_removal(loft_pubkey.clone());
        let canonical_mutation = bounded_mutation_bytes(&mutation)?;
        let reservation_id = reservation_id(RESERVATION_DRAIN, &canonical_mutation, &local_request);
        if let Some(row) = reservation_for_endpoint(&transaction, &authorization.endpoint)? {
            ensure_exact_reservation(
                &row,
                &reservation_id,
                RESERVATION_DRAIN,
                &loft_pubkey,
                sequence,
                &canonical_mutation,
                &local_request,
            )?;
            let previous_checkpoint = registry_checkpoint_from(&transaction)?;
            transaction.commit()?;
            return Ok(ReservedMutation {
                mutation,
                previous_checkpoint,
            });
        }
        ensure_reservation_capacity(&transaction)?;
        {
            let mut savepoint = transaction.savepoint()?;
            apply_drain(&savepoint, authorization, true)?;
            savepoint.rollback()?;
        }
        transaction.execute(
            "INSERT INTO directory_mutation_reservations
                 (reservation_id, endpoint, operation, loft_pubkey, mutation_sequence,
                  canonical_mutation, local_request, reserved_at, capacity_slot)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, 0)",
            params![
                reservation_id.as_slice(),
                authorization.endpoint,
                RESERVATION_DRAIN,
                loft_pubkey,
                sequence,
                canonical_mutation,
                local_request,
                reserved_at,
            ],
        )?;
        let previous_checkpoint = registry_checkpoint_from(&transaction)?;
        transaction.commit()?;
        Ok(ReservedMutation {
            mutation,
            previous_checkpoint,
        })
    }

    /// Authenticate a drain and return its bound loft key without changing durable state.
    ///
    /// The HTTP layer uses this bounded read to charge the correct per-loft bucket before it can
    /// create a reservation. `reserve_drain` intentionally repeats every check transactionally so
    /// this preflight never becomes authorization across a race or another process.
    pub(crate) fn preflight_drain(&self, authorization: &DrainAuthorization) -> Result<String> {
        bounded_local_request(authorization)?;
        drain_loft_pubkey(&self.lock(), authorization)
    }

    pub(crate) fn finalize_drain(
        &self,
        authorization: &DrainAuthorization,
        checkpoint: &PersistedRegistryCheckpoint,
    ) -> Result<()> {
        validate_persisted_checkpoint(checkpoint)?;
        let local_request = bounded_local_request(authorization)?;
        let mut conn = self.lock();
        let transaction = conn.transaction_with_behavior(TransactionBehavior::Immediate)?;
        let loft_pubkey = drain_loft_pubkey(&transaction, authorization)?;
        let mutation = authorization.registry_removal(loft_pubkey.clone());
        let canonical_mutation = bounded_mutation_bytes(&mutation)?;
        let reservation_id = reservation_id(RESERVATION_DRAIN, &canonical_mutation, &local_request);
        let sequence = sql_u64(authorization.sequence, "drain sequence")?;
        match reservation_for_endpoint(&transaction, &authorization.endpoint)? {
            Some(row) => {
                ensure_exact_reservation(
                    &row,
                    &reservation_id,
                    RESERVATION_DRAIN,
                    &loft_pubkey,
                    sequence,
                    &canonical_mutation,
                    &local_request,
                )?;
                let deleted = transaction.execute(
                    "DELETE FROM directory_mutation_reservations WHERE reservation_id = ?1",
                    params![reservation_id.as_slice()],
                )?;
                if deleted != 1 {
                    return Err(DirectoryError::Replay);
                }
                apply_drain(&transaction, authorization, true)?;
            }
            None => {
                if !drain_is_exactly_applied(&transaction, authorization, &mutation)? {
                    return Err(DirectoryError::Replay);
                }
            }
        }
        accept_registry_checkpoint_in(&transaction, checkpoint)?;
        transaction.commit()?;
        Ok(())
    }

    pub(crate) fn pending_mutations(&self, limit: usize) -> Result<Vec<PendingMutation>> {
        let conn = self.lock();
        let limit = limit.clamp(1, MAX_MUTATION_RESERVATIONS);
        let mut statement = conn.prepare(
            "SELECT reservation_id, endpoint, operation, loft_pubkey, mutation_sequence,
                    canonical_mutation, local_request, reserved_at, capacity_slot
             FROM directory_mutation_reservations
             ORDER BY reserved_at, reservation_id LIMIT ?1",
        )?;
        let rows = statement.query_map(params![limit as i64], reservation_row_from_sql)?;
        rows.map(|row| pending_mutation_from_row(&conn, &row?))
            .collect()
    }

    pub(crate) fn has_pending_mutations(&self) -> Result<bool> {
        let count: i64 = self.lock().query_row(
            "SELECT COUNT(*) FROM directory_mutation_reservations",
            [],
            |row| row.get(0),
        )?;
        Ok(count != 0)
    }

    pub(crate) fn finalize_pending_mutation(
        &self,
        pending: &PendingMutation,
        checkpoint: &PersistedRegistryCheckpoint,
    ) -> Result<()> {
        match pending {
            PendingMutation::Add { entry, .. } => self.finalize_add(entry, checkpoint),
            PendingMutation::Drain { authorization, .. } => {
                self.finalize_drain(authorization, checkpoint)
            }
        }
    }

    /// Test-only unbound probe recording. Production accepts only the exact leased claim returned
    /// by the supervised prober through `record_claim_probe_with_retention`.
    #[cfg(test)]
    pub(crate) fn record_probe(&self, result: &ProbeResult, now: u64) -> Result<LoftState> {
        self.record_probe_with_retention(result, now, None)
    }

    #[cfg(test)]
    pub(crate) fn record_probe_with_retention(
        &self,
        result: &ProbeResult,
        now: u64,
        retention_update: Option<RetentionUpdate>,
    ) -> Result<LoftState> {
        Ok(self
            .record_probe_inner(result, now, retention_update, None)?
            .expect("an unbound probe result cannot be stale"))
    }

    /// Record a supervised probe only if the endpoint still names the exact claim that was leased.
    /// Network work happens without the database lock, so an expired claim may be replaced while
    /// its last probe is in flight. Such a result belongs to the old key and must be discarded.
    pub(crate) fn record_claim_probe_with_retention(
        &self,
        result: &ProbeResult,
        now: u64,
        retention_update: Option<RetentionUpdate>,
        expected_pubkey: &str,
        expected_sequence: u64,
    ) -> Result<Option<LoftState>> {
        self.record_probe_inner(
            result,
            now,
            retention_update,
            Some((expected_pubkey, expected_sequence)),
        )
    }

    fn record_probe_inner(
        &self,
        result: &ProbeResult,
        now: u64,
        retention_update: Option<RetentionUpdate>,
        expected_claim: Option<(&str, u64)>,
    ) -> Result<Option<LoftState>> {
        let mut stored_result = result.clone();
        stored_result.at = now;
        if !stored_result.utilization.is_finite()
            || !(0.0..=1.0).contains(&stored_result.utilization)
        {
            stored_result.utilization = 1.0;
            stored_result.stored_and_returned = false;
            stored_result.detail = Some("loft returned invalid utilization".into());
        }
        let healthy = stored_result.healthy();
        let now_sql = sql_u64(now, "probe time")?;
        let mut conn = self.lock();
        let transaction = conn.transaction_with_behavior(TransactionBehavior::Immediate)?;
        expire_pending(&transaction, now)?;
        expire_pending_claims(&transaction, now)?;
        if expected_claim.is_some()
            && reservation_for_endpoint(&transaction, &stored_result.endpoint)?.is_some()
        {
            // The probe was leased before a mutation reserved this endpoint. Its network result
            // belongs to the pre-reservation state and must not race the exact projection.
            transaction.commit()?;
            return Ok(None);
        }
        let row: Option<(LoftProbeStateRow, Vec<u8>, i64)> = transaction
            .query_row(
                "SELECT state, first_seen, fail_streak, degraded_at, clean_since, drain_after,
                        entry, ownership_proven
                 FROM lofts WHERE endpoint = ?1",
                params![result.endpoint],
                |r| {
                    Ok((
                        (
                            r.get(0)?,
                            r.get(1)?,
                            r.get(2)?,
                            r.get(3)?,
                            r.get(4)?,
                            r.get(5)?,
                        ),
                        r.get(6)?,
                        r.get(7)?,
                    ))
                },
            )
            .optional()?;

        let Some((
            (
                mut state,
                mut first_seen,
                mut fail_streak,
                mut degraded_at,
                mut clean_since,
                mut drain_after,
            ),
            entry_blob,
            ownership_proven,
        )) = row
        else {
            return Err(DirectoryError::NotFound);
        };
        if let Some((expected_pubkey, expected_sequence)) = expected_claim {
            let current: DirectoryEntry = serde_json::from_slice(&entry_blob)?;
            if current.pubkey != expected_pubkey || current.sequence != expected_sequence {
                let candidate: Option<Vec<u8>> = transaction
                    .query_row(
                        "SELECT entry FROM pending_claims
                         WHERE endpoint = ?1 AND loft_pubkey = ?2 AND mutation_sequence = ?3",
                        params![
                            stored_result.endpoint,
                            expected_pubkey,
                            sql_u64(expected_sequence, "expected candidate sequence")?,
                        ],
                        |row| row.get(0),
                    )
                    .optional()?;
                let Some(candidate) = candidate else {
                    transaction.commit()?;
                    return Ok(None);
                };
                if ownership_proven != 0 {
                    transaction.execute(
                        "DELETE FROM pending_claims WHERE endpoint = ?1",
                        params![stored_result.endpoint],
                    )?;
                    transaction.commit()?;
                    return Ok(None);
                }
                if !healthy {
                    transaction.commit()?;
                    return Ok(Some(LoftState::Pending));
                }

                let mut candidate: DirectoryEntry = serde_json::from_slice(&candidate)?;
                if candidate.pubkey != expected_pubkey || candidate.sequence != expected_sequence {
                    return Err(DirectoryError::Malformed(
                        "pending claim key or sequence disagrees with its projection".into(),
                    ));
                }
                candidate.state = LoftState::Pending;
                candidate.health = Health::default();
                candidate.utilization = 0.0;
                candidate.drain_after = None;
                candidate.last_mutation_sequence = candidate.sequence;
                transaction.execute(
                    "UPDATE lofts
                     SET entry = ?2, state = 'pending', first_seen = ?3, last_probe = 0,
                         fail_streak = 0, probes_ok = 0, probes_total = 0,
                         degraded_at = NULL, drain_after = NULL,
                         mutation_sequence = ?4, clean_since = NULL, next_probe_at = ?3,
                         ownership_proven = 0
                     WHERE endpoint = ?1 AND ownership_proven = 0",
                    params![
                        stored_result.endpoint,
                        serde_json::to_vec(&candidate)?,
                        now_sql,
                        sql_u64(candidate.sequence, "winning candidate sequence")?,
                    ],
                )?;
                transaction.execute(
                    "DELETE FROM probes WHERE endpoint = ?1",
                    params![stored_result.endpoint],
                )?;
                transaction.execute(
                    "DELETE FROM retention_canaries WHERE endpoint = ?1",
                    params![stored_result.endpoint],
                )?;
                transaction.execute(
                    "DELETE FROM pending_claims WHERE endpoint = ?1",
                    params![stored_result.endpoint],
                )?;
                state = "pending".into();
                first_seen = now_sql;
                fail_streak = 0;
                degraded_at = None;
                clean_since = None;
                drain_after = None;
            }
        }

        transaction.execute(
            "INSERT INTO probes (endpoint, at, result, healthy) VALUES (?1, ?2, ?3, ?4)",
            params![
                stored_result.endpoint,
                now_sql,
                serde_json::to_vec(&stored_result)?,
                i64::from(healthy),
            ],
        )?;
        let cutoff = sql_u64(
            now.saturating_sub(PROBE_RETENTION_SECS),
            "probe retention cutoff",
        )?;
        transaction.execute("DELETE FROM probes WHERE at < ?1", params![cutoff])?;

        let fail_streak = if healthy {
            0
        } else {
            fail_streak.saturating_add(1)
        };
        let (probes_ok, probes_total): (i64, i64) = transaction.query_row(
            "SELECT COALESCE(SUM(healthy), 0), COUNT(*)
             FROM probes WHERE endpoint = ?1 AND at >= ?2",
            params![stored_result.endpoint, cutoff],
            |row| Ok((row.get(0)?, row.get(1)?)),
        )?;

        let current = parse_state(&state);
        let (next, degraded_at, clean_since) = match current {
            LoftState::Draining => {
                if drain_after.is_some_and(|after| after <= now_sql) {
                    (LoftState::Removed, Some(now_sql), None)
                } else {
                    (LoftState::Draining, degraded_at, None)
                }
            }
            LoftState::Removed => (LoftState::Removed, degraded_at, None),

            LoftState::Pending => {
                let first_seen = u64::try_from(first_seen).unwrap_or(0);
                if now.saturating_sub(first_seen) >= PENDING_EXPIRE_SECS {
                    (LoftState::Removed, Some(now_sql), None)
                } else if healthy {
                    let clean_since = clean_since.unwrap_or(now_sql);
                    if now.saturating_sub(u64::try_from(clean_since).unwrap_or(now))
                        >= PROMOTE_AFTER_SECS
                    {
                        let public_entries: i64 = transaction.query_row(
                            "SELECT COUNT(*) FROM lofts
                             WHERE state IN ('active', 'degraded', 'draining')",
                            [],
                            |row| row.get(0),
                        )?;
                        if public_entries < MAX_PUBLIC_ENTRIES as i64 {
                            (LoftState::Active, None, None)
                        } else {
                            (LoftState::Pending, None, Some(clean_since))
                        }
                    } else {
                        (LoftState::Pending, None, Some(clean_since))
                    }
                } else {
                    (LoftState::Pending, None, None)
                }
            }

            LoftState::Active => {
                if fail_streak >= DEGRADE_AFTER_FAILURES as i64 {
                    (LoftState::Degraded, Some(now_sql), None)
                } else {
                    (LoftState::Active, None, None)
                }
            }

            LoftState::Degraded => {
                if healthy {
                    // Recovery is immediate: a node that is working again should carry mail again.
                    (LoftState::Active, None, None)
                } else if degraded_at.is_some_and(|at| {
                    now.saturating_sub(u64::try_from(at).unwrap_or(0)) >= REMOVE_AFTER_SECS
                }) {
                    (LoftState::Removed, degraded_at, None)
                } else {
                    (LoftState::Degraded, degraded_at, None)
                }
            }
        };

        transaction.execute(
            "UPDATE lofts SET state = ?2, last_probe = ?3, fail_streak = ?4,
                              probes_ok = ?5, probes_total = ?6, degraded_at = ?7,
                              clean_since = ?8, next_probe_at = ?9,
                              ownership_proven = CASE WHEN ?10 != 0 THEN 1 ELSE ownership_proven END
             WHERE endpoint = ?1",
            params![
                stored_result.endpoint,
                state_name(next),
                now_sql,
                fail_streak,
                probes_ok,
                probes_total,
                degraded_at,
                clean_since,
                sql_u64(now.saturating_add(PROBE_INTERVAL_SECS), "next probe time")?,
                i64::from(healthy),
            ],
        )?;
        if healthy {
            transaction.execute(
                "DELETE FROM pending_claims WHERE endpoint = ?1",
                params![stored_result.endpoint],
            )?;
        }

        match retention_update {
            Some(RetentionUpdate::Created {
                recipient_seed,
                event_id,
                published_at,
            }) => {
                transaction.execute(
                    "INSERT INTO retention_canaries
                         (endpoint, recipient_seed, event_id, published_at, last_checked_at)
                     VALUES (?1, ?2, ?3, ?4, 0)
                     ON CONFLICT(endpoint) DO NOTHING",
                    params![
                        stored_result.endpoint,
                        recipient_seed.as_slice(),
                        event_id.as_slice(),
                        sql_u64(published_at, "canary publication time")?,
                    ],
                )?;
            }
            Some(RetentionUpdate::Checked { checked_at, rotate }) => {
                if rotate {
                    transaction.execute(
                        "DELETE FROM retention_canaries WHERE endpoint = ?1",
                        params![stored_result.endpoint],
                    )?;
                } else {
                    transaction.execute(
                        "UPDATE retention_canaries SET last_checked_at = ?2 WHERE endpoint = ?1",
                        params![
                            stored_result.endpoint,
                            sql_u64(checked_at, "canary check time")?,
                        ],
                    )?;
                }
            }
            None => {}
        }

        // Keep the entry's published observations in step with what we just measured.
        if let Some(blob) = transaction
            .query_row(
                "SELECT entry FROM lofts WHERE endpoint = ?1",
                params![stored_result.endpoint],
                |r| r.get::<_, Vec<u8>>(0),
            )
            .optional()?
        {
            let mut entry: DirectoryEntry = serde_json::from_slice(&blob)?;
            entry.state = next;
            entry.utilization = stored_result.utilization;
            entry.health = Health {
                uptime_30d: if probes_total == 0 {
                    1.0
                } else {
                    probes_ok as f64 / probes_total as f64
                },
                probe_fail_streak: u32::try_from(fail_streak).unwrap_or(u32::MAX),
                last_probe: now,
            };
            transaction.execute(
                "UPDATE lofts SET entry = ?2 WHERE endpoint = ?1",
                params![stored_result.endpoint, serde_json::to_vec(&entry)?],
            )?;
        }

        transaction.commit()?;
        Ok(Some(next))
    }

    /// Entries safe to place in the client-facing snapshot.
    ///
    /// Pending registrations remain visible in the append-only registry mutation log but do not
    /// consume this fixed-size routing document. Removed entries are likewise omitted.
    pub fn entries(&self) -> Result<Vec<DirectoryEntry>> {
        let conn = self.lock();
        let mut stmt = conn.prepare(
            "SELECT entry FROM lofts
             WHERE state IN ('active', 'degraded', 'draining')
             ORDER BY endpoint LIMIT ?1",
        )?;
        let rows = stmt.query_map(params![(MAX_PUBLIC_ENTRIES + 1) as i64], |r| {
            r.get::<_, Vec<u8>>(0)
        })?;

        let mut out = Vec::new();
        for row in rows {
            out.push(serde_json::from_slice(&row?)?);
        }
        if out.len() > MAX_PUBLIC_ENTRIES {
            return Err(DirectoryError::ResponseTooLarge);
        }
        Ok(out)
    }

    /// Lease due entries in fair order before network work starts. A whole-sweep timeout therefore
    /// cannot make the same slow first page starve every later endpoint on the next tick.
    pub(crate) fn claim_probe_candidates(
        &self,
        now: u64,
        limit: usize,
    ) -> Result<Vec<DirectoryEntry>> {
        let now_sql = sql_u64(now, "probe claim time")?;
        let lease_until = sql_u64(now.saturating_add(PROBE_INTERVAL_SECS), "probe lease time")?;
        let limit = limit.clamp(1, MAX_PROBE_CANDIDATES);
        let mut conn = self.lock();
        let transaction = conn.transaction_with_behavior(TransactionBehavior::Immediate)?;
        expire_pending(&transaction, now)?;
        expire_pending_claims(&transaction, now)?;

        let selected: Vec<(String, String, Vec<u8>, i64)> = {
            let mut statement = transaction.prepare(
                "SELECT endpoint, loft_pubkey, entry, candidate FROM (
                     SELECT endpoint, '' AS loft_pubkey, entry, 0 AS candidate,
                            next_probe_at, last_probe
                     FROM lofts
                     WHERE state != 'removed' AND next_probe_at <= ?1
                       AND NOT EXISTS (
                           SELECT 1 FROM directory_mutation_reservations reservation
                           WHERE reservation.endpoint = lofts.endpoint
                       )
                     UNION ALL
                     SELECT endpoint, loft_pubkey, entry, 1 AS candidate,
                            next_probe_at, 0 AS last_probe
                     FROM pending_claims
                     WHERE next_probe_at <= ?1
                       AND NOT EXISTS (
                           SELECT 1 FROM directory_mutation_reservations reservation
                           WHERE reservation.endpoint = pending_claims.endpoint
                       )
                 )
                 ORDER BY next_probe_at, last_probe, endpoint, loft_pubkey
                 LIMIT ?2",
            )?;
            let rows = statement.query_map(params![now_sql, limit as i64], |row| {
                Ok((row.get(0)?, row.get(1)?, row.get(2)?, row.get(3)?))
            })?;
            rows.collect::<std::result::Result<Vec<_>, _>>()?
        };

        for (endpoint, loft_pubkey, _, candidate) in &selected {
            if *candidate == 0 {
                transaction.execute(
                    "UPDATE lofts SET next_probe_at = ?2 WHERE endpoint = ?1",
                    params![endpoint, lease_until],
                )?;
            } else {
                transaction.execute(
                    "UPDATE pending_claims SET next_probe_at = ?3
                     WHERE endpoint = ?1 AND loft_pubkey = ?2",
                    params![endpoint, loft_pubkey, lease_until],
                )?;
            }
        }
        transaction.commit()?;

        selected
            .into_iter()
            .map(|(_, _, blob, _)| serde_json::from_slice(&blob).map_err(DirectoryError::from))
            .collect()
    }

    pub(crate) fn retention_work(
        &self,
        endpoint: &str,
        retention_days: u64,
        now: u64,
    ) -> Result<Option<RetentionWork>> {
        let conn = self.lock();
        if reservation_for_endpoint(&conn, endpoint)?.is_some() {
            return Ok(None);
        }
        let row: Option<(Vec<u8>, Vec<u8>, i64, i64)> = conn
            .query_row(
                "SELECT recipient_seed, event_id, published_at, last_checked_at
                 FROM retention_canaries WHERE endpoint = ?1",
                params![endpoint],
                |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?, row.get(3)?)),
            )
            .optional()?;
        let Some((seed, event_id, published_at, last_checked_at)) = row else {
            return Ok(Some(RetentionWork::Create));
        };
        let recipient_seed = fixed_32(&seed, "retention canary recipient seed")?;
        let event_id = fixed_32(&event_id, "retention canary event id")?;
        let published_at = u64::try_from(published_at)
            .map_err(|_| DirectoryError::Malformed("negative canary publication time".into()))?;
        let last_checked_at = u64::try_from(last_checked_at)
            .map_err(|_| DirectoryError::Malformed("negative canary check time".into()))?;
        let target_age_secs = retention_days
            .saturating_mul(24 * 60 * 60)
            .saturating_sub(RETENTION_CHECK_MARGIN_SECS)
            .max(PROBE_INTERVAL_SECS);
        let target_at = published_at.saturating_add(target_age_secs);
        let due_at = if last_checked_at == 0 {
            published_at.saturating_add(RETENTION_CHECK_INTERVAL_SECS.min(target_age_secs))
        } else {
            last_checked_at
                .saturating_add(RETENTION_CHECK_INTERVAL_SECS)
                .min(target_at)
        };
        if now < due_at {
            return Ok(None);
        }
        Ok(Some(RetentionWork::Check(RetentionCanary {
            recipient_seed,
            event_id,
            published_at,
            target_age_secs,
        })))
    }

    pub fn entry(&self, endpoint: &str) -> Result<DirectoryEntry> {
        let conn = self.lock();
        let blob: Option<Vec<u8>> = conn
            .query_row(
                "SELECT entry FROM lofts WHERE endpoint = ?1",
                params![endpoint],
                |r| r.get(0),
            )
            .optional()?;
        let blob = match blob {
            Some(blob) => blob,
            None => conn
                .query_row(
                    "SELECT entry FROM pending_claims
                     WHERE endpoint = ?1 ORDER BY first_seen, loft_pubkey LIMIT 1",
                    params![endpoint],
                    |row| row.get(0),
                )
                .optional()?
                .ok_or(DirectoryError::NotFound)?,
        };
        Ok(serde_json::from_slice(&blob)?)
    }

    /// Refuse to serve a pre-transparency database whose existing registrations cannot be tied to
    /// a previously accepted shared-log checkpoint. A fresh empty database is safe; after the
    /// first witnessed mutation, the durable checkpoint establishes the new invariant.
    pub fn verify_registry_logging_ready(&self) -> Result<()> {
        let known: i64 = self.lock().query_row(
            "SELECT (SELECT COUNT(*) FROM lofts) + (SELECT COUNT(*) FROM pending_claims)",
            [],
            |row| row.get(0),
        )?;
        if known > 0 && self.registry_checkpoint()?.is_none() {
            return Err(DirectoryError::RegistryProof(
                "existing directory entries predate transparency logging; migrate or re-enrol the lofts before serving"
                    .into(),
            ));
        }
        Ok(())
    }

    /// Re-prove that a public server is backed by the descriptor-held private database opened by
    /// [`Directory::open`] or [`Directory::open_with_signing_key`].
    ///
    /// `Directory::in_memory` deliberately remains available for unit and loopback fixture use,
    /// but it cannot satisfy the durability or signing-seed custody contract of a public service.
    /// Revalidating the held file and parent descriptors here also detects replacement of either
    /// named object between construction and listener startup.
    pub(crate) fn verify_public_storage_ready(&self) -> Result<()> {
        let custody = self.database_custody.as_ref().ok_or_else(|| {
            DirectoryError::Malformed(
                "public directory service requires descriptor-held persistent storage".into(),
            )
        })?;
        custody.verify_named()?;
        Ok(())
    }

    /// Persist proof that the supervised prober completed a bounded sweep attempt.
    pub(crate) fn mark_probe_sweep(&self, at: u64) -> Result<()> {
        let at = sql_u64(at, "prober heartbeat")?;
        self.lock().execute(
            "INSERT INTO directory_meta (key, value) VALUES (?1, ?2)
             ON CONFLICT(key) DO UPDATE SET value = excluded.value",
            params![PROBER_HEARTBEAT, at.to_le_bytes().as_slice()],
        )?;
        Ok(())
    }

    /// Check only the durable supervised-prober heartbeat used to gate signed public views.
    ///
    /// This is intentionally one indexed metadata lookup. Public reads must not turn the full
    /// SQLite integrity check in `local_readiness` into a request-amplification surface.
    pub(crate) fn prober_freshness(&self, now: u64) -> Result<()> {
        Self::verify_prober_heartbeat(&self.lock(), now)
    }

    /// Check durable storage and the freshness of the supervised prober heartbeat.
    pub(crate) fn local_readiness(&self, now: u64) -> Result<()> {
        if let Some(custody) = self.database_custody.as_ref() {
            custody.verify_named()?;
        }
        let conn = self.lock();
        let quick_check: String = conn.query_row("PRAGMA quick_check(1)", [], |row| row.get(0))?;
        if quick_check != "ok" {
            return Err(DirectoryError::NotReady);
        }
        Self::verify_prober_heartbeat(&conn, now)?;
        drop(conn);
        if let Some(custody) = self.database_custody.as_ref() {
            custody.verify_named()?;
        }
        Ok(())
    }

    fn verify_prober_heartbeat(conn: &Connection, now: u64) -> Result<()> {
        let heartbeat: Option<Vec<u8>> = conn
            .query_row(
                "SELECT value FROM directory_meta WHERE key = ?1",
                params![PROBER_HEARTBEAT],
                |row| row.get(0),
            )
            .optional()?;
        let heartbeat = heartbeat.ok_or(DirectoryError::NotReady)?;
        let bytes: [u8; 8] = heartbeat
            .as_slice()
            .try_into()
            .map_err(|_| DirectoryError::NotReady)?;
        let heartbeat = u64::from_le_bytes(bytes);
        if heartbeat > now.saturating_add(PROBE_INTERVAL_SECS)
            || now.saturating_sub(heartbeat) > PROBER_FRESHNESS_SECS
        {
            return Err(DirectoryError::NotReady);
        }
        Ok(())
    }

    pub(crate) fn registry_checkpoint(&self) -> Result<Option<PersistedRegistryCheckpoint>> {
        registry_checkpoint_from(&self.lock())
    }

    /// Raw measurements, published so anyone can recompute every weight from public data. If we
    /// fudge a weight, it is arithmetic someone else can catch.
    pub fn probes(&self, endpoint: &str, limit: usize) -> Result<Vec<ProbeResult>> {
        let conn = self.lock();
        let limit = limit.clamp(1, 1_000);
        let mut stmt = conn
            .prepare("SELECT result FROM probes WHERE endpoint = ?1 ORDER BY at DESC LIMIT ?2")?;
        let rows = stmt.query_map(params![endpoint, limit as i64], |r| r.get::<_, Vec<u8>>(0))?;

        let mut out = Vec::new();
        for row in rows {
            out.push(serde_json::from_slice(&row?)?);
        }
        Ok(out)
    }

    pub(crate) fn probe_page(
        &self,
        endpoint: &str,
        cursor: Option<u64>,
        limit: usize,
        now: u64,
    ) -> Result<ProbePage> {
        let conn = self.lock();
        let limit = limit.clamp(1, 500);
        let cursor = cursor
            .map(|value| sql_u64(value, "probe cursor"))
            .transpose()?;
        let cutoff = sql_u64(
            now.saturating_sub(PROBE_RETENTION_SECS),
            "probe evidence cutoff",
        )?;
        let mut statement = conn.prepare(
            "SELECT id, result FROM probes
             WHERE endpoint = ?1 AND at >= ?2 AND (?3 IS NULL OR id < ?3)
             ORDER BY id DESC LIMIT ?4",
        )?;
        let rows = statement.query_map(
            params![endpoint, cutoff, cursor, (limit + 1) as i64],
            |row| Ok((row.get::<_, i64>(0)?, row.get::<_, Vec<u8>>(1)?)),
        )?;
        let mut records = rows.collect::<std::result::Result<Vec<_>, _>>()?;
        let more = records.len() > limit;
        if more {
            records.pop();
        }
        let next_cursor = if more {
            records.last().and_then(|(id, _)| u64::try_from(*id).ok())
        } else {
            None
        };
        let probes = records
            .into_iter()
            .map(|(_, blob)| serde_json::from_slice(&blob).map_err(DirectoryError::from))
            .collect::<Result<Vec<_>>>()?;
        Ok(ProbePage {
            probes,
            next_cursor,
            more,
        })
    }

    /// Force a state only inside crate tests. Production state transitions are probe- or
    /// authorization-driven and never gain ownership evidence through an operator shortcut.
    #[cfg(any(test, feature = "test-utilities"))]
    #[doc(hidden)]
    pub fn set_state(&self, endpoint: &str, state: LoftState) -> Result<()> {
        let mut conn = self.lock();
        let transaction = conn.transaction_with_behavior(TransactionBehavior::Immediate)?;
        if reservation_for_endpoint(&transaction, endpoint)?.is_some() {
            return Err(DirectoryError::NotReady);
        }
        let current: Option<String> = transaction
            .query_row(
                "SELECT state FROM lofts WHERE endpoint = ?1",
                params![endpoint],
                |row| row.get(0),
            )
            .optional()?;
        let current = current.ok_or(DirectoryError::NotFound)?;
        if matches!(
            state,
            LoftState::Active | LoftState::Degraded | LoftState::Draining
        ) && !matches!(
            parse_state(&current),
            LoftState::Active | LoftState::Degraded | LoftState::Draining
        ) {
            let published: i64 = transaction.query_row(
                "SELECT COUNT(*) FROM lofts
                 WHERE state IN ('active', 'degraded', 'draining')",
                [],
                |row| row.get(0),
            )?;
            if published >= MAX_PUBLIC_ENTRIES as i64 {
                return Err(DirectoryError::ResponseTooLarge);
            }
        }
        let proves_ownership = matches!(
            state,
            LoftState::Active | LoftState::Degraded | LoftState::Draining
        );
        let changed = transaction.execute(
            "UPDATE lofts
             SET state = ?2,
                 ownership_proven = CASE WHEN ?3 != 0 THEN 1 ELSE ownership_proven END
             WHERE endpoint = ?1",
            params![endpoint, state_name(state), i64::from(proves_ownership)],
        )?;
        debug_assert_eq!(changed, 1);
        if let Some(blob) = transaction
            .query_row(
                "SELECT entry FROM lofts WHERE endpoint = ?1",
                params![endpoint],
                |r| r.get::<_, Vec<u8>>(0),
            )
            .optional()?
        {
            let mut entry: DirectoryEntry = serde_json::from_slice(&blob)?;
            entry.state = state;
            transaction.execute(
                "UPDATE lofts SET entry = ?2 WHERE endpoint = ?1",
                params![endpoint, serde_json::to_vec(&entry)?],
            )?;
        }
        transaction.commit()?;
        Ok(())
    }
}

#[derive(Debug, Clone)]
struct ReservationRow {
    reservation_id: Vec<u8>,
    endpoint: String,
    operation: String,
    loft_pubkey: String,
    mutation_sequence: i64,
    canonical_mutation: Vec<u8>,
    local_request: Vec<u8>,
    reserved_at: i64,
    capacity_slot: i64,
}

impl ReservationRow {
    fn reserved_at_u64(&self) -> Result<u64> {
        u64::try_from(self.reserved_at).map_err(|_| {
            DirectoryError::Malformed("reservation time is negative or too large".into())
        })
    }
}

fn reservation_row_from_sql(row: &rusqlite::Row<'_>) -> rusqlite::Result<ReservationRow> {
    Ok(ReservationRow {
        reservation_id: row.get(0)?,
        endpoint: row.get(1)?,
        operation: row.get(2)?,
        loft_pubkey: row.get(3)?,
        mutation_sequence: row.get(4)?,
        canonical_mutation: row.get(5)?,
        local_request: row.get(6)?,
        reserved_at: row.get(7)?,
        capacity_slot: row.get(8)?,
    })
}

fn reservation_for_endpoint(conn: &Connection, endpoint: &str) -> Result<Option<ReservationRow>> {
    conn.query_row(
        "SELECT reservation_id, endpoint, operation, loft_pubkey, mutation_sequence,
                canonical_mutation, local_request, reserved_at, capacity_slot
         FROM directory_mutation_reservations WHERE endpoint = ?1",
        params![endpoint],
        reservation_row_from_sql,
    )
    .optional()
    .map_err(DirectoryError::from)
}

fn bounded_mutation_bytes<T: serde::Serialize>(mutation: &T) -> Result<Vec<u8>> {
    let encoded = serde_json::to_vec(mutation)?;
    if encoded.is_empty() || encoded.len() > MAX_LOCAL_MUTATION_BYTES {
        return Err(DirectoryError::Malformed(
            "canonical directory mutation is outside the supported bounds".into(),
        ));
    }
    Ok(encoded)
}

fn bounded_local_request<T: serde::Serialize>(request: &T) -> Result<Vec<u8>> {
    let encoded = serde_json::to_vec(request)?;
    if encoded.is_empty() || encoded.len() > MAX_LOCAL_MUTATION_BYTES {
        return Err(DirectoryError::Malformed(
            "directory mutation request is outside the supported bounds".into(),
        ));
    }
    Ok(encoded)
}

fn reservation_id(operation: &str, canonical_mutation: &[u8], local_request: &[u8]) -> [u8; 32] {
    let mut digest = Sha256::new();
    digest.update(RESERVATION_DOMAIN);
    digest.update((operation.len() as u64).to_le_bytes());
    digest.update(operation.as_bytes());
    digest.update((canonical_mutation.len() as u64).to_le_bytes());
    digest.update(canonical_mutation);
    digest.update((local_request.len() as u64).to_le_bytes());
    digest.update(local_request);
    digest.finalize().into()
}

fn validate_reservation_row(row: &ReservationRow) -> Result<()> {
    if row.reservation_id.len() != 32
        || !matches!(row.operation.as_str(), RESERVATION_ADD | RESERVATION_DRAIN)
        || row.endpoint.is_empty()
        || row.endpoint.len() > 2_048
        || row.loft_pubkey.len() != 64
        || row.mutation_sequence < 0
        || row.reserved_at < 0
        || !matches!(row.capacity_slot, 0 | 1)
        || row.canonical_mutation.is_empty()
        || row.canonical_mutation.len() > MAX_LOCAL_MUTATION_BYTES
        || row.local_request.is_empty()
        || row.local_request.len() > MAX_LOCAL_MUTATION_BYTES
    {
        return Err(DirectoryError::Malformed(
            "persisted directory mutation reservation is malformed".into(),
        ));
    }
    let expected = reservation_id(&row.operation, &row.canonical_mutation, &row.local_request);
    if row.reservation_id.as_slice() != expected {
        return Err(DirectoryError::Malformed(
            "persisted directory mutation reservation digest is invalid".into(),
        ));
    }
    Ok(())
}

#[allow(clippy::too_many_arguments)]
fn ensure_exact_reservation(
    row: &ReservationRow,
    reservation_id: &[u8; 32],
    operation: &str,
    loft_pubkey: &str,
    mutation_sequence: i64,
    canonical_mutation: &[u8],
    local_request: &[u8],
) -> Result<()> {
    validate_reservation_row(row)?;
    if row.reservation_id.as_slice() != reservation_id
        || row.operation != operation
        || row.loft_pubkey != loft_pubkey
        || row.mutation_sequence != mutation_sequence
        || row.canonical_mutation != canonical_mutation
        || row.local_request != local_request
    {
        return Err(DirectoryError::Replay);
    }
    Ok(())
}

fn ensure_reservation_capacity(conn: &Connection) -> Result<()> {
    let count: i64 = conn.query_row(
        "SELECT COUNT(*) FROM directory_mutation_reservations",
        [],
        |row| row.get(0),
    )?;
    if count >= MAX_MUTATION_RESERVATIONS as i64 {
        return Err(DirectoryError::Unavailable);
    }
    Ok(())
}

fn pending_load(conn: &Connection) -> Result<i64> {
    conn.query_row(
        "SELECT
             (SELECT COUNT(*) FROM lofts WHERE state = 'pending')
             + (SELECT COUNT(*) FROM pending_claims)
             + (SELECT COALESCE(SUM(capacity_slot), 0)
                FROM directory_mutation_reservations)",
        [],
        |row| row.get(0),
    )
    .map_err(DirectoryError::from)
}

fn apply_submission(
    conn: &Connection,
    mut entry: DirectoryEntry,
    now: u64,
    allow_exact_retry: bool,
) -> Result<()> {
    validate_submission(&entry)?;
    let submitted_key = entry.verify()?;
    let sequence = sql_u64(entry.sequence, "entry sequence")?;
    let existing: Option<(String, i64, Vec<u8>, i64, i64)> = conn
        .query_row(
            "SELECT state, first_seen, entry, mutation_sequence, ownership_proven
             FROM lofts WHERE endpoint = ?1",
            params![entry.endpoint],
            |row| {
                Ok((
                    row.get(0)?,
                    row.get(1)?,
                    row.get(2)?,
                    row.get(3)?,
                    row.get(4)?,
                ))
            },
        )
        .optional()?;

    // Proven bindings stay key-bound forever. An expired claim that never passed a probe may be
    // replaced, but the replacement starts an independent mutation stream and must undergo the
    // complete probation interval with none of the old claim's observations.
    let (state, first_seen, reenrolling, ownership_proven) = match existing {
        Some((state, first_seen, blob, stored_sequence, ownership_proven)) => {
            let previous: DirectoryEntry = serde_json::from_slice(&blob)?;
            let same_key = previous.verify()?.to_bytes() == submitted_key.to_bytes();
            if same_key {
                if sequence < stored_sequence {
                    return Err(DirectoryError::Replay);
                }
                if sequence == stored_sequence {
                    if allow_exact_retry
                        && previous.registry_addition()? == entry.registry_addition()?
                    {
                        return Ok(());
                    }
                    return Err(DirectoryError::Replay);
                }
                entry.health = previous.health;
                entry.utilization = previous.utilization;
                if previous.state == LoftState::Removed {
                    (
                        "pending".to_string(),
                        sql_u64(now, "reenrollment time")?,
                        true,
                        ownership_proven,
                    )
                } else {
                    entry.state = previous.state;
                    entry.drain_after = previous.drain_after;
                    (state, first_seen, false, ownership_proven)
                }
            } else {
                if ownership_proven != 0
                    || !matches!(previous.state, LoftState::Pending | LoftState::Removed)
                {
                    return Err(DirectoryError::KeyMismatch);
                }
                let candidate: Option<(Vec<u8>, i64)> = conn
                    .query_row(
                        "SELECT entry, mutation_sequence FROM pending_claims
                         WHERE endpoint = ?1 AND loft_pubkey = ?2",
                        params![entry.endpoint, entry.pubkey],
                        |row| Ok((row.get(0)?, row.get(1)?)),
                    )
                    .optional()?;
                if let Some((blob, stored_sequence)) = candidate {
                    let previous: DirectoryEntry = serde_json::from_slice(&blob)?;
                    if sequence < stored_sequence {
                        return Err(DirectoryError::Replay);
                    }
                    if sequence == stored_sequence {
                        if allow_exact_retry
                            && previous.registry_addition()? == entry.registry_addition()?
                        {
                            return Ok(());
                        }
                        return Err(DirectoryError::Replay);
                    }
                    entry.state = LoftState::Pending;
                    entry.health = Health::default();
                    entry.utilization = 0.0;
                    entry.drain_after = None;
                    entry.last_mutation_sequence = entry.sequence;
                    conn.execute(
                        "UPDATE pending_claims
                         SET entry = ?3, mutation_sequence = ?4
                         WHERE endpoint = ?1 AND loft_pubkey = ?2",
                        params![
                            entry.endpoint,
                            entry.pubkey,
                            serde_json::to_vec(&entry)?,
                            sequence,
                        ],
                    )?;
                    return Ok(());
                }
                // A first mutation must start at zero (legacy) or one. Exact higher-sequence
                // retries already matched an existing projection or reservation above.
                if entry.sequence > 1 {
                    return Err(DirectoryError::Replay);
                }
                ensure_pending_capacity(conn)?;
                entry.state = LoftState::Pending;
                entry.health = Health::default();
                entry.utilization = 0.0;
                entry.drain_after = None;
                entry.last_mutation_sequence = entry.sequence;
                let first_seen = sql_u64(now, "candidate submission time")?;
                conn.execute(
                    "INSERT INTO pending_claims
                         (endpoint, loft_pubkey, entry, first_seen, mutation_sequence,
                          next_probe_at)
                     VALUES (?1, ?2, ?3, ?4, ?5, ?4)",
                    params![
                        entry.endpoint,
                        entry.pubkey,
                        serde_json::to_vec(&entry)?,
                        first_seen,
                        sequence,
                    ],
                )?;
                return Ok(());
            }
        }
        None => {
            ensure_pending_capacity(conn)?;
            // Sequence zero is the legacy v1 sentinel. Accepting it exactly once keeps old signed
            // clients deployable while the primary key makes its replay fail below.
            if entry.sequence > 1 {
                return Err(DirectoryError::Replay);
            }
            entry.state = LoftState::Pending;
            entry.health = Health::default();
            entry.drain_after = None;
            (
                "pending".to_string(),
                sql_u64(now, "submission time")?,
                false,
                0,
            )
        }
    };
    if reenrolling {
        ensure_pending_capacity(conn)?;
        entry.state = LoftState::Pending;
        entry.health = Health::default();
        entry.utilization = 0.0;
        entry.drain_after = None;
    }
    entry.last_mutation_sequence = entry.sequence;

    conn.execute(
        "INSERT INTO lofts
             (endpoint, entry, state, first_seen, mutation_sequence, next_probe_at,
              ownership_proven)
         VALUES (?1, ?2, ?3, ?4, ?5, ?4, ?6)
         ON CONFLICT(endpoint) DO UPDATE SET
             entry = excluded.entry,
             mutation_sequence = excluded.mutation_sequence,
             ownership_proven = excluded.ownership_proven",
        params![
            entry.endpoint,
            serde_json::to_vec(&entry)?,
            state,
            first_seen,
            sequence,
            ownership_proven,
        ],
    )?;
    if reenrolling {
        conn.execute(
            "UPDATE lofts
             SET state = 'pending', first_seen = ?2, last_probe = 0, fail_streak = 0,
                 probes_ok = 0, probes_total = 0, degraded_at = NULL, drain_after = NULL,
                 clean_since = NULL, next_probe_at = ?2
             WHERE endpoint = ?1",
            params![entry.endpoint, first_seen],
        )?;
        conn.execute(
            "DELETE FROM retention_canaries WHERE endpoint = ?1",
            params![entry.endpoint],
        )?;
        conn.execute(
            "DELETE FROM probes WHERE endpoint = ?1",
            params![entry.endpoint],
        )?;
    }
    Ok(())
}

fn drain_loft_pubkey(conn: &Connection, authorization: &DrainAuthorization) -> Result<String> {
    let blob: Vec<u8> = conn
        .query_row(
            "SELECT entry FROM lofts WHERE endpoint = ?1",
            params![authorization.endpoint],
            |row| row.get(0),
        )
        .optional()?
        .ok_or(DirectoryError::NotFound)?;
    let entry: DirectoryEntry = serde_json::from_slice(&blob)?;
    let key = entry.verify()?;
    authorization.verify(&key)?;
    Ok(entry.pubkey)
}

fn apply_drain(
    conn: &Connection,
    authorization: &DrainAuthorization,
    allow_exact_retry: bool,
) -> Result<()> {
    let sequence = sql_u64(authorization.sequence, "drain sequence")?;
    let drain_after = sql_u64(authorization.after, "drain time")?;
    let row: Option<(Vec<u8>, i64, i64)> = conn
        .query_row(
            "SELECT entry, mutation_sequence, ownership_proven
             FROM lofts WHERE endpoint = ?1",
            params![authorization.endpoint],
            |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
        )
        .optional()?;
    let Some((blob, stored_sequence, ownership_proven)) = row else {
        return Err(DirectoryError::NotFound);
    };
    let mut entry: DirectoryEntry = serde_json::from_slice(&blob)?;
    let key = entry.verify()?;
    authorization.verify(&key)?;
    if sequence < stored_sequence {
        return Err(DirectoryError::Replay);
    }
    if sequence == stored_sequence {
        if allow_exact_retry
            && matches!(entry.state, LoftState::Draining | LoftState::Removed)
            && entry.drain_after == Some(authorization.after)
            && entry.last_mutation_sequence == authorization.sequence
        {
            return Ok(());
        }
        return Err(DirectoryError::Replay);
    }
    if ownership_proven == 0 || matches!(entry.state, LoftState::Pending | LoftState::Removed) {
        return Err(DirectoryError::NotReady);
    }

    entry.state = LoftState::Draining;
    entry.drain_after = Some(authorization.after);
    entry.last_mutation_sequence = authorization.sequence;
    conn.execute(
        "UPDATE lofts
         SET entry = ?2, state = 'draining', drain_after = ?3, mutation_sequence = ?4
         WHERE endpoint = ?1",
        params![
            authorization.endpoint,
            serde_json::to_vec(&entry)?,
            drain_after,
            sequence,
        ],
    )?;
    Ok(())
}

fn add_is_exactly_applied(conn: &Connection, entry: &DirectoryEntry) -> Result<bool> {
    let sequence = sql_u64(entry.sequence, "entry sequence")?;
    let primary: Option<(Vec<u8>, i64)> = conn
        .query_row(
            "SELECT entry, mutation_sequence FROM lofts WHERE endpoint = ?1",
            params![entry.endpoint],
            |row| Ok((row.get(0)?, row.get(1)?)),
        )
        .optional()?;
    if let Some((blob, stored_sequence)) = primary {
        let stored: DirectoryEntry = serde_json::from_slice(&blob)?;
        if stored_sequence == sequence
            && stored.registry_addition()? == entry.registry_addition()?
        {
            return Ok(true);
        }
    }
    let candidate: Option<(Vec<u8>, i64)> = conn
        .query_row(
            "SELECT entry, mutation_sequence FROM pending_claims
             WHERE endpoint = ?1 AND loft_pubkey = ?2",
            params![entry.endpoint, entry.pubkey],
            |row| Ok((row.get(0)?, row.get(1)?)),
        )
        .optional()?;
    if let Some((blob, stored_sequence)) = candidate {
        let stored: DirectoryEntry = serde_json::from_slice(&blob)?;
        return Ok(stored_sequence == sequence
            && stored.registry_addition()? == entry.registry_addition()?);
    }
    Ok(false)
}

fn drain_is_exactly_applied(
    conn: &Connection,
    authorization: &DrainAuthorization,
    mutation: &RegistryDirectoryRemove,
) -> Result<bool> {
    let row: Option<(Vec<u8>, i64)> = conn
        .query_row(
            "SELECT entry, mutation_sequence FROM lofts WHERE endpoint = ?1",
            params![authorization.endpoint],
            |row| Ok((row.get(0)?, row.get(1)?)),
        )
        .optional()?;
    let Some((blob, stored_sequence)) = row else {
        return Ok(false);
    };
    let entry: DirectoryEntry = serde_json::from_slice(&blob)?;
    let key = entry.verify()?;
    authorization.verify(&key)?;
    Ok(
        stored_sequence == sql_u64(authorization.sequence, "drain sequence")?
            && matches!(entry.state, LoftState::Draining | LoftState::Removed)
            && entry.drain_after == Some(authorization.after)
            && entry.last_mutation_sequence == authorization.sequence
            && &authorization.registry_removal(entry.pubkey) == mutation,
    )
}

fn pending_mutation_from_row(conn: &Connection, row: &ReservationRow) -> Result<PendingMutation> {
    validate_reservation_row(row)?;
    let sequence = u64::try_from(row.mutation_sequence).map_err(|_| {
        DirectoryError::Malformed("reservation sequence is negative or too large".into())
    })?;
    match row.operation.as_str() {
        RESERVATION_ADD => {
            let entry: DirectoryEntry = serde_json::from_slice(&row.local_request)?;
            validate_submission(&entry)?;
            let mutation = entry.registry_addition()?;
            if entry.endpoint != row.endpoint
                || entry.pubkey != row.loft_pubkey
                || entry.sequence != sequence
                || bounded_mutation_bytes(&mutation)? != row.canonical_mutation
                || bounded_local_request(&entry)? != row.local_request
            {
                return Err(DirectoryError::Malformed(
                    "persisted add reservation disagrees with its authenticated payload".into(),
                ));
            }
            Ok(PendingMutation::Add { entry, mutation })
        }
        RESERVATION_DRAIN => {
            let authorization: DrainAuthorization = serde_json::from_slice(&row.local_request)?;
            let loft_pubkey = drain_loft_pubkey(conn, &authorization)?;
            let mutation = authorization.registry_removal(loft_pubkey.clone());
            if authorization.endpoint != row.endpoint
                || loft_pubkey != row.loft_pubkey
                || authorization.sequence != sequence
                || bounded_mutation_bytes(&mutation)? != row.canonical_mutation
                || bounded_local_request(&authorization)? != row.local_request
            {
                return Err(DirectoryError::Malformed(
                    "persisted drain reservation disagrees with its authenticated payload".into(),
                ));
            }
            Ok(PendingMutation::Drain {
                authorization,
                mutation,
            })
        }
        _ => Err(DirectoryError::Malformed(
            "persisted reservation operation is unsupported".into(),
        )),
    }
}

fn registry_checkpoint_from(conn: &Connection) -> Result<Option<PersistedRegistryCheckpoint>> {
    let encoded: Option<Vec<u8>> = conn
        .query_row(
            "SELECT value FROM directory_meta WHERE key = ?1",
            params![REGISTRY_CHECKPOINT],
            |row| row.get(0),
        )
        .optional()?;
    encoded
        .map(|value| {
            let checkpoint: PersistedRegistryCheckpoint = serde_json::from_slice(&value)?;
            validate_persisted_checkpoint(&checkpoint)?;
            Ok(checkpoint)
        })
        .transpose()
}

fn accept_registry_checkpoint_in(
    conn: &Connection,
    checkpoint: &PersistedRegistryCheckpoint,
) -> Result<()> {
    validate_persisted_checkpoint(checkpoint)?;
    let previous: Option<Vec<u8>> = conn
        .query_row(
            "SELECT value FROM directory_meta WHERE key = ?1",
            params![REGISTRY_CHECKPOINT],
            |row| row.get(0),
        )
        .optional()?;
    if let Some(previous) = previous {
        let previous: PersistedRegistryCheckpoint = serde_json::from_slice(&previous)?;
        validate_persisted_checkpoint(&previous)?;
        if previous.origin != checkpoint.origin
            || checkpoint.size < previous.size
            || (checkpoint.size == previous.size && checkpoint.root != previous.root)
        {
            return Err(DirectoryError::RegistryProof(
                "registry checkpoint rolled back or equivocated".into(),
            ));
        }
        if checkpoint.size == previous.size && checkpoint.witnessed_at < previous.witnessed_at {
            return Err(DirectoryError::RegistryProof(
                "registry witness timestamp rolled back".into(),
            ));
        }
    }
    conn.execute(
        "INSERT INTO directory_meta (key, value) VALUES (?1, ?2)
         ON CONFLICT(key) DO UPDATE SET value = excluded.value",
        params![REGISTRY_CHECKPOINT, serde_json::to_vec(checkpoint)?],
    )?;
    Ok(())
}

#[cfg(unix)]
fn canonical_database_path(path: &std::path::Path) -> Result<std::path::PathBuf> {
    if path.file_name().is_none() {
        return Err(DirectoryError::Malformed(
            "directory database path must name a file".into(),
        ));
    }
    if path.components().any(|component| {
        matches!(
            component,
            std::path::Component::CurDir | std::path::Component::ParentDir
        )
    }) {
        return Err(DirectoryError::Malformed(
            "directory database path must not contain dot components".into(),
        ));
    }
    if let Some(parent) = path
        .parent()
        .filter(|parent| !parent.as_os_str().is_empty())
    {
        match std::fs::symlink_metadata(parent) {
            Ok(metadata) if metadata.file_type().is_symlink() || !metadata.is_dir() => {
                return Err(DirectoryError::Malformed(
                    "directory database parent must be a directory, not a symbolic link".into(),
                ));
            }
            Ok(_) => {}
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
            Err(error) => return Err(error.into()),
        }
    }
    Ok(path.to_path_buf())
}

#[cfg(windows)]
fn canonical_database_path(path: &std::path::Path) -> Result<std::path::PathBuf> {
    if path.file_name().is_none() {
        return Err(DirectoryError::Malformed(
            "directory database path must name a file".into(),
        ));
    }
    // Windows custody must see every original parent component. Canonicalization would follow a
    // reparse point and erase the evidence that the handle-level checker is required to reject.
    Ok(if path.is_absolute() {
        path.to_path_buf()
    } else {
        std::env::current_dir()?.join(path)
    })
}

#[cfg(not(any(unix, windows)))]
fn canonical_database_path(path: &std::path::Path) -> Result<std::path::PathBuf> {
    if path.file_name().is_none() {
        return Err(DirectoryError::Malformed(
            "directory database path must name a file".into(),
        ));
    }
    Ok(path.to_path_buf())
}

fn has_column(conn: &Connection, table: &str, column: &str) -> Result<bool> {
    let mut statement = conn.prepare(&format!("PRAGMA table_info({table})"))?;
    let names = statement.query_map([], |row| row.get::<_, String>(1))?;
    for name in names {
        if name? == column {
            return Ok(true);
        }
    }
    Ok(false)
}

fn ensure_directory_schema_v3(conn: &Connection) -> Result<()> {
    conn.execute_batch(DIRECTORY_SCHEMA_V3_SQL)?;
    if !has_column(conn, "lofts", "mutation_sequence")? {
        conn.execute(
            "ALTER TABLE lofts ADD COLUMN mutation_sequence INTEGER NOT NULL DEFAULT 0",
            [],
        )?;
    }
    if !has_column(conn, "lofts", "clean_since")? {
        conn.execute("ALTER TABLE lofts ADD COLUMN clean_since INTEGER", [])?;
    }
    if !has_column(conn, "lofts", "next_probe_at")? {
        conn.execute(
            "ALTER TABLE lofts ADD COLUMN next_probe_at INTEGER NOT NULL DEFAULT 0",
            [],
        )?;
    }
    if !has_column(conn, "lofts", "ownership_proven")? {
        conn.execute(
            "ALTER TABLE lofts ADD COLUMN ownership_proven INTEGER NOT NULL DEFAULT 0
             CHECK (ownership_proven IN (0, 1))",
            [],
        )?;
        // Preserve every binding for which the old schema contains durable evidence of endpoint
        // control. Pending/removed rows with no successful probe remain releasable, which is the
        // state needed to repair pre-v2 squatting attempts.
        conn.execute(
            "UPDATE lofts SET ownership_proven = 1
             WHERE probes_ok > 0 OR state IN ('active', 'degraded', 'draining')",
            [],
        )?;
    }
    if !has_column(conn, "probes", "healthy")? {
        conn.execute("ALTER TABLE probes ADD COLUMN healthy INTEGER", [])?;
        backfill_probe_health(conn)?;
    }
    conn.execute(
        "CREATE INDEX IF NOT EXISTS lofts_by_probe_due
         ON lofts (state, next_probe_at, last_probe, endpoint)",
        [],
    )?;
    Ok(())
}

fn ensure_directory_schema_v4(conn: &Connection) -> Result<()> {
    conn.execute_batch(DIRECTORY_SCHEMA_V4_SQL)?;
    Ok(())
}

type DirectorySchemaObject = (String, String, String, String);

fn verify_directory_schema_shape(conn: &Connection, version: u32) -> Result<()> {
    let actual = directory_schema_snapshot(conn)?;
    if !expected_directory_schemas(version)?
        .iter()
        .any(|expected| expected == &actual)
    {
        return Err(DirectoryError::Malformed(format!(
            "directory database schema does not match declared version {version}; operator review is required"
        )));
    }
    Ok(())
}

/// Schema 3 can exist as a pristine database or as the exact v0.1 release shape plus additive
/// migrations. Both lineages are generated here from their canonical starting points; accepting a
/// bounded set of known shapes avoids weakening any constraint merely to tolerate `ALTER TABLE`.
fn expected_directory_schemas(version: u32) -> Result<Vec<Vec<DirectorySchemaObject>>> {
    debug_assert!(matches!(version, 3 | 4));
    let mut expected = Vec::with_capacity(2);
    for legacy in [false, true] {
        let mut reference = Connection::open_in_memory()?;
        if legacy {
            reference.execute_batch(include_str!("../tests/fixtures/v0_1_0_directory.sql"))?;
        }
        let transaction = reference.transaction_with_behavior(TransactionBehavior::Immediate)?;
        ensure_directory_schema_v3(&transaction)?;
        if version == 4 {
            ensure_directory_schema_v4(&transaction)?;
        }
        transaction.commit()?;
        let snapshot = directory_schema_snapshot(&reference)?;
        if !expected.contains(&snapshot) {
            expected.push(snapshot);
        }
    }
    Ok(expected)
}

fn directory_schema_snapshot(conn: &Connection) -> Result<Vec<DirectorySchemaObject>> {
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
            canonical_directory_schema_sql(&row.get::<_, String>(3)?),
        ))
    })?;
    rows.collect::<std::result::Result<Vec<_>, _>>()
        .map_err(Into::into)
}

/// Compare declared types, nullability, defaults, keys, uniqueness, checks, foreign keys, and
/// explicit indexes while tolerating whitespace/comments and additive column order.
fn canonical_directory_schema_sql(sql: &str) -> String {
    let uncommented = sql
        .lines()
        .map(|line| line.split_once("--").map_or(line, |(before, _)| before))
        .collect::<String>();
    let normalized = normalize_directory_schema_sql(&uncommented);
    if !normalized.starts_with("createtable") {
        return normalized;
    }
    let (Some(open), Some(close)) = (uncommented.find('('), uncommented.rfind(')')) else {
        return normalized;
    };
    if close <= open {
        return normalized;
    }
    let mut definitions = split_directory_schema_definitions(&uncommented[(open + 1)..close]);
    definitions.sort();
    format!(
        "{}({})",
        normalize_directory_schema_sql(&uncommented[..open]),
        definitions.join(",")
    )
}

fn split_directory_schema_definitions(body: &str) -> Vec<String> {
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
                definitions.push(normalize_directory_schema_sql(&body[start..offset]));
                start = offset + character.len_utf8();
            }
            _ => {}
        }
    }
    definitions.push(normalize_directory_schema_sql(&body[start..]));
    definitions
}

fn normalize_directory_schema_sql(sql: &str) -> String {
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

fn verify_directory_schema_v3(conn: &Connection) -> Result<()> {
    verify_directory_schema_shape(conn, 3)
}

fn verify_directory_schema(conn: &Connection) -> Result<()> {
    verify_directory_schema_shape(conn, 4)?;

    let rows: Vec<ReservationRow> = {
        let mut statement = conn.prepare(
            "SELECT reservation_id, endpoint, operation, loft_pubkey, mutation_sequence,
                    canonical_mutation, local_request, reserved_at, capacity_slot
             FROM directory_mutation_reservations",
        )?;
        let rows = statement
            .query_map([], reservation_row_from_sql)?
            .collect::<std::result::Result<Vec<_>, _>>()?;
        rows
    };
    if rows.len() > MAX_MUTATION_RESERVATIONS {
        return Err(DirectoryError::Malformed(
            "directory reservation table exceeds its fixed bound".into(),
        ));
    }
    for row in &rows {
        pending_mutation_from_row(conn, row)?;
    }
    Ok(())
}

fn backfill_probe_health(conn: &Connection) -> Result<()> {
    let rows: Vec<(i64, Vec<u8>)> = {
        let mut statement = conn.prepare("SELECT id, result FROM probes WHERE healthy IS NULL")?;
        let rows = statement.query_map([], |row| Ok((row.get(0)?, row.get(1)?)))?;
        rows.collect::<std::result::Result<Vec<_>, _>>()?
    };
    for (id, encoded) in rows {
        let probe: ProbeResult = serde_json::from_slice(&encoded)?;
        conn.execute(
            "UPDATE probes SET healthy = ?2 WHERE id = ?1",
            params![id, i64::from(probe.healthy())],
        )?;
    }
    Ok(())
}

fn fixed_32(bytes: &[u8], field: &str) -> Result<[u8; 32]> {
    bytes.try_into().map_err(|_| {
        DirectoryError::Malformed(format!("persisted {field} must contain exactly 32 bytes"))
    })
}

fn expire_pending(conn: &Connection, now: u64) -> Result<usize> {
    if now < PENDING_EXPIRE_SECS {
        return Ok(0);
    }
    let cutoff = sql_u64(now - PENDING_EXPIRE_SECS, "pending expiry cutoff")?;
    let expired: Vec<(String, Vec<u8>)> = {
        let mut statement = conn.prepare(
            "SELECT endpoint, entry FROM lofts
             WHERE state = 'pending' AND first_seen <= ?1
               AND NOT EXISTS (
                   SELECT 1 FROM directory_mutation_reservations reservation
                   WHERE reservation.endpoint = lofts.endpoint
               )",
        )?;
        let rows = statement.query_map(params![cutoff], |row| Ok((row.get(0)?, row.get(1)?)))?;
        rows.collect::<std::result::Result<Vec<_>, _>>()?
    };

    let removed_at = sql_u64(now, "pending expiry time")?;
    for (endpoint, encoded) in &expired {
        let mut entry: DirectoryEntry = serde_json::from_slice(encoded)?;
        entry.state = LoftState::Removed;
        conn.execute(
            "UPDATE lofts
             SET entry = ?2, state = 'removed', degraded_at = ?3, clean_since = NULL,
                 next_probe_at = ?4
             WHERE endpoint = ?1 AND state = 'pending'",
            params![endpoint, serde_json::to_vec(&entry)?, removed_at, i64::MAX,],
        )?;
        conn.execute(
            "DELETE FROM retention_canaries WHERE endpoint = ?1",
            params![endpoint],
        )?;
    }
    Ok(expired.len())
}

fn expire_pending_claims(conn: &Connection, now: u64) -> Result<usize> {
    if now < PENDING_EXPIRE_SECS {
        return Ok(0);
    }
    let cutoff = sql_u64(now - PENDING_EXPIRE_SECS, "candidate expiry cutoff")?;
    Ok(conn.execute(
        "DELETE FROM pending_claims
         WHERE first_seen <= ?1
           AND NOT EXISTS (
               SELECT 1 FROM directory_mutation_reservations reservation
               WHERE reservation.endpoint = pending_claims.endpoint
           )",
        params![cutoff],
    )?)
}

fn ensure_pending_capacity(conn: &Connection) -> Result<()> {
    let pending = pending_load(conn)?;
    if pending >= MAX_PENDING_ENTRIES as i64 {
        return Err(DirectoryError::Unavailable);
    }
    Ok(())
}

fn validate_submission(entry: &DirectoryEntry) -> Result<()> {
    if entry.endpoint.len() > 2_048 {
        return Err(DirectoryError::Malformed("endpoint is too long".into()));
    }
    let endpoint = reqwest::Url::parse(&entry.endpoint)
        .map_err(|_| DirectoryError::Malformed("endpoint is not a valid URL".into()))?;
    let host = endpoint.host_str();
    let address = host.and_then(|value| {
        value
            .trim_start_matches('[')
            .trim_end_matches(']')
            .parse::<IpAddr>()
            .ok()
    });
    let allowed_origin = match endpoint.scheme() {
        "http" => address.is_some_and(|value| value.is_loopback()),
        "https" => match address {
            Some(value) => is_public_network_address(value),
            None => host.is_some_and(|domain| !is_localhost_name(domain)),
        },
        _ => false,
    };
    if !allowed_origin
        || endpoint.cannot_be_a_base()
        || endpoint.host_str().is_none()
        || !endpoint.username().is_empty()
        || endpoint.password().is_some()
        || endpoint.port() == Some(0)
        || endpoint.query().is_some()
        || endpoint.fragment().is_some()
        || !matches!(endpoint.path(), "" | "/")
    {
        return Err(DirectoryError::Malformed(
            "endpoint must be a public HTTPS origin or an exact numeric loopback HTTP origin"
                .into(),
        ));
    }
    if entry
        .operator
        .as_ref()
        .is_some_and(|value| value.len() > 256)
    {
        return Err(DirectoryError::Malformed(
            "operator identity is too long".into(),
        ));
    }
    if entry.capacity_gb == 0 || entry.retention_days == 0 {
        return Err(DirectoryError::Malformed(
            "capacity and retention must be non-zero".into(),
        ));
    }
    if entry.policy.max_event_bytes == 0 || entry.policy.max_event_bytes > 2 * 1024 * 1024 {
        return Err(DirectoryError::Malformed(
            "max_event_bytes is outside the supported range".into(),
        ));
    }
    if !entry.utilization.is_finite() || !(0.0..=1.0).contains(&entry.utilization) {
        return Err(DirectoryError::Malformed(
            "utilization must be a finite value between zero and one".into(),
        ));
    }
    Ok(())
}

fn validate_persisted_checkpoint(checkpoint: &PersistedRegistryCheckpoint) -> Result<()> {
    if checkpoint.version != 1
        || checkpoint.origin.is_empty()
        || checkpoint.origin.len() > 256
        || checkpoint
            .origin
            .bytes()
            .any(|byte| byte.is_ascii_control() || byte.is_ascii_whitespace())
        || checkpoint.note.is_empty()
        || checkpoint.note.len() > MAX_CHECKPOINT_NOTE_BYTES
        || checkpoint.witnessed_at == 0
        || (checkpoint.size == 0 && checkpoint.root != pigeonpost_registry::log::empty_root())
    {
        return Err(DirectoryError::RegistryProof(
            "persisted registry checkpoint is malformed".into(),
        ));
    }
    Ok(())
}

fn sql_u64(value: u64, field: &str) -> Result<i64> {
    i64::try_from(value).map_err(|_| DirectoryError::Malformed(format!("{field} is too large")))
}

fn parse_state(name: &str) -> LoftState {
    match name {
        "active" => LoftState::Active,
        "degraded" => LoftState::Degraded,
        "draining" => LoftState::Draining,
        "removed" => LoftState::Removed,
        _ => LoftState::Pending,
    }
}

fn state_name(state: LoftState) -> &'static str {
    match state {
        LoftState::Pending => "pending",
        LoftState::Active => "active",
        LoftState::Degraded => "degraded",
        LoftState::Draining => "draining",
        LoftState::Removed => "removed",
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::entry::LoftPolicy;
    use ed25519_dalek::SigningKey;

    fn create_private_database(path: &std::path::Path) -> (PrivateDatabase, Connection) {
        let (custody, created) = PrivateDatabase::open_or_create(path).unwrap();
        assert!(created);
        let connection = Connection::open(custody.sqlite_path()).unwrap();
        #[cfg(any(unix, windows))]
        custody.verify_main_named().unwrap();
        #[cfg(not(any(unix, windows)))]
        custody.verify_named().unwrap();
        (custody, connection)
    }

    fn private_database_path(
        root: &std::path::Path,
        name: impl AsRef<std::path::Path>,
    ) -> std::path::PathBuf {
        let parent = root.join("private");
        #[cfg(not(windows))]
        {
            std::fs::create_dir_all(&parent).unwrap();
            #[cfg(unix)]
            {
                use std::os::unix::fs::PermissionsExt;
                std::fs::set_permissions(&parent, std::fs::Permissions::from_mode(0o700)).unwrap();
            }
        }
        parent.join(name)
    }

    #[cfg(unix)]
    #[test]
    fn database_open_rejects_intermediate_symlink_and_mutable_ancestor_without_creation() {
        use std::os::unix::fs::{symlink, PermissionsExt};

        let root = tempfile::tempdir().unwrap();
        let outside = root.path().join("outside");
        std::fs::create_dir(&outside).unwrap();
        std::fs::set_permissions(&outside, std::fs::Permissions::from_mode(0o700)).unwrap();
        let linked = root.path().join("linked");
        symlink(&outside, &linked).unwrap();
        let through_link = linked.join("new-private/directory.db");
        assert!(Directory::open(through_link.to_str().unwrap()).is_err());
        assert!(!outside.join("new-private").exists());

        let mutable = root.path().join("mutable");
        std::fs::create_dir(&mutable).unwrap();
        std::fs::set_permissions(&mutable, std::fs::Permissions::from_mode(0o770)).unwrap();
        let through_mutable = mutable.join("new-private/directory.db");
        assert!(Directory::open(through_mutable.to_str().unwrap()).is_err());
        assert!(!mutable.join("new-private").exists());
    }

    #[cfg(unix)]
    #[test]
    fn retained_database_custody_detects_main_wal_shm_and_parent_replacement() {
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
            let path = root.path().join("private/directory.db");
            let directory = Directory::open(path.to_str().unwrap()).unwrap();
            directory.verify_public_storage_ready().unwrap();
            let mut target = path.as_os_str().to_os_string();
            target.push(suffix);
            replace(&std::path::PathBuf::from(target));
            assert!(
                directory.verify_public_storage_ready().is_err(),
                "replaced {suffix}"
            );
        }

        let root = tempfile::tempdir().unwrap();
        let parent = root.path().join("private");
        let path = parent.join("directory.db");
        let directory = Directory::open(path.to_str().unwrap()).unwrap();
        let moved_parent = root.path().join("private.original");
        std::fs::rename(&parent, &moved_parent).unwrap();
        std::fs::create_dir(&parent).unwrap();
        std::fs::set_permissions(&parent, std::fs::Permissions::from_mode(0o700)).unwrap();
        std::fs::write(&path, []).unwrap();
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o600)).unwrap();
        assert!(directory.verify_public_storage_ready().is_err());
    }

    #[cfg(windows)]
    fn windows_database_sidecar(path: &std::path::Path, suffix: &str) -> std::path::PathBuf {
        let mut value = path.as_os_str().to_os_string();
        value.push(suffix);
        value.into()
    }

    #[cfg(windows)]
    #[test]
    fn windows_rejects_unsafe_sqlite_sidecars_before_main_creation() {
        for suffix in ["-wal", "-shm", "-journal"] {
            let root = tempfile::tempdir().unwrap();
            let parent = root.path().join("private");
            drop(crate::private_store::PrivateDirectory::open_or_create(&parent).unwrap());
            let path = parent.join("directory.db");
            let sidecar = windows_database_sidecar(&path, suffix);
            std::fs::write(&sidecar, b"unprotected-sidecar-sentinel").unwrap();

            assert!(Directory::open(path.to_str().unwrap()).is_err());
            assert!(
                !path.exists(),
                "unsafe {suffix} reached SQLite and created the main database"
            );
            assert_eq!(
                std::fs::read(&sidecar).unwrap(),
                b"unprotected-sidecar-sentinel",
                "unsafe {suffix} was mutated before rejection"
            );
        }
    }

    #[cfg(windows)]
    #[test]
    fn windows_rejects_hardlinked_and_reparse_sqlite_sidecars_without_effects() {
        use std::io::Write;

        let root = tempfile::tempdir().unwrap();
        let parent = root.path().join("private");
        drop(crate::private_store::PrivateDirectory::open_or_create(&parent).unwrap());
        let path = parent.join("directory.db");
        let source = parent.join("source.bin");
        let (source_file, _) = crate::private_store::PrivateFile::open_or_create(&source).unwrap();
        let mut descriptor = source_file.descriptor();
        descriptor.write_all(b"safe-source").unwrap();
        descriptor.sync_all().unwrap();
        source_file.verify_named().unwrap();
        drop(source_file);
        let wal = windows_database_sidecar(&path, "-wal");
        std::fs::hard_link(&source, &wal).unwrap();
        assert!(Directory::open(path.to_str().unwrap()).is_err());
        assert!(!path.exists());
        assert_eq!(std::fs::read(&source).unwrap(), b"safe-source");

        std::fs::remove_file(&wal).unwrap();
        let target = parent.join("target.bin");
        std::fs::write(&target, b"reparse-target").unwrap();
        let shm = windows_database_sidecar(&path, "-shm");
        if std::os::windows::fs::symlink_file(&target, &shm).is_ok() {
            assert!(Directory::open(path.to_str().unwrap()).is_err());
            assert!(!path.exists());
            assert_eq!(std::fs::read(&target).unwrap(), b"reparse-target");
        }
    }

    #[cfg(windows)]
    #[test]
    fn windows_retains_main_wal_shm_and_parent_until_connection_drop() {
        let root = tempfile::tempdir().unwrap();
        let parent = root.path().join("private");
        let path = parent.join("directory.db");
        let directory = Directory::open(path.to_str().unwrap()).unwrap();
        directory.verify_public_storage_ready().unwrap();

        for suffix in ["", "-wal", "-shm"] {
            let source = windows_database_sidecar(&path, suffix);
            let destination = windows_database_sidecar(&source, ".moved");
            assert!(
                std::fs::rename(&source, &destination).is_err(),
                "{suffix} remained replaceable while Directory was alive"
            );
        }
        assert!(std::fs::rename(&parent, root.path().join("private.moved")).is_err());
        directory.verify_public_storage_ready().unwrap();
    }

    fn create_schema_four(path: &std::path::Path) {
        drop(Directory::open(path.to_str().unwrap()).unwrap());
    }

    fn assert_schema_refused_without_mutation(path: &std::path::Path, expected_version: i64) {
        let before_connection = Connection::open(path).unwrap();
        let before = directory_schema_snapshot(&before_connection).unwrap();
        drop(before_connection);
        assert!(matches!(
            Directory::open(path.to_str().unwrap()),
            Err(DirectoryError::Malformed(_))
        ));
        let after_connection = Connection::open(path).unwrap();
        let after = directory_schema_snapshot(&after_connection).unwrap();
        assert_eq!(after, before, "a refused open changed sqlite_schema");
        let version: i64 = after_connection
            .pragma_query_value(None, "user_version", |row| row.get(0))
            .unwrap();
        assert_eq!(version, expected_version);
    }

    fn entry(seed: u8, endpoint: &str) -> DirectoryEntry {
        DirectoryEntry::signed(
            &SigningKey::from_bytes(&[seed; 32]),
            endpoint,
            None,
            100,
            30,
            LoftPolicy {
                open: true,
                pow_floor: 0,
                max_event_bytes: 65536,
            },
            0.0,
        )
    }

    fn probe(endpoint: &str, healthy: bool) -> ProbeResult {
        ProbeResult {
            endpoint: endpoint.to_string(),
            at: 0,
            reachable: healthy,
            stored_and_returned: healthy,
            utilization: 0.1,
            retention_age_secs: None,
            retention_ok: None,
            detail: None,
        }
    }

    #[test]
    fn a_submission_must_be_signed_by_the_loft_it_names() {
        let directory = Directory::in_memory().unwrap();
        let mut forged = entry(1, "https://a.example");
        forged.capacity_gb = 999_999;

        assert!(matches!(
            directory.submit(forged, 100),
            Err(DirectoryError::BadSignature)
        ));
    }

    #[test]
    fn a_pre_transparency_database_is_not_safe_to_serve() {
        let directory = Directory::in_memory().unwrap();
        assert!(directory.verify_registry_logging_ready().is_ok());
        directory
            .submit(entry(1, "https://a.example"), 100)
            .unwrap();
        assert!(matches!(
            directory.verify_registry_logging_ready(),
            Err(DirectoryError::RegistryProof(_))
        ));
    }

    #[test]
    fn public_storage_gate_rejects_in_memory_and_accepts_held_persistent_storage() {
        let memory = Directory::in_memory().unwrap();
        assert!(matches!(
            memory.verify_public_storage_ready(),
            Err(DirectoryError::Malformed(_))
        ));

        let temp = tempfile::tempdir().unwrap();
        let path = private_database_path(temp.path(), "directory.db");
        let persistent = Directory::open(path.to_str().unwrap()).unwrap();
        persistent.verify_public_storage_ready().unwrap();
    }

    #[cfg(unix)]
    #[test]
    fn public_storage_gate_detects_a_replaced_database_name() {
        use std::os::unix::fs::PermissionsExt;

        let temp = tempfile::tempdir().unwrap();
        let path = temp.path().join("directory.db");
        let displaced = temp.path().join("directory.displaced.db");
        let directory = Directory::open(path.to_str().unwrap()).unwrap();
        directory.verify_public_storage_ready().unwrap();

        std::fs::rename(&path, &displaced).unwrap();
        std::fs::write(&path, []).unwrap();
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o600)).unwrap();

        assert!(matches!(
            directory.verify_public_storage_ready(),
            Err(DirectoryError::Io(_))
        ));
    }

    #[test]
    fn submissions_require_public_https_or_exact_numeric_loopback_http_origins() {
        let directory = Directory::in_memory().unwrap();
        for endpoint in [
            "https://user:pass@loft.example",
            "https://loft.example/internal",
            "https://loft.example?target=internal",
            "https://loft.example/#fragment",
            "https://127.0.0.1",
            "https://10.0.0.1",
            "https://localhost.",
            "https://api.localhost",
            "https://API.LOCALHOST.",
            "https://loft.example:0",
            "wss://loft.example",
            "ws://loft.example",
            "http://loft.example",
            "http://localhost",
            "http://localhost.",
            "file:///etc/passwd",
        ] {
            assert!(
                matches!(
                    directory.submit(entry(1, endpoint), 100),
                    Err(DirectoryError::Malformed(_))
                ),
                "accepted forbidden endpoint {endpoint}"
            );
        }
        for endpoint in [
            "https://loft.example",
            "https://8.8.8.8:443",
            "http://127.0.0.1:8080",
            "http://[::1]:8081",
        ] {
            directory.submit(entry(1, endpoint), 100).unwrap();
        }
    }

    #[test]
    fn non_finite_observations_are_rejected_before_storage() {
        let directory = Directory::in_memory().unwrap();
        let mut invalid = entry(1, "https://a.example");
        invalid.utilization = f64::NAN;
        assert!(matches!(
            directory.submit(invalid, 100),
            Err(DirectoryError::Malformed(_))
        ));
    }

    #[test]
    fn a_new_loft_starts_pending_and_is_not_selectable() {
        let directory = Directory::in_memory().unwrap();
        directory
            .submit(entry(1, "https://a.example"), 100)
            .unwrap();

        let stored = directory.entry("https://a.example").unwrap();
        assert_eq!(stored.state, LoftState::Pending);
        assert_eq!(stored.weight(), 0.0);
        assert!(directory.entries().unwrap().is_empty());
    }

    #[test]
    fn a_healthy_loft_is_promoted_only_after_probing_clean_long_enough() {
        let directory = Directory::in_memory().unwrap();
        directory.submit(entry(1, "https://a.example"), 0).unwrap();

        // An hour in: still pending.
        let state = directory
            .record_probe(&probe("https://a.example", true), 3_600)
            .unwrap();
        assert_eq!(state, LoftState::Pending);

        // The 24-hour interval begins at the first successful probe, not at submission.
        let state = directory
            .record_probe(
                &probe("https://a.example", true),
                3_600 + PROMOTE_AFTER_SECS,
            )
            .unwrap();
        assert_eq!(state, LoftState::Active);
    }

    #[test]
    fn a_failed_probation_probe_restarts_the_full_clean_window() {
        let directory = Directory::in_memory().unwrap();
        directory.submit(entry(1, "https://a.example"), 0).unwrap();
        directory
            .record_probe(&probe("https://a.example", true), 100)
            .unwrap();
        directory
            .record_probe(&probe("https://a.example", false), 10_000)
            .unwrap();
        assert_eq!(
            directory
                .record_probe(&probe("https://a.example", true), 100 + PROMOTE_AFTER_SECS,)
                .unwrap(),
            LoftState::Pending
        );
        assert_eq!(
            directory
                .record_probe(
                    &probe("https://a.example", true),
                    100 + 2 * PROMOTE_AFTER_SECS,
                )
                .unwrap(),
            LoftState::Active
        );
    }

    #[test]
    fn stale_pending_entries_expire_and_stop_consuming_probe_work() {
        let directory = Directory::in_memory().unwrap();
        directory.submit(entry(1, "https://a.example"), 1).unwrap();

        assert!(directory
            .claim_probe_candidates(PENDING_EXPIRE_SECS + 2, 10)
            .unwrap()
            .is_empty());
        assert_eq!(
            directory.entry("https://a.example").unwrap().state,
            LoftState::Removed
        );
        assert!(directory.entries().unwrap().is_empty());
    }

    #[test]
    fn a_live_owner_can_win_immediately_over_an_unproven_attacker_claim() {
        let directory = Directory::in_memory().unwrap();
        directory.submit(entry(1, "https://a.example"), 1).unwrap();
        directory
            .record_probe(&probe("https://a.example", false), 2)
            .unwrap();

        let owner = entry(2, "https://a.example");
        let owner_pubkey = owner.pubkey.clone();
        directory.submit(owner, 3).unwrap();
        assert_eq!(
            directory
                .lock()
                .query_row("SELECT COUNT(*) FROM pending_claims", [], |row| row
                    .get::<_, i64>(0))
                .unwrap(),
            1
        );
        let leased = directory.claim_probe_candidates(3, 10).unwrap();
        assert_eq!(leased.len(), 1);
        assert_eq!(leased[0].pubkey, owner_pubkey);
        assert_eq!(
            directory
                .record_claim_probe_with_retention(
                    &probe("https://a.example", true),
                    4,
                    None,
                    &owner_pubkey,
                    1,
                )
                .unwrap(),
            Some(LoftState::Pending)
        );

        let replacement = directory.entry("https://a.example").unwrap();
        assert_eq!(replacement.state, LoftState::Pending);
        assert_eq!(replacement.sequence, 1);
        assert_eq!(replacement.health.uptime_30d, 1.0);
        assert_eq!(replacement.health.probe_fail_streak, 0);
        assert_eq!(replacement.health.last_probe, 4);
        assert_eq!(replacement.utilization, 0.1);
        assert_eq!(
            replacement.pubkey,
            crate::entry::hex(SigningKey::from_bytes(&[2; 32]).verifying_key().as_bytes())
        );
        let conn = directory.lock();
        let (ownership_proven, probe_count): (i64, i64) = (
            conn.query_row(
                "SELECT ownership_proven FROM lofts WHERE endpoint = ?1",
                params!["https://a.example"],
                |row| row.get(0),
            )
            .unwrap(),
            conn.query_row(
                "SELECT COUNT(*) FROM probes WHERE endpoint = ?1",
                params!["https://a.example"],
                |row| row.get(0),
            )
            .unwrap(),
        );
        assert_eq!(ownership_proven, 1);
        assert_eq!(probe_count, 1, "only the winning key's probe is retained");
        drop(conn);
        assert_eq!(
            directory
                .record_claim_probe_with_retention(
                    &probe("https://a.example", true),
                    4 + PROMOTE_AFTER_SECS,
                    None,
                    &owner_pubkey,
                    1,
                )
                .unwrap(),
            Some(LoftState::Active)
        );
    }

    #[test]
    fn alternating_hostile_claims_cannot_reset_probation_or_evict_the_proven_winner() {
        let directory = Directory::in_memory().unwrap();
        let endpoint = "https://a.example";
        directory.submit(entry(1, endpoint), 1).unwrap();
        let owner = entry(2, endpoint);
        let owner_pubkey = owner.pubkey.clone();
        directory.submit(owner, 2).unwrap();
        directory.submit(entry(3, endpoint), 3).unwrap();
        directory.submit(entry(4, endpoint), 4).unwrap();

        let attacker_key = SigningKey::from_bytes(&[1; 32]);
        directory
            .submit(
                DirectoryEntry::signed_with_sequence(
                    &attacker_key,
                    endpoint,
                    None,
                    101,
                    30,
                    LoftPolicy {
                        open: true,
                        pow_floor: 0,
                        max_event_bytes: 65_536,
                    },
                    0.0,
                    2,
                ),
                5,
            )
            .unwrap();
        let first_seen: i64 = directory
            .lock()
            .query_row(
                "SELECT first_seen FROM lofts WHERE endpoint = ?1",
                params![endpoint],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(
            first_seen, 1,
            "hostile updates cannot restart a claim clock"
        );

        directory
            .record_claim_probe_with_retention(&probe(endpoint, true), 10, None, &owner_pubkey, 1)
            .unwrap();
        for seed in [3u8, 4, 5, 6] {
            assert!(matches!(
                directory.submit(entry(seed, endpoint), 11 + u64::from(seed)),
                Err(DirectoryError::KeyMismatch)
            ));
        }
        let stale_attacker =
            crate::entry::hex(SigningKey::from_bytes(&[3; 32]).verifying_key().as_bytes());
        assert_eq!(
            directory
                .record_claim_probe_with_retention(
                    &probe(endpoint, true),
                    20,
                    None,
                    &stale_attacker,
                    1,
                )
                .unwrap(),
            None
        );
        assert_eq!(directory.entry(endpoint).unwrap().pubkey, owner_pubkey);
        assert_eq!(
            directory
                .record_probe(&probe(endpoint, true), 10 + PROMOTE_AFTER_SECS)
                .unwrap(),
            LoftState::Active
        );
    }

    #[test]
    fn a_successfully_probed_binding_cannot_be_replaced_after_removal() {
        let directory = Directory::in_memory().unwrap();
        directory.submit(entry(1, "https://a.example"), 0).unwrap();
        directory
            .record_probe(&probe("https://a.example", true), 100)
            .unwrap();
        directory
            .set_state("https://a.example", LoftState::Removed)
            .unwrap();

        assert!(matches!(
            directory.submit(entry(2, "https://a.example"), 101),
            Err(DirectoryError::KeyMismatch)
        ));
        assert_eq!(
            directory.entry("https://a.example").unwrap().pubkey,
            crate::entry::hex(SigningKey::from_bytes(&[1; 32]).verifying_key().as_bytes())
        );
    }

    #[test]
    fn public_snapshot_never_includes_more_than_its_fixed_capacity() {
        let directory = Directory::in_memory().unwrap();
        for index in 0..=MAX_PUBLIC_ENTRIES {
            let endpoint = format!("https://loft-{index}.example");
            directory.submit(entry(1, &endpoint), 1).unwrap();
            if index < MAX_PUBLIC_ENTRIES {
                directory.set_state(&endpoint, LoftState::Active).unwrap();
            }
        }

        let snapshot = directory.entries().unwrap();
        assert_eq!(snapshot.len(), MAX_PUBLIC_ENTRIES);
        assert!(snapshot
            .iter()
            .all(|entry| entry.state != LoftState::Pending));
    }

    #[test]
    fn three_failures_degrade_a_loft() {
        let directory = Directory::in_memory().unwrap();
        directory.submit(entry(1, "https://a.example"), 0).unwrap();
        directory
            .set_state("https://a.example", LoftState::Active)
            .unwrap();

        let mut now = PROMOTE_AFTER_SECS;
        for _ in 0..DEGRADE_AFTER_FAILURES - 1 {
            now += 300;
            assert_eq!(
                directory
                    .record_probe(&probe("https://a.example", false), now)
                    .unwrap(),
                LoftState::Active,
                "one bad probe is a blip, not a verdict"
            );
        }

        now += 300;
        assert_eq!(
            directory
                .record_probe(&probe("https://a.example", false), now)
                .unwrap(),
            LoftState::Degraded
        );
    }

    #[test]
    fn a_recovered_loft_becomes_active_again() {
        let directory = Directory::in_memory().unwrap();
        directory.submit(entry(1, "https://a.example"), 0).unwrap();
        directory
            .set_state("https://a.example", LoftState::Degraded)
            .unwrap();

        assert_eq!(
            directory
                .record_probe(&probe("https://a.example", true), 10_000)
                .unwrap(),
            LoftState::Active
        );
    }

    #[test]
    fn a_degraded_loft_is_removed_after_long_enough() {
        let directory = Directory::in_memory().unwrap();
        directory.submit(entry(1, "https://a.example"), 0).unwrap();
        directory
            .set_state("https://a.example", LoftState::Active)
            .unwrap();

        let mut now = PROMOTE_AFTER_SECS;
        for _ in 0..DEGRADE_AFTER_FAILURES {
            now += 300;
            directory
                .record_probe(&probe("https://a.example", false), now)
                .unwrap();
        }

        let later = now + REMOVE_AFTER_SECS + 1;
        assert_eq!(
            directory
                .record_probe(&probe("https://a.example", false), later)
                .unwrap(),
            LoftState::Removed
        );
    }

    #[test]
    fn draining_survives_probing() {
        // A graceful exit is the operator's decision; a healthy probe must not undo it.
        let directory = Directory::in_memory().unwrap();
        directory.submit(entry(1, "https://a.example"), 0).unwrap();
        directory
            .set_state("https://a.example", LoftState::Active)
            .unwrap();
        let key = SigningKey::from_bytes(&[1; 32]);
        directory
            .drain(&DrainAuthorization::signed(
                &key,
                "https://a.example",
                2_000_000,
                2,
            ))
            .unwrap();

        assert_eq!(
            directory
                .record_probe(&probe("https://a.example", true), 10_000)
                .unwrap(),
            LoftState::Draining
        );
        assert_eq!(
            directory
                .record_probe(&probe("https://a.example", true), 2_000_001)
                .unwrap(),
            LoftState::Removed
        );
    }

    #[test]
    fn resubmitting_cannot_reset_a_degraded_loft_to_pending() {
        // Otherwise a failing node could dodge its own history by re-submitting.
        let directory = Directory::in_memory().unwrap();
        directory.submit(entry(1, "https://a.example"), 0).unwrap();
        directory
            .set_state("https://a.example", LoftState::Degraded)
            .unwrap();

        let key = SigningKey::from_bytes(&[1; 32]);
        let updated = DirectoryEntry::signed_with_sequence(
            &key,
            "https://a.example",
            None,
            100,
            30,
            LoftPolicy {
                open: true,
                pow_floor: 0,
                max_event_bytes: 65536,
            },
            0.0,
            2,
        );
        directory.submit(updated, 5_000).unwrap();
        assert_eq!(
            directory.entry("https://a.example").unwrap().state,
            LoftState::Degraded
        );
    }

    #[test]
    fn a_removed_loft_may_reenroll_but_must_complete_probation_again() {
        let directory = Directory::in_memory().unwrap();
        directory.submit(entry(1, "https://a.example"), 0).unwrap();
        directory
            .set_state("https://a.example", LoftState::Removed)
            .unwrap();
        let key = SigningKey::from_bytes(&[1; 32]);
        directory
            .submit(
                DirectoryEntry::signed_with_sequence(
                    &key,
                    "https://a.example",
                    None,
                    100,
                    30,
                    LoftPolicy {
                        open: true,
                        pow_floor: 0,
                        max_event_bytes: 65_536,
                    },
                    0.0,
                    2,
                ),
                1_000,
            )
            .unwrap();
        assert_eq!(
            directory.entry("https://a.example").unwrap().state,
            LoftState::Pending
        );
        assert!(directory.entries().unwrap().is_empty());
        directory
            .record_probe(&probe("https://a.example", true), 1_100)
            .unwrap();
        assert_eq!(
            directory
                .record_probe(
                    &probe("https://a.example", true),
                    1_100 + PROMOTE_AFTER_SECS,
                )
                .unwrap(),
            LoftState::Active
        );
    }

    #[test]
    fn probe_history_is_published() {
        let directory = Directory::in_memory().unwrap();
        directory.submit(entry(1, "https://a.example"), 0).unwrap();
        for i in 0..5 {
            directory
                .record_probe(&probe("https://a.example", i % 2 == 0), 1_000 + i)
                .unwrap();
        }

        let probes = directory.probes("https://a.example", 10).unwrap();
        assert_eq!(
            probes.len(),
            5,
            "weights must be recomputable from public data"
        );
    }

    #[test]
    fn uptime_reflects_the_measured_history() {
        let directory = Directory::in_memory().unwrap();
        directory.submit(entry(1, "https://a.example"), 0).unwrap();
        directory
            .set_state("https://a.example", LoftState::Active)
            .unwrap();

        for i in 0..10 {
            directory
                .record_probe(&probe("https://a.example", i < 8), 1_000 + i)
                .unwrap();
        }

        let entry = directory.entry("https://a.example").unwrap();
        assert!((entry.health.uptime_30d - 0.8).abs() < 0.001);
    }

    #[test]
    fn uptime_discards_measurements_outside_the_exact_thirty_day_window() {
        let directory = Directory::in_memory().unwrap();
        directory.submit(entry(1, "https://a.example"), 0).unwrap();
        directory
            .set_state("https://a.example", LoftState::Active)
            .unwrap();
        directory
            .record_probe(&probe("https://a.example", false), 1)
            .unwrap();
        directory
            .record_probe(&probe("https://a.example", true), PROBE_RETENTION_SECS + 2)
            .unwrap();

        let entry = directory.entry("https://a.example").unwrap();
        assert_eq!(entry.health.uptime_30d, 1.0);
        assert_eq!(directory.probes("https://a.example", 10).unwrap().len(), 1);
    }

    #[test]
    fn measurement_cursor_pages_cover_the_complete_rolling_window() {
        let directory = Directory::in_memory().unwrap();
        directory.submit(entry(1, "https://a.example"), 0).unwrap();
        directory
            .set_state("https://a.example", LoftState::Active)
            .unwrap();
        for at in 1..=501 {
            directory
                .record_probe(&probe("https://a.example", true), at)
                .unwrap();
        }

        let mut cursor = None;
        let mut count = 0;
        loop {
            let page = directory
                .probe_page("https://a.example", cursor, 200, 501)
                .unwrap();
            count += page.probes.len();
            if !page.more {
                assert!(page.next_cursor.is_none());
                break;
            }
            cursor = page.next_cursor;
        }
        assert_eq!(count, 501);
    }

    #[test]
    fn due_probe_claims_lease_entries_in_fair_bounded_batches() {
        let directory = Directory::in_memory().unwrap();
        for index in 0..25 {
            directory
                .submit(entry(1, &format!("https://loft-{index}.example")), 1)
                .unwrap();
        }
        let first = directory.claim_probe_candidates(1, 10).unwrap();
        let second = directory.claim_probe_candidates(1, 10).unwrap();
        assert_eq!(first.len(), 10);
        assert_eq!(second.len(), 10);
        assert!(first
            .iter()
            .all(|entry| second.iter().all(|other| entry.endpoint != other.endpoint)));
    }

    #[test]
    fn retention_canaries_are_persisted_and_checked_daily_until_the_boundary() {
        let directory = Directory::in_memory().unwrap();
        directory.submit(entry(1, "https://a.example"), 0).unwrap();
        assert!(matches!(
            directory
                .retention_work("https://a.example", 30, 100)
                .unwrap(),
            Some(RetentionWork::Create)
        ));
        directory
            .record_probe_with_retention(
                &probe("https://a.example", true),
                100,
                Some(RetentionUpdate::Created {
                    recipient_seed: [7; 32],
                    event_id: [8; 32],
                    published_at: 100,
                }),
            )
            .unwrap();
        assert!(directory
            .retention_work("https://a.example", 30, 100 + 60 * 60)
            .unwrap()
            .is_none());
        let Some(RetentionWork::Check(canary)) = directory
            .retention_work("https://a.example", 30, 100 + RETENTION_CHECK_INTERVAL_SECS)
            .unwrap()
        else {
            panic!("the aged canary must be checked daily");
        };
        assert_eq!(canary.recipient_seed, [7; 32]);
        assert_eq!(canary.event_id, [8; 32]);
        assert_eq!(
            canary.target_age_secs,
            30 * 24 * 60 * 60 - RETENTION_CHECK_MARGIN_SECS
        );
    }

    #[test]
    fn a_missing_aged_canary_is_retried_until_it_degrades_the_loft() {
        let directory = Directory::in_memory().unwrap();
        directory.submit(entry(1, "https://a.example"), 0).unwrap();
        directory
            .set_state("https://a.example", LoftState::Active)
            .unwrap();
        directory
            .record_probe_with_retention(
                &probe("https://a.example", true),
                100,
                Some(RetentionUpdate::Created {
                    recipient_seed: [7; 32],
                    event_id: [8; 32],
                    published_at: 100,
                }),
            )
            .unwrap();
        let due = 100 + RETENTION_CHECK_INTERVAL_SECS;
        let mut missing = probe("https://a.example", true);
        missing.retention_age_secs = Some(RETENTION_CHECK_INTERVAL_SECS);
        missing.retention_ok = Some(false);
        for attempt in 0..DEGRADE_AFTER_FAILURES {
            let now = due + u64::from(attempt) * PROBE_INTERVAL_SECS;
            assert!(matches!(
                directory
                    .retention_work("https://a.example", 30, now)
                    .unwrap(),
                Some(RetentionWork::Check(_))
            ));
            let state = directory
                .record_probe_with_retention(&missing, now, None)
                .unwrap();
            if attempt + 1 == DEGRADE_AFTER_FAILURES {
                assert_eq!(state, LoftState::Degraded);
            }
        }
    }

    #[test]
    fn local_readiness_requires_a_fresh_supervised_prober_heartbeat() {
        let directory = Directory::in_memory().unwrap();
        assert!(matches!(
            directory.local_readiness(1_000),
            Err(DirectoryError::NotReady)
        ));
        assert!(matches!(
            directory.prober_freshness(1_000),
            Err(DirectoryError::NotReady)
        ));
        directory.mark_probe_sweep(1_000).unwrap();
        assert!(directory.local_readiness(1_000).is_ok());
        assert!(directory.prober_freshness(1_000).is_ok());
        assert!(matches!(
            directory.local_readiness(1_000 + PROBER_FRESHNESS_SECS + 1),
            Err(DirectoryError::NotReady)
        ));
        assert!(matches!(
            directory.prober_freshness(1_000 + PROBER_FRESHNESS_SECS + 1),
            Err(DirectoryError::NotReady)
        ));
    }

    #[test]
    fn an_unproven_claim_does_not_reserve_the_endpoint_against_other_candidates() {
        let directory = Directory::in_memory().unwrap();
        directory.submit(entry(1, "https://a.example"), 0).unwrap();

        directory.submit(entry(2, "https://a.example"), 1).unwrap();
        assert_eq!(
            directory.entry("https://a.example").unwrap().pubkey,
            crate::entry::hex(SigningKey::from_bytes(&[1; 32]).verifying_key().as_bytes())
        );
        assert_eq!(
            directory
                .lock()
                .query_row("SELECT COUNT(*) FROM pending_claims", [], |row| row
                    .get::<_, i64>(0))
                .unwrap(),
            1
        );
    }

    #[test]
    fn submit_and_drain_replays_are_rejected_by_one_shared_sequence() {
        let directory = Directory::in_memory().unwrap();
        let key = SigningKey::from_bytes(&[1; 32]);
        let first = entry(1, "https://a.example");
        directory.submit(first.clone(), 0).unwrap();
        assert!(matches!(
            directory.submit(first, 1),
            Err(DirectoryError::Replay)
        ));
        directory
            .set_state("https://a.example", LoftState::Active)
            .unwrap();

        let drain = DrainAuthorization::signed(&key, "https://a.example", 100, 2);
        directory.drain(&drain).unwrap();
        assert_eq!(
            directory
                .entry("https://a.example")
                .unwrap()
                .last_mutation_sequence,
            2
        );
        assert!(matches!(
            directory.drain(&drain),
            Err(DirectoryError::Replay)
        ));

        let stale_update = DirectoryEntry::signed_with_sequence(
            &key,
            "https://a.example",
            None,
            100,
            30,
            LoftPolicy {
                open: true,
                pow_floor: 0,
                max_event_bytes: 65536,
            },
            0.0,
            2,
        );
        assert!(matches!(
            directory.submit(stale_update, 2),
            Err(DirectoryError::Replay)
        ));
    }

    #[test]
    fn drain_requires_the_bound_loft_key() {
        let directory = Directory::in_memory().unwrap();
        directory.submit(entry(1, "https://a.example"), 0).unwrap();
        let attacker = SigningKey::from_bytes(&[2; 32]);

        assert!(matches!(
            directory.drain(&DrainAuthorization::signed(
                &attacker,
                "https://a.example",
                100,
                2,
            )),
            Err(DirectoryError::BadSignature)
        ));
        assert_eq!(
            directory.entry("https://a.example").unwrap().state,
            LoftState::Pending
        );
    }

    #[test]
    fn an_unproven_pending_claim_cannot_publish_itself_by_draining() {
        let directory = Directory::in_memory().unwrap();
        let key = SigningKey::from_bytes(&[1; 32]);
        directory.submit(entry(1, "https://a.example"), 0).unwrap();

        assert!(matches!(
            directory.drain(&DrainAuthorization::signed(
                &key,
                "https://a.example",
                100,
                2,
            )),
            Err(DirectoryError::NotReady)
        ));
        assert!(directory.entries().unwrap().is_empty());
    }

    #[test]
    fn directory_signing_key_survives_a_database_reopen() {
        let temp = tempfile::tempdir().unwrap();
        let path = private_database_path(temp.path(), "directory.db");
        let key = SigningKey::from_bytes(&[7; 32]);
        let first = Directory::open_with_signing_key(path.to_str().unwrap(), key)
            .unwrap()
            .signing_public_key();
        let second = Directory::open_existing(path.to_str().unwrap())
            .unwrap()
            .signing_public_key();
        assert_eq!(first, second);
    }

    #[test]
    fn public_open_refuses_a_fresh_database_without_creating_it() {
        let temp = tempfile::tempdir().unwrap();
        let path = private_database_path(temp.path(), "directory.db");
        assert!(matches!(
            Directory::open_existing(path.to_str().unwrap()),
            Err(DirectoryError::SigningKeyNotProvisioned)
        ));
        assert!(!path.exists());
    }

    #[test]
    fn public_open_refuses_an_unprovisioned_database_before_migration() {
        let temp = tempfile::tempdir().unwrap();
        let path = private_database_path(temp.path(), "directory.db");
        let (custody, connection) = create_private_database(&path);
        drop(connection);
        drop(custody);

        assert!(matches!(
            Directory::open_existing(path.to_str().unwrap()),
            Err(DirectoryError::SigningKeyNotProvisioned)
        ));
        let connection = Connection::open(path).unwrap();
        let tables: i64 = connection
            .query_row(
                "SELECT count(*) FROM sqlite_schema WHERE type = 'table'",
                [],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(tables, 0, "a refused public open changed sqlite_schema");
    }

    #[test]
    fn a_configured_directory_key_is_pinned_on_first_open() {
        let temp = tempfile::tempdir().unwrap();
        let path = private_database_path(temp.path(), "directory.db");
        let key = SigningKey::from_bytes(&[7; 32]);
        let first = Directory::open_with_signing_key(path.to_str().unwrap(), key.clone()).unwrap();
        assert_eq!(first.signing_public_key(), key.verifying_key().to_bytes());
        drop(first);

        Directory::open_with_signing_key(path.to_str().unwrap(), key).unwrap();
        assert!(matches!(
            Directory::open_with_signing_key(
                path.to_str().unwrap(),
                SigningKey::from_bytes(&[8; 32]),
            ),
            Err(DirectoryError::KeyMismatch)
        ));
    }

    #[cfg(unix)]
    #[test]
    fn persistent_database_permissions_protect_the_signing_seed() {
        use std::os::unix::fs::PermissionsExt;

        let temp = tempfile::tempdir().unwrap();
        let path = temp.path().join("directory.db");
        Directory::open(path.to_str().unwrap()).unwrap();
        assert_eq!(
            std::fs::metadata(path).unwrap().permissions().mode() & 0o777,
            0o600
        );
    }

    #[cfg(unix)]
    #[test]
    fn persistent_database_refuses_a_symbolic_link_parent() {
        use std::os::unix::fs::symlink;

        let temp = tempfile::tempdir().unwrap();
        let real = temp.path().join("real");
        std::fs::create_dir(&real).unwrap();
        let linked = temp.path().join("linked");
        symlink(&real, &linked).unwrap();

        assert!(matches!(
            Directory::open(linked.join("directory.db").to_str().unwrap()),
            Err(DirectoryError::Malformed(_))
        ));
    }

    #[test]
    fn opening_a_pre_sequence_database_applies_the_additive_migration() {
        let temp = tempfile::tempdir().unwrap();
        let path = private_database_path(temp.path(), "legacy.db");
        let (_custody, connection) = create_private_database(&path);
        let key = SigningKey::from_bytes(&[1; 32]);
        let legacy = DirectoryEntry::signed_legacy_for_test(
            &key,
            "https://a.example",
            100,
            30,
            LoftPolicy {
                open: true,
                pow_floor: 0,
                max_event_bytes: 65_536,
            },
        );
        let mut legacy_json = serde_json::to_value(&legacy).unwrap();
        let object = legacy_json.as_object_mut().unwrap();
        object.remove("sequence");
        object.remove("last_mutation_sequence");
        connection
            .execute_batch(include_str!("../tests/fixtures/v0_1_0_directory.sql"))
            .unwrap();
        connection
            .execute(
                "INSERT INTO lofts
                 (endpoint, entry, state, first_seen, last_probe, fail_streak, probes_ok,
                  probes_total, degraded_at, drain_after)
                 VALUES (?1, ?2, 'pending', 1, 2, 0, 1, 1, NULL, NULL)",
                params![legacy.endpoint, serde_json::to_vec(&legacy_json).unwrap()],
            )
            .unwrap();
        let old_probe = serde_json::json!({
            "endpoint": "https://a.example",
            "at": 2,
            "reachable": true,
            "stored_and_returned": true,
            "utilization": 0.25,
            "detail": null,
        });
        connection
            .execute(
                "INSERT INTO probes (endpoint, at, result) VALUES (?1, ?2, ?3)",
                params![
                    "https://a.example",
                    2i64,
                    serde_json::to_vec(&old_probe).unwrap()
                ],
            )
            .unwrap();
        drop(connection);

        let directory = Directory::open(path.to_str().unwrap()).unwrap();
        assert!(has_column(&directory.lock(), "lofts", "mutation_sequence").unwrap());
        assert!(has_column(&directory.lock(), "lofts", "clean_since").unwrap());
        assert!(has_column(&directory.lock(), "lofts", "next_probe_at").unwrap());
        assert!(has_column(&directory.lock(), "lofts", "ownership_proven").unwrap());
        assert!(has_column(&directory.lock(), "probes", "healthy").unwrap());
        assert!(has_column(&directory.lock(), "pending_claims", "loft_pubkey").unwrap());
        let migrated_version: i64 = directory
            .lock()
            .pragma_query_value(None, "user_version", |row| row.get(0))
            .unwrap();
        assert_eq!(migrated_version, i64::from(DIRECTORY_SCHEMA_VERSION));
        let migrated_health: i64 = directory
            .lock()
            .query_row("SELECT healthy FROM probes WHERE id = 1", [], |row| {
                row.get(0)
            })
            .unwrap();
        assert_eq!(migrated_health, 1);
        let ownership_proven: i64 = directory
            .lock()
            .query_row(
                "SELECT ownership_proven FROM lofts WHERE endpoint = ?1",
                params!["https://a.example"],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(ownership_proven, 1);
        assert_eq!(directory.entry("https://a.example").unwrap().sequence, 0);
        directory
            .submit(
                DirectoryEntry::signed_with_sequence(
                    &key,
                    "https://a.example",
                    None,
                    101,
                    30,
                    LoftPolicy {
                        open: true,
                        pow_floor: 0,
                        max_event_bytes: 65_536,
                    },
                    0.0,
                    1,
                ),
                3,
            )
            .unwrap();
        assert_eq!(directory.entry("https://a.example").unwrap().sequence, 1);
    }

    #[test]
    fn failed_v0_1_0_probe_backfill_rolls_the_entire_schema_migration_back() {
        let temp = tempfile::tempdir().unwrap();
        let path = private_database_path(temp.path(), "legacy.db");
        let (_custody, connection) = create_private_database(&path);
        connection
            .execute_batch(include_str!("../tests/fixtures/v0_1_0_directory.sql"))
            .unwrap();
        connection
            .execute(
                "INSERT INTO probes (endpoint, at, result) VALUES (?1, 1, ?2)",
                params!["https://a.example", b"{".as_slice()],
            )
            .unwrap();
        drop(connection);

        assert!(matches!(
            Directory::open(path.to_str().unwrap()),
            Err(DirectoryError::Serialization(_))
        ));
        let connection = Connection::open(&path).unwrap();
        let version: i64 = connection
            .pragma_query_value(None, "user_version", |row| row.get(0))
            .unwrap();
        assert_eq!(version, 0);
        assert!(!has_column(&connection, "lofts", "mutation_sequence").unwrap());
        assert!(!has_column(&connection, "probes", "healthy").unwrap());
    }

    #[test]
    fn a_future_directory_schema_is_refused_without_modification() {
        let temp = tempfile::tempdir().unwrap();
        let path = private_database_path(temp.path(), "future.db");
        let (_custody, connection) = create_private_database(&path);
        connection
            .execute_batch(include_str!("../tests/fixtures/v0_1_0_directory.sql"))
            .unwrap();
        connection.pragma_update(None, "user_version", 99).unwrap();
        drop(connection);

        let error = Directory::open(path.to_str().unwrap())
            .err()
            .expect("future schema must be refused");
        assert!(error.to_string().contains("newer than supported"));
        let connection = Connection::open(path).unwrap();
        let version: i64 = connection
            .pragma_query_value(None, "user_version", |row| row.get(0))
            .unwrap();
        assert_eq!(version, 99);
        assert!(!has_column(&connection, "lofts", "mutation_sequence").unwrap());
    }

    #[test]
    fn an_unknown_unversioned_directory_shape_is_refused_and_rolled_back() {
        let temp = tempfile::tempdir().unwrap();
        let path = private_database_path(temp.path(), "unknown.db");
        let (_custody, connection) = create_private_database(&path);
        connection
            .execute_batch(include_str!("../tests/fixtures/v0_1_0_directory.sql"))
            .unwrap();
        connection
            .execute("ALTER TABLE lofts ADD COLUMN surprise TEXT", [])
            .unwrap();
        drop(connection);

        let error = Directory::open(path.to_str().unwrap())
            .err()
            .expect("unknown legacy shape must be refused");
        assert!(error.to_string().contains("operator review"));
        let connection = Connection::open(path).unwrap();
        let version: i64 = connection
            .pragma_query_value(None, "user_version", |row| row.get(0))
            .unwrap();
        assert_eq!(version, 0);
        assert!(has_column(&connection, "lofts", "surprise").unwrap());
        assert!(!has_column(&connection, "lofts", "mutation_sequence").unwrap());
        assert!(!has_column(&connection, "probes", "healthy").unwrap());
    }

    #[test]
    fn a_legacy_v1_registration_is_accepted_once_then_upgrades_to_sequence_one() {
        let directory = Directory::in_memory().unwrap();
        let key = SigningKey::from_bytes(&[1; 32]);
        let legacy = DirectoryEntry::signed_legacy_for_test(
            &key,
            "https://a.example",
            100,
            30,
            LoftPolicy {
                open: true,
                pow_floor: 0,
                max_event_bytes: 65_536,
            },
        );
        directory.submit(legacy.clone(), 1).unwrap();
        assert!(matches!(
            directory.submit(legacy, 2),
            Err(DirectoryError::Replay)
        ));

        directory
            .submit(
                DirectoryEntry::signed_with_sequence(
                    &key,
                    "https://a.example",
                    None,
                    101,
                    30,
                    LoftPolicy {
                        open: true,
                        pow_floor: 0,
                        max_event_bytes: 65_536,
                    },
                    0.0,
                    1,
                ),
                3,
            )
            .unwrap();
        assert_eq!(directory.entry("https://a.example").unwrap().sequence, 1);
    }

    fn checkpoint(size: u64, marker: u8) -> PersistedRegistryCheckpoint {
        PersistedRegistryCheckpoint {
            version: 1,
            origin: "pigeonpost.test/registry".into(),
            size,
            root: [marker; 32],
            note: format!("pigeonpost.test/registry\n{size}\n{marker}"),
            witnessed_at: size.max(1),
        }
    }

    #[test]
    fn an_add_is_durably_reserved_without_becoming_routable_or_probeable() {
        let directory = Directory::in_memory().unwrap();
        let submitted = entry(1, "https://reserved.example");
        let reserved = directory.reserve_add(&submitted, 10).unwrap();
        assert_eq!(reserved.mutation, submitted.registry_addition().unwrap());
        assert!(reserved.previous_checkpoint.is_none());
        assert!(directory.has_pending_mutations().unwrap());
        assert!(matches!(
            directory.entry("https://reserved.example"),
            Err(DirectoryError::NotFound)
        ));
        assert!(directory.entries().unwrap().is_empty());
        assert!(directory.claim_probe_candidates(10, 10).unwrap().is_empty());

        let divergent = DirectoryEntry::signed_with_sequence(
            &SigningKey::from_bytes(&[1; 32]),
            "https://reserved.example",
            None,
            101,
            30,
            LoftPolicy {
                open: true,
                pow_floor: 0,
                max_event_bytes: 65_536,
            },
            0.0,
            1,
        );
        assert!(matches!(
            directory.reserve_add(&divergent, 11),
            Err(DirectoryError::Replay)
        ));
        assert_eq!(directory.pending_mutations(10).unwrap().len(), 1);

        directory
            .finalize_add(&submitted, &checkpoint(1, 1))
            .unwrap();
        assert!(!directory.has_pending_mutations().unwrap());
        assert_eq!(
            directory
                .entry("https://reserved.example")
                .unwrap()
                .sequence,
            1
        );
    }

    #[test]
    fn every_rejected_pre_admission_path_leaves_no_reservation() {
        let directory = Directory::in_memory().unwrap();
        let key = SigningKey::from_bytes(&[1; 32]);

        let mut malformed = entry(1, "https://malformed.example");
        malformed.capacity_gb += 1;
        assert!(matches!(
            directory.reserve_add(&malformed, 1),
            Err(DirectoryError::BadSignature)
        ));

        let future = DirectoryEntry::signed_with_sequence(
            &key,
            "https://future.example",
            None,
            100,
            30,
            LoftPolicy {
                open: true,
                pow_floor: 0,
                max_event_bytes: 65_536,
            },
            0.0,
            2,
        );
        assert!(matches!(
            directory.reserve_add(&future, 1),
            Err(DirectoryError::Replay)
        ));

        let existing = entry(1, "https://bound.example");
        directory.submit(existing.clone(), 1).unwrap();
        directory
            .set_state("https://bound.example", LoftState::Active)
            .unwrap();
        let stale = DirectoryEntry::signed_legacy_for_test(
            &key,
            "https://bound.example",
            100,
            30,
            LoftPolicy {
                open: true,
                pow_floor: 0,
                max_event_bytes: 65_536,
            },
        );
        assert!(matches!(
            directory.reserve_add(&stale, 2),
            Err(DirectoryError::Replay)
        ));
        let conflicting_key = SigningKey::from_bytes(&[2; 32]);
        let conflict = DirectoryEntry::signed_with_sequence(
            &conflicting_key,
            "https://bound.example",
            None,
            100,
            30,
            LoftPolicy {
                open: true,
                pow_floor: 0,
                max_event_bytes: 65_536,
            },
            0.0,
            1,
        );
        assert!(matches!(
            directory.reserve_add(&conflict, 2),
            Err(DirectoryError::KeyMismatch)
        ));

        let pending = entry(3, "https://unproven.example");
        directory.submit(pending, 1).unwrap();
        let drain = DrainAuthorization::signed(
            &SigningKey::from_bytes(&[3; 32]),
            "https://unproven.example",
            100,
            2,
        );
        assert!(matches!(
            directory.reserve_drain(&drain, 2),
            Err(DirectoryError::NotReady)
        ));
        assert!(!directory.has_pending_mutations().unwrap());
    }

    #[test]
    fn capacity_consuming_reservations_share_the_exact_pending_bound() {
        let directory = Directory::in_memory().unwrap();
        let reserved = entry(2, "https://reserved-slot.example");
        directory.reserve_add(&reserved, 1).unwrap();
        let encoded = serde_json::to_vec(&entry(1, "https://template.example")).unwrap();
        {
            let mut conn = directory.lock();
            let transaction = conn
                .transaction_with_behavior(TransactionBehavior::Immediate)
                .unwrap();
            for index in 0..MAX_PENDING_ENTRIES - 1 {
                transaction
                    .execute(
                        "INSERT INTO lofts
                             (endpoint, entry, state, first_seen, mutation_sequence,
                              next_probe_at, ownership_proven)
                         VALUES (?1, ?2, 'pending', 1, 1, 1, 0)",
                        params![format!("https://full-{index}.example"), encoded],
                    )
                    .unwrap();
            }
            transaction.commit().unwrap();
        }
        assert!(matches!(
            directory.reserve_add(&entry(3, "https://one-too-many.example"), 1),
            Err(DirectoryError::Unavailable)
        ));
        assert!(directory.has_pending_mutations().unwrap());
        assert_eq!(pending_load(&directory.lock()).unwrap(), 4_096);
        directory
            .finalize_add(&reserved, &checkpoint(1, 1))
            .unwrap();
        assert!(!directory.has_pending_mutations().unwrap());
        assert_eq!(pending_load(&directory.lock()).unwrap(), 4_096);
    }

    #[test]
    fn a_reserved_endpoint_fences_expiry_state_changes_and_in_flight_probes() {
        let directory = Directory::in_memory().unwrap();
        let key = SigningKey::from_bytes(&[1; 32]);
        let original = entry(1, "https://fenced.example");
        directory.submit(original.clone(), 1).unwrap();
        let leased = directory.claim_probe_candidates(1, 10).unwrap();
        assert_eq!(leased.len(), 1);
        let update = DirectoryEntry::signed_with_sequence(
            &key,
            "https://fenced.example",
            None,
            101,
            30,
            LoftPolicy {
                open: true,
                pow_floor: 0,
                max_event_bytes: 65_536,
            },
            0.0,
            2,
        );
        directory.reserve_add(&update, 2).unwrap();
        assert!(directory
            .claim_probe_candidates(PENDING_EXPIRE_SECS + 10, 10)
            .unwrap()
            .is_empty());
        assert!(matches!(
            directory.set_state("https://fenced.example", LoftState::Active),
            Err(DirectoryError::NotReady)
        ));
        let outcome = directory
            .record_claim_probe_with_retention(
                &probe("https://fenced.example", true),
                PENDING_EXPIRE_SECS + 10,
                None,
                &leased[0].pubkey,
                leased[0].sequence,
            )
            .unwrap();
        assert_eq!(outcome, None);
        assert_eq!(
            directory.entry("https://fenced.example").unwrap().sequence,
            1
        );
        directory.finalize_add(&update, &checkpoint(1, 1)).unwrap();
        assert_eq!(
            directory.entry("https://fenced.example").unwrap().sequence,
            2
        );
    }

    #[test]
    fn persistent_reservations_use_full_sync_and_survive_reopen() {
        let temp = tempfile::tempdir().unwrap();
        let path = private_database_path(temp.path(), "directory.db");
        let submitted = entry(1, "https://durable.example");
        {
            let directory = Directory::open(path.to_str().unwrap()).unwrap();
            let synchronous: i64 = directory
                .lock()
                .pragma_query_value(None, "synchronous", |row| row.get(0))
                .unwrap();
            assert_eq!(synchronous, 2, "persistent reservations require FULL sync");
            directory.reserve_add(&submitted, 10).unwrap();
        }
        let reopened = Directory::open(path.to_str().unwrap()).unwrap();
        let synchronous: i64 = reopened
            .lock()
            .pragma_query_value(None, "synchronous", |row| row.get(0))
            .unwrap();
        assert_eq!(synchronous, 2);
        assert_eq!(reopened.pending_mutations(10).unwrap().len(), 1);
        assert!(matches!(
            reopened.entry("https://durable.example"),
            Err(DirectoryError::NotFound)
        ));
    }

    #[test]
    fn schema_three_migrates_transactionally_and_partial_shape_is_refused() {
        let temp = tempfile::tempdir().unwrap();
        let path = private_database_path(temp.path(), "directory.db");
        Directory::open(path.to_str().unwrap()).unwrap();
        {
            let connection = Connection::open(&path).unwrap();
            connection
                .execute_batch(
                    "DROP TABLE directory_mutation_reservations;
                     PRAGMA user_version = 3;",
                )
                .unwrap();
        }
        let migrated = Directory::open(path.to_str().unwrap()).unwrap();
        let version: i64 = migrated
            .lock()
            .pragma_query_value(None, "user_version", |row| row.get(0))
            .unwrap();
        assert_eq!(version, 4);
        drop(migrated);

        {
            let connection = Connection::open(&path).unwrap();
            connection
                .execute_batch(
                    "DROP TABLE directory_mutation_reservations;
                     CREATE TABLE directory_mutation_reservations (endpoint TEXT PRIMARY KEY);
                     PRAGMA user_version = 3;",
                )
                .unwrap();
        }
        assert!(matches!(
            Directory::open(path.to_str().unwrap()),
            Err(DirectoryError::Malformed(_))
        ));
        let connection = Connection::open(path).unwrap();
        let version: i64 = connection
            .pragma_query_value(None, "user_version", |row| row.get(0))
            .unwrap();
        assert_eq!(version, 3);
        assert_eq!(
            connection
                .prepare("PRAGMA table_info(directory_mutation_reservations)")
                .unwrap()
                .query_map([], |row| row.get::<_, String>(1))
                .unwrap()
                .collect::<std::result::Result<Vec<_>, _>>()
                .unwrap(),
            vec!["endpoint"]
        );
    }

    #[test]
    fn schema_three_refuses_same_columns_with_weakened_keys_or_index_drift() {
        let temp = tempfile::tempdir().unwrap();
        for (name, mutation) in [
            (
                "missing-endpoint-primary-key",
                "DROP TABLE lofts;
                 CREATE TABLE lofts (
                     endpoint TEXT NOT NULL,
                     entry BLOB NOT NULL,
                     state TEXT NOT NULL,
                     first_seen INTEGER NOT NULL,
                     last_probe INTEGER NOT NULL DEFAULT 0,
                     fail_streak INTEGER NOT NULL DEFAULT 0,
                     probes_ok INTEGER NOT NULL DEFAULT 0,
                     probes_total INTEGER NOT NULL DEFAULT 0,
                     degraded_at INTEGER,
                     drain_after INTEGER,
                     mutation_sequence INTEGER NOT NULL DEFAULT 0,
                     clean_since INTEGER,
                     next_probe_at INTEGER NOT NULL DEFAULT 0,
                     ownership_proven INTEGER NOT NULL DEFAULT 0
                         CHECK (ownership_proven IN (0, 1))
                 );
                 CREATE INDEX lofts_by_probe_due
                     ON lofts (state, next_probe_at, last_probe, endpoint);",
            ),
            ("missing-index", "DROP INDEX probes_by_age;"),
            (
                "changed-index",
                "DROP INDEX pending_claims_by_probe_due;
                 CREATE INDEX pending_claims_by_probe_due
                     ON pending_claims (endpoint, loft_pubkey);",
            ),
        ] {
            let path = private_database_path(temp.path(), format!("{name}.db"));
            create_schema_four(&path);
            let connection = Connection::open(&path).unwrap();
            connection
                .execute_batch(
                    "DROP TABLE directory_mutation_reservations;
                     PRAGMA user_version = 3;",
                )
                .unwrap();
            connection.execute_batch(mutation).unwrap();
            drop(connection);
            assert_schema_refused_without_mutation(&path, 3);
        }
    }

    #[test]
    fn schema_four_refuses_weakened_reservation_constraints_and_index_drift() {
        let temp = tempfile::tempdir().unwrap();
        for (name, reservation_id, operation, capacity_slot) in [
            (
                "missing-reservation-unique",
                "BLOB NOT NULL CHECK (length(reservation_id) = 32)",
                "TEXT NOT NULL CHECK (operation IN ('add', 'drain'))",
                "INTEGER NOT NULL CHECK (capacity_slot IN (0, 1))",
            ),
            (
                "missing-operation-check",
                "BLOB NOT NULL UNIQUE CHECK (length(reservation_id) = 32)",
                "TEXT NOT NULL",
                "INTEGER NOT NULL CHECK (capacity_slot IN (0, 1))",
            ),
            (
                "missing-capacity-check",
                "BLOB NOT NULL UNIQUE CHECK (length(reservation_id) = 32)",
                "TEXT NOT NULL CHECK (operation IN ('add', 'drain'))",
                "INTEGER NOT NULL",
            ),
        ] {
            let path = private_database_path(temp.path(), format!("{name}.db"));
            create_schema_four(&path);
            let connection = Connection::open(&path).unwrap();
            connection
                .execute_batch(&format!(
                    "DROP TABLE directory_mutation_reservations;
                     CREATE TABLE directory_mutation_reservations (
                         reservation_id {reservation_id},
                         endpoint TEXT PRIMARY KEY,
                         operation {operation},
                         loft_pubkey TEXT NOT NULL,
                         mutation_sequence INTEGER NOT NULL CHECK (mutation_sequence >= 0),
                         canonical_mutation BLOB NOT NULL,
                         local_request BLOB NOT NULL,
                         reserved_at INTEGER NOT NULL CHECK (reserved_at >= 0),
                         capacity_slot {capacity_slot}
                     );
                     CREATE INDEX directory_reservations_by_age
                         ON directory_mutation_reservations (reserved_at, reservation_id);"
                ))
                .unwrap();
            drop(connection);
            assert_schema_refused_without_mutation(&path, 4);
        }

        for (name, mutation) in [
            (
                "missing-reservation-index",
                "DROP INDEX directory_reservations_by_age;",
            ),
            (
                "changed-reservation-index",
                "DROP INDEX directory_reservations_by_age;
                 CREATE INDEX directory_reservations_by_age
                     ON directory_mutation_reservations (reservation_id, reserved_at);",
            ),
        ] {
            let path = private_database_path(temp.path(), format!("{name}.db"));
            create_schema_four(&path);
            let connection = Connection::open(&path).unwrap();
            connection.execute_batch(mutation).unwrap();
            drop(connection);
            assert_schema_refused_without_mutation(&path, 4);
        }
    }

    #[test]
    fn two_process_finalizers_are_exactly_idempotent_but_never_authorize_divergence() {
        let temp = tempfile::tempdir().unwrap();
        let path = private_database_path(temp.path(), "directory.db");
        let first = Directory::open(path.to_str().unwrap()).unwrap();
        let second = Directory::open(path.to_str().unwrap()).unwrap();
        let submitted = entry(1, "https://race.example");
        first.reserve_add(&submitted, 1).unwrap();
        let pending = second.pending_mutations(10).unwrap().pop().unwrap();
        first
            .finalize_pending_mutation(&pending, &checkpoint(1, 1))
            .unwrap();
        second
            .finalize_pending_mutation(&pending, &checkpoint(1, 1))
            .unwrap();

        let divergent = DirectoryEntry::signed_with_sequence(
            &SigningKey::from_bytes(&[1; 32]),
            "https://race.example",
            None,
            999,
            30,
            LoftPolicy {
                open: true,
                pow_floor: 0,
                max_event_bytes: 65_536,
            },
            0.0,
            2,
        );
        assert!(matches!(
            second.finalize_add(&divergent, &checkpoint(2, 2)),
            Err(DirectoryError::Replay)
        ));

        first
            .set_state("https://race.example", LoftState::Active)
            .unwrap();
        let drain = DrainAuthorization::signed(
            &SigningKey::from_bytes(&[1; 32]),
            "https://race.example",
            10,
            2,
        );
        first.reserve_drain(&drain, 2).unwrap();
        let pending = second.pending_mutations(10).unwrap().pop().unwrap();
        first
            .finalize_pending_mutation(&pending, &checkpoint(2, 2))
            .unwrap();
        second
            .finalize_pending_mutation(&pending, &checkpoint(2, 2))
            .unwrap();
        first
            .record_probe(&probe("https://race.example", true), 10)
            .unwrap();
        assert_eq!(
            first.entry("https://race.example").unwrap().state,
            LoftState::Removed
        );
        second.finalize_drain(&drain, &checkpoint(2, 2)).unwrap();
        let divergent_drain = DrainAuthorization::signed(
            &SigningKey::from_bytes(&[1; 32]),
            "https://race.example",
            11,
            3,
        );
        assert!(matches!(
            second.finalize_drain(&divergent_drain, &checkpoint(3, 3)),
            Err(DirectoryError::Replay)
        ));
    }

    #[test]
    fn concurrent_sequences_cannot_roll_the_endpoint_back() {
        use std::sync::{Arc, Barrier};

        let directory = Arc::new(Directory::in_memory().unwrap());
        let key = SigningKey::from_bytes(&[1; 32]);
        directory.submit(entry(1, "https://a.example"), 0).unwrap();
        let barrier = Arc::new(Barrier::new(3));
        let mut workers = Vec::new();
        for sequence in [2, 3] {
            let directory = Arc::clone(&directory);
            let barrier = Arc::clone(&barrier);
            let key = key.clone();
            workers.push(std::thread::spawn(move || {
                let mutation = DirectoryEntry::signed_with_sequence(
                    &key,
                    "https://a.example",
                    None,
                    100 + sequence,
                    30,
                    LoftPolicy {
                        open: true,
                        pow_floor: 0,
                        max_event_bytes: 65_536,
                    },
                    0.0,
                    sequence,
                );
                barrier.wait();
                directory.submit(mutation, sequence)
            }));
        }
        barrier.wait();
        for worker in workers {
            let _ = worker.join().unwrap();
        }

        let stored = directory.entry("https://a.example").unwrap();
        assert_eq!(stored.sequence, 3);
        assert_eq!(stored.capacity_gb, 103);
    }
}
