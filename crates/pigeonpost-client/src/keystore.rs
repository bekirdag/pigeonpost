//! Key custody.
//!
//! `docs/sds.md` §5.2 ranks the options: OS keychain, then a `0600` file, then delegation. This is
//! the portable middle one; the keychain backend slots in behind the same two functions.
//!
//! The successor key is generated at the same time as the operating key and never later, because
//! an agent that has not committed to a successor can never rotate (`docs/keys.md`). Both land on
//! the same disk by default, which defeats the point — so the client supports an explicit stable
//! recovery directory and warns loudly when both still share a device.

use std::collections::BTreeMap;
#[cfg(not(unix))]
use std::fs::File;
use std::io::{Read, Write};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{SystemTime, UNIX_EPOCH};

use pigeonpost_core::{keys::SuccessorCommitment, Address, AgentRecord, Identity, RotationRecord};
#[cfg(unix)]
use pigeonpost_unix_custody::{
    CustodyError, DirPolicy, EntryKind, FilePolicy, GuardedDir, GuardedFile, LeafName,
    NormalizedPath, OpenAccess,
};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

use crate::error::{ClientError, Result};

const MAX_RECOVERY_PATH_BYTES: usize = 4_096;
#[cfg(unix)]
const MAX_PRIVATE_FILE_BYTES: u64 = 1024 * 1024;
/// At most this many outgoing identities may remain usable during their signed grace windows.
/// This matches the resolver's maximum rotation-chain depth and bounds every wake/open scan.
pub const MAX_LIVE_RETIRED_IDENTITIES: usize = 32;
const MAX_RETIRED_DIRECTORY_ENTRIES: usize = MAX_LIVE_RETIRED_IDENTITIES * 2;

pub(crate) fn require_supported_persistent_storage() -> Result<()> {
    require_supported_persistent_storage_for(cfg!(any(
        target_os = "linux",
        target_os = "macos",
        windows
    )))
}

fn require_supported_persistent_storage_for(supported: bool) -> Result<()> {
    if supported {
        Ok(())
    } else {
        Err(unsupported_persistent_storage_error())
    }
}

pub(crate) fn unsupported_persistent_storage_error() -> ClientError {
    ClientError::Io(std::io::Error::new(
        std::io::ErrorKind::Unsupported,
        "persistent client storage is supported only on Linux, macOS, and Windows",
    ))
}

#[derive(Clone, Debug)]
pub struct KeyPaths {
    pub operating: PathBuf,
    pub successor: PathBuf,
    pub token_secret: PathBuf,
    legacy_successor: PathBuf,
    rotation_lock: PathBuf,
    rotation_journal: PathBuf,
    staged_successor: PathBuf,
    retired_dir: PathBuf,
    default_successor: PathBuf,
    custom_recovery: bool,
}

impl KeyPaths {
    pub fn in_dir(dir: &Path) -> Self {
        let recovery_dir = dir.join("recovery");
        KeyPaths {
            operating: dir.join("identity.key"),
            successor: recovery_dir.join("successor.key"),
            token_secret: dir.join("token.secret"),
            legacy_successor: dir.join("successor.key"),
            rotation_lock: dir.join("rotation.lock"),
            rotation_journal: dir.join("rotation.pending.json"),
            staged_successor: recovery_dir.join("next-successor.key"),
            retired_dir: dir.join("retired"),
            default_successor: dir.join("recovery").join("successor.key"),
            custom_recovery: false,
        }
    }

    /// Keep the operating identity and all mutation journals under `dir`, while placing the
    /// precommitted successor material in an independently configured recovery directory.
    ///
    /// The external directory is an explicit custody boundary: it must already exist as a stable,
    /// canonical, owner-only absolute directory. We never create, chmod, or follow a link supplied
    /// here because doing so could silently bless an attacker-controlled path.
    pub fn in_dir_with_recovery_dir(dir: &Path, recovery_dir: &Path) -> Result<Self> {
        require_supported_persistent_storage()?;
        validate_external_recovery_dir(recovery_dir)?;
        let mut paths = Self::in_dir(dir);
        paths.successor = recovery_dir.join("successor.key");
        paths.staged_successor = recovery_dir.join("next-successor.key");
        paths.custom_recovery = paths.successor != paths.default_successor;
        Ok(paths)
    }

    pub fn recovery_dir(&self) -> &Path {
        self.successor
            .parent()
            .expect("successor paths always have a parent")
    }
}

pub struct LoadedKeys {
    pub identity: Identity,
    pub successor: SuccessorCommitment,
    pub token_secret: [u8; 32],
    pub freshly_created: bool,
    pub retired: Vec<RetiredIdentity>,
}

pub struct RetiredIdentity {
    pub identity: Identity,
    pub record: RotationRecord,
    pub source_record: AgentRecord,
    pub target_record: AgentRecord,
    pub lofts: Vec<String>,
}

pub struct RotationOutcome {
    pub identity: Identity,
    pub successor: SuccessorCommitment,
    pub record: RotationRecord,
    pub source_record: AgentRecord,
    pub target_record: AgentRecord,
    pub lofts: Vec<String>,
}

/// Process-scoped ownership of the active identity mutation boundary.
///
/// The file descriptor is the lease: the OS releases it after a crash. Agent operations retain
/// this guard while they use the cached private key, so rotation is ordered either before or after
/// that operation rather than changing the on-disk identity halfway through it.
pub(crate) struct ActiveIdentityLease {
    #[cfg(unix)]
    _file: GuardedFile,
    #[cfg(not(unix))]
    _file: File,
    lock_path: PathBuf,
}

#[cfg(unix)]
type PrivateFile = GuardedFile;
#[cfg(not(unix))]
type PrivateFile = File;

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct RotationJournal {
    record: RotationRecord,
    source_record: AgentRecord,
    target_record: AgentRecord,
    lofts: Vec<String>,
}

/// Load the identity, creating it — and its successor — on first run.
///
/// Returns the identity, its successor commitment, the independent capability-token secret, and
/// whether this was a fresh creation.
pub fn load_or_create(paths: &KeyPaths) -> Result<LoadedKeys> {
    require_supported_persistent_storage()?;
    let _lock = acquire_rotation_lock(paths)?;
    recover_pending_locked(paths)?;
    let (identity, successor, token_secret, freshly_created) = load_or_create_locked(paths)?;
    let retired = retired_identities_locked(paths, now_secs())?;
    Ok(LoadedKeys {
        identity,
        successor,
        token_secret,
        freshly_created,
        retired,
    })
}

fn load_or_create_locked(
    paths: &KeyPaths,
) -> Result<(Identity, SuccessorCommitment, [u8; 32], bool)> {
    if path_is_present(&paths.operating)? {
        validate_custom_recovery_layout(paths)?;
        migrate_legacy_successor(paths)?;
        let identity = read_key(&paths.operating)?;
        let successor = read_key(&paths.successor)?;
        // Released clients derived tokens from the operating identity. Preserve those tokens on
        // first upgrade, then keep this secret independent through every later key rotation.
        if !path_is_present(&paths.token_secret)? {
            create_secret_once(&paths.token_secret, &legacy_token_secret(&identity))?;
        }
        let token_secret = read_secret(&paths.token_secret)?;
        let commitment = SuccessorCommitment::for_key(&successor.verifying_key());
        return Ok((identity, commitment, token_secret, false));
    }

    if path_is_present(&paths.legacy_successor)? {
        return Err(ClientError::Config(
            "successor key exists without an operating identity".into(),
        ));
    }
    if paths.custom_recovery && path_is_present(&paths.default_successor)? {
        return Err(recovery_migration_error(paths));
    }

    // Publish the successor and token secret before the operating identity. Any process that sees
    // the identity can therefore rely on both prerequisites already being durable. The create-once
    // helper exposes only a fully written inode and never replaces a concurrent winner.
    create_key_once(&paths.successor, &Identity::generate())?;
    create_secret_once(&paths.token_secret, &Identity::generate().to_seed())?;
    let created = create_key_once(&paths.operating, &Identity::generate())?;

    let identity = read_key(&paths.operating)?;
    let successor = read_key(&paths.successor)?;
    let token_secret = read_secret(&paths.token_secret)?;
    let commitment = SuccessorCommitment::for_key(&successor.verifying_key());
    Ok((identity, commitment, token_secret, created))
}

/// Promote the precommitted successor and durably retain the outgoing key for the signed grace
/// interval. The signed source record must already have reached at least one loft.
pub fn rotate(
    paths: &KeyPaths,
    source_record: &AgentRecord,
    lofts: &[String],
    activated_at: u64,
) -> Result<RotationOutcome> {
    require_supported_persistent_storage()?;
    let _lock = acquire_rotation_lock(paths)?;
    recover_pending_locked(paths)?;
    rotate_locked(paths, source_record, lofts, activated_at)
}

pub(crate) fn acquire_active_identity_lease(
    paths: &KeyPaths,
    expected_pubkey: &[u8; 32],
) -> Result<ActiveIdentityLease> {
    require_supported_persistent_storage()?;
    let parent = paths
        .rotation_lock
        .parent()
        .ok_or_else(|| ClientError::Config("identity lock has no parent".into()))?;
    secure_key_dir(parent)?;
    let file = open_private_regular(&paths.rotation_lock, PrivateOpen::ReadWriteCreate)?;
    try_lock_identity_file(&file)?;
    let lease = ActiveIdentityLease {
        _file: file,
        lock_path: paths.rotation_lock.clone(),
    };
    recover_pending_locked(paths)?;
    if read_key(&paths.operating)?.verifying_key().as_bytes() != expected_pubkey {
        return Err(ClientError::Config(
            "agent identity changed on disk; reopen the agent before using its key".into(),
        ));
    }
    Ok(lease)
}

pub(crate) fn rotate_with_lease(
    paths: &KeyPaths,
    lease: &ActiveIdentityLease,
    source_record: &AgentRecord,
    lofts: &[String],
    activated_at: u64,
) -> Result<RotationOutcome> {
    require_supported_persistent_storage()?;
    validate_lease(paths, lease)?;
    rotate_locked(paths, source_record, lofts, activated_at)
}

fn rotate_locked(
    paths: &KeyPaths,
    source_record: &AgentRecord,
    lofts: &[String],
    activated_at: u64,
) -> Result<RotationOutcome> {
    let journal = prepare_rotation_locked(paths, source_record, lofts, activated_at)?;
    complete_rotation_locked(paths, &journal)?;
    let identity = read_key(&paths.operating)?;
    let successor_identity = read_key(&paths.successor)?;
    Ok(RotationOutcome {
        identity,
        successor: SuccessorCommitment::for_key(&successor_identity.verifying_key()),
        record: journal.record,
        source_record: journal.source_record,
        target_record: journal.target_record,
        lofts: journal.lofts,
    })
}

/// Active retired identities. At the exact exclusive grace boundary the key is removed before
/// this returns, so no later drain can authenticate as the retired address.
pub fn retired_identities(paths: &KeyPaths, now: u64) -> Result<Vec<RetiredIdentity>> {
    require_supported_persistent_storage()?;
    let _lock = acquire_rotation_lock(paths)?;
    recover_pending_locked(paths)?;
    retired_identities_locked(paths, now)
}

pub(crate) fn retired_identities_with_lease(
    paths: &KeyPaths,
    lease: &ActiveIdentityLease,
    now: u64,
) -> Result<Vec<RetiredIdentity>> {
    require_supported_persistent_storage()?;
    validate_lease(paths, lease)?;
    retired_identities_locked(paths, now)
}

fn validate_lease(paths: &KeyPaths, lease: &ActiveIdentityLease) -> Result<()> {
    if lease.lock_path != paths.rotation_lock {
        return Err(ClientError::Config(
            "identity lease belongs to a different agent home".into(),
        ));
    }
    Ok(())
}

fn prepare_rotation_locked(
    paths: &KeyPaths,
    source_record: &AgentRecord,
    lofts: &[String],
    activated_at: u64,
) -> Result<RotationJournal> {
    if path_is_present(&paths.rotation_journal)? {
        return Err(ClientError::Config(
            "another key rotation is already pending".into(),
        ));
    }
    let (_, live_retired) = retired_identities_and_live_count_locked(paths, activated_at, None)?;
    if live_retired >= MAX_LIVE_RETIRED_IDENTITIES {
        return Err(ClientError::Config(format!(
            "at most {MAX_LIVE_RETIRED_IDENTITIES} identities may remain in rotation grace"
        )));
    }
    if path_is_present(&paths.staged_successor)? {
        // A crash before journal publication exposes no transition and is safe to discard.
        validate_key_file(&paths.staged_successor)?;
        remove_private_file(&paths.staged_successor)?;
        sync_parent(&paths.staged_successor)?;
    }

    let outgoing = read_key(&paths.operating)?;
    let incoming = read_key(&paths.successor)?;
    let from = outgoing.address();
    source_record.verify(&from)?;
    if source_record.pubkey != outgoing.verifying_key().to_bytes()
        || !source_record
            .successor_commitment()
            .accepts(&incoming.verifying_key())
    {
        return Err(ClientError::Core(pigeonpost_core::Error::SuccessorMismatch));
    }

    create_key_once(&paths.staged_successor, &Identity::generate())?;
    let next = read_key(&paths.staged_successor)?;
    let next_commitment = SuccessorCommitment::for_key(&next.verifying_key());
    let sequence = source_record
        .seq
        .checked_add(1)
        .ok_or_else(|| ClientError::Config("record sequence overflowed".into()))?;
    let record = RotationRecord::new(
        &outgoing,
        &incoming,
        &next_commitment,
        sequence,
        activated_at,
    )?;
    let target_record = AgentRecord::with_policy(
        &incoming,
        &next_commitment,
        sequence,
        source_record.lofts.clone(),
        source_record.pow_min,
        source_record.attribution_requirement,
    );
    let journal = RotationJournal {
        record,
        source_record: source_record.clone(),
        target_record,
        lofts: lofts.to_vec(),
    };
    validate_journal(&journal)?;
    write_json_replacing(&paths.rotation_journal, &journal)?;
    Ok(journal)
}

fn recover_pending_locked(paths: &KeyPaths) -> Result<()> {
    if !path_is_present(&paths.rotation_journal)? {
        if path_is_present(&paths.staged_successor)? {
            validate_key_file(&paths.staged_successor)?;
            remove_private_file(&paths.staged_successor)?;
            sync_parent(&paths.staged_successor)?;
        }
        return Ok(());
    }
    let journal: RotationJournal = read_json(&paths.rotation_journal)?;
    validate_journal(&journal)?;
    complete_rotation_locked(paths, &journal)
}

fn validate_journal(journal: &RotationJournal) -> Result<()> {
    let from = Address::from_pubkey(&pigeonpost_core::keys::verifying_key_from_bytes(
        &journal.record.from_pubkey,
    )?);
    journal.source_record.verify(&from)?;
    journal.record.verify_source_address(&from)?;
    journal.record.verify(
        &journal.source_record.successor_commitment(),
        journal.source_record.seq,
        journal.record.activated_at,
    )?;
    let target = journal.record.target_address()?;
    journal.target_record.verify(&target)?;
    if journal.target_record.pubkey != journal.record.to_pubkey
        || journal.target_record.successor_hash != journal.record.next_successor_hash
        || journal.target_record.seq != journal.record.seq
    {
        return Err(ClientError::Core(pigeonpost_core::Error::SuccessorMismatch));
    }
    Ok(())
}

fn complete_rotation_locked(paths: &KeyPaths, journal: &RotationJournal) -> Result<()> {
    validate_journal(journal)?;
    let pending_stem = retired_stem(&journal.record);
    let (_, live_retired) = retired_identities_and_live_count_locked(
        paths,
        journal.record.activated_at,
        Some(&pending_stem),
    )?;
    secure_key_dir(&paths.retired_dir)?;
    let retired_key = retired_key_path(paths, &journal.record);
    let retired_metadata = retired_metadata_path(paths, &journal.record);
    if !path_is_present(&retired_key)?
        && !path_is_present(&retired_metadata)?
        && live_retired >= MAX_LIVE_RETIRED_IDENTITIES
    {
        return Err(ClientError::Config(format!(
            "at most {MAX_LIVE_RETIRED_IDENTITIES} identities may remain in rotation grace"
        )));
    }
    // Reject an unsafe crash remnant before retaining or promoting any key. A valid private
    // remnant is discarded and later rewritten from the already-validated journal.
    prepare_retired_metadata_staging(paths)?;

    let outgoing = if path_is_present(&retired_key)? {
        read_key(&retired_key)?
    } else {
        let candidate = read_key(&paths.operating)?;
        if candidate.verifying_key().to_bytes() != journal.record.from_pubkey {
            return Err(ClientError::Config(
                "rotation recovery cannot find the outgoing key".into(),
            ));
        }
        create_key_once(&retired_key, &candidate)?;
        candidate
    };
    if outgoing.verifying_key().to_bytes() != journal.record.from_pubkey {
        return Err(ClientError::Config(
            "retired key does not match its rotation record".into(),
        ));
    }

    let operating_is_incoming = path_is_present(&paths.operating)?
        && read_key(&paths.operating)?.verifying_key().to_bytes() == journal.record.to_pubkey;
    if !operating_is_incoming {
        let incoming = read_key(&paths.successor)?;
        if incoming.verifying_key().to_bytes() != journal.record.to_pubkey {
            return Err(ClientError::Config(
                "rotation recovery cannot find the incoming key".into(),
            ));
        }
        replace_secret(&paths.operating, &incoming.to_seed())?;
    }

    let successor_is_next = path_is_present(&paths.successor)?
        && SuccessorCommitment::for_key(&read_key(&paths.successor)?.verifying_key()).as_bytes()
            == &journal.record.next_successor_hash;
    if !successor_is_next {
        let next = read_key(&paths.staged_successor)?;
        if SuccessorCommitment::for_key(&next.verifying_key()).as_bytes()
            != &journal.record.next_successor_hash
        {
            return Err(ClientError::Config(
                "rotation recovery cannot find the next successor key".into(),
            ));
        }
        replace_secret(&paths.successor, &next.to_seed())?;
    }

    write_retired_metadata_replacing(paths, &retired_metadata, journal)?;
    if path_is_present(&paths.staged_successor)? {
        remove_private_file(&paths.staged_successor)?;
        sync_parent(&paths.staged_successor)?;
    }
    remove_private_file(&paths.rotation_journal)?;
    sync_parent(&paths.rotation_journal)?;
    Ok(())
}

fn retired_identities_locked(paths: &KeyPaths, now: u64) -> Result<Vec<RetiredIdentity>> {
    retired_identities_and_live_count_locked(paths, now, None).map(|(retired, _)| retired)
}

/// Load and validate the bounded retired-key directory. Each live identity owns one exact
/// lowercase-hex `.key` + `.json` pair; any other entry is corruption. The iterator stops after
/// the first entry above the fixed pair budget, so an attacker-sized directory cannot make every
/// agent wake scan attacker-sized state.
fn retired_identities_and_live_count_locked(
    paths: &KeyPaths,
    now: u64,
    allowed_incomplete_stem: Option<&str>,
) -> Result<(Vec<RetiredIdentity>, usize)> {
    if !path_is_present(&paths.retired_dir)? {
        return Ok((Vec::new(), 0));
    }
    secure_key_dir(&paths.retired_dir)?;
    #[cfg(unix)]
    let retired_guard = unix_open_private_directory(&paths.retired_dir, false)?;
    #[cfg(windows)]
    let retired_guard = windows_custody::guard_private_directory(&paths.retired_dir)?;
    #[cfg(windows)]
    let retired_directory = retired_guard.path();
    #[cfg(not(any(unix, windows)))]
    let retired_directory = paths.retired_dir.as_path();
    let mut pairs: BTreeMap<String, (Option<PathBuf>, Option<PathBuf>)> = BTreeMap::new();
    #[cfg(unix)]
    let inventory = retired_guard
        .list_bounded(MAX_RETIRED_DIRECTORY_ENTRIES)
        .map_err(map_key_custody_error)?
        .into_iter()
        .map(|entry| {
            if entry.metadata.kind != EntryKind::RegularFile {
                return Err(ClientError::Config(
                    "retired identity directory contains an unexpected entry".into(),
                ));
            }
            let path = paths.retired_dir.join(entry.name.as_os_str());
            Ok((entry.name.as_os_str().to_os_string(), path))
        })
        .collect::<Result<Vec<_>>>()?;
    #[cfg(not(unix))]
    let inventory = std::fs::read_dir(retired_directory)?
        .take(MAX_RETIRED_DIRECTORY_ENTRIES + 1)
        .map(|entry| {
            let entry = entry?;
            if !entry.file_type()?.is_file() {
                return Err(ClientError::Config(
                    "retired identity directory contains an unexpected entry".into(),
                ));
            }
            Ok((entry.file_name(), entry.path()))
        })
        .collect::<Result<Vec<_>>>()?;
    if inventory.len() > MAX_RETIRED_DIRECTORY_ENTRIES {
        return Err(ClientError::Config(format!(
            "retired identity directory exceeds its {MAX_LIVE_RETIRED_IDENTITIES}-identity bound"
        )));
    }
    for (name, path) in inventory {
        let name = name
            .into_string()
            .map_err(|_| ClientError::Config("retired identity filename is not UTF-8".into()))?;
        let (stem, is_key) = if let Some(stem) = name.strip_suffix(".key") {
            (stem, true)
        } else if let Some(stem) = name.strip_suffix(".json") {
            (stem, false)
        } else {
            return Err(ClientError::Config(
                "retired identity directory contains an unexpected entry".into(),
            ));
        };
        if stem.len() != 64
            || !stem
                .bytes()
                .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
        {
            return Err(ClientError::Config(
                "retired identity filename is malformed".into(),
            ));
        }
        let pair = pairs.entry(stem.to_owned()).or_default();
        let slot = if is_key { &mut pair.0 } else { &mut pair.1 };
        if slot.replace(path).is_some() {
            return Err(ClientError::Config(
                "retired identity directory contains duplicate entries".into(),
            ));
        }
    }

    let mut retired = Vec::new();
    let mut live_count = 0usize;
    let mut removed_expired = false;
    for (stem, (key_path, metadata_path)) in pairs {
        if metadata_path.is_none()
            && key_path.is_some()
            && allowed_incomplete_stem == Some(stem.as_str())
        {
            validate_key_file(key_path.as_ref().expect("checked as present"))?;
            continue;
        }
        let (Some(key_path), Some(metadata_path)) = (key_path, metadata_path) else {
            return Err(ClientError::Config(
                "retired identity key and journal must form an exact pair".into(),
            ));
        };
        let journal: RotationJournal = read_json(&metadata_path)?;
        validate_journal(&journal)?;
        if retired_stem(&journal.record) != stem
            || retired_key_path(paths, &journal.record) != key_path
            || retired_metadata_path(paths, &journal.record) != metadata_path
        {
            return Err(ClientError::Config(
                "retired identity filename does not match its rotation record".into(),
            ));
        }
        validate_key_file(&key_path)?;
        if now >= journal.record.grace_until {
            remove_private_file(&key_path)?;
            remove_private_file(&metadata_path)?;
            removed_expired = true;
            continue;
        }
        live_count += 1;
        if now < journal.record.activated_at {
            continue;
        }
        let identity = read_key(&key_path)?;
        if identity.verifying_key().to_bytes() != journal.record.from_pubkey {
            return Err(ClientError::Config(
                "retired key does not match its rotation record".into(),
            ));
        }
        retired.push(RetiredIdentity {
            identity,
            record: journal.record,
            source_record: journal.source_record,
            target_record: journal.target_record,
            lofts: journal.lofts,
        });
    }
    if removed_expired {
        sync_directory(&paths.retired_dir)?;
    }
    if live_count > MAX_LIVE_RETIRED_IDENTITIES {
        return Err(ClientError::Config(format!(
            "retired identity directory exceeds its {MAX_LIVE_RETIRED_IDENTITIES}-identity bound"
        )));
    }
    #[cfg(unix)]
    retired_guard
        .verify_named()
        .map_err(map_key_custody_error)?;
    #[cfg(windows)]
    retired_guard.verify()?;
    Ok((retired, live_count))
}

fn acquire_rotation_lock(paths: &KeyPaths) -> Result<PrivateFile> {
    let parent = paths
        .rotation_lock
        .parent()
        .ok_or_else(|| ClientError::Config("rotation lock has no parent".into()))?;
    secure_key_dir(parent)?;
    let file = open_private_regular(&paths.rotation_lock, PrivateOpen::ReadWriteCreate)?;
    // Agent::open may run in an MCP blocking worker whose outer future can be cancelled. Never
    // leave that worker asleep on a kernel lock only to wake and execute a stale tool later.
    try_lock_identity_file(&file)?;
    Ok(file)
}

trait PrivateLockFile {
    fn as_lock_file(&self) -> &std::fs::File;
}

#[cfg(unix)]
impl PrivateLockFile for GuardedFile {
    fn as_lock_file(&self) -> &std::fs::File {
        self.file()
    }
}

#[cfg(not(unix))]
impl PrivateLockFile for std::fs::File {
    fn as_lock_file(&self) -> &std::fs::File {
        self
    }
}

fn try_lock_identity_file(file: &impl PrivateLockFile) -> Result<()> {
    fs2::FileExt::try_lock_exclusive(file.as_lock_file()).map_err(|error| {
        if is_lock_contention(&error) {
            ClientError::Config("agent identity is busy; retry after the active operation".into())
        } else {
            error.into()
        }
    })
}

fn is_lock_contention(error: &std::io::Error) -> bool {
    if error.kind() == std::io::ErrorKind::WouldBlock {
        return true;
    }
    let Some(actual) = error.raw_os_error() else {
        return false;
    };
    fs2::lock_contended_error().raw_os_error() == Some(actual)
}

fn retired_stem(record: &RotationRecord) -> String {
    record
        .from_pubkey
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect()
}

fn retired_key_path(paths: &KeyPaths, record: &RotationRecord) -> PathBuf {
    paths
        .retired_dir
        .join(format!("{}.key", retired_stem(record)))
}

fn retired_metadata_path(paths: &KeyPaths, record: &RotationRecord) -> PathBuf {
    paths
        .retired_dir
        .join(format!("{}.json", retired_stem(record)))
}

/// True when both keys sit in the same directory, which makes the successor useless as a backup
/// against disk loss. Callers surface this to the operator.
pub fn successor_shares_a_disk(paths: &KeyPaths) -> bool {
    if require_supported_persistent_storage().is_err() {
        return true;
    }
    same_disk(paths).unwrap_or(true)
}

pub const SUCCESSOR_WARNING: &str = "\
warning: the successor key sits beside the operating key.
         Losing this disk loses the address permanently — there is no recovery by design.
         For a new identity, configure --recovery-dir or PIGEONPOST_RECOVERY_DIR before first open.
         To migrate this identity, stop every process, explicitly move successor.key into an
         existing canonical owner-only recovery directory, then reopen every integration with the
         same recovery-dir setting. Never move key material while an agent is running.";

fn read_key(path: &Path) -> Result<Identity> {
    Ok(Identity::from_seed(read_secret(path)?))
}

fn read_secret(path: &Path) -> Result<[u8; 32]> {
    let file = open_private_regular(path, PrivateOpen::ReadOnly)?;
    if private_file_len(&file)? != 32 {
        return Err(ClientError::Config(format!(
            "{} is not a 32-byte key",
            path.display()
        )));
    }
    let mut bytes = Vec::with_capacity(33);
    file.take(33).read_to_end(&mut bytes)?;
    let seed: [u8; 32] = bytes
        .try_into()
        .map_err(|_| ClientError::Config(format!("{} is not a 32-byte key", path.display())))?;
    Ok(seed)
}

fn create_key_once(path: &Path, identity: &Identity) -> Result<bool> {
    create_secret_once(path, &identity.to_seed())
}

fn create_secret_once(path: &Path, secret: &[u8; 32]) -> Result<bool> {
    let parent = path
        .parent()
        .ok_or_else(|| ClientError::Config("key path has no parent".into()))?;
    secure_key_dir(parent)?;

    static TEMP_SEQUENCE: AtomicU64 = AtomicU64::new(0);
    let sequence = TEMP_SEQUENCE.fetch_add(1, Ordering::Relaxed);
    let filename = path
        .file_name()
        .and_then(|name| name.to_str())
        .ok_or_else(|| ClientError::Config("key filename is not valid UTF-8".into()))?;
    let temporary = parent.join(format!(
        ".{filename}.{}.{}.tmp",
        std::process::id(),
        sequence
    ));

    write_new_file(&temporary, secret)?;
    #[cfg(unix)]
    let published = unix_publish_no_replace(&temporary, path)?;
    #[cfg(windows)]
    let published = windows_custody::publish_noclobber(&temporary, path)?;
    #[cfg(not(any(unix, windows)))]
    let published = match std::fs::hard_link(&temporary, path) {
        Ok(()) => true,
        Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => false,
        Err(error) => {
            let _ = remove_private_file(&temporary);
            return Err(error.into());
        }
    };
    #[cfg(not(any(unix, windows)))]
    remove_private_file(&temporary)?;
    if published {
        // Publication preserves the descriptor-validated private inode. Re-open the published
        // name without following links and prove it is still private; never chmod a path that an
        // attacker could swap underneath us.
        validate_key_file(path)?;
        sync_directory(parent)?;
    } else {
        validate_key_file(path)?;
    }
    Ok(published)
}

fn replace_secret(path: &Path, secret: &[u8; 32]) -> Result<()> {
    let parent = path
        .parent()
        .ok_or_else(|| ClientError::Config("key path has no parent".into()))?;
    secure_key_dir(parent)?;
    let temporary = temporary_path(path)?;
    write_new_file(&temporary, secret)?;
    publish_replacement(&temporary, path, parent)
}

fn write_json_replacing<T: Serialize>(path: &Path, value: &T) -> Result<()> {
    let encoded = serde_json::to_vec(value)?;
    if encoded.len() > 1024 * 1024 {
        return Err(ClientError::Config("rotation journal is too large".into()));
    }
    let parent = path
        .parent()
        .ok_or_else(|| ClientError::Config("journal path has no parent".into()))?;
    secure_key_dir(parent)?;
    let temporary = temporary_path(path)?;
    write_new_file(&temporary, &encoded)?;
    publish_replacement(&temporary, path, parent)
}

/// Retired metadata must publish atomically without ever placing a temporary entry inside the
/// exact-pair directory. The identity lease serializes this fixed staging name. A crash remnant is
/// never trusted: recovery accepts only a private regular file, removes it, and rewrites from the
/// already-validated durable rotation journal.
fn write_retired_metadata_replacing<T: Serialize>(
    paths: &KeyPaths,
    path: &Path,
    value: &T,
) -> Result<()> {
    let encoded = serde_json::to_vec(value)?;
    if encoded.len() > 1024 * 1024 {
        return Err(ClientError::Config("rotation journal is too large".into()));
    }
    let temporary = prepare_retired_metadata_staging(paths)?;
    let staging_parent = temporary
        .parent()
        .ok_or_else(|| ClientError::Config("rotation journal has no parent".into()))?;
    let target_parent = path
        .parent()
        .ok_or_else(|| ClientError::Config("retired journal has no parent".into()))?;
    secure_key_dir(target_parent)?;
    write_new_file(&temporary, &encoded)?;
    publish_replacement(&temporary, path, target_parent)?;
    if staging_parent != target_parent {
        sync_directory(staging_parent)?;
    }
    Ok(())
}

fn prepare_retired_metadata_staging(paths: &KeyPaths) -> Result<PathBuf> {
    let staging_parent = paths
        .rotation_journal
        .parent()
        .ok_or_else(|| ClientError::Config("rotation journal has no parent".into()))?;
    secure_key_dir(staging_parent)?;
    let temporary = staging_parent.join(".retired-metadata.pending.replace");
    if path_is_present(&temporary)? {
        validate_key_file(&temporary)?;
        remove_private_file(&temporary)?;
        sync_directory(staging_parent)?;
    }
    Ok(temporary)
}

#[cfg(unix)]
fn unix_publish_no_replace(temporary: &Path, destination: &Path) -> Result<bool> {
    let (source_directory, source_name) = unix_private_parent_and_leaf(temporary, false)?;
    let source = source_directory
        .open_file(
            &source_name,
            OpenAccess::ReadOnly,
            unix_private_file_policy(),
        )
        .map_err(map_key_custody_error)?;
    let (destination_directory, destination_name) =
        unix_private_parent_and_leaf(destination, false)?;
    source.verify_named().map_err(map_key_custody_error)?;
    destination_directory
        .verify_named()
        .map_err(map_key_custody_error)?;
    match source_directory.publish_no_replace(source, &destination_directory, &destination_name) {
        Ok(published) => {
            published.verify_named().map_err(map_key_custody_error)?;
            Ok(true)
        }
        Err(CustodyError::AlreadyExists) => {
            let cleanup = source_directory
                .open_file(
                    &source_name,
                    OpenAccess::ReadOnly,
                    unix_private_file_policy(),
                )
                .map_err(map_key_custody_error)?;
            source_directory
                .unlink_file(cleanup)
                .map_err(map_key_custody_error)?;
            let winner = destination_directory
                .open_file(
                    &destination_name,
                    OpenAccess::ReadOnly,
                    unix_private_file_policy(),
                )
                .map_err(map_key_custody_error)?;
            winner.verify_named().map_err(map_key_custody_error)?;
            Ok(false)
        }
        Err(error) => {
            if let Ok(Some(cleanup)) = source_directory.open_file_optional(
                &source_name,
                OpenAccess::ReadOnly,
                unix_private_file_policy(),
            ) {
                let _ = source_directory.unlink_file(cleanup);
            }
            Err(map_key_custody_error(error))
        }
    }
}

fn publish_replacement(temporary: &Path, path: &Path, _parent: &Path) -> Result<()> {
    #[cfg(unix)]
    {
        let (source_directory, source_name) = unix_private_parent_and_leaf(temporary, false)?;
        let source = source_directory
            .open_file(
                &source_name,
                OpenAccess::ReadOnly,
                unix_private_file_policy(),
            )
            .map_err(map_key_custody_error)?;
        let (destination_directory, destination_name) = unix_private_parent_and_leaf(path, false)?;
        source.verify_named().map_err(map_key_custody_error)?;
        destination_directory
            .verify_named()
            .map_err(map_key_custody_error)?;
        match source_directory.rename_replace(source, &destination_directory, &destination_name) {
            Ok(published) => {
                published.verify_named().map_err(map_key_custody_error)?;
                destination_directory
                    .sync()
                    .map_err(map_key_custody_error)?;
                destination_directory
                    .verify_named()
                    .map_err(map_key_custody_error)
            }
            Err(error) => {
                if let Ok(Some(cleanup)) = source_directory.open_file_optional(
                    &source_name,
                    OpenAccess::ReadOnly,
                    unix_private_file_policy(),
                ) {
                    let _ = source_directory.unlink_file(cleanup);
                }
                Err(map_key_custody_error(error))
            }
        }
    }
    #[cfg(windows)]
    {
        windows_custody::publish_replacement(temporary, path)?;
        sync_directory(_parent)
    }

    #[cfg(all(not(unix), not(windows)))]
    if path.exists() {
        // `std::fs::rename` cannot replace an existing destination on every supported non-Unix
        // platform. Validate before the compatibility fallback so links are never followed.
        validate_key_file(path)?;
        std::fs::remove_file(path)?;
        sync_directory(_parent)?;
    }

    #[cfg(not(any(unix, windows)))]
    if let Err(error) = std::fs::rename(temporary, path) {
        let _ = std::fs::remove_file(temporary);
        return Err(error.into());
    }
    #[cfg(not(any(unix, windows)))]
    validate_key_file(path)?;
    #[cfg(not(any(unix, windows)))]
    sync_directory(_parent)
}

fn read_json<T: serde::de::DeserializeOwned>(path: &Path) -> Result<T> {
    const MAX_JOURNAL_BYTES: u64 = 1024 * 1024;
    let file = open_private_regular(path, PrivateOpen::ReadOnly)?;
    let len = private_file_len(&file)?;
    if len > MAX_JOURNAL_BYTES {
        return Err(ClientError::Config("rotation journal is too large".into()));
    }
    let mut encoded = Vec::with_capacity(len as usize + 1);
    file.take(MAX_JOURNAL_BYTES + 1).read_to_end(&mut encoded)?;
    if encoded.len() as u64 > MAX_JOURNAL_BYTES {
        return Err(ClientError::Config("rotation journal is too large".into()));
    }
    Ok(serde_json::from_slice(&encoded)?)
}

fn temporary_path(path: &Path) -> Result<PathBuf> {
    static REPLACE_SEQUENCE: AtomicU64 = AtomicU64::new(0);
    let parent = path
        .parent()
        .ok_or_else(|| ClientError::Config("temporary path has no parent".into()))?;
    let filename = path
        .file_name()
        .and_then(|name| name.to_str())
        .ok_or_else(|| ClientError::Config("filename is not valid UTF-8".into()))?;
    Ok(parent.join(format!(
        ".{filename}.{}.{}.replace",
        std::process::id(),
        REPLACE_SEQUENCE.fetch_add(1, Ordering::Relaxed)
    )))
}

fn write_new_file(path: &Path, bytes: &[u8]) -> Result<()> {
    #[cfg(unix)]
    {
        if bytes.len() as u64 > MAX_PRIVATE_FILE_BYTES {
            return Err(ClientError::Config("private key file is too large".into()));
        }
        let (directory, name) = unix_private_parent_and_leaf(path, false)?;
        let mut file = directory
            .create_file(&name, unix_private_file_policy())
            .map_err(map_key_custody_error)?;
        if let Err(error) = file.write_all(bytes) {
            let _ = directory.unlink_file(file);
            return Err(error.into());
        }
        if let Err(error) = file.sync_all() {
            let _ = directory.unlink_file(file);
            return Err(map_key_custody_error(error));
        }
        file.verify_named().map_err(map_key_custody_error)?;
        directory.sync().map_err(map_key_custody_error)?;
        directory.verify_named().map_err(map_key_custody_error)?;
        Ok(())
    }
    #[cfg(windows)]
    let (mut file, parents) = windows_custody::create_new_private_file(path)?;
    #[cfg(not(any(unix, windows)))]
    let mut file = {
        let mut options = std::fs::OpenOptions::new();
        options.write(true).create_new(true);
        options.open(path)?
    };
    #[cfg(not(unix))]
    if let Err(error) = (|| -> std::io::Result<()> {
        file.write_all(bytes)?;
        file.sync_all()
    })() {
        drop(file);
        let _ = remove_private_file(path);
        return Err(error.into());
    }
    #[cfg(windows)]
    parents.verify()?;
    #[cfg(not(unix))]
    Ok(())
}

fn sync_parent(path: &Path) -> Result<()> {
    let parent = path
        .parent()
        .ok_or_else(|| ClientError::Config("path has no parent".into()))?;
    sync_directory(parent)
}

fn now_secs() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|duration| duration.as_secs())
        .unwrap_or(0)
}

fn legacy_token_secret(identity: &Identity) -> [u8; 32] {
    let mut hasher = Sha256::new();
    hasher.update(b"pigeonpost/token-secret/v1");
    hasher.update(identity.to_seed());
    hasher.finalize().into()
}

fn migrate_legacy_successor(paths: &KeyPaths) -> Result<()> {
    if path_is_present(&paths.successor)? {
        return Ok(());
    }
    if !path_is_present(&paths.legacy_successor)? {
        return Err(ClientError::Config(
            "operating identity exists but its successor key is missing".into(),
        ));
    }

    validate_key_file(&paths.legacy_successor)?;
    let parent = paths
        .successor
        .parent()
        .ok_or_else(|| ClientError::Config("successor path has no parent".into()))?;
    secure_key_dir(parent)?;
    match rename_private_file(&paths.legacy_successor, &paths.successor) {
        Ok(()) => {
            validate_key_file(&paths.successor)?;
            sync_directory(parent)?;
        }
        Err(ClientError::Io(error))
            if error.kind() == std::io::ErrorKind::NotFound
                && path_is_present(&paths.successor)? => {}
        Err(error) => return Err(error),
    }
    Ok(())
}

fn validate_key_file(path: &Path) -> Result<()> {
    drop(open_private_regular(path, PrivateOpen::ReadOnly)?);
    Ok(())
}

#[derive(Clone, Copy)]
enum PrivateOpen {
    ReadOnly,
    ReadWriteCreate,
}

#[cfg(unix)]
fn private_file_len(file: &GuardedFile) -> Result<u64> {
    file.metadata()
        .map(|metadata| metadata.len)
        .map_err(map_key_custody_error)
}

#[cfg(not(unix))]
fn private_file_len(file: &File) -> Result<u64> {
    file.metadata()
        .map(|metadata| metadata.len())
        .map_err(Into::into)
}

/// Open a key-custody file and validate the opened object, not merely its path.
///
/// On Unix this is an `O_NOFOLLOW` open followed by owner, mode, type, and link-count checks on
/// the file descriptor. Comparing the descriptor's device/inode with a final `lstat` also catches
/// replacement during the open. The caller subsequently reads or locks this same descriptor.
#[cfg(unix)]
fn open_private_regular(path: &Path, access: PrivateOpen) -> Result<GuardedFile> {
    let create = matches!(access, PrivateOpen::ReadWriteCreate);
    let (directory, name) = unix_private_parent_and_leaf(path, create)?;
    let access = match access {
        PrivateOpen::ReadOnly => OpenAccess::ReadOnly,
        PrivateOpen::ReadWriteCreate => OpenAccess::ReadWrite,
    };
    let file = if create {
        directory
            .open_or_create_file(&name, access, unix_private_file_policy())
            .map_err(map_key_custody_error)?
    } else {
        directory
            .open_file(&name, access, unix_private_file_policy())
            .map_err(map_key_custody_error)?
    };
    file.verify_named().map_err(map_key_custody_error)?;
    Ok(file)
}

/// Windows has no `O_NOFOLLOW`; `FILE_FLAG_OPEN_REPARSE_POINT` gives the equivalent handle-level
/// boundary. The handle metadata is checked before any caller reads from it.
#[cfg(windows)]
fn open_private_regular(path: &Path, access: PrivateOpen) -> Result<File> {
    match access {
        PrivateOpen::ReadOnly => windows_custody::open_private_file(path),
        PrivateOpen::ReadWriteCreate => {
            windows_custody::open_or_create_private_file(path).map(|(file, _created)| file)
        }
    }
}

#[cfg(not(any(unix, windows)))]
fn open_private_regular(path: &Path, access: PrivateOpen) -> Result<File> {
    let before = std::fs::symlink_metadata(path)?;
    if before.file_type().is_symlink() || !before.is_file() {
        return Err(private_file_error(path, "must be a regular file"));
    }
    let mut options = std::fs::OpenOptions::new();
    options.read(true);
    if matches!(access, PrivateOpen::ReadWriteCreate) {
        options.write(true).create(true);
    }
    let file = options.open(path)?;
    if !file.metadata()?.is_file() {
        return Err(private_file_error(path, "must be a regular file"));
    }
    Ok(file)
}

#[cfg(not(any(unix, windows)))]
fn private_file_error(path: &Path, reason: &str) -> ClientError {
    ClientError::Config(format!("{} {reason}", path.display()))
}

fn secure_key_dir(path: &Path) -> Result<()> {
    secure_or_create_directory(path)
}

pub(crate) fn secure_or_create_directory(path: &Path) -> Result<()> {
    require_supported_persistent_storage()?;
    #[cfg(unix)]
    {
        unix_open_private_directory(path, true)?;
        Ok(())
    }
    #[cfg(windows)]
    {
        validate_lexical_path(path)?;
        windows_custody::secure_private_directory(path)
    }
    #[cfg(not(any(unix, windows)))]
    {
        validate_lexical_path(path)?;
        create_final_directory(path)?;
        let metadata = std::fs::symlink_metadata(path)?;
        if metadata.file_type().is_symlink() || !metadata.is_dir() {
            return Err(ClientError::Config(format!(
                "{} must be a regular directory, not a symlink",
                path.display()
            )));
        }
        secure_directory_permissions(path)
    }
}

#[cfg(not(unix))]
fn validate_lexical_path(path: &Path) -> Result<()> {
    if path.as_os_str().is_empty() {
        return Err(ClientError::Config(
            "private storage path must not be empty".into(),
        ));
    }
    if path
        .components()
        .any(|component| matches!(component, std::path::Component::ParentDir))
    {
        return Err(ClientError::Config(format!(
            "{} must not contain a parent-directory component",
            path.display()
        )));
    }
    Ok(())
}

#[cfg(not(any(unix, windows)))]
fn create_final_directory(path: &Path) -> Result<bool> {
    validate_lexical_path(path)?;
    match std::fs::create_dir(path) {
        Ok(()) => Ok(true),
        Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => Ok(false),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            let parent = path.parent().ok_or_else(|| {
                ClientError::Config(format!("{} has no parent directory", path.display()))
            })?;
            if !parent.as_os_str().is_empty() {
                std::fs::create_dir_all(parent)?;
            }
            match std::fs::create_dir(path) {
                Ok(()) => Ok(true),
                Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => Ok(false),
                Err(error) => Err(error.into()),
            }
        }
        Err(error) => Err(error.into()),
    }
}

#[cfg(unix)]
fn unix_private_file_policy() -> FilePolicy {
    FilePolicy::private(MAX_PRIVATE_FILE_BYTES)
}

#[cfg(unix)]
fn unix_open_private_directory(path: &Path, create: bool) -> Result<GuardedDir> {
    let normalized = NormalizedPath::new(path).map_err(map_key_custody_error)?;
    let directory = match GuardedDir::open_existing(normalized.as_path(), DirPolicy::trusted()) {
        Ok(directory) => directory,
        Err(CustodyError::NotFound) if create => {
            GuardedDir::create_private(normalized.as_path()).map_err(map_key_custody_error)?
        }
        Err(error) => return Err(map_key_custody_error(error)),
    };
    directory.verify_named().map_err(map_key_custody_error)?;
    Ok(directory)
}

#[cfg(unix)]
fn unix_private_parent_and_leaf(
    path: &Path,
    create_parent: bool,
) -> Result<(GuardedDir, LeafName)> {
    // Parse the complete caller-supplied path before a missing parent may be created.
    let normalized = NormalizedPath::new(path).map_err(map_key_custody_error)?;
    let name = normalized
        .as_path()
        .file_name()
        .ok_or_else(|| map_key_custody_error(CustodyError::InvalidPath("path must name a file")))?;
    let name = LeafName::new(name).map_err(map_key_custody_error)?;
    let parent = normalized
        .as_path()
        .parent()
        .ok_or_else(|| map_key_custody_error(CustodyError::InvalidPath("path has no parent")))?;
    let directory = unix_open_private_directory(parent, create_parent)?;
    Ok((directory, name))
}

#[cfg(unix)]
fn map_key_custody_error(error: CustodyError) -> ClientError {
    match error {
        CustodyError::NotFound => {
            ClientError::Io(std::io::Error::from(std::io::ErrorKind::NotFound))
        }
        CustodyError::AlreadyExists => {
            ClientError::Io(std::io::Error::from(std::io::ErrorKind::AlreadyExists))
        }
        CustodyError::Io(error) if key_custody_io_is_policy_failure(&error) => {
            ClientError::Config(format!("private key custody failed: {error}"))
        }
        CustodyError::Io(error) => ClientError::Io(error),
        error => ClientError::Config(format!("private key custody failed: {error}")),
    }
}

#[cfg(unix)]
fn key_custody_io_is_policy_failure(error: &std::io::Error) -> bool {
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

fn validate_external_recovery_dir(path: &Path) -> Result<()> {
    if !path.is_absolute()
        || path.parent().is_none()
        || path.as_os_str().to_string_lossy().len() > MAX_RECOVERY_PATH_BYTES
    {
        return Err(ClientError::Config(
            "recovery directory must be a bounded absolute non-root path".into(),
        ));
    }
    #[cfg(windows)]
    {
        let normalized = windows_custody::normalized_absolute(path)?;
        if normalized != path {
            return Err(ClientError::Config(
                "recovery directory must be a normalized local path with no ambiguous components"
                    .into(),
            ));
        }
        validate_external_recovery_dir_platform(&normalized)
    }
    #[cfg(not(windows))]
    {
        #[cfg(unix)]
        {
            let normalized = NormalizedPath::new(path).map_err(map_key_custody_error)?;
            if normalized.as_path() != path {
                return Err(ClientError::Config(
                    "recovery directory must be an exact normalized absolute path".into(),
                ));
            }
            let directory = GuardedDir::open_existing(path, DirPolicy::private())
                .map_err(map_key_custody_error)?;
            if directory.absolute_path() != path {
                return Err(ClientError::Config(
                    "recovery directory must use its exact physical path without aliases".into(),
                ));
            }
            directory.verify_named().map_err(map_key_custody_error)
        }
        #[cfg(not(unix))]
        {
            let canonical = std::fs::canonicalize(path).map_err(|_| {
                ClientError::Config(
                    "recovery directory must already exist as a canonical private directory".into(),
                )
            })?;
            if canonical != path {
                return Err(ClientError::Config(
                    "recovery directory must be canonical and contain no symbolic-link components"
                        .into(),
                ));
            }
            let named = std::fs::symlink_metadata(path)?;
            if named.file_type().is_symlink() || !named.is_dir() {
                return Err(ClientError::Config(
                    "recovery directory must be a real directory, not a symbolic link".into(),
                ));
            }
            validate_external_recovery_dir_platform(path)
        }
    }
}

#[cfg(windows)]
fn validate_external_recovery_dir_platform(path: &Path) -> Result<()> {
    windows_custody::validate_private_directory(path)
}

#[cfg(not(any(unix, windows)))]
fn validate_external_recovery_dir_platform(_path: &Path) -> Result<()> {
    require_supported_persistent_storage()
}

fn validate_custom_recovery_layout(paths: &KeyPaths) -> Result<()> {
    if !paths.custom_recovery {
        return Ok(());
    }
    if path_is_present(&paths.default_successor)?
        || path_is_present(&paths.legacy_successor)?
        || !path_is_present(&paths.successor)?
    {
        return Err(recovery_migration_error(paths));
    }
    Ok(())
}

fn path_is_present(path: &Path) -> Result<bool> {
    #[cfg(unix)]
    {
        let normalized = NormalizedPath::new(path).map_err(map_key_custody_error)?;
        let name = normalized.as_path().file_name().ok_or_else(|| {
            map_key_custody_error(CustodyError::InvalidPath("path must name an entry"))
        })?;
        let name = LeafName::new(name).map_err(map_key_custody_error)?;
        let parent = normalized.as_path().parent().ok_or_else(|| {
            map_key_custody_error(CustodyError::InvalidPath("path has no parent"))
        })?;
        let directory = match GuardedDir::open_existing(parent, DirPolicy::trusted()) {
            Ok(directory) => directory,
            Err(CustodyError::NotFound) => return Ok(false),
            Err(error) => return Err(map_key_custody_error(error)),
        };
        let present = directory
            .entry_metadata(&name)
            .map_err(map_key_custody_error)?
            .is_some();
        directory.verify_named().map_err(map_key_custody_error)?;
        Ok(present)
    }
    #[cfg(windows)]
    {
        windows_custody::path_is_present(path)
    }
    #[cfg(not(any(unix, windows)))]
    match std::fs::symlink_metadata(path) {
        Ok(_) => Ok(true),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(false),
        Err(error) => Err(error.into()),
    }
}

fn remove_private_file(path: &Path) -> Result<()> {
    #[cfg(unix)]
    {
        let (directory, name) = unix_private_parent_and_leaf(path, false)?;
        let file = directory
            .open_file(&name, OpenAccess::ReadOnly, unix_private_file_policy())
            .map_err(map_key_custody_error)?;
        directory.unlink_file(file).map_err(map_key_custody_error)?;
        directory.verify_named().map_err(map_key_custody_error)
    }
    #[cfg(windows)]
    {
        windows_custody::remove_private_file(path)
    }
    #[cfg(not(any(unix, windows)))]
    {
        std::fs::remove_file(path)?;
        Ok(())
    }
}

fn rename_private_file(source: &Path, destination: &Path) -> Result<()> {
    #[cfg(unix)]
    {
        let (source_directory, source_name) = unix_private_parent_and_leaf(source, false)?;
        let source_file = source_directory
            .open_file(
                &source_name,
                OpenAccess::ReadOnly,
                unix_private_file_policy(),
            )
            .map_err(map_key_custody_error)?;
        let (destination_directory, destination_name) =
            unix_private_parent_and_leaf(destination, false)?;
        source_file.verify_named().map_err(map_key_custody_error)?;
        destination_directory
            .verify_named()
            .map_err(map_key_custody_error)?;
        let published = source_directory
            .publish_no_replace(source_file, &destination_directory, &destination_name)
            .map_err(map_key_custody_error)?;
        published.verify_named().map_err(map_key_custody_error)
    }
    #[cfg(windows)]
    {
        windows_custody::rename_private_file(source, destination)
    }
    #[cfg(not(any(unix, windows)))]
    {
        std::fs::rename(source, destination)?;
        Ok(())
    }
}

fn recovery_migration_error(paths: &KeyPaths) -> ClientError {
    ClientError::Config(format!(
        "configured recovery directory is missing the committed successor or conflicts with {}. Move the existing successor explicitly while the agent is stopped, then reopen with the same recovery directory",
        paths.default_successor.display()
    ))
}

#[cfg(unix)]
fn same_disk(paths: &KeyPaths) -> Result<bool> {
    let operating = open_private_regular(&paths.operating, PrivateOpen::ReadOnly)?;
    let successor = open_private_regular(&paths.successor, PrivateOpen::ReadOnly)?;
    Ok(operating
        .metadata()
        .map_err(map_key_custody_error)?
        .identity
        .device
        == successor
            .metadata()
            .map_err(map_key_custody_error)?
            .identity
            .device)
}

#[cfg(not(unix))]
fn same_disk(_paths: &KeyPaths) -> Result<bool> {
    // There is no portable volume identifier in std; warn conservatively.
    Ok(true)
}

#[cfg(unix)]
fn sync_directory(path: &Path) -> Result<()> {
    let directory = unix_open_private_directory(path, false)?;
    directory.sync().map_err(map_key_custody_error)?;
    directory.verify_named().map_err(map_key_custody_error)
}

#[cfg(not(unix))]
fn sync_directory(_path: &Path) -> Result<()> {
    Ok(())
}

#[cfg(not(any(unix, windows)))]
fn secure_directory_permissions(_path: &Path) -> Result<()> {
    require_supported_persistent_storage()
}

/// Windows custody primitives shared by key files and the SQLite state file.
///
/// Every operation is handle based: reparse points are opened as the reparse object, descriptors
/// are read or written through that handle, file IDs are compared against a second named open,
/// and all parent components are checked before a private object is accepted. This is the Windows
/// counterpart to the Unix `O_NOFOLLOW`/uid/mode/inode boundary above.
#[cfg(windows)]
pub(crate) mod windows_custody {
    use std::ffi::OsString;
    use std::fs::{File, OpenOptions};
    use std::os::windows::fs::{MetadataExt, OpenOptionsExt};
    use std::path::{Component, Path, PathBuf, Prefix};

    use winapi_util::file::{information, typ};
    use windows_permissions::constants::{
        AccessRights, AceType, SeObjectType, SecurityInformation,
    };
    use windows_permissions::{wrappers, LocalBox, SecurityDescriptor, Sid};

    use crate::error::{ClientError, Result};

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

    #[derive(Debug)]
    struct GuardedParent {
        path: PathBuf,
        name: Option<OsString>,
        file: File,
        identity: pigeonpost_windows_custody::FileIdentity,
        guards_target_name: bool,
    }

    /// Retains no-delete-share handles for the complete ancestor chain while a private name is
    /// opened, created, removed, or published.
    #[derive(Debug)]
    pub(crate) struct ParentGuard {
        target: PathBuf,
        components: Vec<GuardedParent>,
    }

    #[derive(Debug)]
    pub(crate) struct PrivateDirectoryGuard {
        path: PathBuf,
        parents: ParentGuard,
        directory: pigeonpost_windows_custody::LockedDirectory,
    }

    /// A private regular file plus the no-delete-share ancestor handles that bind its exact name.
    /// Keeping this value alive prevents another process from replacing either the file or an
    /// ancestor while a long-lived consumer (notably SQLite) is using the path.
    #[derive(Debug)]
    pub(crate) struct RetainedPrivateFile {
        path: PathBuf,
        parents: ParentGuard,
        file: File,
    }

    impl RetainedPrivateFile {
        pub(crate) fn path(&self) -> &Path {
            &self.path
        }

        pub(crate) fn len(&self) -> Result<u64> {
            self.verify()?;
            Ok(self.file.metadata()?.len())
        }

        pub(crate) fn verify(&self) -> Result<()> {
            verify_private_handle(&self.file, &self.path, false)?;
            verify_same_named_object(&self.file, &self.path, false)?;
            self.parents.verify()
        }
    }

    impl PrivateDirectoryGuard {
        pub(crate) fn path(&self) -> &Path {
            &self.path
        }

        pub(crate) fn verify(&self) -> Result<()> {
            verify_private_handle(self.directory.file(), &self.path, true)?;
            let name = self
                .path
                .file_name()
                .ok_or_else(|| custody_error(&self.path, "has no final directory name"))?;
            let reopened =
                pigeonpost_windows_custody::open_directory(self.parents.immediate_parent()?, name)?;
            if reopened.identity() != self.directory.identity() {
                return Err(custody_error(
                    &self.path,
                    "changed while its custody checks were running",
                ));
            }
            self.parents.verify()
        }
    }

    impl ParentGuard {
        fn acquire(path: &Path) -> Result<Self> {
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
            let mut components = Vec::with_capacity(names.len() + 1);
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

        pub(crate) fn target(&self) -> &Path {
            &self.target
        }

        pub(crate) fn immediate_parent(&self) -> Result<&File> {
            self.components
                .last()
                .map(|component| &component.file)
                .ok_or_else(|| custody_error(&self.target, "has no guarded parent directory"))
        }

        pub(crate) fn verify(&self) -> Result<()> {
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
                        "changed while its custody checks were running",
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

        fn verify_private_parent(&self) -> Result<()> {
            let parent = self
                .components
                .last()
                .ok_or_else(|| custody_error(&self.target, "has no guarded parent directory"))?;
            verify_private_handle(&parent.file, &parent.path, true)
        }
    }

    pub(crate) fn guard_private_parent(path: &Path) -> Result<ParentGuard> {
        let guard = ParentGuard::acquire(path)?;
        guard.verify_private_parent()?;
        guard.verify()?;
        Ok(guard)
    }

    pub(crate) fn guard_private_directory(path: &Path) -> Result<PrivateDirectoryGuard> {
        let parents = guard_private_parent(path)?;
        let path = parents.target().to_path_buf();
        let name = path
            .file_name()
            .ok_or_else(|| custody_error(&path, "has no final directory name"))?;
        let directory =
            pigeonpost_windows_custody::open_directory(parents.immediate_parent()?, name)?;
        verify_private_handle(directory.file(), &path, true)?;
        let guard = PrivateDirectoryGuard {
            path,
            parents,
            directory,
        };
        guard.verify()?;
        Ok(guard)
    }

    pub(crate) fn path_is_present(path: &Path) -> Result<bool> {
        let parents = match guard_private_parent(path) {
            Ok(parents) => parents,
            Err(ClientError::Io(error)) if error.kind() == std::io::ErrorKind::NotFound => {
                return Ok(false);
            }
            Err(error) => return Err(error),
        };
        let present = match std::fs::symlink_metadata(parents.target()) {
            Ok(_) => true,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => false,
            Err(error) => return Err(error.into()),
        };
        parents.verify()?;
        Ok(present)
    }

    pub(crate) fn create_new_private_file(path: &Path) -> Result<(File, ParentGuard)> {
        let parents = guard_private_parent(path)?;
        let target = parents.target().to_path_buf();
        let mut create = OpenOptions::new();
        create
            .read(true)
            .write(true)
            .create_new(true)
            .access_mode(GENERIC_READ | GENERIC_WRITE | READ_CONTROL | WRITE_DAC | WRITE_OWNER)
            .share_mode(FILE_SHARE_READ | FILE_SHARE_WRITE)
            .custom_flags(FILE_FLAG_OPEN_REPARSE_POINT);
        let mut file = create.open(&target)?;
        protect_private_file(&mut file, &target)?;
        verify_same_named_object(&file, &target, false)?;
        parents.verify()?;
        Ok((file, parents))
    }

    pub(crate) fn remove_private_file(path: &Path) -> Result<()> {
        let parents = guard_private_parent(path)?;
        let target = parents.target().to_path_buf();
        let file = open_existing_file(&target, false)?;
        verify_private_handle(&file, &target, false)?;
        verify_same_named_object(&file, &target, false)?;
        drop(file);
        std::fs::remove_file(&target)?;
        parents.verify()
    }

    pub(crate) fn rename_private_file(source: &Path, destination: &Path) -> Result<()> {
        let source_parents = guard_private_parent(source)?;
        let destination_parents = guard_private_parent(destination)?;
        let source = source_parents.target().to_path_buf();
        let destination = destination_parents.target().to_path_buf();
        if source == destination {
            return Err(custody_error(&source, "cannot be renamed onto itself"));
        }
        let file = open_existing_file(&source, false)?;
        verify_private_handle(&file, &source, false)?;
        verify_same_named_object(&file, &source, false)?;
        drop(file);
        pigeonpost_windows_custody::move_file_noclobber_write_through(&source, &destination)?;
        let published = open_existing_file(&destination, false)?;
        verify_private_handle(&published, &destination, false)?;
        verify_same_named_object(&published, &destination, false)?;
        source_parents.verify()?;
        destination_parents.verify()
    }

    pub(crate) fn publish_noclobber(temporary: &Path, destination: &Path) -> Result<bool> {
        let source_parents = guard_private_parent(temporary)?;
        let destination_parents = guard_private_parent(destination)?;
        let source = source_parents.target().to_path_buf();
        let destination = destination_parents.target().to_path_buf();
        let source_file = open_existing_file(&source, false)?;
        verify_private_handle(&source_file, &source, false)?;
        verify_same_named_object(&source_file, &source, false)?;
        drop(source_file);

        match pigeonpost_windows_custody::move_file_noclobber_write_through(&source, &destination) {
            Ok(()) => {
                let published = open_existing_file(&destination, false)?;
                verify_private_handle(&published, &destination, false)?;
                verify_same_named_object(&published, &destination, false)?;
                source_parents.verify()?;
                destination_parents.verify()?;
                Ok(true)
            }
            Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => {
                std::fs::remove_file(&source)?;
                let winner = open_existing_file(&destination, false)?;
                verify_private_handle(&winner, &destination, false)?;
                verify_same_named_object(&winner, &destination, false)?;
                source_parents.verify()?;
                destination_parents.verify()?;
                Ok(false)
            }
            Err(error) => {
                let _ = std::fs::remove_file(&source);
                source_parents.verify()?;
                destination_parents.verify()?;
                Err(error.into())
            }
        }
    }

    pub(crate) fn publish_replacement(temporary: &Path, destination: &Path) -> Result<()> {
        let source_parents = guard_private_parent(temporary)?;
        let destination_parents = guard_private_parent(destination)?;
        let source = source_parents.target().to_path_buf();
        let destination = destination_parents.target().to_path_buf();
        let source_file = open_existing_file(&source, false)?;
        verify_private_handle(&source_file, &source, false)?;
        verify_same_named_object(&source_file, &source, false)?;
        drop(source_file);
        match std::fs::symlink_metadata(&destination) {
            Ok(_) => {
                let existing = open_existing_file(&destination, false)?;
                verify_private_handle(&existing, &destination, false)?;
                verify_same_named_object(&existing, &destination, false)?;
            }
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
            Err(error) => return Err(error.into()),
        }

        if let Err(error) =
            pigeonpost_windows_custody::replace_file_write_through(&source, &destination)
        {
            let _ = std::fs::remove_file(&source);
            source_parents.verify()?;
            destination_parents.verify()?;
            return Err(error.into());
        }
        let published = open_existing_file(&destination, false)?;
        verify_private_handle(&published, &destination, false)?;
        verify_same_named_object(&published, &destination, false)?;
        source_parents.verify()?;
        destination_parents.verify()
    }

    pub(crate) fn open_private_file(path: &Path) -> Result<File> {
        let path = normalized_absolute(path)?;
        let file = open_existing_file(&path, false)?;
        verify_private_named(&file, &path)?;
        Ok(file)
    }

    pub(crate) fn retain_private_file_optional(
        path: &Path,
        writable: bool,
    ) -> Result<Option<RetainedPrivateFile>> {
        let parents = guard_private_parent(path)?;
        let path = parents.target().to_path_buf();
        let file = match open_existing_file(&path, writable) {
            Ok(file) => file,
            Err(ClientError::Io(error)) if error.kind() == std::io::ErrorKind::NotFound => {
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

    pub(crate) fn retain_or_create_private_file(
        path: &Path,
    ) -> Result<(RetainedPrivateFile, bool)> {
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
            Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => {
                (open_existing_file(&path, true)?, false)
            }
            Err(error) => return Err(error.into()),
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

    /// Harden and retain a file that a trusted subsystem just created inside an already-private
    /// directory. Callers must first reject any unsafe pre-existing name before handing the path
    /// to that subsystem; this function never serves as a general existing-file repair path.
    pub(crate) fn retain_and_protect_subsystem_file(path: &Path) -> Result<RetainedPrivateFile> {
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
        if information(&file)?.number_of_links() != 1 {
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

    /// Open a private file for mutation, creating it with an owner-only protected DACL if absent.
    /// Existing files are never silently blessed: they must already satisfy the same checks.
    pub(crate) fn open_or_create_private_file(path: &Path) -> Result<(File, bool)> {
        let parents = guard_private_parent(path)?;
        let path = parents.target();
        let mut create = OpenOptions::new();
        create
            .read(true)
            .write(true)
            .create_new(true)
            .access_mode(GENERIC_READ | GENERIC_WRITE | READ_CONTROL | WRITE_DAC | WRITE_OWNER)
            .share_mode(FILE_SHARE_READ | FILE_SHARE_WRITE)
            .custom_flags(FILE_FLAG_OPEN_REPARSE_POINT);
        match create.open(path) {
            Ok(mut file) => {
                protect_private_file(&mut file, path)?;
                verify_private_handle(&file, path, false)?;
                verify_same_named_object(&file, path, false)?;
                parents.verify()?;
                return Ok((file, true));
            }
            Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => {}
            Err(error) => return Err(error.into()),
        }

        // Somebody else created it. They may still be between `create_new` and
        // `protect_private_file`, during which the file carries the parent's inherited security
        // rather than the owner-only one — so an open here can legitimately see an ownership or
        // DACL that is about to become correct. Retry rather than fail the whole open.
        //
        // This is safe to retry because the acceptance condition never softens: every attempt
        // re-runs the full `verify_private_handle` check, so a file that is genuinely somebody
        // else's still fails after the budget. Waiting cannot turn a foreign file into ours.
        //
        // It does NOT close the underlying window — for those few instructions the file really is
        // inheritably-permissioned on disk. Closing that needs the descriptor supplied at
        // CreateFile via SECURITY_ATTRIBUTES, the way create_private_directory already does it.
        // See docs/planning/SESSION-HANDOFF.md.
        const CONTENDED_OPEN_ATTEMPTS: u32 = 200;
        let mut last: Option<ClientError> = None;
        for attempt in 0..CONTENDED_OPEN_ATTEMPTS {
            match open_existing_file(path, true).and_then(|file| {
                verify_private_handle(&file, path, false)?;
                verify_same_named_object(&file, path, false)?;
                parents.verify()?;
                Ok(file)
            }) {
                Ok(file) => return Ok((file, false)),
                Err(error) if is_transient_contended_open(&error) => {
                    last = Some(error);
                    std::thread::sleep(std::time::Duration::from_millis(if attempt < 20 {
                        1
                    } else {
                        5
                    }));
                }
                Err(error) => return Err(error),
            }
        }
        Err(last.unwrap_or_else(|| {
            custody_error(
                path,
                "could not be opened while another process was creating it",
            )
        }))
    }

    /// Whether an error is one a concurrent creator can produce mid-flight, as opposed to a real
    /// custody failure.
    ///
    /// Deliberately narrow. Anything not listed here — a wrong DACL shape, a hard-link count above
    /// one, a reparse point — is a finding, not a race, and must surface immediately.
    fn is_transient_contended_open(error: &ClientError) -> bool {
        match error {
            // The creator has not applied the owner-only descriptor yet.
            ClientError::Config(message) => {
                message.contains("must be owned by the current user")
                    || message.contains("must grant access only to the current user")
            }
            ClientError::Io(io) => matches!(
                io.raw_os_error(),
                // ERROR_FILE_NOT_FOUND / ERROR_PATH_NOT_FOUND: a losing creator removing its
                // temporary between our open and our stat.
                Some(2) | Some(3)
                // ERROR_ACCESS_DENIED: returned for a delete-pending file.
                | Some(5)
                // ERROR_SHARING_VIOLATION: another handle is open without share-delete.
                | Some(32)
            ),
            _ => false,
        }
    }

    /// Assign the current user as owner and apply a protected, current-user-only DACL to a file
    /// that this process just created.
    pub(crate) fn protect_private_file(file: &mut File, path: &Path) -> Result<()> {
        let current = windows_permissions::utilities::current_process_sid()?;
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
        )?;
        verify_private_handle(file, path, false)
    }

    /// Tighten an agent-owned directory and reject any reparse or mutable-parent path.
    pub(crate) fn secure_private_directory(path: &Path) -> Result<()> {
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
        let bound_identity = locked.identity();
        let (directory, opened_identity) = locked.into_parts();
        if opened_identity != bound_identity {
            return Err(custody_error(
                path,
                "creation identity changed before custody",
            ));
        }
        verify_private_handle(&directory, path, true)?;
        let reopened =
            pigeonpost_windows_custody::open_directory(parents.immediate_parent()?, name)?;
        if reopened.identity() != bound_identity {
            return Err(custody_error(
                path,
                "changed while its custody checks were running",
            ));
        }
        parents.verify()
    }

    /// Validate an explicitly supplied recovery directory without changing its ACL.
    pub(crate) fn validate_private_directory(path: &Path) -> Result<()> {
        let parents = ParentGuard::acquire(path)?;
        let path = parents.target();
        let name = path
            .file_name()
            .ok_or_else(|| custody_error(path, "has no final directory name"))?;
        let directory =
            pigeonpost_windows_custody::open_directory(parents.immediate_parent()?, name)?;
        verify_private_handle(directory.file(), path, true)?;
        parents.verify()
    }

    pub(crate) fn verify_private_named(file: &File, path: &Path) -> Result<()> {
        let parents = guard_private_parent(path)?;
        let path = parents.target();
        verify_private_handle(file, path, false)?;
        verify_same_named_object(file, path, false)?;
        parents.verify()
    }

    fn open_existing_file(path: &Path, writable: bool) -> Result<File> {
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
        options.open(path).map_err(Into::into)
    }

    fn open_directory_readonly(path: &Path) -> Result<File> {
        let mut options = OpenOptions::new();
        options
            .read(true)
            .access_mode(GENERIC_READ | READ_CONTROL)
            .share_mode(FILE_SHARE_READ | FILE_SHARE_WRITE)
            .custom_flags(FILE_FLAG_OPEN_REPARSE_POINT | FILE_FLAG_BACKUP_SEMANTICS);
        options.open(path).map_err(Into::into)
    }

    fn open_root_anchor(path: &Path, can_create_child: bool) -> Result<File> {
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
        options.open(path).map_err(Into::into)
    }

    fn split_absolute_parent(path: &Path) -> Result<(PathBuf, Vec<OsString>)> {
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

    fn private_descriptor(current: &Sid, directory: bool) -> Result<LocalBox<SecurityDescriptor>> {
        let ace_flags = if directory { "OICI" } else { "" };
        let sddl = format!("O:{current}D:P(A;{ace_flags};FA;;;{current})");
        sddl.parse().map_err(Into::into)
    }

    fn security_descriptor(file: &File) -> Result<LocalBox<SecurityDescriptor>> {
        wrappers::GetSecurityInfo(
            file,
            SeObjectType::SE_FILE_OBJECT,
            SecurityInformation::Owner | SecurityInformation::Dacl,
        )
        .map_err(Into::into)
    }

    fn verify_private_handle(file: &File, path: &Path, directory: bool) -> Result<()> {
        verify_disk_object(file, path, directory)?;
        let info = information(file)?;
        if !directory && info.number_of_links() != 1 {
            return Err(custody_error(path, "must have exactly one hard link"));
        }

        let descriptor = security_descriptor(file)?;
        let current = windows_permissions::utilities::current_process_sid()?;
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
        )?;
        if !sddl.to_string_lossy().contains("D:P") {
            return Err(custody_error(path, "must have a protected DACL"));
        }
        Ok(())
    }

    fn verify_disk_object(file: &File, path: &Path, directory: bool) -> Result<()> {
        let metadata = file.metadata()?;
        let attributes = metadata.file_attributes();
        let is_directory = attributes & FILE_ATTRIBUTE_DIRECTORY != 0;
        if attributes & FILE_ATTRIBUTE_REPARSE_POINT != 0
            || is_directory != directory
            || !typ(file)?.is_disk()
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

    fn verify_same_named_object(file: &File, path: &Path, directory: bool) -> Result<()> {
        let named = if directory {
            open_directory_readonly(path)?
        } else {
            open_existing_file(path, false)?
        };
        verify_disk_object(&named, path, directory)?;
        let opened_identity = pigeonpost_windows_custody::file_identity(file)?;
        let named_identity = pigeonpost_windows_custody::file_identity(&named)?;
        let named_info = information(&named)?;
        if opened_identity != named_identity || (!directory && named_info.number_of_links() != 1) {
            return Err(custody_error(
                path,
                "changed while its custody checks were running",
            ));
        }
        Ok(())
    }

    pub(crate) fn normalized_absolute(path: &Path) -> Result<PathBuf> {
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
    ) -> Result<()> {
        let descriptor = security_descriptor(directory)?;
        let current = windows_permissions::utilities::current_process_sid()?;
        let owner = descriptor
            .owner()
            .ok_or_else(|| custody_error(path, "parent component has no owner"))?;
        if !trusted_principal(owner, &current) {
            return Err(custody_error(
                path,
                "parent component is owned by an untrusted principal",
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
                    "parent component grants mutation rights to an untrusted principal",
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
                // FILE_WRITE_EA, FILE_DELETE_CHILD, and FILE_WRITE_ATTRIBUTES can alter or replace
                // an existing traversed component.
                | AccessRights::Bit4
                | AccessRights::Bit6
                | AccessRights::Bit8,
        ) || (guards_target_name
            // FILE_ADD_FILE and FILE_ADD_SUBDIRECTORY can squat the one direct child name this
            // directory guards. On a grandparent they can create only an unrelated sibling unless
            // combined with one of the replacement rights above.
            && rights.intersects(AccessRights::Bit1 | AccessRights::Bit2))
    }

    fn trusted_principal(sid: &Sid, current: &Sid) -> bool {
        if sid == current {
            return true;
        }
        matches!(
            sid.to_string().as_str(),
            // LocalSystem, BUILTIN\\Administrators, and Windows Modules Installer.
            "S-1-5-18"
                | "S-1-5-32-544"
                | "S-1-5-80-956008885-3418522649-1831038044-1853292631-2271478464"
        )
    }

    fn custody_error(path: &Path, reason: &str) -> ClientError {
        ClientError::Config(format!("private storage {} {reason}", path.display()))
    }

    #[cfg(test)]
    mod tests {
        use super::*;

        #[test]
        fn child_creation_rights_are_dangerous_only_on_the_immediate_parent() {
            let child_creation = AccessRights::Bit1 | AccessRights::Bit2;
            assert!(dangerous_parent_rights(child_creation, true));
            assert!(!dangerous_parent_rights(child_creation, false));
            assert!(dangerous_parent_rights(AccessRights::Bit6, false));
        }

        #[test]
        fn retained_chain_blocks_ancestor_rename() {
            let root = tempfile::tempdir().unwrap();
            let private = root.path().join("private");
            secure_private_directory(&private).unwrap();
            let nested = private.join("nested");
            secure_private_directory(&nested).unwrap();
            let guard = ParentGuard::acquire(&nested.join("identity.key")).unwrap();
            let moved = root.path().join("moved");

            assert!(std::fs::rename(&private, &moved).is_err());
            drop(guard);
            std::fs::rename(&private, &moved).unwrap();
        }

        #[test]
        fn existing_permissive_directory_is_rejected_without_adoption() {
            let root = tempfile::tempdir().unwrap();
            let private = root.path().join("private");
            secure_private_directory(&private).unwrap();
            let insecure = private.join("insecure");
            std::fs::create_dir(&insecure).unwrap();

            let mut options = OpenOptions::new();
            options
                .read(true)
                .access_mode(GENERIC_READ | READ_CONTROL | WRITE_DAC)
                .share_mode(FILE_SHARE_READ | FILE_SHARE_WRITE)
                .custom_flags(FILE_FLAG_OPEN_REPARSE_POINT | FILE_FLAG_BACKUP_SEMANTICS);
            let mut directory = options.open(&insecure).unwrap();
            let current = windows_permissions::utilities::current_process_sid().unwrap();
            let permissive: LocalBox<SecurityDescriptor> =
                format!("O:{current}D:P(A;OICI;FA;;;{current})(A;OICI;FA;;;WD)")
                    .parse()
                    .unwrap();
            wrappers::SetSecurityInfo(
                &mut directory,
                SeObjectType::SE_FILE_OBJECT,
                SecurityInformation::Dacl | SecurityInformation::ProtectedDacl,
                None,
                None,
                permissive.dacl(),
                None,
            )
            .unwrap();
            drop(directory);

            assert!(secure_private_directory(&insecure).is_err());
            let unchanged = open_directory_readonly(&insecure).unwrap();
            assert_eq!(
                security_descriptor(&unchanged)
                    .unwrap()
                    .dacl()
                    .unwrap()
                    .len(),
                2
            );
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn persistent_storage_platform_guard_is_fail_closed() {
        assert!(matches!(
            require_supported_persistent_storage_for(false),
            Err(ClientError::Io(error)) if error.kind() == std::io::ErrorKind::Unsupported
        ));
        require_supported_persistent_storage_for(true).unwrap();
    }

    #[cfg(not(any(target_os = "linux", target_os = "macos", windows)))]
    #[test]
    fn persistent_entry_points_reject_before_path_access_or_creation() {
        let root = tempfile::tempdir().unwrap();
        let home = root.path().join("must-not-exist");
        let paths = KeyPaths::in_dir(&home);

        assert!(matches!(
            load_or_create(&paths),
            Err(ClientError::Io(error)) if error.kind() == std::io::ErrorKind::Unsupported
        ));
        assert!(!home.exists());

        let recovery = root.path().join("also-must-not-exist");
        assert!(matches!(
            KeyPaths::in_dir_with_recovery_dir(&home, &recovery),
            Err(ClientError::Io(error)) if error.kind() == std::io::ErrorKind::Unsupported
        ));
        assert!(!recovery.exists());
    }

    fn private_paths(root: &tempfile::TempDir) -> KeyPaths {
        KeyPaths::in_dir(&root.path().join("agent"))
    }

    #[cfg(not(any(unix, windows)))]
    #[test]
    fn final_directory_creation_provenance_marks_only_the_create_winner() {
        let root = tempfile::tempdir().unwrap();
        let path = root.path().join("missing-parent").join("private");

        assert!(create_final_directory(&path).unwrap());
        assert!(!create_final_directory(&path).unwrap());
    }

    #[test]
    fn creates_both_keys_on_first_run_and_reloads_the_same_identity() {
        let dir = tempfile::tempdir().unwrap();
        let paths = private_paths(&dir);

        let first = load_or_create(&paths).unwrap();
        assert!(first.freshly_created);
        assert!(paths.operating.exists());
        assert!(paths.token_secret.exists());
        assert!(
            paths.successor.exists(),
            "an agent with no committed successor can never rotate"
        );

        let again = load_or_create(&paths).unwrap();
        assert!(!again.freshly_created);
        assert_eq!(
            first.identity.verifying_key(),
            again.identity.verifying_key()
        );
        assert_eq!(first.successor.as_bytes(), again.successor.as_bytes());
        assert_eq!(first.token_secret, again.token_secret);
    }

    #[test]
    fn the_successor_is_a_different_key() {
        let dir = tempfile::tempdir().unwrap();
        let paths = private_paths(&dir);
        let loaded = load_or_create(&paths).unwrap();

        assert!(
            !loaded.successor.accepts(&loaded.identity.verifying_key()),
            "committing to yourself would make rotation meaningless"
        );
    }

    #[cfg(unix)]
    #[test]
    fn keys_are_not_world_readable() {
        use std::os::unix::fs::PermissionsExt;
        let dir = tempfile::tempdir().unwrap();
        let paths = private_paths(&dir);
        load_or_create(&paths).unwrap();

        for path in [&paths.operating, &paths.successor, &paths.token_secret] {
            let mode = std::fs::metadata(path).unwrap().permissions().mode();
            assert_eq!(mode & 0o077, 0, "{} is readable by others", path.display());
        }
    }

    #[cfg(windows)]
    #[test]
    fn windows_keys_have_handle_verified_private_custody() {
        let dir = tempfile::tempdir().unwrap();
        let paths = private_paths(&dir);
        load_or_create(&paths).unwrap();

        for path in [&paths.operating, &paths.successor, &paths.token_secret] {
            let file = windows_custody::open_private_file(path).unwrap();
            windows_custody::verify_private_named(&file, path).unwrap();
        }

        let second_name = paths
            .operating
            .parent()
            .unwrap()
            .join("identity-hardlink.key");
        std::fs::hard_link(&paths.operating, &second_name).unwrap();
        assert!(
            windows_custody::open_private_file(&paths.operating).is_err(),
            "a second name defeats single-file custody and must be rejected"
        );
        std::fs::remove_file(second_name).unwrap();
        windows_custody::open_private_file(&paths.operating).unwrap();
    }

    #[cfg(windows)]
    #[test]
    fn windows_rejects_unprotected_files_and_parent_reparse_points() {
        let dir = tempfile::tempdir().unwrap();
        let private = dir.path().join("private");
        secure_key_dir(&private).unwrap();

        let inherited_only = private.join("inherited.key");
        std::fs::write(&inherited_only, [7u8; 32]).unwrap();
        assert!(
            windows_custody::open_private_file(&inherited_only).is_err(),
            "an existing file must not be silently blessed with a protected DACL"
        );

        let target = dir.path().join("target");
        secure_key_dir(&target).unwrap();
        let junction = dir.path().join("junction");
        if std::os::windows::fs::symlink_dir(&target, &junction).is_ok() {
            assert!(
                secure_key_dir(&junction.join("nested")).is_err(),
                "a reparse point in a parent component must fail closed"
            );
        }
    }

    #[cfg(windows)]
    #[test]
    fn windows_secret_replacement_is_atomic_and_remains_private() {
        let dir = tempfile::tempdir().unwrap();
        let paths = private_paths(&dir);
        load_or_create(&paths).unwrap();

        replace_secret(&paths.token_secret, &[0xA7; 32]).unwrap();
        assert_eq!(read_secret(&paths.token_secret).unwrap(), [0xA7; 32]);
        windows_custody::open_private_file(&paths.token_secret).unwrap();
        assert_eq!(
            std::fs::read_dir(paths.operating.parent().unwrap())
                .unwrap()
                .filter_map(|entry| entry.ok())
                .filter(|entry| entry.file_name().to_string_lossy().contains(".replace"))
                .count(),
            0
        );
    }

    #[test]
    fn same_disk_is_detected_so_it_can_be_warned_about() {
        let dir = tempfile::tempdir().unwrap();
        let paths = private_paths(&dir);
        load_or_create(&paths).unwrap();
        assert!(successor_shares_a_disk(&paths));
    }

    #[test]
    fn a_corrupt_key_file_is_a_clear_error() {
        let dir = tempfile::tempdir().unwrap();
        let paths = private_paths(&dir);
        load_or_create(&paths).unwrap();
        std::fs::write(&paths.operating, b"not a key").unwrap();

        assert!(matches!(
            load_or_create(&paths),
            Err(ClientError::Config(_))
        ));
    }

    #[test]
    fn concurrent_first_run_converges_on_one_identity_and_successor() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path().join("agent");
        let barrier = std::sync::Arc::new(std::sync::Barrier::new(8));
        let mut threads = Vec::new();
        for _ in 0..8 {
            let root = root.clone();
            let barrier = std::sync::Arc::clone(&barrier);
            threads.push(std::thread::spawn(move || {
                barrier.wait();
                let loaded = (0..5_000)
                    .find_map(|_| match load_or_create(&KeyPaths::in_dir(&root)) {
                        Ok(loaded) => Some(loaded),
                        Err(ClientError::Config(message)) if message.contains("busy") => {
                            std::thread::sleep(std::time::Duration::from_millis(1));
                            None
                        }
                        Err(error) => panic!("unexpected first-open error: {error}"),
                    })
                    .expect("first-open retry budget exhausted");
                (
                    loaded.identity.verifying_key().to_bytes(),
                    *loaded.successor.as_bytes(),
                    loaded.token_secret,
                )
            }));
        }

        let results: Vec<_> = threads
            .into_iter()
            .map(|thread| thread.join().unwrap())
            .collect();
        assert!(results.iter().all(|result| result == &results[0]));
    }

    #[test]
    fn opening_while_the_identity_is_leased_fails_promptly() {
        let dir = tempfile::tempdir().unwrap();
        let paths = private_paths(&dir);
        let loaded = load_or_create(&paths).unwrap();
        let _lease =
            acquire_active_identity_lease(&paths, loaded.identity.verifying_key().as_bytes())
                .unwrap();

        let error = match load_or_create(&paths) {
            Err(error) => error,
            Ok(_) => panic!("opening unexpectedly waited through the active lease"),
        };
        assert!(matches!(
            error,
            ClientError::Config(message) if message.contains("busy")
        ));
    }

    #[test]
    fn native_lock_contention_has_one_busy_classification() {
        assert!(is_lock_contention(&fs2::lock_contended_error()));
        assert!(is_lock_contention(&std::io::Error::new(
            std::io::ErrorKind::WouldBlock,
            "synthetic contention",
        )));
        assert!(!is_lock_contention(&std::io::Error::new(
            std::io::ErrorKind::PermissionDenied,
            "unrelated denial",
        )));
    }

    #[test]
    fn legacy_successor_is_moved_without_changing_the_commitment() {
        let dir = tempfile::tempdir().unwrap();
        let paths = private_paths(&dir);
        let operating = Identity::from_seed([1; 32]);
        let successor = Identity::from_seed([2; 32]);
        create_key_once(&paths.operating, &operating).unwrap();
        create_key_once(&paths.legacy_successor, &successor).unwrap();

        let loaded = load_or_create(&paths).unwrap();
        assert!(!loaded.freshly_created);
        assert!(loaded.successor.accepts(&successor.verifying_key()));
        assert!(paths.successor.exists());
        assert!(!paths.legacy_successor.exists());
        assert_ne!(paths.operating.parent(), paths.successor.parent());
    }

    #[cfg(unix)]
    #[test]
    fn key_symlinks_are_refused() {
        use std::os::unix::fs::symlink;

        let dir = tempfile::tempdir().unwrap();
        let paths = KeyPaths::in_dir(dir.path());
        std::fs::write(dir.path().join("elsewhere"), [7; 32]).unwrap();
        symlink(dir.path().join("elsewhere"), &paths.operating).unwrap();

        assert!(matches!(
            load_or_create(&paths),
            Err(ClientError::Config(_))
        ));
    }

    #[cfg(unix)]
    #[test]
    fn unix_key_creation_rejects_ambiguous_symlinked_and_mutable_ancestors_without_effects() {
        use std::os::unix::fs::{symlink, PermissionsExt};

        let root = tempfile::tempdir().unwrap();
        let escaped = root.path().join("escaped");
        let ambiguous = root
            .path()
            .join("must-not-exist")
            .join("..")
            .join("escaped");
        assert!(load_or_create(&KeyPaths::in_dir(&ambiguous)).is_err());
        assert!(!root.path().join("must-not-exist").exists());
        assert!(!escaped.exists());

        let outside = root.path().join("outside");
        std::fs::create_dir(&outside).unwrap();
        std::fs::set_permissions(&outside, std::fs::Permissions::from_mode(0o700)).unwrap();
        let linked = root.path().join("linked");
        symlink(&outside, &linked).unwrap();
        assert!(load_or_create(&KeyPaths::in_dir(&linked.join("agent"))).is_err());
        assert!(!outside.join("agent").exists());

        let mutable = root.path().join("mutable");
        std::fs::create_dir(&mutable).unwrap();
        std::fs::set_permissions(&mutable, std::fs::Permissions::from_mode(0o770)).unwrap();
        assert!(load_or_create(&KeyPaths::in_dir(&mutable.join("agent"))).is_err());
        assert!(!mutable.join("agent").exists());
    }

    #[cfg(unix)]
    #[test]
    fn unix_retained_key_guard_detects_file_and_parent_replacement() {
        use std::os::unix::fs::PermissionsExt;

        let root = tempfile::tempdir().unwrap();
        let paths = private_paths(&root);
        load_or_create(&paths).unwrap();
        let retained = open_private_regular(&paths.operating, PrivateOpen::ReadOnly).unwrap();
        let original = paths.operating.with_extension("original");
        std::fs::rename(&paths.operating, &original).unwrap();
        std::fs::write(&paths.operating, [0xA5; 32]).unwrap();
        std::fs::set_permissions(&paths.operating, std::fs::Permissions::from_mode(0o600)).unwrap();
        assert!(retained.verify_named().is_err());

        let root = tempfile::tempdir().unwrap();
        let paths = private_paths(&root);
        load_or_create(&paths).unwrap();
        let retained = open_private_regular(&paths.operating, PrivateOpen::ReadOnly).unwrap();
        let parent = paths.operating.parent().unwrap();
        let moved_parent = root.path().join("agent.original");
        std::fs::rename(parent, &moved_parent).unwrap();
        std::fs::create_dir(parent).unwrap();
        std::fs::set_permissions(parent, std::fs::Permissions::from_mode(0o700)).unwrap();
        std::fs::write(&paths.operating, [0x5A; 32]).unwrap();
        std::fs::set_permissions(&paths.operating, std::fs::Permissions::from_mode(0o600)).unwrap();
        assert!(retained.verify_named().is_err());
    }

    #[cfg(unix)]
    #[test]
    fn unix_no_replace_secret_publication_preserves_collision_winner() {
        let root = tempfile::tempdir().unwrap();
        let path = root.path().join("agent/token.secret");
        assert!(create_secret_once(&path, &[0x11; 32]).unwrap());
        assert!(!create_secret_once(&path, &[0x22; 32]).unwrap());
        assert_eq!(read_secret(&path).unwrap(), [0x11; 32]);
        let parent = unix_open_private_directory(path.parent().unwrap(), false).unwrap();
        assert!(parent.list_bounded(8).unwrap().iter().all(|entry| !entry
            .name
            .as_os_str()
            .to_string_lossy()
            .ends_with(".tmp")));
    }

    #[cfg(unix)]
    #[test]
    fn unix_retired_inventory_stops_at_the_fixed_entry_bound() {
        let root = tempfile::tempdir().unwrap();
        let paths = private_paths(&root);
        secure_key_dir(&paths.retired_dir).unwrap();
        for index in 0..=MAX_RETIRED_DIRECTORY_ENTRIES {
            let path = paths.retired_dir.join(format!("{index:064x}.key"));
            write_new_file(&path, &[index as u8; 32]).unwrap();
        }
        assert!(matches!(
            retired_identities(&paths, now_secs()),
            Err(ClientError::Config(message)) if message.contains("limit")
        ));
    }

    #[test]
    fn a_fresh_token_secret_is_independent_of_the_identity() {
        let dir = tempfile::tempdir().unwrap();
        let paths = private_paths(&dir);
        let loaded = load_or_create(&paths).unwrap();

        assert!(loaded.freshly_created);
        assert_ne!(loaded.token_secret, legacy_token_secret(&loaded.identity));
        assert_ne!(loaded.token_secret, loaded.identity.to_seed());
    }

    #[test]
    fn an_existing_identity_migrates_to_its_legacy_token_secret() {
        let dir = tempfile::tempdir().unwrap();
        let paths = private_paths(&dir);
        let operating = Identity::from_seed([3; 32]);
        let successor = Identity::from_seed([4; 32]);
        create_key_once(&paths.operating, &operating).unwrap();
        create_key_once(&paths.successor, &successor).unwrap();

        let loaded = load_or_create(&paths).unwrap();
        assert!(!loaded.freshly_created);
        assert_eq!(loaded.token_secret, legacy_token_secret(&operating));
        assert_eq!(
            read_secret(&paths.token_secret).unwrap(),
            loaded.token_secret
        );
    }

    #[test]
    fn rotation_promotes_successor_preserves_tokens_and_expires_retired_key_exactly() {
        let dir = tempfile::tempdir().unwrap();
        let paths = private_paths(&dir);
        let loaded = load_or_create(&paths).unwrap();
        let outgoing_address = loaded.identity.address();
        let incoming = read_key(&paths.successor).unwrap();
        let incoming_key = incoming.verifying_key();
        let token_secret = loaded.token_secret;
        let source = AgentRecord::new(
            &loaded.identity,
            &loaded.successor,
            9,
            vec!["https://loft.example".into()],
        );
        let activated_at = now_secs();

        let outcome = rotate(
            &paths,
            &source,
            &["https://loft.example".into()],
            activated_at,
        )
        .unwrap();
        assert_eq!(outcome.identity.verifying_key(), incoming_key);
        assert_eq!(outcome.record.seq, 10);
        assert_eq!(
            outcome.record.target_address().unwrap(),
            outcome.identity.address()
        );
        assert_ne!(outcome.successor.as_bytes(), loaded.successor.as_bytes());

        let reopened = load_or_create(&paths).unwrap();
        assert_eq!(reopened.identity.verifying_key(), incoming_key);
        assert_eq!(reopened.token_secret, token_secret);
        assert_eq!(reopened.retired.len(), 1);
        assert_eq!(reopened.retired[0].identity.address(), outgoing_address);
        assert_eq!(
            retired_identities(&paths, outcome.record.grace_until - 1)
                .unwrap()
                .len(),
            1
        );
        assert!(retired_identities(&paths, outcome.record.grace_until)
            .unwrap()
            .is_empty());
        assert!(retired_identities(&paths, outcome.record.grace_until + 1)
            .unwrap()
            .is_empty());
    }

    #[test]
    fn rotation_grace_set_stops_at_the_fixed_boundary_without_partial_mutation() {
        let dir = tempfile::tempdir().unwrap();
        let paths = private_paths(&dir);
        let loaded = load_or_create(&paths).unwrap();
        let mut source = AgentRecord::new(&loaded.identity, &loaded.successor, 1, vec![]);
        let activated_at = now_secs();

        for offset in 0..MAX_LIVE_RETIRED_IDENTITIES {
            let outcome = rotate(&paths, &source, &[], activated_at + offset as u64).unwrap();
            source = outcome.target_record;
        }
        assert_eq!(
            retired_identities(&paths, activated_at + MAX_LIVE_RETIRED_IDENTITIES as u64)
                .unwrap()
                .len(),
            MAX_LIVE_RETIRED_IDENTITIES
        );
        assert_eq!(
            std::fs::read_dir(&paths.retired_dir).unwrap().count(),
            MAX_RETIRED_DIRECTORY_ENTRIES
        );

        let operating_before = read_secret(&paths.operating).unwrap();
        let successor_before = read_secret(&paths.successor).unwrap();
        let token_before = read_secret(&paths.token_secret).unwrap();
        assert!(matches!(
            rotate(
                &paths,
                &source,
                &[],
                activated_at + MAX_LIVE_RETIRED_IDENTITIES as u64
            ),
            Err(ClientError::Config(_))
        ));
        assert_eq!(read_secret(&paths.operating).unwrap(), operating_before);
        assert_eq!(read_secret(&paths.successor).unwrap(), successor_before);
        assert_eq!(read_secret(&paths.token_secret).unwrap(), token_before);
        assert!(!paths.rotation_journal.exists());
        assert!(!paths.staged_successor.exists());
        assert_eq!(
            std::fs::read_dir(&paths.retired_dir).unwrap().count(),
            MAX_RETIRED_DIRECTORY_ENTRIES
        );
    }

    #[test]
    fn retired_directory_rejects_unexpected_or_unpaired_entries() {
        for name in ["unexpected".to_owned(), format!("{}.key", "a".repeat(64))] {
            let dir = tempfile::tempdir().unwrap();
            let paths = private_paths(&dir);
            load_or_create(&paths).unwrap();
            secure_key_dir(&paths.retired_dir).unwrap();
            write_new_file(&paths.retired_dir.join(name), &[0xA5; 32]).unwrap();

            assert!(matches!(
                retired_identities(&paths, now_secs()),
                Err(ClientError::Config(_))
            ));
        }
    }

    #[test]
    fn interrupted_rotation_is_completed_when_the_agent_reopens() {
        let dir = tempfile::tempdir().unwrap();
        let paths = private_paths(&dir);
        let loaded = load_or_create(&paths).unwrap();
        let source = AgentRecord::new(&loaded.identity, &loaded.successor, 20, vec![]);
        let original_token = loaded.token_secret;
        let expected_incoming = read_key(&paths.successor).unwrap().verifying_key();

        let lock = acquire_rotation_lock(&paths).unwrap();
        let journal = prepare_rotation_locked(&paths, &source, &[], now_secs()).unwrap();
        drop(lock); // Simulate process death after the durable journal, before promotion.

        let recovered = load_or_create(&paths).unwrap();
        assert_eq!(recovered.identity.verifying_key(), expected_incoming);
        assert_eq!(recovered.token_secret, original_token);
        assert_eq!(recovered.retired.len(), 1);
        assert_eq!(recovered.retired[0].record, journal.record);
        assert!(!paths.rotation_journal.exists());
        assert!(!paths.staged_successor.exists());
    }

    #[test]
    fn recovery_accepts_only_the_pending_journals_incomplete_retired_pair() {
        let dir = tempfile::tempdir().unwrap();
        let paths = private_paths(&dir);
        let loaded = load_or_create(&paths).unwrap();
        let source = AgentRecord::new(&loaded.identity, &loaded.successor, 21, vec![]);
        let expected_incoming = read_key(&paths.successor).unwrap().verifying_key();

        let lock = acquire_rotation_lock(&paths).unwrap();
        let journal = prepare_rotation_locked(&paths, &source, &[], now_secs()).unwrap();
        secure_key_dir(&paths.retired_dir).unwrap();
        let incomplete_key = retired_key_path(&paths, &journal.record);
        create_key_once(&incomplete_key, &loaded.identity).unwrap();
        let leftover = paths
            .operating
            .parent()
            .unwrap()
            .join(".retired-metadata.pending.replace");
        write_new_file(&leftover, b"untrusted crash remnant").unwrap();
        assert!(!retired_metadata_path(&paths, &journal.record).exists());
        drop(lock); // Death after outgoing-key retention, before metadata publication.

        let recovered = load_or_create(&paths).unwrap();
        assert_eq!(recovered.identity.verifying_key(), expected_incoming);
        assert_eq!(recovered.retired.len(), 1);
        assert_eq!(recovered.retired[0].record, journal.record);
        assert!(retired_metadata_path(&paths, &journal.record).exists());
        assert!(!leftover.exists());
        assert!(!paths.rotation_journal.exists());
        assert!(!paths.staged_successor.exists());
    }

    #[test]
    fn recovery_rejects_a_non_file_retired_metadata_crash_remnant_before_promotion() {
        let dir = tempfile::tempdir().unwrap();
        let paths = private_paths(&dir);
        let loaded = load_or_create(&paths).unwrap();
        let source = AgentRecord::new(&loaded.identity, &loaded.successor, 22, vec![]);
        let outgoing = loaded.identity.verifying_key();

        let lock = acquire_rotation_lock(&paths).unwrap();
        let journal = prepare_rotation_locked(&paths, &source, &[], now_secs()).unwrap();
        secure_key_dir(&paths.retired_dir).unwrap();
        create_key_once(&retired_key_path(&paths, &journal.record), &loaded.identity).unwrap();
        let crash_remnant = paths
            .operating
            .parent()
            .unwrap()
            .join(".retired-metadata.pending.replace");
        secure_key_dir(&crash_remnant).unwrap();
        drop(lock);

        let error = match load_or_create(&paths) {
            Ok(_) => panic!("a directory was accepted at a file-only crash-remnant name"),
            Err(error) => error,
        };
        #[cfg(windows)]
        assert!(
            matches!(&error, ClientError::Config(_))
                || matches!(
                    &error,
                    ClientError::Io(error)
                        if error.kind() == std::io::ErrorKind::PermissionDenied
                ),
            "Windows must reject a directory at a file-only crash-remnant name: {error}"
        );
        #[cfg(not(windows))]
        assert!(matches!(error, ClientError::Config(_)));
        assert_eq!(
            read_key(&paths.operating).unwrap().verifying_key(),
            outgoing
        );
        assert!(paths.rotation_journal.exists());
        assert!(paths.staged_successor.exists());
    }

    #[cfg(unix)]
    fn private_recovery_dir(root: &Path, name: &str) -> PathBuf {
        use std::os::unix::fs::PermissionsExt;

        let path = root.join(name);
        std::fs::create_dir(&path).unwrap();
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o700)).unwrap();
        std::fs::canonicalize(path).unwrap()
    }

    #[cfg(unix)]
    #[test]
    fn external_recovery_layout_creates_reopens_rotates_and_recovers_a_crash() {
        let root = tempfile::tempdir().unwrap();
        let home = root.path().join("agent");
        std::fs::create_dir(&home).unwrap();
        let recovery = private_recovery_dir(root.path(), "recovery-device");
        let paths = KeyPaths::in_dir_with_recovery_dir(&home, &recovery).unwrap();

        let loaded = load_or_create(&paths).unwrap();
        assert!(loaded.freshly_created);
        assert!(recovery.join("successor.key").exists());
        assert!(!home.join("recovery").join("successor.key").exists());
        let original_token = loaded.token_secret;
        let incoming = read_key(&paths.successor).unwrap().verifying_key();
        let source = AgentRecord::new(&loaded.identity, &loaded.successor, 30, vec![]);

        // Simulate death after the journal is durable but before promotion. The external staged
        // successor is recovered through the same explicit layout on reopen.
        let lock = acquire_rotation_lock(&paths).unwrap();
        let journal = prepare_rotation_locked(&paths, &source, &[], now_secs()).unwrap();
        drop(lock);
        let recovered = load_or_create(&paths).unwrap();
        assert_eq!(recovered.identity.verifying_key(), incoming);
        assert_eq!(recovered.token_secret, original_token);
        assert_eq!(recovered.retired[0].record, journal.record);
        assert!(!paths.rotation_journal.exists());
        assert!(!paths.staged_successor.exists());

        let second_source = AgentRecord::new(
            &recovered.identity,
            &recovered.successor,
            journal.record.seq,
            vec![],
        );
        let rotated = rotate(&paths, &second_source, &[], now_secs()).unwrap();
        assert_ne!(rotated.identity.verifying_key(), incoming);
        assert_eq!(
            load_or_create(&paths).unwrap().identity.address(),
            rotated.identity.address()
        );
        assert!(!home.join("recovery").join("successor.key").exists());
    }

    #[cfg(unix)]
    #[test]
    fn external_recovery_directory_validation_fails_without_replacing_anything() {
        use std::os::unix::fs::{symlink, PermissionsExt};

        let root = tempfile::tempdir().unwrap();
        let home = root.path().join("agent");
        std::fs::create_dir(&home).unwrap();
        let missing = root.path().join("missing");
        assert!(KeyPaths::in_dir_with_recovery_dir(&home, &missing).is_err());
        assert!(KeyPaths::in_dir_with_recovery_dir(&home, Path::new("relative")).is_err());
        assert!(KeyPaths::in_dir_with_recovery_dir(&home, Path::new("/")).is_err());

        let file = root.path().join("not-a-directory");
        std::fs::write(&file, b"sentinel").unwrap();
        assert!(KeyPaths::in_dir_with_recovery_dir(&home, &file).is_err());
        assert_eq!(std::fs::read(&file).unwrap(), b"sentinel");

        let insecure = root.path().join("insecure");
        std::fs::create_dir(&insecure).unwrap();
        std::fs::set_permissions(&insecure, std::fs::Permissions::from_mode(0o755)).unwrap();
        assert!(KeyPaths::in_dir_with_recovery_dir(&home, &insecure).is_err());
        assert_eq!(
            std::fs::metadata(&insecure).unwrap().permissions().mode() & 0o777,
            0o755,
            "explicit recovery custody must never silently chmod an insecure directory"
        );

        let target = private_recovery_dir(root.path(), "target");
        let link = root.path().join("recovery-link");
        symlink(&target, &link).unwrap();
        assert!(KeyPaths::in_dir_with_recovery_dir(&home, &link).is_err());
        assert!(!target.join("successor.key").exists());

        let mutable_parent = root.path().join("mutable-parent");
        std::fs::create_dir(&mutable_parent).unwrap();
        std::fs::set_permissions(&mutable_parent, std::fs::Permissions::from_mode(0o777)).unwrap();
        let traversed = mutable_parent.join("private-leaf");
        std::fs::create_dir(&traversed).unwrap();
        std::fs::set_permissions(&traversed, std::fs::Permissions::from_mode(0o700)).unwrap();
        let traversed = std::fs::canonicalize(traversed).unwrap();
        assert!(KeyPaths::in_dir_with_recovery_dir(&home, &traversed).is_err());
        assert!(!traversed.join("successor.key").exists());
        std::fs::set_permissions(&mutable_parent, std::fs::Permissions::from_mode(0o700)).unwrap();
    }

    #[cfg(unix)]
    #[test]
    fn configured_recovery_never_silently_moves_an_existing_default_successor() {
        let root = tempfile::tempdir().unwrap();
        let home = root.path().join("agent");
        std::fs::create_dir(&home).unwrap();
        let default_paths = KeyPaths::in_dir(&home);
        load_or_create(&default_paths).unwrap();
        let original = std::fs::read(&default_paths.successor).unwrap();
        let recovery = private_recovery_dir(root.path(), "external");
        let external = KeyPaths::in_dir_with_recovery_dir(&home, &recovery).unwrap();

        let error = match load_or_create(&external) {
            Err(error) => error,
            Ok(_) => panic!("custom recovery unexpectedly moved the default successor"),
        };
        assert!(
            matches!(error, ClientError::Config(message) if message.contains("Move the existing successor explicitly"))
        );
        assert_eq!(std::fs::read(&default_paths.successor).unwrap(), original);
        assert!(!external.successor.exists());
    }

    #[cfg(unix)]
    #[test]
    fn an_external_directory_on_the_same_device_still_triggers_the_warning() {
        let root = tempfile::tempdir().unwrap();
        let home = root.path().join("agent");
        std::fs::create_dir(&home).unwrap();
        let recovery = private_recovery_dir(root.path(), "external");
        let paths = KeyPaths::in_dir_with_recovery_dir(&home, &recovery).unwrap();
        load_or_create(&paths).unwrap();
        assert!(successor_shares_a_disk(&paths));
    }

    #[cfg(unix)]
    #[test]
    fn token_secret_symlinks_are_refused() {
        use std::os::unix::fs::symlink;

        let dir = tempfile::tempdir().unwrap();
        let paths = KeyPaths::in_dir(dir.path());
        let identity = Identity::from_seed([5; 32]);
        let successor = Identity::from_seed([6; 32]);
        create_key_once(&paths.operating, &identity).unwrap();
        create_key_once(&paths.successor, &successor).unwrap();
        std::fs::write(dir.path().join("elsewhere-secret"), [7; 32]).unwrap();
        symlink(dir.path().join("elsewhere-secret"), &paths.token_secret).unwrap();

        assert!(matches!(
            load_or_create(&paths),
            Err(ClientError::Config(_))
        ));
    }

    #[cfg(unix)]
    #[test]
    fn existing_keys_with_unsafe_permissions_are_rejected_not_rewritten() {
        use std::os::unix::fs::PermissionsExt;

        let dir = tempfile::tempdir().unwrap();
        let paths = KeyPaths::in_dir(dir.path());
        std::fs::write(&paths.operating, [7_u8; 32]).unwrap();
        std::fs::set_permissions(&paths.operating, std::fs::Permissions::from_mode(0o644)).unwrap();

        assert!(matches!(
            load_or_create(&paths),
            Err(ClientError::Config(_))
        ));
        assert_eq!(
            std::fs::metadata(&paths.operating)
                .unwrap()
                .permissions()
                .mode()
                & 0o777,
            0o644,
            "a path-based chmod would silently bless an attacker-controlled inode"
        );
    }

    #[cfg(unix)]
    #[test]
    fn hard_linked_keys_are_refused() {
        use std::os::unix::fs::PermissionsExt;

        let dir = tempfile::tempdir().unwrap();
        let paths = KeyPaths::in_dir(dir.path());
        std::fs::write(&paths.operating, [7_u8; 32]).unwrap();
        std::fs::set_permissions(&paths.operating, std::fs::Permissions::from_mode(0o600)).unwrap();
        std::fs::hard_link(&paths.operating, dir.path().join("identity.alias")).unwrap();

        assert!(matches!(
            load_or_create(&paths),
            Err(ClientError::Config(_))
        ));
    }

    #[cfg(windows)]
    #[test]
    fn windows_parent_components_are_rejected_before_any_creation() {
        let root = tempfile::tempdir().unwrap();
        let escaped = root.path().join("escaped");
        let attempted = root
            .path()
            .join("would-be-created")
            .join("..")
            .join("escaped");

        assert!(secure_or_create_directory(&attempted).is_err());
        assert!(!root.path().join("would-be-created").exists());
        assert!(!escaped.exists());
    }

    #[cfg(windows)]
    #[test]
    fn windows_reparse_parent_is_rejected_without_touching_its_target() {
        let root = tempfile::tempdir().unwrap();
        let target = root.path().join("outside");
        std::fs::create_dir(&target).unwrap();
        let linked = root.path().join("linked");
        let junction = std::process::Command::new("cmd.exe")
            .args(["/C", "mklink", "/J"])
            .arg(&linked)
            .arg(&target)
            .output()
            .unwrap();
        assert!(
            junction.status.success(),
            "failed to create test junction: {}",
            String::from_utf8_lossy(&junction.stderr)
        );

        assert!(secure_or_create_directory(&linked.join("private")).is_err());
        assert!(!target.join("private").exists());
    }
}
