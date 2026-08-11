//! Public two-phase disclosure leaves and an append-only Merkle accumulator.

use std::collections::{HashMap, HashSet};
#[cfg(any(not(unix), test))]
use std::fs::OpenOptions;
use std::fs::{self, File};
use std::io::{Read, Seek, SeekFrom, Write};
use std::path::{Path, PathBuf};
use std::sync::Mutex;

use hmac::{Hmac, Mac};
use pigeonpost_compliance_format::{
    ComplianceKeyId, CompliancePurpose, Jurisdiction, COMPLIANCE_KEY_ID_LEN,
};
use pigeonpost_registry::log::{empty_root, leaf_hash, Hash, MerkleFrontier};
use pigeonpost_registry::Checkpoint;
#[cfg(unix)]
use pigeonpost_unix_custody::{
    CustodyError, DirPolicy, FilePolicy, GuardedDir, GuardedFile, LeafName, OpenAccess,
};
use rand_core::{OsRng, RngCore};
use sha2::{Digest, Sha256};
use zeroize::Zeroizing;

use crate::approval::{AuthorizedDisclosure, DisclosureRequest, MAX_DISCLOSURE_KEYS};
use crate::error::{ComplianceError, Result};

const INTENT_VERSION: u8 = 1;
const COMPLETION_VERSION: u8 = 1;
const INTENT_DOMAIN: &[u8] = b"pigeonpost/disclosure-intent/v1";
const COMPLETION_DOMAIN: &[u8] = b"pigeonpost/disclosure-completion/v1";
const GENERIC_FAILURE_DOMAIN: &[u8] = b"pigeonpost/disclosure-operation-failed/v1";
const LOG_MAGIC: &[u8; 8] = b"PPDISC\0\0";
const STATE_MAGIC: &[u8; 8] = b"PPDSTAT\0";
const STATE_VERSION: u8 = 2;
const STATE_AUTH_DOMAIN: &[u8] = b"pigeonpost/disclosure-ledger-state-auth/v1";
const STATE_KEY_DOMAIN: &[u8] = b"pigeonpost/disclosure-ledger-state-key/v1";
const LEDGER_IDENTITY_DOMAIN: &[u8] = b"pigeonpost/disclosure-ledger-file/v1";
const MAX_LEAF_BYTES: usize = 4 * 1024;
const MAX_LOG_BYTES: u64 = 512 * 1024 * 1024;
// Restart state contains fixed committed-prefix metadata and at most one bounded pending leaf. The
// operational indexes are rebuilt by streaming the disclosure log; allowing a multi-megabyte
// sidecar would hide accidental whole-ledger persistence and restore quadratic append I/O.
const MAX_STATE_BYTES: u64 = 16 * 1024;
const MAX_RECOVERY_TAIL_BYTES: u64 = 4 + MAX_LEAF_BYTES as u64;
const PROOF_BLOCK_LEAVES: usize = 1024;
pub const MAX_DISCLOSURE_LEAVES: usize = 100_000;

/// Public pre-operation commitment. It contains no raw order, requester, selector, address, or
/// identity-provider subject.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DisclosureIntent {
    pub request_id: [u8; 32],
    pub timestamp_ms: u64,
    pub jurisdiction: Jurisdiction,
    pub purpose: CompliancePurpose,
    pub key_ids: Vec<ComplianceKeyId>,
    pub order_commitment: [u8; 32],
    pub requester_commitment: [u8; 32],
    pub selector_commitment: [u8; 32],
    pub approver_commitments: Vec<[u8; 32]>,
}

impl DisclosureIntent {
    pub fn encode(&self) -> Result<Vec<u8>> {
        if self.timestamp_ms == 0
            || self.request_id == [0u8; 32]
            || self.order_commitment == [0u8; 32]
            || self.requester_commitment == [0u8; 32]
            || self.selector_commitment == [0u8; 32]
            || self.key_ids.is_empty()
            || self.key_ids.len() > MAX_DISCLOSURE_KEYS
            || self.approver_commitments.len() != 2
            || self.approver_commitments[0] == self.approver_commitments[1]
            || self.approver_commitments.contains(&[0u8; 32])
            || !strictly_sorted_key_ids(&self.key_ids)
            || self.key_ids.iter().any(|key_id| {
                key_id.validate().is_err()
                    || key_id.purpose != self.purpose
                    || key_id.jurisdiction != self.jurisdiction
            })
        {
            return Err(ComplianceError::InvalidRequest);
        }
        let mut out = Vec::with_capacity(
            INTENT_DOMAIN.len() + 1 + 32 + 8 + 3 + self.key_ids.len() * 47 + 96 + 1 + 64,
        );
        out.extend_from_slice(INTENT_DOMAIN);
        out.push(INTENT_VERSION);
        out.extend_from_slice(&self.request_id);
        out.extend_from_slice(&self.timestamp_ms.to_be_bytes());
        out.push(self.jurisdiction.into());
        out.push(self.purpose.into());
        out.push(self.key_ids.len() as u8);
        for key_id in &self.key_ids {
            out.extend_from_slice(
                &key_id
                    .encode()
                    .map_err(|_| ComplianceError::InvalidRequest)?,
            );
        }
        out.extend_from_slice(&self.order_commitment);
        out.extend_from_slice(&self.requester_commitment);
        out.extend_from_slice(&self.selector_commitment);
        out.push(self.approver_commitments.len() as u8);
        for commitment in &self.approver_commitments {
            out.extend_from_slice(commitment);
        }
        Ok(out)
    }

    /// Decode one exact intent encoding; unknown values, duplicate/out-of-order keys, and trailing
    /// bytes fail closed.
    pub fn decode(bytes: &[u8]) -> Result<Self> {
        let fixed_prefix = INTENT_DOMAIN.len() + 1 + 32 + 8 + 3;
        if bytes.len() < fixed_prefix + 96 + 1 + 64
            || !bytes.starts_with(INTENT_DOMAIN)
            || bytes[INTENT_DOMAIN.len()] != INTENT_VERSION
        {
            return Err(ComplianceError::InvalidRequest);
        }
        let mut cursor = INTENT_DOMAIN.len() + 1;
        let mut request_id = [0u8; 32];
        request_id.copy_from_slice(&bytes[cursor..cursor + 32]);
        cursor += 32;
        let timestamp_ms =
            u64::from_be_bytes(bytes[cursor..cursor + 8].try_into().expect("fixed slice"));
        cursor += 8;
        let jurisdiction =
            Jurisdiction::try_from(bytes[cursor]).map_err(|_| ComplianceError::InvalidRequest)?;
        cursor += 1;
        let purpose = CompliancePurpose::try_from(bytes[cursor])
            .map_err(|_| ComplianceError::InvalidRequest)?;
        cursor += 1;
        let key_count = bytes[cursor] as usize;
        cursor += 1;
        if key_count == 0 || key_count > MAX_DISCLOSURE_KEYS {
            return Err(ComplianceError::InvalidRequest);
        }
        let variable = key_count
            .checked_mul(COMPLIANCE_KEY_ID_LEN)
            .ok_or(ComplianceError::LimitExceeded)?;
        let expected = cursor
            .checked_add(variable)
            .and_then(|value| value.checked_add(96 + 1 + 64))
            .ok_or(ComplianceError::LimitExceeded)?;
        if bytes.len() != expected {
            return Err(ComplianceError::InvalidRequest);
        }
        let mut key_ids = Vec::with_capacity(key_count);
        for _ in 0..key_count {
            key_ids.push(
                ComplianceKeyId::decode(&bytes[cursor..cursor + COMPLIANCE_KEY_ID_LEN])
                    .map_err(|_| ComplianceError::InvalidRequest)?,
            );
            cursor += COMPLIANCE_KEY_ID_LEN;
        }
        let order_commitment = read_fixed32(bytes, &mut cursor);
        let requester_commitment = read_fixed32(bytes, &mut cursor);
        let selector_commitment = read_fixed32(bytes, &mut cursor);
        if bytes[cursor] != 2 {
            return Err(ComplianceError::InvalidRequest);
        }
        cursor += 1;
        let approver_commitments = vec![
            read_fixed32(bytes, &mut cursor),
            read_fixed32(bytes, &mut cursor),
        ];
        let intent = Self {
            request_id,
            timestamp_ms,
            jurisdiction,
            purpose,
            key_ids,
            order_commitment,
            requester_commitment,
            selector_commitment,
            approver_commitments,
        };
        if !strictly_sorted_key_ids(&intent.key_ids) {
            return Err(ComplianceError::InvalidRequest);
        }
        intent.encode()?;
        Ok(intent)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[repr(u8)]
pub enum CompletionStatus {
    Succeeded = 1,
    Failed = 2,
}

/// Public post-operation commitment. Failure details and disclosed records stay private.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DisclosureCompletion {
    pub request_id: [u8; 32],
    pub timestamp_ms: u64,
    pub status: CompletionStatus,
    pub record_count: u32,
    pub result_commitment: [u8; 32],
}

impl DisclosureCompletion {
    pub fn encode(&self) -> Result<Vec<u8>> {
        if self.timestamp_ms == 0
            || self.request_id == [0u8; 32]
            || self.result_commitment == [0u8; 32]
        {
            return Err(ComplianceError::InvalidRequest);
        }
        if self.status == CompletionStatus::Failed && self.record_count != 0 {
            return Err(ComplianceError::InvalidRequest);
        }
        let mut out = Vec::with_capacity(COMPLETION_DOMAIN.len() + 1 + 32 + 8 + 1 + 4 + 32);
        out.extend_from_slice(COMPLETION_DOMAIN);
        out.push(COMPLETION_VERSION);
        out.extend_from_slice(&self.request_id);
        out.extend_from_slice(&self.timestamp_ms.to_be_bytes());
        out.push(self.status as u8);
        out.extend_from_slice(&self.record_count.to_be_bytes());
        out.extend_from_slice(&self.result_commitment);
        Ok(out)
    }

    pub fn decode(bytes: &[u8]) -> Result<Self> {
        let expected = COMPLETION_DOMAIN.len() + 1 + 32 + 8 + 1 + 4 + 32;
        if bytes.len() != expected
            || !bytes.starts_with(COMPLETION_DOMAIN)
            || bytes[COMPLETION_DOMAIN.len()] != COMPLETION_VERSION
        {
            return Err(ComplianceError::InvalidRequest);
        }
        let mut cursor = COMPLETION_DOMAIN.len() + 1;
        let mut request_id = [0u8; 32];
        request_id.copy_from_slice(&bytes[cursor..cursor + 32]);
        cursor += 32;
        let timestamp_ms =
            u64::from_be_bytes(bytes[cursor..cursor + 8].try_into().expect("fixed slice"));
        cursor += 8;
        let status = match bytes[cursor] {
            1 => CompletionStatus::Succeeded,
            2 => CompletionStatus::Failed,
            _ => return Err(ComplianceError::InvalidRequest),
        };
        cursor += 1;
        let record_count =
            u32::from_be_bytes(bytes[cursor..cursor + 4].try_into().expect("fixed slice"));
        cursor += 4;
        let mut result_commitment = [0u8; 32];
        result_commitment.copy_from_slice(&bytes[cursor..]);
        let completion = Self {
            request_id,
            timestamp_ms,
            status,
            record_count,
            result_commitment,
        };
        completion.encode()?;
        Ok(completion)
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum DisclosureLeaf {
    Intent(DisclosureIntent),
    Completion(DisclosureCompletion),
}

impl DisclosureLeaf {
    pub fn encode(&self) -> Result<Vec<u8>> {
        match self {
            Self::Intent(value) => value.encode(),
            Self::Completion(value) => value.encode(),
        }
    }

    pub fn hash(&self) -> Result<[u8; 32]> {
        let bytes = self.encode()?;
        Ok(leaf_hash(&bytes))
    }

    pub fn decode(bytes: &[u8]) -> Result<Self> {
        if bytes.starts_with(INTENT_DOMAIN) {
            DisclosureIntent::decode(bytes).map(Self::Intent)
        } else if bytes.starts_with(COMPLETION_DOMAIN) {
            DisclosureCompletion::decode(bytes).map(Self::Completion)
        } else {
            Err(ComplianceError::InvalidRequest)
        }
    }
}

/// Successful operation result supplied to the automatic completion writer.
pub struct DisclosureOutput<T> {
    pub value: T,
    pub record_count: u32,
    /// Private-audit-keyed commitment to the exact disclosure artifact handed to the requester.
    pub result_commitment: [u8; 32],
}

impl<T> core::fmt::Debug for DisclosureOutput<T> {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        // `value` is the authorized disclosure itself. Do not add a `T: Debug` bound or make a
        // lawful-disclosure plaintext accidentally printable through generic instrumentation.
        f.debug_struct("DisclosureOutput")
            .field("record_count", &self.record_count)
            .field("value", &"withheld")
            .field("result_commitment", &"withheld")
            .finish()
    }
}

/// Crash-safe disclosure log using the registry's exact RFC 6962 Merkle implementation.
pub struct DisclosureLedger {
    file: Option<Mutex<File>>,
    #[cfg(unix)]
    custody: Option<LedgerCustody>,
    state_store: Option<LedgerStateStore>,
    state_poisoned: bool,
    /// Payload offsets are the bounded disk index used for leaf reads and public proofs. They use
    /// eight bytes per leaf instead of retaining up to 4 KiB of decoded attacker-controlled data.
    leaf_offsets: Vec<u64>,
    /// Only the test-only in-memory ledger retains encodings. Durable ledgers leave this empty.
    memory_leaves: Vec<Vec<u8>>,
    frontier: MerkleFrontier,
    root: Hash,
    /// One hash per complete proof block plus at most one partial block. This keeps arbitrary
    /// RFC 6962 proofs available without retaining the full leaf-hash vector.
    proof_block_roots: Vec<Hash>,
    proof_tail_frontier: MerkleFrontier,
    proof_tail_hashes: Vec<Hash>,
    /// All request IDs are retained as a compact uniqueness index; timestamps are retained only
    /// for outstanding intents that can still receive a completion.
    request_ids: HashSet<[u8; 32]>,
    incomplete: HashMap<[u8; 32], PendingIntent>,
}

#[cfg(unix)]
struct LedgerCustody {
    directory: GuardedDir,
    ledger: GuardedFile,
    state_name: LeafName,
    state: Mutex<Option<GuardedFile>>,
}

#[derive(Debug, Clone, Copy)]
struct PendingIntent {
    timestamp_ms: u64,
    sequence: u64,
}

struct LedgerStateStore {
    ledger_path: PathBuf,
    #[cfg_attr(unix, allow(dead_code))]
    path: PathBuf,
    authentication_key: Zeroizing<[u8; 32]>,
    file_identity: [u8; 32],
    mutation_marker: [u8; 16],
    generation: u64,
    log_len: u64,
    last_leaf_hash: Option<Hash>,
    /// Authenticated write-ahead record. A crash tail is accepted only when it exactly matches
    /// these bytes, so an unauthenticated writer cannot smuggle one otherwise-valid leaf into the
    /// log while leaving the committed state untouched.
    pending_append: Option<Vec<u8>>,
}

struct PersistedLedgerState {
    file_identity: [u8; 32],
    mutation_marker: [u8; 16],
    generation: u64,
    log_len: u64,
    last_leaf_hash: Option<Hash>,
    pending_append: Option<Vec<u8>>,
    root: Hash,
}

impl DisclosureLedger {
    /// Create a new append-only disclosure log and its authenticated restart state. Both files are
    /// owner-only and fsynced before this returns; the PPDISC log bytes remain unchanged.
    pub fn create(path: impl AsRef<Path>, state_secret: &[u8; 32]) -> Result<Self> {
        Self::create_for_platform(
            crate::platform::OfflinePlatform::current(),
            path,
            state_secret,
        )
    }

    fn create_for_platform(
        platform: crate::platform::OfflinePlatform,
        path: impl AsRef<Path>,
        state_secret: &[u8; 32],
    ) -> Result<Self> {
        platform.require()?;
        let path = path.as_ref();
        let parent = path.parent().ok_or(ComplianceError::Storage)?;
        let state_path = disclosure_state_path(path)?;

        #[cfg(unix)]
        let (file, custody) = {
            #[cfg(test)]
            {
                use std::os::unix::fs::PermissionsExt;
                std::fs::set_permissions(parent, std::fs::Permissions::from_mode(0o700))
                    .map_err(|_| ComplianceError::Storage)?;
            }
            let directory = GuardedDir::create_private(parent).map_err(map_custody_error)?;
            let ledger_name = leaf_for_path(path)?;
            let state_name = leaf_for_path(&state_path)?;
            if directory
                .entry_metadata(&state_name)
                .map_err(map_custody_error)?
                .is_some()
            {
                return Err(ComplianceError::Storage);
            }
            let mut ledger = directory
                .create_file(&ledger_name, FilePolicy::private(MAX_LOG_BYTES))
                .map_err(map_custody_error)?;
            ledger
                .write_all(LOG_MAGIC)
                .map_err(|_| ComplianceError::Storage)?;
            ledger.sync_all().map_err(map_custody_error)?;
            ledger.verify_named().map_err(map_custody_error)?;
            let file = ledger
                .file()
                .try_clone()
                .map_err(|_| ComplianceError::Storage)?;
            (
                file,
                LedgerCustody {
                    directory,
                    ledger,
                    state_name,
                    state: Mutex::new(None),
                },
            )
        };

        #[cfg(not(unix))]
        let mut file = {
            secure_private_parent(parent)?;
            if state_path
                .try_exists()
                .map_err(|_| ComplianceError::Storage)?
            {
                return Err(ComplianceError::Storage);
            }
            let mut options = OpenOptions::new();
            options.create_new(true).read(true).write(true);
            #[cfg(unix)]
            {
                use std::os::unix::fs::OpenOptionsExt;
                options.mode(0o600);
            }
            options.open(path).map_err(|_| ComplianceError::Storage)?
        };
        #[cfg(not(unix))]
        validate_opened_ledger(&file, path, MAX_LOG_BYTES)?;
        #[cfg(not(unix))]
        file.write_all(LOG_MAGIC)
            .map_err(|_| ComplianceError::Storage)?;
        #[cfg(not(unix))]
        file.sync_all().map_err(|_| ComplianceError::Storage)?;
        #[cfg(not(unix))]
        sync_parent(parent)?;
        let metadata = file.metadata().map_err(|_| ComplianceError::Storage)?;
        let ledger = Self {
            file: Some(Mutex::new(file)),
            #[cfg(unix)]
            custody: Some(custody),
            state_store: Some(LedgerStateStore {
                ledger_path: path.to_owned(),
                path: state_path,
                authentication_key: derive_state_authentication_key(state_secret),
                file_identity: ledger_file_identity(path, &metadata)?,
                mutation_marker: ledger_mutation_marker(&metadata)?,
                generation: 0,
                log_len: LOG_MAGIC.len() as u64,
                last_leaf_hash: None,
                pending_append: None,
            }),
            state_poisoned: false,
            leaf_offsets: Vec::new(),
            memory_leaves: Vec::new(),
            frontier: MerkleFrontier::new(),
            root: empty_root(),
            proof_block_roots: Vec::new(),
            proof_tail_frontier: MerkleFrontier::new(),
            proof_tail_hashes: Vec::new(),
            request_ids: HashSet::new(),
            incomplete: HashMap::new(),
        };
        ledger.persist_state()?;
        Ok(ledger)
    }

    /// Open from authenticated committed-prefix metadata and replay at most one crash-interrupted
    /// tail record. The bounded PPDISC log is streamed to rebuild offsets, request state, proof
    /// blocks, and the Merkle frontier; its computed generation/root/last hash must exactly match
    /// the authenticated companion state before a pending append can be recovered.
    pub fn open(path: impl AsRef<Path>, state_secret: &[u8; 32]) -> Result<Self> {
        Self::open_for_platform(
            crate::platform::OfflinePlatform::current(),
            path,
            state_secret,
        )
    }

    fn open_for_platform(
        platform: crate::platform::OfflinePlatform,
        path: impl AsRef<Path>,
        state_secret: &[u8; 32],
    ) -> Result<Self> {
        platform.require()?;
        let path = path.as_ref();
        let state_path = disclosure_state_path(path)?;
        let authentication_key = derive_state_authentication_key(state_secret);

        #[cfg(unix)]
        let (mut file, custody, state_exists, persisted) = {
            let parent = path.parent().ok_or(ComplianceError::Storage)?;
            let directory = GuardedDir::open_existing(parent, DirPolicy::private_mutable())
                .map_err(map_custody_error)?;
            let ledger = directory
                .open_file(
                    &leaf_for_path(path)?,
                    OpenAccess::ReadWrite,
                    FilePolicy::private(MAX_LOG_BYTES),
                )
                .map_err(map_custody_error)?;
            let state_name = leaf_for_path(&state_path)?;
            let state = directory
                .open_file_optional(
                    &state_name,
                    OpenAccess::ReadOnly,
                    FilePolicy::private(MAX_STATE_BYTES),
                )
                .map_err(map_custody_error)?;
            let persisted = state
                .as_ref()
                .map(|guard| read_persisted_state_guarded(guard, &authentication_key))
                .transpose()?;
            let state_exists = state.is_some();
            let file = ledger
                .file()
                .try_clone()
                .map_err(|_| ComplianceError::Storage)?;
            (
                file,
                LedgerCustody {
                    directory,
                    ledger,
                    state_name,
                    state: Mutex::new(state),
                },
                state_exists,
                persisted,
            )
        };

        #[cfg(not(unix))]
        let mut file = open_private_ledger(path)?;
        let metadata = file.metadata().map_err(|_| ComplianceError::Storage)?;
        if metadata.len() > MAX_LOG_BYTES {
            return Err(ComplianceError::LimitExceeded);
        }
        let file_len = metadata.len();
        if file_len < LOG_MAGIC.len() as u64 {
            return Err(ComplianceError::InvalidRequest);
        }
        file.seek(SeekFrom::Start(0))
            .map_err(|_| ComplianceError::Storage)?;
        let mut magic = [0u8; LOG_MAGIC.len()];
        file.read_exact(&mut magic)
            .map_err(|_| ComplianceError::Storage)?;
        if &magic != LOG_MAGIC {
            return Err(ComplianceError::InvalidRequest);
        }
        let identity = ledger_file_identity(path, &metadata)?;
        let marker = ledger_mutation_marker(&metadata)?;

        #[cfg(not(unix))]
        let state_exists = state_path
            .try_exists()
            .map_err(|_| ComplianceError::Storage)?;

        #[cfg(not(unix))]
        let persisted = if state_exists {
            Some(read_persisted_state(&state_path, &authentication_key)?)
        } else if file_len == LOG_MAGIC.len() as u64 {
            None
        } else {
            return Err(ComplianceError::Storage);
        };
        let mut ledger = if let Some(state) = persisted {
            state.validate(path, &mut file, file_len, identity, marker)?;
            Self::from_persisted(file, path, state_path, authentication_key, state)?
        } else {
            Self {
                file: Some(Mutex::new(file)),
                #[cfg(unix)]
                custody: None,
                state_store: Some(LedgerStateStore {
                    ledger_path: path.to_owned(),
                    path: state_path,
                    authentication_key,
                    file_identity: identity,
                    mutation_marker: marker,
                    generation: 0,
                    log_len: LOG_MAGIC.len() as u64,
                    last_leaf_hash: None,
                    pending_append: None,
                }),
                state_poisoned: false,
                leaf_offsets: Vec::new(),
                memory_leaves: Vec::new(),
                frontier: MerkleFrontier::new(),
                root: empty_root(),
                proof_block_roots: Vec::new(),
                proof_tail_frontier: MerkleFrontier::new(),
                proof_tail_hashes: Vec::new(),
                request_ids: HashSet::new(),
                incomplete: HashMap::new(),
            }
        };
        #[cfg(unix)]
        {
            ledger.custody = Some(custody);
        }
        let recovered = ledger.recover_bounded_tail(file_len)?;
        {
            let guard = ledger
                .file
                .as_ref()
                .ok_or(ComplianceError::Storage)?
                .lock()
                .map_err(|_| ComplianceError::Storage)?;
            let final_metadata = guard.metadata().map_err(|_| ComplianceError::Storage)?;
            let store = ledger
                .state_store
                .as_ref()
                .ok_or(ComplianceError::Storage)?;
            if final_metadata.len() != store.log_len
                || ledger_file_identity(path, &final_metadata)? != store.file_identity
                || (!recovered && ledger_mutation_marker(&final_metadata)? != store.mutation_marker)
            {
                return Err(ComplianceError::Storage);
            }
        }
        ledger.verify_ledger_custody()?;
        if recovered || !state_exists {
            ledger.refresh_store_file_stamp()?;
            ledger.persist_state()?;
        }
        Ok(ledger)
    }

    fn from_persisted(
        mut file: File,
        ledger_path: &Path,
        state_path: PathBuf,
        authentication_key: Zeroizing<[u8; 32]>,
        state: PersistedLedgerState,
    ) -> Result<Self> {
        let PersistedLedgerState {
            file_identity,
            mutation_marker,
            generation,
            log_len,
            last_leaf_hash,
            pending_append,
            root: expected_root,
        } = state;

        // The authenticated sidecar deliberately does not persist indexes. Rebuild them by
        // streaming the exact committed prefix so append cost stays constant and every restart
        // revalidates the public record sequence and Merkle commitment.
        let mut ledger = Self {
            file: None,
            #[cfg(unix)]
            custody: None,
            state_store: None,
            state_poisoned: false,
            leaf_offsets: Vec::with_capacity(
                usize::try_from(generation).map_err(|_| ComplianceError::LimitExceeded)?,
            ),
            memory_leaves: Vec::new(),
            frontier: MerkleFrontier::new(),
            root: empty_root(),
            proof_block_roots: Vec::with_capacity(
                usize::try_from(generation).map_err(|_| ComplianceError::LimitExceeded)?
                    / PROOF_BLOCK_LEAVES,
            ),
            proof_tail_frontier: MerkleFrontier::new(),
            proof_tail_hashes: Vec::with_capacity(PROOF_BLOCK_LEAVES),
            request_ids: HashSet::with_capacity(
                usize::try_from(generation).map_err(|_| ComplianceError::LimitExceeded)?,
            ),
            incomplete: HashMap::new(),
        };
        let mut record_offset = LOG_MAGIC.len() as u64;
        let mut streamed_last_hash = None;
        while record_offset < log_len {
            if ledger.leaf_count() as usize >= MAX_DISCLOSURE_LEAVES {
                return Err(ComplianceError::LimitExceeded);
            }
            let payload_offset = record_offset
                .checked_add(4)
                .ok_or(ComplianceError::LimitExceeded)?;
            let (encoded, end) = read_record_at(&mut file, payload_offset, log_len)?;
            let leaf = DisclosureLeaf::decode(&encoded)?;
            validate_leaf_sequence(&ledger.request_ids, &ledger.incomplete, &leaf)?;
            let sequence = ledger.leaf_count();
            let hash = leaf_hash(&encoded);
            ledger.leaf_offsets.push(payload_offset);
            ledger.absorb_hash(hash)?;
            record_leaf_state(
                &mut ledger.request_ids,
                &mut ledger.incomplete,
                &leaf,
                sequence,
            );
            streamed_last_hash = Some(hash);
            record_offset = end;
        }
        if record_offset != log_len
            || ledger.leaf_count() != generation
            || ledger.root != expected_root
            || streamed_last_hash != last_leaf_hash
        {
            return Err(ComplianceError::Storage);
        }
        if let Some(encoded) = pending_append.as_deref() {
            let leaf = DisclosureLeaf::decode(encoded)?;
            validate_leaf_sequence(&ledger.request_ids, &ledger.incomplete, &leaf)?;
        }
        file.seek(SeekFrom::End(0))
            .map_err(|_| ComplianceError::Storage)?;
        ledger.file = Some(Mutex::new(file));
        ledger.state_store = Some(LedgerStateStore {
            ledger_path: ledger_path.to_owned(),
            path: state_path,
            authentication_key,
            file_identity,
            mutation_marker,
            generation,
            log_len,
            last_leaf_hash,
            pending_append,
        });
        Ok(ledger)
    }

    fn recover_bounded_tail(&mut self, file_len: u64) -> Result<bool> {
        self.verify_ledger_custody()?;
        let (indexed_len, pending_append) = {
            let store = self.state_store.as_ref().ok_or(ComplianceError::Storage)?;
            (store.log_len, store.pending_append.clone())
        };
        if file_len < indexed_len || file_len - indexed_len > MAX_RECOVERY_TAIL_BYTES {
            return Err(ComplianceError::Storage);
        }
        let Some(encoded) = pending_append else {
            if file_len != indexed_len {
                return Err(ComplianceError::Storage);
            }
            let mut file = self
                .file
                .as_ref()
                .ok_or(ComplianceError::Storage)?
                .lock()
                .map_err(|_| ComplianceError::Storage)?;
            file.seek(SeekFrom::End(0))
                .map_err(|_| ComplianceError::Storage)?;
            return Ok(false);
        };

        let mut expected_record = Vec::with_capacity(4 + encoded.len());
        expected_record.extend_from_slice(&(encoded.len() as u32).to_be_bytes());
        expected_record.extend_from_slice(&encoded);
        let remaining =
            usize::try_from(file_len - indexed_len).map_err(|_| ComplianceError::LimitExceeded)?;
        if remaining > expected_record.len() {
            return Err(ComplianceError::Storage);
        }

        let complete = {
            let mut file = self
                .file
                .as_ref()
                .ok_or(ComplianceError::Storage)?
                .lock()
                .map_err(|_| ComplianceError::Storage)?;
            file.seek(SeekFrom::Start(indexed_len))
                .map_err(|_| ComplianceError::Storage)?;
            let mut actual = vec![0u8; remaining];
            file.read_exact(&mut actual)
                .map_err(|_| ComplianceError::Storage)?;
            if actual != expected_record[..remaining] {
                return Err(ComplianceError::Storage);
            }
            if remaining < expected_record.len() {
                file.set_len(indexed_len)
                    .and_then(|_| file.sync_all())
                    .map_err(|_| ComplianceError::Storage)?;
                file.seek(SeekFrom::End(0))
                    .map_err(|_| ComplianceError::Storage)?;
                false
            } else {
                file.seek(SeekFrom::End(0))
                    .map_err(|_| ComplianceError::Storage)?;
                true
            }
        };

        if complete {
            let leaf = DisclosureLeaf::decode(&encoded)?;
            validate_leaf_sequence(&self.request_ids, &self.incomplete, &leaf)?;
            if self.leaf_count() as usize >= MAX_DISCLOSURE_LEAVES {
                return Err(ComplianceError::LimitExceeded);
            }
            let sequence = self.leaf_count();
            let hash = leaf_hash(&encoded);
            self.leaf_offsets.push(indexed_len + 4);
            self.absorb_hash(hash)?;
            record_leaf_state(&mut self.request_ids, &mut self.incomplete, &leaf, sequence);
            let generation = self.leaf_count();
            let store = self.state_store.as_mut().ok_or(ComplianceError::Storage)?;
            store.generation = generation;
            store.log_len = file_len;
            store.last_leaf_hash = Some(hash);
        }
        self.state_store
            .as_mut()
            .ok_or(ComplianceError::Storage)?
            .pending_append = None;
        self.verify_ledger_custody()?;
        Ok(true)
    }

    fn refresh_store_file_stamp(&mut self) -> Result<()> {
        self.verify_ledger_custody()?;
        let (ledger_path, expected_identity) = {
            let store = self.state_store.as_ref().ok_or(ComplianceError::Storage)?;
            (store.ledger_path.clone(), store.file_identity)
        };
        let metadata = self
            .file
            .as_ref()
            .ok_or(ComplianceError::Storage)?
            .lock()
            .map_err(|_| ComplianceError::Storage)?
            .metadata()
            .map_err(|_| ComplianceError::Storage)?;
        let identity = ledger_file_identity(&ledger_path, &metadata)?;
        if identity != expected_identity {
            return Err(ComplianceError::Storage);
        }
        let marker = ledger_mutation_marker(&metadata)?;
        let store = self.state_store.as_mut().ok_or(ComplianceError::Storage)?;
        store.mutation_marker = marker;
        store.log_len = metadata.len();
        Ok(())
    }

    fn validate_current_store_file_stamp(&self) -> Result<()> {
        self.verify_ledger_custody()?;
        let store = self.state_store.as_ref().ok_or(ComplianceError::Storage)?;
        let file = self
            .file
            .as_ref()
            .ok_or(ComplianceError::Storage)?
            .lock()
            .map_err(|_| ComplianceError::Storage)?;
        let metadata = file.metadata().map_err(|_| ComplianceError::Storage)?;
        if metadata.len() != store.log_len
            || ledger_file_identity(&store.ledger_path, &metadata)? != store.file_identity
            || ledger_mutation_marker(&metadata)? != store.mutation_marker
        {
            return Err(ComplianceError::Storage);
        }
        Ok(())
    }

    #[cfg(unix)]
    fn verify_ledger_custody(&self) -> Result<()> {
        if self.file.is_none() {
            return Ok(());
        }
        let custody = self.custody.as_ref().ok_or(ComplianceError::Storage)?;
        custody
            .directory
            .verify_named()
            .and_then(|_| custody.ledger.verify_named())
            .map_err(map_custody_error)?;
        let state = custody.state.lock().map_err(|_| ComplianceError::Storage)?;
        if let Some(state) = state.as_ref() {
            state.verify_named().map_err(map_custody_error)?;
        } else if custody
            .directory
            .entry_metadata(&custody.state_name)
            .map_err(map_custody_error)?
            .is_some()
        {
            return Err(ComplianceError::Storage);
        }
        Ok(())
    }

    #[cfg(not(unix))]
    fn verify_ledger_custody(&self) -> Result<()> {
        let Some(file) = &self.file else {
            return Ok(());
        };
        let store = self.state_store.as_ref().ok_or(ComplianceError::Storage)?;
        let file = file.lock().map_err(|_| ComplianceError::Storage)?;
        if !named_ledger_matches(&file, &store.ledger_path) {
            return Err(ComplianceError::Storage);
        }
        Ok(())
    }

    fn persist_state(&self) -> Result<()> {
        let store = self.state_store.as_ref().ok_or(ComplianceError::Storage)?;
        if store.generation != self.leaf_count()
            || store.log_len < LOG_MAGIC.len() as u64
            || store.last_leaf_hash.is_some() != (self.leaf_count() != 0)
        {
            return Err(ComplianceError::Storage);
        }
        let state = PersistedLedgerState {
            file_identity: store.file_identity,
            mutation_marker: store.mutation_marker,
            generation: store.generation,
            log_len: store.log_len,
            last_leaf_hash: store.last_leaf_hash,
            pending_append: store.pending_append.clone(),
            root: self.root,
        };
        let encoded = state.encode_authenticated(&store.authentication_key)?;
        #[cfg(unix)]
        return write_private_state_guarded(
            self.custody.as_ref().ok_or(ComplianceError::Storage)?,
            &encoded,
        );
        #[cfg(not(unix))]
        write_private_state_atomic(&store.path, &encoded)
    }

    #[cfg(test)]
    pub(crate) fn in_memory() -> Self {
        Self {
            file: None,
            #[cfg(unix)]
            custody: None,
            state_store: None,
            state_poisoned: false,
            leaf_offsets: Vec::new(),
            memory_leaves: Vec::new(),
            frontier: MerkleFrontier::new(),
            root: empty_root(),
            proof_block_roots: Vec::new(),
            proof_tail_frontier: MerkleFrontier::new(),
            proof_tail_hashes: Vec::new(),
            request_ids: HashSet::new(),
            incomplete: HashMap::new(),
        }
    }

    pub fn leaf_count(&self) -> u64 {
        self.frontier.size()
    }

    /// Read and strictly decode one public leaf without retaining the rest of the ledger.
    pub fn leaf(&self, index: u64) -> Result<Option<DisclosureLeaf>> {
        let Some(encoded) = self.encoded_leaf(index)? else {
            return Ok(None);
        };
        DisclosureLeaf::decode(&encoded).map(Some)
    }

    pub fn root(&self) -> Hash {
        self.root
    }

    pub fn inclusion_proof(&self, index: u64) -> Result<Option<Vec<Hash>>> {
        let size = self.leaf_count();
        if index >= size {
            return Ok(None);
        }
        let proof = self.inclusion_path(0, size, index)?;
        let leaf = self.leaf_hash_at(index)?;
        if !pigeonpost_registry::verify_inclusion(&leaf, index, size, &proof, &self.root) {
            return Err(ComplianceError::Storage);
        }
        Ok(Some(proof))
    }

    pub fn consistency_proof(&self, old_size: u64) -> Result<Option<Vec<Hash>>> {
        let size = self.leaf_count();
        if old_size == 0 || old_size > size {
            return Ok(None);
        }
        Ok(Some(self.consistency_path(0, size, old_size, true)?))
    }

    /// Construct a checkpoint that callers sign/publish with the disclosure-log signing key.
    pub fn checkpoint(&self, origin: impl Into<String>) -> Checkpoint {
        Checkpoint {
            origin: origin.into(),
            size: self.leaf_count(),
            root: self.root,
        }
    }

    /// Requests whose durable intent has no matching completion, normally because a process or
    /// storage device failed during the operation.
    pub fn incomplete_request_ids(&self) -> Vec<[u8; 32]> {
        let mut pending: Vec<_> = self
            .incomplete
            .iter()
            .map(|(request_id, intent)| (intent.sequence, *request_id))
            .collect();
        pending.sort_unstable_by_key(|(sequence, _)| *sequence);
        pending
            .into_iter()
            .map(|(_, request_id)| request_id)
            .collect()
    }

    /// Close a crash-interrupted intent with a non-descriptive public failure record.
    pub fn close_incomplete_failure(
        &mut self,
        request_id: [u8; 32],
        timestamp_ms: u64,
    ) -> Result<()> {
        if !self.incomplete.contains_key(&request_id) {
            return Err(ComplianceError::StateConflict);
        }
        self.append_failure(request_id, timestamp_ms)
    }

    /// Gate every custody operation behind an intent leaf and append success/failure completion.
    pub fn execute<T>(
        &mut self,
        request: &mut DisclosureRequest,
        started_at_ms: u64,
        completion_clock: impl FnOnce() -> Result<u64>,
        operation: impl FnOnce(&AuthorizedDisclosure) -> Result<DisclosureOutput<T>>,
    ) -> Result<T> {
        request.ensure_authorized(started_at_ms)?;
        self.append(DisclosureLeaf::Intent(DisclosureIntent {
            request_id: request.request_id(),
            timestamp_ms: started_at_ms,
            jurisdiction: request.jurisdiction(),
            purpose: request.purpose(),
            key_ids: request.key_ids().to_vec(),
            order_commitment: request.order_commitment(),
            requester_commitment: request.requester_commitment(),
            selector_commitment: request.selector_commitment(),
            approver_commitments: request.approver_commitments().to_vec(),
        }))?;
        let authorization = request.consume(started_at_ms)?;
        let operation_result = operation(&authorization);
        // Sample after the custody operation has returned and immediately before the terminal
        // append. Supplying a timestamp captured before work begins would make the public record
        // materially misleading for slow or failed operations.
        let completed_at_ms = completion_clock()?;
        if completed_at_ms < started_at_ms {
            return Err(ComplianceError::InvalidRequest);
        }
        match operation_result {
            Ok(output) if output.result_commitment != [0u8; 32] => {
                self.append(DisclosureLeaf::Completion(DisclosureCompletion {
                    request_id: authorization.request_id(),
                    timestamp_ms: completed_at_ms,
                    status: CompletionStatus::Succeeded,
                    record_count: output.record_count,
                    result_commitment: output.result_commitment,
                }))?;
                Ok(output.value)
            }
            Ok(_) => {
                self.append_failure(authorization.request_id(), completed_at_ms)?;
                Err(ComplianceError::InvalidRequest)
            }
            Err(error) => {
                self.append_failure(authorization.request_id(), completed_at_ms)?;
                Err(error)
            }
        }
    }

    fn append_failure(&mut self, request_id: [u8; 32], timestamp_ms: u64) -> Result<()> {
        self.append(DisclosureLeaf::Completion(DisclosureCompletion {
            request_id,
            timestamp_ms,
            status: CompletionStatus::Failed,
            record_count: 0,
            result_commitment: Sha256::digest(GENERIC_FAILURE_DOMAIN).into(),
        }))
    }

    fn append(&mut self, leaf: DisclosureLeaf) -> Result<()> {
        if self.state_poisoned {
            return Err(ComplianceError::Storage);
        }
        if self.leaf_count() as usize >= MAX_DISCLOSURE_LEAVES {
            return Err(ComplianceError::LimitExceeded);
        }
        validate_leaf_sequence(&self.request_ids, &self.incomplete, &leaf)?;
        let encoded = leaf.encode()?;
        if encoded.len() > MAX_LEAF_BYTES {
            return Err(ComplianceError::LimitExceeded);
        }
        let hash = leaf_hash(&encoded);
        let mut next_frontier = self.frontier.clone();
        next_frontier
            .append_hash(hash)
            .ok_or(ComplianceError::LimitExceeded)?;
        let next_root = next_frontier.root().ok_or(ComplianceError::Storage)?;
        let mut next_tail_frontier = self.proof_tail_frontier.clone();
        next_tail_frontier
            .append_hash(hash)
            .ok_or(ComplianceError::LimitExceeded)?;
        let completed_block_root = if self.proof_tail_hashes.len() + 1 == PROOF_BLOCK_LEAVES {
            Some(next_tail_frontier.root().ok_or(ComplianceError::Storage)?)
        } else {
            None
        };

        if self.file.is_some() {
            if self.validate_current_store_file_stamp().is_err() {
                self.state_poisoned = true;
                return Err(ComplianceError::Storage);
            }
            self.state_store
                .as_mut()
                .ok_or(ComplianceError::Storage)?
                .pending_append = Some(encoded.clone());
            if self.persist_state().is_err() {
                self.state_poisoned = true;
                return Err(ComplianceError::Storage);
            }
        }

        let mut payload_offset = None;
        let mut durable_len = None;
        if let Some(file) = &self.file {
            self.verify_ledger_custody()?;
            let append_result = (|| {
                let mut file = file.lock().map_err(|_| ComplianceError::Storage)?;
                let record_offset = file
                    .seek(SeekFrom::End(0))
                    .map_err(|_| ComplianceError::Storage)?;
                let next_len = record_offset
                    .checked_add(4)
                    .and_then(|value| value.checked_add(encoded.len() as u64))
                    .ok_or(ComplianceError::LimitExceeded)?;
                if next_len > MAX_LOG_BYTES {
                    return Err(ComplianceError::LimitExceeded);
                }
                file.write_all(&(encoded.len() as u32).to_be_bytes())
                    .and_then(|_| file.write_all(&encoded))
                    .and_then(|_| file.sync_data())
                    .map_err(|_| ComplianceError::Storage)?;
                Ok((record_offset, next_len))
            })();
            let (record_offset, next_len) = match append_result {
                Ok(offsets) => offsets,
                Err(error) => {
                    self.state_poisoned = true;
                    return Err(error);
                }
            };
            self.verify_ledger_custody()?;
            payload_offset = Some(record_offset + 4);
            durable_len = Some(next_len);
        }
        let sequence = self.leaf_count();
        if let Some(offset) = payload_offset {
            self.leaf_offsets.push(offset);
        } else {
            self.memory_leaves.push(encoded);
        }
        self.frontier = next_frontier;
        self.root = next_root;
        if let Some(block_root) = completed_block_root {
            self.proof_block_roots.push(block_root);
            self.proof_tail_frontier = MerkleFrontier::new();
            self.proof_tail_hashes.clear();
        } else {
            self.proof_tail_frontier = next_tail_frontier;
            self.proof_tail_hashes.push(hash);
        }
        record_leaf_state(&mut self.request_ids, &mut self.incomplete, &leaf, sequence);
        if let Some(log_len) = durable_len {
            let generation = self.leaf_count();
            let store = self.state_store.as_mut().ok_or(ComplianceError::Storage)?;
            store.generation = generation;
            store.log_len = log_len;
            store.last_leaf_hash = Some(hash);
            store.pending_append = None;
            if self
                .refresh_store_file_stamp()
                .and_then(|_| self.persist_state())
                .is_err()
            {
                self.state_poisoned = true;
                return Err(ComplianceError::Storage);
            }
        }
        Ok(())
    }

    fn absorb_hash(&mut self, hash: Hash) -> Result<()> {
        self.frontier
            .append_hash(hash)
            .ok_or(ComplianceError::LimitExceeded)?;
        self.root = self.frontier.root().ok_or(ComplianceError::Storage)?;
        self.proof_tail_frontier
            .append_hash(hash)
            .ok_or(ComplianceError::LimitExceeded)?;
        self.proof_tail_hashes.push(hash);
        if self.proof_tail_hashes.len() == PROOF_BLOCK_LEAVES {
            self.proof_block_roots.push(
                self.proof_tail_frontier
                    .root()
                    .ok_or(ComplianceError::Storage)?,
            );
            self.proof_tail_frontier = MerkleFrontier::new();
            self.proof_tail_hashes.clear();
        }
        Ok(())
    }

    fn encoded_leaf(&self, index: u64) -> Result<Option<Vec<u8>>> {
        if index >= self.leaf_count() {
            return Ok(None);
        }
        if let Some(file) = &self.file {
            self.verify_ledger_custody()?;
            let payload_offset = *self
                .leaf_offsets
                .get(index as usize)
                .ok_or(ComplianceError::Storage)?;
            let mut file = file.lock().map_err(|_| ComplianceError::Storage)?;
            file.seek(SeekFrom::Start(payload_offset - 4))
                .map_err(|_| ComplianceError::Storage)?;
            let mut length_bytes = [0u8; 4];
            file.read_exact(&mut length_bytes)
                .map_err(|_| ComplianceError::Storage)?;
            let length = u32::from_be_bytes(length_bytes) as usize;
            if length == 0 || length > MAX_LEAF_BYTES {
                return Err(ComplianceError::Storage);
            }
            let mut encoded = vec![0u8; length];
            file.read_exact(&mut encoded)
                .map_err(|_| ComplianceError::Storage)?;
            drop(file);
            self.verify_ledger_custody()?;
            Ok(Some(encoded))
        } else {
            self.memory_leaves
                .get(index as usize)
                .cloned()
                .map(Some)
                .ok_or(ComplianceError::Storage)
        }
    }

    fn leaf_hash_at(&self, index: u64) -> Result<Hash> {
        let completed_leaves = self.proof_block_roots.len() * PROOF_BLOCK_LEAVES;
        let index_usize = usize::try_from(index).map_err(|_| ComplianceError::LimitExceeded)?;
        if index_usize >= completed_leaves {
            return self
                .proof_tail_hashes
                .get(index_usize - completed_leaves)
                .copied()
                .ok_or(ComplianceError::Storage);
        }
        let encoded = self.encoded_leaf(index)?.ok_or(ComplianceError::Storage)?;
        Ok(leaf_hash(&encoded))
    }

    fn range_root(&self, start: u64, count: u64) -> Result<Hash> {
        if count == 0 {
            return Ok(empty_root());
        }
        let block = PROOF_BLOCK_LEAVES as u64;
        if start % block == 0 && count % block == 0 {
            let first =
                usize::try_from(start / block).map_err(|_| ComplianceError::LimitExceeded)?;
            let blocks =
                usize::try_from(count / block).map_err(|_| ComplianceError::LimitExceeded)?;
            if first
                .checked_add(blocks)
                .is_some_and(|end| end <= self.proof_block_roots.len())
            {
                return Ok(hash_range(&self.proof_block_roots[first..first + blocks]));
            }
        }
        if count == 1 {
            return self.leaf_hash_at(start);
        }
        let split = merkle_split(count);
        let left = self.range_root(start, split)?;
        let right = self.range_root(start + split, count - split)?;
        Ok(node_hash(&left, &right))
    }

    fn inclusion_path(&self, start: u64, count: u64, index: u64) -> Result<Vec<Hash>> {
        if count <= 1 {
            return Ok(Vec::new());
        }
        let split = merkle_split(count);
        if index < start + split {
            let mut proof = self.inclusion_path(start, split, index)?;
            proof.push(self.range_root(start + split, count - split)?);
            Ok(proof)
        } else {
            let mut proof = self.inclusion_path(start + split, count - split, index)?;
            proof.push(self.range_root(start, split)?);
            Ok(proof)
        }
    }

    fn consistency_path(
        &self,
        start: u64,
        count: u64,
        old_count: u64,
        is_root: bool,
    ) -> Result<Vec<Hash>> {
        if old_count == count {
            return if is_root {
                Ok(Vec::new())
            } else {
                Ok(vec![self.range_root(start, count)?])
            };
        }
        let split = merkle_split(count);
        if old_count <= split {
            let mut proof = self.consistency_path(start, split, old_count, is_root)?;
            proof.push(self.range_root(start + split, count - split)?);
            Ok(proof)
        } else {
            let mut proof =
                self.consistency_path(start + split, count - split, old_count - split, false)?;
            proof.push(self.range_root(start, split)?);
            Ok(proof)
        }
    }
}

impl PersistedLedgerState {
    fn encode_authenticated(&self, authentication_key: &[u8; 32]) -> Result<Vec<u8>> {
        let mut payload = Vec::new();
        payload.extend_from_slice(&self.file_identity);
        payload.extend_from_slice(&self.mutation_marker);
        payload.extend_from_slice(&self.generation.to_be_bytes());
        payload.extend_from_slice(&self.log_len.to_be_bytes());
        push_optional_hash(&mut payload, self.last_leaf_hash);
        push_optional_state_bytes(&mut payload, self.pending_append.as_deref())?;
        payload.extend_from_slice(&self.root);

        let mut encoded = Vec::with_capacity(STATE_MAGIC.len() + 1 + 8 + payload.len() + 32);
        encoded.extend_from_slice(STATE_MAGIC);
        encoded.push(STATE_VERSION);
        encoded.extend_from_slice(&(payload.len() as u64).to_be_bytes());
        encoded.extend_from_slice(&payload);
        let tag = state_authentication_tag(authentication_key, &encoded)?;
        encoded.extend_from_slice(&tag);
        if encoded.len() as u64 > MAX_STATE_BYTES {
            return Err(ComplianceError::LimitExceeded);
        }
        Ok(encoded)
    }

    fn decode_authenticated(bytes: &[u8], authentication_key: &[u8; 32]) -> Result<Self> {
        let prefix_len = STATE_MAGIC.len() + 1 + 8;
        if bytes.len() < prefix_len + 32
            || bytes.len() as u64 > MAX_STATE_BYTES
            || &bytes[..STATE_MAGIC.len()] != STATE_MAGIC
            || bytes[STATE_MAGIC.len()] != STATE_VERSION
        {
            return Err(ComplianceError::Storage);
        }
        let payload_len = u64::from_be_bytes(
            bytes[STATE_MAGIC.len() + 1..prefix_len]
                .try_into()
                .expect("fixed state length"),
        );
        let authenticated_len = prefix_len
            .checked_add(usize::try_from(payload_len).map_err(|_| ComplianceError::LimitExceeded)?)
            .ok_or(ComplianceError::LimitExceeded)?;
        if authenticated_len
            .checked_add(32)
            .filter(|length| *length == bytes.len())
            .is_none()
        {
            return Err(ComplianceError::Storage);
        }
        verify_state_authentication_tag(
            authentication_key,
            &bytes[..authenticated_len],
            &bytes[authenticated_len..],
        )?;
        let mut reader = StateReader::new(&bytes[prefix_len..authenticated_len]);
        let file_identity = reader.array32()?;
        let mutation_marker = reader.array16()?;
        let generation = reader.u64()?;
        let log_len = reader.u64()?;
        let last_leaf_hash = reader.optional_hash()?;
        let pending_append = reader.optional_bytes(MAX_LEAF_BYTES)?;
        let root = reader.array32()?;
        if !reader.finished() {
            return Err(ComplianceError::Storage);
        }
        Ok(Self {
            file_identity,
            mutation_marker,
            generation,
            log_len,
            last_leaf_hash,
            pending_append,
            root,
        })
    }

    fn validate(
        &self,
        path: &Path,
        file: &mut File,
        file_len: u64,
        actual_identity: [u8; 32],
        actual_marker: [u8; 16],
    ) -> Result<()> {
        let recovery_tail_len = file_len
            .checked_sub(self.log_len)
            .ok_or(ComplianceError::Storage)?;
        let expected_recovery_len = self
            .pending_append
            .as_ref()
            .map(|encoded| {
                4u64.checked_add(encoded.len() as u64)
                    .ok_or(ComplianceError::LimitExceeded)
            })
            .transpose()?;
        if self.file_identity != actual_identity
            || self.generation > MAX_DISCLOSURE_LEAVES as u64
            || self.log_len < LOG_MAGIC.len() as u64
            || self.log_len > file_len
            || recovery_tail_len > MAX_RECOVERY_TAIL_BYTES
            || expected_recovery_len.is_none() && recovery_tail_len != 0
            || expected_recovery_len.is_some_and(|expected| recovery_tail_len > expected)
            || (recovery_tail_len == 0 && self.mutation_marker != actual_marker)
        {
            return Err(ComplianceError::Storage);
        }
        #[cfg(not(unix))]
        if !named_ledger_matches(file, path) {
            return Err(ComplianceError::Storage);
        }
        #[cfg(unix)]
        let _ = (path, file);
        if self.pending_append.is_some() && self.generation >= MAX_DISCLOSURE_LEAVES as u64 {
            return Err(ComplianceError::LimitExceeded);
        }
        if self.generation == 0 {
            if self.log_len != LOG_MAGIC.len() as u64
                || self.last_leaf_hash.is_some()
                || self.root != empty_root()
            {
                return Err(ComplianceError::Storage);
            }
        } else if self.log_len == LOG_MAGIC.len() as u64 || self.last_leaf_hash.is_none() {
            return Err(ComplianceError::Storage);
        }
        if let Some(encoded) = self.pending_append.as_deref() {
            if encoded.is_empty() || encoded.len() > MAX_LEAF_BYTES {
                return Err(ComplianceError::LimitExceeded);
            }
        }
        Ok(())
    }
}

struct StateReader<'a> {
    bytes: &'a [u8],
    cursor: usize,
}

impl<'a> StateReader<'a> {
    const fn new(bytes: &'a [u8]) -> Self {
        Self { bytes, cursor: 0 }
    }

    fn take(&mut self, length: usize) -> Result<&'a [u8]> {
        let end = self
            .cursor
            .checked_add(length)
            .ok_or(ComplianceError::LimitExceeded)?;
        let value = self
            .bytes
            .get(self.cursor..end)
            .ok_or(ComplianceError::Storage)?;
        self.cursor = end;
        Ok(value)
    }

    fn u32(&mut self) -> Result<u32> {
        Ok(u32::from_be_bytes(
            self.take(4)?.try_into().expect("fixed state u32"),
        ))
    }

    fn u64(&mut self) -> Result<u64> {
        Ok(u64::from_be_bytes(
            self.take(8)?.try_into().expect("fixed state u64"),
        ))
    }

    fn array16(&mut self) -> Result<[u8; 16]> {
        Ok(self.take(16)?.try_into().expect("fixed state array"))
    }

    fn array32(&mut self) -> Result<[u8; 32]> {
        Ok(self.take(32)?.try_into().expect("fixed state array"))
    }

    fn optional_hash(&mut self) -> Result<Option<Hash>> {
        let present = self.take(1)?[0];
        let hash = self.array32()?;
        match (present, hash) {
            (0, hash) if hash == [0u8; 32] => Ok(None),
            (1, hash) => Ok(Some(hash)),
            _ => Err(ComplianceError::Storage),
        }
    }

    fn bytes(&mut self) -> Result<&'a [u8]> {
        let length = usize::try_from(self.u32()?).map_err(|_| ComplianceError::LimitExceeded)?;
        self.take(length)
    }

    fn optional_bytes(&mut self, maximum: usize) -> Result<Option<Vec<u8>>> {
        match self.take(1)?[0] {
            0 => Ok(None),
            1 => {
                let bytes = self.bytes()?;
                if bytes.is_empty() || bytes.len() > maximum {
                    return Err(ComplianceError::LimitExceeded);
                }
                Ok(Some(bytes.to_vec()))
            }
            _ => Err(ComplianceError::Storage),
        }
    }

    fn finished(&self) -> bool {
        self.cursor == self.bytes.len()
    }
}

pub(crate) fn disclosure_state_path(path: &Path) -> Result<PathBuf> {
    let file_name = path.file_name().ok_or(ComplianceError::Storage)?;
    let mut state_name = file_name.to_os_string();
    state_name.push(".state");
    Ok(path.with_file_name(state_name))
}

#[cfg(unix)]
fn leaf_for_path(path: &Path) -> Result<LeafName> {
    LeafName::new(path.file_name().ok_or(ComplianceError::Storage)?).map_err(map_custody_error)
}

#[cfg(unix)]
fn map_custody_error(error: CustodyError) -> ComplianceError {
    match error {
        CustodyError::LimitExceeded(_) => ComplianceError::LimitExceeded,
        _ => ComplianceError::Storage,
    }
}

fn derive_state_authentication_key(secret: &[u8; 32]) -> Zeroizing<[u8; 32]> {
    let mut hash = Sha256::new();
    hash.update(STATE_KEY_DOMAIN);
    hash.update(secret);
    Zeroizing::new(hash.finalize().into())
}

fn state_authentication_tag(key: &[u8; 32], bytes: &[u8]) -> Result<[u8; 32]> {
    let mut mac = Hmac::<Sha256>::new_from_slice(key).map_err(|_| ComplianceError::Storage)?;
    mac.update(STATE_AUTH_DOMAIN);
    mac.update(bytes);
    Ok(mac.finalize().into_bytes().into())
}

fn verify_state_authentication_tag(key: &[u8; 32], bytes: &[u8], tag: &[u8]) -> Result<()> {
    let mut mac = Hmac::<Sha256>::new_from_slice(key).map_err(|_| ComplianceError::Storage)?;
    mac.update(STATE_AUTH_DOMAIN);
    mac.update(bytes);
    mac.verify_slice(tag).map_err(|_| ComplianceError::Storage)
}

fn push_optional_hash(out: &mut Vec<u8>, hash: Option<Hash>) {
    out.push(u8::from(hash.is_some()));
    out.extend_from_slice(&hash.unwrap_or([0u8; 32]));
}

fn push_optional_state_bytes(out: &mut Vec<u8>, bytes: Option<&[u8]>) -> Result<()> {
    out.push(u8::from(bytes.is_some()));
    if let Some(bytes) = bytes {
        if bytes.is_empty() || bytes.len() > MAX_LEAF_BYTES {
            return Err(ComplianceError::LimitExceeded);
        }
        push_state_bytes(out, bytes)?;
    }
    Ok(())
}

fn push_state_u32(out: &mut Vec<u8>, value: usize) -> Result<()> {
    let value = u32::try_from(value).map_err(|_| ComplianceError::LimitExceeded)?;
    out.extend_from_slice(&value.to_be_bytes());
    Ok(())
}

fn push_state_bytes(out: &mut Vec<u8>, bytes: &[u8]) -> Result<()> {
    push_state_u32(out, bytes.len())?;
    out.extend_from_slice(bytes);
    Ok(())
}

#[cfg(not(unix))]
fn read_persisted_state(
    path: &Path,
    authentication_key: &[u8; 32],
) -> Result<PersistedLedgerState> {
    let mut file = open_private_ledger(path)?;
    validate_opened_ledger(&file, path, MAX_STATE_BYTES)?;
    let length = file.metadata().map_err(|_| ComplianceError::Storage)?.len();
    let bytes = read_bounded_state_bytes(&mut file, length, MAX_STATE_BYTES)?;
    if file.metadata().map_err(|_| ComplianceError::Storage)?.len() != length
        || !named_ledger_matches(&file, path)
    {
        return Err(ComplianceError::Storage);
    }
    PersistedLedgerState::decode_authenticated(&bytes, authentication_key)
}

#[cfg(unix)]
fn read_persisted_state_guarded(
    guard: &GuardedFile,
    authentication_key: &[u8; 32],
) -> Result<PersistedLedgerState> {
    guard.verify_named().map_err(map_custody_error)?;
    let metadata = guard.metadata().map_err(map_custody_error)?;
    let mut file = guard
        .file()
        .try_clone()
        .map_err(|_| ComplianceError::Storage)?;
    file.seek(SeekFrom::Start(0))
        .map_err(|_| ComplianceError::Storage)?;
    let bytes = read_bounded_state_bytes(&mut file, metadata.len, MAX_STATE_BYTES)?;
    guard.verify_named().map_err(map_custody_error)?;
    if guard.metadata().map_err(map_custody_error)? != metadata
        || bytes.len() as u64 != metadata.len
    {
        return Err(ComplianceError::Storage);
    }
    PersistedLedgerState::decode_authenticated(&bytes, authentication_key)
}

fn read_bounded_state_bytes<R: Read>(
    reader: &mut R,
    announced_length: u64,
    maximum: u64,
) -> Result<Vec<u8>> {
    if announced_length > maximum {
        return Err(ComplianceError::LimitExceeded);
    }
    let mut bytes = Vec::with_capacity(
        usize::try_from(announced_length).map_err(|_| ComplianceError::LimitExceeded)?,
    );
    let read_limit = maximum
        .checked_add(1)
        .ok_or(ComplianceError::LimitExceeded)?;
    reader
        .take(read_limit)
        .read_to_end(&mut bytes)
        .map_err(|_| ComplianceError::Storage)?;
    if bytes.len() as u64 > maximum {
        return Err(ComplianceError::LimitExceeded);
    }
    Ok(bytes)
}

#[cfg(not(unix))]
fn write_private_state_atomic(path: &Path, bytes: &[u8]) -> Result<()> {
    if bytes.len() as u64 > MAX_STATE_BYTES {
        return Err(ComplianceError::LimitExceeded);
    }
    let parent = path.parent().ok_or(ComplianceError::Storage)?;
    secure_private_parent(parent)?;
    if path.try_exists().map_err(|_| ComplianceError::Storage)? {
        let existing = open_private_ledger(path)?;
        validate_opened_ledger(&existing, path, MAX_STATE_BYTES)?;
    }
    let file_name = path
        .file_name()
        .and_then(|value| value.to_str())
        .ok_or(ComplianceError::Storage)?;
    let mut nonce = [0u8; 16];
    OsRng.fill_bytes(&mut nonce);
    let temp = parent.join(format!(".{file_name}.{}.tmp", hex_bytes(&nonce)));
    let mut options = OpenOptions::new();
    options.create_new(true).write(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.mode(0o600);
    }
    let result = (|| {
        let mut file = options.open(&temp).map_err(|_| ComplianceError::Storage)?;
        file.write_all(bytes)
            .and_then(|_| file.sync_all())
            .map_err(|_| ComplianceError::Storage)?;
        validate_opened_ledger(&file, &temp, MAX_STATE_BYTES)?;
        fs::rename(&temp, path).map_err(|_| ComplianceError::Storage)?;
        let published = open_private_ledger(path)?;
        validate_opened_ledger(&published, path, MAX_STATE_BYTES)?;
        sync_parent(parent)
    })();
    if result.is_err() {
        let _ = fs::remove_file(&temp);
    }
    result
}

#[cfg(unix)]
fn write_private_state_guarded(custody: &LedgerCustody, bytes: &[u8]) -> Result<()> {
    if bytes.len() as u64 > MAX_STATE_BYTES {
        return Err(ComplianceError::LimitExceeded);
    }
    custody
        .directory
        .verify_named()
        .and_then(|_| custody.ledger.verify_named())
        .map_err(map_custody_error)?;
    let mut state = custody.state.lock().map_err(|_| ComplianceError::Storage)?;
    if let Some(existing) = state.as_ref() {
        existing.verify_named().map_err(map_custody_error)?;
    } else if custody
        .directory
        .entry_metadata(&custody.state_name)
        .map_err(map_custody_error)?
        .is_some()
    {
        return Err(ComplianceError::Storage);
    }

    let mut nonce = [0u8; 16];
    OsRng.fill_bytes(&mut nonce);
    let temp_name = LeafName::new(format!(
        ".{}.{}.tmp",
        custody.state_name.as_os_str().to_string_lossy(),
        hex_bytes(&nonce)
    ))
    .map_err(map_custody_error)?;
    let mut temp = custody
        .directory
        .create_file(&temp_name, FilePolicy::private(MAX_STATE_BYTES))
        .map_err(map_custody_error)?;
    temp.write_all(bytes)
        .map_err(|_| ComplianceError::Storage)?;
    temp.sync_all().map_err(map_custody_error)?;
    if temp.metadata().map_err(map_custody_error)?.len != bytes.len() as u64 {
        return Err(ComplianceError::Storage);
    }
    let cleanup = custody
        .directory
        .open_file(
            &temp_name,
            OpenAccess::ReadOnly,
            FilePolicy::private(MAX_STATE_BYTES),
        )
        .map_err(map_custody_error)?;

    let publication = if let Some(existing) = state.as_ref() {
        // Deterministic replacement before the publication effect fails here. The retained 0700
        // parent is the platform custody boundary for the final verify-to-rename interval.
        existing.verify_named().map_err(map_custody_error)?;
        custody
            .directory
            .rename_replace(temp, &custody.directory, &custody.state_name)
    } else {
        custody
            .directory
            .publish_no_replace(temp, &custody.directory, &custody.state_name)
    };
    let published = match publication {
        Ok(published) => published,
        Err(error) => {
            let _ = custody.directory.unlink_file(cleanup);
            return Err(map_custody_error(error));
        }
    };
    published.verify_named().map_err(map_custody_error)?;
    if published.metadata().map_err(map_custody_error)?.len != bytes.len() as u64 {
        return Err(ComplianceError::Storage);
    }
    custody.directory.sync().map_err(map_custody_error)?;
    *state = Some(published);
    Ok(())
}

fn read_record_at(
    file: &mut File,
    payload_offset: u64,
    maximum_end: u64,
) -> Result<(Vec<u8>, u64)> {
    let record_offset = payload_offset
        .checked_sub(4)
        .ok_or(ComplianceError::Storage)?;
    file.seek(SeekFrom::Start(record_offset))
        .map_err(|_| ComplianceError::Storage)?;
    let mut length_bytes = [0u8; 4];
    file.read_exact(&mut length_bytes)
        .map_err(|_| ComplianceError::Storage)?;
    let length = u32::from_be_bytes(length_bytes) as usize;
    if length == 0 || length > MAX_LEAF_BYTES {
        return Err(ComplianceError::Storage);
    }
    let end = payload_offset
        .checked_add(length as u64)
        .filter(|end| *end <= maximum_end)
        .ok_or(ComplianceError::Storage)?;
    let mut encoded = vec![0u8; length];
    file.read_exact(&mut encoded)
        .map_err(|_| ComplianceError::Storage)?;
    Ok((encoded, end))
}

fn hex_bytes(bytes: &[u8]) -> String {
    const HEX: &[u8; 16] = b"0123456789abcdef";
    let mut output = String::with_capacity(bytes.len() * 2);
    for byte in bytes {
        output.push(HEX[(byte >> 4) as usize] as char);
        output.push(HEX[(byte & 0x0f) as usize] as char);
    }
    output
}

#[cfg(unix)]
fn ledger_file_identity(_path: &Path, metadata: &fs::Metadata) -> Result<[u8; 32]> {
    use std::os::unix::fs::MetadataExt;

    let mut hash = Sha256::new();
    hash.update(LEDGER_IDENTITY_DOMAIN);
    hash.update(metadata.dev().to_be_bytes());
    hash.update(metadata.ino().to_be_bytes());
    Ok(hash.finalize().into())
}

#[cfg(unix)]
fn ledger_mutation_marker(metadata: &fs::Metadata) -> Result<[u8; 16]> {
    use std::os::unix::fs::MetadataExt;

    let mut marker = [0u8; 16];
    marker[..8].copy_from_slice(&metadata.ctime().to_be_bytes());
    marker[8..].copy_from_slice(&metadata.ctime_nsec().to_be_bytes());
    Ok(marker)
}

#[cfg(windows)]
fn ledger_file_identity(path: &Path, metadata: &fs::Metadata) -> Result<[u8; 32]> {
    use std::os::windows::fs::MetadataExt;

    let canonical = fs::canonicalize(path).map_err(|_| ComplianceError::Storage)?;
    let mut hash = Sha256::new();
    hash.update(LEDGER_IDENTITY_DOMAIN);
    hash.update(canonical.to_string_lossy().as_bytes());
    hash.update(metadata.creation_time().to_be_bytes());
    Ok(hash.finalize().into())
}

#[cfg(windows)]
fn ledger_mutation_marker(metadata: &fs::Metadata) -> Result<[u8; 16]> {
    use std::os::windows::fs::MetadataExt;

    let mut marker = [0u8; 16];
    marker[..8].copy_from_slice(&metadata.last_write_time().to_be_bytes());
    marker[8..].copy_from_slice(&metadata.file_size().to_be_bytes());
    Ok(marker)
}

#[cfg(not(any(unix, windows)))]
fn ledger_file_identity(path: &Path, _metadata: &fs::Metadata) -> Result<[u8; 32]> {
    let canonical = fs::canonicalize(path).map_err(|_| ComplianceError::Storage)?;
    let mut hash = Sha256::new();
    hash.update(LEDGER_IDENTITY_DOMAIN);
    hash.update(canonical.to_string_lossy().as_bytes());
    Ok(hash.finalize().into())
}

#[cfg(not(any(unix, windows)))]
fn ledger_mutation_marker(metadata: &fs::Metadata) -> Result<[u8; 16]> {
    let modified = metadata
        .modified()
        .map_err(|_| ComplianceError::Storage)?
        .duration_since(std::time::UNIX_EPOCH)
        .map_err(|_| ComplianceError::Storage)?
        .as_nanos();
    Ok(modified.to_be_bytes())
}

#[cfg(all(unix, test))]
fn open_private_ledger(path: &Path) -> Result<File> {
    use rustix::fs::{Mode, OFlags};

    let owned = rustix::fs::open(
        path,
        OFlags::RDWR | OFlags::NOFOLLOW | OFlags::CLOEXEC,
        Mode::empty(),
    )
    .map_err(|_| ComplianceError::Storage)?;
    let file = File::from(owned);
    validate_opened_ledger(&file, path, MAX_LOG_BYTES)?;
    Ok(file)
}

#[cfg(windows)]
fn open_private_ledger(path: &Path) -> Result<File> {
    use std::os::windows::fs::OpenOptionsExt;

    const FILE_FLAG_OPEN_REPARSE_POINT: u32 = 0x0020_0000;
    let mut options = OpenOptions::new();
    options
        .read(true)
        .write(true)
        .custom_flags(FILE_FLAG_OPEN_REPARSE_POINT);
    let file = options.open(path).map_err(|_| ComplianceError::Storage)?;
    validate_opened_ledger(&file, path, MAX_LOG_BYTES)?;
    Ok(file)
}

#[cfg(not(any(unix, windows)))]
fn open_private_ledger(path: &Path) -> Result<File> {
    let before = fs::symlink_metadata(path).map_err(|_| ComplianceError::Storage)?;
    if before.file_type().is_symlink() || !before.is_file() {
        return Err(ComplianceError::Storage);
    }
    let file = OpenOptions::new()
        .read(true)
        .write(true)
        .open(path)
        .map_err(|_| ComplianceError::Storage)?;
    validate_opened_ledger(&file, path, MAX_LOG_BYTES)?;
    Ok(file)
}

#[cfg(any(not(unix), test))]
fn validate_opened_ledger(file: &File, path: &Path, max_bytes: u64) -> Result<()> {
    let metadata = file.metadata().map_err(|_| ComplianceError::Storage)?;
    if !metadata.is_file() || metadata.len() > max_bytes || !named_ledger_matches(file, path) {
        return Err(ComplianceError::Storage);
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::MetadataExt;

        if metadata.uid() != rustix::process::geteuid().as_raw()
            || metadata.nlink() != 1
            || metadata.mode() & 0o077 != 0
        {
            return Err(ComplianceError::Storage);
        }
    }
    #[cfg(windows)]
    {
        use std::os::windows::fs::MetadataExt;
        const FILE_ATTRIBUTE_REPARSE_POINT: u32 = 0x0000_0400;
        if metadata.file_attributes() & FILE_ATTRIBUTE_REPARSE_POINT != 0 {
            return Err(ComplianceError::Storage);
        }
    }
    Ok(())
}

#[cfg(all(unix, test))]
fn named_ledger_matches(file: &File, path: &Path) -> bool {
    use std::os::unix::fs::MetadataExt;

    let Ok(opened) = file.metadata() else {
        return false;
    };
    let Ok(named) = fs::symlink_metadata(path) else {
        return false;
    };
    named.is_file() && named.dev() == opened.dev() && named.ino() == opened.ino()
}

#[cfg(not(unix))]
fn named_ledger_matches(file: &File, path: &Path) -> bool {
    file.metadata().is_ok_and(|metadata| metadata.is_file())
        && fs::symlink_metadata(path)
            .is_ok_and(|metadata| !metadata.file_type().is_symlink() && metadata.is_file())
}

#[cfg(not(unix))]
fn secure_private_parent(parent: &Path) -> Result<()> {
    fs::create_dir_all(parent).map_err(|_| ComplianceError::Storage)?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::MetadataExt;

        use rustix::fs::{Mode, OFlags};

        let owned = rustix::fs::open(
            parent,
            OFlags::RDONLY | OFlags::DIRECTORY | OFlags::NOFOLLOW | OFlags::CLOEXEC,
            Mode::empty(),
        )
        .map_err(|_| ComplianceError::Storage)?;
        let directory = File::from(owned);
        rustix::fs::fchmod(&directory, Mode::from_raw_mode(0o700))
            .map_err(|_| ComplianceError::Storage)?;
        let opened = directory.metadata().map_err(|_| ComplianceError::Storage)?;
        let named = fs::symlink_metadata(parent).map_err(|_| ComplianceError::Storage)?;
        if !opened.is_dir()
            || opened.uid() != rustix::process::geteuid().as_raw()
            || opened.mode() & 0o077 != 0
            || !named.is_dir()
            || named.dev() != opened.dev()
            || named.ino() != opened.ino()
        {
            return Err(ComplianceError::Storage);
        }
    }
    #[cfg(not(unix))]
    {
        let metadata = fs::symlink_metadata(parent).map_err(|_| ComplianceError::Storage)?;
        if metadata.file_type().is_symlink() || !metadata.is_dir() {
            return Err(ComplianceError::Storage);
        }
    }
    Ok(())
}

impl core::fmt::Debug for DisclosureLedger {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        f.debug_struct("DisclosureLedger")
            .field("storage", &"<withheld>")
            .field("leaf_count", &self.leaf_count())
            .field("root", &self.root)
            .finish()
    }
}

fn node_hash(left: &Hash, right: &Hash) -> Hash {
    let mut hasher = Sha256::new();
    hasher.update([0x01]);
    hasher.update(left);
    hasher.update(right);
    hasher.finalize().into()
}

fn merkle_split(count: u64) -> u64 {
    debug_assert!(count > 1);
    1u64 << (u64::BITS - 1 - (count - 1).leading_zeros())
}

fn hash_range(hashes: &[Hash]) -> Hash {
    match hashes.len() {
        0 => empty_root(),
        1 => hashes[0],
        count => {
            let split = merkle_split(count as u64) as usize;
            node_hash(&hash_range(&hashes[..split]), &hash_range(&hashes[split..]))
        }
    }
}

fn read_fixed32(bytes: &[u8], cursor: &mut usize) -> [u8; 32] {
    let mut value = [0u8; 32];
    value.copy_from_slice(&bytes[*cursor..*cursor + 32]);
    *cursor += 32;
    value
}

fn strictly_sorted_key_ids(key_ids: &[ComplianceKeyId]) -> bool {
    key_ids
        .windows(2)
        .all(|pair| match (pair[0].encode(), pair[1].encode()) {
            (Ok(left), Ok(right)) => left < right,
            _ => false,
        })
}

fn validate_leaf_sequence(
    request_ids: &HashSet<[u8; 32]>,
    incomplete: &HashMap<[u8; 32], PendingIntent>,
    next: &DisclosureLeaf,
) -> Result<()> {
    match next {
        DisclosureLeaf::Intent(intent) => {
            if request_ids.contains(&intent.request_id) {
                return Err(ComplianceError::StateConflict);
            }
        }
        DisclosureLeaf::Completion(completion) => {
            let Some(intent) = incomplete.get(&completion.request_id) else {
                return Err(ComplianceError::StateConflict);
            };
            if completion.timestamp_ms < intent.timestamp_ms {
                return Err(ComplianceError::StateConflict);
            }
        }
    }
    Ok(())
}

fn record_leaf_state(
    request_ids: &mut HashSet<[u8; 32]>,
    incomplete: &mut HashMap<[u8; 32], PendingIntent>,
    leaf: &DisclosureLeaf,
    sequence: u64,
) {
    match leaf {
        DisclosureLeaf::Intent(intent) => {
            request_ids.insert(intent.request_id);
            incomplete.insert(
                intent.request_id,
                PendingIntent {
                    timestamp_ms: intent.timestamp_ms,
                    sequence,
                },
            );
        }
        DisclosureLeaf::Completion(completion) => {
            incomplete.remove(&completion.request_id);
        }
    }
}

#[cfg(not(unix))]
fn sync_parent(_parent: &Path) -> Result<()> {
    Ok(())
}

#[cfg(test)]
mod tests {
    use std::cell::Cell;
    use std::io::Write;

    use ed25519_dalek::{Signer, SigningKey};
    use pigeonpost_registry::log::MerkleLog;
    use tempfile::tempdir;

    use super::*;
    use crate::approval::{DisclosureState, SensitiveRequestMaterial};

    struct PanicPath;

    impl AsRef<Path> for PanicPath {
        fn as_ref(&self) -> &Path {
            panic!("unsupported persistent ledger operation must not inspect its path")
        }
    }

    #[test]
    fn unsupported_platform_rejects_persistent_ledger_before_path_or_randomness() {
        let platform = crate::platform::OfflinePlatform::unsupported_for_test();
        assert!(matches!(
            DisclosureLedger::create_for_platform(platform, PanicPath, &STATE_SECRET),
            Err(ComplianceError::UnsupportedPlatform)
        ));
        assert!(matches!(
            DisclosureLedger::open_for_platform(platform, PanicPath, &STATE_SECRET),
            Err(ComplianceError::UnsupportedPlatform)
        ));
    }

    const STATE_SECRET: [u8; 32] = [91u8; 32];

    #[test]
    fn disclosure_output_debug_withholds_the_authorized_plaintext() {
        let output = DisclosureOutput {
            value: "disclosure-plaintext-debug-canary-2468".to_owned(),
            record_count: 3,
            result_commitment: [0xA7; 32],
        };
        let debugged = format!("{output:?}");

        assert_eq!(
            debugged,
            "DisclosureOutput { record_count: 3, value: \"withheld\", result_commitment: \"withheld\" }"
        );
        assert!(!debugged.contains("disclosure-plaintext-debug-canary-2468"));
    }

    fn authorized_request() -> DisclosureRequest {
        let (mut request, _) = DisclosureRequest::new(
            Jurisdiction::Test,
            CompliancePurpose::NetworkTrace,
            vec![ComplianceKeyId::new(
                CompliancePurpose::NetworkTrace,
                Jurisdiction::Test,
                [1; 32],
                10,
                1,
            )],
            100,
            300,
            SensitiveRequestMaterial {
                order_reference: b"order-secret-99",
                requester_identity: b"requester-secret",
                selectors: b"198.51.100.9",
            },
        )
        .unwrap();
        for seed in [[2u8; 32], [3u8; 32]] {
            let signer = SigningKey::from_bytes(&seed);
            let signature = signer.sign(&request.approval_preimage(150)).to_bytes();
            request
                .approve(signer.verifying_key().to_bytes(), 150, signature)
                .unwrap();
        }
        request
    }

    fn disclosure_pair(index: u64) -> [DisclosureLeaf; 2] {
        let mut request_id = [0u8; 32];
        request_id[24..].copy_from_slice(&(index + 1).to_be_bytes());
        [
            DisclosureLeaf::Intent(DisclosureIntent {
                request_id,
                timestamp_ms: index.saturating_mul(2).saturating_add(1),
                jurisdiction: Jurisdiction::Test,
                purpose: CompliancePurpose::NetworkTrace,
                key_ids: vec![ComplianceKeyId::new(
                    CompliancePurpose::NetworkTrace,
                    Jurisdiction::Test,
                    [1; 32],
                    10,
                    1,
                )],
                order_commitment: [4; 32],
                requester_commitment: [5; 32],
                selector_commitment: [6; 32],
                approver_commitments: vec![[2; 32], [3; 32]],
            }),
            DisclosureLeaf::Completion(DisclosureCompletion {
                request_id,
                timestamp_ms: index.saturating_mul(2).saturating_add(2),
                status: CompletionStatus::Succeeded,
                record_count: 1,
                result_commitment: [7; 32],
            }),
        ]
    }

    fn append_raw(file: &mut File, leaf: &DisclosureLeaf) -> Vec<u8> {
        let encoded = leaf.encode().unwrap();
        file.write_all(&(encoded.len() as u32).to_be_bytes())
            .unwrap();
        file.write_all(&encoded).unwrap();
        encoded
    }

    /// Build one authenticated fixture snapshot after bulk-writing exact PPDISC records. Runtime
    /// open never calls this: it exists so the proof-boundary test can cover thousands of leaves
    /// without turning per-append fsync/state rewrites into the thing being benchmarked.
    fn install_authenticated_fixture_state(path: &Path) {
        let mut file = open_private_ledger(path).unwrap();
        let file_len = file.metadata().unwrap().len();
        let mut magic = [0u8; LOG_MAGIC.len()];
        file.seek(SeekFrom::Start(0)).unwrap();
        file.read_exact(&mut magic).unwrap();
        assert_eq!(&magic, LOG_MAGIC);

        let mut ledger = DisclosureLedger::in_memory();
        let mut record_offset = LOG_MAGIC.len() as u64;
        while record_offset < file_len {
            file.seek(SeekFrom::Start(record_offset)).unwrap();
            let mut length_bytes = [0u8; 4];
            file.read_exact(&mut length_bytes).unwrap();
            let length = u32::from_be_bytes(length_bytes) as usize;
            assert!((1..=MAX_LEAF_BYTES).contains(&length));
            let mut encoded = vec![0u8; length];
            file.read_exact(&mut encoded).unwrap();
            let leaf = DisclosureLeaf::decode(&encoded).unwrap();
            validate_leaf_sequence(&ledger.request_ids, &ledger.incomplete, &leaf).unwrap();
            let sequence = ledger.leaf_count();
            let hash = leaf_hash(&encoded);
            ledger.leaf_offsets.push(record_offset + 4);
            ledger.absorb_hash(hash).unwrap();
            record_leaf_state(
                &mut ledger.request_ids,
                &mut ledger.incomplete,
                &leaf,
                sequence,
            );
            record_offset += 4 + length as u64;
        }
        assert_eq!(record_offset, file_len);
        file.seek(SeekFrom::End(0)).unwrap();
        let metadata = file.metadata().unwrap();
        #[cfg(unix)]
        {
            let directory =
                GuardedDir::open_existing(path.parent().unwrap(), DirPolicy::private_mutable())
                    .unwrap();
            let state_name = leaf_for_path(&disclosure_state_path(path).unwrap()).unwrap();
            let state = directory
                .open_file_optional(
                    &state_name,
                    OpenAccess::ReadOnly,
                    FilePolicy::private(MAX_STATE_BYTES),
                )
                .unwrap();
            let ledger_guard = directory
                .open_file(
                    &leaf_for_path(path).unwrap(),
                    OpenAccess::ReadWrite,
                    FilePolicy::private(MAX_LOG_BYTES),
                )
                .unwrap();
            ledger.custody = Some(LedgerCustody {
                directory,
                ledger: ledger_guard,
                state_name,
                state: Mutex::new(state),
            });
        }
        ledger.file = Some(Mutex::new(file));
        ledger.state_store = Some(LedgerStateStore {
            ledger_path: path.to_owned(),
            path: disclosure_state_path(path).unwrap(),
            authentication_key: derive_state_authentication_key(&STATE_SECRET),
            file_identity: ledger_file_identity(path, &metadata).unwrap(),
            mutation_marker: ledger_mutation_marker(&metadata).unwrap(),
            generation: ledger.leaf_count(),
            log_len: file_len,
            last_leaf_hash: ledger.leaf_offsets.last().map(|_| {
                let last = ledger.leaf_count() - 1;
                let encoded = ledger.encoded_leaf(last).unwrap().unwrap();
                leaf_hash(&encoded)
            }),
            pending_append: None,
        });
        ledger.persist_state().unwrap();
    }

    fn arm_pending_append(ledger: &mut DisclosureLedger, leaf: &DisclosureLeaf) -> Vec<u8> {
        validate_leaf_sequence(&ledger.request_ids, &ledger.incomplete, leaf).unwrap();
        let encoded = leaf.encode().unwrap();
        ledger.validate_current_store_file_stamp().unwrap();
        ledger.state_store.as_mut().unwrap().pending_append = Some(encoded.clone());
        ledger.persist_state().unwrap();
        let mut record = Vec::with_capacity(4 + encoded.len());
        record.extend_from_slice(&(encoded.len() as u32).to_be_bytes());
        record.extend_from_slice(&encoded);
        record
    }

    #[test]
    fn intent_precedes_automatic_success_completion() {
        let mut request = authorized_request();
        let mut ledger = DisclosureLedger::in_memory();
        let operation_finished = Cell::new(false);
        let value = ledger
            .execute(
                &mut request,
                160,
                || {
                    assert!(operation_finished.get());
                    Ok(170)
                },
                |_| {
                    operation_finished.set(true);
                    Ok(DisclosureOutput {
                        value: "artifact",
                        record_count: 2,
                        result_commitment: [9; 32],
                    })
                },
            )
            .unwrap();
        assert_eq!(value, "artifact");
        assert_eq!(request.state(), DisclosureState::Consumed);
        assert!(matches!(
            ledger.leaf(0).unwrap(),
            Some(DisclosureLeaf::Intent(_))
        ));
        assert!(matches!(
            ledger.leaf(1).unwrap(),
            Some(DisclosureLeaf::Completion(_))
        ));
        for index in 0..ledger.leaf_count() {
            let leaf = ledger.leaf(index).unwrap().unwrap();
            let encoded = leaf.encode().unwrap();
            assert_eq!(DisclosureLeaf::decode(&encoded).unwrap(), leaf);
            let mut extended = encoded;
            extended.push(0);
            assert!(DisclosureLeaf::decode(&extended).is_err());
        }
        assert_ne!(ledger.root(), [0u8; 32]);
    }

    #[test]
    fn failures_still_get_a_completion_and_raw_values_never_serialize() {
        let mut request = authorized_request();
        let mut ledger = DisclosureLedger::in_memory();
        let operation_finished = Cell::new(false);
        assert_eq!(
            ledger.execute::<()>(
                &mut request,
                160,
                || {
                    assert!(operation_finished.get());
                    Ok(170)
                },
                |_| {
                    operation_finished.set(true);
                    Err(ComplianceError::Crypto)
                },
            ),
            Err(ComplianceError::Crypto)
        );
        assert_eq!(ledger.leaf_count(), 2);
        let bytes: Vec<u8> = (0..ledger.leaf_count())
            .flat_map(|index| ledger.leaf(index).unwrap().unwrap().encode().unwrap())
            .collect();
        let text = String::from_utf8_lossy(&bytes);
        assert!(!text.contains("order-secret-99"));
        assert!(!text.contains("requester-secret"));
        assert!(!text.contains("198.51.100.9"));
    }

    #[cfg_attr(
        not(any(target_os = "linux", target_os = "macos")),
        ignore = "persistent offline custody is supported only on Linux and macOS"
    )]
    #[test]
    fn durable_log_recovers_tail_and_uses_registry_proofs_and_checkpoints() {
        let temp = tempdir().unwrap();
        let path = temp.path().join("disclosures.log");
        let mut request = authorized_request();
        let mut ledger = DisclosureLedger::create(&path, &STATE_SECRET).unwrap();
        ledger
            .execute(
                &mut request,
                160,
                || Ok(170),
                |_| {
                    Ok(DisclosureOutput {
                        value: (),
                        record_count: 1,
                        result_commitment: [8; 32],
                    })
                },
            )
            .unwrap();
        let root = ledger.root();
        let proof = ledger.inclusion_proof(0).unwrap().unwrap();
        assert!(pigeonpost_registry::verify_inclusion(
            &ledger.leaf(0).unwrap().unwrap().hash().unwrap(),
            0,
            2,
            &proof,
            &root,
        ));
        let signer = SigningKey::from_bytes(&[44; 32]);
        let signed = ledger
            .checkpoint("pigeonpost.dev/disclosures")
            .sign(&signer);
        let verified = Checkpoint::verify(&signed, &signer.verifying_key()).unwrap();
        assert_eq!(verified.root, root);
        let pending = arm_pending_append(&mut ledger, &disclosure_pair(10)[0]);
        drop(ledger);

        OpenOptions::new()
            .append(true)
            .open(&path)
            .unwrap()
            .write_all(&pending[..2])
            .unwrap();
        let reopened = DisclosureLedger::open(&path, &STATE_SECRET).unwrap();
        assert_eq!(reopened.leaf_count(), 2);
        assert_eq!(reopened.root(), root);
        let persisted = fs::read(path).unwrap();
        let text = String::from_utf8_lossy(&persisted);
        assert!(!text.contains("order-secret-99"));
        assert!(!text.contains("requester-secret"));
        assert!(!text.contains("198.51.100.9"));
    }

    #[cfg_attr(
        not(any(target_os = "linux", target_os = "macos")),
        ignore = "persistent offline custody is supported only on Linux and macOS"
    )]
    #[test]
    fn large_streamed_restart_matches_reference_roots_and_public_proofs() {
        let temp = tempdir().unwrap();
        let path = temp.path().join("large-disclosures.log");
        drop(DisclosureLedger::create(&path, &STATE_SECRET).unwrap());

        let mut file = OpenOptions::new().append(true).open(&path).unwrap();
        let mut reference = MerkleLog::new();
        let mut reference_hashes = Vec::new();
        for index in 0..4_097 {
            for leaf in disclosure_pair(index) {
                let encoded = append_raw(&mut file, &leaf);
                reference.append(&encoded);
                reference_hashes.push(leaf_hash(&encoded));
            }
        }
        file.sync_all().unwrap();
        drop(file);
        install_authenticated_fixture_state(&path);

        let ledger = DisclosureLedger::open(&path, &STATE_SECRET).unwrap();
        let size = reference.size();
        assert_eq!(ledger.leaf_count(), size);
        assert_eq!(ledger.root(), reference.root());
        assert_eq!(ledger.leaf_offsets.len(), size as usize);
        assert!(ledger.memory_leaves.is_empty());
        assert_eq!(ledger.request_ids.len(), 4_097);
        assert!(ledger.incomplete.is_empty());
        assert!(ledger.frontier.validate());
        assert!(ledger.proof_tail_frontier.validate());
        assert_eq!(
            ledger.proof_block_roots.len(),
            size as usize / PROOF_BLOCK_LEAVES
        );
        assert!(ledger.proof_tail_hashes.len() < PROOF_BLOCK_LEAVES);

        for index in [0, 1, 1_023, 1_024, 4_096, size - 2, size - 1] {
            let proof = ledger.inclusion_proof(index).unwrap().unwrap();
            assert_eq!(proof, reference.inclusion_proof(index, size).unwrap());
            assert!(pigeonpost_registry::verify_inclusion(
                &reference_hashes[index as usize],
                index,
                size,
                &proof,
                &ledger.root(),
            ));
        }
        for old_size in [1, 2, 1_024, 4_097, 8_192, size] {
            let proof = ledger.consistency_proof(old_size).unwrap().unwrap();
            assert_eq!(proof, reference.consistency_proof(old_size, size).unwrap());
            let old_root =
                MerkleLog::from_leaves(reference_hashes[..old_size as usize].to_vec()).root();
            assert!(pigeonpost_registry::verify_consistency(
                old_size,
                &old_root,
                size,
                &ledger.root(),
                &proof,
            ));
        }
        drop(ledger);

        let reopened = DisclosureLedger::open(&path, &STATE_SECRET).unwrap();
        assert_eq!(reopened.leaf_count(), size);
        assert_eq!(reopened.root(), reference.root());
        assert_eq!(
            reopened.leaf(size - 1).unwrap().unwrap(),
            disclosure_pair(4_096)[1]
        );
        assert_eq!(
            reopened.inclusion_proof(1_024).unwrap().unwrap(),
            reference.inclusion_proof(1_024, size).unwrap()
        );
    }

    #[cfg_attr(
        not(any(target_os = "linux", target_os = "macos")),
        ignore = "persistent offline custody is supported only on Linux and macOS"
    )]
    #[test]
    fn restart_sidecar_size_is_constant_as_the_ledger_grows() {
        let temp = tempdir().unwrap();
        let path = temp.path().join("constant-state-disclosures.log");
        let state_path = disclosure_state_path(&path).unwrap();
        let mut ledger = DisclosureLedger::create(&path, &STATE_SECRET).unwrap();

        let first = disclosure_pair(0);
        ledger.append(first[0].clone()).unwrap();
        let single_leaf_state_len = fs::metadata(&state_path).unwrap().len();
        assert!(single_leaf_state_len <= 256);
        ledger.append(first[1].clone()).unwrap();
        for index in 1..32 {
            for leaf in disclosure_pair(index) {
                ledger.append(leaf).unwrap();
            }
        }

        assert_eq!(
            fs::metadata(&state_path).unwrap().len(),
            single_leaf_state_len,
            "committed restart state must not grow an offset, request-id, or proof index"
        );
        drop(ledger);

        let reopened = DisclosureLedger::open(&path, &STATE_SECRET).unwrap();
        assert_eq!(reopened.leaf_count(), 64);
        assert_eq!(reopened.leaf_offsets.len(), 64);
        assert_eq!(reopened.request_ids.len(), 32);
        assert!(reopened.incomplete.is_empty());
    }

    #[cfg_attr(
        not(any(target_os = "linux", target_os = "macos")),
        ignore = "persistent offline custody is supported only on Linux and macOS"
    )]
    #[test]
    fn recovery_truncates_only_an_incomplete_final_record() {
        let temp = tempdir().unwrap();
        let path = temp.path().join("truncated-disclosures.log");
        let mut ledger = DisclosureLedger::create(&path, &STATE_SECRET).unwrap();
        let first = disclosure_pair(0)[0].clone();
        ledger.append(first).unwrap();
        let valid_len = fs::metadata(&path).unwrap().len();
        let pending = arm_pending_append(&mut ledger, &disclosure_pair(0)[1]);
        drop(ledger);
        let mut file = OpenOptions::new().append(true).open(&path).unwrap();
        file.write_all(&pending[..pending.len() / 2]).unwrap();
        file.sync_all().unwrap();
        drop(file);

        let reopened = DisclosureLedger::open(&path, &STATE_SECRET).unwrap();
        assert_eq!(reopened.leaf_count(), 1);
        assert_eq!(fs::metadata(&path).unwrap().len(), valid_len);
        assert_eq!(reopened.incomplete_request_ids().len(), 1);
    }

    #[cfg_attr(
        not(any(target_os = "linux", target_os = "macos")),
        ignore = "persistent offline custody is supported only on Linux and macOS"
    )]
    #[test]
    fn malformed_complete_tail_is_never_treated_as_crash_recovery() {
        let temp = tempdir().unwrap();
        let path = temp.path().join("malformed-disclosures.log");
        let mut ledger = DisclosureLedger::create(&path, &STATE_SECRET).unwrap();
        let mut pending = arm_pending_append(&mut ledger, &disclosure_pair(0)[0]);
        drop(ledger);
        *pending.last_mut().unwrap() ^= 1;
        let mut file = OpenOptions::new().append(true).open(&path).unwrap();
        file.write_all(&pending).unwrap();
        file.sync_all().unwrap();
        let corrupt_len = file.metadata().unwrap().len();
        drop(file);

        assert!(matches!(
            DisclosureLedger::open(&path, &STATE_SECRET),
            Err(ComplianceError::Storage)
        ));
        assert_eq!(fs::metadata(&path).unwrap().len(), corrupt_len);
    }

    #[cfg_attr(
        not(any(target_os = "linux", target_os = "macos")),
        ignore = "persistent offline custody is supported only on Linux and macOS"
    )]
    #[test]
    fn oversized_tail_length_fails_closed_without_truncation() {
        let temp = tempdir().unwrap();
        let path = temp.path().join("oversized-disclosures.log");
        drop(DisclosureLedger::create(&path, &STATE_SECRET).unwrap());
        let mut file = OpenOptions::new().append(true).open(&path).unwrap();
        file.write_all(&((MAX_LEAF_BYTES + 1) as u32).to_be_bytes())
            .unwrap();
        file.sync_all().unwrap();
        let corrupt_len = file.metadata().unwrap().len();
        drop(file);

        assert!(matches!(
            DisclosureLedger::open(&path, &STATE_SECRET),
            Err(ComplianceError::Storage)
        ));
        assert_eq!(fs::metadata(&path).unwrap().len(), corrupt_len);
    }

    #[cfg_attr(
        not(any(target_os = "linux", target_os = "macos")),
        ignore = "persistent offline custody is supported only on Linux and macOS"
    )]
    #[test]
    fn duplicate_completed_request_remains_invalid_after_streamed_restart() {
        let temp = tempdir().unwrap();
        let path = temp.path().join("duplicate-disclosures.log");
        let mut ledger = DisclosureLedger::create(&path, &STATE_SECRET).unwrap();
        let pair = disclosure_pair(0);
        ledger.append(pair[0].clone()).unwrap();
        ledger.append(pair[1].clone()).unwrap();
        drop(ledger);

        let mut reopened = DisclosureLedger::open(&path, &STATE_SECRET).unwrap();
        assert_eq!(
            reopened.append(pair[0].clone()),
            Err(ComplianceError::StateConflict)
        );
    }

    #[cfg_attr(
        not(any(target_os = "linux", target_os = "macos")),
        ignore = "persistent offline custody is supported only on Linux and macOS"
    )]
    #[test]
    fn authenticated_state_rejects_wrong_key_tampering_and_missing_state() {
        let temp = tempdir().unwrap();
        let path = temp.path().join("authenticated-disclosures.log");
        let state_path = disclosure_state_path(&path).unwrap();
        let mut ledger = DisclosureLedger::create(&path, &STATE_SECRET).unwrap();
        ledger.append(disclosure_pair(0)[0].clone()).unwrap();
        drop(ledger);

        assert!(matches!(
            DisclosureLedger::open(&path, &[92u8; 32]),
            Err(ComplianceError::Storage)
        ));
        let original_state = fs::read(&state_path).unwrap();
        let mut tampered_state = original_state.clone();
        *tampered_state.last_mut().unwrap() ^= 1;
        fs::write(&state_path, &tampered_state).unwrap();
        assert!(matches!(
            DisclosureLedger::open(&path, &STATE_SECRET),
            Err(ComplianceError::Storage)
        ));

        fs::write(&state_path, &original_state).unwrap();
        fs::remove_file(&state_path).unwrap();
        assert!(matches!(
            DisclosureLedger::open(&path, &STATE_SECRET),
            Err(ComplianceError::Storage)
        ));
    }

    #[test]
    fn persisted_state_read_is_bounded_after_the_metadata_check() {
        use std::io::Cursor;

        let mut concurrently_grown = Cursor::new(vec![0u8; 1024]);
        assert_eq!(
            read_bounded_state_bytes(&mut concurrently_grown, 8, 64),
            Err(ComplianceError::LimitExceeded)
        );
        assert_eq!(concurrently_grown.position(), 65);
    }

    #[cfg_attr(
        not(any(target_os = "linux", target_os = "macos")),
        ignore = "persistent offline custody is supported only on Linux and macOS"
    )]
    #[test]
    fn authenticated_state_rejects_log_and_state_rollback() {
        let temp = tempdir().unwrap();
        let path = temp.path().join("rollback-disclosures.log");
        let state_path = disclosure_state_path(&path).unwrap();
        let pair = disclosure_pair(0);
        let mut ledger = DisclosureLedger::create(&path, &STATE_SECRET).unwrap();
        ledger.append(pair[0].clone()).unwrap();
        let old_log_len = fs::metadata(&path).unwrap().len();
        let old_state = fs::read(&state_path).unwrap();
        ledger.append(pair[1].clone()).unwrap();
        drop(ledger);
        let current_state = fs::read(&state_path).unwrap();

        fs::write(&state_path, &old_state).unwrap();
        assert!(matches!(
            DisclosureLedger::open(&path, &STATE_SECRET),
            Err(ComplianceError::Storage)
        ));

        fs::write(&state_path, &current_state).unwrap();
        OpenOptions::new()
            .write(true)
            .open(&path)
            .unwrap()
            .set_len(old_log_len)
            .unwrap();
        assert!(matches!(
            DisclosureLedger::open(&path, &STATE_SECRET),
            Err(ComplianceError::Storage)
        ));
    }

    #[cfg_attr(
        not(any(target_os = "linux", target_os = "macos")),
        ignore = "persistent offline custody is supported only on Linux and macOS"
    )]
    #[test]
    fn same_length_historical_log_tampering_fails_closed() {
        let temp = tempdir().unwrap();
        let path = temp.path().join("tampered-disclosures.log");
        let pair = disclosure_pair(0);
        let mut ledger = DisclosureLedger::create(&path, &STATE_SECRET).unwrap();
        ledger.append(pair[0].clone()).unwrap();
        ledger.append(pair[1].clone()).unwrap();
        drop(ledger);

        let mut bytes = fs::read(&path).unwrap();
        bytes[LOG_MAGIC.len() + 4 + INTENT_DOMAIN.len() + 1] ^= 1;
        fs::write(&path, &bytes).unwrap();
        assert!(matches!(
            DisclosureLedger::open(&path, &STATE_SECRET),
            Err(ComplianceError::Storage)
        ));
    }

    #[cfg_attr(
        not(any(target_os = "linux", target_os = "macos")),
        ignore = "persistent offline custody is supported only on Linux and macOS"
    )]
    #[test]
    fn authenticated_complete_crash_tail_is_replayed_once() {
        let temp = tempdir().unwrap();
        let path = temp.path().join("complete-tail-disclosures.log");
        let leaf = disclosure_pair(0)[0].clone();
        let mut ledger = DisclosureLedger::create(&path, &STATE_SECRET).unwrap();
        let pending = arm_pending_append(&mut ledger, &leaf);
        drop(ledger);
        let mut file = OpenOptions::new().append(true).open(&path).unwrap();
        file.write_all(&pending).unwrap();
        file.sync_all().unwrap();
        drop(file);

        let reopened = DisclosureLedger::open(&path, &STATE_SECRET).unwrap();
        assert_eq!(reopened.leaf_count(), 1);
        assert_eq!(reopened.leaf(0).unwrap(), Some(leaf));
        let root = reopened.root();
        drop(reopened);
        let reopened_again = DisclosureLedger::open(&path, &STATE_SECRET).unwrap();
        assert_eq!(reopened_again.leaf_count(), 1);
        assert_eq!(reopened_again.root(), root);
    }

    #[cfg(unix)]
    #[test]
    fn log_and_authenticated_state_are_owner_only_separate_files() {
        use std::os::unix::fs::{MetadataExt, PermissionsExt};

        let temp = tempdir().unwrap();
        let path = temp.path().join("private-disclosures.log");
        let state_path = disclosure_state_path(&path).unwrap();
        drop(DisclosureLedger::create(&path, &STATE_SECRET).unwrap());
        assert_eq!(fs::read(&path).unwrap(), LOG_MAGIC);
        for private_path in [&path, &state_path] {
            let metadata = fs::metadata(private_path).unwrap();
            assert_eq!(metadata.uid(), rustix::process::geteuid().as_raw());
            assert_eq!(metadata.permissions().mode() & 0o077, 0);
        }
    }

    #[cfg(unix)]
    #[test]
    fn private_ledger_rejects_symlink_and_hardlink_aliases() {
        use std::os::unix::fs::symlink;

        let temp = tempdir().unwrap();
        let path = temp.path().join("disclosures.log");
        drop(DisclosureLedger::create(&path, &STATE_SECRET).unwrap());

        let hardlink = temp.path().join("disclosures.alias");
        fs::hard_link(&path, &hardlink).unwrap();
        assert!(matches!(
            DisclosureLedger::open(&path, &STATE_SECRET),
            Err(ComplianceError::Storage)
        ));
        fs::remove_file(&hardlink).unwrap();

        let symlink_path = temp.path().join("disclosures.link");
        symlink(&path, &symlink_path).unwrap();
        assert!(matches!(
            DisclosureLedger::open(&symlink_path, &STATE_SECRET),
            Err(ComplianceError::Storage)
        ));
    }

    #[cfg(unix)]
    #[test]
    fn retained_log_and_state_replacement_fail_before_append() {
        use std::os::unix::fs::PermissionsExt;

        let log_temp = tempdir().unwrap();
        let log_path = log_temp.path().join("disclosures.log");
        let mut ledger = DisclosureLedger::create(&log_path, &STATE_SECRET).unwrap();
        let original_len = fs::metadata(&log_path).unwrap().len();
        let displaced_log = log_temp.path().join("disclosures.original");
        fs::rename(&log_path, &displaced_log).unwrap();
        fs::copy(&displaced_log, &log_path).unwrap();
        fs::set_permissions(&log_path, fs::Permissions::from_mode(0o600)).unwrap();
        assert_eq!(
            ledger.append(disclosure_pair(0)[0].clone()),
            Err(ComplianceError::Storage)
        );
        assert_eq!(fs::metadata(&log_path).unwrap().len(), original_len);
        assert_eq!(fs::metadata(&displaced_log).unwrap().len(), original_len);

        let state_temp = tempdir().unwrap();
        let log_path = state_temp.path().join("disclosures.log");
        let state_path = disclosure_state_path(&log_path).unwrap();
        let mut ledger = DisclosureLedger::create(&log_path, &STATE_SECRET).unwrap();
        let log_len = fs::metadata(&log_path).unwrap().len();
        let displaced_state = state_temp.path().join("disclosures.state.original");
        fs::rename(&state_path, &displaced_state).unwrap();
        fs::copy(&displaced_state, &state_path).unwrap();
        fs::set_permissions(&state_path, fs::Permissions::from_mode(0o600)).unwrap();
        assert_eq!(
            ledger.append(disclosure_pair(0)[0].clone()),
            Err(ComplianceError::Storage)
        );
        assert_eq!(fs::metadata(&log_path).unwrap().len(), log_len);
        assert!(displaced_state.exists());
    }
}
