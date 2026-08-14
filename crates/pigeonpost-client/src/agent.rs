//! The agent: identity plus state plus lofts.
//!
//! This is the surface every integration uses (`docs/integration.md`). The CLI and the MCP server
//! are thin shells over it, so there is one implementation of the hard parts.
//!
//! Nothing here runs in the background. `send` queues and flushes, `drain` fetches and stops. An
//! agent wakes, does those, and exits — requirement 7.

use std::collections::{HashMap, HashSet, VecDeque};
use std::fmt;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, LazyLock};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use futures_util::stream::{FuturesUnordered, StreamExt};
use pigeonpost_compliance_format::Jurisdiction;
use pigeonpost_core::{
    envelope,
    keys::SuccessorCommitment,
    record::{validate_loft_list, AGENT_RECORD_VERSION, MAX_AGENT_RECORD_LOFTS},
    Address, AgentRecord, AttributionRequirement, Destination, Identity, RotationRecord,
    UntrustedBody,
};
use pigeonpost_directory::{
    rendezvous, select, verify_snapshot, DirectoryClient, DirectoryEntry, FetchOutcome, Rng,
    SelectionCriteria, TARGET_LOFTS,
};
use pigeonpost_loft::{wire::FetchResponse, LoftClient, LoftEndpoint};
use pigeonpost_registry::{Handle, RegistryClient, RegistryError, RegistryTrust};

use crate::error::{ClientError, Result};
use crate::keystore::{self, KeyPaths};
use crate::spam::{self, Disposition, SenderContext};
use crate::state::{
    validate_token_label, CompletedDelivery, ConfiguredLoft, DeadLetter, DeliveryStatus,
    OutboxEntry, OutboxRecordId, OutboxRoute, OwnRotation, PendingDelivery, PlacementState,
    PublicationTarget, Resolution, State, StorageLimits, StorageStatus, StoredMessage,
};
use crate::trust::{
    RegistryTrustBundle, RegistryTrustInput, RegistryTrustStatus, REGISTRY_TRUST_RESET_CONFIRMATION,
};

/// How many lofts a sender deposits each message with (`docs/network.md`).
pub const PUBLISH_FANOUT: usize = 3;

/// Outbox entries attempted per flush. Bounded so a long backlog does not stall a wake-up.
const FLUSH_BATCH: usize = 64;
const DEFAULT_WAKEUP_CONCURRENCY: usize = 8;
// Keep ordinary delivery wakes short even though registry-audit tools have a separate, longer MCP
// deadline. Callers receive a structured partial report rather than an outer timeout race.
const DEFAULT_WAKEUP_TIMEOUT: Duration = Duration::from_secs(25);
const MAX_WAKEUP_CONCURRENCY: usize = 32;
const MAX_WAKEUP_TIMEOUT: Duration = Duration::from_secs(5 * 60);
/// One default wake-up's worth of worst-case fetch pages. This process-wide budget also caps
/// concurrent drains from multiple Agent homes; raising wake-up concurrency never raises memory.
const MAX_DRAIN_RESPONSE_BYTES: usize =
    DEFAULT_WAKEUP_CONCURRENCY * pigeonpost_loft::client::MAX_FETCH_RESPONSE_BYTES;
const MAX_DRAIN_PAGES_PER_ROUTE: usize = 256;
const MAX_ROTATION_HOPS: usize = keystore::MAX_LIVE_RETIRED_IDENTITIES;
const MAX_RESOLUTION_CANDIDATES: usize = 40;
const MAX_PLACEMENT_ATTEMPTS_PER_WAKE: usize = 16;
const COMPLETED_OUTBOX_RETENTION_SECS: u64 = 30 * 24 * 60 * 60;
const OUTBOX_PRUNE_BATCH: usize = 64;
const PLACEMENT_PUBLICATION_CONCURRENCY: usize = 4;
const RENDEZVOUS_TIER: usize = 3;
const RENDEZVOUS_WALK: usize = 12;
const RESOLUTION_TOTAL_TIMEOUT: Duration = Duration::from_secs(20);
const POW_MINING_BUDGET: Duration = Duration::from_secs(10);
const POW_MINING_MAX_ATTEMPTS: u64 = 8_000_000;
const POW_CANCELLATION_POLL_INTERVAL: u64 = 1_024;
const MAX_CONCURRENT_POW_MINERS: usize = 2;
static POW_MINING_SLOTS: LazyLock<Arc<tokio::sync::Semaphore>> =
    LazyLock::new(|| Arc::new(tokio::sync::Semaphore::new(MAX_CONCURRENT_POW_MINERS)));
static DRAIN_RESPONSE_BUDGET: LazyLock<Arc<tokio::sync::Semaphore>> =
    LazyLock::new(|| Arc::new(tokio::sync::Semaphore::new(MAX_DRAIN_RESPONSE_BYTES)));

/// Limit a retired identity's historical loft list to routes that are still active or within
/// their locally persisted removal-retention window. Retired key custody lasts longer than some
/// loft retention contracts, so the historical rotation record must not resurrect an expired
/// route after [`State::lofts_for_drain_with_local_trust`] has removed it.
fn retired_drain_targets(historical: &[String], current: &[ConfiguredLoft]) -> Vec<ConfiguredLoft> {
    historical
        .iter()
        .filter_map(|url| {
            current
                .iter()
                .find(|(known, _, _)| known == url)
                .map(|(_, key, allow_local)| (url.clone(), *key, *allow_local))
        })
        .collect()
}

/// Stable key-custody options used every time an agent home is opened.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct AgentOpenOptions {
    /// Existing canonical owner-only directory holding `successor.key` and its staged replacement.
    /// The operating identity, token secret, state, journals, lock, and retired keys stay in home.
    pub recovery_dir: Option<PathBuf>,
}

pub struct Agent {
    identity: Identity,
    successor: SuccessorCommitment,
    token_secret: [u8; 32],
    state: State,
    home: PathBuf,
    key_paths: KeyPaths,
    open_options: AgentOpenOptions,
    /// Set when the keys were just created, so the caller can surface first-run advice.
    pub freshly_created: bool,
}

/// Operation-scoped ownership of the active signing identity.
///
/// The underlying process lease stays held until this value is dropped. Callers that publish an
/// externally durable statement must retain it from signature construction through confirmation
/// of that statement, preventing a concurrent rotation from retiring the signing key mid-flight.
pub struct IdentityOperation<'a> {
    identity: &'a Identity,
    _lease: keystore::ActiveIdentityLease,
}

impl fmt::Debug for IdentityOperation<'_> {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("IdentityOperation")
            .field("address", &self.identity.address())
            .finish_non_exhaustive()
    }
}

impl IdentityOperation<'_> {
    pub fn address(&self) -> Address {
        self.identity.address()
    }

    pub fn verifying_key(&self) -> ed25519_dalek::VerifyingKey {
        self.identity.verifying_key()
    }

    /// Sign a caller-domain-separated statement while the active identity remains leased.
    pub fn sign(&self, message: &[u8]) -> ed25519_dalek::Signature {
        self.identity.sign(message)
    }
}

#[derive(Debug, Clone)]
struct LoftCandidate {
    url: String,
    allow_local: bool,
}

struct DirectoryPool {
    entries: Vec<DirectoryEntry>,
    locally_trusted: HashSet<String>,
}

/// What one `send` did.
#[derive(Debug, Clone)]
pub struct SendReport {
    pub message_id: String,
    pub delivered: usize,
    pub queued: usize,
    pub terminal: usize,
    pub deadline_exceeded: bool,
}

/// Bounded execution policy for one foreground flush or drain wake-up.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct WakeupLimits {
    pub max_concurrency: usize,
    pub timeout: Duration,
}

impl WakeupLimits {
    pub fn new(max_concurrency: usize, timeout: Duration) -> Result<Self> {
        let limits = Self {
            max_concurrency,
            timeout,
        };
        limits.validate()?;
        Ok(limits)
    }

    fn validate(self) -> Result<()> {
        if self.max_concurrency == 0
            || self.max_concurrency > MAX_WAKEUP_CONCURRENCY
            || self.timeout.is_zero()
            || self.timeout > MAX_WAKEUP_TIMEOUT
        {
            return Err(ClientError::Config(
                "wakeup concurrency or timeout is outside the allowed bounds".into(),
            ));
        }
        Ok(())
    }
}

impl Default for WakeupLimits {
    fn default() -> Self {
        Self {
            max_concurrency: DEFAULT_WAKEUP_CONCURRENCY,
            timeout: DEFAULT_WAKEUP_TIMEOUT,
        }
    }
}

/// What one bounded outbox wake-up did.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct FlushReport {
    pub attempted: usize,
    pub delivered: usize,
    pub retryable: usize,
    pub terminal: usize,
    /// In-flight requests cancelled when the whole-wakeup deadline elapsed.
    pub cancelled: usize,
    /// Total retryable copies still owed, including delayed and unattempted copies.
    pub queued: u64,
    /// Total terminal copies requiring operator attention.
    pub dead_letters: u64,
    pub deadline_exceeded: bool,
}

#[derive(Debug, Clone)]
pub struct RotationReport {
    pub from: Address,
    pub to: Address,
    pub grace_until: u64,
    pub published: usize,
    pub failed: usize,
    /// True when the supplied source had already been promoted and this call resumed its exact
    /// durable transition instead of rotating the current key again.
    pub resumed: bool,
}

/// What one `drain` did.
#[derive(Debug, Clone, Default)]
pub struct DrainReport {
    pub fetched: usize,
    pub new_messages: usize,
    pub duplicates: usize,
    pub undecryptable: usize,
    /// Held for review because the sender is unknown (`acceptAll = false`).
    pub pending: usize,
    /// Discarded at unwrap: the operator has flagged this sender repeatedly.
    pub dropped: usize,
    pub lofts_failed: Vec<String>,
    /// True when the whole wake-up budget elapsed. Pending requests were cancelled before return.
    pub deadline_exceeded: bool,
}

#[derive(Debug, Clone)]
struct DrainWork {
    url: String,
    pubkey: Option<[u8; 32]>,
    allow_local: bool,
    cursor: u64,
    pages: usize,
}

/// Keeps the full response reservation alive until its decoded events have been processed and the
/// durable cursor has advanced. A completed future cannot leave an unaccounted response buffered
/// inside `FuturesUnordered`.
struct BudgetedFetchResponse {
    response: FetchResponse,
    permit: tokio::sync::OwnedSemaphorePermit,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum DeliveryDisposition {
    Retryable,
    Terminal,
}

impl Agent {
    /// Open (or create) the agent rooted at `home`.
    pub fn open(home: &Path) -> Result<Self> {
        Self::open_with_options(home, AgentOpenOptions::default())
    }

    /// Open an agent with an explicit, stable successor-key custody layout.
    pub fn open_with_options(home: &Path, options: AgentOpenOptions) -> Result<Self> {
        keystore::require_supported_persistent_storage()?;
        secure_agent_home(home)?;
        let paths = match options.recovery_dir.as_deref() {
            Some(recovery_dir) => KeyPaths::in_dir_with_recovery_dir(home, recovery_dir)?,
            None => KeyPaths::in_dir(home),
        };
        let loaded = keystore::load_or_create(&paths)?;
        let state = State::open(&home.join("state.db"))?;
        state.finalize_expired_lofts(now())?;
        state.adopt_legacy_cursors(&loaded.identity.address())?;
        for retired in &loaded.retired {
            state.ensure_record_seq_at_least(retired.target_record.seq)?;
            state.save_own_rotation(
                &retired.record,
                &retired.source_record,
                &retired.target_record,
                &retired.lofts,
            )?;
        }

        Ok(Agent {
            identity: loaded.identity,
            successor: loaded.successor,
            token_secret: loaded.token_secret,
            state,
            home: home.to_path_buf(),
            key_paths: paths,
            open_options: options,
            freshly_created: loaded.freshly_created,
        })
    }

    // ---- spam control ---------------------------------------------------------------------

    pub fn accept_all(&self) -> Result<bool> {
        self.state.accept_all()
    }

    /// Proof-of-work this agent asks of unsolicited mail.
    pub fn pow_floor(&self) -> Result<u32> {
        self.state.pow_floor()
    }

    /// Demand proof-of-work from unsolicited senders.
    ///
    /// Two things have to happen together, or the floor is either unenforced or unpayable: the
    /// lofts are told to enforce it, and the signed agent record advertises it so senders can pay
    /// it up front rather than being rejected and told to retry.
    pub async fn set_pow_floor(&self, bits: u32) -> Result<()> {
        let identity_lease = self.active_identity_lease()?;
        self.state.set_pow_floor(bits)?;
        self.sync_policy_with_lease(&identity_lease).await?;
        self.publish_record_with_lease(&identity_lease).await
    }

    /// Require attribution under one exact jurisdiction and stable custody authority.
    ///
    /// Policies reach every active/draining Loft first. Only a complete policy update is followed
    /// by the signed AgentRecord, so senders never learn a requirement that a responsive Loft was
    /// not already told to enforce. Partial external updates remain visible as an error and are
    /// retried; the local desired state is retained.
    pub async fn set_attribution_requirement(
        &self,
        requirement: Option<AttributionRequirement>,
    ) -> Result<()> {
        if let Some(requirement) = requirement {
            if self.state.registry_configuration()?.is_none() {
                return Err(ClientError::Config(
                    "recipient attribution requires configured witnessed registry trust".into(),
                ));
            }
            self.refresh_compliance_keys().await?;
            let checked_at = now();
            if self
                .state
                .current_attribution_key(
                    &requirement,
                    checked_at.saturating_mul(1_000),
                    checked_at,
                )?
                .is_none()
            {
                return Err(ClientError::Config(
                    "recipient attribution scope has no fresh witnessed Active key".into(),
                ));
            }
        }
        let identity_lease = self.active_identity_lease()?;
        self.state
            .set_recipient_attribution_requirement(requirement)?;
        self.sync_policy_with_lease(&identity_lease).await?;
        self.publish_record_with_lease(&identity_lease).await
    }

    /// Compatibility surface. Enabling a bare boolean is unsafe and therefore rejected.
    pub async fn set_attribution_required(&self, required: bool) -> Result<()> {
        let identity_lease = self.active_identity_lease()?;
        self.state.set_attribution_required(required)?;
        self.sync_policy_with_lease(&identity_lease).await?;
        self.publish_record_with_lease(&identity_lease).await
    }

    pub fn attribution_required(&self) -> Result<bool> {
        self.state.attribution_required()
    }

    pub fn attribution_requirement(&self) -> Result<Option<AttributionRequirement>> {
        self.state.recipient_attribution_requirement()
    }

    /// Open or close the inbox to strangers.
    pub fn set_accept_all(&self, value: bool) -> Result<()> {
        self.state.set_accept_all(value)
    }

    /// Flag a message as spam: score down its sender and remove any allowlist entry.
    ///
    /// Flags on distinct messages eventually cross the drop threshold, after which that sender's
    /// mail is discarded at unwrap. Retrying the same message id is idempotent. The score is
    /// local, so the spammer learns nothing.
    pub fn mark_spam(&self, id: &str) -> Result<i64> {
        let message = self.read(id)?;
        self.state
            .mark_spam(&message.id, &message.from_pubkey, now())
    }

    /// Allowlist a sender and accept everything of theirs currently held pending.
    pub fn allow_sender(&self, pubkey: &[u8; 32], reason: &str) -> Result<usize> {
        self.state.allow_sender(pubkey, reason, now())
    }

    pub fn block_sender(&self, pubkey: &[u8; 32]) -> Result<i64> {
        self.state.block_sender(pubkey, now())
    }

    /// Mail held for review because its sender is unknown.
    pub fn pending(&self, limit: usize) -> Result<Vec<StoredMessage>> {
        self.state.pending_messages(limit)
    }

    /// O(1) persisted local-state limits and exact usage counters.
    pub fn storage_status(&self) -> Result<StorageStatus> {
        self.state.storage_status()
    }

    /// Atomically replace bounded local-state limits. Existing data is never removed.
    pub fn set_storage_limits(&self, limits: StorageLimits) -> Result<StorageStatus> {
        self.state.set_storage_limits(limits)
    }

    // ---- capability tokens ----------------------------------------------------------------

    /// The secret every capability token is derived from.
    ///
    /// Stored independently from the operating identity so identity rotation does not revoke every
    /// capability token. Existing installations migrate the old identity-derived value once.
    pub fn token_secret(&self) -> Result<[u8; 32]> {
        Ok(self.token_secret)
    }

    /// Publish a token, so mail carrying it is accepted at an otherwise closed inbox.
    ///
    /// The presentation registered with each loft is bound to *that loft's* key, so one loft
    /// seeing a token cannot replay it at the others (`docs/spam.md`).
    pub async fn publish_token(&self, label: &str) -> Result<()> {
        validate_token_label(label)?;
        let identity_lease = self.active_identity_lease()?;
        let mut labels = self.token_labels()?;
        if !labels.contains(&label.to_string()) {
            if labels.len() >= pigeonpost_core::policy::MAX_TOKENS {
                return Err(ClientError::Core(pigeonpost_core::Error::TooLarge));
            }
            labels.push(label.to_string());
        }
        self.state.set_token_gate_enabled(true)?;
        self.sync_tokens_with_lease(&identity_lease, &labels).await
    }

    pub async fn revoke_token(&self, label: &str) -> Result<()> {
        validate_token_label(label)?;
        let identity_lease = self.active_identity_lease()?;
        let labels: Vec<String> = self
            .token_labels()?
            .into_iter()
            .filter(|l| l != label)
            .collect();
        self.sync_tokens_with_lease(&identity_lease, &labels).await
    }

    /// Disable or re-enable the loft token gate without discarding the locally retained labels.
    /// Revoking the final token deliberately leaves an enabled gate with an empty live set (deny
    /// all) until the operator explicitly disables it here.
    pub async fn set_token_gate(&self, enabled: bool) -> Result<()> {
        let identity_lease = self.active_identity_lease()?;
        self.state.set_token_gate_enabled(enabled)?;
        self.sync_policy_with_lease(&identity_lease).await
    }

    pub fn token_labels(&self) -> Result<Vec<String>> {
        self.state.token_labels()
    }

    /// Push the live token set to every loft, each hash bound to that loft's key.
    async fn sync_tokens_with_lease(
        &self,
        identity_lease: &keystore::ActiveIdentityLease,
        labels: &[String],
    ) -> Result<()> {
        self.state.set_token_labels(labels)?;
        self.sync_policy_with_lease(identity_lease).await
    }

    /// Compose every signed policy field from durable local state. Updating one setting must never
    /// silently clear another security gate.
    async fn sync_policy(&self) -> Result<()> {
        let identity_lease = self.active_identity_lease()?;
        self.sync_policy_with_lease(&identity_lease).await
    }

    async fn sync_policy_with_lease(
        &self,
        _identity_lease: &keystore::ActiveIdentityLease,
    ) -> Result<()> {
        use pigeonpost_core::Token;

        // Parse every persisted security input before incrementing a sequence, touching a route, or
        // constructing a signed policy. Present corruption is never normalized or republished.
        let security = self.state.security_settings()?;
        let labels = security.token_labels;
        let secret = self.token_secret()?;
        let seq = self.state.next_policy_seq()?;
        let mut failures = 0usize;
        // A loft removed from the advertised AgentRecord can still receive copies from stale
        // sender caches until its absolute drain deadline. Keep every admission gate synchronized
        // for that whole grace interval; the state query atomically deletes expired routes first,
        // so a policy update cannot revive one after the deadline.
        let lofts = self.state.lofts_for_drain_with_local_trust(now())?;
        let total = lofts.len();
        // A policy that reached no loft while lofts are still configured is not in force anywhere,
        // and reporting success for it is the worst possible answer: the caller believes a spam
        // floor, a token requirement or an attribution requirement protects them while every loft
        // still applies the permissive default and accepts unstamped mail.
        //
        // Having *no* lofts is different and legitimate — a removed or fully expired route leaves
        // nothing to tell and nothing to deliver through, which is the drain-to-expiry case.
        if total == 0 && !self.state.lofts_with_local_trust()?.is_empty() {
            return Err(ClientError::PolicyIncomplete {
                succeeded: 0,
                total: self.state.lofts_with_local_trust()?.len(),
            });
        }

        for (url, loft_pubkey, allow_local) in lofts {
            let Some(loft_pubkey) = loft_pubkey else {
                failures += 1;
                continue;
            };
            let hashes: Vec<[u8; 32]> = labels
                .iter()
                .map(|label| {
                    Token::mint(&secret, label)
                        .presentation(&loft_pubkey, &url)
                        .map(|presentation| *presentation.as_bytes())
                })
                .collect::<pigeonpost_core::Result<Vec<_>>>()?;

            let policy = pigeonpost_core::RecipientPolicy::with_attribution_requirement(
                &self.identity,
                security.pow_floor,
                security.token_gate_enabled,
                hashes,
                seq,
                security.attribution_requirement,
            );
            let failed = match loft_client_for_route(&url, allow_local).await {
                Ok(client) => client.set_policy(&policy).await.is_err(),
                Err(_) => true,
            };
            if failed {
                failures += 1;
            }
        }
        if failures > 0 {
            tracing::warn!(failures, "recipient policy remains pending at some lofts");
            return Err(ClientError::PolicyIncomplete {
                succeeded: total - failures,
                total,
            });
        }
        Ok(())
    }

    pub fn address(&self) -> Address {
        self.identity.address()
    }

    pub fn state(&self) -> &State {
        &self.state
    }

    pub fn home(&self) -> &Path {
        &self.home
    }

    pub fn open_options(&self) -> &AgentOpenOptions {
        &self.open_options
    }

    /// This agent's public key, for callers that need to bind it somewhere (a handle claim).
    pub fn verifying_key(&self) -> ed25519_dalek::VerifyingKey {
        self.identity.verifying_key()
    }

    /// Sign arbitrary bytes with the agent's key. Used to prove key possession when claiming a
    /// handle; the payload is always domain-separated by the caller.
    pub fn sign(&self, message: &[u8]) -> Result<ed25519_dalek::Signature> {
        Ok(self.identity_operation()?.sign(message))
    }

    /// Lease the current identity for one complete externally durable signing operation.
    pub fn identity_operation(&self) -> Result<IdentityOperation<'_>> {
        Ok(IdentityOperation {
            identity: &self.identity,
            _lease: self.active_identity_lease()?,
        })
    }

    fn active_identity_lease(&self) -> Result<keystore::ActiveIdentityLease> {
        keystore::acquire_active_identity_lease(
            &self.key_paths,
            self.identity.verifying_key().as_bytes(),
        )
    }

    pub fn successor_shares_a_disk(&self) -> bool {
        keystore::successor_shares_a_disk(&self.key_paths)
    }

    /// Move to the precommitted successor and publish the authenticated transition.
    ///
    /// A fresh source record is first accepted by at least one loft. Only then are local keys
    /// promoted. The resulting signed source/target/rotation bundle is durable and every later
    /// wake-up may retry partial publication without resigning or changing its sequence.
    pub async fn rotate(&mut self) -> Result<RotationReport> {
        let expected_source = self.address();
        self.rotate_expected(&expected_source).await
    }

    /// Rotate only from `expected_source`, or resume the exact journaled transition when that
    /// source was already promoted. This is the safe retry surface for operator confirmation: a
    /// repeated command can repair zero-ack publication without ever rotating the successor twice.
    pub async fn rotate_expected(&mut self, expected_source: &Address) -> Result<RotationReport> {
        self.reconcile_retired_rotation_journals()?;
        let current = self.address();
        if current == *expected_source {
            if self.state.own_rotation(expected_source)?.is_some() {
                return Err(ClientError::Config(
                    "rotation journal targets another identity but the source is still active"
                        .into(),
                ));
            }
            return self.rotate_current().await;
        }

        let rotation = self.state.own_rotation(expected_source)?.ok_or_else(|| {
            ClientError::Config(
                "confirmed rotation source is neither active nor a journaled predecessor".into(),
            )
        })?;
        if rotation.record.target_address()? != current {
            return Err(ClientError::Config(
                "confirmed rotation source does not target the active identity".into(),
            ));
        }
        self.resume_expected_rotation(expected_source, rotation)
            .await
    }

    fn reconcile_retired_rotation_journals(&self) -> Result<()> {
        self.state
            .prune_expired_own_rotations(now(), keystore::MAX_LIVE_RETIRED_IDENTITIES)?;
        let identity_lease = self.active_identity_lease()?;
        for retired in
            keystore::retired_identities_with_lease(&self.key_paths, &identity_lease, now())?
        {
            self.state
                .ensure_record_seq_at_least(retired.target_record.seq)?;
            self.state.save_own_rotation(
                &retired.record,
                &retired.source_record,
                &retired.target_record,
                &retired.lofts,
            )?;
        }
        Ok(())
    }

    async fn resume_expected_rotation(
        &self,
        from: &Address,
        own: OwnRotation,
    ) -> Result<RotationReport> {
        let to = own.record.target_address()?;
        let attempted_at = now();
        let deadline = tokio::time::Instant::now() + DEFAULT_WAKEUP_TIMEOUT;
        let mut remaining = MAX_PLACEMENT_ATTEMPTS_PER_WAKE;

        // Retry the exact durable transition before consulting directory membership. An expired
        // snapshot plus an offline directory must never erase or block previously admitted debt.
        remaining = remaining.saturating_sub(
            self.attempt_rotation_publication_until(deadline, remaining)
                .await?,
        );
        remaining = remaining.saturating_sub(
            self.attempt_record_publication_until(deadline, remaining)
                .await?,
        );

        // Reconcile only after at least one positively verified, non-degraded refresh. Empty or
        // partially unavailable membership cannot shrink an exact publication journal.
        if remaining > 0 && tokio::time::Instant::now() < deadline {
            let configured_directories = self.state.directories()?.len();
            if configured_directories > 0 {
                let refresh =
                    tokio::time::timeout_at(deadline, self.refresh_directories_with_status()).await;
                match refresh {
                    Ok(Ok((refreshed, 0))) if refreshed > 0 => {
                        let pool = self.directory_pool(false).await?;
                        if pool.entries.is_empty() {
                            self.state.set_placement_health(true, attempted_at)?;
                        } else {
                            self.state.set_placement_health(false, attempted_at)?;
                            let target_targets = self.publication_targets_from_pool(
                                &to,
                                &own.target_record.lofts,
                                &pool,
                            )?;
                            self.state.save_record_publication(
                                &to,
                                &own.target_record,
                                &target_targets,
                                attempted_at,
                            )?;
                            self.sync_rotation_publication_targets(&pool)?;
                            remaining = remaining.saturating_sub(
                                self.attempt_rotation_publication_until(deadline, remaining)
                                    .await?,
                            );
                            if remaining > 0 && tokio::time::Instant::now() < deadline {
                                self.attempt_record_publication_until(deadline, remaining)
                                    .await?;
                            }
                        }
                    }
                    Ok(Ok(_)) | Err(_) => {
                        self.state.set_placement_health(true, attempted_at)?;
                    }
                    Ok(Err(error)) => {
                        self.state.set_placement_health(true, attempted_at)?;
                        return Err(error);
                    }
                }
            }
        }
        let (published, failed) = self.state.rotation_target_progress(from)?;
        if published == 0 {
            return Err(ClientError::Undeliverable);
        }
        if let Err(error) = self.sync_policy().await {
            tracing::warn!(
                error_class = drain_error_class(&error),
                "rotated recipient policy remains pending"
            );
        }
        Ok(RotationReport {
            from: from.clone(),
            to,
            grace_until: own.record.grace_until,
            published,
            failed,
            resumed: true,
        })
    }

    async fn rotate_current(&mut self) -> Result<RotationReport> {
        // This one lease spans source-record construction/publication through durable key
        // promotion. A second process therefore cannot publish a competing transition from the
        // same cached source key.
        let identity_lease = self.active_identity_lease()?;
        let configured: Vec<String> = self
            .state
            .lofts_with_local_trust()?
            .into_iter()
            .map(|(url, _, _)| url)
            .collect();
        if configured.is_empty() {
            return Err(ClientError::NoLofts);
        }
        validate_loft_list(&configured)?;

        let from = self.address();
        let pow_floor = self.pow_floor()?;
        let attribution_requirement = self.state.recipient_attribution_requirement()?;
        let existing_source = self.state.record_publication()?.filter(|publication| {
            publication.address == from
                && publication.record.version == AGENT_RECORD_VERSION
                && publication.record.pubkey == self.identity.verifying_key().to_bytes()
                && publication.record.successor_hash == *self.successor.as_bytes()
                && publication.record.lofts == configured
                && publication.record.pow_min == pow_floor
                && publication.record.attribution_requirement == attribution_requirement
        });
        let source_record = match existing_source {
            Some(publication) => publication.record,
            None => AgentRecord::with_policy(
                &self.identity,
                &self.successor,
                self.state.next_record_seq()?,
                configured.clone(),
                pow_floor,
                attribution_requirement,
            ),
        };
        let attempted_at = now();
        let (_, refresh_failures) = self.refresh_directories_with_status().await?;
        self.state
            .set_placement_health(refresh_failures > 0, attempted_at)?;
        let pool = self.directory_pool(false).await?;
        let source_targets = self.publication_targets_from_pool(&from, &configured, &pool)?;
        self.state
            .save_record_publication(&from, &source_record, &source_targets, attempted_at)?;
        let source_deadline = tokio::time::Instant::now() + DEFAULT_WAKEUP_TIMEOUT;
        self.attempt_record_publication_until(source_deadline, MAX_PLACEMENT_ATTEMPTS_PER_WAKE)
            .await?;
        let source_successes: Vec<String> = self
            .state
            .record_publication()?
            .into_iter()
            .flat_map(|publication| publication.targets)
            .filter(|target| target.completed)
            .map(|target| target.url)
            .collect();
        if source_successes.is_empty() {
            return Err(ClientError::Undeliverable);
        }

        let outcome = keystore::rotate_with_lease(
            &self.key_paths,
            &identity_lease,
            &source_record,
            &source_successes,
            now(),
        )?;
        self.identity = outcome.identity;
        self.successor = outcome.successor;
        self.state.ensure_record_seq_at_least(outcome.record.seq)?;
        drop(identity_lease);

        let to = self.address();
        let target_targets = self.publication_targets_from_pool(&to, &configured, &pool)?;
        let mut rotation_targets = source_targets;
        for target in &target_targets {
            merge_publication_target(
                &mut rotation_targets,
                target.url.clone(),
                target.allow_local,
                target.rendezvous,
            );
        }
        let target_urls: Vec<String> = rotation_targets
            .iter()
            .map(|target| target.url.clone())
            .collect();
        self.state.save_own_rotation(
            &outcome.record,
            &outcome.source_record,
            &outcome.target_record,
            &target_urls,
        )?;
        self.state
            .sync_own_rotation_targets(&from, &rotation_targets)?;
        self.state
            .save_record_publication(&to, &outcome.target_record, &target_targets, now())?;
        let own = OwnRotation {
            record: outcome.record,
            source_record: outcome.source_record,
            target_record: outcome.target_record,
            lofts: target_urls,
        };
        let publication_deadline = tokio::time::Instant::now() + DEFAULT_WAKEUP_TIMEOUT;
        let attempted = self
            .attempt_rotation_publication_until(
                publication_deadline,
                MAX_PLACEMENT_ATTEMPTS_PER_WAKE,
            )
            .await?;
        if attempted < MAX_PLACEMENT_ATTEMPTS_PER_WAKE
            && tokio::time::Instant::now() < publication_deadline
        {
            self.attempt_record_publication_until(
                publication_deadline,
                MAX_PLACEMENT_ATTEMPTS_PER_WAKE - attempted,
            )
            .await?;
        }
        let (published, failed) = self.state.rotation_target_progress(&from)?;
        if let Err(error) = self.sync_policy().await {
            tracing::warn!(
                error_class = drain_error_class(&error),
                "rotated recipient policy remains pending"
            );
        }
        if published == 0 {
            return Err(ClientError::Undeliverable);
        }
        Ok(RotationReport {
            from,
            to,
            grace_until: own.record.grace_until,
            published,
            failed,
            resumed: false,
        })
    }

    // ---- lofts ----------------------------------------------------------------------------

    /// Add a loft and publish our record to it, so senders can find us there.
    ///
    /// The record is republished to *every* loft, not just the new one: the loft list inside it
    /// has changed, and a stale copy elsewhere would send mail to a loft we no longer read.
    pub async fn add_loft(&self, url: &str) -> Result<()> {
        let (client, allow_local) = match LoftEndpoint::explicit(url) {
            Ok(explicit) => (LoftClient::from_endpoint(explicit)?, true),
            Err(_) => (LoftClient::new_untrusted(url).await?, false),
        };
        let canonical = client.base_url().to_owned();
        let configured = self.state.lofts()?;
        if !configured.iter().any(|(known, _)| known == &canonical)
            && configured.len() >= MAX_AGENT_RECORD_LOFTS
        {
            return Err(ClientError::Core(pigeonpost_core::Error::TooLarge));
        }
        let info = client.info().await?;
        let pubkey = parse_hex32(&info.pubkey)
            .ok_or_else(|| ClientError::Config(format!("{url} returned a malformed pubkey")))?;

        let identity_lease = self.active_identity_lease()?;
        self.state.add_loft_with_retention_and_local_trust(
            &canonical,
            Some(pubkey),
            info.retention_days,
            now(),
            allow_local,
        )?;
        self.sync_policy_with_lease(&identity_lease).await?;
        let publication = self.publish_record_with_lease(&identity_lease).await;
        drop(identity_lease);
        publication
    }

    /// Stop advertising a loft now, while continuing to collect copies routed by stale sender
    /// caches through the bounded replacement grace window.
    pub async fn remove_loft(&self, url: &str) -> Result<bool> {
        let parsed =
            url::Url::parse(url).map_err(|_| ClientError::Config("loft URL is invalid".into()))?;
        let canonical = parsed.as_str().trim_end_matches('/').to_owned();
        validate_loft_list(std::slice::from_ref(&canonical))?;
        let identity_lease = self.active_identity_lease()?;
        let removed = self.state.remove_loft(&canonical, now())?;
        if removed {
            // Publication targets include the draining loft itself, but the signed routing list
            // does not. Cached senders can still deposit there; newly resolving senders move on.
            self.publish_record_with_lease(&identity_lease).await?;
        }
        Ok(removed)
    }

    pub fn lofts(&self) -> Result<Vec<(String, Option<[u8; 32]>)>> {
        self.state.lofts()
    }

    // ---- trusted handle registry ----------------------------------------------------------

    /// Install an out-of-band registry trust policy. The URL is validated before anything is
    /// persisted, and subsequent changes require [`Self::reset_registry_trust`] explicitly.
    pub fn configure_registry(&self, url: &str, trust: RegistryTrust) -> Result<()> {
        let bundle = RegistryTrustBundle::from_registry_trust(url, &trust)?;
        self.persist_registry_trust(&bundle)
    }

    /// Validate and durably install a complete out-of-band registry trust bundle.
    ///
    /// The first import is idempotent. Any later change fails until the operator calls
    /// [`Self::reset_registry_trust`] with the exact confirmation phrase.
    pub fn import_registry_trust(&self, input: RegistryTrustInput) -> Result<RegistryTrustStatus> {
        let bundle = RegistryTrustBundle::try_from(input)?;
        self.persist_registry_trust(&bundle)?;
        self.registry_trust_status()?
            .ok_or_else(|| ClientError::Config("registry trust was not durably installed".into()))
    }

    fn persist_registry_trust(&self, bundle: &RegistryTrustBundle) -> Result<()> {
        let trust = bundle.to_registry_trust()?;
        RegistryClient::new(bundle.registry_url(), trust.clone())?;
        self.state
            .configure_registry(bundle.registry_url(), &trust, now())
    }

    /// Return the exact public trust anchors and newest accepted witnessed checkpoint.
    pub fn registry_trust_status(&self) -> Result<Option<RegistryTrustStatus>> {
        self.state
            .registry_configuration()?
            .map(|configuration| {
                let bundle = RegistryTrustBundle::from_registry_trust(
                    &configuration.url,
                    &configuration.trust,
                )?;
                Ok(RegistryTrustStatus::new(
                    bundle,
                    configuration.checkpoint.as_ref(),
                    configuration.witnessed_at,
                    now(),
                ))
            })
            .transpose()
    }

    /// Compatibility surface for disabling the old jurisdiction-only selector. Enabling without
    /// a stable authority is rejected; use [`Self::set_sender_attribution_requirement`].
    pub fn set_attribution_jurisdiction(&self, jurisdiction: Option<Jurisdiction>) -> Result<()> {
        self.state.set_attribution_jurisdiction(jurisdiction)
    }

    pub fn attribution_jurisdiction(&self) -> Result<Option<Jurisdiction>> {
        self.state.attribution_jurisdiction()
    }

    /// Compatibility alias for the retired jurisdiction-only selector.
    pub fn set_sender_jurisdiction(&self, jurisdiction: Option<Jurisdiction>) -> Result<()> {
        self.set_attribution_jurisdiction(jurisdiction)
    }

    pub fn sender_jurisdiction(&self) -> Result<Option<Jurisdiction>> {
        self.attribution_jurisdiction()
    }

    /// Record explicit sender consent to the complete custody scope. When a resolved recipient
    /// requires attribution, sends proceed only if this value matches exactly.
    pub fn set_sender_attribution_requirement(
        &self,
        requirement: Option<AttributionRequirement>,
    ) -> Result<()> {
        self.state.set_sender_attribution_requirement(requirement)
    }

    pub fn sender_attribution_requirement(&self) -> Result<Option<AttributionRequirement>> {
        self.state.sender_attribution_requirement()
    }

    /// Refresh the complete compliance-key projection under the configured registry trust root.
    pub async fn refresh_compliance_keys(&self) -> Result<usize> {
        let configured = self
            .state
            .registry_configuration()?
            .ok_or_else(|| ClientError::Config("registry is not configured".into()))?;
        if configured.trust.witness_threshold() == 0 {
            return Err(ClientError::Config(
                "compliance keys require a nonzero witness quorum".into(),
            ));
        }
        let client = RegistryClient::new(&configured.url, configured.trust)?;
        let previous = self.state.compliance_audit()?;
        let (verified, audit) = client
            .compliance_keys_audited(
                previous.map(Arc::new),
                configured.checkpoint.as_ref(),
                now(),
            )
            .await?;
        let count = verified.keys().len();
        self.state.save_compliance_keys(&verified, &audit, now())?;
        Ok(count)
    }

    async fn refresh_compliance_keys_if_available(&self) -> Result<()> {
        let Some(configured) = self.state.registry_configuration()? else {
            return Ok(());
        };
        if configured.trust.witness_threshold() == 0 {
            return Ok(());
        }
        match self.refresh_compliance_keys().await {
            Ok(_) => Ok(()),
            Err(ClientError::Registry(
                RegistryError::RegistryUnavailable
                | RegistryError::RateLimited
                | RegistryError::Overloaded,
            )) => Ok(()),
            Err(error) => Err(error),
        }
    }

    /// Remove the registry trust root and all state learned through it after explicit confirmation.
    pub fn reset_registry_trust(&self, confirmation: &str) -> Result<()> {
        if confirmation != REGISTRY_TRUST_RESET_CONFIRMATION {
            return Err(ClientError::Config(format!(
                "registry trust reset requires confirmation `{REGISTRY_TRUST_RESET_CONFIRMATION}`"
            )));
        }
        self.state.reset_registry()
    }

    // ---- signed directory discovery -------------------------------------------------------

    /// Add a directory with an out-of-band pinned signing key and immediately verify a snapshot.
    pub async fn add_directory(&self, url: &str, signing_key: [u8; 32]) -> Result<usize> {
        let client = DirectoryClient::new(url, signing_key)?;
        let inserted = self.state.add_directory(url, &signing_key, now())?;
        let fetched = client.fetch(None, now()).await;
        match fetched {
            Ok(FetchOutcome::Modified { document, etag }) => {
                let count = document.lofts.len();
                if let Err(error) =
                    self.state
                        .save_directory_snapshot(url, &document, etag.as_deref())
                {
                    if inserted {
                        self.state
                            .remove_uninitialized_directory(url, &signing_key)?;
                    }
                    return Err(error);
                }
                Ok(count)
            }
            Ok(FetchOutcome::NotModified) => {
                if inserted {
                    self.state
                        .remove_uninitialized_directory(url, &signing_key)?;
                }
                Err(ClientError::Config(
                    "directory returned not-modified before a snapshot was cached".into(),
                ))
            }
            Err(error) => {
                if inserted {
                    self.state
                        .remove_uninitialized_directory(url, &signing_key)?;
                }
                Err(error.into())
            }
        }
    }

    /// Remove one exact pinned directory and its cached snapshot so the slot can be reused or its
    /// key can be rolled explicitly.
    pub fn remove_directory(&self, url: &str) -> Result<bool> {
        self.state.remove_directory(url)
    }

    /// Refresh all configured directories. A temporarily unavailable mirror does not erase its
    /// last verified snapshot; stale snapshots are rejected when the pool is assembled.
    pub async fn refresh_directories(&self) -> Result<usize> {
        Ok(self.refresh_directories_with_status().await?.0)
    }

    /// Return successful and failed refresh counts separately so placement health can remain
    /// durable and inspectable without making a directory outage fatal to delivery.
    async fn refresh_directories_with_status(&self) -> Result<(usize, usize)> {
        let directories = self.state.directories()?;
        let mut refreshed = 0usize;
        let mut failures = 0usize;
        for directory in directories {
            let client = DirectoryClient::new(&directory.url, directory.signing_key)?;
            match client.fetch(directory.etag.as_deref(), now()).await {
                Ok(FetchOutcome::Modified { document, etag }) => {
                    self.state.save_directory_snapshot(
                        &directory.url,
                        &document,
                        etag.as_deref(),
                    )?;
                    refreshed += 1;
                }
                Ok(FetchOutcome::NotModified) => refreshed += 1,
                Err(_) => failures += 1,
            }
        }
        if failures > 0 {
            tracing::warn!(
                failures,
                "some signed directory snapshots could not be refreshed"
            );
        }
        Ok((refreshed, failures))
    }

    /// Select enough active, diverse directory lofts to reach the target and publish this agent.
    pub async fn bootstrap_lofts(&self) -> Result<usize> {
        let DirectoryPool {
            entries: pool,
            locally_trusted,
        } = self.directory_pool(true).await?;
        let current = self.state.lofts()?;
        if current.len() >= TARGET_LOFTS {
            return Ok(0);
        }

        let current_urls: HashSet<&str> = current.iter().map(|(url, _)| url.as_str()).collect();
        let current_failure_domains: HashSet<String> = pool
            .iter()
            .filter(|entry| current_urls.contains(entry.endpoint.as_str()))
            .flat_map(DirectoryEntry::failure_domains)
            .collect();
        let candidates: Vec<DirectoryEntry> = pool
            .iter()
            .filter(|entry| !current_urls.contains(entry.endpoint.as_str()))
            .filter(|entry| {
                entry
                    .failure_domains()
                    .iter()
                    .all(|domain| !current_failure_domains.contains(domain))
            })
            .cloned()
            .collect();
        let needed = TARGET_LOFTS - current.len();
        // Rank more candidates than the target so a dead or key-mismatched directory entry does
        // not prevent bootstrap from trying the next independently operated loft.
        let criteria = SelectionCriteria {
            target: candidates.len().min(needed.saturating_mul(3).max(needed)),
            ..SelectionCriteria::default()
        };
        let chosen = select(None, &[], &candidates, &criteria, &mut Rng::from_entropy());
        let mut verified_candidates = Vec::with_capacity(needed);
        let mut failures = 0usize;
        for entry in chosen {
            if verified_candidates.len() >= needed {
                break;
            }
            let allow_local = locally_trusted.contains(&entry.endpoint);
            let verified = async {
                let info = loft_client_for_route(&entry.endpoint, allow_local)
                    .await?
                    .info()
                    .await?;
                let observed = parse_hex32(&info.pubkey).ok_or_else(|| {
                    ClientError::Config("loft returned a malformed public key".into())
                })?;
                let expected =
                    pigeonpost_directory::entry::parse_hex32(&entry.pubkey).ok_or_else(|| {
                        ClientError::Config("directory contains a malformed loft key".into())
                    })?;
                if observed != expected {
                    return Err(ClientError::Config(
                        "loft key does not match its signed directory entry".into(),
                    ));
                }
                Ok((observed, info.retention_days))
            }
            .await;
            let (observed, retention_days) = match verified {
                Ok(observed) => observed,
                Err(_) => {
                    failures += 1;
                    continue;
                }
            };
            verified_candidates.push((entry.clone(), observed, retention_days, allow_local));
        }
        if failures > 0 {
            tracing::warn!(failures, "some directory loft candidates were unavailable");
        }
        if verified_candidates.is_empty() {
            return if self.state.lofts()?.is_empty() {
                Err(ClientError::NoLofts)
            } else {
                Ok(0)
            };
        }

        // Endpoint authentication is intentionally outside the identity lease. Re-read the route
        // set after acquiring it so a concurrent completed mutation cannot make this optimistic
        // selection exceed the target or violate the failure-domain constraint.
        let identity_lease = self.active_identity_lease()?;
        let current = self.state.lofts()?;
        if current.len() >= TARGET_LOFTS {
            return Ok(0);
        }
        let mut current_urls: HashSet<String> = current.into_iter().map(|(url, _)| url).collect();
        let mut current_failure_domains: HashSet<String> = pool
            .iter()
            .filter(|entry| current_urls.contains(&entry.endpoint))
            .flat_map(DirectoryEntry::failure_domains)
            .collect();
        let needed = TARGET_LOFTS - current_urls.len();
        let mut added = 0usize;
        for (entry, observed, retention_days, allow_local) in verified_candidates {
            if added >= needed || current_urls.contains(&entry.endpoint) {
                continue;
            }
            let failure_domains = entry.failure_domains();
            if failure_domains
                .iter()
                .any(|domain| current_failure_domains.contains(domain))
            {
                continue;
            }
            self.state.add_loft_with_retention_and_local_trust(
                &entry.endpoint,
                Some(observed),
                retention_days,
                now(),
                allow_local,
            )?;
            current_urls.insert(entry.endpoint);
            current_failure_domains.extend(failure_domains);
            added += 1;
        }
        if added > 0 {
            self.sync_policy_with_lease(&identity_lease).await?;
            self.publish_record_with_lease(&identity_lease).await?;
        }
        Ok(added)
    }

    async fn directory_pool(&self, refresh: bool) -> Result<DirectoryPool> {
        if refresh {
            self.refresh_directories_with_status().await?;
        }
        let mut by_endpoint: HashMap<String, DirectoryEntry> = HashMap::new();
        let mut conflicts = HashSet::new();
        let mut locally_trusted = HashSet::new();
        for directory in self.state.directories()? {
            let Some(document) = directory.snapshot else {
                continue;
            };
            if verify_snapshot(&document, &directory.signing_key, now()).is_err() {
                continue;
            }
            let local_directory = exact_loopback_directory(&directory.url);
            for entry in document.lofts {
                if conflicts.contains(&entry.endpoint) {
                    continue;
                }
                if local_directory {
                    locally_trusted.insert(entry.endpoint.clone());
                }
                match by_endpoint.get(&entry.endpoint) {
                    Some(existing) if existing.pubkey != entry.pubkey => {
                        by_endpoint.remove(&entry.endpoint);
                        locally_trusted.remove(&entry.endpoint);
                        conflicts.insert(entry.endpoint);
                    }
                    Some(existing)
                        if existing.last_mutation_sequence >= entry.last_mutation_sequence => {}
                    _ => {
                        by_endpoint.insert(entry.endpoint.clone(), entry);
                    }
                }
            }
        }
        locally_trusted.retain(|endpoint| by_endpoint.contains_key(endpoint));
        Ok(DirectoryPool {
            entries: by_endpoint.into_values().collect(),
            locally_trusted,
        })
    }

    /// Publish our signed agent record to all our lofts and deterministic rendezvous lofts.
    ///
    /// The exact signed bytes and every target are committed before the first request. A partial
    /// result is therefore a durable retry plan, not an in-memory warning lost on process exit.
    pub async fn publish_record(&self) -> Result<()> {
        let identity_lease = self.active_identity_lease()?;
        self.publish_record_with_lease(&identity_lease).await
    }

    async fn publish_record_with_lease(
        &self,
        _identity_lease: &keystore::ActiveIdentityLease,
    ) -> Result<()> {
        let attempted_at = now();
        let (_, refresh_failures) = self.refresh_directories_with_status().await?;
        self.state
            .set_placement_health(refresh_failures > 0, attempted_at)?;
        let pool = self.directory_pool(false).await?;
        let publication = self.prepare_record_publication(&pool, attempted_at)?;
        self.sync_rotation_publication_targets(&pool)?;

        let deadline = tokio::time::Instant::now() + DEFAULT_WAKEUP_TIMEOUT;
        let mut remaining = MAX_PLACEMENT_ATTEMPTS_PER_WAKE;
        remaining = remaining.saturating_sub(
            self.attempt_record_publication_until(deadline, remaining)
                .await?,
        );
        self.attempt_rotation_publication_until(deadline, remaining)
            .await?;

        let status = self.state.placement_state()?;
        if status.record_pending > 0 || status.rotation_pending > 0 {
            tracing::warn!(
                record_pending = status.record_pending,
                rotation_pending = status.rotation_pending,
                "signed placement remains pending at some lofts"
            );
        }
        if publication.is_some()
            && status.record_targets > 0
            && status.record_pending == status.record_targets
        {
            return Err(ClientError::Undeliverable);
        }
        Ok(())
    }

    fn prepare_record_publication(
        &self,
        pool: &DirectoryPool,
        attempted_at: u64,
    ) -> Result<Option<crate::state::OwnRecordPublication>> {
        let record_lofts = self.state.lofts_with_local_trust()?;
        let drain_targets = self.state.lofts_for_drain_with_local_trust(attempted_at)?;
        if record_lofts.is_empty() && drain_targets.is_empty() {
            return Ok(None);
        }

        let urls: Vec<String> = record_lofts.iter().map(|(url, _, _)| url.clone()).collect();
        validate_loft_list(&urls)?;
        let address = self.address();
        let existing = self.state.record_publication()?;
        let pow_floor = self.pow_floor()?;
        let attribution_requirement = self.state.recipient_attribution_requirement()?;
        let existing_record = existing.as_ref().filter(|publication| {
            publication.address == address
                && publication.record.version == AGENT_RECORD_VERSION
                && publication.record.pubkey == self.identity.verifying_key().to_bytes()
                && publication.record.successor_hash == *self.successor.as_bytes()
                && publication.record.lofts == urls
                && publication.record.pow_min == pow_floor
                && publication.record.attribution_requirement == attribution_requirement
        });
        let recovered_rotation_record = self
            .state
            .own_rotations()?
            .into_iter()
            .map(|rotation| rotation.target_record)
            .filter(|record| {
                record.pubkey == self.identity.verifying_key().to_bytes()
                    && record.version == AGENT_RECORD_VERSION
                    && record.successor_hash == *self.successor.as_bytes()
                    && record.lofts == urls
                    && record.pow_min == pow_floor
                    && record.attribution_requirement == attribution_requirement
            })
            .max_by_key(|record| record.seq);
        let record = match (existing_record, recovered_rotation_record) {
            (Some(publication), Some(recovered)) if recovered.seq > publication.record.seq => {
                recovered
            }
            (Some(publication), _) => publication.record.clone(),
            (None, Some(recovered)) => recovered,
            (None, None) => AgentRecord::with_policy(
                &self.identity,
                &self.successor,
                self.state.next_record_seq()?,
                urls,
                pow_floor,
                attribution_requirement,
            ),
        };
        let primary: Vec<String> = drain_targets
            .iter()
            .map(|(url, _, _)| url.clone())
            .collect();
        let targets = self.publication_targets_from_pool(&address, &primary, pool)?;
        self.state
            .save_record_publication(&address, &record, &targets, attempted_at)?;
        self.state.record_publication()
    }

    fn publication_targets_from_pool(
        &self,
        address: &Address,
        primary: &[String],
        pool: &DirectoryPool,
    ) -> Result<Vec<PublicationTarget>> {
        let configured_local: HashSet<String> = self
            .state
            .lofts_for_drain_with_local_trust(now())?
            .into_iter()
            .filter_map(|(url, _, allow_local)| allow_local.then_some(url))
            .collect();
        let mut targets = Vec::new();
        for url in primary {
            merge_publication_target(
                &mut targets,
                url.clone(),
                configured_local.contains(url),
                false,
            );
        }
        for entry in rendezvous(&pool.entries, address.as_str(), TARGET_LOFTS) {
            merge_publication_target(
                &mut targets,
                entry.endpoint.clone(),
                pool.locally_trusted.contains(&entry.endpoint),
                true,
            );
        }
        Ok(targets)
    }

    fn sync_rotation_publication_targets(&self, pool: &DirectoryPool) -> Result<()> {
        self.state
            .prune_expired_own_rotations(now(), keystore::MAX_LIVE_RETIRED_IDENTITIES)?;
        let primary: Vec<String> = self
            .state
            .lofts_for_drain_with_local_trust(now())?
            .into_iter()
            .map(|(url, _, _)| url)
            .collect();
        for rotation in self
            .state
            .own_rotations()?
            .into_iter()
            .take(MAX_ROTATION_HOPS)
        {
            let from = Address::from_pubkey(&pigeonpost_core::keys::verifying_key_from_bytes(
                &rotation.record.from_pubkey,
            )?);
            let to = rotation.record.target_address()?;
            let mut targets = self.publication_targets_from_pool(&from, &primary, pool)?;
            for target in self.publication_targets_from_pool(&to, &primary, pool)? {
                merge_publication_target(
                    &mut targets,
                    target.url,
                    target.allow_local,
                    target.rendezvous,
                );
            }
            self.state.sync_own_rotation_targets(&from, &targets)?;
        }
        Ok(())
    }

    async fn attempt_record_publication_until(
        &self,
        deadline: tokio::time::Instant,
        limit: usize,
    ) -> Result<usize> {
        self.attempt_record_publication_with(
            deadline,
            limit,
            |address, record, target| async move {
                match loft_client_for_route(&target.url, target.allow_local).await {
                    Ok(client) => client.put_agent_record(&address, &record).await.is_ok(),
                    Err(_) => false,
                }
            },
        )
        .await
    }

    async fn attempt_record_publication_with<F, Fut>(
        &self,
        deadline: tokio::time::Instant,
        limit: usize,
        publish: F,
    ) -> Result<usize>
    where
        F: Fn(Address, AgentRecord, PublicationTarget) -> Fut,
        Fut: std::future::Future<Output = bool>,
    {
        let Some(publication) = self.state.record_publication()? else {
            return Ok(0);
        };
        let mut queued: VecDeque<_> = publication
            .targets
            .iter()
            .filter(|target| !target.completed)
            .take(limit)
            .cloned()
            .collect();
        let mut in_flight = FuturesUnordered::new();
        let mut attempted = 0usize;
        while !queued.is_empty() || !in_flight.is_empty() {
            while in_flight.len() < PLACEMENT_PUBLICATION_CONCURRENCY {
                let Some(target) = queued.pop_front() else {
                    break;
                };
                let address = publication.address.clone();
                let record = publication.record.clone();
                let retained_target = target.clone();
                let publication = publish(address, record, target);
                in_flight.push(async move { (retained_target, publication.await) });
            }
            let outcome = tokio::select! {
                _ = tokio::time::sleep_until(deadline) => None,
                outcome = in_flight.next() => outcome,
            };
            let Some((target, succeeded)) = outcome else {
                drop(in_flight);
                break;
            };
            attempted += 1;
            if succeeded {
                self.state.mark_record_target_complete(
                    &publication.address,
                    &publication.record,
                    &target.url,
                )?;
            }
        }
        Ok(attempted)
    }

    async fn attempt_rotation_publication_until(
        &self,
        deadline: tokio::time::Instant,
        limit: usize,
    ) -> Result<usize> {
        self.attempt_rotation_publication_with(deadline, limit, |rotation, target| async move {
            let Ok(source_key) =
                pigeonpost_core::keys::verifying_key_from_bytes(&rotation.record.from_pubkey)
            else {
                return false;
            };
            let from = Address::from_pubkey(&source_key);
            let Ok(to) = rotation.record.target_address() else {
                return false;
            };
            let outcome: Result<()> = async {
                let client = loft_client_for_route(&target.url, target.allow_local).await?;
                client
                    .put_agent_record(&from, &rotation.source_record)
                    .await?;
                client
                    .put_agent_record(&to, &rotation.target_record)
                    .await?;
                client.put_rotation_record(&from, &rotation.record).await?;
                Ok(())
            }
            .await;
            outcome.is_ok()
        })
        .await
    }

    async fn attempt_rotation_publication_with<F, Fut>(
        &self,
        deadline: tokio::time::Instant,
        limit: usize,
        publish: F,
    ) -> Result<usize>
    where
        F: Fn(OwnRotation, PublicationTarget) -> Fut,
        Fut: std::future::Future<Output = bool>,
    {
        let mut queued: VecDeque<_> = self
            .state
            .pending_rotation_targets(limit)?
            .into_iter()
            .collect();
        let mut in_flight = FuturesUnordered::new();
        let mut attempted = 0usize;
        while !queued.is_empty() || !in_flight.is_empty() {
            while in_flight.len() < PLACEMENT_PUBLICATION_CONCURRENCY {
                let Some((rotation, target)) = queued.pop_front() else {
                    break;
                };
                let retained_rotation = rotation.clone();
                let retained_target = target.clone();
                let publication = publish(rotation, target);
                in_flight
                    .push(async move { (retained_rotation, retained_target, publication.await) });
            }
            let outcome = tokio::select! {
                _ = tokio::time::sleep_until(deadline) => None,
                outcome = in_flight.next() => outcome,
            };
            let Some((rotation, target, succeeded)) = outcome else {
                drop(in_flight);
                break;
            };
            attempted += 1;
            if succeeded {
                let from = Address::from_pubkey(&pigeonpost_core::keys::verifying_key_from_bytes(
                    &rotation.record.from_pubkey,
                )?);
                let to = rotation.record.target_address()?;
                self.state
                    .mark_rotation_target_complete(&from, &target.url)?;
                // The bundle's target-record PUT may also satisfy the active record plan. The
                // exact-byte guard makes this a no-op after another rotation or record change.
                self.state.mark_record_target_complete(
                    &to,
                    &rotation.target_record,
                    &target.url,
                )?;
            }
        }
        Ok(attempted)
    }

    async fn maintain_placement_until(
        &self,
        deadline: tokio::time::Instant,
    ) -> Result<PlacementState> {
        let _identity_lease = self.active_identity_lease()?;
        let attempted_at = now();
        self.state
            .prune_expired_own_rotations(attempted_at, keystore::MAX_LIVE_RETIRED_IDENTITIES)?;
        let mut remaining = MAX_PLACEMENT_ATTEMPTS_PER_WAKE;

        // First retry exact durable bytes. A directory outage or malformed new snapshot cannot
        // erase or block work that was already admitted on an earlier wake.
        remaining = remaining.saturating_sub(
            self.attempt_record_publication_until(deadline, remaining)
                .await?,
        );
        remaining = remaining.saturating_sub(
            self.attempt_rotation_publication_until(deadline, remaining)
                .await?,
        );
        if tokio::time::Instant::now() >= deadline {
            self.state.set_placement_health(true, attempted_at)?;
            return self.state.placement_state();
        }

        let refresh =
            tokio::time::timeout_at(deadline, self.refresh_directories_with_status()).await;
        let refresh_failures = match refresh {
            Ok(Ok((_, failures))) => failures,
            Ok(Err(error)) => {
                self.state.set_placement_health(true, attempted_at)?;
                return Err(error);
            }
            Err(_) => {
                self.state.set_placement_health(true, attempted_at)?;
                return self.state.placement_state();
            }
        };
        self.state
            .set_placement_health(refresh_failures > 0, attempted_at)?;
        let pool = self.directory_pool(false).await?;
        self.prepare_record_publication(&pool, attempted_at)?;
        self.sync_rotation_publication_targets(&pool)?;

        if remaining > 0 && tokio::time::Instant::now() < deadline {
            remaining = remaining.saturating_sub(
                self.attempt_record_publication_until(deadline, remaining)
                    .await?,
            );
            self.attempt_rotation_publication_until(deadline, remaining)
                .await?;
        }
        self.state.placement_state()
    }

    /// Run one bounded placement-maintenance wake and report durable degraded state.
    pub async fn maintain_placement(&self) -> Result<PlacementState> {
        self.maintain_placement_until(tokio::time::Instant::now() + DEFAULT_WAKEUP_TIMEOUT)
            .await
    }

    /// Inspect placement state without contacting a directory or loft.
    pub fn placement_status(&self) -> Result<PlacementState> {
        self.state
            .prune_expired_own_rotations(now(), keystore::MAX_LIVE_RETIRED_IDENTITIES)?;
        self.state.placement_state()
    }

    async fn maintain_placement_best_effort(&self, deadline: tokio::time::Instant) {
        if tokio::time::Instant::now() >= deadline {
            return;
        }
        if let Err(error) = self.maintain_placement_until(deadline).await {
            tracing::warn!(
                error_class = drain_error_class(&error),
                "signed placement maintenance remains degraded"
            );
        }
    }

    // ---- sending --------------------------------------------------------------------------

    /// Wrap a message for `to`, queue a copy for each of the recipient's lofts, and try to
    /// deliver. Anything undelivered stays in the outbox for the next wake-up.
    pub async fn send(&self, to: &Address, body: &str) -> Result<SendReport> {
        self.send_to(&Destination::from(to.clone()), body).await
    }

    /// Send with an exact, call-local attribution agreement. This never changes the persistent
    /// sender default, so concurrent callers targeting different custody scopes cannot race.
    pub async fn send_with_attribution_agreement(
        &self,
        to: &Address,
        body: &str,
        agreement: Option<AttributionRequirement>,
    ) -> Result<SendReport> {
        self.send_to_with_attribution_agreement(&Destination::from(to.clone()), body, agreement)
            .await
    }

    /// Send to a fully parsed destination, preserving its optional loft hint and capability token.
    pub async fn send_to(&self, to: &Destination, body: &str) -> Result<SendReport> {
        let agreement = self.state.sender_attribution_requirement()?;
        self.send_to_with_attribution_agreement(to, body, agreement)
            .await
    }

    /// Send to a parsed destination with call-local attribution consent. `None` explicitly means
    /// unattributed for this call; it does not consult or mutate the persistent sender default.
    pub async fn send_to_with_attribution_agreement(
        &self,
        to: &Destination,
        body: &str,
        agreement: Option<AttributionRequirement>,
    ) -> Result<SendReport> {
        let (address, resolution) = self.resolve_destination_target(to).await?;
        validate_supported_pow_floor(resolution.pow_min)?;
        let recipient = pigeonpost_core::keys::verifying_key_from_bytes(&resolution.pubkey)?;

        let send_time = now();
        let attribution_requirement =
            agreed_attribution_requirement(agreement, resolution.attribution_requirement)?;
        let attribution_key = if let Some(requirement) = attribution_requirement {
            self.refresh_compliance_keys_if_available().await?;
            Some(
                self.state
                    .current_attribution_key(
                        &requirement,
                        send_time.saturating_mul(1_000),
                        send_time,
                    )?
                    .ok_or_else(|| {
                        ClientError::Config(format!(
                            "no fresh witnessed attribution key is available for the agreed {jurisdiction:?} custody scope",
                            jurisdiction = requirement.jurisdiction,
                        ))
                    })?,
            )
        } else {
            None
        };
        let wrap = {
            // Use the cached private key only while its on-disk identity is exclusively pinned.
            // Resolution and registry I/O happen before this short critical section.
            let _identity_lease = self.active_identity_lease()?;
            match attribution_key.as_ref() {
                Some(key) => envelope::wrap_attributed(
                    &self.identity,
                    &recipient,
                    body,
                    send_time,
                    &key.public_key,
                    &key.publication.key_id,
                )?,
                None => envelope::wrap(&self.identity, &recipient, body, send_time)?,
            }
        };

        // Pay the recipient's advertised floor. Mining here rather than after a rejection means
        // one round trip, and the floor is signed so a loft cannot inflate it.
        let wrap = stamp_with_budget(wrap, resolution.pow_min).await?;

        let message_id = hex(&wrap.id());

        let targets: Vec<&String> = resolution.lofts.iter().take(PUBLISH_FANOUT).collect();
        if targets.is_empty() {
            return Err(ClientError::Undeliverable);
        }
        let mut locally_trusted: HashSet<String> = self
            .state
            .lofts_with_local_trust()?
            .into_iter()
            .filter_map(|(url, _, allow_local)| allow_local.then_some(url))
            .collect();
        // A signed snapshot fetched from an explicitly configured numeric-loopback directory is
        // a second independent local authorization source. Record and hint text alone never enter
        // this set. Persist the result with each outbox row so retries after restart keep exactly
        // the provenance established here.
        locally_trusted.extend(self.directory_pool(false).await?.locally_trusted);

        let routes: Vec<OutboxRoute<'_>> = targets
            .iter()
            .map(|url| OutboxRoute::new(url, locally_trusted.contains(url.as_str())))
            .collect();
        // Queue every copy and grant reply trust in one durable transaction. A failed resolution,
        // attribution refresh, work budget, or route check must never make an unsent target a
        // trusted inbound sender; a crash cannot split queue durability from correspondence.
        self.commit_outbound(
            &message_id,
            address.as_str(),
            &routes,
            &wrap,
            to.token(),
            &resolution.pubkey,
            now(),
        )?;

        let flush = self.flush().await?;
        let DeliveryStatus {
            delivered,
            queued,
            terminal,
        } = self.state.delivery_status(&message_id)?;

        Ok(SendReport {
            message_id,
            delivered,
            queued,
            terminal,
            deadline_exceeded: flush.deadline_exceeded,
        })
    }

    #[allow(clippy::too_many_arguments)]
    fn commit_outbound(
        &self,
        message_id: &str,
        to_addr: &str,
        routes: &[OutboxRoute<'_>],
        wrap: &envelope::Wrap,
        token: Option<&pigeonpost_core::Token>,
        correspondent: &[u8; 32],
        committed_at: u64,
    ) -> Result<()> {
        // Mining and directory refresh intentionally run without holding the identity lock. Pin
        // the cached key again at the durable commit point: if another process rotated meanwhile,
        // no outbox copy or reply-trust mutation is allowed to land.
        let _identity_lease = self.active_identity_lease()?;
        self.state.queue_correspondence(
            message_id,
            to_addr,
            routes,
            wrap,
            token,
            correspondent,
            committed_at,
        )
    }

    /// Attempt a bounded batch of ready outbox entries within one whole-wakeup budget.
    pub async fn flush(&self) -> Result<FlushReport> {
        self.flush_with_limits(WakeupLimits::default()).await
    }

    /// As [`Self::flush`], with explicit bounded limits for foreground integrators and tests.
    pub async fn flush_with_limits(&self, limits: WakeupLimits) -> Result<FlushReport> {
        limits.validate()?;
        let deadline = tokio::time::Instant::now() + limits.timeout;
        let attempt_time = now();
        let mut queued: VecDeque<_> = self
            .state
            .pending(FLUSH_BATCH, attempt_time)?
            .into_iter()
            .collect();
        let mut in_flight = FuturesUnordered::new();
        let mut report = FlushReport::default();

        while !queued.is_empty() || !in_flight.is_empty() {
            while in_flight.len() < limits.max_concurrency {
                let Some(entry) = queued.pop_front() else {
                    break;
                };
                in_flight.push(attempt_outbox_delivery(entry));
            }

            let result = tokio::select! {
                _ = tokio::time::sleep_until(deadline) => {
                    report.deadline_exceeded = true;
                    report.cancelled = in_flight.len();
                    None
                }
                result = in_flight.next() => result,
            };
            let Some((row, outcome)) = result else {
                // These futures are not spawned. Dropping the set synchronously cancels every
                // request, so no client task can update state after this wake-up returns.
                drop(in_flight);
                break;
            };

            report.attempted += 1;
            match outcome {
                Ok(()) => {
                    self.state.mark_sent(row, now())?;
                    report.delivered += 1;
                }
                Err(error) => {
                    let (disposition, reason) = delivery_failure(&error);
                    tracing::debug!(
                        outbox_row = row,
                        error_class = reason.as_str(),
                        terminal = disposition == DeliveryDisposition::Terminal,
                        "delivery attempt failed"
                    );
                    match disposition {
                        DeliveryDisposition::Retryable => {
                            self.state.mark_failed(row, &reason, now())?;
                            report.retryable += 1;
                        }
                        DeliveryDisposition::Terminal => {
                            self.state.mark_terminal(row, &reason, now())?;
                            report.terminal += 1;
                        }
                    }
                }
            }
        }

        report.queued = self.state.pending_count()?;
        report.dead_letters = self.state.terminal_count()?;
        self.prune_completed_outbox_wake()?;
        // Delivery owns the wake budget. Placement uses only time left over and never changes the
        // flush result or turns a directory outage into an outbox failure.
        self.maintain_placement_best_effort(deadline).await;
        Ok(report)
    }

    /// Bounded operator view of terminal outbox copies.
    pub fn dead_letters(&self, limit: usize) -> Result<Vec<DeadLetter>> {
        self.state.dead_letters(limit)
    }

    /// Bounded payload-free metadata for every undelivered copy, including delayed retries.
    pub fn pending_deliveries(&self, limit: usize) -> Result<Vec<PendingDelivery>> {
        self.state.pending_deliveries(limit)
    }

    /// Bounded metadata for successful copies whose payloads have already been erased.
    pub fn completed_deliveries(&self, limit: usize) -> Result<Vec<CompletedDelivery>> {
        self.state.completed_deliveries(limit)
    }

    pub fn delete_completed_delivery(&self, row: OutboxRecordId) -> Result<bool> {
        self.state.delete_completed_delivery(row)
    }

    pub fn delete_dead_letter(&self, row: OutboxRecordId) -> Result<bool> {
        self.state.delete_dead_letter(row)
    }

    pub fn delete_pending_outbox(&self, row: OutboxRecordId, confirmation: &str) -> Result<bool> {
        self.state.delete_pending_outbox(row, confirmation)
    }

    /// Explicitly prune successful delivery metadata. Dead letters remain until the operator
    /// deletes them individually.
    pub fn prune_completed_outbox(&self, before: u64, limit: usize) -> Result<usize> {
        self.state.prune_completed_outbox(before, limit)
    }

    /// Explicitly remove bounded payload-free metadata for successful or terminal copies. The
    /// exact confirmation keeps terminal debt out of ordinary wake maintenance.
    pub fn prune_finished_outbox(
        &self,
        before: u64,
        limit: usize,
        confirmation: &str,
    ) -> Result<usize> {
        self.state
            .prune_finished_outbox(before, limit, confirmation)
    }

    fn prune_completed_outbox_wake(&self) -> Result<usize> {
        self.state.prune_completed_outbox(
            now().saturating_sub(COMPLETED_OUTBOX_RETENTION_SECS),
            OUTBOX_PRUNE_BATCH,
        )
    }

    // ---- receiving ------------------------------------------------------------------------

    /// Drain every loft: fetch since our cursor, open what we can, store what is new.
    ///
    /// The same message arriving from two lofts is the normal case, not an error — it is
    /// deduplicated on the wrap id, which is identical everywhere.
    pub async fn drain(&self) -> Result<DrainReport> {
        self.drain_with_limits(WakeupLimits::default()).await
    }

    /// As [`Self::drain`], with explicit bounded limits for foreground integrators and tests.
    pub async fn drain_with_limits(&self, limits: WakeupLimits) -> Result<DrainReport> {
        let identity_lease = self.active_identity_lease()?;
        limits.validate()?;
        self.state
            .prune_expired_own_rotations(now(), keystore::MAX_LIVE_RETIRED_IDENTITIES)?;
        let deadline = tokio::time::Instant::now() + limits.timeout;
        let lofts = self.state.lofts_for_drain_with_local_trust(now())?;
        if lofts.is_empty() {
            return Err(ClientError::NoLofts);
        }

        let mut report = DrainReport::default();
        // Refresh once per wake-up. Availability failures may use a still-fresh witnessed cache;
        // malformed proofs, rollback, and trust failures stop the drain.
        match tokio::time::timeout_at(deadline, self.refresh_compliance_keys_if_available()).await {
            Ok(result) => result?,
            Err(_) => {
                report.deadline_exceeded = true;
                report
                    .lofts_failed
                    .extend(lofts.iter().map(|(url, _, _)| url.clone()));
                return Ok(report);
            }
        }

        self.drain_identity_until(&self.identity, &lofts, deadline, limits, &mut report)
            .await?;

        // The operating address changes immediately, but old mailbox authentication remains
        // available for exactly the signed grace interval. Each address has an independent
        // cursor, even when it drains from the same physical loft.
        for retired in
            keystore::retired_identities_with_lease(&self.key_paths, &identity_lease, now())?
        {
            if report.deadline_exceeded || tokio::time::Instant::now() >= deadline {
                report.deadline_exceeded = true;
                break;
            }
            self.state.save_own_rotation(
                &retired.record,
                &retired.source_record,
                &retired.target_record,
                &retired.lofts,
            )?;
            let targets = retired_drain_targets(&retired.lofts, &lofts);
            self.drain_identity_until(&retired.identity, &targets, deadline, limits, &mut report)
                .await?;
        }
        report.lofts_failed.sort();
        report.lofts_failed.dedup();
        drop(identity_lease);
        self.prune_completed_outbox_wake()?;
        // Cached routes are drained first. Directory refresh and record repair are opportunistic
        // work in the remaining wake budget, so control-plane loss cannot starve message receipt.
        self.maintain_placement_best_effort(deadline).await;
        Ok(report)
    }

    async fn drain_identity_until(
        &self,
        identity: &Identity,
        lofts: &[(String, Option<[u8; 32]>, bool)],
        deadline: tokio::time::Instant,
        limits: WakeupLimits,
        report: &mut DrainReport,
    ) -> Result<()> {
        let address = identity.address();
        let mut queued = VecDeque::with_capacity(lofts.len());
        let mut unfinished = HashSet::with_capacity(lofts.len());
        for (url, pubkey, allow_local) in lofts {
            unfinished.insert(url.clone());
            queued.push_back(DrainWork {
                url: url.clone(),
                pubkey: *pubkey,
                allow_local: *allow_local,
                cursor: self.state.cursor(url, &address)?,
                pages: 0,
            });
        }
        let mut in_flight = FuturesUnordered::new();

        while !queued.is_empty() || !in_flight.is_empty() {
            while in_flight.len() < limits.max_concurrency {
                let Some(work) = queued.pop_front() else {
                    break;
                };
                in_flight.push(fetch_drain_page(
                    identity,
                    work,
                    Arc::clone(&DRAIN_RESPONSE_BUDGET),
                ));
            }

            let result = tokio::select! {
                _ = tokio::time::sleep_until(deadline) => {
                    report.deadline_exceeded = true;
                    None
                }
                result = in_flight.next() => result,
            };
            let Some((mut work, outcome)) = result else {
                drop(in_flight);
                report.lofts_failed.extend(unfinished.into_iter());
                break;
            };

            let response = match outcome {
                Ok(response) => response,
                Err(error) => {
                    // Transport errors frequently embed the full URL or connected address.
                    // Network identifiers belong only in the sealed trace store.
                    tracing::warn!(error_class = drain_error_class(&error), "fetch failed");
                    report.lofts_failed.push(work.url.clone());
                    unfinished.remove(&work.url);
                    continue;
                }
            };
            let BudgetedFetchResponse {
                response,
                permit: response_budget_permit,
            } = response;

            let malformed_cursor = (response.events.is_empty()
                && (response.more || response.next_cursor != work.cursor))
                || (!response.events.is_empty() && response.next_cursor <= work.cursor)
                // SQLite INTEGER is signed. Reject an unpersistable protocol cursor before
                // decrypting or storing any event so a hostile loft cannot poison durable state
                // (or make a successfully processed batch replay forever).
                || i64::try_from(response.next_cursor).is_err();
            if malformed_cursor {
                tracing::warn!(error_class = "loft_protocol", "fetch failed");
                report.lofts_failed.push(work.url.clone());
                unfinished.remove(&work.url);
                continue;
            }
            if response.events.is_empty() {
                unfinished.remove(&work.url);
                continue;
            }

            report.fetched += response.events.len();
            match self.process_fetched_events(identity, &response, report) {
                Ok(()) => {}
                Err(ClientError::AttributionTrustUnavailable) => {
                    // One lagging or hostile Loft must not starve unrelated routes. The fetched
                    // page remains durable at this Loft because its cursor is deliberately not
                    // advanced; stop only this route for the current wake and continue draining
                    // every other in-flight/configured route.
                    tracing::warn!(
                        error_class = "attribution_trust",
                        "loft fetch deferred pending registry refresh"
                    );
                    report.lofts_failed.push(work.url.clone());
                    unfinished.remove(&work.url);
                    drop(response_budget_permit);
                    continue;
                }
                Err(error) => return Err(error),
            }

            // Advance only after the batch is stored, so a crash re-reads rather than skips.
            work.cursor = response.next_cursor;
            self.state.set_cursor(&work.url, &address, work.cursor)?;
            work.pages += 1;
            if response.more && work.pages < MAX_DRAIN_PAGES_PER_ROUTE {
                queued.push_back(work);
            } else {
                if response.more {
                    tracing::warn!(error_class = "loft_page_limit", "fetch stopped");
                    report.lofts_failed.push(work.url.clone());
                }
                unfinished.remove(&work.url);
            }
            drop(response_budget_permit);
        }

        Ok(())
    }

    fn process_fetched_events(
        &self,
        identity: &Identity,
        response: &FetchResponse,
        report: &mut DrainReport,
    ) -> Result<()> {
        let attribution_requirement = self.state.recipient_attribution_requirement()?;
        let attribution_required = attribution_requirement.is_some();
        // Agent instances are intentionally long-lived in MCP hosts while CLI processes may
        // update the same WAL. Read the validated durable setting for every fetched page instead
        // of retaining a constructor snapshot.
        let accept_all = self.state.accept_all()?;
        let decision_time = now();
        for wrap in &response.events {
            // An unavailable or stale witnessed cache is a local trust failure, not evidence that
            // the sender's attribution is invalid. Propagate it so the batch cursor stays put and
            // the same ciphertexts can be retried after registry trust becomes available again.
            // Even a fresh older prefix cannot prove that a matching key was not appended later;
            // an exact required-scope miss therefore also propagates and pins only this Loft's
            // cursor until a refresh can decide it. Optional policy retains the non-blocking
            // Invalid verdict because one hostile optional block must not pin ordinary traffic.
            let trusted = match wrap.attribution.as_ref().and_then(|block| block.as_v3()) {
                Some(block) => {
                    let key = if let Some(requirement) = attribution_requirement {
                        if !requirement.matches_key_id(&block.key_id) {
                            None
                        } else {
                            self.state.attribution_key(&block.key_id, decision_time)?
                        }
                    } else {
                        self.state
                            .optional_attribution_key(&block.key_id, decision_time)?
                    };
                    key.map(|key| {
                        envelope::TrustedAttributionKey::new(
                            key.publication.key_id,
                            key.public_key,
                            key.publication.not_before_ms,
                            key.publication.not_after_ms,
                        )
                    })
                    .transpose()?
                }
                None => None,
            };
            match envelope::open_attributed_trusted(identity, wrap, trusted.as_ref()) {
                Ok((sender, body, attribution)) => {
                    // A loft enforces only the public block shape. It may be stale or hostile, so
                    // the recipient must enforce the independently verified verdict after unwrap.
                    // Drop rather than aborting the batch: one malicious event must not pin the
                    // cursor and deny delivery of every valid event behind it.
                    if attribution_required && attribution != envelope::Attribution::Valid {
                        report.dropped += 1;
                        continue;
                    }

                    let id = hex(&wrap.id());
                    let from = Address::from_pubkey(&sender);
                    let pubkey = sender.to_bytes();

                    // The sender is only knowable here, after unwrap — which is exactly why these
                    // layers cannot live in the loft (`docs/spam.md`).
                    let (raw_score, updated_at) = self.state.score(&pubkey)?;
                    let context = SenderContext {
                        allowlisted: self.state.is_allowed(&pubkey)?,
                        raw_score,
                        score_updated_at: updated_at,
                        has_handle: self
                            .state
                            .has_current_verified_handle(&pubkey, decision_time)?,
                    };

                    match spam::decide(&context, accept_all, decision_time) {
                        Disposition::Drop => {
                            // Silently. Telling a spammer why would help it adapt.
                            report.dropped += 1;
                        }
                        disposition => {
                            let state = if disposition == Disposition::Accept {
                                "accepted"
                            } else {
                                "pending"
                            };
                            let is_new = self.state.store_message_with_attribution(
                                &id,
                                &pubkey,
                                from.as_str(),
                                now(),
                                &body,
                                state,
                                attribution,
                            )?;
                            if is_new {
                                report.new_messages += 1;
                                if disposition == Disposition::Pending {
                                    report.pending += 1;
                                } else {
                                    self.state.adjust_score(
                                        &pubkey,
                                        spam::SCORE_ACCEPTED,
                                        now(),
                                    )?;
                                }
                            } else {
                                report.duplicates += 1;
                            }
                        }
                    }
                }
                Err(_) => {
                    // Mail we cannot open is not a crash: a loft may hold anything, and an attacker
                    // can address junk to us cheaply. Count it and move on.
                    tracing::debug!(kind = "unopenable_event", "discarding an unopenable event");
                    report.undecryptable += 1;
                }
            }
        }
        Ok(())
    }

    // ---- inbox ----------------------------------------------------------------------------

    pub fn inbox(&self, unread_only: bool, limit: usize) -> Result<Vec<StoredMessage>> {
        self.state.messages(unread_only, limit)
    }

    /// Read a message by id or unambiguous id prefix. Does **not** mark it read — `ack` does.
    pub fn read(&self, id: &str) -> Result<StoredMessage> {
        self.state
            .message(id)?
            .ok_or_else(|| ClientError::NoSuchMessage(id.to_string()))
    }

    pub fn ack(&self, id: &str) -> Result<StoredMessage> {
        let message = self.read(id)?;
        self.state.mark_read(&message.id)?;
        Ok(message)
    }

    /// Explicitly delete exactly one inbound message id. No wake-up ever calls this method.
    pub fn delete_message(&self, id: &str) -> Result<bool> {
        self.state.delete_message(id)
    }

    pub fn unread_count(&self) -> Result<u64> {
        self.state.unread_count()
    }

    // ---- resolution -----------------------------------------------------------------------

    /// Find an address's key and lofts.
    ///
    /// Ask an explicit hint, our own lofts, and a bounded deterministic rendezvous walk.
    pub async fn resolve(&self, address: &Address) -> Result<Resolution> {
        Ok(self.resolve_address_target(address, None).await?.1)
    }

    pub async fn resolve_destination(&self, destination: &Destination) -> Result<Resolution> {
        Ok(self.resolve_destination_target(destination).await?.1)
    }

    /// Resolve a destination to the final authenticated key address and routing record. Handle
    /// callers need the derived address, while rotated key-address callers need the final hop.
    pub async fn resolve_destination_target(
        &self,
        destination: &Destination,
    ) -> Result<(Address, Resolution)> {
        self.resolve_destination_target_inner(destination, true)
            .await
    }

    /// Resolve with a mandatory live registry proof for handle inputs. Registration flows use
    /// this after appending a binding so a disappearing registry cannot turn an older cached
    /// mapping into apparent proof that the new append succeeded.
    pub async fn resolve_destination_target_online(
        &self,
        destination: &Destination,
    ) -> Result<(Address, Resolution)> {
        self.resolve_destination_target_inner(destination, false)
            .await
    }

    async fn resolve_destination_target_inner(
        &self,
        destination: &Destination,
        allow_cached_handle: bool,
    ) -> Result<(Address, Resolution)> {
        let address = match destination.address() {
            Some(address) => address.clone(),
            None => {
                self.resolve_handle(
                    destination.handle().ok_or_else(|| {
                        ClientError::Config("destination has no identity target".into())
                    })?,
                    allow_cached_handle,
                )
                .await?
            }
        };
        self.resolve_address_target(&address, destination.loft_hint())
            .await
    }

    async fn resolve_handle(&self, handle: &str, allow_cached: bool) -> Result<Address> {
        let parsed = Handle::parse(handle)?;
        let configuration = self
            .state
            .registry_configuration()?
            .ok_or_else(|| ClientError::Config("handle registry is not configured".into()))?;
        let client = RegistryClient::new(&configuration.url, configuration.trust)?;
        match self
            .state
            .resolve_handle_audited(&client, &parsed, now())
            .await
        {
            // The sole checkpoint pin, global audit frontier, projection delta, and requested
            // binding commit atomically after the network stream and root verification finish.
            Ok((_, address)) => Ok(address),
            Err(ClientError::Registry(RegistryError::RegistryUnavailable)) if allow_cached => self
                .state
                .handle_resolution(handle)?
                .ok_or(ClientError::Registry(RegistryError::RegistryUnavailable)),
            Err(error) => Err(error),
        }
    }

    async fn resolve_address_target(
        &self,
        address: &Address,
        loft_hint: Option<&str>,
    ) -> Result<(Address, Resolution)> {
        tokio::time::timeout(
            RESOLUTION_TOTAL_TIMEOUT,
            self.resolve_address_target_bounded(address, loft_hint),
        )
        .await
        .map_err(|_| ClientError::Config("address resolution exceeded its total deadline".into()))?
    }

    async fn resolve_address_target_bounded(
        &self,
        address: &Address,
        loft_hint: Option<&str>,
    ) -> Result<(Address, Resolution)> {
        let configured_candidates = self.state.lofts_with_local_trust()?;
        let configured_local: HashSet<String> = configured_candidates
            .iter()
            .filter_map(|(url, _, allow_local)| allow_local.then_some(url.clone()))
            .collect();
        let mut inherited_candidates: Vec<String> = Vec::new();
        // A portable self-hosted hint is a complete bootstrap route. Do not disclose the target to
        // a directory or rendezvous operator when the sender explicitly chose that route.
        let pool = optional_resolution_pool(loft_hint, || self.directory_pool(true)).await?;
        let mut current = address.clone();
        let mut visited = HashSet::new();
        let mut expected: Option<([u8; 32], [u8; 32], u64)> = None;

        for _ in 0..MAX_ROTATION_HOPS {
            if !visited.insert(current.to_string()) {
                return Err(ClientError::Config(
                    "rotation chain contains a cycle".into(),
                ));
            }

            let cached = self.state.resolution(&current)?;
            if let Some(cached) = cached.as_ref() {
                validate_loft_list(&cached.lofts)?;
            }
            let (records, rotations) = if let Some(hint) = loft_hint {
                let mut candidates = Vec::new();
                push_candidate(
                    &mut candidates,
                    hint.to_owned(),
                    configured_local.contains(hint),
                );
                let replies = fetch_resolution_candidates(&current, candidates).await;
                resolution_data(&replies)
            } else {
                let pool = pool.as_ref().expect("directory pool exists without a hint");
                let ranked = rendezvous(&pool.entries, current.as_str(), RENDEZVOUS_WALK);
                let ranked: Vec<LoftCandidate> = ranked
                    .into_iter()
                    .map(|entry| LoftCandidate {
                        url: entry.endpoint.clone(),
                        allow_local: pool.locally_trusted.contains(&entry.endpoint),
                    })
                    .collect();
                let replies = if ranked.is_empty() {
                    // Fully self-hosted circles may share an explicitly configured loft without a
                    // directory. This compatibility path is used only when no rendezvous ranking
                    // exists; it never adds eager contacts to the three-plus-fallback contract.
                    let mut direct = Vec::new();
                    for (url, _, allow_local) in
                        configured_candidates.iter().take(MAX_AGENT_RECORD_LOFTS)
                    {
                        push_candidate(&mut direct, url.clone(), *allow_local);
                    }
                    for url in inherited_candidates.iter().take(MAX_AGENT_RECORD_LOFTS) {
                        push_candidate(&mut direct, url.clone(), false);
                    }
                    if let Some(cached) = cached.as_ref() {
                        for url in cached.lofts.iter().take(MAX_AGENT_RECORD_LOFTS) {
                            push_candidate(&mut direct, url.clone(), false);
                        }
                    }
                    if direct.is_empty() && cached.is_none() {
                        return Err(ClientError::NoLofts);
                    }
                    fetch_resolution_candidates(&current, direct).await
                } else {
                    fetch_ranked_resolution_walk(
                        &current,
                        &ranked,
                        cached.as_ref(),
                        |address, candidates| async move {
                            fetch_resolution_candidates(&address, candidates).await
                        },
                    )
                    .await?
                };
                resolution_data(&replies)
            };

            // A rotation is accepted only with the exact source record it advances. Invalid
            // records from a hostile loft are ignored; two distinct valid transitions equivocate.
            let verified_rotation =
                select_verified_rotation(&records, &rotations, cached.as_ref())?;

            let resolution = if let Some((rotation, source)) = verified_rotation {
                verify_expected(&expected, &source)?;
                self.state.save_resolution(&current, &source, now())?;
                self.state.save_rotation(&current, &rotation, now())?;
                inherited_candidates = source.lofts.clone();
                expected = Some((
                    rotation.to_pubkey,
                    rotation.next_successor_hash,
                    rotation.seq,
                ));
                current = rotation.target_address()?;
                continue;
            } else if let Some(record) = highest_unambiguous_record(&records)? {
                let resolution = resolution_from_record(record);
                verify_expected(&expected, &resolution)?;
                self.state.save_resolution(&current, &resolution, now())?
            } else if let Some(cached) = cached {
                tracing::debug!(
                    kind = "cached_resolution",
                    "no loft reachable; using cached state"
                );
                verify_expected(&expected, &cached)?;
                cached
            } else {
                return Err(ClientError::Unresolvable(current.to_string()));
            };

            // Previously verified chains remain routable while every loft is offline.
            if let Some(rotation) = self.state.rotation(&current)? {
                rotation.verify(
                    &SuccessorCommitment(resolution.successor_hash),
                    resolution.seq,
                    now(),
                )?;
                expected = Some((
                    rotation.to_pubkey,
                    rotation.next_successor_hash,
                    rotation.seq,
                ));
                current = rotation.target_address()?;
                continue;
            }
            return Ok((current, resolution));
        }
        Err(ClientError::Config(
            "rotation chain exceeds the maximum hop count".into(),
        ))
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum PowMiningOutcome {
    Found(u64),
    Exhausted,
    Cancelled,
}

struct PowCancellation(Arc<AtomicBool>);

impl Drop for PowCancellation {
    fn drop(&mut self) {
        self.0.store(true, Ordering::Relaxed);
    }
}

fn validate_supported_pow_floor(bits: u32) -> Result<()> {
    if bits > spam::MAX_SUPPORTED_POW_BITS {
        return Err(ClientError::Config(format!(
            "recipient requires {bits} proof-of-work bits; this client supports at most {}",
            spam::MAX_SUPPORTED_POW_BITS
        )));
    }
    Ok(())
}

fn agreed_attribution_requirement(
    sender: Option<AttributionRequirement>,
    recipient: Option<AttributionRequirement>,
) -> Result<Option<AttributionRequirement>> {
    match (sender, recipient) {
        (None, None) => Ok(None),
        (Some(sender), None) => Ok(Some(sender)),
        (Some(sender), Some(recipient)) if sender == recipient => Ok(Some(recipient)),
        (None, Some(recipient)) => Err(ClientError::Config(format!(
            "recipient requires attribution in {:?} with authority {}; explicitly configure that signed scope before sending",
            recipient.jurisdiction,
            hex(&recipient.authority),
        ))),
        (Some(_), Some(recipient)) => Err(ClientError::Config(format!(
            "sender attribution scope does not match the recipient's signed {:?} authority {} requirement",
            recipient.jurisdiction,
            hex(&recipient.authority),
        ))),
    }
}

/// Mine on Tokio's blocking pool under both an attempt cap and a wall-clock deadline.
///
/// The cancellation flag is owned by an RAII guard, so a timed-out or externally cancelled send
/// asks a detached blocking task to stop within one small polling interval.
async fn stamp_with_budget(mut wrap: envelope::Wrap, bits: u32) -> Result<envelope::Wrap> {
    validate_supported_pow_floor(bits)?;
    if bits == 0 {
        wrap.pow_nonce = 0;
        return Ok(wrap);
    }
    wrap.pow_nonce = mine_pow_nonce_with_budget(wrap.id(), bits).await?;
    Ok(wrap)
}

async fn mine_pow_nonce_with_budget(id: [u8; 32], bits: u32) -> Result<u64> {
    mine_pow_nonce_with_deadline(id, bits, POW_MINING_BUDGET).await
}

async fn mine_pow_nonce_with_deadline(id: [u8; 32], bits: u32, budget: Duration) -> Result<u64> {
    validate_supported_pow_floor(bits)?;
    if bits == 0 {
        return Ok(0);
    }
    let permit = acquire_pow_mining_slot()?;
    let cancelled = Arc::new(AtomicBool::new(false));
    let cancellation = PowCancellation(Arc::clone(&cancelled));
    let deadline = Instant::now() + budget;
    let mut mining = tokio::task::spawn_blocking(move || {
        // The permit belongs to the worker rather than its awaiting future. A timeout/cancel asks
        // the loop to stop, but capacity is released only when the CPU work has actually exited.
        let _permit = permit;
        for nonce in 0..POW_MINING_MAX_ATTEMPTS {
            if nonce % POW_CANCELLATION_POLL_INTERVAL == 0
                && (cancelled.load(Ordering::Relaxed) || Instant::now() >= deadline)
            {
                return PowMiningOutcome::Cancelled;
            }
            if pigeonpost_core::pow::work(&id, nonce) >= bits {
                return PowMiningOutcome::Found(nonce);
            }
        }
        PowMiningOutcome::Exhausted
    });

    let outcome = match tokio::time::timeout_at(deadline.into(), &mut mining).await {
        Ok(Ok(outcome)) => outcome,
        Ok(Err(_)) => {
            return Err(ClientError::Config(
                "proof-of-work worker stopped unexpectedly".into(),
            ));
        }
        Err(_) => {
            cancellation.0.store(true, Ordering::Relaxed);
            // Tokio cannot abort an already-running blocking closure; that closure observes the
            // flag. If it is still queued, abort prevents a timed-out job from starting later and
            // releases its captured global permit immediately.
            mining.abort();
            PowMiningOutcome::Cancelled
        }
    };
    drop(cancellation);
    match outcome {
        PowMiningOutcome::Found(nonce) => Ok(nonce),
        PowMiningOutcome::Exhausted => {
            Err(ClientError::Core(pigeonpost_core::Error::InsufficientWork))
        }
        PowMiningOutcome::Cancelled => Err(ClientError::Config(format!(
            "proof-of-work mining exceeded the {} second client budget",
            budget.as_secs()
        ))),
    }
}

fn acquire_pow_mining_slot() -> Result<tokio::sync::OwnedSemaphorePermit> {
    Arc::clone(&POW_MINING_SLOTS)
        .try_acquire_owned()
        .map_err(|_| {
            ClientError::Config(format!(
                "proof-of-work capacity is busy (maximum {MAX_CONCURRENT_POW_MINERS} concurrent miners)"
            ))
        })
}

fn push_candidate(candidates: &mut Vec<LoftCandidate>, url: String, allow_local: bool) {
    if let Some(existing) = candidates.iter_mut().find(|candidate| candidate.url == url) {
        // Local trust is granted only by an independent configured source. Seeing the same URL in
        // an untrusted hint or record can never clear or create that authorization.
        existing.allow_local |= allow_local;
    } else if candidates.len() < MAX_RESOLUTION_CANDIDATES {
        candidates.push(LoftCandidate { url, allow_local });
    }
}

fn merge_publication_target(
    targets: &mut Vec<PublicationTarget>,
    url: String,
    allow_local: bool,
    rendezvous: bool,
) {
    if let Some(existing) = targets.iter_mut().find(|target| target.url == url) {
        existing.allow_local |= allow_local;
        existing.rendezvous |= rendezvous;
    } else {
        targets.push(PublicationTarget::pending(url, allow_local, rendezvous));
    }
}

async fn loft_client_for_route(url: &str, allow_local: bool) -> Result<LoftClient> {
    if allow_local {
        let explicit = LoftEndpoint::explicit(url)?;
        if explicit.is_exact_loopback() {
            return Ok(LoftClient::from_endpoint(explicit)?);
        }
    }
    Ok(LoftClient::new_untrusted(url).await?)
}

fn exact_loopback_directory(input: &str) -> bool {
    let Ok(url) = url::Url::parse(input) else {
        return false;
    };
    if !matches!(url.scheme(), "http" | "https")
        || !url.username().is_empty()
        || url.password().is_some()
        || url.query().is_some()
        || url.fragment().is_some()
        || !matches!(url.path(), "" | "/")
        || url.port() == Some(0)
    {
        return false;
    }
    url.host_str()
        .map(|host| host.trim_start_matches('[').trim_end_matches(']'))
        .and_then(|host| host.parse::<std::net::IpAddr>().ok())
        .is_some_and(|address| address.is_loopback())
}

fn resolution_from_record(record: &AgentRecord) -> Resolution {
    Resolution {
        pubkey: record.pubkey,
        successor_hash: record.successor_hash,
        seq: record.seq,
        lofts: record.lofts.clone(),
        pow_min: record.pow_min,
        attribution_requirement: record.attribution_requirement,
    }
}

async fn optional_resolution_pool<F, Fut>(
    loft_hint: Option<&str>,
    load: F,
) -> Result<Option<DirectoryPool>>
where
    F: FnOnce() -> Fut,
    Fut: std::future::Future<Output = Result<DirectoryPool>>,
{
    if loft_hint.is_some() {
        Ok(None)
    } else {
        load().await.map(Some)
    }
}

struct ResolutionReply {
    endpoint: String,
    record: Option<AgentRecord>,
    rotation: Option<RotationRecord>,
}

async fn fetch_resolution_candidates(
    address: &Address,
    candidates: Vec<LoftCandidate>,
) -> Vec<ResolutionReply> {
    let mut replies = Vec::new();
    let mut tasks = tokio::task::JoinSet::new();
    for candidate in candidates {
        let address = address.clone();
        tasks.spawn(async move {
            let endpoint = candidate.url.clone();
            let Ok(client) = loft_client_for_route(&candidate.url, candidate.allow_local).await
            else {
                return ResolutionReply {
                    endpoint,
                    record: None,
                    rotation: None,
                };
            };
            let (record, rotation) = tokio::join!(
                client.agent_record(&address),
                client.rotation_record(&address)
            );
            ResolutionReply {
                endpoint,
                record: record.ok(),
                rotation: rotation.ok(),
            }
        });
    }
    while let Some(joined) = tasks.join_next().await {
        if let Ok(reply) = joined {
            replies.push(reply);
        }
    }
    replies
}

/// Query exactly the primary rendezvous tier, then fill only missing valid responses by walking
/// the same deterministic ranking. The fetch closure keeps contact-count tests independent of the
/// production DNS/TLS path; production always supplies `fetch_resolution_candidates`.
async fn fetch_ranked_resolution_walk<F, Fut>(
    address: &Address,
    ranked: &[LoftCandidate],
    cached: Option<&Resolution>,
    fetch: F,
) -> Result<Vec<ResolutionReply>>
where
    F: Fn(Address, Vec<LoftCandidate>) -> Fut,
    Fut: std::future::Future<Output = Vec<ResolutionReply>>,
{
    let ranked = &ranked[..ranked.len().min(RENDEZVOUS_WALK)];
    let ranked_endpoints: HashSet<String> = ranked
        .iter()
        .map(|candidate| candidate.url.clone())
        .collect();
    let target_responses = RENDEZVOUS_TIER.min(ranked_endpoints.len());
    let primary_count = target_responses.min(ranked.len());
    let primary = ranked[..primary_count].to_vec();
    let mut contacted: HashSet<String> = primary
        .iter()
        .map(|candidate| candidate.url.clone())
        .collect();
    let mut replies = if primary.is_empty() {
        Vec::new()
    } else {
        fetch(address.clone(), primary).await
    };
    let mut next_rank = primary_count;

    loop {
        let usable = usable_ranked_reply_count(&replies, &ranked_endpoints, cached)?;
        if usable >= target_responses || next_rank >= ranked.len() {
            break;
        }
        let needed = target_responses - usable;
        let mut candidates = Vec::with_capacity(needed);
        while candidates.len() < needed && next_rank < ranked.len() {
            let candidate = ranked[next_rank].clone();
            next_rank += 1;
            if contacted.insert(candidate.url.clone()) {
                candidates.push(candidate);
            }
        }
        if candidates.is_empty() {
            continue;
        }
        replies.extend(fetch(address.clone(), candidates).await);
    }
    Ok(replies)
}

fn resolution_data(replies: &[ResolutionReply]) -> (Vec<AgentRecord>, Vec<RotationRecord>) {
    let mut records = Vec::new();
    let mut rotations = Vec::new();
    for reply in replies {
        if let Some(record) = reply.record.as_ref() {
            if !records.contains(record) {
                records.push(record.clone());
            }
        }
        if let Some(rotation) = reply.rotation.as_ref() {
            if !rotations.contains(rotation) {
                rotations.push(rotation.clone());
            }
        }
    }
    (records, rotations)
}

fn usable_ranked_reply_count(
    replies: &[ResolutionReply],
    ranked_endpoints: &HashSet<String>,
    cached: Option<&Resolution>,
) -> Result<usize> {
    let (records, rotations) = resolution_data(replies);
    // Detect authenticated equivocation before using the response count to decide whether to stop.
    highest_unambiguous_record(&records)?;
    select_verified_rotation(&records, &rotations, cached)?;

    let mut usable = 0;
    for reply in replies {
        if !ranked_endpoints.contains(&reply.endpoint) {
            continue;
        }
        let rotation_is_verified = match reply.rotation.as_ref() {
            Some(rotation) => {
                select_verified_rotation(&records, std::slice::from_ref(rotation), cached)?
                    .is_some()
            }
            None => false,
        };
        if reply.record.is_some() || rotation_is_verified {
            usable += 1;
        }
    }
    Ok(usable)
}

fn select_verified_rotation(
    records: &[AgentRecord],
    rotations: &[RotationRecord],
    cached: Option<&Resolution>,
) -> Result<Option<(RotationRecord, Resolution)>> {
    let mut verified: Option<(RotationRecord, Resolution)> = None;
    for rotation in rotations {
        let mut live_source: Option<&AgentRecord> = None;
        for record in records
            .iter()
            .filter(|record| record.seq.checked_add(1) == Some(rotation.seq))
        {
            if rotation
                .verify(&record.successor_commitment(), record.seq, now())
                .is_err()
            {
                continue;
            }
            if live_source.is_some_and(|known| known != record) {
                return Err(ClientError::Core(pigeonpost_core::Error::StaleSequence));
            }
            live_source = Some(record);
        }
        let source = live_source.map(resolution_from_record).or_else(|| {
            cached.and_then(|known| {
                rotation
                    .verify(&SuccessorCommitment(known.successor_hash), known.seq, now())
                    .ok()
                    .map(|_| known.clone())
            })
        });
        let Some(source) = source else {
            continue;
        };
        match verified.as_ref() {
            Some((known, _)) if known != rotation => {
                return Err(ClientError::Core(pigeonpost_core::Error::StaleSequence));
            }
            Some(_) => {}
            None => verified = Some((rotation.clone(), source)),
        }
    }
    Ok(verified)
}

fn highest_unambiguous_record(records: &[AgentRecord]) -> Result<Option<&AgentRecord>> {
    let mut best: Option<&AgentRecord> = None;
    let mut by_sequence: HashMap<u64, &AgentRecord> = HashMap::new();
    for record in records {
        if let Some(known) = by_sequence.insert(record.seq, record) {
            if known != record {
                return Err(ClientError::Core(pigeonpost_core::Error::StaleSequence));
            }
        }
        match best {
            Some(known) if known.seq >= record.seq => {}
            _ => best = Some(record),
        }
    }
    Ok(best)
}

fn verify_expected(
    expected: &Option<([u8; 32], [u8; 32], u64)>,
    resolution: &Resolution,
) -> Result<()> {
    if let Some((pubkey, successor, sequence)) = expected {
        if resolution.pubkey != *pubkey
            || resolution.successor_hash != *successor
            || resolution.seq < *sequence
        {
            return Err(ClientError::Core(pigeonpost_core::Error::SuccessorMismatch));
        }
    }
    Ok(())
}

async fn attempt_outbox_delivery(entry: OutboxEntry) -> (i64, Result<()>) {
    let row = entry.row;
    let outcome = async {
        let client = loft_client_for_route(&entry.loft_url, entry.allow_local).await?;
        let presentation = if let Some(token) = entry.token.as_ref() {
            let info = client.info().await?;
            let loft_pubkey = parse_hex32(&info.pubkey).ok_or_else(|| {
                ClientError::Config("loft returned a malformed public key".into())
            })?;
            Some(hex(token
                .presentation(&loft_pubkey, client.base_url())?
                .as_bytes()))
        } else {
            None
        };
        client.publish(&entry.wrap, presentation).await?;
        Ok(())
    }
    .await;
    (row, outcome)
}

async fn fetch_drain_page(
    identity: &Identity,
    mut work: DrainWork,
    response_budget: Arc<tokio::sync::Semaphore>,
) -> (DrainWork, Result<BudgetedFetchResponse>) {
    let outcome = async {
        let client = loft_client_for_route(&work.url, work.allow_local).await?;
        let pubkey = match work.pubkey {
            Some(pubkey) => pubkey,
            None => {
                let info = client.info().await?;
                parse_hex32(&info.pubkey).ok_or_else(|| {
                    ClientError::Config("loft returned a malformed public key".into())
                })?
            }
        };
        work.pubkey = Some(pubkey);
        let response_permits = u32::try_from(pigeonpost_loft::client::MAX_FETCH_RESPONSE_BYTES)
            .expect("the protocol fetch ceiling fits Tokio's permit count");
        let permit = response_budget
            .acquire_many_owned(response_permits)
            .await
            .map_err(|_| ClientError::Config("fetch response capacity is unavailable".into()))?;
        let response = client
            .fetch(identity, &pubkey, work.cursor, now(), None)
            .await
            .map_err(ClientError::from)?;
        Ok(BudgetedFetchResponse { response, permit })
    }
    .await;
    (work, outcome)
}

fn delivery_failure(error: &ClientError) -> (DeliveryDisposition, String) {
    match error {
        ClientError::Loft(pigeonpost_loft::ClientError::Transport(error)) => (
            DeliveryDisposition::Retryable,
            if error.is_timeout() {
                "transport_timeout"
            } else {
                "transport"
            }
            .into(),
        ),
        ClientError::Loft(pigeonpost_loft::ClientError::Refused { status, .. }) => {
            let disposition =
                if matches!(*status, 408 | 409 | 425 | 429) || (500..=599).contains(status) {
                    DeliveryDisposition::Retryable
                } else {
                    DeliveryDisposition::Terminal
                };
            (disposition, format!("http_{status}"))
        }
        ClientError::Loft(pigeonpost_loft::ClientError::ResolutionFailed) => {
            (DeliveryDisposition::Retryable, "resolution_failed".into())
        }
        ClientError::AttributionTrustUnavailable => {
            (DeliveryDisposition::Retryable, "attribution_trust".into())
        }
        ClientError::Loft(
            pigeonpost_loft::ClientError::ResponseTooLarge
            | pigeonpost_loft::ClientError::UnsupportedEncoding
            | pigeonpost_loft::ClientError::ProtocolMismatch
            | pigeonpost_loft::ClientError::Decode(_)
            | pigeonpost_loft::ClientError::Core(_),
        )
        | ClientError::Core(_) => (DeliveryDisposition::Terminal, "protocol".into()),
        ClientError::Loft(
            pigeonpost_loft::ClientError::InvalidUrl
            | pigeonpost_loft::ClientError::UnsafeNetworkTarget
            | pigeonpost_loft::ClientError::OriginMismatch,
        )
        | ClientError::Config(_) => (DeliveryDisposition::Terminal, "configuration".into()),
        ClientError::Directory(_) | ClientError::Registry(_) => {
            (DeliveryDisposition::Terminal, "configuration".into())
        }
        ClientError::State(_)
        | ClientError::StorageLimit(_)
        | ClientError::Serialization(_)
        | ClientError::Io(_)
        | ClientError::NoIdentity
        | ClientError::NoLofts
        | ClientError::Unresolvable(_)
        | ClientError::NoSuchMessage(_)
        | ClientError::AmbiguousMessage(_)
        | ClientError::Undeliverable
        | ClientError::PolicyIncomplete { .. } => (DeliveryDisposition::Terminal, "client".into()),
    }
}

fn drain_error_class(error: &ClientError) -> &'static str {
    match error {
        ClientError::Loft(pigeonpost_loft::ClientError::Transport(_)) => "transport",
        ClientError::Loft(pigeonpost_loft::ClientError::Refused { status, .. })
            if (500..=599).contains(status) =>
        {
            "loft_5xx"
        }
        ClientError::Loft(pigeonpost_loft::ClientError::Refused { .. }) => "loft_refused",
        ClientError::Loft(pigeonpost_loft::ClientError::ResolutionFailed) => "resolution_failed",
        ClientError::Loft(_) | ClientError::Core(_) => "protocol",
        ClientError::AttributionTrustUnavailable => "attribution_trust",
        ClientError::Config(_) => "configuration",
        _ => "client",
    }
}

fn secure_agent_home(home: &Path) -> Result<()> {
    // The custody helper records whether this call won creation of the final directory. Windows
    // may safely transfer ownership of that exact new object when an elevated runner assigns the
    // Administrators group as owner; pre-existing or race-lost directories remain fail-closed.
    keystore::secure_or_create_directory(home)
}

pub fn now() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0)
}

pub fn hex(bytes: &[u8]) -> String {
    bytes.iter().map(|b| format!("{b:02x}")).collect()
}

fn parse_hex32(hex: &str) -> Option<[u8; 32]> {
    if hex.len() != 64 {
        return None;
    }
    let mut out = [0u8; 32];
    for (i, chunk) in hex.as_bytes().chunks(2).enumerate() {
        out[i] = u8::from_str_radix(std::str::from_utf8(chunk).ok()?, 16).ok()?;
    }
    Some(out)
}

/// Re-exported so callers can render a body without reaching for the raw string.
pub fn fenced(body: &UntrustedBody) -> String {
    body.fenced()
}

#[cfg(test)]
mod endpoint_trust_tests {
    use super::*;
    use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
    use std::sync::{Arc, Mutex};

    use axum::extract::State as AxumState;
    use axum::http::StatusCode;
    use axum::routing::{post, put};
    use axum::Router;
    use ed25519_dalek::SigningKey;
    use pigeonpost_directory::{Directory, LoftPolicy, LoftState};

    static POW_TEST_SERIAL: tokio::sync::Mutex<()> = tokio::sync::Mutex::const_new(());

    struct BlockingWorkerRelease(Arc<AtomicBool>);

    impl Drop for BlockingWorkerRelease {
        fn drop(&mut self) {
            self.0.store(true, Ordering::SeqCst);
        }
    }

    #[cfg(not(any(target_os = "linux", target_os = "macos", windows)))]
    #[test]
    fn unsupported_agent_open_rejects_before_home_creation() {
        let root = tempfile::tempdir().unwrap();
        let home = root.path().join("must-not-exist");
        assert!(matches!(
            Agent::open(&home),
            Err(ClientError::Io(error)) if error.kind() == std::io::ErrorKind::Unsupported
        ));
        assert!(!home.exists());
    }

    #[test]
    fn sender_agreement_must_match_the_recipient_signed_scope_exactly() {
        let required = AttributionRequirement::new(Jurisdiction::Eu, [0x41; 32]);
        let wrong_authority = AttributionRequirement::new(Jurisdiction::Eu, [0x42; 32]);
        let wrong_jurisdiction = AttributionRequirement::new(Jurisdiction::Tr, [0x41; 32]);

        assert_eq!(
            agreed_attribution_requirement(Some(required), Some(required)).unwrap(),
            Some(required)
        );
        assert!(agreed_attribution_requirement(None, Some(required)).is_err());
        assert!(agreed_attribution_requirement(Some(wrong_authority), Some(required)).is_err());
        assert!(agreed_attribution_requirement(Some(wrong_jurisdiction), Some(required)).is_err());
        assert_eq!(
            agreed_attribution_requirement(Some(required), None).unwrap(),
            Some(required),
            "a sender may voluntarily attribute to an exact witnessed scope"
        );
    }

    #[test]
    fn retired_history_cannot_resurrect_an_expired_removed_loft() {
        let state = State::in_memory().unwrap();
        let loft = "https://retired-route.example";
        let added_at = 100;
        state
            .add_loft_with_retention_and_local_trust(loft, Some([0xA5; 32]), 1, added_at, false)
            .unwrap();
        state.remove_loft(loft, added_at).unwrap();

        let historical = vec![loft.to_owned()];
        let before_deadline = state
            .lofts_for_drain_with_local_trust(added_at + 86_400 - 1)
            .unwrap();
        assert_eq!(
            retired_drain_targets(&historical, &before_deadline),
            vec![(loft.to_owned(), Some([0xA5; 32]), false)]
        );

        let at_deadline = state
            .lofts_for_drain_with_local_trust(added_at + 86_400)
            .unwrap();
        assert!(at_deadline.is_empty());
        assert!(retired_drain_targets(&historical, &at_deadline).is_empty());
    }

    #[derive(Clone)]
    struct FetchBudgetGate {
        started: Arc<AtomicUsize>,
        active: Arc<AtomicUsize>,
        max_active: Arc<AtomicUsize>,
        release: Arc<tokio::sync::Semaphore>,
    }

    #[derive(Clone)]
    struct RecordPublicationGate {
        origin: String,
        pubkey: [u8; 32],
        pause_first_record: Arc<AtomicBool>,
        started: Arc<tokio::sync::Semaphore>,
        release: Arc<tokio::sync::Semaphore>,
        pause_first_policy: Arc<AtomicBool>,
        policy_started: Arc<tokio::sync::Semaphore>,
        policy_release: Arc<tokio::sync::Semaphore>,
    }

    async fn count_policy_request(AxumState(requests): AxumState<Arc<AtomicUsize>>) -> StatusCode {
        requests.fetch_add(1, Ordering::SeqCst);
        StatusCode::NO_CONTENT
    }

    async fn accept_control_request() -> StatusCode {
        StatusCode::NO_CONTENT
    }

    async fn publication_gate_info(
        AxumState(gate): AxumState<RecordPublicationGate>,
    ) -> axum::Json<pigeonpost_loft::wire::InfoResponse> {
        axum::Json(pigeonpost_loft::wire::InfoResponse {
            software: "pigeonpost-loft".into(),
            version: "test".into(),
            protocol: pigeonpost_core::PROTOCOL_VERSION.into(),
            pubkey: hex(&gate.pubkey),
            origin: gate.origin,
            capacity_bytes: 1_000_000,
            used_bytes: 0,
            utilization: 0.0,
            retention_days: 7,
            open: true,
            pow_floor: 0,
            max_event_bytes: 1_000_000,
            event_count: 0,
            accepting: true,
        })
    }

    async fn pause_first_record_publication(
        AxumState(gate): AxumState<RecordPublicationGate>,
    ) -> StatusCode {
        if gate.pause_first_record.swap(false, Ordering::SeqCst) {
            gate.started.add_permits(1);
            let release = gate.release.acquire().await.unwrap();
            release.forget();
        }
        StatusCode::NO_CONTENT
    }

    async fn pause_first_policy_publication(
        AxumState(gate): AxumState<RecordPublicationGate>,
    ) -> StatusCode {
        if gate.pause_first_policy.swap(false, Ordering::SeqCst) {
            gate.policy_started.add_permits(1);
            let release = gate.policy_release.acquire().await.unwrap();
            release.forget();
        }
        StatusCode::NO_CONTENT
    }

    async fn start_record_publication_gate(
    ) -> (String, RecordPublicationGate, tokio::task::JoinHandle<()>) {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let origin = format!("http://{}", listener.local_addr().unwrap());
        let pubkey = SigningKey::from_bytes(&[0x91; 32])
            .verifying_key()
            .to_bytes();
        let gate = RecordPublicationGate {
            origin: origin.clone(),
            pubkey,
            pause_first_record: Arc::new(AtomicBool::new(true)),
            started: Arc::new(tokio::sync::Semaphore::new(0)),
            release: Arc::new(tokio::sync::Semaphore::new(0)),
            pause_first_policy: Arc::new(AtomicBool::new(false)),
            policy_started: Arc::new(tokio::sync::Semaphore::new(0)),
            policy_release: Arc::new(tokio::sync::Semaphore::new(0)),
        };
        let app = Router::new()
            .route("/v1/info", axum::routing::get(publication_gate_info))
            .route("/v1/policy", post(pause_first_policy_publication))
            .route("/v1/agent/{address}", put(pause_first_record_publication))
            .route("/v1/rotation/{address}", put(accept_control_request))
            .with_state(gate.clone());
        let server = tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
        (origin, gate, server)
    }

    async fn gated_fetch(AxumState(gate): AxumState<FetchBudgetGate>) -> axum::Json<FetchResponse> {
        let active = gate.active.fetch_add(1, Ordering::SeqCst) + 1;
        gate.max_active.fetch_max(active, Ordering::SeqCst);
        gate.started.fetch_add(1, Ordering::SeqCst);
        let release = gate.release.acquire().await.unwrap();
        release.forget();
        gate.active.fetch_sub(1, Ordering::SeqCst);
        axum::Json(FetchResponse {
            events: Vec::new(),
            next_cursor: 0,
            more: false,
        })
    }

    async fn start_gated_fetch_route(
        gate: FetchBudgetGate,
    ) -> (String, tokio::task::JoinHandle<()>) {
        let app = Router::new()
            .route("/v1/fetch", post(gated_fetch))
            .route("/v1/policy", post(accept_control_request))
            .route("/v1/agent/{address}", put(accept_control_request))
            .with_state(gate);
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let origin = format!("http://{}", listener.local_addr().unwrap());
        let server = tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
        (origin, server)
    }

    async fn wait_for_count(counter: &AtomicUsize, expected: usize) {
        tokio::time::timeout(Duration::from_secs(60), async {
            while counter.load(Ordering::SeqCst) < expected {
                tokio::task::yield_now().await;
            }
        })
        .await
        .expect("fetch route did not start within the test deadline");
    }

    #[tokio::test]
    async fn network_sources_never_gain_loopback_access_from_their_url() {
        let loopback = "http://127.0.0.1:7717";
        assert!(loft_client_for_route(loopback, false).await.is_err());

        let local = loft_client_for_route(loopback, true).await.unwrap();
        assert_eq!(local.base_url(), loopback);

        assert!(loft_client_for_route("https://127.0.0.1:7717", false)
            .await
            .is_err());
    }

    #[tokio::test]
    async fn multi_route_fetches_share_and_release_one_response_byte_budget() {
        let gate = FetchBudgetGate {
            started: Arc::new(AtomicUsize::new(0)),
            active: Arc::new(AtomicUsize::new(0)),
            max_active: Arc::new(AtomicUsize::new(0)),
            release: Arc::new(tokio::sync::Semaphore::new(0)),
        };
        let mut servers = Vec::new();
        let mut routes = Vec::new();
        for _ in 0..3 {
            let (route, server) = start_gated_fetch_route(gate.clone()).await;
            routes.push(route);
            servers.push(server);
        }

        let per_fetch = pigeonpost_loft::client::MAX_FETCH_RESPONSE_BYTES;
        let capacity = per_fetch * 2;
        let response_budget = Arc::new(tokio::sync::Semaphore::new(capacity));
        let mut fetches = FuturesUnordered::new();
        for url in routes {
            let response_budget = Arc::clone(&response_budget);
            fetches.push(tokio::spawn(async move {
                let identity = Identity::from_seed([0x71; 32]);
                fetch_drain_page(
                    &identity,
                    DrainWork {
                        url,
                        pubkey: Some([0x72; 32]),
                        allow_local: true,
                        cursor: 0,
                        pages: 0,
                    },
                    response_budget,
                )
                .await
            }));
        }

        wait_for_count(&gate.started, 2).await;
        assert_eq!(gate.started.load(Ordering::SeqCst), 2);
        assert_eq!(gate.max_active.load(Ordering::SeqCst), 2);
        assert_eq!(response_budget.available_permits(), 0);

        gate.release.add_permits(2);
        let (_, first) = tokio::time::timeout(Duration::from_secs(60), fetches.next())
            .await
            .unwrap()
            .unwrap()
            .unwrap();
        let first = first.unwrap();
        assert!(first.response.events.is_empty());
        // The decoded response still owns its complete reservation, and the third hostile route
        // therefore cannot begin while the caller has not consumed or dropped it.
        assert_eq!(gate.started.load(Ordering::SeqCst), 2);
        assert_eq!(response_budget.available_permits(), 0);

        drop(first);
        wait_for_count(&gate.started, 3).await;
        assert_eq!(gate.max_active.load(Ordering::SeqCst), 2);

        gate.release.add_permits(1);
        for _ in 0..2 {
            let (_, outcome) = tokio::time::timeout(Duration::from_secs(60), fetches.next())
                .await
                .unwrap()
                .unwrap()
                .unwrap();
            drop(outcome.unwrap());
        }
        assert!(fetches.is_empty());
        assert_eq!(gate.active.load(Ordering::SeqCst), 0);
        assert_eq!(response_budget.available_permits(), capacity);

        for server in servers {
            server.abort();
            let _ = server.await;
        }
    }

    #[tokio::test]
    async fn add_loft_acquires_before_mutation_and_holds_through_record_publication() {
        let (loft, gate, server) = start_record_publication_gate().await;
        let home_root = tempfile::tempdir().unwrap();
        let home = home_root.path().join("agent");
        let publisher = Agent::open(&home).unwrap();
        let source = publisher.address();
        let mut rotator = Agent::open(&home).unwrap();

        let blocker = rotator.identity_operation().unwrap();
        let error = publisher.add_loft(&loft).await.unwrap_err();
        assert!(matches!(
            error,
            ClientError::Config(message) if message.contains("identity is busy")
        ));
        assert!(
            publisher.state.lofts().unwrap().is_empty(),
            "lease contention must be detected before durable loft mutation"
        );
        drop(blocker);

        let add = publisher.add_loft(&loft);
        tokio::pin!(add);
        let record_started = gate.started.clone().acquire_owned();
        tokio::pin!(record_started);
        tokio::select! {
            permit = &mut record_started => permit.unwrap().forget(),
            result = &mut add => panic!("add_loft completed before its record PUT was released: {result:?}"),
        }

        let error = tokio::time::timeout(Duration::from_secs(60), rotator.rotate())
            .await
            .expect("contending rotation did not fail fast")
            .unwrap_err();
        assert!(matches!(
            error,
            ClientError::Config(message) if message.contains("identity is busy")
        ));

        gate.release.add_permits(1);
        tokio::time::timeout(Duration::from_secs(60), &mut add)
            .await
            .expect("add_loft did not finish after record publication resumed")
            .unwrap();

        let rotated = tokio::time::timeout(Duration::from_secs(60), rotator.rotate())
            .await
            .expect("rotation did not resume after add_loft released its lease")
            .unwrap();
        assert_eq!(rotated.from, source);

        server.abort();
        let _ = server.await;
    }

    #[tokio::test]
    async fn signed_mutators_fail_before_mutation_while_rotation_is_paused() {
        let (loft, gate, server) = start_record_publication_gate().await;
        let home_root = tempfile::tempdir().unwrap();
        let home = home_root.path().join("agent");
        let publisher = Agent::open(&home).unwrap();
        publisher
            .state
            .add_loft_with_retention_and_local_trust(&loft, Some(gate.pubkey), 7, now(), true)
            .unwrap();
        publisher.state.set_pow_floor(3).unwrap();
        publisher
            .state
            .set_token_labels(&["keep".to_owned()])
            .unwrap();
        publisher.state.set_token_gate_enabled(true).unwrap();

        let source = publisher.address();
        let mut rotator = Agent::open(&home).unwrap();
        let rotation = rotator.rotate();
        tokio::pin!(rotation);
        let record_started = gate.started.clone().acquire_owned();
        tokio::pin!(record_started);
        tokio::select! {
            permit = &mut record_started => permit.unwrap().forget(),
            result = &mut rotation => panic!("rotation completed before its record PUT was released: {result:?}"),
        }

        let error = publisher.set_pow_floor(7).await.unwrap_err();
        assert!(matches!(
            error,
            ClientError::Config(message) if message.contains("identity is busy")
        ));
        assert_eq!(publisher.pow_floor().unwrap(), 3);

        let error = publisher.publish_token("new").await.unwrap_err();
        assert!(matches!(
            error,
            ClientError::Config(message) if message.contains("identity is busy")
        ));
        assert_eq!(publisher.token_labels().unwrap(), vec!["keep"]);
        assert!(publisher.state.token_gate_enabled().unwrap());

        let error = publisher.revoke_token("keep").await.unwrap_err();
        assert!(matches!(
            error,
            ClientError::Config(message) if message.contains("identity is busy")
        ));
        assert_eq!(publisher.token_labels().unwrap(), vec!["keep"]);

        let error = publisher.set_token_gate(false).await.unwrap_err();
        assert!(matches!(
            error,
            ClientError::Config(message) if message.contains("identity is busy")
        ));
        assert!(publisher.state.token_gate_enabled().unwrap());

        let error = publisher.remove_loft(&loft).await.unwrap_err();
        assert!(matches!(
            error,
            ClientError::Config(message) if message.contains("identity is busy")
        ));
        assert_eq!(publisher.state.lofts().unwrap().len(), 1);

        assert_eq!(publisher.bootstrap_lofts().await.unwrap(), 0);
        assert_eq!(publisher.state.lofts().unwrap().len(), 1);

        gate.release.add_permits(1);
        let rotated = tokio::time::timeout(Duration::from_secs(60), &mut rotation)
            .await
            .expect("rotation did not resume after record publication was released")
            .unwrap();
        assert_eq!(rotated.from, source);

        server.abort();
        let _ = server.await;
    }

    #[tokio::test]
    async fn bootstrap_lofts_fails_busy_before_a_verified_route_is_added() {
        let (loft, gate, loft_server) = start_record_publication_gate().await;
        let directory = Arc::new(Directory::in_memory().unwrap());
        let loft_key = SigningKey::from_bytes(&[0x91; 32]);
        let entry = DirectoryEntry::signed(
            &loft_key,
            &loft,
            None,
            100,
            7,
            LoftPolicy {
                open: true,
                pow_floor: 0,
                max_event_bytes: 64 * 1024,
            },
            0.0,
        );
        assert_eq!(entry.pubkey, hex(&gate.pubkey));
        directory.submit(entry, now()).unwrap();
        directory.set_state(&loft, LoftState::Active).unwrap();
        let directory_key = directory.signing_public_key();
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let directory_url = format!("http://{}", listener.local_addr().unwrap());
        let (stop, stopped) = tokio::sync::oneshot::channel();
        let directory_server = tokio::spawn({
            let directory = Arc::clone(&directory);
            async move {
                pigeonpost_directory::serve_loopback_test(listener, directory, async {
                    let _ = stopped.await;
                })
                .await
                .unwrap();
            }
        });

        let home_root = tempfile::tempdir().unwrap();
        let home = home_root.path().join("agent");
        let publisher = Agent::open(&home).unwrap();
        publisher
            .add_directory(&directory_url, directory_key)
            .await
            .unwrap();
        let blocker_owner = Agent::open(&home).unwrap();
        let blocker = blocker_owner.identity_operation().unwrap();

        let error = publisher.bootstrap_lofts().await.unwrap_err();
        assert!(matches!(
            error,
            ClientError::Config(message) if message.contains("identity is busy")
        ));
        assert!(
            publisher.state.lofts().unwrap().is_empty(),
            "bootstrap contention must be detected before a verified route is persisted"
        );

        drop(blocker);
        let _ = stop.send(());
        directory_server.await.unwrap();
        loft_server.abort();
        let _ = loft_server.await;
    }

    #[tokio::test]
    async fn policy_and_routing_mutators_hold_the_lease_through_publication() {
        let (loft, gate, server) = start_record_publication_gate().await;
        let home_root = tempfile::tempdir().unwrap();
        let home = home_root.path().join("agent");
        let publisher = Agent::open(&home).unwrap();
        publisher
            .state
            .add_loft_with_retention_and_local_trust(&loft, Some(gate.pubkey), 7, now(), true)
            .unwrap();
        let mut rotator = Agent::open(&home).unwrap();

        let set_pow = publisher.set_pow_floor(7);
        tokio::pin!(set_pow);
        let record_started = gate.started.clone().acquire_owned();
        tokio::pin!(record_started);
        tokio::select! {
            permit = &mut record_started => permit.unwrap().forget(),
            result = &mut set_pow => panic!("PoW update completed before record publication was released: {result:?}"),
        }
        let error = tokio::time::timeout(Duration::from_secs(10), rotator.rotate())
            .await
            .expect("rotation must not wait for the active identity lease")
            .unwrap_err();
        assert!(matches!(
            error,
            ClientError::Config(message) if message.contains("identity is busy")
        ));
        gate.release.add_permits(1);
        tokio::time::timeout(Duration::from_secs(60), &mut set_pow)
            .await
            .expect("PoW update did not resume after record publication was released")
            .unwrap();
        assert_eq!(
            publisher
                .state
                .record_publication()
                .unwrap()
                .unwrap()
                .record
                .pow_min,
            7
        );

        publisher
            .state
            .set_token_labels(&["keep".to_owned()])
            .unwrap();
        gate.pause_first_policy.store(true, Ordering::SeqCst);
        let revoke = publisher.revoke_token("keep");
        tokio::pin!(revoke);
        let policy_started = gate.policy_started.clone().acquire_owned();
        tokio::pin!(policy_started);
        tokio::select! {
            permit = &mut policy_started => permit.unwrap().forget(),
            result = &mut revoke => panic!("token revocation completed before policy publication was released: {result:?}"),
        }
        let error = tokio::time::timeout(Duration::from_secs(10), rotator.rotate())
            .await
            .expect("rotation must not wait for the active identity lease")
            .unwrap_err();
        assert!(matches!(
            error,
            ClientError::Config(message) if message.contains("identity is busy")
        ));
        gate.policy_release.add_permits(1);
        tokio::time::timeout(Duration::from_secs(60), &mut revoke)
            .await
            .expect("token revocation did not resume after policy publication was released")
            .unwrap();
        assert!(publisher.token_labels().unwrap().is_empty());

        gate.pause_first_record.store(true, Ordering::SeqCst);
        let remove = publisher.remove_loft(&loft);
        tokio::pin!(remove);
        let record_started = gate.started.clone().acquire_owned();
        tokio::pin!(record_started);
        tokio::select! {
            permit = &mut record_started => permit.unwrap().forget(),
            result = &mut remove => panic!("Loft removal completed before record publication was released: {result:?}"),
        }
        let error = rotator.rotate().await.unwrap_err();
        assert!(matches!(
            error,
            ClientError::Config(message) if message.contains("identity is busy")
        ));
        gate.release.add_permits(1);
        assert!(tokio::time::timeout(Duration::from_secs(60), &mut remove)
            .await
            .expect("Loft removal did not resume after record publication was released")
            .unwrap());
        assert!(publisher.state.lofts().unwrap().is_empty());

        server.abort();
        let _ = server.await;
    }

    #[tokio::test]
    async fn shared_home_scope_change_cannot_cross_an_in_flight_drain_lease() {
        let gate = FetchBudgetGate {
            started: Arc::new(AtomicUsize::new(0)),
            active: Arc::new(AtomicUsize::new(0)),
            max_active: Arc::new(AtomicUsize::new(0)),
            release: Arc::new(tokio::sync::Semaphore::new(0)),
        };
        let (route, server) = start_gated_fetch_route(gate.clone()).await;
        let home_root = tempfile::tempdir().unwrap();
        let home = home_root.path().join("agent");
        let draining = Agent::open(&home).unwrap();
        let requirement = AttributionRequirement::new(Jurisdiction::Test, [0x81; 32]);
        draining
            .state
            .set_recipient_attribution_requirement(Some(requirement))
            .unwrap();
        draining
            .state
            .add_loft_with_local_trust(&route, Some([0x82; 32]), 1, true)
            .unwrap();
        let changing = Agent::open(&home).unwrap();

        let drain = async {
            draining
                .drain_with_limits(WakeupLimits::new(1, Duration::from_secs(5)).unwrap())
                .await
        };
        let concurrent_change = async {
            wait_for_count(&gate.started, 1).await;
            let error = changing
                .set_attribution_requirement(None)
                .await
                .unwrap_err();
            assert!(matches!(
                error,
                ClientError::Config(message) if message.contains("identity is busy")
            ));
            assert_eq!(
                changing.attribution_requirement().unwrap(),
                Some(requirement),
                "a refused concurrent change must not mutate the durable receive scope"
            );
            gate.release.add_permits(1);
        };
        let (drained, ()) = tokio::join!(drain, concurrent_change);
        let drained = drained.unwrap();
        assert!(drained.lofts_failed.is_empty());
        changing.set_attribution_requirement(None).await.unwrap();
        assert_eq!(changing.attribution_requirement().unwrap(), None);

        server.abort();
        let _ = server.await;
    }

    #[test]
    fn v0_2_pow_support_has_an_exact_fail_closed_boundary() {
        assert!(validate_supported_pow_floor(spam::MAX_SUPPORTED_POW_BITS).is_ok());
        assert!(validate_supported_pow_floor(spam::MAX_SUPPORTED_POW_BITS + 1).is_err());
    }

    #[tokio::test(flavor = "current_thread")]
    async fn bounded_pow_mining_keeps_the_async_runtime_responsive() {
        let _serial = POW_TEST_SERIAL.lock().await;
        // This fixed vector's first 18-bit nonce is 1,077,137. It is deliberately long enough
        // that an inline loop would finish before the single-threaded runtime could poll `tick`.
        let finished = Arc::new(AtomicBool::new(false));
        let finished_after_mining = Arc::clone(&finished);
        let mining = async move {
            // Keep the production ten-second bound unchanged while giving this scheduler test
            // enough headroom when the complete workspace suite is saturating the host.
            let nonce = mine_pow_nonce_with_deadline(
                [6; 32],
                spam::MAX_SUPPORTED_POW_BITS,
                Duration::from_secs(60),
            )
            .await
            .unwrap();
            finished_after_mining.store(true, Ordering::SeqCst);
            nonce
        };
        let tick = async {
            tokio::time::sleep(Duration::from_millis(1)).await;
            assert!(
                !finished.load(Ordering::SeqCst),
                "proof-of-work monopolized the async runtime"
            );
        };
        let (nonce, ()) = tokio::join!(mining, tick);
        assert_eq!(nonce, 1_077_137);
        assert!(
            pigeonpost_core::pow::verify(&[6; 32], nonce, spam::MAX_SUPPORTED_POW_BITS).is_ok()
        );
    }

    #[tokio::test(flavor = "current_thread")]
    async fn process_wide_pow_capacity_is_bounded_and_fail_fast() {
        let _serial = POW_TEST_SERIAL.lock().await;
        let release = Arc::new(AtomicBool::new(false));
        let release_on_unwind = BlockingWorkerRelease(Arc::clone(&release));

        // Reserve the entire process-wide capacity atomically so a concurrent test cannot leave a
        // partial reservation behind. The permit lives in the blocking closure until it exits.
        let permits = Arc::clone(&POW_MINING_SLOTS)
            .acquire_many_owned(MAX_CONCURRENT_POW_MINERS as u32)
            .await
            .unwrap();
        let (started_tx, started_rx) = tokio::sync::oneshot::channel();
        let worker_release = Arc::clone(&release);
        let worker = tokio::task::spawn_blocking(move || {
            let _permits = permits;
            let _ = started_tx.send(());
            while !worker_release.load(Ordering::SeqCst) {
                std::thread::sleep(Duration::from_millis(1));
            }
        });
        started_rx.await.unwrap();
        assert_eq!(POW_MINING_SLOTS.available_permits(), 0);

        let heartbeat = Arc::new(AtomicBool::new(false));
        let beat = Arc::clone(&heartbeat);
        let ticker = tokio::spawn(async move {
            tokio::time::sleep(Duration::from_millis(1)).await;
            beat.store(true, Ordering::SeqCst);
        });
        let error = mine_pow_nonce_with_budget([6; 32], spam::MAX_SUPPORTED_POW_BITS)
            .await
            .unwrap_err();
        assert!(matches!(
            error,
            ClientError::Config(message) if message.contains("capacity is busy")
        ));
        ticker.await.unwrap();
        assert!(heartbeat.load(Ordering::SeqCst));
        assert_eq!(POW_MINING_SLOTS.available_permits(), 0);

        release.store(true, Ordering::SeqCst);
        worker.await.unwrap();
        drop(release_on_unwind);
        assert_eq!(
            POW_MINING_SLOTS.available_permits(),
            MAX_CONCURRENT_POW_MINERS
        );
    }

    #[test]
    fn rotation_between_wrap_and_commit_leaves_no_outbox_or_reply_trust() {
        let dir = tempfile::tempdir().unwrap();
        let home = dir.path().join("agent");
        let stale = Agent::open(&home).unwrap();
        let current = Agent::open(&home).unwrap();
        let recipient = Identity::from_seed([0xE1; 32]);

        let wrap = {
            let _lease = stale.active_identity_lease().unwrap();
            envelope::wrap(
                &stale.identity,
                &recipient.verifying_key(),
                "race boundary",
                now(),
            )
            .unwrap()
        };
        let source = AgentRecord::new(&current.identity, &current.successor, 1, Vec::new());
        keystore::rotate(&current.key_paths, &source, &[], now()).unwrap();

        let recipient_key = recipient.verifying_key().to_bytes();
        let routes = [OutboxRoute::new("https://loft.example", false)];
        let error = stale
            .commit_outbound(
                &hex(&wrap.id()),
                recipient.address().as_str(),
                &routes,
                &wrap,
                None,
                &recipient_key,
                now(),
            )
            .unwrap_err();
        assert!(matches!(
            error,
            ClientError::Config(message) if message.contains("reopen")
        ));
        assert_eq!(stale.state.pending_count().unwrap(), 0);
        assert!(!stale.state.is_allowed(&recipient_key).unwrap());
        assert_eq!(stale.state.score(&recipient_key).unwrap(), (0, 0));
    }

    #[tokio::test]
    async fn claim_signature_lease_blocks_rotation_until_publication_confirmation() {
        let dir = tempfile::tempdir().unwrap();
        let home = dir.path().join("agent");
        let publisher = Agent::open(&home).unwrap();
        let mut rotator = Agent::open(&home).unwrap();
        let handle = pigeonpost_registry::Handle::parse("/github/lease-race").unwrap();

        // Deterministic pause immediately after the claim signature is created. The same guard
        // used by CLI/MCP registration remains live while a second Agent attempts rotation.
        let operation = publisher.identity_operation().unwrap();
        let signed_key = operation.verifying_key();
        let payload =
            pigeonpost_registry::entry::claim_payload(&handle.as_path(), signed_key.as_bytes());
        let signature = operation.sign(&payload);

        let error = tokio::time::timeout(Duration::from_secs(10), rotator.rotate())
            .await
            .expect("rotation must not wait for the active identity lease")
            .unwrap_err();
        assert!(matches!(
            error,
            ClientError::Config(message) if message.contains("identity is busy")
        ));

        // Resume the publication operation: its signature is still made by the active key, and
        // no rotation record could have been committed while the external append was in flight.
        signed_key.verify_strict(&payload, &signature).unwrap();
        assert_eq!(operation.address(), publisher.address());
        assert!(publisher.state().own_rotations().unwrap().is_empty());
        drop(operation);

        let reopened = Agent::open(&home).unwrap();
        assert_eq!(reopened.verifying_key(), signed_key);
        assert!(reopened.state().own_rotations().unwrap().is_empty());
    }

    #[test]
    fn candidate_sets_are_bounded_and_only_independent_sources_upgrade_trust() {
        let mut candidates = Vec::new();
        push_candidate(&mut candidates, "http://127.0.0.1:7717".into(), false);
        assert!(!candidates[0].allow_local);

        // This `true` models the independent configured-loft/directory provenance established by
        // the caller; merely seeing the URL in a hint always passes `false`.
        push_candidate(&mut candidates, "http://127.0.0.1:7717".into(), true);
        assert!(candidates[0].allow_local);

        for index in 0..(MAX_RESOLUTION_CANDIDATES + 10) {
            push_candidate(
                &mut candidates,
                format!("https://loft-{index}.example"),
                false,
            );
        }
        assert_eq!(candidates.len(), MAX_RESOLUTION_CANDIDATES);
    }

    fn ranked_test_candidates(count: usize) -> Vec<LoftCandidate> {
        (0..count)
            .map(|index| LoftCandidate {
                url: format!("https://r{index}.example"),
                allow_local: false,
            })
            .collect()
    }

    fn test_resolution_record(identity: &Identity, seq: u64, loft: &str) -> AgentRecord {
        let successor =
            SuccessorCommitment::for_key(&Identity::from_seed([0xD2; 32]).verifying_key());
        AgentRecord::new(identity, &successor, seq, vec![loft.into()])
    }

    fn placement_pool(endpoints: &[(&str, u8)]) -> DirectoryPool {
        let entries = endpoints
            .iter()
            .map(|(endpoint, seed)| {
                let mut entry = DirectoryEntry::signed(
                    &SigningKey::from_bytes(&[*seed; 32]),
                    endpoint,
                    Some(format!("/github/operator{seed}")),
                    100,
                    30,
                    LoftPolicy {
                        open: true,
                        pow_floor: 0,
                        max_event_bytes: 64 * 1024,
                    },
                    0.0,
                );
                entry.state = LoftState::Active;
                entry
            })
            .collect();
        DirectoryPool {
            entries,
            locally_trusted: HashSet::new(),
        }
    }

    #[tokio::test]
    async fn directory_membership_shift_reuses_exact_record_and_publishes_only_new_target() {
        let dir = tempfile::tempdir().unwrap();
        let agent = Agent::open(&dir.path().join("agent")).unwrap();
        agent
            .state
            .add_loft_with_local_trust("https://own.example", None, now(), false)
            .unwrap();
        let pool_a = placement_pool(&[
            ("https://r1.example", 1),
            ("https://r2.example", 2),
            ("https://r3.example", 3),
        ]);
        let first = agent
            .prepare_record_publication(&pool_a, now())
            .unwrap()
            .unwrap();
        agent
            .attempt_record_publication_with(
                tokio::time::Instant::now() + Duration::from_secs(60),
                MAX_PLACEMENT_ATTEMPTS_PER_WAKE,
                |_address, _record, _target| async { true },
            )
            .await
            .unwrap();
        assert_eq!(agent.placement_status().unwrap().record_pending, 0);

        let pool_b = placement_pool(&[
            ("https://r2.example", 2),
            ("https://r3.example", 3),
            ("https://r4.example", 4),
        ]);
        let shifted = agent
            .prepare_record_publication(&pool_b, now())
            .unwrap()
            .unwrap();
        assert_eq!(shifted.record, first.record, "membership does not resign");
        let contacted = Arc::new(Mutex::new(Vec::new()));
        let observed = Arc::clone(&contacted);
        agent
            .attempt_record_publication_with(
                tokio::time::Instant::now() + Duration::from_secs(60),
                MAX_PLACEMENT_ATTEMPTS_PER_WAKE,
                move |_address, _record, target| {
                    let observed = Arc::clone(&observed);
                    async move {
                        observed.lock().unwrap().push(target.url);
                        true
                    }
                },
            )
            .await
            .unwrap();
        assert_eq!(
            *contacted.lock().unwrap(),
            vec!["https://r4.example"],
            "completed unchanged placements are never republished"
        );
        let fresh_reply = ResolutionReply {
            endpoint: "https://r4.example".into(),
            record: Some(shifted.record.clone()),
            rotation: None,
        };
        let (records, _) = resolution_data(&[fresh_reply]);
        assert_eq!(
            highest_unambiguous_record(&records).unwrap(),
            Some(&shifted.record),
            "a fresh sender can accept the exact record from the new rendezvous target"
        );
    }

    #[tokio::test]
    async fn own_loft_success_and_three_rendezvous_failures_recover_after_restart() {
        let dir = tempfile::tempdir().unwrap();
        let home = dir.path().join("agent");
        let agent = Agent::open(&home).unwrap();
        agent
            .state
            .add_loft_with_local_trust("https://own.example", None, now(), false)
            .unwrap();
        let pool = placement_pool(&[
            ("https://r1.example", 1),
            ("https://r2.example", 2),
            ("https://r3.example", 3),
        ]);
        agent.prepare_record_publication(&pool, now()).unwrap();
        agent
            .attempt_record_publication_with(
                tokio::time::Instant::now() + Duration::from_secs(60),
                MAX_PLACEMENT_ATTEMPTS_PER_WAKE,
                |_address, _record, target| async move { target.url == "https://own.example" },
            )
            .await
            .unwrap();
        let partial = agent.placement_status().unwrap();
        assert_eq!(partial.rendezvous_targets, 3);
        assert_eq!(partial.rendezvous_pending, 3);
        drop(agent);

        let reopened = Agent::open(&home).unwrap();
        reopened
            .attempt_record_publication_with(
                tokio::time::Instant::now() + Duration::from_secs(60),
                MAX_PLACEMENT_ATTEMPTS_PER_WAKE,
                |_address, _record, _target| async { true },
            )
            .await
            .unwrap();
        let recovered = reopened.placement_status().unwrap();
        assert_eq!(recovered.record_pending, 0);
        assert_eq!(recovered.rendezvous_pending, 0);
    }

    #[tokio::test]
    async fn partial_rotation_bundle_retries_exact_bytes_after_restart() {
        let dir = tempfile::tempdir().unwrap();
        let home = dir.path().join("agent");
        let agent = Agent::open(&home).unwrap();
        let outgoing = Identity::from_seed([0xE2; 32]);
        let incoming = Identity::from_seed([0xE3; 32]);
        let next = Identity::from_seed([0xE4; 32]);
        let source_successor = SuccessorCommitment::for_key(&incoming.verifying_key());
        let next_successor = SuccessorCommitment::for_key(&next.verifying_key());
        let source = AgentRecord::new(
            &outgoing,
            &source_successor,
            8,
            vec!["https://own.example".into()],
        );
        let rotation =
            RotationRecord::new(&outgoing, &incoming, &next_successor, 9, now()).unwrap();
        let target = AgentRecord::new(
            &incoming,
            &next_successor,
            9,
            vec!["https://own.example".into()],
        );
        let targets = vec![
            PublicationTarget::pending("https://r1.example".into(), false, true),
            PublicationTarget::pending("https://r2.example".into(), false, true),
            PublicationTarget::pending("https://r3.example".into(), false, true),
        ];
        let from = outgoing.address();
        agent
            .state
            .save_own_rotation(
                &rotation,
                &source,
                &target,
                &targets
                    .iter()
                    .map(|target| target.url.clone())
                    .collect::<Vec<_>>(),
            )
            .unwrap();
        agent
            .state
            .sync_own_rotation_targets(&from, &targets)
            .unwrap();
        agent
            .attempt_rotation_publication_with(
                tokio::time::Instant::now() + Duration::from_secs(60),
                MAX_PLACEMENT_ATTEMPTS_PER_WAKE,
                |_rotation, target| async move { target.url == "https://r1.example" },
            )
            .await
            .unwrap();
        assert_eq!(agent.state.rotation_target_progress(&from).unwrap(), (1, 2));
        drop(agent);

        let reopened = Agent::open(&home).unwrap();
        let seen_sequences = Arc::new(Mutex::new(Vec::new()));
        let observed = Arc::clone(&seen_sequences);
        reopened
            .attempt_rotation_publication_with(
                tokio::time::Instant::now() + Duration::from_secs(60),
                MAX_PLACEMENT_ATTEMPTS_PER_WAKE,
                move |bundle, _target| {
                    let observed = Arc::clone(&observed);
                    async move {
                        observed.lock().unwrap().push(bundle.record.seq);
                        true
                    }
                },
            )
            .await
            .unwrap();
        assert_eq!(
            *seen_sequences.lock().unwrap(),
            vec![rotation.seq, rotation.seq]
        );
        assert_eq!(
            reopened.state.rotation_target_progress(&from).unwrap(),
            (3, 0)
        );
    }

    #[tokio::test]
    async fn ranked_resolution_stops_after_three_successful_primary_contacts() {
        let identity = Identity::from_seed([0xD1; 32]);
        let address = identity.address();
        let record = test_resolution_record(&identity, 1, "https://mailbox.example");
        let contacts = Arc::new(Mutex::new(Vec::new()));
        let observed = Arc::clone(&contacts);
        let replies = fetch_ranked_resolution_walk(
            &address,
            &ranked_test_candidates(12),
            None,
            move |_address, candidates| {
                let observed = Arc::clone(&observed);
                let record = record.clone();
                async move {
                    candidates
                        .into_iter()
                        .map(|candidate| {
                            observed.lock().unwrap().push(candidate.url.clone());
                            ResolutionReply {
                                endpoint: candidate.url,
                                record: Some(record.clone()),
                                rotation: None,
                            }
                        })
                        .collect()
                }
            },
        )
        .await
        .unwrap();
        assert_eq!(replies.len(), 3);
        assert_eq!(
            *contacts.lock().unwrap(),
            vec![
                "https://r0.example",
                "https://r1.example",
                "https://r2.example"
            ]
        );
    }

    #[tokio::test]
    async fn ranked_resolution_fills_only_missing_slots_and_accepts_newer_fallback() {
        let identity = Identity::from_seed([0xD3; 32]);
        let address = identity.address();
        let stale = test_resolution_record(&identity, 1, "https://old.example");
        let fresh = test_resolution_record(&identity, 2, "https://new.example");
        let contacts = Arc::new(Mutex::new(Vec::new()));
        let observed = Arc::clone(&contacts);
        let replies = fetch_ranked_resolution_walk(
            &address,
            &ranked_test_candidates(12),
            None,
            move |_address, candidates| {
                let observed = Arc::clone(&observed);
                let stale = stale.clone();
                let fresh = fresh.clone();
                async move {
                    candidates
                        .into_iter()
                        .map(|candidate| {
                            observed.lock().unwrap().push(candidate.url.clone());
                            let record = match candidate.url.as_str() {
                                "https://r0.example" => Some(stale.clone()),
                                "https://r3.example" | "https://r4.example" => Some(fresh.clone()),
                                _ => None,
                            };
                            ResolutionReply {
                                endpoint: candidate.url,
                                record,
                                rotation: None,
                            }
                        })
                        .collect()
                }
            },
        )
        .await
        .unwrap();
        let (records, _) = resolution_data(&replies);
        assert_eq!(
            highest_unambiguous_record(&records).unwrap().unwrap().seq,
            2
        );
        assert_eq!(
            *contacts.lock().unwrap(),
            vec![
                "https://r0.example",
                "https://r1.example",
                "https://r2.example",
                "https://r3.example",
                "https://r4.example"
            ]
        );
    }

    #[tokio::test]
    async fn ranked_resolution_all_misses_stop_at_twelve() {
        let identity = Identity::from_seed([0xD4; 32]);
        let contacts = Arc::new(Mutex::new(Vec::new()));
        let observed = Arc::clone(&contacts);
        let replies = fetch_ranked_resolution_walk(
            &identity.address(),
            &ranked_test_candidates(20),
            None,
            move |_address, candidates| {
                let observed = Arc::clone(&observed);
                async move {
                    candidates
                        .into_iter()
                        .map(|candidate| {
                            observed.lock().unwrap().push(candidate.url.clone());
                            ResolutionReply {
                                endpoint: candidate.url,
                                record: None,
                                rotation: None,
                            }
                        })
                        .collect()
                }
            },
        )
        .await
        .unwrap();
        assert_eq!(replies.len(), RENDEZVOUS_WALK);
        assert_eq!(contacts.lock().unwrap().len(), RENDEZVOUS_WALK);
    }

    #[tokio::test]
    async fn explicit_hint_bypasses_directory_pool_loading() {
        let loads = Arc::new(AtomicUsize::new(0));
        let observed = Arc::clone(&loads);
        let pool = optional_resolution_pool(Some("https://hint.example"), move || {
            let observed = Arc::clone(&observed);
            async move {
                observed.fetch_add(1, Ordering::SeqCst);
                Ok(DirectoryPool {
                    entries: Vec::new(),
                    locally_trusted: HashSet::new(),
                })
            }
        })
        .await
        .unwrap();
        assert!(pool.is_none());
        assert_eq!(loads.load(Ordering::SeqCst), 0);
    }

    #[test]
    fn lower_sequence_equivocation_is_rejected_even_with_a_higher_record() {
        let identity = Identity::from_seed([0xD5; 32]);
        let lower_a = test_resolution_record(&identity, 3, "https://a.example");
        let lower_b = test_resolution_record(&identity, 3, "https://b.example");
        let higher = test_resolution_record(&identity, 4, "https://c.example");
        assert!(matches!(
            highest_unambiguous_record(&[lower_a, higher, lower_b]),
            Err(ClientError::Core(pigeonpost_core::Error::StaleSequence))
        ));
    }

    #[test]
    fn only_unambiguous_numeric_loopback_directory_origins_are_locally_trusted() {
        assert!(exact_loopback_directory("http://127.0.0.1:7718"));
        assert!(exact_loopback_directory("https://[::1]:7718/"));

        for origin in [
            "http://localhost:7718",
            "http://192.168.1.1:7718",
            "ftp://127.0.0.1:7718",
            "http://user@127.0.0.1:7718",
            "http://127.0.0.1:7718/internal",
            "http://127.0.0.1:7718?next=public",
        ] {
            assert!(!exact_loopback_directory(origin), "{origin}");
        }
    }

    #[test]
    fn delivery_status_taxonomy_is_explicit_and_response_text_is_ignored() {
        for status in [408, 409, 425, 429, 500, 503, 599] {
            let error = ClientError::Loft(pigeonpost_loft::ClientError::Refused {
                status,
                message: "untrusted sensitive detail".into(),
            });
            assert_eq!(
                delivery_failure(&error),
                (DeliveryDisposition::Retryable, format!("http_{status}"))
            );
        }
        for status in [300, 400, 401, 403, 404, 422, 451] {
            let error = ClientError::Loft(pigeonpost_loft::ClientError::Refused {
                status,
                message: "untrusted sensitive detail".into(),
            });
            assert_eq!(
                delivery_failure(&error),
                (DeliveryDisposition::Terminal, format!("http_{status}"))
            );
        }
        assert_eq!(
            delivery_failure(&ClientError::Config("private route".into())),
            (DeliveryDisposition::Terminal, "configuration".into())
        );
        assert_eq!(
            delivery_failure(&ClientError::Loft(
                pigeonpost_loft::ClientError::ProtocolMismatch,
            )),
            (DeliveryDisposition::Terminal, "protocol".into())
        );
    }

    #[test]
    fn wakeup_limits_reject_unbounded_or_zero_work() {
        assert!(WakeupLimits::new(0, Duration::from_secs(1)).is_err());
        assert!(WakeupLimits::new(MAX_WAKEUP_CONCURRENCY + 1, Duration::from_secs(1)).is_err());
        assert!(WakeupLimits::new(1, Duration::ZERO).is_err());
        assert!(WakeupLimits::new(1, MAX_WAKEUP_TIMEOUT + Duration::from_secs(1)).is_err());
        assert!(WakeupLimits::new(1, Duration::from_secs(1)).is_ok());
    }

    #[tokio::test]
    async fn malformed_security_state_is_never_resigned_or_sent_to_a_loft() {
        let requests = Arc::new(AtomicUsize::new(0));
        let app = Router::new()
            .route("/v1/policy", post(count_policy_request))
            .with_state(Arc::clone(&requests));
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let loft_url = format!("http://{}", listener.local_addr().unwrap());
        let server = tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });

        let home_root = tempfile::tempdir().unwrap();
        let home = home_root.path().join("agent");
        let agent = Agent::open(&home).unwrap();
        agent
            .state
            .add_loft_with_local_trust(&loft_url, Some([0xA5; 32]), now(), true)
            .unwrap();
        assert!(matches!(
            agent.set_pow_floor(crate::state::MAX_POW_FLOOR + 1).await,
            Err(ClientError::Config(_))
        ));
        assert_eq!(agent.state.get_meta("pow_floor").unwrap(), None);

        for (key, malformed, restored) in [
            ("accept_all", "TRUE", "false"),
            ("pow_floor", "01", "0"),
            ("token_labels", "[\"same\",\"same\"]", "[]"),
            ("token_gate_enabled", "1", "false"),
            ("attribution_required", "yes", "false"),
        ] {
            agent.state.set_meta(key, malformed).unwrap();
            assert!(matches!(
                agent.sync_policy().await,
                Err(ClientError::Config(_))
            ));
            assert_eq!(
                requests.load(Ordering::SeqCst),
                0,
                "network mutation for {key}"
            );
            assert_eq!(agent.state.get_meta("policy_seq").unwrap(), None);
            assert_eq!(
                agent.state.get_meta(key).unwrap().as_deref(),
                Some(malformed),
                "{key} was normalized after failure"
            );
            agent.state.set_meta(key, restored).unwrap();
        }

        server.abort();
    }

    #[cfg(unix)]
    #[test]
    fn agent_home_is_descriptor_secured_and_symlinks_are_refused() {
        use std::os::unix::fs::{symlink, MetadataExt, PermissionsExt};

        let parent = tempfile::tempdir().unwrap();
        let unsafe_home = parent.path().join("unsafe-agent-home");
        std::fs::create_dir(&unsafe_home).unwrap();
        std::fs::set_permissions(&unsafe_home, std::fs::Permissions::from_mode(0o777)).unwrap();
        assert!(matches!(
            secure_agent_home(&unsafe_home),
            Err(ClientError::Config(_))
        ));

        let home = parent.path().join("agent-home");
        std::fs::create_dir(&home).unwrap();
        std::fs::set_permissions(&home, std::fs::Permissions::from_mode(0o700)).unwrap();
        secure_agent_home(&home).unwrap();
        let secured = std::fs::metadata(&home).unwrap();
        assert_eq!(secured.uid(), rustix::process::geteuid().as_raw());
        assert_eq!(secured.mode() & 0o777, 0o700);

        let link = parent.path().join("agent-home-link");
        symlink(&home, &link).unwrap();
        assert!(matches!(
            secure_agent_home(&link),
            Err(ClientError::Config(_))
        ));
    }
}
