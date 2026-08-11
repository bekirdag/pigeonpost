//! Transactional registry storage and persisted RFC 6962 nodes.
//!
//! The running service never rebuilds or clones the whole log.  Every append stores at most one
//! node per tree level, signs the resulting root, and commits the entry, nodes, projections, and
//! checkpoint in one `BEGIN IMMEDIATE` transaction.

use std::collections::HashMap;

use ed25519_dalek::SigningKey;
use pigeonpost_compliance_format::ComplianceKeyId;
use rusqlite::{params, Connection, OptionalExtension, Transaction, TransactionBehavior};
use subtle::ConstantTimeEq;

use crate::checkpoint::Checkpoint;
use crate::entry::{
    ComplianceKeyPublish, ComplianceKeyStatus, DirectoryAdd, DirectoryRemove, EntryKind, LogEntry,
};
use crate::error::{RegistryError, Result};
use crate::log::{self, Hash};
use crate::witness::WitnessReceipt;

pub(crate) const SCHEMA_VERSION: u32 = 9;

/// The most handles one provider account may hold at once.
///
/// Schema 8 and earlier enforced exactly one, via `UNIQUE` on `current_bindings.subject`. That is
/// stricter than the product needs: an account whose upstream name changes has no way to keep the
/// name people already published. Three is the agreed allowance, and it is enforced by counting
/// rather than by a constraint, so the number can move without another migration.
pub(crate) const MAX_HANDLES_PER_SUBJECT: usize = 3;
const DIRECTORY_SCHEMA_VERSION: u32 = 3;
const WITNESS_SCHEMA_VERSION: u32 = 4;
const IDENTITY_CHALLENGE_SCHEMA_VERSION: u32 = 5;
const DIRECTORY_CLAIM_STREAM_SCHEMA_VERSION: u32 = 6;
const GLOBAL_BINDING_ADMISSION_SCHEMA_VERSION: u32 = 7;
const IDENTITY_CHALLENGE_RESULT_SCHEMA_VERSION: u32 = 8;
pub(crate) const MAX_PAGE_SIZE: u64 = 1_000;
/// One FULL-synchronous reservation amortizes at most this many trace-backed claim admissions.
/// Any reserved slots left in memory are intentionally burned by restart or crash.
const GLOBAL_BINDING_ADMISSION_BATCH_SIZE: u32 = 64;
const GLOBAL_BINDING_ADMISSION_MIGRATION_CEILING: u32 = 1_000_000;
/// Keep committed challenge results long enough for a client to recover after a dropped blocking
/// timeout. This is in addition to the original five-minute challenge lifetime.
const IDENTITY_CHALLENGE_RESULT_RETENTION_MS: u64 = 5 * 60 * 1_000;

#[derive(Debug, Default)]
pub(crate) struct GlobalBindingAdmissionBatch {
    /// Highest UTC minute requested or observed by this process. It never moves backward even if
    /// this process still owns reserve from an older minute.
    highest_requested_minute: u64,
    /// Lexicographic durable high-water independent of the current lease. It detects same-minute
    /// rollback after exhaustion or a runtime-limit change.
    durable_high_water: Option<(u64, u32)>,
    lease: Option<GlobalBindingAdmissionLease>,
}

#[derive(Debug, Clone, Copy)]
struct GlobalBindingAdmissionLease {
    minute: u64,
    limit: u32,
    remaining: u32,
    reserved_through: u32,
}

fn validate_global_binding_observation(
    batch: &mut GlobalBindingAdmissionBatch,
    observed: (u64, u32),
) -> Result<()> {
    if batch
        .durable_high_water
        .is_some_and(|high_water| observed < high_water)
    {
        batch.lease = None;
        return Err(RegistryError::RegistryUnavailable);
    }
    if batch
        .durable_high_water
        .is_none_or(|high_water| observed > high_water)
    {
        batch.durable_high_water = Some(observed);
    }
    Ok(())
}

pub(crate) enum LegacyAuthorization<'a> {
    Refuse,
    SignedCheckpoint(&'a str),
}

#[derive(Debug, Clone)]
pub(crate) struct TreeState {
    pub size: u64,
    pub root: Hash,
    pub checkpoint: String,
}

#[derive(Debug, Clone)]
pub(crate) struct PublishedState {
    pub state: TreeState,
    pub witnessed_at: Option<u64>,
}

struct StoredIdentityChallenge {
    provider: String,
    handle: String,
    pubkey: Vec<u8>,
    pkce_challenge: Option<String>,
    expires_at: i64,
    consumed_at: Option<i64>,
    binding_seq: Option<i64>,
}

#[derive(Debug, Clone)]
pub(crate) struct HandleAppend {
    pub seq: u64,
    pub appended: bool,
}

#[derive(Debug, Clone)]
pub(crate) struct DirectoryAppend {
    pub seq: u64,
    pub appended: bool,
}

#[derive(Debug, Clone)]
pub(crate) struct ComplianceAppend {
    pub seq: u64,
    pub appended: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum HandleAppendMode {
    Register,
    Rotate,
}

pub(crate) struct HandleAppendRequest<'a> {
    pub handle: &'a str,
    pub pubkey: &'a str,
    pub subject: &'a str,
    pub ts_ms: u64,
    pub mode: HandleAppendMode,
}

pub(crate) fn initialize(
    conn: &mut Connection,
    origin: &str,
    signing_key: &SigningKey,
    authorization: LegacyAuthorization<'_>,
) -> Result<()> {
    let version = user_version(conn)?;
    if version > SCHEMA_VERSION {
        return Err(RegistryError::UnsupportedSchema {
            found: version,
            supported: SCHEMA_VERSION,
        });
    }

    if version == SCHEMA_VERSION {
        return verify_storage_snapshot(conn, origin, &signing_key.verifying_key());
    }
    if (2..=IDENTITY_CHALLENGE_RESULT_SCHEMA_VERSION).contains(&version) {
        return migrate_supported_schema(conn, version, origin, &signing_key.verifying_key());
    }
    if version != 0 {
        return Err(RegistryError::MigrationRequired(format!(
            "no migration path from schema {version} to {SCHEMA_VERSION}"
        )));
    }

    if schema_objects(conn)?.is_empty() {
        return create_fresh(conn, origin, signing_key);
    }

    migrate_legacy(conn, origin, signing_key, authorization)
}

/// Validate one exact predecessor before changing it, apply the complete chain under one writer
/// transaction, and validate the final schema and authority projections before commit. A failure
/// at any point leaves both `sqlite_schema` and `user_version` at the predecessor snapshot.
fn migrate_supported_schema(
    conn: &mut Connection,
    version: u32,
    origin: &str,
    verifying_key: &ed25519_dalek::VerifyingKey,
) -> Result<()> {
    let tx = conn.transaction_with_behavior(TransactionBehavior::Immediate)?;
    verify_canonical_schema(&tx, version)?;

    if version <= 2 {
        migrate_v2_directory_projection(&tx)?;
    }
    if version <= DIRECTORY_SCHEMA_VERSION {
        migrate_v3_witness_publication(&tx)?;
    }
    if version <= WITNESS_SCHEMA_VERSION {
        migrate_v4_identity_challenge_binding(&tx)?;
    }
    if version <= IDENTITY_CHALLENGE_SCHEMA_VERSION {
        migrate_v5_directory_claim_streams(&tx)?;
    }
    if version <= DIRECTORY_CLAIM_STREAM_SCHEMA_VERSION {
        migrate_v6_global_binding_admission(&tx)?;
    }
    if version <= GLOBAL_BINDING_ADMISSION_SCHEMA_VERSION {
        migrate_v7_identity_challenge_results(&tx)?;
    }
    if version <= IDENTITY_CHALLENGE_RESULT_SCHEMA_VERSION {
        migrate_v8_handle_quota(&tx)?;
    }

    verify_canonical_schema(&tx, SCHEMA_VERSION)?;
    verify_storage(&tx, origin, verifying_key)?;
    tx.commit()?;
    Ok(())
}

fn migrate_v2_directory_projection(conn: &Connection) -> Result<()> {
    let directory_entries: i64 = conn.query_row(
        "SELECT COUNT(*) FROM entries
         WHERE entry_type IN ('directory_add', 'directory_remove')",
        [],
        |row| row.get(0),
    )?;
    if directory_entries != 0 {
        return Err(RegistryError::MigrationRequired(
            "schema v2 contains unauthenticated prototype directory leaves; operator review is required"
                .into(),
        ));
    }

    create_directory_projection_v3(conn)?;
    conn.execute(
        "INSERT INTO schema_migrations
         (version, applied_at_ms, source_schema, authorization_checkpoint)
         VALUES (?1, ?2, 'schema-v2', NULL)",
        params![DIRECTORY_SCHEMA_VERSION, to_i64(now_ms(), "timestamp")?],
    )?;
    conn.pragma_update(None, "user_version", DIRECTORY_SCHEMA_VERSION)?;
    Ok(())
}

fn migrate_v3_witness_publication(conn: &Connection) -> Result<()> {
    conn.execute_batch(
        r#"
        CREATE TABLE handle_history (
            seq      INTEGER PRIMARY KEY REFERENCES entries(seq),
            handle   TEXT NOT NULL,
            pubkey   TEXT NOT NULL,
            subject  TEXT NOT NULL
        );
        CREATE INDEX handle_history_by_handle ON handle_history (handle, seq DESC);

        CREATE TABLE published_state (
            singleton        INTEGER PRIMARY KEY CHECK (singleton = 1),
            tree_size        INTEGER NOT NULL CHECK (tree_size >= 0),
            root             BLOB NOT NULL CHECK (length(root) = 32),
            checkpoint_note  TEXT NOT NULL,
            witnessed_at     INTEGER CHECK (witnessed_at IS NULL OR witnessed_at > 0)
        );

        CREATE TABLE witness_receipts (
            witness_name   TEXT PRIMARY KEY,
            tree_size      INTEGER NOT NULL CHECK (tree_size >= 0),
            root           BLOB NOT NULL CHECK (length(root) = 32),
            witnessed_at   INTEGER NOT NULL CHECK (witnessed_at > 0),
            receipt_json   TEXT NOT NULL,
            updated_at_ms  INTEGER NOT NULL CHECK (updated_at_ms >= 0)
        );
        CREATE INDEX witness_receipts_by_head
            ON witness_receipts (tree_size, root, witnessed_at);
        "#,
    )?;

    let mut history = Vec::new();
    {
        let mut statement = conn.prepare(
            "SELECT seq, version, entry_type, entry_json, ts_ms, leaf_hash FROM entries
             WHERE entry_type IN ('handle_bind', 'handle_rotate') ORDER BY seq",
        )?;
        let rows = statement.query_map([], |row| {
            Ok((
                row.get(0)?,
                row.get(1)?,
                row.get(2)?,
                row.get(3)?,
                row.get(4)?,
                row.get(5)?,
            ))
        })?;
        for row in rows {
            let entry = parse_entry_row(row?)?;
            let (handle, pubkey, subject) = entry.handle_binding().ok_or_else(|| {
                RegistryError::CorruptStorage(
                    "handle history migration found a non-binding entry".into(),
                )
            })?;
            history.push((
                entry.seq(),
                handle.to_owned(),
                pubkey.to_owned(),
                subject.to_owned(),
            ));
        }
    }
    for (seq, handle, pubkey, subject) in history {
        conn.execute(
            "INSERT INTO handle_history (seq, handle, pubkey, subject) VALUES (?1, ?2, ?3, ?4)",
            params![to_i64(seq, "entry sequence")?, handle, pubkey, subject],
        )?;
    }

    // Schema v3 never persisted independently verified cosignatures. Start publication at the
    // operator-signed empty checkpoint rather than manufacturing witness state for existing leaves.
    let (root, note): (Vec<u8>, String) = conn.query_row(
        "SELECT root, note FROM checkpoints WHERE tree_size = 0",
        [],
        |row| Ok((row.get(0)?, row.get(1)?)),
    )?;
    conn.execute(
        "INSERT INTO published_state
         (singleton, tree_size, root, checkpoint_note, witnessed_at)
         VALUES (1, 0, ?1, ?2, NULL)",
        params![root, note],
    )?;
    conn.execute(
        "INSERT INTO schema_migrations
         (version, applied_at_ms, source_schema, authorization_checkpoint)
         VALUES (?1, ?2, 'schema-v3', NULL)",
        params![WITNESS_SCHEMA_VERSION, to_i64(now_ms(), "timestamp")?],
    )?;
    conn.pragma_update(None, "user_version", WITNESS_SCHEMA_VERSION)?;
    Ok(())
}

/// Schema-v4 challenges were not bound to the key requesting them. Challenges are deliberately
/// ephemeral, so migration invalidates every outstanding flow instead of carrying forward rows
/// that cannot be authenticated retroactively.
fn migrate_v4_identity_challenge_binding(conn: &Connection) -> Result<()> {
    conn.execute_batch(
        r#"
        DROP INDEX identity_challenges_expiry;
        DROP TABLE identity_challenges;
        CREATE TABLE identity_challenges (
            challenge_hash  BLOB PRIMARY KEY CHECK (length(challenge_hash) = 32),
            provider        TEXT NOT NULL CHECK (provider IN ('github', 'google')),
            handle          TEXT NOT NULL,
            pubkey          BLOB NOT NULL CHECK (length(pubkey) = 32),
            pkce_challenge  TEXT,
            expires_at_ms   INTEGER NOT NULL CHECK (expires_at_ms >= 0),
            consumed_at_ms  INTEGER CHECK (consumed_at_ms IS NULL OR consumed_at_ms >= 0),
            CHECK ((provider = 'github' AND pkce_challenge IS NOT NULL)
                OR (provider = 'google' AND pkce_challenge IS NULL))
        );
        CREATE INDEX identity_challenges_expiry
            ON identity_challenges (expires_at_ms, consumed_at_ms);
        "#,
    )?;
    conn.execute(
        "INSERT INTO schema_migrations
         (version, applied_at_ms, source_schema, authorization_checkpoint)
         VALUES (?1, ?2, 'schema-v4', NULL)",
        params![
            IDENTITY_CHALLENGE_SCHEMA_VERSION,
            to_i64(now_ms(), "timestamp")?
        ],
    )?;
    conn.pragma_update(None, "user_version", IDENTITY_CHALLENGE_SCHEMA_VERSION)?;
    Ok(())
}

/// Schema v5 projected one current owner per endpoint. That made an unauthenticated-to-the-origin
/// first claim a permanent registry-level reservation, even when every directory probe proved that
/// key was not served there. Schema v6 tracks an independent monotonic stream per endpoint/key;
/// directories remain the fail-closed authority that decides which proven stream may route.
fn migrate_v5_directory_claim_streams(conn: &Connection) -> Result<()> {
    conn.execute_batch(
        r#"
        ALTER TABLE directory_mutations RENAME TO directory_mutations_v5;
        CREATE TABLE directory_mutations (
            endpoint           TEXT NOT NULL,
            loft_pubkey        TEXT NOT NULL,
            mutation_sequence  INTEGER NOT NULL CHECK (mutation_sequence >= 0),
            entry_seq          INTEGER NOT NULL UNIQUE REFERENCES entries(seq),
            mutation_kind      TEXT NOT NULL CHECK (mutation_kind IN ('directory_add', 'directory_remove')),
            PRIMARY KEY (endpoint, loft_pubkey)
        );
        "#,
    )?;

    let history = {
        let mut statement = conn.prepare(
            "SELECT seq, version, entry_type, entry_json, ts_ms, leaf_hash
             FROM entries
             WHERE entry_type IN ('directory_add', 'directory_remove')
             ORDER BY seq",
        )?;
        let rows = statement.query_map([], |row| {
            Ok((
                row.get(0)?,
                row.get(1)?,
                row.get(2)?,
                row.get(3)?,
                row.get(4)?,
                row.get(5)?,
            ))
        })?;
        rows.map(|row| parse_entry_row(row?))
            .collect::<Result<Vec<_>>>()?
    };
    for entry in history {
        update_directory_projection(conn, &entry)?;
    }
    conn.execute("DROP TABLE directory_mutations_v5", [])?;
    conn.execute(
        "INSERT INTO schema_migrations
         (version, applied_at_ms, source_schema, authorization_checkpoint)
         VALUES (?1, ?2, 'schema-v5', NULL)",
        params![
            DIRECTORY_CLAIM_STREAM_SCHEMA_VERSION,
            to_i64(now_ms(), "timestamp")?
        ],
    )?;
    conn.pragma_update(None, "user_version", DIRECTORY_CLAIM_STREAM_SCHEMA_VERSION)?;
    Ok(())
}

/// Schema v7 makes the trace-backed global claim rate restart-safe. One durable UTC-minute row
/// prevents repeated process restarts (or two processes sharing the WAL) from creating more rate
/// windows than the conservative storage planner accounts for.
fn migrate_v6_global_binding_admission(conn: &Connection) -> Result<()> {
    create_global_binding_admission(conn)?;
    // V6 did not persist this counter. Cool down the remainder of the migration minute instead of
    // pretending no pre-upgrade claims occurred and accidentally granting a second full window.
    cool_down_global_binding_admission(conn)?;
    conn.execute(
        "INSERT INTO schema_migrations
         (version, applied_at_ms, source_schema, authorization_checkpoint)
         VALUES (?1, ?2, 'schema-v6', NULL)",
        params![
            GLOBAL_BINDING_ADMISSION_SCHEMA_VERSION,
            to_i64(now_ms(), "timestamp")?
        ],
    )?;
    conn.pragma_update(
        None,
        "user_version",
        GLOBAL_BINDING_ADMISSION_SCHEMA_VERSION,
    )?;
    Ok(())
}

/// Schema v8 couples challenge consumption to the exact committed binding sequence. Challenges
/// are short-lived authorization material, so the migration invalidates all outstanding v7 flows
/// instead of carrying rows that cannot retroactively prove an atomic result.
fn migrate_v7_identity_challenge_results(conn: &Connection) -> Result<()> {
    conn.execute_batch(
        r#"
        DROP INDEX identity_challenges_expiry;
        DROP TABLE identity_challenges;
        CREATE TABLE identity_challenges (
            challenge_hash  BLOB PRIMARY KEY CHECK (length(challenge_hash) = 32),
            provider        TEXT NOT NULL CHECK (provider IN ('github', 'google')),
            handle          TEXT NOT NULL,
            pubkey          BLOB NOT NULL CHECK (length(pubkey) = 32),
            pkce_challenge  TEXT,
            expires_at_ms   INTEGER NOT NULL CHECK (expires_at_ms >= 0),
            consumed_at_ms  INTEGER CHECK (consumed_at_ms IS NULL OR consumed_at_ms >= 0),
            binding_seq     INTEGER UNIQUE REFERENCES entries(seq),
            CHECK ((provider = 'github' AND pkce_challenge IS NOT NULL)
                OR (provider = 'google' AND pkce_challenge IS NULL)),
            CHECK ((consumed_at_ms IS NULL AND binding_seq IS NULL)
                OR (consumed_at_ms IS NOT NULL AND binding_seq IS NOT NULL))
        );
        CREATE INDEX identity_challenges_expiry
            ON identity_challenges (expires_at_ms, consumed_at_ms);
        "#,
    )?;
    conn.execute(
        "INSERT INTO schema_migrations
         (version, applied_at_ms, source_schema, authorization_checkpoint)
         VALUES (?1, ?2, 'schema-v7', NULL)",
        params![
            IDENTITY_CHALLENGE_RESULT_SCHEMA_VERSION,
            to_i64(now_ms(), "timestamp")?
        ],
    )?;
    conn.pragma_update(
        None,
        "user_version",
        IDENTITY_CHALLENGE_RESULT_SCHEMA_VERSION,
    )?;
    Ok(())
}

/// Schema 8 → 9: one provider account may hold up to [`MAX_HANDLES_PER_SUBJECT`] handles.
///
/// Schema 8 spelled "exactly one" as `UNIQUE` on `current_bindings.subject`. SQLite cannot drop a
/// column constraint in place, so the table is rebuilt. The quota itself is deliberately *not* a
/// constraint — it is counted at admission, so changing the allowance later is a constant, not
/// another migration.
fn migrate_v8_handle_quota(conn: &Connection) -> Result<()> {
    conn.execute_batch(
        r#"
        -- Rename the predecessor out of the way and create the replacement under its final name.
        -- `ALTER TABLE ... RENAME TO` rewrites the stored CREATE statement with a quoted
        -- identifier, which would no longer match the canonical schema text byte for byte.
        ALTER TABLE current_bindings RENAME TO current_bindings_v8;
        CREATE TABLE current_bindings (
            handle       TEXT PRIMARY KEY,
            pubkey       TEXT NOT NULL,
            subject      TEXT NOT NULL,
            seq          INTEGER NOT NULL UNIQUE REFERENCES entries(seq)
        );
        INSERT INTO current_bindings (handle, pubkey, subject, seq)
            SELECT handle, pubkey, subject, seq FROM current_bindings_v8;
        DROP TABLE current_bindings_v8;
        CREATE INDEX current_bindings_by_subject ON current_bindings (subject);
        "#,
    )?;
    conn.execute(
        "INSERT INTO schema_migrations
         (version, applied_at_ms, source_schema, authorization_checkpoint)
         VALUES (?1, ?2, 'schema-v8', NULL)",
        params![SCHEMA_VERSION, to_i64(now_ms(), "timestamp")?],
    )?;
    conn.pragma_update(None, "user_version", SCHEMA_VERSION)?;
    Ok(())
}

fn cool_down_global_binding_admission(conn: &Connection) -> Result<()> {
    let updated = conn.execute(
        "UPDATE global_binding_admission
         SET window_minute = ?1, admissions = ?2
         WHERE singleton = 1",
        params![
            to_i64(now_ms() / 60_000, "global binding migration minute")?,
            i64::from(GLOBAL_BINDING_ADMISSION_MIGRATION_CEILING)
        ],
    )?;
    if updated != 1 {
        return Err(RegistryError::CorruptStorage(
            "global binding admission state is not a singleton".into(),
        ));
    }
    Ok(())
}

fn verify_storage_snapshot(
    conn: &mut Connection,
    origin: &str,
    verifying_key: &ed25519_dalek::VerifyingKey,
) -> Result<()> {
    let tx = conn.transaction_with_behavior(TransactionBehavior::Deferred)?;
    verify_canonical_schema(&tx, SCHEMA_VERSION)?;
    verify_storage(&tx, origin, verifying_key)?;
    tx.commit()?;
    Ok(())
}

fn create_fresh(conn: &mut Connection, origin: &str, signing_key: &SigningKey) -> Result<()> {
    let tx = conn.transaction_with_behavior(TransactionBehavior::Immediate)?;
    create_tables(&tx)?;
    let root = log::empty_root();
    let note = Checkpoint {
        origin: origin.to_owned(),
        size: 0,
        root,
    }
    .sign(signing_key);
    tx.execute(
        "INSERT INTO registry_state (singleton, tree_size, root, checkpoint_note)
         VALUES (1, 0, ?1, ?2)",
        params![root.as_slice(), note],
    )?;
    tx.execute(
        "INSERT INTO published_state
         (singleton, tree_size, root, checkpoint_note, witnessed_at)
         VALUES (1, 0, ?1, ?2, NULL)",
        params![root.as_slice(), note],
    )?;
    tx.execute(
        "INSERT INTO checkpoints (tree_size, root, note, created_at_ms)
         VALUES (0, ?1, ?2, ?3)",
        params![root.as_slice(), note, to_i64(now_ms(), "timestamp")?],
    )?;
    tx.execute(
        "INSERT INTO schema_migrations
         (version, applied_at_ms, source_schema, authorization_checkpoint)
         VALUES (?1, ?2, 'fresh', NULL)",
        params![SCHEMA_VERSION, to_i64(now_ms(), "timestamp")?],
    )?;
    tx.pragma_update(None, "user_version", SCHEMA_VERSION)?;
    verify_canonical_schema(&tx, SCHEMA_VERSION)?;
    verify_storage(&tx, origin, &signing_key.verifying_key())?;
    tx.commit()?;
    Ok(())
}

fn migrate_legacy(
    conn: &mut Connection,
    origin: &str,
    signing_key: &SigningKey,
    authorization: LegacyAuthorization<'_>,
) -> Result<()> {
    let tx = conn.transaction_with_behavior(TransactionBehavior::Immediate)?;
    verify_legacy_schema(&tx)?;
    validate_legacy_columns(&tx)?;
    let entries = read_legacy_entries(&tx)?;
    let mut log = log::MerkleLog::new();
    for entry in &entries {
        log.append(&entry.leaf_bytes().map_err(malformed_entry)?);
    }
    let root = log.root();
    let size = entries.len() as u64;

    let authorization_note = match (size, authorization) {
        (0, _) => Checkpoint {
            origin: origin.to_owned(),
            size,
            root,
        }
        .sign(signing_key),
        (_, LegacyAuthorization::Refuse) => {
            return Err(RegistryError::MigrationRequired(
                "supply the last signed legacy checkpoint with Registry::open_with_legacy_checkpoint"
                    .into(),
            ));
        }
        (_, LegacyAuthorization::SignedCheckpoint(note)) => {
            let checkpoint = Checkpoint::verify(note, &signing_key.verifying_key())?;
            if checkpoint.origin != origin
                || checkpoint.size != size
                || !log::hash_eq(&checkpoint.root, &root)
            {
                return Err(RegistryError::MigrationRequired(
                    "legacy checkpoint does not match this database's origin, size, and root"
                        .into(),
                ));
            }
            note.to_owned()
        }
    };

    if table_exists(&tx, "legacy_entries_v0")? {
        return Err(RegistryError::MigrationRequired(
            "legacy_entries_v0 already exists; operator inspection is required".into(),
        ));
    }
    tx.execute_batch("ALTER TABLE entries RENAME TO legacy_entries_v0;")?;
    create_tables(&tx)?;
    // The unversioned predecessor had no durable counter and may already have served this UTC
    // minute, even when no binding leaf survived. Never grant a second migration-minute window.
    cool_down_global_binding_admission(&tx)?;

    for entry in &entries {
        insert_entry_rows(&tx, entry)?;
        append_merkle_leaf(
            &tx,
            entry.seq(),
            log::leaf_hash(&entry.leaf_bytes().map_err(malformed_entry)?),
        )?;
    }

    let empty_root = log::empty_root();
    let empty_note = Checkpoint {
        origin: origin.to_owned(),
        size: 0,
        root: empty_root,
    }
    .sign(signing_key);
    tx.execute(
        "INSERT INTO checkpoints (tree_size, root, note, created_at_ms)
         VALUES (0, ?1, ?2, ?3)",
        params![
            empty_root.as_slice(),
            empty_note,
            to_i64(now_ms(), "timestamp")?
        ],
    )?;
    if size > 0 {
        tx.execute(
            "INSERT INTO checkpoints (tree_size, root, note, created_at_ms)
             VALUES (?1, ?2, ?3, ?4)",
            params![
                to_i64(size, "tree size")?,
                root.as_slice(),
                authorization_note,
                to_i64(now_ms(), "timestamp")?
            ],
        )?;
    }
    tx.execute(
        "INSERT INTO registry_state (singleton, tree_size, root, checkpoint_note)
         VALUES (1, ?1, ?2, ?3)",
        params![
            to_i64(size, "tree size")?,
            root.as_slice(),
            authorization_note
        ],
    )?;
    tx.execute(
        "INSERT INTO published_state
         (singleton, tree_size, root, checkpoint_note, witnessed_at)
         VALUES (1, 0, ?1, ?2, NULL)",
        params![empty_root.as_slice(), empty_note],
    )?;
    tx.execute(
        "INSERT INTO schema_migrations
         (version, applied_at_ms, source_schema, authorization_checkpoint)
         VALUES (?1, ?2, 'legacy-v0', ?3)",
        params![
            SCHEMA_VERSION,
            to_i64(now_ms(), "timestamp")?,
            authorization_note
        ],
    )?;
    tx.pragma_update(None, "user_version", SCHEMA_VERSION)?;
    verify_canonical_schema(&tx, SCHEMA_VERSION)?;
    verify_storage(&tx, origin, &signing_key.verifying_key())?;
    tx.commit()?;
    Ok(())
}

fn create_tables(conn: &Connection) -> Result<()> {
    conn.execute_batch(
        r#"
        CREATE TABLE entries (
            seq          INTEGER PRIMARY KEY CHECK (seq >= 0),
            version      INTEGER NOT NULL CHECK (version IN (0, 1)),
            entry_type   TEXT NOT NULL CHECK (entry_type IN (
                'handle_bind', 'handle_rotate', 'directory_add', 'directory_remove',
                'compliance_key_publish'
            )),
            entry_json   TEXT NOT NULL,
            ts_ms        INTEGER NOT NULL CHECK (ts_ms >= 0),
            leaf_hash    BLOB NOT NULL CHECK (length(leaf_hash) = 32)
        );
        CREATE INDEX registry_entries_by_type ON entries (entry_type, seq);

        CREATE TABLE current_bindings (
            handle       TEXT PRIMARY KEY,
            pubkey       TEXT NOT NULL,
            subject      TEXT NOT NULL,
            seq          INTEGER NOT NULL UNIQUE REFERENCES entries(seq)
        );
        CREATE INDEX current_bindings_by_subject ON current_bindings (subject);

        CREATE TABLE handle_history (
            seq      INTEGER PRIMARY KEY REFERENCES entries(seq),
            handle   TEXT NOT NULL,
            pubkey   TEXT NOT NULL,
            subject  TEXT NOT NULL
        );
        CREATE INDEX handle_history_by_handle ON handle_history (handle, seq DESC);

        CREATE TABLE compliance_keys (
            seq             INTEGER PRIMARY KEY REFERENCES entries(seq),
            key_id          BLOB NOT NULL CHECK (length(key_id) = 47),
            public_key      BLOB NOT NULL CHECK (length(public_key) = 32),
            purpose         INTEGER NOT NULL,
            jurisdiction    INTEGER NOT NULL,
            authority       BLOB NOT NULL CHECK (length(authority) = 32),
            epoch_start_ms  INTEGER NOT NULL,
            generation      INTEGER NOT NULL,
            not_before_ms   INTEGER NOT NULL,
            not_after_ms    INTEGER NOT NULL,
            published_at_ms INTEGER NOT NULL,
            status          INTEGER NOT NULL CHECK (status IN (1, 2, 3))
        );
        CREATE INDEX compliance_keys_by_id ON compliance_keys (key_id, seq);
        CREATE INDEX compliance_keys_live
            ON compliance_keys (purpose, jurisdiction, status, not_before_ms, not_after_ms);
        CREATE INDEX compliance_keys_history
            ON compliance_keys (authority, purpose, jurisdiction, epoch_start_ms, generation);

        CREATE TABLE directory_mutations (
            endpoint           TEXT NOT NULL,
            loft_pubkey        TEXT NOT NULL,
            mutation_sequence  INTEGER NOT NULL CHECK (mutation_sequence >= 0),
            entry_seq          INTEGER NOT NULL UNIQUE REFERENCES entries(seq),
            mutation_kind      TEXT NOT NULL CHECK (mutation_kind IN ('directory_add', 'directory_remove')),
            PRIMARY KEY (endpoint, loft_pubkey)
        );

        CREATE TABLE merkle_nodes (
            level       INTEGER NOT NULL CHECK (level >= 0),
            node_index  INTEGER NOT NULL CHECK (node_index >= 0),
            hash        BLOB NOT NULL CHECK (length(hash) = 32),
            PRIMARY KEY (level, node_index)
        );

        CREATE TABLE checkpoints (
            tree_size      INTEGER PRIMARY KEY CHECK (tree_size >= 0),
            root           BLOB NOT NULL CHECK (length(root) = 32),
            note           TEXT NOT NULL,
            created_at_ms  INTEGER NOT NULL CHECK (created_at_ms >= 0)
        );

        CREATE TABLE registry_state (
            singleton        INTEGER PRIMARY KEY CHECK (singleton = 1),
            tree_size        INTEGER NOT NULL CHECK (tree_size >= 0),
            root             BLOB NOT NULL CHECK (length(root) = 32),
            checkpoint_note  TEXT NOT NULL
        );

        CREATE TABLE published_state (
            singleton        INTEGER PRIMARY KEY CHECK (singleton = 1),
            tree_size        INTEGER NOT NULL CHECK (tree_size >= 0),
            root             BLOB NOT NULL CHECK (length(root) = 32),
            checkpoint_note  TEXT NOT NULL,
            witnessed_at     INTEGER CHECK (witnessed_at IS NULL OR witnessed_at > 0)
        );

        CREATE TABLE witness_receipts (
            witness_name   TEXT PRIMARY KEY,
            tree_size      INTEGER NOT NULL CHECK (tree_size >= 0),
            root           BLOB NOT NULL CHECK (length(root) = 32),
            witnessed_at   INTEGER NOT NULL CHECK (witnessed_at > 0),
            receipt_json   TEXT NOT NULL,
            updated_at_ms  INTEGER NOT NULL CHECK (updated_at_ms >= 0)
        );
        CREATE INDEX witness_receipts_by_head
            ON witness_receipts (tree_size, root, witnessed_at);

        CREATE TABLE schema_migrations (
            version                   INTEGER PRIMARY KEY,
            applied_at_ms             INTEGER NOT NULL,
            source_schema             TEXT NOT NULL,
            authorization_checkpoint  TEXT
        );

        CREATE TABLE identity_challenges (
            challenge_hash  BLOB PRIMARY KEY CHECK (length(challenge_hash) = 32),
            provider        TEXT NOT NULL CHECK (provider IN ('github', 'google')),
            handle          TEXT NOT NULL,
            pubkey          BLOB NOT NULL CHECK (length(pubkey) = 32),
            pkce_challenge  TEXT,
            expires_at_ms   INTEGER NOT NULL CHECK (expires_at_ms >= 0),
            consumed_at_ms  INTEGER CHECK (consumed_at_ms IS NULL OR consumed_at_ms >= 0),
            binding_seq     INTEGER UNIQUE REFERENCES entries(seq),
            CHECK ((provider = 'github' AND pkce_challenge IS NOT NULL)
                OR (provider = 'google' AND pkce_challenge IS NULL)),
            CHECK ((consumed_at_ms IS NULL AND binding_seq IS NULL)
                OR (consumed_at_ms IS NOT NULL AND binding_seq IS NOT NULL))
        );
        CREATE INDEX identity_challenges_expiry
            ON identity_challenges (expires_at_ms, consumed_at_ms);

        CREATE TRIGGER entries_require_contiguous_sequence
        BEFORE INSERT ON entries
        WHEN NEW.seq != (SELECT COUNT(*) FROM entries)
        BEGIN
            SELECT RAISE(ABORT, 'non-contiguous registry log sequence');
        END;

        CREATE TRIGGER entries_are_append_only_update
        BEFORE UPDATE ON entries
        BEGIN
            SELECT RAISE(ABORT, 'registry entries are append-only');
        END;
        CREATE TRIGGER entries_are_append_only_delete
        BEFORE DELETE ON entries
        BEGIN
            SELECT RAISE(ABORT, 'registry entries are append-only');
        END;
        CREATE TRIGGER checkpoints_are_append_only_update
        BEFORE UPDATE ON checkpoints
        BEGIN
            SELECT RAISE(ABORT, 'registry checkpoints are append-only');
        END;
        CREATE TRIGGER checkpoints_are_append_only_delete
        BEFORE DELETE ON checkpoints
        BEGIN
            SELECT RAISE(ABORT, 'registry checkpoints are append-only');
        END;
        "#,
    )?;
    create_global_binding_admission(conn)?;
    Ok(())
}

fn create_global_binding_admission(conn: &Connection) -> Result<()> {
    conn.execute_batch(
        r#"
        CREATE TABLE global_binding_admission (
            singleton      INTEGER PRIMARY KEY CHECK (singleton = 1),
            window_minute  INTEGER NOT NULL CHECK (window_minute >= 0),
            admissions     INTEGER NOT NULL CHECK (admissions BETWEEN 0 AND 1000000)
        );
        INSERT INTO global_binding_admission (singleton, window_minute, admissions)
            VALUES (1, 0, 0);
        "#,
    )?;
    Ok(())
}

fn create_directory_projection_v3(conn: &Connection) -> Result<()> {
    conn.execute_batch(
        r#"
        CREATE TABLE directory_mutations (
            endpoint           TEXT PRIMARY KEY,
            loft_pubkey        TEXT NOT NULL,
            mutation_sequence  INTEGER NOT NULL CHECK (mutation_sequence >= 0),
            entry_seq          INTEGER NOT NULL UNIQUE REFERENCES entries(seq),
            mutation_kind      TEXT NOT NULL CHECK (mutation_kind IN ('directory_add', 'directory_remove'))
        );
        "#,
    )?;
    Ok(())
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct SchemaObject {
    kind: String,
    name: String,
    table: String,
    sql: String,
}

/// Read the durable, operator-created schema. SQLite's implicit auto-indexes have no SQL and are
/// covered by their owning table's canonical UNIQUE/PRIMARY KEY declaration.
fn schema_objects(conn: &Connection) -> Result<Vec<SchemaObject>> {
    let mut statement = conn.prepare(
        "SELECT type, name, tbl_name, sql
         FROM sqlite_schema
         WHERE sql IS NOT NULL AND name NOT LIKE 'sqlite_%'
         ORDER BY type, name",
    )?;
    let rows = statement.query_map([], |row| {
        Ok(SchemaObject {
            kind: row.get(0)?,
            name: row.get(1)?,
            table: row.get(2)?,
            sql: normalize_schema_sql(&row.get::<_, String>(3)?),
        })
    })?;
    rows.collect::<rusqlite::Result<Vec<_>>>()
        .map_err(RegistryError::from)
}

fn normalize_schema_sql(sql: &str) -> String {
    sql.chars()
        .filter(|character| !character.is_ascii_whitespace())
        .flat_map(char::to_lowercase)
        .collect()
}

fn verify_canonical_schema(conn: &Connection, version: u32) -> Result<()> {
    if !(2..=SCHEMA_VERSION).contains(&version) {
        return Err(RegistryError::MigrationRequired(format!(
            "schema {version} has no canonical Registry shape"
        )));
    }
    let actual = schema_objects(conn)?;
    let carries_legacy_archive = actual
        .iter()
        .any(|object| object.name == "legacy_entries_v0" || object.name == "entries_by_handle");
    let expected = canonical_schema(version, carries_legacy_archive)?;
    if actual == expected {
        return Ok(());
    }

    let describe = |objects: &[SchemaObject]| {
        objects
            .iter()
            .map(|object| format!("{}:{}", object.kind, object.name))
            .collect::<Vec<_>>()
            .join(", ")
    };
    let message = format!(
        "schema v{version} is not the exact released shape (found [{}], expected [{}]; first mismatch: {})",
        describe(&actual),
        describe(&expected),
        actual
            .iter()
            .zip(&expected)
            .find(|(found, expected)| found != expected)
            .map(|(found, expected)| format!(
                "{}:{} `{}` != `{}`",
                found.kind, found.name, found.sql, expected.sql
            ))
            .unwrap_or_else(|| "object count differs".into())
    );
    if version == SCHEMA_VERSION {
        Err(RegistryError::CorruptStorage(message))
    } else {
        Err(RegistryError::MigrationRequired(message))
    }
}

fn verify_legacy_schema(conn: &Connection) -> Result<()> {
    let expected = {
        let reference = Connection::open_in_memory()?;
        create_legacy_reference_schema(&reference)?;
        schema_objects(&reference)?
    };
    let actual = schema_objects(conn)?;
    if actual != expected {
        return Err(RegistryError::MigrationRequired(
            "unversioned Registry storage is not the exact v0.1.0 released shape".into(),
        ));
    }
    Ok(())
}

fn canonical_schema(version: u32, carries_legacy_archive: bool) -> Result<Vec<SchemaObject>> {
    let reference = Connection::open_in_memory()?;
    if carries_legacy_archive {
        create_legacy_reference_schema(&reference)?;
        reference.execute_batch("ALTER TABLE entries RENAME TO legacy_entries_v0;")?;
    }
    create_tables(&reference)?;
    shape_reference_schema(&reference, version)?;
    schema_objects(&reference)
}

/// Derive every tracked predecessor from the final schema using the inverse of its released
/// migration. This keeps unchanged table/index/trigger SQL byte-for-byte tied to `create_tables`.
fn shape_reference_schema(conn: &Connection, version: u32) -> Result<()> {
    if version < SCHEMA_VERSION {
        // Inverse of `migrate_v8_handle_quota`: restore `UNIQUE` on subject.
        conn.execute_batch(
            r#"
            DROP INDEX current_bindings_by_subject;
            DROP TABLE current_bindings;
            CREATE TABLE current_bindings (
                handle       TEXT PRIMARY KEY,
                pubkey       TEXT NOT NULL,
                subject      TEXT NOT NULL UNIQUE,
                seq          INTEGER NOT NULL UNIQUE REFERENCES entries(seq)
            );
            "#,
        )?;
    }
    if version < IDENTITY_CHALLENGE_RESULT_SCHEMA_VERSION {
        conn.execute_batch(
            r#"
            DROP INDEX identity_challenges_expiry;
            DROP TABLE identity_challenges;
            CREATE TABLE identity_challenges (
                challenge_hash  BLOB PRIMARY KEY CHECK (length(challenge_hash) = 32),
                provider        TEXT NOT NULL CHECK (provider IN ('github', 'google')),
                handle          TEXT NOT NULL,
                pubkey          BLOB NOT NULL CHECK (length(pubkey) = 32),
                pkce_challenge  TEXT,
                expires_at_ms   INTEGER NOT NULL CHECK (expires_at_ms >= 0),
                consumed_at_ms  INTEGER CHECK (consumed_at_ms IS NULL OR consumed_at_ms >= 0),
                CHECK ((provider = 'github' AND pkce_challenge IS NOT NULL)
                    OR (provider = 'google' AND pkce_challenge IS NULL))
            );
            CREATE INDEX identity_challenges_expiry
                ON identity_challenges (expires_at_ms, consumed_at_ms);
            "#,
        )?;
    }
    if version < GLOBAL_BINDING_ADMISSION_SCHEMA_VERSION {
        conn.execute_batch("DROP TABLE global_binding_admission;")?;
    }
    if version < DIRECTORY_CLAIM_STREAM_SCHEMA_VERSION {
        conn.execute_batch("DROP TABLE directory_mutations;")?;
        if version >= DIRECTORY_SCHEMA_VERSION {
            create_directory_projection_v3(conn)?;
        }
    }
    if version < IDENTITY_CHALLENGE_SCHEMA_VERSION {
        conn.execute_batch(
            r#"
            DROP INDEX identity_challenges_expiry;
            DROP TABLE identity_challenges;
            CREATE TABLE identity_challenges (
                challenge_hash  BLOB PRIMARY KEY CHECK (length(challenge_hash) = 32),
                provider        TEXT NOT NULL CHECK (provider IN ('github', 'google')),
                pkce_challenge  TEXT,
                expires_at_ms   INTEGER NOT NULL CHECK (expires_at_ms >= 0),
                consumed_at_ms  INTEGER CHECK (consumed_at_ms IS NULL OR consumed_at_ms >= 0),
                CHECK ((provider = 'github' AND pkce_challenge IS NOT NULL)
                    OR (provider = 'google' AND pkce_challenge IS NULL))
            );
            CREATE INDEX identity_challenges_expiry
                ON identity_challenges (expires_at_ms, consumed_at_ms);
            "#,
        )?;
    }
    if version < WITNESS_SCHEMA_VERSION {
        conn.execute_batch(
            r#"
            DROP INDEX witness_receipts_by_head;
            DROP TABLE witness_receipts;
            DROP TABLE published_state;
            DROP INDEX handle_history_by_handle;
            DROP TABLE handle_history;
            "#,
        )?;
    }
    Ok(())
}

fn create_legacy_reference_schema(conn: &Connection) -> Result<()> {
    conn.execute_batch(
        r#"
        CREATE TABLE entries (
            idx        INTEGER PRIMARY KEY,
            kind       TEXT NOT NULL,
            handle     TEXT NOT NULL,
            pubkey     TEXT NOT NULL,
            subject    TEXT NOT NULL,
            timestamp  INTEGER NOT NULL
        );
        CREATE INDEX entries_by_handle ON entries (handle, idx);
        "#,
    )?;
    Ok(())
}

/// Spend one trace-backed claim admission from a FULL-committed reservation in the canonical
/// UTC-minute window, refilling the bounded process-local reservation when needed.
///
/// The refill transaction is intentionally independent of the later trace/log append. Once
/// dispensed, a failure or cancellation burns the slot and cannot create a refund race; reserve
/// still unused at a crash is also burned. A backward clock step fails closed until wall time
/// reaches the already-persisted minute.
pub(crate) fn charge_global_binding_admission(
    conn: &mut Connection,
    batch: &mut GlobalBindingAdmissionBatch,
    timestamp_ms: u64,
    limit: u32,
) -> Result<()> {
    if limit == 0 {
        return Err(RegistryError::InvalidConfiguration(
            "global binding admission limit must not be zero".into(),
        ));
    }
    let window_minute = timestamp_ms / 60_000;
    if window_minute < batch.highest_requested_minute {
        return Err(RegistryError::RegistryUnavailable);
    }
    if window_minute > batch.highest_requested_minute {
        batch.highest_requested_minute = window_minute;
        batch.lease = None;
    }

    // A local reserve still validates the durable singleton with a read-only query. This catches
    // peer-process rollover and database rollback without another FULL-synchronous write.
    let observed = load_global_binding_admission(conn)?;
    validate_global_binding_observation(batch, observed)?;
    if observed.0 > batch.highest_requested_minute {
        batch.highest_requested_minute = observed.0;
        batch.lease = None;
    }
    if window_minute < observed.0 {
        return Err(RegistryError::RegistryUnavailable);
    }

    let lease_is_usable = batch.lease.is_some_and(|lease| {
        lease.minute == window_minute && lease.limit == limit && lease.remaining > 0
    });
    if lease_is_usable {
        let lease = batch
            .lease
            .as_mut()
            .expect("usable global binding admission lease exists");
        if observed.0 != window_minute || observed.1 < lease.reserved_through {
            batch.lease = None;
            return Err(RegistryError::RegistryUnavailable);
        }
        lease.remaining -= 1;
        return Ok(());
    }
    // A changed limit, exhausted lease, or rollover burns any unused process-local slots. Their
    // durable reservation stays charged and therefore makes a lower limit fail closed.
    batch.lease = None;

    let tx = conn.transaction_with_behavior(TransactionBehavior::Immediate)?;
    let (stored_minute, admissions) = load_global_binding_admission(&tx)?;
    validate_global_binding_observation(batch, (stored_minute, admissions))?;
    if window_minute < stored_minute {
        batch.highest_requested_minute = stored_minute;
        return Err(RegistryError::RegistryUnavailable);
    }
    let admitted = if window_minute > stored_minute {
        0
    } else {
        admissions
    };
    let available = limit
        .checked_sub(admitted)
        .ok_or(RegistryError::RateLimited)?;
    if available == 0 {
        return Err(RegistryError::RateLimited);
    }
    let reserved = available.min(GLOBAL_BINDING_ADMISSION_BATCH_SIZE);
    let next = admitted.checked_add(reserved).ok_or_else(|| {
        RegistryError::CorruptStorage("global binding admission count overflowed".into())
    })?;
    let updated = tx.execute(
        "UPDATE global_binding_admission
         SET window_minute = ?1, admissions = ?2
         WHERE singleton = 1 AND window_minute = ?3 AND admissions = ?4",
        params![
            to_i64(window_minute, "global binding admission minute")?,
            i64::from(next),
            to_i64(stored_minute, "stored global binding admission minute")?,
            i64::from(admissions)
        ],
    )?;
    if updated != 1 {
        return Err(RegistryError::RegistryUnavailable);
    }
    tx.commit()?;
    // Dispense only after the reservation has crossed the configured FULL-sync commit boundary.
    batch.lease = Some(GlobalBindingAdmissionLease {
        minute: window_minute,
        limit,
        remaining: reserved - 1,
        reserved_through: next,
    });
    batch.durable_high_water = Some((window_minute, next));
    Ok(())
}

fn load_global_binding_admission(conn: &Connection) -> Result<(u64, u32)> {
    let row: Option<(i64, i64)> = conn
        .query_row(
            "SELECT window_minute, admissions FROM global_binding_admission WHERE singleton = 1",
            [],
            |row| Ok((row.get(0)?, row.get(1)?)),
        )
        .optional()?;
    let Some((window_minute, admissions)) = row else {
        return Err(RegistryError::CorruptStorage(
            "global binding admission state is missing".into(),
        ));
    };
    let row_count: i64 =
        conn.query_row("SELECT COUNT(*) FROM global_binding_admission", [], |row| {
            row.get(0)
        })?;
    if row_count != 1 {
        return Err(RegistryError::CorruptStorage(
            "global binding admission state is not a singleton".into(),
        ));
    }
    let admissions =
        u32::try_from(nonnegative(admissions, "global binding admissions")?).map_err(|_| {
            RegistryError::CorruptStorage("global binding admission count is oversized".into())
        })?;
    Ok((
        nonnegative(window_minute, "global binding admission minute")?,
        admissions,
    ))
}

pub(crate) fn append_compliance_key(
    conn: &mut Connection,
    origin: &str,
    signing_key: &SigningKey,
    publication: ComplianceKeyPublish,
    ts_ms: u64,
) -> Result<ComplianceAppend> {
    // Allocate the sequence only after reserving the database writer. A second Registry process
    // may append to this WAL between any pre-transaction head read and BEGIN IMMEDIATE.
    let tx = conn.transaction_with_behavior(TransactionBehavior::Immediate)?;
    let seq = load_state(&tx)?.size;
    let entry = LogEntry::compliance_key(seq, publication, ts_ms);
    entry.validate().map_err(malformed_entry)?;
    if let Some(existing_seq) = validate_compliance_transition(&tx, &entry)? {
        tx.commit()?;
        return Ok(ComplianceAppend {
            seq: existing_seq,
            appended: false,
        });
    }
    append_entry_in_transaction(&tx, origin, signing_key, &entry)?;
    tx.commit()?;
    Ok(ComplianceAppend {
        seq,
        appended: true,
    })
}

pub(crate) fn append_directory_add(
    conn: &mut Connection,
    origin: &str,
    signing_key: &SigningKey,
    mutation: DirectoryAdd,
    ts_ms: u64,
) -> Result<DirectoryAppend> {
    append_directory_mutation(
        conn,
        origin,
        signing_key,
        DirectoryMutation::Add(mutation),
        ts_ms,
    )
}

pub(crate) fn append_directory_remove(
    conn: &mut Connection,
    origin: &str,
    signing_key: &SigningKey,
    mutation: DirectoryRemove,
    ts_ms: u64,
) -> Result<DirectoryAppend> {
    append_directory_mutation(
        conn,
        origin,
        signing_key,
        DirectoryMutation::Remove(mutation),
        ts_ms,
    )
}

enum DirectoryMutation {
    Add(DirectoryAdd),
    Remove(DirectoryRemove),
}

fn append_directory_mutation(
    conn: &mut Connection,
    origin: &str,
    signing_key: &SigningKey,
    mutation: DirectoryMutation,
    ts_ms: u64,
) -> Result<DirectoryAppend> {
    let tx = conn.transaction_with_behavior(TransactionBehavior::Immediate)?;
    let state = load_state(&tx)?;
    let entry = match mutation {
        DirectoryMutation::Add(payload) => LogEntry::directory_add(state.size, payload, ts_ms),
        DirectoryMutation::Remove(payload) => {
            LogEntry::directory_remove(state.size, payload, ts_ms)
        }
    };
    entry.validate().map_err(malformed_entry)?;
    let (endpoint, loft_pubkey, mutation_sequence) =
        entry.authenticated_directory_mutation().ok_or_else(|| {
            RegistryError::MalformedEntry(
                "directory append requires the original loft authentication".into(),
            )
        })?;
    let current = current_directory_mutation(&tx, endpoint, loft_pubkey)?;
    match current {
        None if matches!(entry, LogEntry::DirectoryRemove(_)) => {
            return Err(RegistryError::NotFound);
        }
        None if mutation_sequence > 1 => return Err(RegistryError::DirectoryReplay),
        None => {}
        Some((entry_seq, current_sequence, _)) => {
            if mutation_sequence < current_sequence {
                return Err(RegistryError::DirectoryReplay);
            }
            if mutation_sequence == current_sequence {
                let existing = entry_at(&tx, entry_seq)?.ok_or_else(|| {
                    RegistryError::CorruptStorage(
                        "directory projection points at a missing entry".into(),
                    )
                })?;
                let exact_retry = match (&existing, &entry) {
                    (LogEntry::DirectoryAdd(previous), LogEntry::DirectoryAdd(next)) => {
                        previous.payload == next.payload
                    }
                    (LogEntry::DirectoryRemove(previous), LogEntry::DirectoryRemove(next)) => {
                        previous.payload == next.payload
                    }
                    _ => false,
                };
                if !exact_retry {
                    return Err(RegistryError::DirectoryReplay);
                }
                tx.commit()?;
                return Ok(DirectoryAppend {
                    seq: entry_seq,
                    appended: false,
                });
            }
        }
    }

    append_entry_in_transaction(&tx, origin, signing_key, &entry)?;
    tx.commit()?;
    Ok(DirectoryAppend {
        seq: entry.seq(),
        appended: true,
    })
}

#[cfg(test)]
pub(crate) fn append_handle(
    conn: &mut Connection,
    origin: &str,
    signing_key: &SigningKey,
    request: HandleAppendRequest<'_>,
) -> Result<HandleAppend> {
    let tx = conn.transaction_with_behavior(TransactionBehavior::Immediate)?;
    let appended = append_handle_in_transaction(&tx, origin, signing_key, request)?;
    tx.commit()?;
    Ok(appended)
}

fn append_handle_in_transaction(
    tx: &Transaction<'_>,
    origin: &str,
    signing_key: &SigningKey,
    request: HandleAppendRequest<'_>,
) -> Result<HandleAppend> {
    let state = load_state(tx)?;
    let current = current_binding(tx, request.handle)?;
    // A provider account may hold up to `MAX_HANDLES_PER_SUBJECT` handles at once. Counted here,
    // inside the writer transaction that appends the leaf, so two concurrent claims for the same
    // account cannot both observe a free slot and both take it.
    //
    // Only a *new* handle consumes a slot: a rotation rebinds a handle this subject already owns,
    // and re-registering the same handle is idempotent further down.
    let held_by_subject: usize = tx
        .query_row(
            "SELECT COUNT(*) FROM current_bindings WHERE subject = ?1 AND handle <> ?2",
            params![request.subject, request.handle],
            |row| row.get::<_, i64>(0),
        )
        .optional()?
        .unwrap_or(0)
        .try_into()
        .unwrap_or(usize::MAX);
    let claims_new_slot = matches!(request.mode, HandleAppendMode::Register) && current.is_none();
    if claims_new_slot && held_by_subject >= MAX_HANDLES_PER_SUBJECT {
        return Err(RegistryError::HandleQuotaExceeded {
            limit: MAX_HANDLES_PER_SUBJECT,
        });
    }
    match (&current, request.mode) {
        (None, HandleAppendMode::Rotate) => return Err(RegistryError::NotFound),
        (Some((seq, existing, existing_subject)), HandleAppendMode::Register)
            if existing == request.pubkey && existing_subject == request.subject =>
        {
            return Ok(HandleAppend {
                seq: *seq,
                appended: false,
            });
        }
        (Some(_), HandleAppendMode::Register) => return Err(RegistryError::AlreadyBound),
        (Some((_, _, existing_subject)), HandleAppendMode::Rotate)
            if existing_subject != request.subject =>
        {
            // Upstream usernames can be reassigned. A fresh proof for the same spelling must not
            // let a different provider account inherit the prior owner's append-only identity.
            return Err(RegistryError::AlreadyBound);
        }
        (Some((seq, existing, existing_subject)), HandleAppendMode::Rotate)
            if existing == request.pubkey && existing_subject == request.subject =>
        {
            return Ok(HandleAppend {
                seq: *seq,
                appended: false,
            });
        }
        _ => {}
    }

    let entry = match request.mode {
        HandleAppendMode::Register => LogEntry::handle_claim(
            state.size,
            request.handle.to_owned(),
            request.pubkey.to_owned(),
            request.subject.to_owned(),
            request.ts_ms,
        ),
        HandleAppendMode::Rotate => LogEntry::handle_rotation(
            state.size,
            request.handle.to_owned(),
            request.pubkey.to_owned(),
            request.subject.to_owned(),
            request.ts_ms,
        ),
    };
    entry.validate().map_err(malformed_entry)?;
    append_entry_in_transaction(tx, origin, signing_key, &entry)?;
    Ok(HandleAppend {
        seq: entry.seq(),
        appended: true,
    })
}

/// Validate the append-only state machine. An exact retry returns the already committed sequence,
/// allowing an operator to resume witness publication after a crash or witness outage without
/// creating another leaf.
fn validate_compliance_transition(conn: &Connection, entry: &LogEntry) -> Result<Option<u64>> {
    let publication = entry.compliance_publication().ok_or_else(|| {
        RegistryError::CorruptStorage("compliance append received another entry kind".into())
    })?;
    let key_id = publication
        .key_id
        .encode()
        .map_err(|error| RegistryError::MalformedEntry(error.to_string()))?;
    let previous: Option<(i64, Vec<u8>, i64, i64, i64)> = conn
        .query_row(
            "SELECT seq, public_key, not_before_ms, not_after_ms, status
             FROM compliance_keys WHERE key_id = ?1 ORDER BY seq DESC LIMIT 1",
            params![key_id.as_slice()],
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
    let Some((previous_seq, previous_key, previous_start, previous_end, previous_status)) =
        previous
    else {
        if publication.status != ComplianceKeyStatus::Active {
            return Err(RegistryError::MalformedEntry(
                "the first compliance-key publication must be active".into(),
            ));
        }
        return Ok(None);
    };
    let public_key = decode_hex(&publication.public_key, 32)
        .ok_or_else(|| RegistryError::MalformedEntry("invalid compliance public key".into()))?;
    if !bool::from(previous_key.ct_eq(&public_key))
        || nonnegative(previous_start, "previous validity start")? != publication.not_before_ms
        || nonnegative(previous_end, "previous validity end")? != publication.not_after_ms
    {
        return Err(RegistryError::MalformedEntry(
            "a compliance key status entry cannot redefine its key or validity interval".into(),
        ));
    }
    let previous_status = u8::try_from(previous_status).map_err(|_| {
        RegistryError::CorruptStorage("previous compliance status is out of range".into())
    })?;
    let next_status = publication.status as u8;
    if next_status == previous_status {
        return Ok(Some(nonnegative(
            previous_seq,
            "previous compliance sequence",
        )?));
    }
    if next_status <= previous_status
        || !matches!(
            publication.status,
            ComplianceKeyStatus::Retired | ComplianceKeyStatus::Revoked
        )
    {
        return Err(RegistryError::MalformedEntry(
            "compliance key status transitions must advance active -> retired/revoked or retired -> revoked"
                .into(),
        ));
    }
    Ok(None)
}

fn append_entry_in_transaction(
    tx: &Transaction<'_>,
    origin: &str,
    signing_key: &SigningKey,
    entry: &LogEntry,
) -> Result<TreeState> {
    let state = load_state(tx)?;
    if entry.seq() != state.size {
        return Err(RegistryError::CorruptStorage(format!(
            "append sequence {} does not equal current tree size {}",
            entry.seq(),
            state.size
        )));
    }

    insert_entry_rows(tx, entry)?;
    let leaf = log::leaf_hash(&entry.leaf_bytes().map_err(malformed_entry)?);
    append_merkle_leaf(tx, entry.seq(), leaf)?;
    let size = state
        .size
        .checked_add(1)
        .ok_or_else(|| RegistryError::CorruptStorage("tree size overflow".into()))?;
    let root = root_for_size(tx, size)?;
    let note = Checkpoint {
        origin: origin.to_owned(),
        size,
        root,
    }
    .sign(signing_key);

    tx.execute(
        "INSERT INTO checkpoints (tree_size, root, note, created_at_ms)
         VALUES (?1, ?2, ?3, ?4)",
        params![
            to_i64(size, "tree size")?,
            root.as_slice(),
            note,
            to_i64(now_ms(), "timestamp")?
        ],
    )?;
    tx.execute(
        "UPDATE registry_state
         SET tree_size = ?1, root = ?2, checkpoint_note = ?3
         WHERE singleton = 1",
        params![to_i64(size, "tree size")?, root.as_slice(), note],
    )?;
    Ok(TreeState {
        size,
        root,
        checkpoint: note,
    })
}

fn insert_entry_rows(conn: &Connection, entry: &LogEntry) -> Result<()> {
    let json = serde_json::to_string(entry)?;
    let leaf = log::leaf_hash(&entry.leaf_bytes().map_err(malformed_entry)?);
    conn.execute(
        "INSERT INTO entries (seq, version, entry_type, entry_json, ts_ms, leaf_hash)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
        params![
            to_i64(entry.seq(), "entry sequence")?,
            i64::from(entry.version()),
            entry.kind().as_str(),
            json,
            to_i64(entry.ts_ms(), "entry timestamp")?,
            leaf.as_slice()
        ],
    )?;

    if let Some((handle, pubkey, subject)) = entry.handle_binding() {
        conn.execute(
            "INSERT INTO handle_history (seq, handle, pubkey, subject)
             VALUES (?1, ?2, ?3, ?4)",
            params![
                to_i64(entry.seq(), "entry sequence")?,
                handle,
                pubkey,
                subject,
            ],
        )?;
        conn.execute(
            "INSERT INTO current_bindings (handle, pubkey, subject, seq)
             VALUES (?1, ?2, ?3, ?4)
             ON CONFLICT(handle) DO UPDATE SET
                pubkey = excluded.pubkey,
                subject = excluded.subject,
                seq = excluded.seq",
            params![
                handle,
                pubkey,
                subject,
                to_i64(entry.seq(), "entry sequence")?
            ],
        )?;
    }

    update_directory_projection(conn, entry)?;

    if let Some(publication) = entry.compliance_publication() {
        let key_id = publication
            .key_id
            .encode()
            .map_err(|e| RegistryError::MalformedEntry(e.to_string()))?;
        let public_key = decode_hex(&publication.public_key, 32)
            .ok_or_else(|| RegistryError::MalformedEntry("invalid compliance public key".into()))?;
        conn.execute(
            "INSERT INTO compliance_keys
             (seq, key_id, public_key, purpose, jurisdiction, authority, epoch_start_ms,
              generation, not_before_ms, not_after_ms, published_at_ms, status)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12)",
            params![
                to_i64(entry.seq(), "entry sequence")?,
                key_id.as_slice(),
                public_key,
                u8::from(publication.key_id.purpose),
                u8::from(publication.key_id.jurisdiction),
                publication.key_id.authority.as_slice(),
                to_i64(publication.key_id.epoch_start_ms, "key epoch")?,
                i64::from(publication.key_id.generation),
                to_i64(publication.not_before_ms, "validity start")?,
                to_i64(publication.not_after_ms, "validity end")?,
                to_i64(entry.ts_ms(), "publication timestamp")?,
                publication.status as u8,
            ],
        )?;
    }
    Ok(())
}

fn append_merkle_leaf(conn: &Connection, seq: u64, leaf: Hash) -> Result<()> {
    let mut level = 0u32;
    let mut node_index = seq;
    let mut hash = leaf;
    conn.execute(
        "INSERT INTO merkle_nodes (level, node_index, hash) VALUES (0, ?1, ?2)",
        params![to_i64(node_index, "node index")?, hash.as_slice()],
    )?;

    while node_index % 2 == 1 {
        let left = read_node(conn, level, node_index - 1)?;
        hash = log::node_hash(&left, &hash);
        node_index /= 2;
        level += 1;
        conn.execute(
            "INSERT INTO merkle_nodes (level, node_index, hash) VALUES (?1, ?2, ?3)",
            params![
                i64::from(level),
                to_i64(node_index, "node index")?,
                hash.as_slice()
            ],
        )?;
    }
    Ok(())
}

pub(crate) fn load_state(conn: &Connection) -> Result<TreeState> {
    let (size, root, checkpoint): (i64, Vec<u8>, String) = conn.query_row(
        "SELECT tree_size, root, checkpoint_note FROM registry_state WHERE singleton = 1",
        [],
        |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
    )?;
    Ok(TreeState {
        size: nonnegative(size, "tree size")?,
        root: hash_from_blob(root, "state root")?,
        checkpoint,
    })
}

pub(crate) fn load_published_state(conn: &Connection) -> Result<PublishedState> {
    let (size, root, checkpoint, witnessed_at): (i64, Vec<u8>, String, Option<i64>) = conn
        .query_row(
            "SELECT tree_size, root, checkpoint_note, witnessed_at
             FROM published_state WHERE singleton = 1",
            [],
            |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?, row.get(3)?)),
        )?;
    Ok(PublishedState {
        state: TreeState {
            size: nonnegative(size, "published tree size")?,
            root: hash_from_blob(root, "published root")?,
            checkpoint,
        },
        witnessed_at: witnessed_at
            .map(|value| nonnegative(value, "published witness timestamp"))
            .transpose()?,
    })
}

pub(crate) fn current_binding(
    conn: &Connection,
    handle: &str,
) -> Result<Option<(u64, String, String)>> {
    let row: Option<(i64, String, String)> = conn
        .query_row(
            "SELECT seq, pubkey, subject FROM current_bindings WHERE handle = ?1",
            params![handle],
            |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
        )
        .optional()?;
    row.map(|(seq, pubkey, subject)| Ok((nonnegative(seq, "binding sequence")?, pubkey, subject)))
        .transpose()
}

pub(crate) fn binding_before(
    conn: &Connection,
    handle: &str,
    upper_bound: u64,
) -> Result<Option<(u64, String, String)>> {
    let row: Option<(i64, String, String)> = conn
        .query_row(
            "SELECT seq, pubkey, subject FROM handle_history
             WHERE handle = ?1 AND seq < ?2 ORDER BY seq DESC LIMIT 1",
            params![handle, to_i64(upper_bound, "published tree size")?],
            |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
        )
        .optional()?;
    row.map(|(seq, pubkey, subject)| Ok((nonnegative(seq, "binding sequence")?, pubkey, subject)))
        .transpose()
}

pub(crate) fn save_witness_receipt(
    conn: &mut Connection,
    witness_name: &str,
    size: u64,
    root: &Hash,
    witnessed_at: u64,
    receipt_json: &str,
) -> Result<bool> {
    if witness_name.is_empty()
        || witness_name.len() > 256
        || witness_name
            .bytes()
            .any(|byte| byte.is_ascii_control() || byte.is_ascii_whitespace())
        || witnessed_at == 0
        || receipt_json.is_empty()
        || receipt_json.len() > 64 * 1024
    {
        return Err(RegistryError::WitnessConflict);
    }
    let tx = conn.transaction_with_behavior(TransactionBehavior::Immediate)?;
    let committed = load_state(&tx)?;
    if size > committed.size || !log::hash_eq(&root_for_size(&tx, size)?, root) {
        return Err(RegistryError::WitnessConflict);
    }
    let previous: Option<(i64, Vec<u8>, i64, String)> = tx
        .query_row(
            "SELECT tree_size, root, witnessed_at, receipt_json
             FROM witness_receipts WHERE witness_name = ?1",
            params![witness_name],
            |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?, row.get(3)?)),
        )
        .optional()?;
    if let Some((old_size, old_root, old_witnessed_at, old_json)) = previous {
        let old_size = nonnegative(old_size, "witness receipt size")?;
        let old_root = hash_from_blob(old_root, "witness receipt root")?;
        let old_witnessed_at = nonnegative(old_witnessed_at, "witness receipt timestamp")?;
        if size < old_size || (size == old_size && !log::hash_eq(root, &old_root)) {
            return Err(RegistryError::WitnessConflict);
        }
        if size == old_size && witnessed_at < old_witnessed_at {
            tx.commit()?;
            return Ok(false);
        }
        if size == old_size && witnessed_at == old_witnessed_at {
            if receipt_json != old_json {
                return Err(RegistryError::WitnessConflict);
            }
            tx.commit()?;
            return Ok(false);
        }
    }
    tx.execute(
        "INSERT INTO witness_receipts
         (witness_name, tree_size, root, witnessed_at, receipt_json, updated_at_ms)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6)
         ON CONFLICT(witness_name) DO UPDATE SET
            tree_size = excluded.tree_size,
            root = excluded.root,
            witnessed_at = excluded.witnessed_at,
            receipt_json = excluded.receipt_json,
            updated_at_ms = excluded.updated_at_ms",
        params![
            witness_name,
            to_i64(size, "witness receipt size")?,
            root.as_slice(),
            to_i64(witnessed_at, "witness receipt timestamp")?,
            receipt_json,
            to_i64(now_ms(), "timestamp")?,
        ],
    )?;
    tx.commit()?;
    Ok(true)
}

pub(crate) fn witness_receipt_json(
    conn: &Connection,
    witness_name: &str,
) -> Result<Option<String>> {
    Ok(conn
        .query_row(
            "SELECT receipt_json FROM witness_receipts WHERE witness_name = ?1",
            params![witness_name],
            |row| row.get(0),
        )
        .optional()?)
}

pub(crate) fn witness_receipt_jsons_at(
    conn: &Connection,
    size: u64,
    root: &Hash,
) -> Result<Vec<String>> {
    let mut statement = conn.prepare(
        "SELECT receipt_json FROM witness_receipts
         WHERE tree_size = ?1 AND root = ?2 ORDER BY witness_name LIMIT 64",
    )?;
    let rows = statement.query_map(
        params![to_i64(size, "witness receipt size")?, root.as_slice()],
        |row| row.get::<_, String>(0),
    )?;
    let mut receipts = Vec::new();
    for row in rows {
        receipts.push(row?);
    }
    Ok(receipts)
}

pub(crate) fn promote_published_state(
    conn: &mut Connection,
    checkpoint: &Checkpoint,
    note: &str,
    witnessed_at: u64,
) -> Result<bool> {
    if witnessed_at == 0 || note.is_empty() || note.len() > 64 * 1024 {
        return Err(RegistryError::WitnessConflict);
    }
    let tx = conn.transaction_with_behavior(TransactionBehavior::Immediate)?;
    let committed = load_state(&tx)?;
    if checkpoint.size > committed.size
        || !log::hash_eq(&root_for_size(&tx, checkpoint.size)?, &checkpoint.root)
    {
        return Err(RegistryError::WitnessConflict);
    }
    let current = load_published_state(&tx)?;
    if checkpoint.size < current.state.size
        || (checkpoint.size == current.state.size
            && !log::hash_eq(&checkpoint.root, &current.state.root))
    {
        return Err(RegistryError::WitnessConflict);
    }
    if checkpoint.size == current.state.size
        && current.witnessed_at.is_some_and(|old| old >= witnessed_at)
    {
        tx.commit()?;
        return Ok(false);
    }
    tx.execute(
        "UPDATE published_state
         SET tree_size = ?1, root = ?2, checkpoint_note = ?3, witnessed_at = ?4
         WHERE singleton = 1",
        params![
            to_i64(checkpoint.size, "published tree size")?,
            checkpoint.root.as_slice(),
            note,
            to_i64(witnessed_at, "published witness timestamp")?,
        ],
    )?;
    tx.commit()?;
    Ok(true)
}

fn current_directory_mutation(
    conn: &Connection,
    endpoint: &str,
    loft_pubkey: &str,
) -> Result<Option<(u64, u64, String)>> {
    let row: Option<(i64, i64, String)> = conn
        .query_row(
            "SELECT entry_seq, mutation_sequence, mutation_kind
             FROM directory_mutations WHERE endpoint = ?1 AND loft_pubkey = ?2",
            params![endpoint, loft_pubkey],
            |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
        )
        .optional()?;
    row.map(|(entry_seq, mutation_sequence, mutation_kind)| {
        Ok((
            nonnegative(entry_seq, "directory entry sequence")?,
            nonnegative(mutation_sequence, "directory mutation sequence")?,
            mutation_kind,
        ))
    })
    .transpose()
}

fn update_directory_projection(conn: &Connection, entry: &LogEntry) -> Result<()> {
    let Some((endpoint, loft_pubkey, mutation_sequence)) = entry.authenticated_directory_mutation()
    else {
        return Ok(());
    };
    conn.execute(
        "INSERT INTO directory_mutations
         (endpoint, loft_pubkey, mutation_sequence, entry_seq, mutation_kind)
         VALUES (?1, ?2, ?3, ?4, ?5)
         ON CONFLICT(endpoint, loft_pubkey) DO UPDATE SET
            mutation_sequence = excluded.mutation_sequence,
            entry_seq = excluded.entry_seq,
            mutation_kind = excluded.mutation_kind",
        params![
            endpoint,
            loft_pubkey,
            to_i64(mutation_sequence, "directory mutation sequence")?,
            to_i64(entry.seq(), "entry sequence")?,
            entry.kind().as_str(),
        ],
    )?;
    Ok(())
}

pub(crate) fn entry_at(conn: &Connection, seq: u64) -> Result<Option<LogEntry>> {
    let row: Option<(i64, i64, String, String, i64, Vec<u8>)> = conn
        .query_row(
            "SELECT seq, version, entry_type, entry_json, ts_ms, leaf_hash
             FROM entries WHERE seq = ?1",
            params![to_i64(seq, "entry sequence")?],
            |row| {
                Ok((
                    row.get(0)?,
                    row.get(1)?,
                    row.get(2)?,
                    row.get(3)?,
                    row.get(4)?,
                    row.get(5)?,
                ))
            },
        )
        .optional()?;
    row.map(parse_entry_row).transpose()
}

pub(crate) fn entries_page(conn: &Connection, from: u64, limit: u64) -> Result<Vec<LogEntry>> {
    let limit = limit.clamp(1, MAX_PAGE_SIZE);
    let mut stmt = conn.prepare(
        "SELECT seq, version, entry_type, entry_json, ts_ms, leaf_hash
         FROM entries WHERE seq >= ?1 ORDER BY seq LIMIT ?2",
    )?;
    let rows = stmt.query_map(
        params![to_i64(from, "entry sequence")?, to_i64(limit, "page size")?],
        |row| {
            Ok((
                row.get(0)?,
                row.get(1)?,
                row.get(2)?,
                row.get(3)?,
                row.get(4)?,
                row.get(5)?,
            ))
        },
    )?;
    let mut out = Vec::with_capacity(limit as usize);
    for row in rows {
        out.push(parse_entry_row(row?)?);
    }
    Ok(out)
}

fn parse_entry_row(row: (i64, i64, String, String, i64, Vec<u8>)) -> Result<LogEntry> {
    let (seq, version, entry_type, json, ts_ms, stored_leaf) = row;
    let seq = nonnegative(seq, "entry sequence")?;
    let version = u8::try_from(version)
        .map_err(|_| RegistryError::CorruptStorage("entry version is out of range".into()))?;
    let ts_ms = nonnegative(ts_ms, "entry timestamp")?;
    let entry: LogEntry = serde_json::from_str(&json)
        .map_err(|e| RegistryError::CorruptStorage(format!("strict entry decode failed: {e}")))?;
    if entry.seq() != seq
        || entry.version() != version
        || entry.kind().as_str() != entry_type
        || entry.ts_ms() != ts_ms
    {
        return Err(RegistryError::CorruptStorage(
            "entry columns disagree with the canonical entry".into(),
        ));
    }
    let computed = log::leaf_hash(&entry.leaf_bytes().map_err(malformed_entry)?);
    if computed != hash_from_blob(stored_leaf, "stored leaf")? {
        return Err(RegistryError::CorruptStorage(
            "entry leaf hash does not match its canonical bytes".into(),
        ));
    }
    Ok(entry)
}

pub(crate) fn inclusion_proof(
    conn: &Connection,
    index: u64,
    size: u64,
) -> Result<Option<Vec<Hash>>> {
    let state = load_state(conn)?;
    if size > state.size || index >= size {
        return Ok(None);
    }
    Ok(Some(inclusion_path(conn, 0, size, index)?))
}

fn inclusion_path(conn: &Connection, start: u64, size: u64, index: u64) -> Result<Vec<Hash>> {
    if size <= 1 {
        return Ok(Vec::new());
    }
    let k = split_u64(size);
    if index < k {
        let mut proof = inclusion_path(conn, start, k, index)?;
        proof.push(tree_hash_range(conn, start + k, size - k)?);
        Ok(proof)
    } else {
        let mut proof = inclusion_path(conn, start + k, size - k, index - k)?;
        proof.push(tree_hash_range(conn, start, k)?);
        Ok(proof)
    }
}

pub(crate) fn consistency_proof(
    conn: &Connection,
    old: u64,
    new: u64,
) -> Result<Option<Vec<Hash>>> {
    let state = load_state(conn)?;
    if old == 0 || old > new || new > state.size {
        return Ok(None);
    }
    Ok(Some(consistency_path(conn, 0, new, old, true)?))
}

fn consistency_path(
    conn: &Connection,
    start: u64,
    size: u64,
    old: u64,
    is_root: bool,
) -> Result<Vec<Hash>> {
    if old == size {
        return if is_root {
            Ok(Vec::new())
        } else {
            Ok(vec![tree_hash_range(conn, start, size)?])
        };
    }
    let k = split_u64(size);
    if old <= k {
        let mut proof = consistency_path(conn, start, k, old, is_root)?;
        proof.push(tree_hash_range(conn, start + k, size - k)?);
        Ok(proof)
    } else {
        let mut proof = consistency_path(conn, start + k, size - k, old - k, false)?;
        proof.push(tree_hash_range(conn, start, k)?);
        Ok(proof)
    }
}

pub(crate) fn root_for_size(conn: &Connection, size: u64) -> Result<Hash> {
    if size == 0 {
        return Ok(log::empty_root());
    }
    tree_hash_range(conn, 0, size)
}

fn tree_hash_range(conn: &Connection, start: u64, size: u64) -> Result<Hash> {
    if size == 0 {
        return Ok(log::empty_root());
    }
    if size.is_power_of_two() && start % size == 0 {
        let level = size.trailing_zeros();
        return read_node(conn, level, start / size);
    }
    let k = split_u64(size);
    let left = tree_hash_range(conn, start, k)?;
    let right = tree_hash_range(conn, start + k, size - k)?;
    Ok(log::node_hash(&left, &right))
}

fn read_node(conn: &Connection, level: u32, index: u64) -> Result<Hash> {
    let blob: Vec<u8> = conn
        .query_row(
            "SELECT hash FROM merkle_nodes WHERE level = ?1 AND node_index = ?2",
            params![i64::from(level), to_i64(index, "node index")?],
            |row| row.get(0),
        )
        .map_err(|error| match error {
            rusqlite::Error::QueryReturnedNoRows => RegistryError::CorruptStorage(format!(
                "missing Merkle node at level {level}, index {index}"
            )),
            other => RegistryError::Storage(other),
        })?;
    hash_from_blob(blob, "Merkle node")
}

pub(crate) fn compliance_sequences(
    conn: &Connection,
    key_id: Option<&ComplianceKeyId>,
    purpose: Option<u8>,
    jurisdiction: Option<u8>,
    at_ms: Option<u64>,
    include_inactive: bool,
    upper_bound: u64,
) -> Result<Vec<u64>> {
    let at_ms = at_ms.unwrap_or_else(now_ms);
    if let Some(key_id) = key_id {
        let encoded = key_id
            .encode()
            .map_err(|e| RegistryError::MalformedEntry(e.to_string()))?;
        let seq: Option<i64> = conn
            .query_row(
                "SELECT ck.seq FROM compliance_keys ck
                 WHERE ck.key_id = ?1
                   AND ck.published_at_ms <= ?3
                   AND ck.seq < ?4
                   AND ck.seq = (
                       SELECT MAX(newer.seq) FROM compliance_keys newer
                       WHERE newer.key_id = ck.key_id AND newer.published_at_ms <= ?3
                         AND newer.seq < ?4
                   )
                   AND (?2 OR (ck.status = 1
                       AND ck.not_before_ms <= ?3 AND ck.not_after_ms > ?3))",
                params![
                    encoded.as_slice(),
                    include_inactive,
                    to_i64(at_ms, "historical lookup time")?,
                    to_i64(upper_bound, "published tree size")?
                ],
                |row| row.get(0),
            )
            .optional()?;
        return seq
            .map(|value| Ok(vec![nonnegative(value, "compliance sequence")?]))
            .unwrap_or_else(|| Ok(Vec::new()));
    }

    let purpose = purpose.map(i64::from);
    let jurisdiction = jurisdiction.map(i64::from);
    let mut stmt = conn.prepare(
        "SELECT ck.seq FROM compliance_keys ck
         WHERE (?1 IS NULL OR ck.purpose = ?1)
           AND (?2 IS NULL OR ck.jurisdiction = ?2)
           AND ck.published_at_ms <= ?4
           AND ck.seq < ?5
           AND ck.seq = (
               SELECT MAX(newer.seq) FROM compliance_keys newer
               WHERE newer.key_id = ck.key_id AND newer.published_at_ms <= ?4
                 AND newer.seq < ?5
           )
           AND (?3 OR (ck.status = 1
               AND ck.not_before_ms <= ?4 AND ck.not_after_ms > ?4))
         ORDER BY ck.epoch_start_ms, ck.generation, ck.seq
         LIMIT 1000",
    )?;
    let rows = stmt.query_map(
        params![
            purpose,
            jurisdiction,
            include_inactive,
            to_i64(at_ms, "historical lookup time")?,
            to_i64(upper_bound, "published tree size")?
        ],
        |row| row.get::<_, i64>(0),
    )?;
    let mut out = Vec::new();
    for row in rows {
        out.push(nonnegative(row?, "compliance sequence")?);
    }
    Ok(out)
}

pub(crate) fn insert_identity_challenge(
    conn: &mut Connection,
    provider: &str,
    challenge_hash: &Hash,
    handle: &str,
    pubkey: &[u8; 32],
    pkce_challenge: Option<&str>,
    expires_at_ms: u64,
) -> Result<()> {
    let tx = conn.transaction_with_behavior(TransactionBehavior::Immediate)?;
    let now = now_ms();
    let retained_result_cutoff = now.saturating_sub(IDENTITY_CHALLENGE_RESULT_RETENTION_MS);
    tx.execute(
        "DELETE FROM identity_challenges
         WHERE challenge_hash IN (
             SELECT challenge_hash FROM identity_challenges
             WHERE (consumed_at_ms IS NULL AND expires_at_ms < ?1)
                OR (consumed_at_ms IS NOT NULL AND consumed_at_ms < ?2)
             ORDER BY expires_at_ms LIMIT 256
         )",
        params![
            to_i64(now, "timestamp")?,
            to_i64(retained_result_cutoff, "challenge result retention cutoff")?
        ],
    )?;
    let outstanding: i64 = tx.query_row(
        "SELECT COUNT(*) FROM identity_challenges
         WHERE consumed_at_ms IS NULL AND expires_at_ms >= ?1",
        params![to_i64(now, "timestamp")?],
        |row| row.get(0),
    )?;
    if outstanding >= 100_000 {
        return Err(RegistryError::ProofRejected(
            "identity challenge capacity is temporarily exhausted".into(),
        ));
    }
    tx.execute(
        "INSERT INTO identity_challenges
         (challenge_hash, provider, handle, pubkey, pkce_challenge, expires_at_ms,
          consumed_at_ms, binding_seq)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, NULL, NULL)",
        params![
            challenge_hash.as_slice(),
            provider,
            handle,
            pubkey.as_slice(),
            pkce_challenge,
            to_i64(expires_at_ms, "challenge expiry")?
        ],
    )?;
    tx.commit()?;
    Ok(())
}

fn read_identity_challenge(
    conn: &Connection,
    challenge_hash: &Hash,
) -> Result<Option<StoredIdentityChallenge>> {
    conn.query_row(
        "SELECT provider, handle, pubkey, pkce_challenge, expires_at_ms, consumed_at_ms,
                binding_seq
         FROM identity_challenges WHERE challenge_hash = ?1",
        params![challenge_hash.as_slice()],
        |row| {
            Ok(StoredIdentityChallenge {
                provider: row.get(0)?,
                handle: row.get(1)?,
                pubkey: row.get(2)?,
                pkce_challenge: row.get(3)?,
                expires_at: row.get(4)?,
                consumed_at: row.get(5)?,
                binding_seq: row.get(6)?,
            })
        },
    )
    .optional()
    .map_err(RegistryError::from)
}

fn validate_identity_challenge_fields(
    stored: &StoredIdentityChallenge,
    provider: &str,
    handle: &str,
    pubkey: &[u8; 32],
    expected_pkce_challenge: Option<&str>,
) -> Result<()> {
    if stored.provider != provider
        || stored.handle != handle
        || stored.pubkey.as_slice().ct_eq(pubkey).unwrap_u8() != 1
        || !secret_option_eq(stored.pkce_challenge.as_deref(), expected_pkce_challenge)
    {
        return Err(RegistryError::ProofRejected(
            "identity challenge is invalid, expired, or already used".into(),
        ));
    }
    Ok(())
}

pub(crate) struct IdentityChallengeCommit<'a> {
    pub provider: &'a str,
    pub challenge_hash: &'a Hash,
    pub pubkey: &'a [u8; 32],
    pub expected_pkce_challenge: Option<&'a str>,
}

/// Atomically consume a one-time identity challenge and append (or recover) its exact binding.
/// A timeout after SQLite commits can therefore be retried without burning authorization or
/// creating a second leaf: the challenge row names the committed sequence in the same transaction.
pub(crate) fn commit_handle_binding(
    conn: &mut Connection,
    origin: &str,
    signing_key: &SigningKey,
    challenge: Option<IdentityChallengeCommit<'_>>,
    request: HandleAppendRequest<'_>,
) -> Result<HandleAppend> {
    let tx = conn.transaction_with_behavior(TransactionBehavior::Immediate)?;
    let stored = challenge
        .as_ref()
        .map(|challenge| read_identity_challenge(&tx, challenge.challenge_hash))
        .transpose()?
        .flatten();

    if let Some(challenge) = &challenge {
        if decode_hex(request.pubkey, 32)
            .is_none_or(|bytes| bytes.as_slice().ct_eq(challenge.pubkey).unwrap_u8() != 1)
        {
            return Err(RegistryError::CorruptStorage(
                "challenge commit public key encoding disagrees with its bound key".into(),
            ));
        }
        let Some(stored) = &stored else {
            return Err(RegistryError::ProofRejected(
                "identity challenge is unknown or expired".into(),
            ));
        };
        validate_identity_challenge_fields(
            stored,
            challenge.provider,
            request.handle,
            challenge.pubkey,
            challenge.expected_pkce_challenge,
        )?;

        if stored.consumed_at.is_some() {
            let seq = nonnegative(
                stored.binding_seq.ok_or_else(|| {
                    RegistryError::CorruptStorage(
                        "consumed identity challenge has no binding result".into(),
                    )
                })?,
                "challenge binding sequence",
            )?;
            validate_recovered_handle_binding(&tx, seq, request)?;
            tx.commit()?;
            return Ok(HandleAppend {
                seq,
                appended: false,
            });
        }
        if nonnegative(stored.expires_at, "challenge expiry")? < now_ms() {
            return Err(RegistryError::ProofRejected(
                "identity challenge is invalid, expired, or already used".into(),
            ));
        }
    }

    let appended = append_handle_in_transaction(&tx, origin, signing_key, request)?;
    if let Some(challenge) = challenge {
        let changed = tx.execute(
            "UPDATE identity_challenges
             SET consumed_at_ms = ?1, binding_seq = ?2
             WHERE challenge_hash = ?3 AND consumed_at_ms IS NULL AND binding_seq IS NULL",
            params![
                to_i64(now_ms(), "timestamp")?,
                to_i64(appended.seq, "challenge binding sequence")?,
                challenge.challenge_hash.as_slice()
            ],
        )?;
        if changed != 1 {
            return Err(RegistryError::ProofRejected(
                "identity challenge is invalid, expired, or already used".into(),
            ));
        }
    }
    tx.commit()?;
    Ok(appended)
}

fn validate_recovered_handle_binding(
    conn: &Connection,
    seq: u64,
    request: HandleAppendRequest<'_>,
) -> Result<()> {
    let entry = entry_at(conn, seq)?.ok_or_else(|| {
        RegistryError::CorruptStorage("challenge result points at a missing entry".into())
    })?;
    let expected_kind = matches!(
        (&entry, request.mode),
        (LogEntry::HandleClaim(_), HandleAppendMode::Register)
            | (LogEntry::HandleRotation(_), HandleAppendMode::Rotate)
    );
    let expected_binding = entry
        .handle_binding()
        .is_some_and(|(handle, pubkey, subject)| {
            handle == request.handle && pubkey == request.pubkey && subject == request.subject
        });
    if !expected_kind || !expected_binding {
        return Err(RegistryError::ProofRejected(
            "identity challenge was already used for another binding".into(),
        ));
    }
    Ok(())
}

pub(crate) fn validate_identity_challenge(
    conn: &Connection,
    provider: &str,
    challenge_hash: &Hash,
    handle: &str,
    pubkey: &[u8; 32],
    expected_pkce_challenge: Option<&str>,
) -> Result<Option<u64>> {
    let Some(stored) = read_identity_challenge(conn, challenge_hash)? else {
        return Err(RegistryError::ProofRejected(
            "identity challenge is unknown or expired".into(),
        ));
    };
    validate_identity_challenge_fields(&stored, provider, handle, pubkey, expected_pkce_challenge)?;
    if stored.consumed_at.is_none()
        && nonnegative(stored.expires_at, "challenge expiry")? < now_ms()
    {
        return Err(RegistryError::ProofRejected(
            "identity challenge is invalid, expired, or already used".into(),
        ));
    }
    if stored.consumed_at.is_some() && stored.binding_seq.is_none() {
        return Err(RegistryError::CorruptStorage(
            "consumed identity challenge has no binding result".into(),
        ));
    }
    stored
        .binding_seq
        .map(|seq| nonnegative(seq, "challenge binding sequence"))
        .transpose()
}

pub(crate) fn verify_storage(
    conn: &Connection,
    origin: &str,
    verifying_key: &ed25519_dalek::VerifyingKey,
) -> Result<()> {
    verify_storage_without_global_admission(conn, origin, verifying_key)?;
    load_global_binding_admission(conn)?;
    Ok(())
}

fn verify_storage_without_global_admission(
    conn: &Connection,
    origin: &str,
    verifying_key: &ed25519_dalek::VerifyingKey,
) -> Result<()> {
    let state = load_state(conn)?;
    let (count, max_seq): (i64, Option<i64>) =
        conn.query_row("SELECT COUNT(*), MAX(seq) FROM entries", [], |row| {
            Ok((row.get(0)?, row.get(1)?))
        })?;
    let count = nonnegative(count, "entry count")?;
    if count != state.size || max_seq.map(|v| v as u64 + 1).unwrap_or(0) != state.size {
        return Err(RegistryError::CorruptStorage(
            "entries are not a contiguous sequence matching tree_size".into(),
        ));
    }

    let computed_root = root_for_size(conn, state.size)?;
    if !log::hash_eq(&computed_root, &state.root) {
        return Err(RegistryError::CorruptStorage(
            "persisted Merkle nodes do not produce the stored root".into(),
        ));
    }
    let checkpoint = Checkpoint::verify(&state.checkpoint, verifying_key)?;
    if checkpoint.origin != origin
        || checkpoint.size != state.size
        || !log::hash_eq(&checkpoint.root, &state.root)
    {
        return Err(RegistryError::CorruptStorage(
            "latest signed checkpoint disagrees with registry state".into(),
        ));
    }

    let persisted: Option<(Vec<u8>, String)> = conn
        .query_row(
            "SELECT root, note FROM checkpoints WHERE tree_size = ?1",
            params![to_i64(state.size, "tree size")?],
            |row| Ok((row.get(0)?, row.get(1)?)),
        )
        .optional()?;
    let Some((persisted_root, persisted_note)) = persisted else {
        return Err(RegistryError::CorruptStorage(
            "latest checkpoint is not persisted in checkpoint history".into(),
        ));
    };
    if !log::hash_eq(
        &hash_from_blob(persisted_root, "checkpoint root")?,
        &state.root,
    ) || persisted_note != state.checkpoint
    {
        return Err(RegistryError::CorruptStorage(
            "checkpoint history disagrees with registry state".into(),
        ));
    }

    let published = load_published_state(conn)?;
    if published.state.size > state.size
        || !log::hash_eq(
            &root_for_size(conn, published.state.size)?,
            &published.state.root,
        )
    {
        return Err(RegistryError::CorruptStorage(
            "published checkpoint is outside the committed log".into(),
        ));
    }
    let published_checkpoint = Checkpoint::verify(&published.state.checkpoint, verifying_key)?;
    if published_checkpoint.origin != origin
        || published_checkpoint.size != published.state.size
        || !log::hash_eq(&published_checkpoint.root, &published.state.root)
    {
        return Err(RegistryError::CorruptStorage(
            "published checkpoint note disagrees with published state".into(),
        ));
    }
    let published_root: Option<Vec<u8>> = conn
        .query_row(
            "SELECT root FROM checkpoints WHERE tree_size = ?1",
            params![to_i64(published.state.size, "published tree size")?],
            |row| row.get(0),
        )
        .optional()?;
    if published_root
        .map(|root| hash_from_blob(root, "published checkpoint history root"))
        .transpose()?
        .as_ref()
        .is_none_or(|root| !log::hash_eq(root, &published.state.root))
    {
        return Err(RegistryError::CorruptStorage(
            "published checkpoint is absent from checkpoint history".into(),
        ));
    }

    let mut statement = conn.prepare(
        "SELECT witness_name, tree_size, root, witnessed_at, receipt_json
         FROM witness_receipts ORDER BY witness_name",
    )?;
    let rows = statement.query_map([], |row| {
        Ok((
            row.get::<_, String>(0)?,
            row.get::<_, i64>(1)?,
            row.get::<_, Vec<u8>>(2)?,
            row.get::<_, i64>(3)?,
            row.get::<_, String>(4)?,
        ))
    })?;
    for row in rows {
        let (name, size, root, witnessed_at, json) = row?;
        let size = nonnegative(size, "witness receipt size")?;
        let root = hash_from_blob(root, "witness receipt root")?;
        let witnessed_at = nonnegative(witnessed_at, "witness receipt timestamp")?;
        let receipt: WitnessReceipt = serde_json::from_str(&json).map_err(|_| {
            RegistryError::CorruptStorage("persisted witness receipt JSON is malformed".into())
        })?;
        let checkpoint = Checkpoint::verify(receipt.note(), verifying_key)?;
        if name != receipt.witness_name()
            || size != receipt.size()
            || witnessed_at == 0
            || witnessed_at != receipt.witnessed_at()
            || !log::hash_eq(&root, receipt.root())
            || checkpoint.origin != origin
            || checkpoint.size != size
            || !log::hash_eq(&checkpoint.root, &root)
            || size > state.size
            || !log::hash_eq(&root_for_size(conn, size)?, &root)
        {
            return Err(RegistryError::CorruptStorage(
                "persisted witness receipt disagrees with the local log".into(),
            ));
        }
    }
    drop(statement);

    verify_full_authority_replay(conn, &state)?;

    let previous: Option<(i64, Vec<u8>, String)> = conn
        .query_row(
            "SELECT tree_size, root, note FROM checkpoints
             WHERE tree_size < ?1 ORDER BY tree_size DESC LIMIT 1",
            params![to_i64(state.size, "tree size")?],
            |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
        )
        .optional()?;
    if let Some((old_size, old_root, old_note)) = previous {
        let old_size = nonnegative(old_size, "checkpoint size")?;
        let old_root = hash_from_blob(old_root, "checkpoint root")?;
        let old_checkpoint = Checkpoint::verify(&old_note, verifying_key)?;
        if old_checkpoint.origin != origin
            || old_checkpoint.size != old_size
            || !log::hash_eq(&old_checkpoint.root, &old_root)
        {
            return Err(RegistryError::CorruptStorage(
                "previous signed checkpoint disagrees with checkpoint history".into(),
            ));
        }
        if old_size > 0 {
            let proof = consistency_proof(conn, old_size, state.size)?.ok_or_else(|| {
                RegistryError::CorruptStorage(
                    "cannot construct checkpoint consistency proof".into(),
                )
            })?;
            if !log::verify_consistency(old_size, &old_root, state.size, &state.root, &proof) {
                return Err(RegistryError::CorruptStorage(
                    "persisted checkpoints are not mutually consistent".into(),
                ));
            }
        }
    }
    Ok(())
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct ComplianceProjection {
    key_id: Vec<u8>,
    public_key: Vec<u8>,
    purpose: i64,
    jurisdiction: i64,
    authority: Vec<u8>,
    epoch_start_ms: i64,
    generation: i64,
    not_before_ms: i64,
    not_after_ms: i64,
    published_at_ms: i64,
    status: i64,
}

/// Replay every signed leaf at startup. Besides binding every canonical payload to its level-zero
/// Merkle node, this reconstructs every table that can authorize a mutation or a public response.
/// A valid signed root is therefore insufficient if any mutable projection has been deleted,
/// inserted, or rewritten offline.
fn verify_full_authority_replay(conn: &Connection, state: &TreeState) -> Result<()> {
    let mut history_statement =
        conn.prepare("SELECT seq, handle, pubkey, subject FROM handle_history ORDER BY seq")?;
    let mut history_rows = history_statement.query([])?;
    let mut compliance_statement = conn.prepare(
        "SELECT seq, key_id, public_key, purpose, jurisdiction, authority, epoch_start_ms,
                generation, not_before_ms, not_after_ms, published_at_ms, status
         FROM compliance_keys ORDER BY seq",
    )?;
    let mut compliance_rows = compliance_statement.query([])?;
    let mut expected_directory = HashMap::<(String, String), (u64, u64, String)>::new();
    let mut from = 0;
    while from < state.size {
        let page = entries_page(conn, from, MAX_PAGE_SIZE)?;
        if page.is_empty() {
            return Err(RegistryError::CorruptStorage(
                "entry audit encountered a gap".into(),
            ));
        }
        for entry in page {
            if entry.seq() != from {
                return Err(RegistryError::CorruptStorage(
                    "startup replay encountered a non-contiguous sequence".into(),
                ));
            }
            let leaf = log::leaf_hash(&entry.leaf_bytes().map_err(malformed_entry)?);
            if read_node(conn, 0, from)? != leaf {
                return Err(RegistryError::CorruptStorage(format!(
                    "entry {from} payload disagrees with its Merkle leaf"
                )));
            }

            if let Some((handle, pubkey, subject)) = entry.handle_binding() {
                let row = history_rows.next()?.ok_or_else(|| {
                    RegistryError::CorruptStorage(
                        "handle history projection is missing a logged binding".into(),
                    )
                })?;
                let actual = (
                    nonnegative(row.get(0)?, "handle history sequence")?,
                    row.get::<_, String>(1)?,
                    row.get::<_, String>(2)?,
                    row.get::<_, String>(3)?,
                );
                let expected = (
                    entry.seq(),
                    handle.to_owned(),
                    pubkey.to_owned(),
                    subject.to_owned(),
                );
                if actual != expected {
                    return Err(RegistryError::CorruptStorage(
                        "handle history projection disagrees with the append-only log".into(),
                    ));
                }
            }
            if let Some(publication) = entry.compliance_publication() {
                let key_id = publication
                    .key_id
                    .encode()
                    .map_err(|error| RegistryError::CorruptStorage(error.to_string()))?;
                let public_key = decode_hex(&publication.public_key, 32).ok_or_else(|| {
                    RegistryError::CorruptStorage(
                        "logged compliance public key is not canonical".into(),
                    )
                })?;
                let expected = ComplianceProjection {
                    key_id: key_id.to_vec(),
                    public_key,
                    purpose: i64::from(u8::from(publication.key_id.purpose)),
                    jurisdiction: i64::from(u8::from(publication.key_id.jurisdiction)),
                    authority: publication.key_id.authority.to_vec(),
                    epoch_start_ms: to_i64(
                        publication.key_id.epoch_start_ms,
                        "compliance key epoch",
                    )?,
                    generation: i64::from(publication.key_id.generation),
                    not_before_ms: to_i64(publication.not_before_ms, "compliance validity start")?,
                    not_after_ms: to_i64(publication.not_after_ms, "compliance validity end")?,
                    published_at_ms: to_i64(entry.ts_ms(), "compliance publication timestamp")?,
                    status: i64::from(publication.status as u8),
                };
                let row = compliance_rows.next()?.ok_or_else(|| {
                    RegistryError::CorruptStorage(
                        "compliance-key projection is missing a logged publication".into(),
                    )
                })?;
                let actual_seq = nonnegative(row.get(0)?, "compliance projection sequence")?;
                let actual = ComplianceProjection {
                    key_id: row.get(1)?,
                    public_key: row.get(2)?,
                    purpose: row.get(3)?,
                    jurisdiction: row.get(4)?,
                    authority: row.get(5)?,
                    epoch_start_ms: row.get(6)?,
                    generation: row.get(7)?,
                    not_before_ms: row.get(8)?,
                    not_after_ms: row.get(9)?,
                    published_at_ms: row.get(10)?,
                    status: row.get(11)?,
                };
                if actual_seq != entry.seq() || actual != expected {
                    return Err(RegistryError::CorruptStorage(
                        "compliance-key projection disagrees with the append-only log".into(),
                    ));
                }
            }
            if matches!(
                entry.kind(),
                EntryKind::DirectoryAdd | EntryKind::DirectoryRemove
            ) {
                let (endpoint, loft_pubkey, mutation_sequence) =
                    entry.authenticated_directory_mutation().ok_or_else(|| {
                        RegistryError::CorruptStorage(
                            "directory log contains an unauthenticated mutation".into(),
                        )
                    })?;
                expected_directory.insert(
                    (endpoint.to_owned(), loft_pubkey.to_owned()),
                    (
                        mutation_sequence,
                        entry.seq(),
                        entry.kind().as_str().to_owned(),
                    ),
                );
            }
            from += 1;
        }
    }
    if !log::hash_eq(&root_for_size(conn, state.size)?, &state.root) {
        return Err(RegistryError::CorruptStorage(
            "audited Merkle root disagrees with registry state".into(),
        ));
    }

    if history_rows.next()?.is_some() {
        return Err(RegistryError::CorruptStorage(
            "handle history projection contains an unlogged binding".into(),
        ));
    }
    if compliance_rows.next()?.is_some() {
        return Err(RegistryError::CorruptStorage(
            "compliance-key projection contains an unlogged publication".into(),
        ));
    }
    drop(history_rows);
    drop(history_statement);
    drop(compliance_rows);
    drop(compliance_statement);

    verify_current_bindings_projection(conn)?;
    verify_directory_projection(conn, expected_directory)?;
    Ok(())
}

fn verify_current_bindings_projection(conn: &Connection) -> Result<()> {
    // `handle_history` was compared leaf-for-leaf above. Derive its latest row per handle using the
    // canonical `(handle, seq DESC)` index, then prove exact two-way set equality without retaining
    // the complete handle population in process memory.
    let mismatches: i64 = conn.query_row(
        "SELECT
            (SELECT COUNT(*) FROM current_bindings AS current
             WHERE NOT EXISTS (
                 SELECT 1 FROM handle_history AS history
                 WHERE history.handle = current.handle
                   AND history.pubkey = current.pubkey
                   AND history.subject = current.subject
                   AND history.seq = current.seq
                   AND NOT EXISTS (
                       SELECT 1 FROM handle_history AS newer
                       WHERE newer.handle = history.handle AND newer.seq > history.seq
                   )
             ))
          + (SELECT COUNT(*) FROM handle_history AS history
             WHERE NOT EXISTS (
                       SELECT 1 FROM handle_history AS newer
                       WHERE newer.handle = history.handle AND newer.seq > history.seq
                   )
               AND NOT EXISTS (
                       SELECT 1 FROM current_bindings AS current
                       WHERE current.handle = history.handle
                         AND current.pubkey = history.pubkey
                         AND current.subject = history.subject
                         AND current.seq = history.seq
                   ))",
        [],
        |row| row.get(0),
    )?;
    if mismatches != 0 {
        return Err(RegistryError::CorruptStorage(
            "current binding projection disagrees with the append-only log".into(),
        ));
    }
    Ok(())
}

fn verify_directory_projection(
    conn: &Connection,
    expected: HashMap<(String, String), (u64, u64, String)>,
) -> Result<()> {
    let mut actual = HashMap::new();
    let mut statement = conn.prepare(
        "SELECT endpoint, loft_pubkey, mutation_sequence, entry_seq, mutation_kind
         FROM directory_mutations",
    )?;
    let rows = statement.query_map([], |row| {
        Ok((
            row.get::<_, String>(0)?,
            row.get::<_, String>(1)?,
            row.get::<_, i64>(2)?,
            row.get::<_, i64>(3)?,
            row.get::<_, String>(4)?,
        ))
    })?;
    for row in rows {
        let (endpoint, loft_pubkey, mutation_sequence, entry_seq, mutation_kind) = row?;
        actual.insert(
            (endpoint, loft_pubkey),
            (
                nonnegative(mutation_sequence, "directory mutation sequence")?,
                nonnegative(entry_seq, "directory entry sequence")?,
                mutation_kind,
            ),
        );
    }
    if actual != expected {
        return Err(RegistryError::CorruptStorage(
            "directory mutation projection disagrees with the append-only log".into(),
        ));
    }
    Ok(())
}

/// Full payload/leaf and authority-projection audit for operator diagnostics. Startup runs the same
/// replay, while this entry point remains useful for an explicit live audit.
pub(crate) fn audit_all_entries(conn: &Connection) -> Result<u64> {
    let state = load_state(conn)?;
    verify_full_authority_replay(conn, &state)?;
    Ok(state.size)
}

fn validate_legacy_columns(conn: &Connection) -> Result<()> {
    let mut stmt = conn.prepare("PRAGMA table_info(entries)")?;
    let rows = stmt.query_map([], |row| row.get::<_, String>(1))?;
    let mut columns = Vec::new();
    for row in rows {
        columns.push(row?);
    }
    let expected = ["idx", "kind", "handle", "pubkey", "subject", "timestamp"];
    if columns != expected {
        return Err(RegistryError::MigrationRequired(format!(
            "legacy entries schema is not the expected released shape: {columns:?}"
        )));
    }
    Ok(())
}

fn read_legacy_entries(conn: &Connection) -> Result<Vec<LogEntry>> {
    let mut stmt = conn.prepare(
        "SELECT idx, kind, handle, pubkey, subject, timestamp FROM entries ORDER BY idx",
    )?;
    let rows = stmt.query_map([], |row| {
        Ok((
            row.get::<_, i64>(0)?,
            row.get::<_, String>(1)?,
            row.get::<_, String>(2)?,
            row.get::<_, String>(3)?,
            row.get::<_, String>(4)?,
            row.get::<_, i64>(5)?,
        ))
    })?;
    let mut entries = Vec::new();
    for row in rows {
        let (idx, kind, handle, pubkey, subject, timestamp) = row?;
        let seq = nonnegative(idx, "legacy sequence")?;
        if seq != entries.len() as u64 {
            return Err(RegistryError::MigrationRequired(
                "legacy entries are not contiguous from sequence zero".into(),
            ));
        }
        let kind = match kind.as_str() {
            "handle_bind" => EntryKind::HandleClaim,
            "handle_rotate" => EntryKind::HandleRotation,
            other => {
                return Err(RegistryError::MigrationRequired(format!(
                    "unknown legacy entry kind {other:?}; refusing reinterpretation"
                )));
            }
        };
        entries.push(
            LogEntry::legacy_handle(
                kind,
                seq,
                handle,
                pubkey,
                subject,
                nonnegative(timestamp, "legacy timestamp")?,
            )
            .map_err(malformed_entry)?,
        );
    }
    Ok(entries)
}

fn table_exists(conn: &Connection, table: &str) -> Result<bool> {
    Ok(conn
        .query_row(
            "SELECT 1 FROM sqlite_master WHERE type = 'table' AND name = ?1",
            params![table],
            |_| Ok(()),
        )
        .optional()?
        .is_some())
}

fn user_version(conn: &Connection) -> Result<u32> {
    let version: i64 = conn.pragma_query_value(None, "user_version", |row| row.get(0))?;
    u32::try_from(version)
        .map_err(|_| RegistryError::CorruptStorage("negative or oversized user_version".into()))
}

fn split_u64(n: u64) -> u64 {
    debug_assert!(n > 1);
    1u64 << (63 - (n - 1).leading_zeros())
}

fn hash_from_blob(blob: Vec<u8>, description: &str) -> Result<Hash> {
    blob.try_into().map_err(|_| {
        RegistryError::CorruptStorage(format!("{description} is not exactly 32 bytes"))
    })
}

fn nonnegative(value: i64, description: &str) -> Result<u64> {
    u64::try_from(value).map_err(|_| {
        RegistryError::CorruptStorage(format!("{description} is negative or out of range"))
    })
}

fn to_i64(value: u64, description: &str) -> Result<i64> {
    i64::try_from(value)
        .map_err(|_| RegistryError::MalformedEntry(format!("{description} exceeds SQLite range")))
}

fn decode_hex(input: &str, len: usize) -> Option<Vec<u8>> {
    if input.len() != len * 2 {
        return None;
    }
    input
        .as_bytes()
        .chunks_exact(2)
        .map(|chunk| u8::from_str_radix(std::str::from_utf8(chunk).ok()?, 16).ok())
        .collect()
}

fn secret_option_eq(left: Option<&str>, right: Option<&str>) -> bool {
    match (left, right) {
        (Some(left), Some(right)) => bool::from(left.as_bytes().ct_eq(right.as_bytes())),
        (None, None) => true,
        _ => false,
    }
}

fn malformed_entry(error: crate::entry::EntryError) -> RegistryError {
    RegistryError::MalformedEntry(error.to_string())
}

fn now_ms() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|duration| duration.as_millis().min(u128::from(u64::MAX)) as u64)
        .unwrap_or(0)
}

#[cfg(test)]
mod tests {
    use super::*;
    use ed25519_dalek::Signer;
    use pigeonpost_compliance_format::{CompliancePurpose, Jurisdiction, TRACE_EPOCH_DURATION_MS};

    use crate::entry::directory_add_claim_payload;

    const INTEGRITY_ORIGIN: &str = "pigeonpost.test/registry-storage-integrity";

    fn empty_predecessor(version: u32) -> (Connection, SigningKey) {
        let signing_key = SigningKey::from_bytes(&[91; 32]);
        let mut conn = Connection::open_in_memory().unwrap();
        initialize(
            &mut conn,
            INTEGRITY_ORIGIN,
            &signing_key,
            LegacyAuthorization::Refuse,
        )
        .unwrap();
        conn.execute("DELETE FROM schema_migrations", []).unwrap();
        conn.execute(
            "INSERT INTO schema_migrations
             (version, applied_at_ms, source_schema, authorization_checkpoint)
             VALUES (?1, 1, 'test-predecessor', NULL)",
            params![version],
        )
        .unwrap();
        shape_reference_schema(&conn, version).unwrap();
        conn.pragma_update(None, "user_version", version).unwrap();
        verify_canonical_schema(&conn, version).unwrap();
        (conn, signing_key)
    }

    fn populated_integrity_store() -> (Connection, SigningKey) {
        let signing_key = SigningKey::from_bytes(&[92; 32]);
        let mut conn = Connection::open_in_memory().unwrap();
        initialize(
            &mut conn,
            INTEGRITY_ORIGIN,
            &signing_key,
            LegacyAuthorization::Refuse,
        )
        .unwrap();
        append_handle(
            &mut conn,
            INTEGRITY_ORIGIN,
            &signing_key,
            HandleAppendRequest {
                handle: "/github/alice",
                pubkey: &"11".repeat(32),
                subject: "github:account-1",
                ts_ms: 1,
                mode: HandleAppendMode::Register,
            },
        )
        .unwrap();

        append_compliance_key(
            &mut conn,
            INTEGRITY_ORIGIN,
            &signing_key,
            ComplianceKeyPublish {
                key_id: ComplianceKeyId::new(
                    CompliancePurpose::NetworkTrace,
                    Jurisdiction::Test,
                    [7; 32],
                    0,
                    1,
                ),
                public_key: "22".repeat(32),
                not_before_ms: 0,
                not_after_ms: TRACE_EPOCH_DURATION_MS,
                status: ComplianceKeyStatus::Active,
            },
            2,
        )
        .unwrap();

        let loft_key = SigningKey::from_bytes(&[93; 32]);
        let endpoint = "https://loft.example";
        let loft_pubkey = crate::registry::hex(loft_key.verifying_key().as_bytes());
        let claim =
            directory_add_claim_payload(endpoint, &loft_pubkey, None, 1, 30, true, 0, 65_536, 1)
                .unwrap();
        let mutation = DirectoryAdd::authenticated(
            endpoint.into(),
            loft_pubkey,
            None,
            1,
            30,
            true,
            0,
            65_536,
            1,
            crate::registry::hex(&loft_key.sign(&claim).to_bytes()),
        )
        .unwrap();
        append_directory_add(&mut conn, INTEGRITY_ORIGIN, &signing_key, mutation, 3).unwrap();
        (conn, signing_key)
    }

    #[test]
    fn every_exact_versioned_predecessor_migrates_to_head() {
        for version in 2..=IDENTITY_CHALLENGE_RESULT_SCHEMA_VERSION {
            let (mut conn, signing_key) = empty_predecessor(version);
            initialize(
                &mut conn,
                INTEGRITY_ORIGIN,
                &signing_key,
                LegacyAuthorization::Refuse,
            )
            .unwrap_or_else(|error| panic!("schema v{version} migration failed: {error}"));
            assert_eq!(user_version(&conn).unwrap(), SCHEMA_VERSION);
            verify_canonical_schema(&conn, SCHEMA_VERSION).unwrap();
            verify_storage(&conn, INTEGRITY_ORIGIN, &signing_key.verifying_key()).unwrap();
        }
    }

    #[test]
    fn malformed_predecessors_are_refused_before_any_ddl() {
        for version in 2..=GLOBAL_BINDING_ADMISSION_SCHEMA_VERSION {
            let (mut conn, signing_key) = empty_predecessor(version);
            conn.execute_batch("DROP INDEX registry_entries_by_type;")
                .unwrap();
            let before = schema_objects(&conn).unwrap();
            let error = initialize(
                &mut conn,
                INTEGRITY_ORIGIN,
                &signing_key,
                LegacyAuthorization::Refuse,
            )
            .unwrap_err();
            assert!(matches!(error, RegistryError::MigrationRequired(_)));
            assert_eq!(user_version(&conn).unwrap(), version);
            assert_eq!(schema_objects(&conn).unwrap(), before);
        }
    }

    #[test]
    fn failed_predecessor_replay_rolls_back_the_entire_migration_chain() {
        for version in 2..=GLOBAL_BINDING_ADMISSION_SCHEMA_VERSION {
            let (mut conn, signing_key) = empty_predecessor(version);
            conn.execute("UPDATE registry_state SET root = zeroblob(32)", [])
                .unwrap();
            let before = schema_objects(&conn).unwrap();
            let error = initialize(
                &mut conn,
                INTEGRITY_ORIGIN,
                &signing_key,
                LegacyAuthorization::Refuse,
            )
            .unwrap_err();
            assert!(matches!(error, RegistryError::CorruptStorage(_)));
            assert_eq!(user_version(&conn).unwrap(), version);
            assert_eq!(schema_objects(&conn).unwrap(), before);
        }
    }

    #[test]
    fn unversioned_storage_must_match_the_exact_released_schema() {
        let signing_key = SigningKey::from_bytes(&[94; 32]);
        let mut conn = Connection::open_in_memory().unwrap();
        create_legacy_reference_schema(&conn).unwrap();
        conn.execute_batch("DROP INDEX entries_by_handle;").unwrap();
        let before = schema_objects(&conn).unwrap();
        let error = initialize(
            &mut conn,
            INTEGRITY_ORIGIN,
            &signing_key,
            LegacyAuthorization::Refuse,
        )
        .unwrap_err();
        assert!(matches!(error, RegistryError::MigrationRequired(_)));
        assert_eq!(user_version(&conn).unwrap(), 0);
        assert_eq!(schema_objects(&conn).unwrap(), before);
    }

    #[test]
    fn current_schema_refuses_missing_or_weakened_authority_objects() {
        let cases = [
            (
                "table",
                "DROP INDEX identity_challenges_expiry;
                 DROP TABLE identity_challenges;",
            ),
            (
                "index",
                "DROP INDEX compliance_keys_history;
                 CREATE INDEX compliance_keys_history ON compliance_keys (seq);",
            ),
            (
                "trigger",
                "DROP TRIGGER entries_are_append_only_update;
                 CREATE TRIGGER entries_are_append_only_update
                 BEFORE UPDATE ON entries
                 BEGIN
                     SELECT 1;
                 END;",
            ),
            (
                "constraint",
                "DROP TABLE global_binding_admission;
                 CREATE TABLE global_binding_admission (
                     singleton INTEGER PRIMARY KEY,
                     window_minute INTEGER NOT NULL,
                     admissions INTEGER NOT NULL
                 );
                 INSERT INTO global_binding_admission VALUES (1, 0, 0);",
            ),
        ];
        for (kind, tamper) in cases {
            let signing_key = SigningKey::from_bytes(&[95; 32]);
            let mut conn = Connection::open_in_memory().unwrap();
            initialize(
                &mut conn,
                INTEGRITY_ORIGIN,
                &signing_key,
                LegacyAuthorization::Refuse,
            )
            .unwrap();
            conn.execute_batch(tamper).unwrap();
            let error = initialize(
                &mut conn,
                INTEGRITY_ORIGIN,
                &signing_key,
                LegacyAuthorization::Refuse,
            )
            .unwrap_err();
            assert!(
                matches!(error, RegistryError::CorruptStorage(_)),
                "missing or weakened {kind} unexpectedly opened: {error}"
            );
        }
    }

    #[test]
    fn startup_replays_old_payloads_not_only_the_newest_leaf() {
        let (mut conn, signing_key) = populated_integrity_store();
        conn.execute_batch(
            "DROP TRIGGER entries_are_append_only_update;
             UPDATE entries
             SET entry_json = replace(entry_json, 'github:account-1', 'github:account-9')
             WHERE seq = 0;
             CREATE TRIGGER entries_are_append_only_update
             BEFORE UPDATE ON entries
             BEGIN
                 SELECT RAISE(ABORT, 'registry entries are append-only');
             END;",
        )
        .unwrap();
        let error = initialize(
            &mut conn,
            INTEGRITY_ORIGIN,
            &signing_key,
            LegacyAuthorization::Refuse,
        )
        .unwrap_err();
        assert!(matches!(
            error,
            RegistryError::CorruptStorage(ref message) if message.contains("leaf")
        ));
    }

    #[test]
    fn startup_reconstructs_every_authority_projection() {
        let cases = [
            (
                "handle history",
                "UPDATE handle_history SET subject = 'github:tampered' WHERE seq = 0",
            ),
            (
                "current binding",
                "UPDATE current_bindings SET subject = 'github:tampered' WHERE handle = '/github/alice'",
            ),
            (
                "compliance-key",
                "UPDATE compliance_keys SET public_key = zeroblob(32)",
            ),
            (
                "directory mutation",
                "UPDATE directory_mutations SET mutation_sequence = mutation_sequence + 1",
            ),
        ];
        for (projection, tamper) in cases {
            let (mut conn, signing_key) = populated_integrity_store();
            conn.execute(tamper, []).unwrap();
            let error = initialize(
                &mut conn,
                INTEGRITY_ORIGIN,
                &signing_key,
                LegacyAuthorization::Refuse,
            )
            .unwrap_err();
            assert!(
                matches!(error, RegistryError::CorruptStorage(_)),
                "tampered {projection} projection unexpectedly opened: {error}"
            );
        }
    }

    #[test]
    fn global_binding_admission_is_exact_across_windows_and_restarts() {
        let root = tempfile::tempdir().unwrap();
        let path = root.path().join("registry.db");
        let signing_key = SigningKey::from_bytes(&[70; 32]);
        let origin = "pigeonpost.test/durable-global-admission";
        let minute = 12_345u64;
        {
            let mut conn = Connection::open(&path).unwrap();
            initialize(&mut conn, origin, &signing_key, LegacyAuthorization::Refuse).unwrap();
            let mut batch = GlobalBindingAdmissionBatch::default();
            charge_global_binding_admission(&mut conn, &mut batch, minute * 60_000, 2).unwrap();
            charge_global_binding_admission(&mut conn, &mut batch, minute * 60_000 + 59_999, 2)
                .unwrap();
        }
        {
            let mut conn = Connection::open(&path).unwrap();
            initialize(&mut conn, origin, &signing_key, LegacyAuthorization::Refuse).unwrap();
            let mut batch = GlobalBindingAdmissionBatch::default();
            assert!(matches!(
                charge_global_binding_admission(&mut conn, &mut batch, minute * 60_000 + 1, 2),
                Err(RegistryError::RateLimited)
            ));
            charge_global_binding_admission(&mut conn, &mut batch, (minute + 1) * 60_000, 2)
                .unwrap();
            assert!(matches!(
                charge_global_binding_admission(&mut conn, &mut batch, minute * 60_000 + 59_999, 2),
                Err(RegistryError::RegistryUnavailable)
            ));
            assert_eq!(
                load_global_binding_admission(&conn).unwrap(),
                (minute + 1, 2)
            );
        }
    }

    #[test]
    fn global_binding_reservations_amortize_commits_and_enforce_nonmultiple_limit() {
        let mut conn = Connection::open_in_memory().unwrap();
        let signing_key = SigningKey::from_bytes(&[72; 32]);
        initialize(
            &mut conn,
            "pigeonpost.test/global-admission-batch",
            &signing_key,
            LegacyAuthorization::Refuse,
        )
        .unwrap();
        let mut batch = GlobalBindingAdmissionBatch::default();
        let minute = 23_456u64;
        let limit = GLOBAL_BINDING_ADMISSION_BATCH_SIZE * 2 + 2;

        charge_global_binding_admission(&mut conn, &mut batch, minute * 60_000, limit).unwrap();
        assert_eq!(
            load_global_binding_admission(&conn).unwrap(),
            (minute, GLOBAL_BINDING_ADMISSION_BATCH_SIZE)
        );
        for _ in 1..GLOBAL_BINDING_ADMISSION_BATCH_SIZE {
            charge_global_binding_admission(&mut conn, &mut batch, minute * 60_000 + 1, limit)
                .unwrap();
        }
        assert_eq!(
            load_global_binding_admission(&conn).unwrap(),
            (minute, GLOBAL_BINDING_ADMISSION_BATCH_SIZE)
        );

        charge_global_binding_admission(&mut conn, &mut batch, minute * 60_000 + 2, limit).unwrap();
        assert_eq!(
            load_global_binding_admission(&conn).unwrap(),
            (minute, GLOBAL_BINDING_ADMISSION_BATCH_SIZE * 2)
        );
        for _ in 1..GLOBAL_BINDING_ADMISSION_BATCH_SIZE {
            charge_global_binding_admission(&mut conn, &mut batch, minute * 60_000 + 3, limit)
                .unwrap();
        }
        charge_global_binding_admission(&mut conn, &mut batch, minute * 60_000 + 4, limit).unwrap();
        assert_eq!(
            load_global_binding_admission(&conn).unwrap(),
            (minute, limit)
        );
        charge_global_binding_admission(&mut conn, &mut batch, minute * 60_000 + 5, limit).unwrap();
        assert!(matches!(
            charge_global_binding_admission(&mut conn, &mut batch, minute * 60_000 + 6, limit),
            Err(RegistryError::RateLimited)
        ));
    }

    #[test]
    fn global_binding_unused_reserve_burns_across_restart() {
        let root = tempfile::tempdir().unwrap();
        let path = root.path().join("registry.db");
        let signing_key = SigningKey::from_bytes(&[73; 32]);
        let origin = "pigeonpost.test/global-admission-restart-burn";
        let minute = 34_567u64;
        let limit = GLOBAL_BINDING_ADMISSION_BATCH_SIZE + 1;
        {
            let mut conn = Connection::open(&path).unwrap();
            initialize(&mut conn, origin, &signing_key, LegacyAuthorization::Refuse).unwrap();
            let mut batch = GlobalBindingAdmissionBatch::default();
            charge_global_binding_admission(&mut conn, &mut batch, minute * 60_000, limit).unwrap();
            assert_eq!(
                load_global_binding_admission(&conn).unwrap(),
                (minute, GLOBAL_BINDING_ADMISSION_BATCH_SIZE)
            );
        }

        let mut conn = Connection::open(&path).unwrap();
        initialize(&mut conn, origin, &signing_key, LegacyAuthorization::Refuse).unwrap();
        let mut batch = GlobalBindingAdmissionBatch::default();
        charge_global_binding_admission(&mut conn, &mut batch, minute * 60_000 + 1, limit).unwrap();
        assert_eq!(
            load_global_binding_admission(&conn).unwrap(),
            (minute, limit)
        );
        assert!(matches!(
            charge_global_binding_admission(&mut conn, &mut batch, minute * 60_000 + 2, limit),
            Err(RegistryError::RateLimited)
        ));
    }

    #[test]
    fn global_binding_batches_are_exact_across_connections_and_rollover() {
        let root = tempfile::tempdir().unwrap();
        let path = root.path().join("registry.db");
        let signing_key = SigningKey::from_bytes(&[74; 32]);
        let origin = "pigeonpost.test/global-admission-peer-process";
        let mut first = Connection::open(&path).unwrap();
        initialize(
            &mut first,
            origin,
            &signing_key,
            LegacyAuthorization::Refuse,
        )
        .unwrap();
        let mut second = Connection::open(&path).unwrap();
        initialize(
            &mut second,
            origin,
            &signing_key,
            LegacyAuthorization::Refuse,
        )
        .unwrap();
        let mut first_batch = GlobalBindingAdmissionBatch::default();
        let mut second_batch = GlobalBindingAdmissionBatch::default();
        let minute = 45_678u64;
        let limit = GLOBAL_BINDING_ADMISSION_BATCH_SIZE * 2 + 2;

        charge_global_binding_admission(&mut first, &mut first_batch, minute * 60_000, limit)
            .unwrap();
        charge_global_binding_admission(&mut second, &mut second_batch, minute * 60_000, limit)
            .unwrap();
        for _ in 1..GLOBAL_BINDING_ADMISSION_BATCH_SIZE {
            charge_global_binding_admission(
                &mut first,
                &mut first_batch,
                minute * 60_000 + 1,
                limit,
            )
            .unwrap();
            charge_global_binding_admission(
                &mut second,
                &mut second_batch,
                minute * 60_000 + 1,
                limit,
            )
            .unwrap();
        }
        charge_global_binding_admission(&mut first, &mut first_batch, minute * 60_000 + 2, limit)
            .unwrap();
        assert!(matches!(
            charge_global_binding_admission(
                &mut second,
                &mut second_batch,
                minute * 60_000 + 2,
                limit
            ),
            Err(RegistryError::RateLimited)
        ));
        charge_global_binding_admission(&mut first, &mut first_batch, minute * 60_000 + 3, limit)
            .unwrap();
        assert!(matches!(
            charge_global_binding_admission(
                &mut first,
                &mut first_batch,
                minute * 60_000 + 4,
                limit
            ),
            Err(RegistryError::RateLimited)
        ));

        charge_global_binding_admission(
            &mut second,
            &mut second_batch,
            (minute + 1) * 60_000,
            limit,
        )
        .unwrap();
        assert!(matches!(
            charge_global_binding_admission(
                &mut first,
                &mut first_batch,
                minute * 60_000 + 59_999,
                limit
            ),
            Err(RegistryError::RegistryUnavailable)
        ));
    }

    #[test]
    fn global_binding_high_water_survives_exhaustion_and_limit_change() {
        let signing_key = SigningKey::from_bytes(&[75; 32]);
        let minute = 56_789u64;
        let limit = GLOBAL_BINDING_ADMISSION_BATCH_SIZE * 2;

        let mut exhausted = Connection::open_in_memory().unwrap();
        initialize(
            &mut exhausted,
            "pigeonpost.test/global-admission-exhausted-rollback",
            &signing_key,
            LegacyAuthorization::Refuse,
        )
        .unwrap();
        let mut exhausted_batch = GlobalBindingAdmissionBatch::default();
        for _ in 0..GLOBAL_BINDING_ADMISSION_BATCH_SIZE {
            charge_global_binding_admission(
                &mut exhausted,
                &mut exhausted_batch,
                minute * 60_000,
                limit,
            )
            .unwrap();
        }
        exhausted
            .execute(
                "UPDATE global_binding_admission SET admissions = admissions - 1 WHERE singleton = 1",
                [],
            )
            .unwrap();
        assert!(matches!(
            charge_global_binding_admission(
                &mut exhausted,
                &mut exhausted_batch,
                minute * 60_000 + 1,
                limit
            ),
            Err(RegistryError::RegistryUnavailable)
        ));

        let mut reconfigured = Connection::open_in_memory().unwrap();
        initialize(
            &mut reconfigured,
            "pigeonpost.test/global-admission-reconfigured-rollback",
            &signing_key,
            LegacyAuthorization::Refuse,
        )
        .unwrap();
        let mut reconfigured_batch = GlobalBindingAdmissionBatch::default();
        charge_global_binding_admission(
            &mut reconfigured,
            &mut reconfigured_batch,
            minute * 60_000,
            limit,
        )
        .unwrap();
        assert!(matches!(
            charge_global_binding_admission(
                &mut reconfigured,
                &mut reconfigured_batch,
                minute * 60_000 + 1,
                1
            ),
            Err(RegistryError::RateLimited)
        ));
        reconfigured
            .execute(
                "UPDATE global_binding_admission SET admissions = 0 WHERE singleton = 1",
                [],
            )
            .unwrap();
        assert!(matches!(
            charge_global_binding_admission(
                &mut reconfigured,
                &mut reconfigured_batch,
                minute * 60_000 + 2,
                limit
            ),
            Err(RegistryError::RegistryUnavailable)
        ));
    }

    #[test]
    fn rotation_cannot_change_the_stable_provider_subject() {
        let mut conn = Connection::open_in_memory().unwrap();
        let signing_key = SigningKey::from_bytes(&[71; 32]);
        let origin = "pigeonpost.test/stable-subject";
        initialize(&mut conn, origin, &signing_key, LegacyAuthorization::Refuse).unwrap();
        append_handle(
            &mut conn,
            origin,
            &signing_key,
            HandleAppendRequest {
                handle: "/github/alice",
                pubkey: &"11".repeat(32),
                subject: "github:account-1",
                ts_ms: 1,
                mode: HandleAppendMode::Register,
            },
        )
        .unwrap();

        let error = append_handle(
            &mut conn,
            origin,
            &signing_key,
            HandleAppendRequest {
                handle: "/github/alice",
                pubkey: &"22".repeat(32),
                subject: "github:account-2",
                ts_ms: 2,
                mode: HandleAppendMode::Rotate,
            },
        )
        .unwrap_err();
        assert!(matches!(error, RegistryError::AlreadyBound));
        assert_eq!(load_state(&conn).unwrap().size, 1);
    }

    #[test]
    fn a_renamed_subject_may_keep_its_old_handle_and_claim_the_new_one() {
        // Schema 8 refused this outright. The account keeps the name people already published
        // while taking the name the provider now shows, and both bind to the same subject.
        let mut conn = Connection::open_in_memory().unwrap();
        let signing_key = SigningKey::from_bytes(&[72; 32]);
        let origin = "pigeonpost.test/subject-uniqueness";
        initialize(&mut conn, origin, &signing_key, LegacyAuthorization::Refuse).unwrap();
        append_handle(
            &mut conn,
            origin,
            &signing_key,
            HandleAppendRequest {
                handle: "/github/alice",
                pubkey: &"11".repeat(32),
                subject: "github:account-1",
                ts_ms: 1,
                mode: HandleAppendMode::Register,
            },
        )
        .unwrap();

        append_handle(
            &mut conn,
            origin,
            &signing_key,
            HandleAppendRequest {
                handle: "/github/alice-renamed",
                pubkey: &"22".repeat(32),
                subject: "github:account-1",
                ts_ms: 2,
                mode: HandleAppendMode::Register,
            },
        )
        .expect("the renamed spelling is the account's second of three allowed handles");

        assert_eq!(load_state(&conn).unwrap().size, 2);
        for handle in ["/github/alice", "/github/alice-renamed"] {
            let (_, _, subject) = current_binding(&conn, handle)
                .unwrap()
                .unwrap_or_else(|| panic!("{handle} must still resolve"));
            assert_eq!(subject, "github:account-1");
        }
    }

    #[test]
    fn challenge_consumption_and_binding_append_are_atomic_and_retryable() {
        let signing_key = SigningKey::from_bytes(&[81; 32]);
        let mut conn = Connection::open_in_memory().unwrap();
        initialize(
            &mut conn,
            INTEGRITY_ORIGIN,
            &signing_key,
            LegacyAuthorization::Refuse,
        )
        .unwrap();
        let challenge_hash = [82; 32];
        let pubkey = [83; 32];
        let pubkey_hex = crate::registry::hex(&pubkey);
        let pkce = "a".repeat(43);
        insert_identity_challenge(
            &mut conn,
            "github",
            &challenge_hash,
            "/github/alice",
            &pubkey,
            Some(&pkce),
            now_ms() + 60_000,
        )
        .unwrap();

        let commit = |conn: &mut Connection, subject: &str| {
            commit_handle_binding(
                conn,
                INTEGRITY_ORIGIN,
                &signing_key,
                Some(IdentityChallengeCommit {
                    provider: "github",
                    challenge_hash: &challenge_hash,
                    pubkey: &pubkey,
                    expected_pkce_challenge: Some(&pkce),
                }),
                HandleAppendRequest {
                    handle: "/github/alice",
                    pubkey: &pubkey_hex,
                    subject,
                    ts_ms: 1,
                    mode: HandleAppendMode::Register,
                },
            )
        };

        let first = commit(&mut conn, "github:account-1").unwrap();
        assert!(first.appended);
        assert_eq!(first.seq, 0);
        let persisted: (i64, i64) = conn
            .query_row(
                "SELECT consumed_at_ms, binding_seq FROM identity_challenges
                 WHERE challenge_hash = ?1",
                params![challenge_hash.as_slice()],
                |row| Ok((row.get(0)?, row.get(1)?)),
            )
            .unwrap();
        assert!(persisted.0 > 0);
        assert_eq!(persisted.1, 0);

        let recovered = commit(&mut conn, "github:account-1").unwrap();
        assert!(!recovered.appended);
        assert_eq!(recovered.seq, first.seq);
        assert_eq!(load_state(&conn).unwrap().size, 1);

        let error = commit(&mut conn, "github:another-account").unwrap_err();
        assert!(matches!(error, RegistryError::ProofRejected(_)));
        assert_eq!(load_state(&conn).unwrap().size, 1);
    }
}

#[cfg(test)]
mod handle_quota_tests {
    use super::*;

    const QUOTA_ORIGIN: &str = "pigeonpost.test/registry-handle-quota";

    fn store() -> (Connection, SigningKey) {
        let signing_key = SigningKey::from_bytes(&[77; 32]);
        let mut conn = Connection::open_in_memory().unwrap();
        initialize(
            &mut conn,
            QUOTA_ORIGIN,
            &signing_key,
            LegacyAuthorization::Refuse,
        )
        .unwrap();
        (conn, signing_key)
    }

    fn register(
        conn: &mut Connection,
        key: &SigningKey,
        handle: &str,
        subject: &str,
        seed: u8,
    ) -> Result<HandleAppend> {
        append_handle(
            conn,
            QUOTA_ORIGIN,
            key,
            HandleAppendRequest {
                handle,
                pubkey: &format!("{seed:02x}").repeat(32),
                subject,
                ts_ms: u64::from(seed),
                mode: HandleAppendMode::Register,
            },
        )
    }

    /// The headline rule. Schema 8 allowed exactly one handle per account; this is the relaxation.
    #[test]
    fn one_account_may_hold_three_handles_and_no_more() {
        let (mut conn, key) = store();
        for (index, handle) in ["/github/one", "/github/two", "/github/three"]
            .iter()
            .enumerate()
        {
            register(&mut conn, &key, handle, "github:4711", index as u8 + 1)
                .unwrap_or_else(|error| panic!("handle {handle} must be admitted: {error}"));
        }

        let refused = register(&mut conn, &key, "/github/four", "github:4711", 4).unwrap_err();
        assert!(
            matches!(
                refused,
                RegistryError::HandleQuotaExceeded {
                    limit: MAX_HANDLES_PER_SUBJECT
                }
            ),
            "a fourth handle must be refused as a quota breach, not as a binding conflict: {refused:?}"
        );
    }

    /// A different account is unaffected by a neighbour that has spent its allowance.
    #[test]
    fn the_quota_is_per_account_not_global() {
        let (mut conn, key) = store();
        for (index, handle) in ["/github/a1", "/github/a2", "/github/a3"].iter().enumerate() {
            register(&mut conn, &key, handle, "github:aaa", index as u8 + 1).unwrap();
        }
        register(&mut conn, &key, "/github/b1", "github:bbb", 9)
            .expect("a second account still has its full allowance");
    }

    /// Rotation rebinds a handle the account already owns, so it must not consume a slot —
    /// otherwise an account at its limit could never rotate a key, which is the recovery path.
    #[test]
    fn rotation_at_the_limit_still_succeeds() {
        let (mut conn, key) = store();
        for (index, handle) in ["/github/r1", "/github/r2", "/github/r3"].iter().enumerate() {
            register(&mut conn, &key, handle, "github:rot", index as u8 + 1).unwrap();
        }

        let rotated = append_handle(
            &mut conn,
            QUOTA_ORIGIN,
            &key,
            HandleAppendRequest {
                handle: "/github/r2",
                pubkey: &"ee".repeat(32),
                subject: "github:rot",
                ts_ms: 50,
                mode: HandleAppendMode::Rotate,
            },
        )
        .expect("rotation must not be charged against the registration quota");
        assert!(rotated.appended);
    }

    /// Re-sending an identical claim is idempotent and must not burn a slot on the retry.
    #[test]
    fn idempotent_retry_does_not_consume_a_second_slot() {
        let (mut conn, key) = store();
        let first = register(&mut conn, &key, "/github/only", "github:idem", 1).unwrap();
        let retry = register(&mut conn, &key, "/github/only", "github:idem", 1).unwrap();
        assert_eq!(retry.seq, first.seq);
        assert!(!retry.appended);

        for (index, handle) in ["/github/two", "/github/three"].iter().enumerate() {
            register(&mut conn, &key, handle, "github:idem", index as u8 + 2)
                .expect("the retry must not have consumed one of the remaining slots");
        }
    }

    /// A handle already held by someone else is a binding conflict, and that answer must not
    /// change just because the caller happens to be under quota.
    #[test]
    fn another_accounts_handle_is_still_a_binding_conflict() {
        let (mut conn, key) = store();
        register(&mut conn, &key, "/github/taken", "github:owner", 1).unwrap();
        let error = register(&mut conn, &key, "/github/taken", "github:stranger", 2).unwrap_err();
        assert!(matches!(error, RegistryError::AlreadyBound), "{error:?}");
    }
}
