//! Witnessed, cache-only compliance-key resolution for loft admission and trace rollover.
//!
//! Refreshing the registry is a supervised background operation. `resolve` takes only a read lock
//! over previously verified state: no publish request can trigger registry I/O, follow redirects,
//! or accept a Merkle root returned beside its own proof as a trust anchor.

use std::collections::HashMap;
use std::io::{Read, Write};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, RwLock};
use std::time::{SystemTime, UNIX_EPOCH};

#[cfg(not(unix))]
use std::fs::{self, File, OpenOptions};

use pigeonpost_compliance_format::{
    validate_compliance_epoch, ComplianceKeyId, CompliancePurpose, Jurisdiction,
};
use pigeonpost_core::keys;
use pigeonpost_registry::{
    witness_quorum_intersects, Checkpoint, CheckpointPin as RegistryCheckpointPin,
    ComplianceAuditState, ComplianceKeyPublish, ComplianceKeyStatus, RegistryClient, RegistryTrust,
    WitnessKey,
};
#[cfg(unix)]
use pigeonpost_unix_custody::{
    CustodyError, FilePolicy, GuardedDir, LeafName, NormalizedPath, OpenAccess,
};
use serde::{Deserialize, Serialize};
use tokio::sync::Semaphore;

use crate::attribution::{
    AttributionKeyResolver, AttributionResolutionError, ResolvedAttributionKey,
};
use crate::sealed_trace::{ResolvedTraceKey, TraceKeyResolver};

const MAX_COMPLIANCE_KEYS: usize = 4_096;
const MAX_STATE_BYTES: u64 = 16 * 1024 * 1024;
const WITNESS_FUTURE_SKEW_SECS: u64 = 60;
const PERSISTED_AUDIT_VERSION: u8 = 1;
const REGISTRY_KEY_BLOCKING_LANES: usize = 1;
static TEMP_COUNTER: AtomicU64 = AtomicU64::new(0);

/// An out-of-band checkpoint root below which the cache will never roll back.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CheckpointPin {
    pub size: u64,
    pub root: [u8; 32],
}

/// One independently configured witness key.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WitnessKeyConfig {
    pub name: String,
    pub public_key: [u8; 32],
}

/// Runtime configuration for a witnessed registry cache.
#[derive(Clone)]
pub struct WitnessedRegistryConfig {
    pub registry_url: String,
    pub expected_origin: String,
    pub registry_checkpoint_key: [u8; 32],
    pub witnesses: Vec<WitnessKeyConfig>,
    pub witness_threshold: usize,
    pub minimum_checkpoint: CheckpointPin,
    pub max_staleness_ms: u64,
    pub refresh_interval_ms: u64,
    pub state_path: PathBuf,
}

impl core::fmt::Debug for WitnessedRegistryConfig {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        f.debug_struct("WitnessedRegistryConfig")
            .field("registry_url", &"<withheld>")
            .field("expected_origin", &self.expected_origin)
            .field("registry_checkpoint_key", &self.registry_checkpoint_key)
            .field("witnesses", &self.witnesses)
            .field("witness_threshold", &self.witness_threshold)
            .field("minimum_checkpoint", &self.minimum_checkpoint)
            .field("max_staleness_ms", &self.max_staleness_ms)
            .field("refresh_interval_ms", &self.refresh_interval_ms)
            .field("state_path", &"<withheld>")
            .finish()
    }
}

/// Durable full-log audit plus the local time at which it was accepted.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct PersistedRegistryAudit {
    version: u8,
    audit: ComplianceAuditState,
    observed_at_ms: u64,
}

#[derive(Debug, Clone)]
struct CachedKey {
    publication: ComplianceKeyPublish,
    public_key: [u8; 32],
    log_index: u64,
}

#[derive(Debug, Default)]
struct CacheState {
    audit: Option<Arc<ComplianceAuditState>>,
    checkpoint: Option<Checkpoint>,
    observed_at_ms: u64,
    witnessed_at_secs: u64,
    keys: HashMap<ComplianceKeyId, CachedKey>,
}

struct PreparedRegistryAudit {
    audit: ComplianceAuditState,
    checkpoint: Checkpoint,
    observed_at_ms: u64,
    witnessed_at_secs: u64,
    keys: HashMap<ComplianceKeyId, CachedKey>,
}

#[cfg(unix)]
#[derive(Clone, Debug)]
struct RegistryStateCustody {
    directory: GuardedDir,
    name: LeafName,
}

#[cfg(unix)]
impl RegistryStateCustody {
    fn new(path: &Path) -> Result<Self, AttributionResolutionError> {
        let normalized =
            NormalizedPath::new(path).map_err(|_| AttributionResolutionError::Unavailable)?;
        let parent = normalized
            .as_path()
            .parent()
            .ok_or(AttributionResolutionError::Unavailable)?;
        let name = normalized
            .as_path()
            .file_name()
            .ok_or(AttributionResolutionError::Unavailable)
            .and_then(|name| {
                LeafName::new(name).map_err(|_| AttributionResolutionError::Unavailable)
            })?;
        let directory = GuardedDir::create_private(parent)
            .map_err(|_| AttributionResolutionError::Unavailable)?;
        Ok(Self { directory, name })
    }

    fn path(&self) -> PathBuf {
        self.directory.absolute_path().join(self.name.as_os_str())
    }
}

/// Registry-backed cache shared by attribution admission and the daily trace writer.
pub struct WitnessedRegistryKeyCache {
    config: WitnessedRegistryConfig,
    #[cfg(unix)]
    state_custody: RegistryStateCustody,
    trust: RegistryTrust,
    client: RegistryClient,
    state: RwLock<CacheState>,
    refresh_lock: tokio::sync::Mutex<()>,
    blocking: Arc<Semaphore>,
}

impl core::fmt::Debug for WitnessedRegistryKeyCache {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        let state = self.state.read().unwrap_or_else(|error| error.into_inner());
        f.debug_struct("WitnessedRegistryKeyCache")
            .field("origin", &self.config.expected_origin)
            .field(
                "checkpoint_size",
                &state.checkpoint.as_ref().map(|head| head.size),
            )
            .field("key_count", &state.keys.len())
            .field("registry_url", &"<withheld>")
            .finish()
    }
}

impl WitnessedRegistryKeyCache {
    /// Construct a cache from out-of-band trust pins and load any durable verified snapshot.
    pub fn new(config: WitnessedRegistryConfig) -> Result<Arc<Self>, AttributionResolutionError> {
        #[cfg(unix)]
        let mut config = config;
        validate_config(&config)?;
        require_supported_persistent_cache_platform()?;
        let witnesses = config
            .witnesses
            .iter()
            .map(|witness| {
                let key = keys::verifying_key_from_bytes(&witness.public_key)
                    .map_err(|_| AttributionResolutionError::Unavailable)?;
                WitnessKey::new(witness.name.clone(), key)
                    .map_err(|_| AttributionResolutionError::Unavailable)
            })
            .collect::<Result<Vec<_>, _>>()?;
        let max_age_secs = config.max_staleness_ms / 1_000;
        let trust = RegistryTrust::new(
            config.expected_origin.clone(),
            config.registry_checkpoint_key,
            witnesses,
            config.witness_threshold,
            RegistryCheckpointPin {
                size: config.minimum_checkpoint.size,
                root: config.minimum_checkpoint.root,
            },
            max_age_secs,
            WITNESS_FUTURE_SKEW_SECS.min(max_age_secs),
        )
        .map_err(|_| AttributionResolutionError::Unavailable)?;
        let client = RegistryClient::new(&config.registry_url, trust.clone())
            .map_err(|_| AttributionResolutionError::Unavailable)?;
        #[cfg(unix)]
        let state_custody = {
            let custody = RegistryStateCustody::new(&config.state_path)?;
            config.state_path = custody.path();
            custody
        };
        let cache = Arc::new(Self {
            config,
            #[cfg(unix)]
            state_custody,
            trust,
            client,
            state: RwLock::new(CacheState::default()),
            refresh_lock: tokio::sync::Mutex::new(()),
            blocking: Arc::new(Semaphore::new(REGISTRY_KEY_BLOCKING_LANES)),
        });
        cache.load_persisted()?;
        Ok(cache)
    }

    /// Fetch a fresh witnessed head and audit every unseen log leaf before updating the cache.
    pub async fn refresh_once(&self) -> Result<(), AttributionResolutionError> {
        let _guard = self.refresh_lock.lock().await;
        let previous = self
            .state
            .read()
            .unwrap_or_else(|error| error.into_inner())
            .audit
            .clone();
        let accepted = self
            .state
            .read()
            .unwrap_or_else(|error| error.into_inner())
            .checkpoint
            .clone();
        let observed_at_ms = now_ms();
        let (_, audit) = self
            .client
            .compliance_keys_audited(previous, accepted.as_ref(), observed_at_ms / 1_000)
            .await
            .map_err(|_| AttributionResolutionError::Unavailable)?;
        #[cfg(unix)]
        let state_custody = self.state_custody.clone();
        #[cfg(not(unix))]
        let state_path = self.config.state_path.clone();
        // Projection, bounded serialization, and both fsyncs are synchronous. Keep all of them in
        // one non-queueing blocking job so a canceled refresh cannot release its capacity while a
        // detached write is still running. The live cache is installed only after that job has
        // durably replaced the snapshot.
        let prepared = run_registry_key_blocking(Arc::clone(&self.blocking), move || {
            let keys = cached_audit_keys(&audit)?;
            let checkpoint = audit.checkpoint();
            let witnessed_at_secs = audit.witnessed_at();
            let persisted = PersistedRegistryAudit {
                version: PERSISTED_AUDIT_VERSION,
                audit: audit.clone(),
                observed_at_ms,
            };
            #[cfg(unix)]
            persist_registry_audit(&state_custody, &persisted)?;
            #[cfg(not(unix))]
            persist_registry_audit(&state_path, &persisted)?;
            Ok(PreparedRegistryAudit {
                audit,
                checkpoint,
                observed_at_ms,
                witnessed_at_secs,
                keys,
            })
        })
        .await?;
        let mut state = self
            .state
            .write()
            .unwrap_or_else(|error| error.into_inner());
        state.checkpoint = Some(prepared.checkpoint);
        state.observed_at_ms = prepared.observed_at_ms;
        state.witnessed_at_secs = prepared.witnessed_at_secs;
        state.keys = prepared.keys;
        state.audit = Some(Arc::new(prepared.audit));
        Ok(())
    }

    pub fn checkpoint(&self) -> Option<CheckpointPin> {
        self.state
            .read()
            .unwrap_or_else(|error| error.into_inner())
            .checkpoint
            .as_ref()
            .map(|head| CheckpointPin {
                size: head.size,
                root: head.root,
            })
    }

    fn load_persisted(&self) -> Result<(), AttributionResolutionError> {
        #[cfg(unix)]
        let Some(bytes) = load_registry_audit(&self.state_custody)?
        else {
            return Ok(());
        };
        #[cfg(not(unix))]
        let Some(bytes) = load_registry_audit(&self.config.state_path)?
        else {
            return Ok(());
        };
        let persisted: PersistedRegistryAudit =
            serde_json::from_slice(&bytes).map_err(|_| AttributionResolutionError::Unavailable)?;
        let current_ms = now_ms();
        if persisted.version != PERSISTED_AUDIT_VERSION
            || persisted.observed_at_ms == 0
            || persisted.observed_at_ms > current_ms.saturating_add(5 * 60 * 1_000)
        {
            return Err(AttributionResolutionError::Unavailable);
        }
        persisted
            .audit
            .verify_witnesses(&self.trust, current_ms / 1_000)
            .map_err(|_| AttributionResolutionError::Unavailable)?;
        let keys = cached_audit_keys(&persisted.audit)?;
        let mut state = self
            .state
            .write()
            .unwrap_or_else(|error| error.into_inner());
        state.checkpoint = Some(persisted.audit.checkpoint());
        state.observed_at_ms = persisted.observed_at_ms;
        state.witnessed_at_secs = persisted.audit.witnessed_at();
        state.keys = keys;
        state.audit = Some(Arc::new(persisted.audit));
        Ok(())
    }

    fn resolve_cached(
        &self,
        key_id: &ComplianceKeyId,
    ) -> Result<Option<CachedKey>, AttributionResolutionError> {
        self.ensure_fresh(now_ms())?;
        let state = self.state.read().unwrap_or_else(|error| error.into_inner());
        Ok(state
            .keys
            .get(key_id)
            .cloned()
            .filter(|key| key.publication.status != ComplianceKeyStatus::Revoked))
    }

    fn ensure_fresh(&self, at_ms: u64) -> Result<(), AttributionResolutionError> {
        let state = self.state.read().unwrap_or_else(|error| error.into_inner());
        let now_secs = at_ms / 1_000;
        let max_age_secs = self.config.max_staleness_ms / 1_000;
        let future_skew_secs = WITNESS_FUTURE_SKEW_SECS.min(max_age_secs);
        if state.audit.is_none()
            || state.checkpoint.is_none()
            || state.observed_at_ms == 0
            || state.witnessed_at_secs == 0
            || state.witnessed_at_secs > now_secs.saturating_add(future_skew_secs)
            || now_secs > state.witnessed_at_secs.saturating_add(max_age_secs)
        {
            return Err(AttributionResolutionError::Unavailable);
        }
        Ok(())
    }
}

#[cfg(unix)]
fn load_registry_audit(
    custody: &RegistryStateCustody,
) -> Result<Option<Vec<u8>>, AttributionResolutionError> {
    let Some(mut file) = custody
        .directory
        .open_file_optional(
            &custody.name,
            OpenAccess::ReadOnly,
            FilePolicy::private(MAX_STATE_BYTES),
        )
        .map_err(|_| AttributionResolutionError::Unavailable)?
    else {
        return Ok(None);
    };
    let opened = file.metadata().map_err(map_custody_error)?;
    let mut bytes = Vec::with_capacity(usize::try_from(opened.len).unwrap_or(0));
    (&mut file)
        .take(MAX_STATE_BYTES.saturating_add(1))
        .read_to_end(&mut bytes)
        .map_err(|_| AttributionResolutionError::Unavailable)?;
    let final_metadata = file.metadata().map_err(map_custody_error)?;
    if bytes.len() as u64 > MAX_STATE_BYTES
        || bytes.len() as u64 != opened.len
        || final_metadata != opened
    {
        return Err(AttributionResolutionError::Unavailable);
    }
    file.verify_named().map_err(map_custody_error)?;
    Ok(Some(bytes))
}

#[cfg(not(unix))]
fn load_registry_audit(path: &Path) -> Result<Option<Vec<u8>>, AttributionResolutionError> {
    let metadata = match fs::symlink_metadata(path) {
        Ok(metadata) => metadata,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(_) => return Err(AttributionResolutionError::Unavailable),
    };
    if metadata.file_type().is_symlink() || !metadata.is_file() || metadata.len() > MAX_STATE_BYTES
    {
        return Err(AttributionResolutionError::Unavailable);
    }
    let file = File::open(path).map_err(|_| AttributionResolutionError::Unavailable)?;
    let opened = file
        .metadata()
        .map_err(|_| AttributionResolutionError::Unavailable)?;
    if !opened.is_file() || opened.len() > MAX_STATE_BYTES {
        return Err(AttributionResolutionError::Unavailable);
    }
    let mut bytes = Vec::new();
    file.take(MAX_STATE_BYTES.saturating_add(1))
        .read_to_end(&mut bytes)
        .map_err(|_| AttributionResolutionError::Unavailable)?;
    if bytes.len() as u64 > MAX_STATE_BYTES || bytes.len() as u64 != opened.len() {
        return Err(AttributionResolutionError::Unavailable);
    }
    Ok(Some(bytes))
}

async fn run_registry_key_blocking<T, F>(
    blocking: Arc<Semaphore>,
    operation: F,
) -> Result<T, AttributionResolutionError>
where
    T: Send + 'static,
    F: FnOnce() -> Result<T, AttributionResolutionError> + Send + 'static,
{
    let permit = blocking
        .try_acquire_owned()
        .map_err(|_| AttributionResolutionError::Unavailable)?;
    tokio::task::spawn_blocking(move || {
        let _permit = permit;
        operation()
    })
    .await
    .map_err(|_| AttributionResolutionError::Unavailable)?
}

#[cfg(unix)]
fn persist_registry_audit(
    custody: &RegistryStateCustody,
    snapshot: &PersistedRegistryAudit,
) -> Result<(), AttributionResolutionError> {
    let encoded =
        serde_json::to_vec(snapshot).map_err(|_| AttributionResolutionError::Unavailable)?;
    if encoded.len() as u64 > MAX_STATE_BYTES {
        return Err(AttributionResolutionError::Unavailable);
    }
    custody
        .directory
        .verify_named()
        .map_err(map_custody_error)?;
    let (temp_name, mut temporary) = create_registry_temp(custody, encoded.len() as u64)?;
    let cleanup = match custody.directory.open_file(
        &temp_name,
        OpenAccess::ReadOnly,
        FilePolicy::private(encoded.len() as u64),
    ) {
        Ok(cleanup) => cleanup,
        Err(error) => {
            let _ = custody.directory.unlink_file(temporary);
            return Err(map_custody_error(error));
        }
    };
    let publication = (|| {
        temporary
            .write_all(&encoded)
            .map_err(|_| AttributionResolutionError::Unavailable)?;
        temporary.sync_all().map_err(map_custody_error)?;
        if temporary.metadata().map_err(map_custody_error)?.len != encoded.len() as u64 {
            return Err(AttributionResolutionError::Unavailable);
        }
        temporary.verify_named().map_err(map_custody_error)?;
        let existing = custody
            .directory
            .open_file_optional(
                &custody.name,
                OpenAccess::ReadOnly,
                FilePolicy::private(MAX_STATE_BYTES),
            )
            .map_err(map_custody_error)?;
        if let Some(existing) = existing.as_ref() {
            existing.verify_named().map_err(map_custody_error)?;
        }
        let published = match existing {
            Some(_) => {
                custody
                    .directory
                    .rename_replace(temporary, &custody.directory, &custody.name)
            }
            None => {
                custody
                    .directory
                    .publish_no_replace(temporary, &custody.directory, &custody.name)
            }
        }
        .map_err(map_custody_error)?;
        if published.metadata().map_err(map_custody_error)?.len != encoded.len() as u64 {
            return Err(AttributionResolutionError::Unavailable);
        }
        published.verify_named().map_err(map_custody_error)?;
        let reopened = custody
            .directory
            .open_file(
                &custody.name,
                OpenAccess::ReadOnly,
                FilePolicy::private_exact(encoded.len() as u64),
            )
            .map_err(map_custody_error)?;
        if reopened.identity() != published.identity() {
            return Err(AttributionResolutionError::Unavailable);
        }
        reopened.verify_named().map_err(map_custody_error)
    })();
    if publication.is_err() {
        let _ = custody.directory.unlink_file(cleanup);
    }
    publication
}

#[cfg(unix)]
fn create_registry_temp(
    custody: &RegistryStateCustody,
    max_len: u64,
) -> Result<(LeafName, pigeonpost_unix_custody::GuardedFile), AttributionResolutionError> {
    for _ in 0..16 {
        let name = LeafName::new(format!(
            ".registry-keys.{}.{}.tmp",
            std::process::id(),
            TEMP_COUNTER.fetch_add(1, Ordering::Relaxed)
        ))
        .map_err(map_custody_error)?;
        match custody
            .directory
            .create_file(&name, FilePolicy::private(max_len))
        {
            Ok(file) => return Ok((name, file)),
            Err(CustodyError::AlreadyExists) => continue,
            Err(error) => return Err(map_custody_error(error)),
        }
    }
    Err(AttributionResolutionError::Unavailable)
}

#[cfg(unix)]
fn map_custody_error(_error: CustodyError) -> AttributionResolutionError {
    AttributionResolutionError::Unavailable
}

#[cfg(not(unix))]
fn persist_registry_audit(
    state_path: &Path,
    snapshot: &PersistedRegistryAudit,
) -> Result<(), AttributionResolutionError> {
    let encoded =
        serde_json::to_vec(snapshot).map_err(|_| AttributionResolutionError::Unavailable)?;
    if encoded.len() as u64 > MAX_STATE_BYTES {
        return Err(AttributionResolutionError::Unavailable);
    }
    let parent = state_path
        .parent()
        .ok_or(AttributionResolutionError::Unavailable)?;
    fs::create_dir_all(parent).map_err(|_| AttributionResolutionError::Unavailable)?;
    let temp = parent.join(format!(
        ".registry-keys.{}.{}.tmp",
        std::process::id(),
        TEMP_COUNTER.fetch_add(1, Ordering::Relaxed)
    ));
    let mut file = OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(&temp)
        .map_err(|_| AttributionResolutionError::Unavailable)?;
    let result = (|| {
        file.write_all(&encoded)?;
        file.sync_all()?;
        replace_state_file(&temp, state_path)?;
        sync_parent(parent)
    })();
    if result.is_err() {
        let _ = fs::remove_file(&temp);
        return Err(AttributionResolutionError::Unavailable);
    }
    Ok(())
}

impl AttributionKeyResolver for WitnessedRegistryKeyCache {
    fn resolve(
        &self,
        key_id: &ComplianceKeyId,
    ) -> Result<Option<ResolvedAttributionKey>, AttributionResolutionError> {
        if key_id.purpose != CompliancePurpose::Attribution {
            return Ok(None);
        }
        Ok(self
            .resolve_cached(key_id)?
            .map(|key| ResolvedAttributionKey {
                public_key: key.public_key,
                not_before_ms: key.publication.not_before_ms,
                not_after_ms: key.publication.not_after_ms,
                status: key.publication.status,
            }))
    }

    fn readiness(&self, now_ms: u64) -> Result<(), AttributionResolutionError> {
        self.ensure_fresh(now_ms)
    }

    fn refresh_interval_ms(&self) -> Option<u64> {
        Some(self.config.refresh_interval_ms)
    }

    fn refresh(
        &self,
    ) -> std::pin::Pin<
        Box<dyn std::future::Future<Output = Result<(), AttributionResolutionError>> + Send + '_>,
    > {
        Box::pin(self.refresh_once())
    }
}

impl TraceKeyResolver for WitnessedRegistryKeyCache {
    fn readiness(&self, now_ms: u64) -> Result<(), AttributionResolutionError> {
        self.ensure_fresh(now_ms)
    }

    fn resolve_trace_key(
        &self,
        purpose: CompliancePurpose,
        jurisdiction: Jurisdiction,
        at_ms: u64,
    ) -> Result<Option<ResolvedTraceKey>, AttributionResolutionError> {
        if !matches!(
            purpose,
            CompliancePurpose::NetworkTrace | CompliancePurpose::IdentityTrace
        ) {
            return Ok(None);
        }
        self.ensure_fresh(now_ms())?;
        let state = self.state.read().unwrap_or_else(|error| error.into_inner());
        Ok(state
            .keys
            .values()
            .filter(|key| {
                key.publication.status == ComplianceKeyStatus::Active
                    && key.publication.key_id.purpose == purpose
                    && key.publication.key_id.jurisdiction == jurisdiction
                    && key.publication.not_before_ms <= at_ms
                    && at_ms < key.publication.not_after_ms
            })
            .max_by_key(|key| {
                (
                    key.publication.key_id.epoch_start_ms,
                    key.publication.key_id.generation,
                    key.log_index,
                )
            })
            .map(|key| ResolvedTraceKey {
                key_id: key.publication.key_id,
                public_key: key.public_key,
                not_before_ms: key.publication.not_before_ms,
                not_after_ms: key.publication.not_after_ms,
            }))
    }
}

fn cached_audit_keys(
    audit: &ComplianceAuditState,
) -> Result<HashMap<ComplianceKeyId, CachedKey>, AttributionResolutionError> {
    if audit.keys().len() > MAX_COMPLIANCE_KEYS {
        return Err(AttributionResolutionError::Unavailable);
    }
    let mut keys = HashMap::with_capacity(audit.keys().len());
    for key in audit.keys() {
        if validate_compliance_epoch(
            &key.publication().key_id,
            key.publication().not_before_ms,
            key.publication().not_after_ms,
        )
        .is_err()
        {
            return Err(AttributionResolutionError::Unavailable);
        }
        let cached = CachedKey {
            publication: key.publication().clone(),
            public_key: *key.public_key(),
            log_index: key.log_index(),
        };
        if keys.insert(cached.publication.key_id, cached).is_some() {
            return Err(AttributionResolutionError::Unavailable);
        }
    }
    Ok(keys)
}

fn validate_config(config: &WitnessedRegistryConfig) -> Result<(), AttributionResolutionError> {
    if config.expected_origin.is_empty()
        || config.expected_origin.len() > 256
        || config
            .expected_origin
            .bytes()
            .any(|byte| byte.is_ascii_control())
        || config.registry_checkpoint_key == [0u8; 32]
        || config.witnesses.is_empty()
        || !witness_quorum_intersects(config.witness_threshold, config.witnesses.len())
        || config.max_staleness_ms < 1_000
        || config.refresh_interval_ms == 0
        || config.refresh_interval_ms >= config.max_staleness_ms
        || config.state_path.as_os_str().is_empty()
    {
        return Err(AttributionResolutionError::Unavailable);
    }
    Ok(())
}

fn require_supported_persistent_cache_platform() -> Result<(), AttributionResolutionError> {
    #[cfg(any(target_os = "linux", target_os = "macos"))]
    {
        Ok(())
    }
    #[cfg(not(any(target_os = "linux", target_os = "macos")))]
    {
        Err(AttributionResolutionError::Unavailable)
    }
}

fn now_ms() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|duration| u64::try_from(duration.as_millis()).unwrap_or(u64::MAX))
        .unwrap_or(0)
}

#[cfg(not(unix))]
fn sync_parent(path: &Path) -> std::io::Result<()> {
    let _ = path;
    Ok(())
}

#[cfg(all(not(unix), not(windows)))]
fn replace_state_file(source: &Path, destination: &Path) -> std::io::Result<()> {
    fs::rename(source, destination)
}

#[cfg(windows)]
fn replace_state_file(source: &Path, destination: &Path) -> std::io::Result<()> {
    // std::fs::rename cannot replace an existing file on Windows. This cache is
    // reconstructible from the witnessed log, so a crash between these calls
    // may only remove the cache; startup then fails closed until it is refreshed.
    match fs::remove_file(destination) {
        Ok(()) => {}
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
        Err(error) => return Err(error),
    }
    fs::rename(source, destination)
}

#[cfg(test)]
mod tests {
    #[cfg(unix)]
    use std::fs;
    #[cfg(unix)]
    use std::os::unix::fs::{symlink, OpenOptionsExt, PermissionsExt};
    #[cfg(unix)]
    use std::process::Command;
    use std::sync::atomic::{AtomicBool, Ordering as AtomicOrdering};
    use std::time::Duration;

    use ed25519_dalek::SigningKey;

    use super::*;

    fn configuration(count: usize, threshold: usize) -> WitnessedRegistryConfig {
        WitnessedRegistryConfig {
            registry_url: "https://registry.example".into(),
            expected_origin: "registry.example/log".into(),
            registry_checkpoint_key: SigningKey::from_bytes(&[1; 32]).verifying_key().to_bytes(),
            witnesses: (0..count)
                .map(|index| WitnessKeyConfig {
                    name: format!("witness-{index}"),
                    public_key: SigningKey::from_bytes(&[u8::try_from(index + 2).unwrap(); 32])
                        .verifying_key()
                        .to_bytes(),
                })
                .collect(),
            witness_threshold: threshold,
            minimum_checkpoint: CheckpointPin {
                size: 0,
                root: pigeonpost_registry::log::empty_root(),
            },
            max_staleness_ms: 60_000,
            refresh_interval_ms: 5_000,
            state_path: PathBuf::from("registry-state.json"),
        }
    }

    #[cfg(not(any(target_os = "linux", target_os = "macos")))]
    #[test]
    fn cache_rejects_unsupported_platform_before_state_path_side_effects() {
        let root = tempfile::tempdir().unwrap();
        let requested_parent = root.path().join("unsupported-cache");
        let mut config = configuration(1, 1);
        config.state_path = requested_parent.join("registry-state.json");

        assert!(matches!(
            WitnessedRegistryKeyCache::new(config),
            Err(AttributionResolutionError::Unavailable)
        ));
        assert!(!requested_parent.exists());
    }

    #[cfg(unix)]
    fn private_directory(path: &Path) {
        fs::create_dir(path).unwrap();
        fs::set_permissions(path, fs::Permissions::from_mode(0o700)).unwrap();
    }

    #[cfg(unix)]
    fn test_snapshot(observed_at_ms: u64) -> PersistedRegistryAudit {
        let audit = serde_json::from_value(serde_json::json!({
            "origin": "registry.example/log",
            "size": 0,
            "root": pigeonpost_registry::log::empty_root(),
            "checkpoint_note": "",
            "witnessed_at": 1,
            "frontier": { "size": 0, "peaks": [] },
            "keys": []
        }))
        .unwrap();
        PersistedRegistryAudit {
            version: PERSISTED_AUDIT_VERSION,
            audit,
            observed_at_ms,
        }
    }

    #[test]
    fn loft_cache_requires_a_strictly_intersecting_quorum() {
        assert!(validate_config(&configuration(1, 1)).is_ok());
        assert!(validate_config(&configuration(3, 2)).is_ok());
        assert!(validate_config(&configuration(2, 1)).is_err());
        assert!(validate_config(&configuration(3, 1)).is_err());
    }

    #[tokio::test(flavor = "current_thread")]
    async fn stalled_refresh_processing_is_nonqueueing_and_keeps_its_permit_after_cancellation() {
        let blocking = Arc::new(Semaphore::new(REGISTRY_KEY_BLOCKING_LANES));
        let started = Arc::new(AtomicBool::new(false));
        let release = Arc::new(AtomicBool::new(false));
        let task = tokio::spawn({
            let blocking = Arc::clone(&blocking);
            let started = Arc::clone(&started);
            let release = Arc::clone(&release);
            async move {
                run_registry_key_blocking(blocking, move || {
                    // A deterministic stand-in for a filesystem whose data or directory fsync is
                    // stalled. The permit must belong to this closure, not its cancelable caller.
                    started.store(true, AtomicOrdering::Release);
                    while !release.load(AtomicOrdering::Acquire) {
                        std::thread::sleep(Duration::from_millis(1));
                    }
                    Ok(())
                })
                .await
            }
        });

        let entered = tokio::time::timeout(Duration::from_secs(10), async {
            while !started.load(AtomicOrdering::Acquire) {
                tokio::task::yield_now().await;
            }
        })
        .await
        .is_ok();

        task.abort();
        let _ = task.await;
        let permit_retained = blocking.available_permits() == 0;
        let saturated = tokio::time::timeout(
            Duration::from_secs(10),
            run_registry_key_blocking(Arc::clone(&blocking), || Ok(())),
        )
        .await;

        // Release before asserting so a regression cannot strand a blocking runtime worker.
        release.store(true, AtomicOrdering::Release);
        let recovered = tokio::time::timeout(Duration::from_secs(10), async {
            while blocking.available_permits() != REGISTRY_KEY_BLOCKING_LANES {
                tokio::task::yield_now().await;
            }
        })
        .await
        .is_ok();

        assert!(
            entered,
            "refresh processing did not enter the blocking lane"
        );
        assert!(
            permit_retained,
            "canceling the refresh released capacity before its blocking job ended"
        );
        assert!(matches!(
            saturated,
            Ok(Err(AttributionResolutionError::Unavailable))
        ));
        assert!(recovered, "the refresh-processing permit was not recovered");
        run_registry_key_blocking(blocking, || Ok(()))
            .await
            .unwrap();
    }

    #[cfg(unix)]
    #[test]
    fn cache_publication_is_durable_bounded_and_recovers_from_temp_collision() {
        let root = tempfile::tempdir().unwrap();
        let directory = root.path().join("cache");
        private_directory(&directory);
        let custody = RegistryStateCustody::new(&directory.join("registry-state.json")).unwrap();

        persist_registry_audit(&custody, &test_snapshot(11)).unwrap();
        let first = load_registry_audit(&custody).unwrap().unwrap();
        assert_eq!(
            serde_json::from_slice::<PersistedRegistryAudit>(&first)
                .unwrap()
                .observed_at_ms,
            11
        );

        let colliding_counter = TEMP_COUNTER.load(Ordering::Relaxed);
        let collision = directory.join(format!(
            ".registry-keys.{}.{}.tmp",
            std::process::id(),
            colliding_counter
        ));
        let mut collision_options = std::fs::OpenOptions::new();
        collision_options.write(true).create_new(true).mode(0o600);
        collision_options
            .open(&collision)
            .unwrap()
            .write_all(b"collision")
            .unwrap();

        persist_registry_audit(&custody, &test_snapshot(12)).unwrap();
        assert_eq!(fs::read(&collision).unwrap(), b"collision");
        let second = load_registry_audit(&custody).unwrap().unwrap();
        assert_eq!(
            serde_json::from_slice::<PersistedRegistryAudit>(&second)
                .unwrap()
                .observed_at_ms,
            12
        );
        let unexpected_temps = fs::read_dir(&directory)
            .unwrap()
            .filter_map(Result::ok)
            .filter(|entry| {
                entry
                    .file_name()
                    .to_string_lossy()
                    .starts_with(".registry-keys.")
                    && entry.path() != collision
            })
            .count();
        assert_eq!(unexpected_temps, 0);
    }

    #[cfg(unix)]
    #[test]
    fn cache_rejects_intermediate_symlinks_mutable_ancestors_hardlinks_and_fifos() {
        let root = tempfile::tempdir().unwrap();

        let actual = root.path().join("actual");
        private_directory(&actual);
        let alias = root.path().join("alias");
        symlink(&actual, &alias).unwrap();
        assert!(RegistryStateCustody::new(&alias.join("registry-state.json")).is_err());
        assert!(!actual.join("registry-state.json").exists());

        let mutable = root.path().join("mutable");
        private_directory(&mutable);
        fs::set_permissions(&mutable, fs::Permissions::from_mode(0o770)).unwrap();
        let nested = mutable.join("nested");
        assert!(RegistryStateCustody::new(&nested.join("registry-state.json")).is_err());
        assert!(!nested.exists());

        let cache = root.path().join("cache");
        private_directory(&cache);
        let state_path = cache.join("registry-state.json");
        let mut options = std::fs::OpenOptions::new();
        options.write(true).create_new(true).mode(0o600);
        options.open(&state_path).unwrap().write_all(b"{}").unwrap();
        let hardlink = cache.join("state-alias");
        fs::hard_link(&state_path, &hardlink).unwrap();
        let custody = RegistryStateCustody::new(&state_path).unwrap();
        assert!(load_registry_audit(&custody).is_err());
        fs::remove_file(&hardlink).unwrap();
        fs::remove_file(&state_path).unwrap();

        assert!(Command::new("mkfifo")
            .arg(&state_path)
            .status()
            .unwrap()
            .success());
        assert!(load_registry_audit(&custody).is_err());
    }

    #[cfg(unix)]
    #[test]
    fn cache_parent_replacement_fails_before_publication_without_side_effects() {
        let root = tempfile::tempdir().unwrap();
        let directory = root.path().join("cache");
        private_directory(&directory);
        let custody = RegistryStateCustody::new(&directory.join("registry-state.json")).unwrap();
        let moved = root.path().join("cache-retained");
        fs::rename(&directory, &moved).unwrap();
        private_directory(&directory);

        assert!(persist_registry_audit(&custody, &test_snapshot(20)).is_err());
        assert!(!directory.join("registry-state.json").exists());
        assert!(!moved.join("registry-state.json").exists());
        assert_eq!(fs::read_dir(&directory).unwrap().count(), 0);
        assert_eq!(fs::read_dir(&moved).unwrap().count(), 0);
    }
}
