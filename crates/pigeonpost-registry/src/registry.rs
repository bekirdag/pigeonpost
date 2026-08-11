//! Transactional registry service over the persisted transparency tree.

use std::collections::{HashMap, VecDeque};
use std::net::SocketAddr;
#[cfg(any(target_os = "linux", target_os = "macos"))]
use std::path::Path;
#[cfg(any(target_os = "linux", target_os = "macos"))]
use std::path::PathBuf;
use std::sync::Arc;
use std::sync::Mutex;
use std::time::{Duration, Instant};

use ed25519_dalek::SigningKey;
use pigeonpost_compliance_format::{ComplianceKeyId, CompliancePurpose, Jurisdiction};
use pigeonpost_compliance_seal::{
    required_identity_trace_storage_bytes, required_network_trace_storage_bytes,
    IdentityProvider as TraceIdentityProvider, MAX_EPOCH_MANIFEST_SEGMENTS, MAX_SEGMENT_RECORDS,
    MAX_TRACE_STORAGE_BYTES, TRACE_RATE_WINDOWS_PER_UTC_DAY,
};
use pigeonpost_core::keys;
#[cfg(any(target_os = "linux", target_os = "macos"))]
use pigeonpost_unix_custody::{
    CustodyError, DirPolicy, FilePolicy, GuardedDir, GuardedFile, LeafName, NormalizedPath,
    OpenAccess,
};
use rand_core::{OsRng, RngCore};
#[cfg(any(target_os = "linux", target_os = "macos"))]
use rusqlite::OpenFlags;
use rusqlite::{Connection, TransactionBehavior};
use sha2::{Digest, Sha256};

use crate::checkpoint::Checkpoint;
use crate::claim_trace::{
    ClaimTraceCapacity, ClaimTraceInput, ClaimTraceSink, SealedClaimTraceSink,
    UnconfiguredClaimTraceSink,
};
use crate::entry::{claim_payload, ComplianceKeyPublish, DirectoryAdd, DirectoryRemove, LogEntry};
use crate::error::{RegistryError, Result};
use crate::handle::Handle;
use crate::identity::{
    challenge_hash, validate_challenge_token, validate_pkce_challenge, IdentityProvider,
    ProofPayload,
};
use crate::log::Hash;
use crate::storage::{self, LegacyAuthorization};
use crate::witness::{WitnessPolicy, WitnessReceipt};
use crate::{GITHUB_AUTHORIZATION_ENDPOINT, GOOGLE_AUTHORIZATION_ENDPOINT};

const IDENTITY_CHALLENGE_TTL_MS: u64 = 5 * 60 * 1_000;
const ACCOUNT_RATE_WINDOW: Duration = Duration::from_secs(60);
const MAX_ACCOUNT_EXPIRATIONS_PER_CHARGE: usize = 256;
/// Largest per-minute verified-binding ceiling representable by one canonical UTC-day terminal
/// manifest at the maximum segment size. Shorter configured segments impose a lower startup bound.
pub const MAX_REGISTRATION_BINDINGS_PER_MINUTE: u32 = ((MAX_EPOCH_MANIFEST_SEGMENTS as u64)
    * (MAX_SEGMENT_RECORDS as u64)
    / TRACE_RATE_WINDOWS_PER_UTC_DAY) as u32;
#[cfg(any(target_os = "linux", target_os = "macos"))]
const MAX_SQLITE_FILE_BYTES: u64 = 1 << 40;

/// Bounded registration budget keyed to the stable account returned by a verified provider.
///
/// Keys are domain-separated SHA-256 digests retained in memory for at most one rate window. Raw
/// provider subjects are never stored in the limiter or emitted when it rejects a request.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct RegistrationLimits {
    /// Exact global ceiling on verified claims that may reach durable trace submission.
    pub global_bindings_per_minute: u32,
    pub account_bindings_per_minute: u32,
    pub max_account_keys: usize,
}

impl Default for RegistrationLimits {
    fn default() -> Self {
        Self {
            // Preserve the pre-contract ceiling imposed by the default HTTP global request limit.
            global_bindings_per_minute: 6_000,
            account_bindings_per_minute: 10,
            max_account_keys: 4_096,
        }
    }
}

#[derive(Debug, Clone, Copy)]
struct AccountRateBucket {
    requests: u32,
    window_start: Instant,
}

impl AccountRateBucket {
    fn fresh(now: Instant) -> Self {
        Self {
            requests: 0,
            window_start: now,
        }
    }

    fn charge(&mut self, now: Instant, limit: u32) -> Result<()> {
        if now.duration_since(self.window_start) >= ACCOUNT_RATE_WINDOW {
            *self = Self::fresh(now);
        }
        if self.requests >= limit {
            return Err(RegistryError::RateLimited);
        }
        self.requests = self.requests.saturating_add(1);
        Ok(())
    }
}

#[derive(Debug)]
struct AccountLimiter {
    state: Mutex<AccountLimiterState>,
    limits: RegistrationLimits,
}

#[derive(Debug, Default)]
struct AccountLimiterState {
    buckets: HashMap<[u8; 32], AccountRateBucket>,
    expirations: VecDeque<(Instant, [u8; 32])>,
    #[cfg(test)]
    cleanup_passes: usize,
}

impl AccountLimiterState {
    fn schedule(&mut self, key: [u8; 32], window_start: Instant) {
        let expires = window_start
            .checked_add(ACCOUNT_RATE_WINDOW)
            .unwrap_or(window_start);
        self.expirations.push_back((expires, key));
    }

    fn reclaim_expired(&mut self, now: Instant) {
        if self
            .expirations
            .front()
            .is_none_or(|(expires, _)| *expires > now)
        {
            return;
        }
        #[cfg(test)]
        {
            self.cleanup_passes = self.cleanup_passes.saturating_add(1);
        }
        for _ in 0..MAX_ACCOUNT_EXPIRATIONS_PER_CHARGE {
            let Some((expires, key)) = self.expirations.front().copied() else {
                break;
            };
            if expires > now {
                break;
            }
            self.expirations.pop_front();
            let remove = self.buckets.get(&key).is_some_and(|bucket| {
                bucket
                    .window_start
                    .checked_add(ACCOUNT_RATE_WINDOW)
                    .is_none_or(|current_expiry| current_expiry <= now && current_expiry == expires)
            });
            if remove {
                self.buckets.remove(&key);
            }
        }
    }
}

impl AccountLimiter {
    fn new(limits: RegistrationLimits) -> Result<Self> {
        validate_registration_limits(limits)?;
        Ok(Self {
            state: Mutex::new(AccountLimiterState::default()),
            limits,
        })
    }

    fn charge(&self, namespace: &str, opaque_id: &str) -> Result<()> {
        self.charge_at(namespace, opaque_id, Instant::now())
    }

    fn charge_at(&self, namespace: &str, opaque_id: &str, now: Instant) -> Result<()> {
        let mut hasher = Sha256::new();
        hasher.update(b"pigeonpost/provider-account-rate-limit/v1\0");
        hasher.update((namespace.len() as u64).to_be_bytes());
        hasher.update(namespace.as_bytes());
        hasher.update((opaque_id.len() as u64).to_be_bytes());
        hasher.update(opaque_id.as_bytes());
        let key: [u8; 32] = hasher.finalize().into();

        let mut state = self.state.lock().unwrap_or_else(|error| error.into_inner());
        state.reclaim_expired(now);
        if !state.buckets.contains_key(&key) && state.buckets.len() >= self.limits.max_account_keys
        {
            return Err(RegistryError::RateLimited);
        }
        if let Some(bucket) = state.buckets.get_mut(&key) {
            let previous_start = bucket.window_start;
            bucket.charge(now, self.limits.account_bindings_per_minute)?;
            if bucket.window_start != previous_start {
                let window_start = bucket.window_start;
                state.schedule(key, window_start);
            }
            return Ok(());
        }
        let mut bucket = AccountRateBucket::fresh(now);
        bucket.charge(now, self.limits.account_bindings_per_minute)?;
        state.buckets.insert(key, bucket);
        state.schedule(key, now);
        Ok(())
    }

    #[cfg(test)]
    fn state_counts(&self) -> (usize, usize, usize) {
        let state = self.state.lock().unwrap_or_else(|error| error.into_inner());
        (
            state.buckets.len(),
            state.expirations.len(),
            state.cleanup_passes,
        )
    }
}

fn validate_registration_limits(limits: RegistrationLimits) -> Result<()> {
    if limits.global_bindings_per_minute == 0
        || limits.global_bindings_per_minute > MAX_REGISTRATION_BINDINGS_PER_MINUTE
        || limits.account_bindings_per_minute == 0
        || limits.account_bindings_per_minute > MAX_REGISTRATION_BINDINGS_PER_MINUTE
        || limits.max_account_keys == 0
        || limits.max_account_keys > 1_000_000
    {
        return Err(RegistryError::InvalidConfiguration(
            "registry registration limits are outside the supported bounds".into(),
        ));
    }
    Ok(())
}

pub struct RegistryConfig {
    /// Goes in every checkpoint. Witnesses key their cosignatures on it.
    pub origin: String,
    /// Signs checkpoints. Not an agent identity and never used for anything else.
    pub signing_key: SigningKey,
    /// Test-only escape hatch, off by default.
    pub allow_mock_identities: bool,
}

struct ValidatedRegistryConfig(RegistryConfig);

impl ValidatedRegistryConfig {
    fn new(config: RegistryConfig) -> Result<Self> {
        validate_origin(&config.origin)?;
        if config.signing_key.to_bytes() == [0u8; 32] {
            return Err(RegistryError::InvalidConfiguration(
                "checkpoint signing key must not use the all-zero seed".into(),
            ));
        }
        Ok(Self(config))
    }
}

pub struct Registry {
    config: RegistryConfig,
    providers: HashMap<&'static str, Box<dyn IdentityProvider>>,
    conn: Mutex<Connection>,
    // Field declaration order is drop order. The SQLite connection must close before the retained
    // directory and file custody descriptors are released.
    storage_custody: RegistryStorageCustody,
    global_binding_admission: Mutex<storage::GlobalBindingAdmissionBatch>,
    claim_trace: Arc<dyn ClaimTraceSink>,
    witness_policy: Option<WitnessPolicy>,
    account_limiter: AccountLimiter,
    #[cfg(test)]
    commit_test_barrier: Mutex<Option<Arc<CommitTestBarrier>>>,
}

#[cfg(test)]
pub(crate) struct CommitTestBarrier {
    reached: Mutex<Option<tokio::sync::oneshot::Sender<()>>>,
    release: Mutex<std::sync::mpsc::Receiver<()>>,
}

#[cfg(test)]
impl CommitTestBarrier {
    pub(crate) fn new() -> (
        Arc<Self>,
        tokio::sync::oneshot::Receiver<()>,
        std::sync::mpsc::Sender<()>,
    ) {
        let (reached_tx, reached_rx) = tokio::sync::oneshot::channel();
        let (release_tx, release_rx) = std::sync::mpsc::channel();
        (
            Arc::new(Self {
                reached: Mutex::new(Some(reached_tx)),
                release: Mutex::new(release_rx),
            }),
            reached_rx,
            release_tx,
        )
    }

    fn wait(&self) {
        if let Some(reached) = self
            .reached
            .lock()
            .unwrap_or_else(|error| error.into_inner())
            .take()
        {
            let _ = reached.send(());
        }
        let _ = self
            .release
            .lock()
            .unwrap_or_else(|error| error.into_inner())
            .recv();
    }
}

enum RegistryStorageCustody {
    Memory,
    #[cfg(any(target_os = "linux", target_os = "macos"))]
    Persistent(Box<PersistentRegistryStorage>),
}

impl RegistryStorageCustody {
    fn revalidate(&self) -> Result<()> {
        match self {
            Self::Memory => Ok(()),
            #[cfg(any(target_os = "linux", target_os = "macos"))]
            Self::Persistent(custody) => custody.verify_all_named(),
        }
    }

    fn require_public(&self) -> Result<()> {
        match self {
            Self::Memory => Err(RegistryError::InvalidConfiguration(
                "public witnessed serving requires a custody-verified persistent registry database"
                    .into(),
            )),
            #[cfg(any(target_os = "linux", target_os = "macos"))]
            Self::Persistent(custody) => custody.verify_all_named(),
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct WitnessPublicationStatus {
    pub committed_size: u64,
    pub published_size: u64,
    pub lag_entries: u64,
    pub witnessed_at: Option<u64>,
}

/// Inclusion data is always tied to the persisted signed checkpoint returned alongside it.
#[derive(Debug, Clone)]
pub struct ProofBundle {
    pub size: u64,
    pub root: Hash,
    pub path: Vec<Hash>,
    pub checkpoint: String,
}

#[derive(Debug, Clone)]
pub struct Registration {
    pub handle: String,
    pub index: u64,
    pub appended: bool,
    pub inclusion: ProofBundle,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum HandleBindingOperation {
    Register,
    Rotate,
}

impl HandleBindingOperation {
    fn storage_mode(self) -> storage::HandleAppendMode {
        match self {
            Self::Register => storage::HandleAppendMode::Register,
            Self::Rotate => storage::HandleAppendMode::Rotate,
        }
    }
}

pub(crate) struct PreparedHandleBinding {
    handle: Handle,
    pubkey: [u8; 32],
    proof: ProofPayload,
    source: SocketAddr,
    operation: HandleBindingOperation,
    challenge: Option<(Hash, Option<String>)>,
    recovered_seq: Option<u64>,
}

impl PreparedHandleBinding {
    pub(crate) const fn is_recovery(&self) -> bool {
        self.recovered_seq.is_some()
    }
}

pub(crate) struct VerifiedHandleBinding {
    prepared: PreparedHandleBinding,
    subject_name: String,
    subject_id: String,
}

#[derive(Debug, Clone)]
pub struct Resolved {
    pub handle: String,
    pub pubkey: String,
    pub index: u64,
    pub inclusion: ProofBundle,
}

#[derive(Debug, Clone, Default)]
pub struct ComplianceKeyQuery {
    pub key_id: Option<ComplianceKeyId>,
    pub purpose: Option<CompliancePurpose>,
    pub jurisdiction: Option<Jurisdiction>,
    /// Historical point in milliseconds. `None` means now.
    pub at_ms: Option<u64>,
    /// Include retired, revoked, and time-invalid publications.
    pub include_inactive: bool,
    /// Return only the authenticated snapshot head. This is used by clients that need a fresh
    /// witnessed checkpoint without paying to materialize every matching key record.
    pub metadata_only: bool,
}

#[derive(Debug, Clone)]
pub struct LoggedComplianceKey {
    pub publication: ComplianceKeyPublish,
    /// The exact immutable log leaf. Keeping this beside the projection avoids one client HTTP
    /// request per key while preserving byte-for-byte inclusion verification.
    pub entry: LogEntry,
    pub index: u64,
    pub inclusion: ProofBundle,
}

/// Result of a local compliance-key append attempt. `appended` is false only when the exact
/// publication was already committed, which lets an offline operator safely resume witness
/// publication after losing the first command's response.
#[derive(Debug, Clone)]
pub struct ComplianceKeyPublicationResult {
    pub key: LoggedComplianceKey,
    pub appended: bool,
}

/// One compliance-key query evaluated against one SQLite read snapshot, including empty results.
#[derive(Debug, Clone)]
pub struct ComplianceKeySet {
    pub head: ProofBundle,
    pub keys: Vec<LoggedComplianceKey>,
}

/// An authenticated directory mutation committed to the registry's shared transparency log.
#[derive(Debug, Clone)]
pub struct LoggedDirectoryMutation {
    /// The exact immutable leaf, including the original loft signature.
    pub entry: LogEntry,
    pub index: u64,
    /// False only for an exact retry of an already committed mutation.
    pub appended: bool,
    pub inclusion: ProofBundle,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum IdentityChallengeProvider {
    Github,
    Google,
}

impl IdentityChallengeProvider {
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Github => "github",
            Self::Google => "google",
        }
    }

    const fn namespace(self) -> &'static str {
        match self {
            Self::Github => "github",
            Self::Google => "google",
        }
    }

    fn authorization(self, client_id: String) -> AuthorizationMetadata {
        match self {
            Self::Github => AuthorizationMetadata {
                client_id,
                authorization_endpoint: GITHUB_AUTHORIZATION_ENDPOINT,
                response_type: "code",
                response_mode: "query",
                scopes: Vec::new(),
                challenge_parameter: "state",
                pkce_method: Some("S256"),
            },
            Self::Google => AuthorizationMetadata {
                client_id,
                authorization_endpoint: GOOGLE_AUTHORIZATION_ENDPOINT,
                response_type: "id_token",
                // The implicit OIDC response is returned in the URI fragment. A loopback client
                // must serve a tiny local page that relays the fragment to its one-shot listener;
                // the fragment itself is never sent to a remote server.
                response_mode: "fragment",
                // The product's Google OIDC contract requests the narrow `openid profile` pair,
                // then discards every optional profile claim: the registry binds only the stable
                // `sub` value.
                scopes: vec!["openid", "profile"],
                challenge_parameter: "nonce",
                pkce_method: None,
            },
        }
    }
}

/// Public, bounded inputs needed to construct the provider authorization URL.
///
/// The client must still use a fixed loopback redirect URI and independently allowlist the
/// endpoint for the selected provider. No provider secret is represented by this type.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AuthorizationMetadata {
    pub client_id: String,
    pub authorization_endpoint: &'static str,
    pub response_type: &'static str,
    pub response_mode: &'static str,
    pub scopes: Vec<&'static str>,
    pub challenge_parameter: &'static str,
    pub pkce_method: Option<&'static str>,
}

#[derive(Clone)]
pub struct IdentityChallenge {
    pub provider: IdentityChallengeProvider,
    /// GitHub `state` or OIDC `nonce`. Only its domain-separated hash is persisted.
    pub value: String,
    pub expires_at_ms: u64,
    pub authorization: AuthorizationMetadata,
}

impl std::fmt::Debug for IdentityChallenge {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("IdentityChallenge")
            .field("provider", &self.provider)
            .field("value", &"<redacted>")
            .field("expires_at_ms", &self.expires_at_ms)
            .field("authorization", &self.authorization)
            .finish()
    }
}

impl Registry {
    /// Open a current registry. Non-empty pre-versioned databases fail closed.
    pub fn open(path: &str, config: RegistryConfig) -> Result<Self> {
        Self::open_persistent(
            path,
            config,
            LegacyAuthorization::Refuse,
            PersistentOpenMode::CreateIfMissing,
        )
    }

    /// Open a current registry that must already exist.
    ///
    /// This is the operator-ceremony boundary: a missing main database is rejected before SQLite
    /// can create the database, a journal, or either WAL sidecar.
    pub fn open_existing(path: &str, config: RegistryConfig) -> Result<Self> {
        Self::open_persistent(
            path,
            config,
            LegacyAuthorization::Refuse,
            PersistentOpenMode::Existing,
        )
    }

    /// One-time authenticated migration from the released flat handle-entry schema.
    ///
    /// `signed_checkpoint` must be a checkpoint produced by that database before the upgrade and
    /// signed by `config.signing_key`; its origin, size, and root must all match the imported rows.
    pub fn open_with_legacy_checkpoint(
        path: &str,
        config: RegistryConfig,
        signed_checkpoint: &str,
    ) -> Result<Self> {
        Self::open_persistent(
            path,
            config,
            LegacyAuthorization::SignedCheckpoint(signed_checkpoint),
            PersistentOpenMode::Existing,
        )
    }

    pub fn in_memory(config: RegistryConfig) -> Result<Self> {
        let config = ValidatedRegistryConfig::new(config)?;
        Self::build(
            Connection::open_in_memory()?,
            config,
            LegacyAuthorization::Refuse,
            RegistryStorageCustody::Memory,
        )
    }

    fn open_persistent(
        path: &str,
        config: RegistryConfig,
        migration: LegacyAuthorization<'_>,
        mode: PersistentOpenMode,
    ) -> Result<Self> {
        require_supported_persistent_registry_platform()?;
        let config = ValidatedRegistryConfig::new(config)?;
        let (conn, storage_custody) = open_persistent_connection(path, mode)?;
        Self::build(conn, config, migration, storage_custody)
    }

    fn build(
        mut conn: Connection,
        config: ValidatedRegistryConfig,
        migration: LegacyAuthorization<'_>,
        storage_custody: RegistryStorageCustody,
    ) -> Result<Self> {
        let ValidatedRegistryConfig(config) = config;
        conn.pragma_update(None, "journal_mode", "WAL")?;
        conn.pragma_update(None, "synchronous", "FULL")?;
        conn.pragma_update(None, "foreign_keys", true)?;
        conn.pragma_update(None, "busy_timeout", 5_000)?;
        storage::initialize(&mut conn, &config.origin, &config.signing_key, migration)?;
        storage_custody.revalidate()?;
        Ok(Self {
            config,
            storage_custody,
            providers: HashMap::new(),
            conn: Mutex::new(conn),
            global_binding_admission: Mutex::new(storage::GlobalBindingAdmissionBatch::default()),
            claim_trace: Arc::new(UnconfiguredClaimTraceSink),
            witness_policy: None,
            account_limiter: AccountLimiter::new(RegistrationLimits::default())?,
            #[cfg(test)]
            commit_test_barrier: Mutex::new(None),
        })
    }

    #[cfg(test)]
    pub(crate) fn install_commit_test_barrier(&self, barrier: Arc<CommitTestBarrier>) {
        *self
            .commit_test_barrier
            .lock()
            .unwrap_or_else(|error| error.into_inner()) = Some(barrier);
    }

    pub fn with_provider(mut self, provider: Box<dyn IdentityProvider>) -> Self {
        self.providers.insert(provider.namespace(), provider);
        self
    }

    /// Configure the mandatory purpose-separated claim trace. Without this, reads still work but
    /// identity challenges, registration, and rotation fail closed.
    pub fn with_claim_trace(mut self, claim_trace: Arc<dyn ClaimTraceSink>) -> Self {
        self.claim_trace = claim_trace;
        self
    }

    /// Replace the stable-provider-account registration budget before sharing this registry.
    pub fn with_registration_limits(mut self, limits: RegistrationLimits) -> Result<Self> {
        self.account_limiter = AccountLimiter::new(limits)?;
        Ok(self)
    }

    /// Enable fail-closed quorum publication. Committed leaves remain durable while every public
    /// read is pinned to the last independently witnessed head.
    pub fn with_witness_policy(mut self, policy: WitnessPolicy) -> Result<Self> {
        if policy.witnesses().iter().any(|witness| {
            witness.key().as_bytes() == self.config.signing_key.verifying_key().as_bytes()
        }) {
            return Err(RegistryError::InvalidConfiguration(
                "witness keys must be distinct from the registry checkpoint key".into(),
            ));
        }
        {
            let conn = self.conn.lock().unwrap_or_else(|error| error.into_inner());
            for witness in policy.witnesses() {
                let Some(json) = storage::witness_receipt_json(&conn, witness.name())? else {
                    continue;
                };
                let receipt: WitnessReceipt = serde_json::from_str(&json).map_err(|_| {
                    RegistryError::CorruptStorage("persisted witness receipt is malformed".into())
                })?;
                let verified = Checkpoint::verify_with_fresh_witnesses(
                    receipt.note(),
                    &self.config.signing_key.verifying_key(),
                    &[witness.clone()],
                    1,
                    receipt.witnessed_at(),
                    policy.max_cosignature_age_secs(),
                    0,
                )
                .map_err(|_| RegistryError::WitnessConflict)?;
                if receipt.witness_name() != witness.name()
                    || verified.witnessed_at != Some(receipt.witnessed_at())
                    || verified.checkpoint.origin != self.config.origin
                    || verified.checkpoint.size != receipt.size()
                    || verified.checkpoint.root != *receipt.root()
                {
                    return Err(RegistryError::WitnessConflict);
                }
            }
            let published = storage::load_published_state(&conn)?;
            if let Some(witnessed_at) = published.witnessed_at {
                let verified = policy
                    .verify_checkpoint(
                        &published.state.checkpoint,
                        &self.config.signing_key.verifying_key(),
                        witnessed_at,
                    )
                    .map_err(|_| RegistryError::WitnessConflict)?;
                if verified.checkpoint.origin != self.config.origin
                    || verified.checkpoint.size != published.state.size
                    || verified.checkpoint.root != published.state.root
                {
                    return Err(RegistryError::WitnessConflict);
                }
            }
        }
        self.witness_policy = Some(policy);
        Ok(self)
    }

    pub fn witness_policy(&self) -> Option<&WitnessPolicy> {
        self.witness_policy.as_ref()
    }

    pub fn registration_enabled(&self) -> bool {
        !self.providers.is_empty()
    }

    pub fn registration_limits(&self) -> RegistrationLimits {
        self.account_limiter.limits
    }

    pub fn claim_trace_capacity_contract(&self) -> Option<ClaimTraceCapacity> {
        self.claim_trace.capacity_contract()
    }

    /// Validate the release contract at the witnessed serving boundary. This intentionally applies
    /// to provider-enabled loopback listeners too because they are commonly exposed by a proxy.
    pub(crate) fn validate_registration_capacity_contract(&self) -> Result<()> {
        if !self.registration_enabled() {
            return Ok(());
        }
        let Some(capacity) = self.claim_trace_capacity_contract() else {
            return Err(RegistryError::InvalidConfiguration(
                "public identity-provider serving requires a claim-trace capacity contract".into(),
            ));
        };
        let required_epochs = capacity.policy.required_capacity_epochs().map_err(|_| {
            RegistryError::InvalidConfiguration(
                "invalid jurisdictional claim-trace capacity policy".into(),
            )
        })?;
        let network_required = required_network_trace_storage_bytes(
            capacity.records_per_minute,
            capacity.utc_epochs,
            capacity.max_records_per_segment,
        )
        .map_err(|_| {
            RegistryError::InvalidConfiguration("invalid claim-trace capacity contract".into())
        })?;
        let identity_required = required_identity_trace_storage_bytes(
            capacity.records_per_minute,
            capacity.utc_epochs,
            capacity.max_records_per_segment,
        )
        .map_err(|_| {
            RegistryError::InvalidConfiguration("invalid claim-trace capacity contract".into())
        })?;
        if capacity.records_per_minute < self.registration_limits().global_bindings_per_minute
            || capacity.utc_epochs < required_epochs
            || capacity.network_logical_limit_bytes > MAX_TRACE_STORAGE_BYTES
            || capacity.identity_logical_limit_bytes > MAX_TRACE_STORAGE_BYTES
            || capacity.network_logical_limit_bytes < network_required
            || capacity.identity_logical_limit_bytes < identity_required
        {
            return Err(RegistryError::InvalidConfiguration(
                "claim-trace capacity does not cover the global binding admission and purpose-storage requirements"
                    .into(),
            ));
        }
        Ok(())
    }

    pub(crate) fn validate_public_registration_capacity(&self) -> Result<()> {
        self.storage_custody.require_public()?;
        if self.config.allow_mock_identities {
            return Err(RegistryError::InvalidConfiguration(
                "public witnessed serving cannot enable mock identity providers".into(),
            ));
        }
        self.validate_registration_capacity_contract()?;
        if self.registration_enabled()
            && self.claim_trace.as_ref().type_id() != std::any::TypeId::of::<SealedClaimTraceSink>()
        {
            return Err(RegistryError::InvalidConfiguration(
                "public identity-provider serving requires the audited sealed claim-trace adapter"
                    .into(),
            ));
        }
        Ok(())
    }

    pub fn registration_readiness(&self, now_ms: u64) -> Result<()> {
        self.storage_custody.revalidate()?;
        if self.registration_enabled() {
            self.validate_registration_capacity_contract()?;
            self.claim_trace
                .readiness(now_ms)
                .map_err(|_| RegistryError::ClaimTraceUnavailable)?;
        }
        self.witness_readiness(now_ms / 1_000)?;
        Ok(())
    }

    pub fn witness_readiness(&self, now_secs: u64) -> Result<WitnessPublicationStatus> {
        self.storage_custody.revalidate()?;
        let Some(policy) = &self.witness_policy else {
            let size = self.committed_size()?;
            return Ok(WitnessPublicationStatus {
                committed_size: size,
                published_size: size,
                lag_entries: 0,
                witnessed_at: None,
            });
        };
        let conn = self.conn.lock().unwrap_or_else(|error| error.into_inner());
        let committed = storage::load_state(&conn)?;
        let published = storage::load_published_state(&conn)?;
        let lag_entries = committed.size.saturating_sub(published.state.size);
        let verified = policy
            .verify_checkpoint(
                &published.state.checkpoint,
                &self.config.signing_key.verifying_key(),
                now_secs,
            )
            .map_err(|_| RegistryError::WitnessUnavailable)?;
        if published.witnessed_at.is_none()
            || verified.witnessed_at != published.witnessed_at
            || verified.checkpoint.origin != self.config.origin
            || verified.checkpoint.size != published.state.size
            || verified.checkpoint.root != published.state.root
            || lag_entries > policy.max_lag_entries()
        {
            return Err(RegistryError::WitnessUnavailable);
        }
        Ok(WitnessPublicationStatus {
            committed_size: committed.size,
            published_size: published.state.size,
            lag_entries,
            witnessed_at: published.witnessed_at,
        })
    }

    pub fn witness_publication_status(&self) -> Result<WitnessPublicationStatus> {
        let conn = self.conn.lock().unwrap_or_else(|error| error.into_inner());
        let committed = storage::load_state(&conn)?;
        if self.witness_policy.is_none() {
            return Ok(WitnessPublicationStatus {
                committed_size: committed.size,
                published_size: committed.size,
                lag_entries: 0,
                witnessed_at: None,
            });
        }
        let published = storage::load_published_state(&conn)?;
        Ok(WitnessPublicationStatus {
            committed_size: committed.size,
            published_size: published.state.size,
            lag_entries: committed.size.saturating_sub(published.state.size),
            witnessed_at: published.witnessed_at,
        })
    }

    pub fn shutdown_claim_trace(&self, now_ms: u64) -> Result<()> {
        self.claim_trace
            .shutdown(now_ms)
            .map_err(|_| RegistryError::ClaimTraceUnavailable)
    }

    pub fn origin(&self) -> &str {
        &self.config.origin
    }

    pub fn verifying_key(&self) -> ed25519_dalek::VerifyingKey {
        self.config.signing_key.verifying_key()
    }

    /// Create a short-lived, single-use OAuth state or OIDC nonce bound to one authenticated
    /// handle/key claim. The challenge cannot later be consumed for a different key.
    pub fn issue_identity_challenge(
        &self,
        provider: IdentityChallengeProvider,
        handle: &Handle,
        pubkey: &[u8; 32],
        signature: &[u8; 64],
        pkce_challenge: Option<&str>,
    ) -> Result<IdentityChallenge> {
        self.registration_readiness(now_ms())?;
        if handle.namespace() != provider.namespace() {
            return Err(RegistryError::ProofRejected(
                "identity provider does not match the handle namespace".into(),
            ));
        }
        let key = keys::verifying_key_from_bytes(pubkey)?;
        let payload = claim_payload(&handle.as_path(), pubkey);
        keys::verify(
            &key,
            &payload,
            &ed25519_dalek::Signature::from_bytes(signature),
        )
        .map_err(|_| RegistryError::KeyPossessionNotProved)?;

        match provider {
            IdentityChallengeProvider::Github => {
                let challenge = pkce_challenge.ok_or_else(|| {
                    RegistryError::ProofRejected("GitHub challenge requires PKCE S256".into())
                })?;
                validate_pkce_challenge(challenge)?;
            }
            IdentityChallengeProvider::Google if pkce_challenge.is_some() => {
                return Err(RegistryError::ProofRejected(
                    "Google nonce challenge does not accept PKCE material".into(),
                ));
            }
            IdentityChallengeProvider::Google => {}
        }

        let client_id = self
            .providers
            .get(provider.namespace())
            .and_then(|identity| identity.public_client_id())
            .ok_or(RegistryError::ProviderNotConfigured)?;
        validate_public_client_id(client_id)?;
        let authorization = provider.authorization(client_id.to_owned());

        let mut random = [0u8; 32];
        OsRng.fill_bytes(&mut random);
        let value = hex(&random);
        let hash = challenge_hash(provider.as_str(), &value);
        let expires_at_ms = now_ms()
            .checked_add(IDENTITY_CHALLENGE_TTL_MS)
            .ok_or_else(|| RegistryError::ProofRejected("challenge expiry overflow".into()))?;
        let mut conn = self.conn.lock().unwrap_or_else(|error| error.into_inner());
        storage::insert_identity_challenge(
            &mut conn,
            provider.as_str(),
            &hash,
            &handle.as_path(),
            pubkey,
            pkce_challenge,
            expires_at_ms,
        )?;
        Ok(IdentityChallenge {
            provider,
            value,
            expires_at_ms,
            authorization,
        })
    }

    /// Claim a previously unbound handle after identity and key-possession verification.
    /// An exact retry is idempotent; changing an existing binding requires [`Registry::rotate`].
    pub async fn register(
        &self,
        handle: &Handle,
        pubkey: &[u8; 32],
        signature: &[u8; 64],
        proof: &ProofPayload,
        source: SocketAddr,
    ) -> Result<Registration> {
        self.bind_handle(
            handle,
            pubkey,
            signature,
            proof,
            source,
            HandleBindingOperation::Register,
        )
        .await
    }

    /// Rotate an existing handle after a fresh identity proof. Rotations always append; an exact
    /// retry is idempotent and returns the prior rotation index.
    pub async fn rotate(
        &self,
        handle: &Handle,
        pubkey: &[u8; 32],
        signature: &[u8; 64],
        proof: &ProofPayload,
        source: SocketAddr,
    ) -> Result<Registration> {
        self.bind_handle(
            handle,
            pubkey,
            signature,
            proof,
            source,
            HandleBindingOperation::Rotate,
        )
        .await
    }

    async fn bind_handle(
        &self,
        handle: &Handle,
        pubkey: &[u8; 32],
        signature: &[u8; 64],
        proof: &ProofPayload,
        source: SocketAddr,
        operation: HandleBindingOperation,
    ) -> Result<Registration> {
        let prepared = self.prepare_handle_binding(
            handle.clone(),
            *pubkey,
            *signature,
            proof.clone(),
            source,
            operation,
        )?;
        if prepared.recovered_seq.is_some() {
            return self.recover_handle_binding(prepared);
        }
        let verified = self.verify_handle_binding(prepared).await?;
        let verified = self.admit_handle_binding(verified)?;
        self.capture_handle_binding(&verified).await?;
        self.commit_handle_binding(verified)
    }

    /// Synchronous validation phase. The HTTP adapter always runs this inside its bounded blocking
    /// lane because challenge validation reads SQLite before any provider request is made.
    pub(crate) fn prepare_handle_binding(
        &self,
        handle: Handle,
        pubkey: [u8; 32],
        signature: [u8; 64],
        proof: ProofPayload,
        source: SocketAddr,
        operation: HandleBindingOperation,
    ) -> Result<PreparedHandleBinding> {
        if proof.is_test_mock() && !self.config.allow_mock_identities {
            return Err(RegistryError::ProofRejected(
                "mock identities are disabled".into(),
            ));
        }

        // Reject forged key bindings before making any provider request.
        let key = keys::verifying_key_from_bytes(&pubkey)?;
        let payload = claim_payload(&handle.as_path(), &pubkey);
        keys::verify(
            &key,
            &payload,
            &ed25519_dalek::Signature::from_bytes(&signature),
        )
        .map_err(|_| RegistryError::KeyPossessionNotProved)?;

        let (challenge, recovered_seq) = if !proof.is_test_mock() {
            let token = proof.challenge_token().ok_or_else(|| {
                RegistryError::ProofRejected("identity challenge is required".into())
            })?;
            validate_challenge_token(token)?;
            let expected_pkce = proof.expected_pkce_challenge()?;
            let hash = challenge_hash(proof.provider_slug(), token);
            let conn = self.conn.lock().unwrap_or_else(|error| error.into_inner());
            let recovered_seq = storage::validate_identity_challenge(
                &conn,
                proof.provider_slug(),
                &hash,
                &handle.as_path(),
                &pubkey,
                expected_pkce.as_deref(),
            )?;
            (Some((hash, expected_pkce)), recovered_seq)
        } else {
            (None, None)
        };

        Ok(PreparedHandleBinding {
            handle,
            pubkey,
            proof,
            source,
            operation,
            challenge,
            recovered_seq,
        })
    }

    /// Recover the exact receipt of a challenge that already committed, without repeating the
    /// external provider request, trace write, account charge, or global binding admission.
    /// Possession of the challenge token is still paired with the bound key signature above.
    pub(crate) fn recover_handle_binding(
        &self,
        prepared: PreparedHandleBinding,
    ) -> Result<Registration> {
        let seq = prepared.recovered_seq.ok_or_else(|| {
            RegistryError::CorruptStorage("recovered challenge has no binding sequence".into())
        })?;
        let pubkey_hex = hex(&prepared.pubkey);
        let conn = self.conn.lock().unwrap_or_else(|error| error.into_inner());
        let entry = storage::entry_at(&conn, seq)?.ok_or_else(|| {
            RegistryError::CorruptStorage(
                "challenge result points at a missing handle binding".into(),
            )
        })?;
        let expected_kind = matches!(
            (&entry, prepared.operation),
            (LogEntry::HandleClaim(_), HandleBindingOperation::Register)
                | (LogEntry::HandleRotation(_), HandleBindingOperation::Rotate)
        );
        let expected_binding = entry.handle_binding().is_some_and(|(handle, pubkey, _)| {
            handle == prepared.handle.as_path() && pubkey == pubkey_hex
        });
        if !expected_kind || !expected_binding {
            return Err(RegistryError::CorruptStorage(
                "challenge result disagrees with its committed handle binding".into(),
            ));
        }

        let serving = self.serving_state(&conn)?;
        let path = if seq < serving.size {
            storage::inclusion_proof(&conn, seq, serving.size)?.ok_or_else(|| {
                RegistryError::CorruptStorage(
                    "published challenge result has no inclusion proof".into(),
                )
            })?
        } else {
            Vec::new()
        };
        Ok(Registration {
            handle: prepared.handle.as_path(),
            index: seq,
            appended: false,
            inclusion: ProofBundle {
                size: serving.size,
                root: serving.root,
                path,
                checkpoint: serving.checkpoint,
            },
        })
    }

    /// Network-only identity-provider phase. No Registry mutex or SQLite connection is touched
    /// while the external proof is awaited.
    pub(crate) async fn verify_handle_binding(
        &self,
        prepared: PreparedHandleBinding,
    ) -> Result<VerifiedHandleBinding> {
        let provider = self
            .providers
            .get(prepared.handle.namespace())
            .ok_or(RegistryError::ProviderNotConfigured)?;
        let subject = provider.verify(&prepared.proof).await?;
        if subject.namespace != prepared.handle.namespace() {
            return Err(RegistryError::SubjectMismatch {
                proved: format!("{}:{}", subject.namespace, subject.name),
                claimed: prepared.handle.as_path(),
            });
        }

        let subject_id = format!("{}:{}", subject.namespace, subject.opaque_id);
        // Charge the stable verified account before any trace submission or registry mutation.
        // The limiter retains only a domain-separated digest, so source-IP rotation cannot evade
        // this budget and the raw provider subject never becomes limiter state.
        self.account_limiter
            .charge(subject.namespace, &subject.opaque_id)?;
        Ok(VerifiedHandleBinding {
            prepared,
            subject_name: subject.name,
            subject_id,
        })
    }

    /// Synchronous readiness, rotation-state, and durable global-admission phase. Public HTTP runs
    /// the whole phase in the configured fail-fast blocking lane.
    pub(crate) fn admit_handle_binding(
        &self,
        verified: VerifiedHandleBinding,
    ) -> Result<VerifiedHandleBinding> {
        self.registration_readiness(now_ms())?;
        match verified.prepared.operation {
            HandleBindingOperation::Register => {
                // A first claim reflects the provider's current public spelling. Stable opaque
                // subject identity does not grant a second handle after a provider rename.
                if !verified
                    .subject_name
                    .eq_ignore_ascii_case(verified.prepared.handle.name())
                {
                    return Err(RegistryError::SubjectMismatch {
                        proved: format!(
                            "{}:{}",
                            verified.prepared.handle.namespace(),
                            verified.subject_name
                        ),
                        claimed: verified.prepared.handle.as_path(),
                    });
                }
            }
            HandleBindingOperation::Rotate => {
                // Provider names are mutable and can later be reassigned. Recovery therefore
                // authorizes the exact existing handle by its stable opaque subject, never by the
                // provider's current display/login name.
                let conn = self.conn.lock().unwrap_or_else(|error| error.into_inner());
                let Some((_, _, existing_subject)) =
                    storage::current_binding(&conn, &verified.prepared.handle.as_path())?
                else {
                    return Err(RegistryError::NotFound);
                };
                if existing_subject != verified.subject_id {
                    return Err(RegistryError::AlreadyBound);
                }
            }
        }
        // Every attempt that can reach trace submission consumes one exact global slot. Trace
        // failure or caller cancellation may burn that slot, but it is never refunded. The UTC
        // minute and count are durable, so restarts and peer processes cannot reset the bound.
        {
            let mut batch = self
                .global_binding_admission
                .lock()
                .unwrap_or_else(|error| error.into_inner());
            let mut conn = self.conn.lock().unwrap_or_else(|error| error.into_inner());
            storage::charge_global_binding_admission(
                &mut conn,
                &mut batch,
                now_ms(),
                self.registration_limits().global_bindings_per_minute,
            )?;
        }
        Ok(verified)
    }

    /// Asynchronous trace receipt phase. This is deliberately outside `spawn_blocking`; the trace
    /// worker is supervised and the caller must await its durable receipt directly.
    pub(crate) async fn capture_handle_binding(
        &self,
        verified: &VerifiedHandleBinding,
    ) -> Result<()> {
        let trace_provider = match &verified.prepared.proof {
            ProofPayload::Github { .. } => TraceIdentityProvider::Oauth2,
            ProofPayload::Google { .. } => TraceIdentityProvider::Oidc,
            #[cfg(any(test, feature = "test-utilities"))]
            ProofPayload::Mock { .. } => TraceIdentityProvider::LocalDirectory,
        };
        let trace = Arc::clone(&self.claim_trace);
        let trace_input = ClaimTraceInput {
            timestamp_ms: now_ms(),
            source: verified.prepared.source,
            provider: trace_provider,
            provider_subject: verified.subject_id.clone(),
        };
        trace
            .submit(trace_input)
            .map_err(|_| RegistryError::ClaimTraceUnavailable)?
            .wait()
            .await
            .map_err(|_| RegistryError::ClaimTraceUnavailable)
    }

    /// Final synchronous consume-and-append phase. The HTTP adapter moves this into the same
    /// bounded blocking lane as every other Registry SQLite operation.
    pub(crate) fn commit_handle_binding(
        &self,
        verified: VerifiedHandleBinding,
    ) -> Result<Registration> {
        // A transient trace failure must not burn a legitimate challenge. After trace durability,
        // challenge consumption and append commit atomically with the exact resulting sequence.
        // A caller that times out after SQLite commits can therefore recover the same receipt.
        let pubkey_hex = hex(&verified.prepared.pubkey);
        let challenge = verified
            .prepared
            .challenge
            .as_ref()
            .map(|(hash, expected_pkce)| storage::IdentityChallengeCommit {
                provider: verified.prepared.proof.provider_slug(),
                challenge_hash: hash,
                pubkey: &verified.prepared.pubkey,
                expected_pkce_challenge: expected_pkce.as_deref(),
            });
        let appended = {
            let mut conn = self.conn.lock().unwrap_or_else(|error| error.into_inner());
            storage::commit_handle_binding(
                &mut conn,
                &self.config.origin,
                &self.config.signing_key,
                challenge,
                storage::HandleAppendRequest {
                    handle: &verified.prepared.handle.as_path(),
                    pubkey: &pubkey_hex,
                    subject: &verified.subject_id,
                    ts_ms: now_ms(),
                    mode: verified.prepared.operation.storage_mode(),
                },
            )?
        };
        #[cfg(test)]
        if let Some(barrier) = self
            .commit_test_barrier
            .lock()
            .unwrap_or_else(|error| error.into_inner())
            .take()
        {
            // Deterministically model a drop-only HTTP blocking timeout after SQLite committed but
            // before the response future observed the result.
            barrier.wait();
        }
        let conn = self.conn.lock().unwrap_or_else(|error| error.into_inner());
        let serving = self.serving_state(&conn)?;
        let path = if appended.seq < serving.size {
            storage::inclusion_proof(&conn, appended.seq, serving.size)?.ok_or_else(|| {
                RegistryError::CorruptStorage("published entry has no inclusion proof".into())
            })?
        } else {
            Vec::new()
        };
        Ok(Registration {
            handle: verified.prepared.handle.as_path(),
            index: appended.seq,
            appended: appended.appended,
            inclusion: ProofBundle {
                size: serving.size,
                root: serving.root,
                path,
                checkpoint: serving.checkpoint,
            },
        })
    }

    pub fn resolve(&self, handle: &Handle) -> Result<Resolved> {
        let mut conn = self.conn.lock().unwrap_or_else(|error| error.into_inner());
        let tx = conn.transaction_with_behavior(TransactionBehavior::Deferred)?;
        let state = self.serving_state(&tx)?;
        let (index, pubkey, _) = storage::binding_before(&tx, &handle.as_path(), state.size)?
            .ok_or(RegistryError::NotFound)?;
        let path = storage::inclusion_proof(&tx, index, state.size)?.ok_or_else(|| {
            RegistryError::CorruptStorage("binding has no inclusion proof".into())
        })?;
        tx.commit()?;
        Ok(Resolved {
            handle: handle.as_path(),
            pubkey,
            index,
            inclusion: ProofBundle {
                size: state.size,
                root: state.root,
                path,
                checkpoint: state.checkpoint,
            },
        })
    }

    /// Publish a purpose- and jurisdiction-scoped compliance public key in the same log.
    pub fn publish_compliance_key(
        &self,
        publication: ComplianceKeyPublish,
    ) -> Result<LoggedComplianceKey> {
        Ok(self.publish_compliance_key_idempotent(publication)?.key)
    }

    /// Idempotent local operator form of [`Self::publish_compliance_key`].
    pub fn publish_compliance_key_idempotent(
        &self,
        publication: ComplianceKeyPublish,
    ) -> Result<ComplianceKeyPublicationResult> {
        let mut conn = self.conn.lock().unwrap_or_else(|error| error.into_inner());
        let appended = storage::append_compliance_key(
            &mut conn,
            &self.config.origin,
            &self.config.signing_key,
            publication.clone(),
            now_ms(),
        )?;
        let index = appended.seq;
        let serving = self.serving_state(&conn)?;
        let path = if index < serving.size {
            storage::inclusion_proof(&conn, index, serving.size)?.ok_or_else(|| {
                RegistryError::CorruptStorage(
                    "published compliance key has no inclusion proof".into(),
                )
            })?
        } else {
            Vec::new()
        };
        let entry = storage::entry_at(&conn, index)?.ok_or_else(|| {
            RegistryError::CorruptStorage("published compliance key has no log leaf".into())
        })?;
        Ok(ComplianceKeyPublicationResult {
            key: LoggedComplianceKey {
                publication,
                entry,
                index,
                inclusion: ProofBundle {
                    size: serving.size,
                    root: serving.root,
                    path,
                    checkpoint: serving.checkpoint,
                },
            },
            appended: appended.appended,
        })
    }

    /// Commit a loft-authenticated directory registration to the shared transparency log.
    pub fn append_directory_add(&self, mutation: DirectoryAdd) -> Result<LoggedDirectoryMutation> {
        let mut conn = self.conn.lock().unwrap_or_else(|error| error.into_inner());
        let appended = storage::append_directory_add(
            &mut conn,
            &self.config.origin,
            &self.config.signing_key,
            mutation,
            now_ms(),
        )?;
        self.directory_mutation_result(&conn, appended)
    }

    /// Commit a loft-authenticated graceful removal to the shared transparency log.
    pub fn append_directory_remove(
        &self,
        mutation: DirectoryRemove,
    ) -> Result<LoggedDirectoryMutation> {
        let mut conn = self.conn.lock().unwrap_or_else(|error| error.into_inner());
        let appended = storage::append_directory_remove(
            &mut conn,
            &self.config.origin,
            &self.config.signing_key,
            mutation,
            now_ms(),
        )?;
        self.directory_mutation_result(&conn, appended)
    }

    /// Live or historical compliance-key lookup. Every item is proven at one shared checkpoint.
    pub fn compliance_keys(&self, query: &ComplianceKeyQuery) -> Result<Vec<LoggedComplianceKey>> {
        Ok(self.compliance_key_set(query)?.keys)
    }

    /// Snapshot-bearing form used by HTTP so an empty result cannot be accidentally paired with
    /// a newer head appended by another registry process.
    pub fn compliance_key_set(&self, query: &ComplianceKeyQuery) -> Result<ComplianceKeySet> {
        let mut conn = self.conn.lock().unwrap_or_else(|error| error.into_inner());
        let tx = conn.transaction_with_behavior(TransactionBehavior::Deferred)?;
        let state = self.serving_state(&tx)?;
        let sequences = if query.metadata_only {
            Vec::new()
        } else {
            storage::compliance_sequences(
                &tx,
                query.key_id.as_ref(),
                query.purpose.map(u8::from),
                query.jurisdiction.map(u8::from),
                query.at_ms,
                query.include_inactive,
                state.size,
            )?
        };
        let mut records = Vec::with_capacity(sequences.len());
        for index in sequences {
            let entry = storage::entry_at(&tx, index)?.ok_or_else(|| {
                RegistryError::CorruptStorage(format!(
                    "compliance projection points at missing entry {index}"
                ))
            })?;
            let publication = entry.compliance_publication().cloned().ok_or_else(|| {
                RegistryError::CorruptStorage(format!(
                    "compliance projection points at non-compliance entry {index}"
                ))
            })?;
            let path = storage::inclusion_proof(&tx, index, state.size)?.ok_or_else(|| {
                RegistryError::CorruptStorage(format!(
                    "compliance entry {index} has no inclusion proof"
                ))
            })?;
            records.push(LoggedComplianceKey {
                publication,
                entry,
                index,
                inclusion: ProofBundle {
                    size: state.size,
                    root: state.root,
                    path,
                    checkpoint: state.checkpoint.clone(),
                },
            });
        }
        tx.commit()?;
        Ok(ComplianceKeySet {
            head: ProofBundle {
                size: state.size,
                root: state.root,
                path: Vec::new(),
                checkpoint: state.checkpoint,
            },
            keys: records,
        })
    }

    pub fn entry(&self, seq: u64) -> Result<LogEntry> {
        let conn = self.conn.lock().unwrap_or_else(|error| error.into_inner());
        if seq >= self.serving_state(&conn)?.size {
            return Err(RegistryError::NotFound);
        }
        storage::entry_at(&conn, seq)?.ok_or(RegistryError::NotFound)
    }

    pub fn entries(&self, from: u64, limit: u64) -> Result<Vec<LogEntry>> {
        let conn = self.conn.lock().unwrap_or_else(|error| error.into_inner());
        let size = self.serving_state(&conn)?.size;
        if from >= size {
            return Ok(Vec::new());
        }
        storage::entries_page(&conn, from, limit.min(size - from))
    }

    /// Immutable commitment used to cache one exact `[from, to)` NDJSON range independently of
    /// later appends.
    pub(crate) fn dump_range_root(&self, from: u64, to: u64) -> Result<Hash> {
        let conn = self.conn.lock().unwrap_or_else(|error| error.into_inner());
        let size = self.serving_state(&conn)?.size;
        if from >= to {
            return Err(RegistryError::MalformedEntry(
                "registry dump range must be non-empty and increasing".into(),
            ));
        }
        if to > size {
            return Err(RegistryError::NotFound);
        }
        storage::root_for_size(&conn, to)
    }

    /// Compatibility helper for local callers. HTTP uses bounded pages/streaming.
    pub fn dump(&self) -> Result<Vec<LogEntry>> {
        let mut conn = self.conn.lock().unwrap_or_else(|error| error.into_inner());
        let tx = conn.transaction_with_behavior(TransactionBehavior::Deferred)?;
        let size = self.serving_state(&tx)?.size;
        let mut from = 0;
        let mut out = Vec::new();
        while from < size {
            let page = storage::entries_page(&tx, from, (size - from).min(storage::MAX_PAGE_SIZE))?;
            if page.is_empty() {
                return Err(RegistryError::CorruptStorage(
                    "log dump encountered a gap".into(),
                ));
            }
            from += page.len() as u64;
            out.extend(page);
        }
        tx.commit()?;
        Ok(out)
    }

    pub fn checkpoint(&self) -> Result<String> {
        let conn = self.conn.lock().unwrap_or_else(|error| error.into_inner());
        Ok(self.serving_state(&conn)?.checkpoint)
    }

    pub fn head(&self) -> Result<ProofBundle> {
        let conn = self.conn.lock().unwrap_or_else(|error| error.into_inner());
        let state = self.serving_state(&conn)?;
        Ok(ProofBundle {
            size: state.size,
            root: state.root,
            path: Vec::new(),
            checkpoint: state.checkpoint,
        })
    }

    pub fn consistency_proof(&self, old_size: u64) -> Result<Option<(u64, Hash, Vec<Hash>)>> {
        let mut conn = self.conn.lock().unwrap_or_else(|error| error.into_inner());
        let tx = conn.transaction_with_behavior(TransactionBehavior::Deferred)?;
        let state = self.serving_state(&tx)?;
        let proof = storage::consistency_proof(&tx, old_size, state.size)?
            .map(|proof| (state.size, state.root, proof));
        tx.commit()?;
        Ok(proof)
    }

    pub fn size(&self) -> Result<u64> {
        let conn = self.conn.lock().unwrap_or_else(|error| error.into_inner());
        Ok(self.serving_state(&conn)?.size)
    }

    pub fn committed_size(&self) -> Result<u64> {
        let conn = self.conn.lock().unwrap_or_else(|error| error.into_inner());
        Ok(storage::load_state(&conn)?.size)
    }

    pub fn committed_head(&self) -> Result<ProofBundle> {
        let conn = self.conn.lock().unwrap_or_else(|error| error.into_inner());
        let state = storage::load_state(&conn)?;
        Ok(ProofBundle {
            size: state.size,
            root: state.root,
            path: Vec::new(),
            checkpoint: state.checkpoint,
        })
    }

    pub fn consistency_proof_between(&self, old: u64, new: u64) -> Result<Vec<Hash>> {
        let mut conn = self.conn.lock().unwrap_or_else(|error| error.into_inner());
        let tx = conn.transaction_with_behavior(TransactionBehavior::Deferred)?;
        let proof = storage::consistency_proof(&tx, old, new)?
            .ok_or_else(|| RegistryError::WitnessConflict)?;
        tx.commit()?;
        Ok(proof)
    }

    pub(crate) fn published_consistency_proof_between(
        &self,
        old: u64,
        new: u64,
    ) -> Result<(Hash, Vec<Hash>)> {
        let mut conn = self.conn.lock().unwrap_or_else(|error| error.into_inner());
        let tx = conn.transaction_with_behavior(TransactionBehavior::Deferred)?;
        let published = self.serving_state(&tx)?;
        if old == 0 || old > new || new > published.size {
            return Err(RegistryError::MalformedEntry(
                "consistency range must be non-zero, ordered, and within the published checkpoint"
                    .into(),
            ));
        }
        let proof = storage::consistency_proof(&tx, old, new)?.ok_or_else(|| {
            RegistryError::MalformedEntry("consistency range has no proof".into())
        })?;
        let root = storage::root_for_size(&tx, new)?;
        tx.commit()?;
        Ok((root, proof))
    }

    pub fn witness_receipt(&self, witness_name: &str) -> Result<Option<WitnessReceipt>> {
        let conn = self.conn.lock().unwrap_or_else(|error| error.into_inner());
        storage::witness_receipt_json(&conn, witness_name)?
            .map(|json| {
                serde_json::from_str(&json).map_err(|_| {
                    RegistryError::CorruptStorage("persisted witness receipt is malformed".into())
                })
            })
            .transpose()
    }

    pub fn save_witness_receipt(&self, receipt: &WitnessReceipt, now_secs: u64) -> Result<bool> {
        let policy = self
            .witness_policy
            .as_ref()
            .ok_or(RegistryError::WitnessUnavailable)?;
        let witness = policy
            .witnesses()
            .iter()
            .find(|witness| witness.name() == receipt.witness_name())
            .ok_or(RegistryError::WitnessConflict)?;
        let verified = Checkpoint::verify_with_fresh_witnesses(
            receipt.note(),
            &self.config.signing_key.verifying_key(),
            &[witness.clone()],
            1,
            now_secs,
            policy.max_cosignature_age_secs(),
            policy.future_clock_skew_secs(),
        )
        .map_err(|_| RegistryError::WitnessConflict)?;
        if verified.witnessed_at != Some(receipt.witnessed_at())
            || verified.checkpoint.origin != self.config.origin
            || verified.checkpoint.size != receipt.size()
            || verified.checkpoint.root != *receipt.root()
        {
            return Err(RegistryError::WitnessConflict);
        }
        let json = serde_json::to_string(receipt)?;
        let mut conn = self.conn.lock().unwrap_or_else(|error| error.into_inner());
        storage::save_witness_receipt(
            &mut conn,
            receipt.witness_name(),
            receipt.size(),
            receipt.root(),
            receipt.witnessed_at(),
            &json,
        )
    }

    pub fn promote_witnessed_head(&self, now_secs: u64) -> Result<bool> {
        let policy = self
            .witness_policy
            .as_ref()
            .ok_or(RegistryError::WitnessUnavailable)?;
        let mut conn = self.conn.lock().unwrap_or_else(|error| error.into_inner());
        let committed = storage::load_state(&conn)?;
        let json_receipts =
            storage::witness_receipt_jsons_at(&conn, committed.size, &committed.root)?;
        let receipts: Vec<WitnessReceipt> = json_receipts
            .iter()
            .map(|json| {
                serde_json::from_str(json).map_err(|_| {
                    RegistryError::CorruptStorage("persisted witness receipt is malformed".into())
                })
            })
            .collect::<Result<_>>()?;
        let (verified, note) = policy
            .assemble_checkpoint(
                &committed.checkpoint,
                &self.config.signing_key.verifying_key(),
                &receipts,
                now_secs,
            )
            .map_err(|_| RegistryError::WitnessUnavailable)?;
        let witnessed_at = verified
            .witnessed_at
            .ok_or(RegistryError::WitnessUnavailable)?;
        if verified.checkpoint.origin != self.config.origin
            || verified.checkpoint.size != committed.size
            || verified.checkpoint.root != committed.root
        {
            return Err(RegistryError::WitnessConflict);
        }
        storage::promote_published_state(&mut conn, &verified.checkpoint, &note, witnessed_at)
    }

    fn serving_state(&self, conn: &Connection) -> Result<storage::TreeState> {
        if self.witness_policy.is_some() {
            Ok(storage::load_published_state(conn)?.state)
        } else {
            storage::load_state(conn)
        }
    }

    /// Full payload-to-leaf audit for an operator command; never runs per request.
    pub fn audit_storage(&self) -> Result<u64> {
        let mut conn = self.conn.lock().unwrap_or_else(|error| error.into_inner());
        let tx = conn.transaction_with_behavior(TransactionBehavior::Deferred)?;
        let audited = storage::audit_all_entries(&tx)?;
        tx.commit()?;
        Ok(audited)
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum PersistentOpenMode {
    Existing,
    CreateIfMissing,
}

impl Registry {
    fn directory_mutation_result(
        &self,
        conn: &Connection,
        appended: storage::DirectoryAppend,
    ) -> Result<LoggedDirectoryMutation> {
        let serving = self.serving_state(conn)?;
        let path = if appended.seq < serving.size {
            storage::inclusion_proof(conn, appended.seq, serving.size)?.ok_or_else(|| {
                RegistryError::CorruptStorage(
                    "published directory mutation has no inclusion proof".into(),
                )
            })?
        } else {
            Vec::new()
        };
        let entry = storage::entry_at(conn, appended.seq)?.ok_or_else(|| {
            RegistryError::CorruptStorage("directory mutation has no log leaf".into())
        })?;
        if entry.authenticated_directory_mutation().is_none() {
            return Err(RegistryError::CorruptStorage(
                "directory projection points at an unauthenticated leaf".into(),
            ));
        }
        Ok(LoggedDirectoryMutation {
            entry,
            index: appended.seq,
            appended: appended.appended,
            inclusion: ProofBundle {
                size: serving.size,
                root: serving.root,
                path,
                checkpoint: serving.checkpoint,
            },
        })
    }
}

fn validate_public_client_id(client_id: &str) -> Result<()> {
    if client_id.is_empty()
        || client_id.len() > 512
        || !client_id
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'.' | b'_' | b'-'))
    {
        return Err(RegistryError::InvalidConfiguration(
            "identity provider client id is malformed".into(),
        ));
    }
    Ok(())
}

#[cfg(any(target_os = "linux", target_os = "macos"))]
struct PersistentRegistryStorage {
    directory: GuardedDir,
    file: GuardedFile,
    file_name: LeafName,
    path: PathBuf,
    sidecar_names: [LeafName; 2],
    journal_name: LeafName,
    retained_sidecars: Mutex<[Option<GuardedFile>; 2]>,
}

#[cfg(any(target_os = "linux", target_os = "macos"))]
impl PersistentRegistryStorage {
    fn open(requested: &Path, mode: PersistentOpenMode) -> Result<Self> {
        let normalized = NormalizedPath::new(requested).map_err(map_registry_custody_error)?;
        let file_name = normalized.as_path().file_name().ok_or_else(|| {
            map_registry_custody_error(CustodyError::InvalidPath("database path must name a file"))
        })?;
        let file_name = LeafName::new(file_name).map_err(map_registry_custody_error)?;
        let parent = normalized.as_path().parent().ok_or_else(|| {
            map_registry_custody_error(CustodyError::InvalidPath("database path has no parent"))
        })?;
        let directory = match mode {
            PersistentOpenMode::Existing => {
                GuardedDir::open_existing(parent, DirPolicy::private_mutable())
            }
            PersistentOpenMode::CreateIfMissing => GuardedDir::create_private(parent),
        }
        .map_err(map_registry_custody_error)?;
        let path = directory.absolute_path().join(file_name.as_os_str());
        let sidecar_names = [
            registry_suffixed_leaf(&file_name, "-wal")?,
            registry_suffixed_leaf(&file_name, "-shm")?,
        ];
        let journal_name = registry_suffixed_leaf(&file_name, "-journal")?;

        // Existing-only callers must prove the main file first, before SQLite or any sidecar
        // operation can mutate the namespace. Create-if-missing callers instead validate hostile
        // sidecar leftovers before publishing a new main database.
        let existing_file = match mode {
            PersistentOpenMode::Existing => Some(
                directory
                    .open_file(
                        &file_name,
                        OpenAccess::ReadWrite,
                        registry_sqlite_file_policy(),
                    )
                    .map_err(map_registry_custody_error)?,
            ),
            PersistentOpenMode::CreateIfMissing => None,
        };
        for sidecar in &sidecar_names {
            directory
                .validate_file(sidecar, registry_sqlite_file_policy())
                .map_err(map_registry_custody_error)?;
        }
        directory
            .validate_file(&journal_name, registry_sqlite_file_policy())
            .map_err(map_registry_custody_error)?;
        let file = match existing_file {
            Some(file) => file,
            None => directory
                .open_or_create_file(
                    &file_name,
                    OpenAccess::ReadWrite,
                    registry_sqlite_file_policy(),
                )
                .map_err(map_registry_custody_error)?,
        };
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
        self.directory
            .verify_named()
            .map_err(map_registry_custody_error)?;
        self.file
            .verify_named()
            .map_err(map_registry_custody_error)?;
        let named = self
            .directory
            .validate_file(&self.file_name, registry_sqlite_file_policy())
            .map_err(map_registry_custody_error)?
            .ok_or_else(|| map_registry_custody_error(CustodyError::NotFound))?;
        if named.identity != self.file.identity() {
            return Err(map_registry_custody_error(CustodyError::UnsafeFile(
                "database name no longer identifies retained main file",
            )));
        }
        Ok(())
    }

    fn verify_sqlite_connection(&self, conn: &Connection) -> Result<()> {
        if conn.path().map(Path::new) != Some(self.path.as_path()) {
            return Err(map_registry_custody_error(CustodyError::UnsafeFile(
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
                file.verify_named().map_err(map_registry_custody_error)?;
                continue;
            }
            match self
                .directory
                .open_file_optional(name, OpenAccess::ReadOnly, registry_sqlite_file_policy())
                .map_err(map_registry_custody_error)?
            {
                Some(file) => *retained_file = Some(file),
                None if require_wal_and_shm => {
                    return Err(map_registry_custody_error(CustodyError::UnsafeFile(
                        "required SQLite WAL or SHM sidecar is missing",
                    )));
                }
                None => {}
            }
        }
        self.directory
            .validate_file(&self.journal_name, registry_sqlite_file_policy())
            .map_err(map_registry_custody_error)?;
        Ok(())
    }

    fn verify_all_named(&self) -> Result<()> {
        self.verify_main_named()?;
        self.verify_sidecars(true)
    }
}

#[cfg(all(test, any(target_os = "linux", target_os = "macos")))]
fn registry_sidecar_path(path: &Path, suffix: &str) -> PathBuf {
    let mut value = path.as_os_str().to_os_string();
    value.push(suffix);
    value.into()
}

#[cfg(any(target_os = "linux", target_os = "macos"))]
fn registry_sqlite_file_policy() -> FilePolicy {
    FilePolicy::private(MAX_SQLITE_FILE_BYTES)
}

#[cfg(any(target_os = "linux", target_os = "macos"))]
fn registry_suffixed_leaf(name: &LeafName, suffix: &str) -> Result<LeafName> {
    let mut value = name.as_os_str().to_os_string();
    value.push(suffix);
    LeafName::new(value).map_err(map_registry_custody_error)
}

#[cfg(any(target_os = "linux", target_os = "macos"))]
fn map_registry_custody_error(error: CustodyError) -> RegistryError {
    let error = match error {
        CustodyError::Io(error) if registry_custody_io_is_policy_failure(&error) => {
            std::io::Error::new(std::io::ErrorKind::PermissionDenied, error)
        }
        CustodyError::Io(error) => error,
        error => std::io::Error::new(std::io::ErrorKind::PermissionDenied, error),
    };
    RegistryError::Io(error)
}

#[cfg(any(target_os = "linux", target_os = "macos"))]
fn registry_custody_io_is_policy_failure(error: &std::io::Error) -> bool {
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

#[cfg(any(target_os = "linux", target_os = "macos"))]
fn open_persistent_connection(
    path: &str,
    mode: PersistentOpenMode,
) -> Result<(Connection, RegistryStorageCustody)> {
    // SQLite treats these spellings as memory/temporary databases even through its ordinary open
    // API. Public serving relies on `Registry::open` meaning restart-persistent storage, so keep
    // URI interpretation out of this boundary entirely.
    if path.is_empty() || path == ":memory:" || path.starts_with("file:") {
        return Err(RegistryError::InvalidConfiguration(
            "persistent registry storage requires a non-URI filesystem path".into(),
        ));
    }

    let custody = PersistentRegistryStorage::open(Path::new(path), mode)?;
    let sqlite_path = custody.path.clone();
    let flags = OpenFlags::SQLITE_OPEN_READ_WRITE
        | OpenFlags::SQLITE_OPEN_NO_MUTEX
        | OpenFlags::SQLITE_OPEN_NOFOLLOW;
    custody.verify_main_named()?;
    let conn = Connection::open_with_flags(&sqlite_path, flags)?;
    custody.verify_sqlite_connection(&conn)?;
    Ok((conn, RegistryStorageCustody::Persistent(Box::new(custody))))
}

#[cfg(not(any(target_os = "linux", target_os = "macos")))]
fn open_persistent_connection(
    _path: &str,
    _mode: PersistentOpenMode,
) -> Result<(Connection, RegistryStorageCustody)> {
    Err(RegistryError::Io(std::io::Error::new(
        std::io::ErrorKind::Unsupported,
        "persistent registry storage is supported only on Linux and macOS",
    )))
}

fn require_supported_persistent_registry_platform() -> Result<()> {
    #[cfg(any(target_os = "linux", target_os = "macos"))]
    {
        Ok(())
    }
    #[cfg(not(any(target_os = "linux", target_os = "macos")))]
    {
        Err(RegistryError::Io(std::io::Error::new(
            std::io::ErrorKind::Unsupported,
            "persistent registry storage is supported only on Linux and macOS",
        )))
    }
}

fn validate_origin(origin: &str) -> Result<()> {
    if origin.is_empty()
        || origin.len() > 256
        || !origin.bytes().all(|byte| byte.is_ascii_graphic())
    {
        return Err(RegistryError::InvalidConfiguration(
            "checkpoint origin must be 1..=256 visible ASCII bytes".into(),
        ));
    }
    Ok(())
}

fn now_ms() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|duration| duration.as_millis().min(u128::from(u64::MAX)) as u64)
        .unwrap_or(0)
}

pub fn hex(bytes: &[u8]) -> String {
    const HEX: &[u8; 16] = b"0123456789abcdef";
    let mut out = String::with_capacity(bytes.len() * 2);
    for byte in bytes {
        out.push(HEX[(byte >> 4) as usize] as char);
        out.push(HEX[(byte & 0x0f) as usize] as char);
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::claim_trace::{ClaimCapturePolicy, ClaimTraceCapacity, ClaimTraceError};
    use crate::identity::{pkce_s256, GithubProvider, Subject};
    use pigeonpost_compliance_format::TraceRetentionPolicy;
    use pigeonpost_core::Identity;

    #[derive(Debug)]
    struct TestClaimTrace;

    impl ClaimTraceSink for TestClaimTrace {
        fn capacity_contract(&self) -> Option<ClaimTraceCapacity> {
            Some(ClaimTraceCapacity {
                policy: TraceRetentionPolicy {
                    jurisdiction: Jurisdiction::Test,
                    capture: ClaimCapturePolicy::Standing,
                    retention_days: None,
                },
                records_per_minute: 10_000,
                utc_epochs: 1,
                max_records_per_segment: 10_000,
                network_logical_limit_bytes: MAX_TRACE_STORAGE_BYTES,
                identity_logical_limit_bytes: MAX_TRACE_STORAGE_BYTES,
            })
        }

        fn readiness(&self, _now_ms: u64) -> std::result::Result<(), ClaimTraceError> {
            Ok(())
        }

        fn capture(&self, _input: ClaimTraceInput) -> std::result::Result<(), ClaimTraceError> {
            Ok(())
        }

        fn shutdown(&self, _timestamp_ms: u64) -> std::result::Result<(), ClaimTraceError> {
            Ok(())
        }
    }

    struct MutableProvider {
        subject: Arc<Mutex<Subject>>,
    }

    #[async_trait::async_trait]
    impl IdentityProvider for MutableProvider {
        fn namespace(&self) -> &'static str {
            "github"
        }

        async fn verify(&self, _proof: &ProofPayload) -> Result<Subject> {
            Ok(self
                .subject
                .lock()
                .unwrap_or_else(|error| error.into_inner())
                .clone())
        }
    }

    fn signed_handle(path: &str, seed: u8) -> (Handle, [u8; 32], [u8; 64]) {
        let identity = Identity::from_seed([seed; 32]);
        let handle = Handle::parse(path).unwrap();
        let pubkey = identity.verifying_key().to_bytes();
        let signature = identity
            .sign(&claim_payload(&handle.as_path(), &pubkey))
            .to_bytes();
        (handle, pubkey, signature)
    }

    fn custody_test_config(origin: &str, seed: u8) -> RegistryConfig {
        RegistryConfig {
            origin: origin.into(),
            signing_key: SigningKey::from_bytes(&[seed; 32]),
            allow_mock_identities: false,
        }
    }

    fn invalid_registry_configs() -> Vec<(&'static str, String, u8)> {
        vec![
            ("empty-origin", String::new(), 0x43),
            ("oversized-origin", "x".repeat(257), 0x44),
            (
                "non-visible-origin",
                "registry.test/line\nbreak".into(),
                0x45,
            ),
            (
                "zero-checkpoint-seed",
                "registry.test/zero-checkpoint-seed".into(),
                0,
            ),
        ]
    }

    #[test]
    fn in_memory_prevalidates_every_registry_config_constraint() {
        for (_, origin, seed) in invalid_registry_configs() {
            assert!(matches!(
                Registry::in_memory(custody_test_config(&origin, seed)),
                Err(RegistryError::InvalidConfiguration(_))
            ));
        }
    }

    #[cfg(any(target_os = "linux", target_os = "macos"))]
    #[test]
    fn persistent_apis_prevalidate_config_without_storage_side_effects() {
        for (case, origin, seed) in invalid_registry_configs() {
            for api in ["open", "existing", "legacy"] {
                let temp = tempfile::tempdir().unwrap();
                let parent = temp.path().join(format!("{case}-{api}"));
                let database = parent.join("registry.db");
                let path = database.to_str().unwrap();
                let result = match api {
                    "open" => Registry::open(path, custody_test_config(&origin, seed)),
                    "existing" => Registry::open_existing(path, custody_test_config(&origin, seed)),
                    "legacy" => Registry::open_with_legacy_checkpoint(
                        path,
                        custody_test_config(&origin, seed),
                        "configuration must fail before this migration input is inspected",
                    ),
                    _ => unreachable!(),
                };

                assert!(matches!(
                    result,
                    Err(RegistryError::InvalidConfiguration(_))
                ));
                assert!(!parent.exists(), "{api} created {}", parent.display());
                for candidate in [
                    database.clone(),
                    registry_sidecar_path(&database, "-wal"),
                    registry_sidecar_path(&database, "-shm"),
                    registry_sidecar_path(&database, "-journal"),
                ] {
                    assert!(!candidate.exists(), "{api} created {}", candidate.display());
                }
            }
        }
    }

    #[test]
    fn existing_only_open_rejects_a_missing_database_without_side_effects() {
        let temp = tempfile::tempdir().unwrap();
        let directory = temp.path().join("private-registry");
        std::fs::create_dir(&directory).unwrap();
        #[cfg(any(target_os = "linux", target_os = "macos"))]
        {
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(&directory, std::fs::Permissions::from_mode(0o700)).unwrap();
        }
        let path = directory.join("registry.db");
        let origin = "registry.test/existing-only";

        assert!(
            Registry::open_existing(path.to_str().unwrap(), custody_test_config(origin, 0x41),)
                .is_err()
        );
        assert!(Registry::open_with_legacy_checkpoint(
            path.to_str().unwrap(),
            custody_test_config(origin, 0x41),
            "missing source must fail before this note is parsed",
        )
        .is_err());
        for candidate in [
            path.clone(),
            path.with_file_name("registry.db-wal"),
            path.with_file_name("registry.db-shm"),
            path.with_file_name("registry.db-journal"),
        ] {
            assert!(
                !candidate.exists(),
                "existing-only open created {}",
                candidate.display()
            );
        }

        #[cfg(any(target_os = "linux", target_os = "macos"))]
        {
            let registry =
                Registry::open(path.to_str().unwrap(), custody_test_config(origin, 0x41)).unwrap();
            assert_eq!(registry.size().unwrap(), 0);
            drop(registry);

            let reopened =
                Registry::open_existing(path.to_str().unwrap(), custody_test_config(origin, 0x41))
                    .unwrap();
            assert_eq!(reopened.size().unwrap(), 0);
        }
    }

    #[cfg(not(any(target_os = "linux", target_os = "macos")))]
    #[test]
    fn persistent_registry_apis_reject_before_path_or_legacy_input_side_effects() {
        let temp = tempfile::tempdir().unwrap();
        let database = temp.path().join("must-not-exist/registry.db");
        let path = database.to_str().unwrap();
        let config = || custody_test_config("", 0);

        for result in [
            Registry::open(path, config()),
            Registry::open_existing(path, config()),
            Registry::open_with_legacy_checkpoint(
                path,
                config(),
                "must not be inspected before the platform gate",
            ),
        ] {
            assert!(matches!(
                result,
                Err(RegistryError::Io(error))
                    if error.kind() == std::io::ErrorKind::Unsupported
            ));
        }
        assert!(!database.parent().unwrap().exists());
        assert!(!database.exists());
    }

    #[cfg(any(target_os = "linux", target_os = "macos"))]
    #[test]
    fn persistent_open_rejects_sqlite_memory_and_uri_spellings() {
        for (index, path) in ["", ":memory:", "file:registry?mode=memory"]
            .into_iter()
            .enumerate()
        {
            let result = Registry::open(
                path,
                RegistryConfig {
                    origin: format!("registry.test/persistent-path-{index}"),
                    signing_key: SigningKey::from_bytes(&[0x40 + index as u8; 32]),
                    allow_mock_identities: false,
                },
            );
            assert!(matches!(
                result,
                Err(RegistryError::InvalidConfiguration(_))
            ));
        }
    }

    #[test]
    fn public_witnessed_storage_gate_rejects_memory_with_or_without_a_provider() {
        let witness = crate::WitnessKey::new(
            "custody-witness.test",
            SigningKey::from_bytes(&[0x72; 32]).verifying_key(),
        )
        .unwrap();
        let policy = WitnessPolicy::new(vec![witness], 1, 600, 30, 0).unwrap();

        let witnessed =
            Registry::in_memory(custody_test_config("registry.test/memory-witnessed", 0x70))
                .unwrap()
                .with_witness_policy(policy.clone())
                .unwrap();
        assert!(matches!(
            witnessed.validate_public_registration_capacity(),
            Err(RegistryError::InvalidConfiguration(_))
        ));

        let provider_enabled =
            Registry::in_memory(custody_test_config("registry.test/memory-provider", 0x71))
                .unwrap()
                .with_provider(Box::new(GithubProvider::new("client-id", "client-secret")))
                .with_witness_policy(policy)
                .unwrap();
        assert!(matches!(
            provider_enabled.validate_public_registration_capacity(),
            Err(RegistryError::InvalidConfiguration(_))
        ));
    }

    #[cfg(any(target_os = "linux", target_os = "macos"))]
    #[test]
    fn ordinary_umask_creates_owner_only_registry_storage() {
        let current = std::env::current_exe().unwrap();
        let status = std::process::Command::new("/bin/sh")
            .arg("-c")
            .arg(
                "umask 022; exec \"$0\" --ignored --exact \
                 registry::tests::ordinary_umask_registry_storage_child --test-threads=1",
            )
            .arg(current)
            .status()
            .unwrap();
        assert!(status.success());
    }

    #[cfg(any(target_os = "linux", target_os = "macos"))]
    #[test]
    #[ignore = "invoked in a child process with an explicit ordinary umask"]
    fn ordinary_umask_registry_storage_child() {
        use std::os::unix::fs::MetadataExt;

        let temp = tempfile::tempdir().unwrap();
        let directory = temp.path().join("private-registry");
        let path = directory.join("registry.db");
        let registry = Registry::open(
            path.to_str().unwrap(),
            custody_test_config("registry.test/owner-only", 0x73),
        )
        .unwrap();
        registry.validate_public_registration_capacity().unwrap();
        registry.registration_readiness(now_ms()).unwrap();

        assert_eq!(
            std::fs::metadata(&directory).unwrap().mode() & 0o7777,
            0o700
        );
        for protected in [
            path.clone(),
            registry_sidecar_path(&path, "-wal"),
            registry_sidecar_path(&path, "-shm"),
        ] {
            let metadata = std::fs::metadata(&protected).unwrap_or_else(|error| {
                panic!(
                    "missing protected SQLite file {}: {error}",
                    protected.display()
                )
            });
            assert_eq!(metadata.mode() & 0o7777, 0o600, "{}", protected.display());
            assert_eq!(metadata.nlink(), 1, "{}", protected.display());
        }
    }

    #[cfg(any(target_os = "linux", target_os = "macos"))]
    #[test]
    fn unsafe_registry_parent_and_database_names_are_refused() {
        use std::os::unix::fs::{symlink, PermissionsExt};

        let temp = tempfile::tempdir().unwrap();
        let directory = temp.path().join("private-registry");
        std::fs::create_dir(&directory).unwrap();
        std::fs::set_permissions(&directory, std::fs::Permissions::from_mode(0o755)).unwrap();
        let path = directory.join("registry.db");
        assert!(matches!(
            Registry::open(
                path.to_str().unwrap(),
                custody_test_config("registry.test/public-parent", 0x74),
            ),
            Err(RegistryError::Io(error))
                if error.kind() == std::io::ErrorKind::PermissionDenied
        ));
        assert!(!path.exists());

        std::fs::set_permissions(&directory, std::fs::Permissions::from_mode(0o700)).unwrap();
        std::fs::write(&path, []).unwrap();
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o644)).unwrap();
        assert!(matches!(
            Registry::open(
                path.to_str().unwrap(),
                custody_test_config("registry.test/public-file", 0x75),
            ),
            Err(RegistryError::Io(error))
                if error.kind() == std::io::ErrorKind::PermissionDenied
        ));

        std::fs::remove_file(&path).unwrap();
        let outside = temp.path().join("outside.db");
        std::fs::write(&outside, []).unwrap();
        symlink(&outside, &path).unwrap();
        assert!(matches!(
            Registry::open(
                path.to_str().unwrap(),
                custody_test_config("registry.test/symlink-file", 0x76),
            ),
            Err(RegistryError::Io(error))
                if error.kind() == std::io::ErrorKind::PermissionDenied
        ));

        std::fs::remove_file(&path).unwrap();
        std::fs::write(&path, []).unwrap();
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o600)).unwrap();
        std::fs::hard_link(&path, temp.path().join("registry-copy.db")).unwrap();
        assert!(matches!(
            Registry::open(
                path.to_str().unwrap(),
                custody_test_config("registry.test/hardlink-file", 0x77),
            ),
            Err(RegistryError::Io(error))
                if error.kind() == std::io::ErrorKind::PermissionDenied
        ));
    }

    #[cfg(any(target_os = "linux", target_os = "macos"))]
    #[test]
    fn intermediate_symlink_and_mutable_ancestor_are_refused_without_side_effects() {
        use std::os::unix::fs::{symlink, PermissionsExt};

        let temp = tempfile::tempdir().unwrap();
        let outside = temp.path().join("outside");
        std::fs::create_dir(&outside).unwrap();
        std::fs::set_permissions(&outside, std::fs::Permissions::from_mode(0o700)).unwrap();
        let linked = temp.path().join("linked");
        symlink(&outside, &linked).unwrap();
        let through_link = linked.join("new-private/registry.db");
        assert!(Registry::open(
            through_link.to_str().unwrap(),
            custody_test_config("registry.test/intermediate-link", 0x7e),
        )
        .is_err());
        assert!(!outside.join("new-private").exists());

        let mutable = temp.path().join("mutable");
        std::fs::create_dir(&mutable).unwrap();
        std::fs::set_permissions(&mutable, std::fs::Permissions::from_mode(0o770)).unwrap();
        let through_mutable = mutable.join("new-private/registry.db");
        assert!(Registry::open(
            through_mutable.to_str().unwrap(),
            custody_test_config("registry.test/mutable-ancestor", 0x7f),
        )
        .is_err());
        assert!(!mutable.join("new-private").exists());
    }

    #[cfg(any(target_os = "linux", target_os = "macos"))]
    #[test]
    fn unsafe_preexisting_registry_sidecars_are_refused() {
        use std::os::unix::fs::{symlink, PermissionsExt};

        let temp = tempfile::tempdir().unwrap();
        let directory = temp.path().join("private-registry");
        std::fs::create_dir(&directory).unwrap();
        std::fs::set_permissions(&directory, std::fs::Permissions::from_mode(0o700)).unwrap();
        let path = directory.join("registry.db");
        let outside = temp.path().join("outside");
        std::fs::write(&outside, []).unwrap();
        let wal = registry_sidecar_path(&path, "-wal");
        symlink(&outside, &wal).unwrap();
        assert!(Registry::open(
            path.to_str().unwrap(),
            custody_test_config("registry.test/symlink-sidecar", 0x78),
        )
        .is_err());
        assert!(!path.exists());

        std::fs::remove_file(&wal).unwrap();
        let shm = registry_sidecar_path(&path, "-shm");
        std::fs::write(&shm, []).unwrap();
        std::fs::set_permissions(&shm, std::fs::Permissions::from_mode(0o644)).unwrap();
        assert!(matches!(
            Registry::open(
                path.to_str().unwrap(),
                custody_test_config("registry.test/public-sidecar", 0x79),
            ),
            Err(RegistryError::Io(error))
                if error.kind() == std::io::ErrorKind::PermissionDenied
        ));
        assert!(!path.exists());

        std::fs::remove_file(&shm).unwrap();
        std::fs::write(&wal, []).unwrap();
        std::fs::set_permissions(&wal, std::fs::Permissions::from_mode(0o600)).unwrap();
        std::fs::hard_link(&wal, temp.path().join("wal-copy")).unwrap();
        assert!(matches!(
            Registry::open(
                path.to_str().unwrap(),
                custody_test_config("registry.test/hardlink-sidecar", 0x7a),
            ),
            Err(RegistryError::Io(error))
                if error.kind() == std::io::ErrorKind::PermissionDenied
        ));
        assert!(!path.exists());
    }

    #[cfg(any(target_os = "linux", target_os = "macos"))]
    #[test]
    fn readiness_rejects_post_open_database_sidecar_or_parent_replacement() {
        use std::os::unix::fs::PermissionsExt;

        let temp = tempfile::tempdir().unwrap();
        let database_directory = temp.path().join("database/private-registry");
        let database_path = database_directory.join("registry.db");
        let database_registry = Registry::open(
            database_path.to_str().unwrap(),
            custody_test_config("registry.test/replaced-file", 0x7b),
        )
        .unwrap();
        database_registry.registration_readiness(now_ms()).unwrap();

        let moved_database = database_directory.join("registry.db.original");
        std::fs::rename(&database_path, &moved_database).unwrap();
        std::fs::write(&database_path, []).unwrap();
        std::fs::set_permissions(&database_path, std::fs::Permissions::from_mode(0o600)).unwrap();
        assert!(matches!(
            database_registry.registration_readiness(now_ms()),
            Err(RegistryError::Io(error))
                if error.kind() == std::io::ErrorKind::PermissionDenied
        ));
        drop(database_registry);

        let sidecar_directory = temp.path().join("sidecar/private-registry");
        let sidecar_path = sidecar_directory.join("registry.db");
        let sidecar_registry = Registry::open(
            sidecar_path.to_str().unwrap(),
            custody_test_config("registry.test/replaced-sidecar", 0x7c),
        )
        .unwrap();
        sidecar_registry.registration_readiness(now_ms()).unwrap();
        let wal = registry_sidecar_path(&sidecar_path, "-wal");
        let moved_wal = registry_sidecar_path(&sidecar_path, "-wal.original");
        std::fs::rename(&wal, &moved_wal).unwrap();
        std::fs::write(&wal, []).unwrap();
        std::fs::set_permissions(&wal, std::fs::Permissions::from_mode(0o600)).unwrap();
        assert!(matches!(
            sidecar_registry.registration_readiness(now_ms()),
            Err(RegistryError::Io(error))
                if error.kind() == std::io::ErrorKind::PermissionDenied
        ));
        drop(sidecar_registry);

        let parent_directory = temp.path().join("parent/private-registry");
        let parent_path = parent_directory.join("registry.db");
        let parent_registry = Registry::open(
            parent_path.to_str().unwrap(),
            custody_test_config("registry.test/replaced-parent", 0x7d),
        )
        .unwrap();
        parent_registry.registration_readiness(now_ms()).unwrap();
        let moved_parent = temp.path().join("parent/private-registry.original");
        std::fs::rename(&parent_directory, &moved_parent).unwrap();
        std::fs::create_dir(&parent_directory).unwrap();
        std::fs::set_permissions(&parent_directory, std::fs::Permissions::from_mode(0o700))
            .unwrap();
        std::fs::write(&parent_path, []).unwrap();
        std::fs::set_permissions(&parent_path, std::fs::Permissions::from_mode(0o600)).unwrap();
        assert!(matches!(
            parent_registry.registration_readiness(now_ms()),
            Err(RegistryError::Io(error))
                if error.kind() == std::io::ErrorKind::PermissionDenied
        ));
    }

    #[cfg(any(target_os = "linux", target_os = "macos"))]
    #[test]
    fn readiness_rejects_replaced_or_missing_wal_and_shm() {
        use std::os::unix::fs::PermissionsExt;

        for (index, suffix) in ["-wal", "-shm"].into_iter().enumerate() {
            let temp = tempfile::tempdir().unwrap();
            let path = temp.path().join("private-registry/registry.db");
            let registry = Registry::open(
                path.to_str().unwrap(),
                custody_test_config(
                    &format!("registry.test/sidecar-{index}"),
                    0x80 + index as u8,
                ),
            )
            .unwrap();
            registry.registration_readiness(now_ms()).unwrap();
            let sidecar = registry_sidecar_path(&path, suffix);
            let original = registry_sidecar_path(&path, &format!("{suffix}.original"));
            std::fs::rename(&sidecar, &original).unwrap();
            assert!(registry.registration_readiness(now_ms()).is_err());
            std::fs::write(&sidecar, []).unwrap();
            std::fs::set_permissions(&sidecar, std::fs::Permissions::from_mode(0o600)).unwrap();
            assert!(matches!(
                registry.registration_readiness(now_ms()),
                Err(RegistryError::Io(error))
                    if error.kind() == std::io::ErrorKind::PermissionDenied
            ));
        }
    }

    #[test]
    fn identity_challenge_expires_five_minutes_after_issue() {
        let registry = Registry::in_memory(RegistryConfig {
            origin: "registry.test/log".into(),
            signing_key: SigningKey::from_bytes(&[0x51; 32]),
            allow_mock_identities: false,
        })
        .unwrap()
        .with_provider(Box::new(GithubProvider::new("client-id", "client-secret")))
        .with_claim_trace(Arc::new(TestClaimTrace));
        let verifier = "a".repeat(43);
        let pkce = pkce_s256(&verifier).unwrap();
        let identity = Identity::from_seed([0x52; 32]);
        let handle = Handle::parse("/github/alice").unwrap();
        let pubkey = identity.verifying_key().to_bytes();
        let signature = identity
            .sign(&claim_payload(&handle.as_path(), &pubkey))
            .to_bytes();

        let issued_not_before = now_ms();
        let challenge = registry
            .issue_identity_challenge(
                IdentityChallengeProvider::Github,
                &handle,
                &pubkey,
                &signature,
                Some(&pkce),
            )
            .unwrap();
        let issued_not_after = now_ms();

        assert!(
            challenge.expires_at_ms
                >= issued_not_before
                    .checked_add(IDENTITY_CHALLENGE_TTL_MS)
                    .unwrap()
        );
        assert!(
            challenge.expires_at_ms
                <= issued_not_after
                    .checked_add(IDENTITY_CHALLENGE_TTL_MS)
                    .unwrap(),
            "challenge TTL must not be added twice"
        );
    }

    #[test]
    fn identity_challenge_issuance_requires_the_bound_keys_signature() {
        let registry = Registry::in_memory(RegistryConfig {
            origin: "registry.test/log".into(),
            signing_key: SigningKey::from_bytes(&[0x53; 32]),
            allow_mock_identities: false,
        })
        .unwrap()
        .with_provider(Box::new(GithubProvider::new("client-id", "client-secret")))
        .with_claim_trace(Arc::new(TestClaimTrace));
        let owner = Identity::from_seed([0x54; 32]);
        let attacker = Identity::from_seed([0x55; 32]);
        let handle = Handle::parse("/github/alice").unwrap();
        let pubkey = owner.verifying_key().to_bytes();
        let forged = attacker
            .sign(&claim_payload(&handle.as_path(), &pubkey))
            .to_bytes();
        let pkce = pkce_s256(&"a".repeat(43)).unwrap();

        let error = registry
            .issue_identity_challenge(
                IdentityChallengeProvider::Github,
                &handle,
                &pubkey,
                &forged,
                Some(&pkce),
            )
            .unwrap_err();
        assert!(matches!(error, RegistryError::KeyPossessionNotProved));
    }

    #[tokio::test]
    async fn provider_rename_keeps_the_old_handle_and_allows_the_new_spelling() {
        let subject = Arc::new(Mutex::new(Subject {
            namespace: "github",
            name: "alice".into(),
            opaque_id: "account-1".into(),
        }));
        let registry = Registry::in_memory(RegistryConfig {
            origin: "registry.test/provider-rename".into(),
            signing_key: SigningKey::from_bytes(&[0x61; 32]),
            allow_mock_identities: true,
        })
        .unwrap()
        .with_provider(Box::new(MutableProvider {
            subject: Arc::clone(&subject),
        }))
        .with_claim_trace(Arc::new(TestClaimTrace));
        let proof = ProofPayload::Mock {
            name: "provider-proof".into(),
        };
        let source = "127.0.0.1:4242".parse().unwrap();

        let (original, original_key, original_signature) = signed_handle("/github/alice", 0x62);
        registry
            .register(
                &original,
                &original_key,
                &original_signature,
                &proof,
                source,
            )
            .await
            .unwrap();

        subject
            .lock()
            .unwrap_or_else(|error| error.into_inner())
            .name = "alice-renamed".into();
        let (_, rotated_key, rotated_signature) = signed_handle("/github/alice", 0x63);
        let rotation = registry
            .rotate(&original, &rotated_key, &rotated_signature, &proof, source)
            .await
            .unwrap();
        assert!(rotation.appended);
        assert_eq!(rotation.handle, "/github/alice");
        assert_eq!(
            registry.resolve(&original).unwrap().pubkey,
            hex(&rotated_key)
        );

        let renamed = Handle::parse("/github/alice-renamed").unwrap();
        let renamed_identity = Identity::from_seed([0x64; 32]);
        let renamed_key = renamed_identity.verifying_key().to_bytes();
        let renamed_signature = renamed_identity
            .sign(&claim_payload(&renamed.as_path(), &renamed_key))
            .to_bytes();
        // The renamed spelling is a *second* handle for the same account, not a conflict: the
        // allowance is three. The original keeps resolving, so nothing published earlier breaks.
        registry
            .register(&renamed, &renamed_key, &renamed_signature, &proof, source)
            .await
            .expect("a renamed account may also claim its new spelling");
        assert_eq!(registry.resolve(&renamed).unwrap().pubkey, hex(&renamed_key));
        assert_eq!(
            registry.resolve(&original).unwrap().pubkey,
            hex(&rotated_key)
        );

        {
            let mut reassigned = subject.lock().unwrap_or_else(|error| error.into_inner());
            reassigned.name = "alice".into();
            reassigned.opaque_id = "account-2".into();
        }
        let (_, attacker_key, attacker_signature) = signed_handle("/github/alice", 0x65);
        let reassigned_account = registry
            .rotate(
                &original,
                &attacker_key,
                &attacker_signature,
                &proof,
                source,
            )
            .await
            .unwrap_err();
        assert!(matches!(reassigned_account, RegistryError::AlreadyBound));
        assert_eq!(
            registry.resolve(&original).unwrap().pubkey,
            hex(&rotated_key)
        );
    }

    #[test]
    fn account_limiter_suppresses_full_map_scans_until_expiry_and_bounds_cleanup() {
        let limiter = AccountLimiter::new(RegistrationLimits {
            global_bindings_per_minute: 10,
            account_bindings_per_minute: 10,
            max_account_keys: 2,
        })
        .unwrap();
        let start = Instant::now();
        limiter.charge_at("github", "one", start).unwrap();
        limiter.charge_at("github", "two", start).unwrap();

        for index in 0..1_000 {
            assert!(matches!(
                limiter.charge_at(
                    "github",
                    &format!("overflow-{index}"),
                    start + Duration::from_secs(1),
                ),
                Err(RegistryError::RateLimited)
            ));
        }
        assert_eq!(limiter.state_counts(), (2, 2, 0));

        limiter
            .charge_at("github", "replacement", start + ACCOUNT_RATE_WINDOW)
            .unwrap();
        assert_eq!(limiter.state_counts(), (1, 1, 1));
    }

    #[test]
    fn account_limiter_keeps_one_expiration_for_a_long_lived_key() {
        let limiter = AccountLimiter::new(RegistrationLimits {
            global_bindings_per_minute: 10,
            account_bindings_per_minute: 10,
            max_account_keys: 1_000,
        })
        .unwrap();
        let start = Instant::now();

        for window in 0..1_000_u32 {
            limiter
                .charge_at("github", "long-lived", start + ACCOUNT_RATE_WINDOW * window)
                .unwrap();
        }

        assert_eq!(limiter.state_counts(), (1, 1, 999));
    }

    #[test]
    fn identity_challenge_debug_redacts_the_one_shot_value() {
        let canary = "identity-challenge-canary-6427";
        let challenge = IdentityChallenge {
            provider: IdentityChallengeProvider::Google,
            value: canary.into(),
            expires_at_ms: 42,
            authorization: IdentityChallengeProvider::Google.authorization("public-client".into()),
        };
        let rendered = format!("{challenge:?}");
        assert!(rendered.contains("Google"));
        assert!(rendered.contains("public-client"));
        assert!(!rendered.contains(canary));
    }
}
