//! Bounded, trust-pinned registry reads for ordinary Pigeonpost clients.
//!
//! A resolve response, its Merkle proof, and the root returned beside that proof all come from the
//! same server and therefore do not authenticate one another. This client starts from an
//! out-of-band checkpoint key and checkpoint pin, verifies a fresh C2SP witness quorum, verifies
//! consistency from the last accepted pin, rebuilds the exact witnessed log prefix, derives its
//! latest valid state, and only then returns a key.

use std::collections::HashMap;
use std::io;
use std::net::{IpAddr, SocketAddr};
use std::sync::{Arc, OnceLock};
use std::time::Duration;

use ed25519_dalek::{VerifyingKey, PUBLIC_KEY_LENGTH};
use pigeonpost_compliance_format::{validate_compliance_epoch, ComplianceKeyId};
use pigeonpost_core::{
    keys,
    network::{is_localhost_name, is_public_network_address},
};
use reqwest::dns::{Addrs, Name, Resolve, Resolving};
use reqwest::{Client, StatusCode, Url};
use serde::de::DeserializeOwned;
use serde::{Deserialize, Serialize};
use tokio::sync::{OwnedSemaphorePermit, Semaphore};

use crate::checkpoint::{witness_quorum_intersects, Checkpoint, VerifiedCheckpoint, WitnessKey};
use crate::entry::{ComplianceKeyPublish, ComplianceKeyStatus, EntryKind, LogEntry};
use crate::error::{RegistryError, Result};
use crate::handle::Handle;
use crate::log::{
    empty_root, leaf_hash, verify_consistency, verify_inclusion, Hash, MerkleFrontier,
};
use crate::AUDIT_DUMP_SEGMENT_ENTRIES;

const MAX_URL_BYTES: usize = 2_048;
const MAX_RESPONSE_BYTES: usize = 1024 * 1024;
const MAX_CHECKPOINT_NOTE_BYTES: usize = 64 * 1024;
const MAX_COMPLIANCE_KEYS: usize = 4_096;
const AUDIT_PAGE_ENTRIES: u64 = 256;
const MAX_AUDIT_DUMP_LINE_BYTES: usize = 64 * 1024;
const MAX_AUDIT_SEGMENT_BYTES: u64 = 32 * 1024 * 1024;
#[cfg(test)]
const V0_2_AUDIT_MAX_LEAVES: u64 = 1_000_000;
#[cfg(test)]
const V0_2_AUDIT_MAX_CANONICAL_BYTES: u64 = 256 * 1024 * 1024;
#[cfg(test)]
const V0_2_AUDIT_MIN_EFFECTIVE_BYTES_PER_SEC: u64 = 10 * 1024 * 1024;
#[cfg(test)]
const V0_2_AUDIT_MAX_RTT: Duration = Duration::from_millis(100);
const CONNECT_TIMEOUT: Duration = Duration::from_secs(5);
const REQUEST_TIMEOUT: Duration = Duration::from_secs(15);
const DNS_TIMEOUT: Duration = Duration::from_secs(3);
const MAX_RESOLVED_ADDRESSES: usize = 16;
const AUDIT_SEGMENT_TIMEOUT: Duration = Duration::from_secs(30);
#[cfg(not(test))]
const MAX_CONCURRENT_REGISTRY_AUDITS: usize = 4;
#[cfg(test)]
const MAX_CONCURRENT_REGISTRY_AUDITS: usize = 64;
/// Absolute client budget for one complete witnessed-registry audit.
///
/// Outer integrations must leave additional completion headroom rather than truncating this
/// cryptographic audit while its final verified state is being returned.
pub const REGISTRY_AUDIT_TOTAL_TIMEOUT: Duration = Duration::from_secs(120);

#[derive(Clone)]
struct AuditCpuLane {
    permits: Arc<Semaphore>,
}

impl AuditCpuLane {
    fn global() -> Self {
        static PERMITS: OnceLock<Arc<Semaphore>> = OnceLock::new();
        Self {
            permits: Arc::clone(
                PERMITS.get_or_init(|| Arc::new(Semaphore::new(MAX_CONCURRENT_REGISTRY_AUDITS))),
            ),
        }
    }

    #[cfg(test)]
    fn with_limit(limit: usize) -> Self {
        Self {
            permits: Arc::new(Semaphore::new(limit)),
        }
    }

    async fn run<T, F>(&self, operation: F) -> Result<T>
    where
        T: Send + 'static,
        F: FnOnce() -> Result<T> + Send + 'static,
    {
        let permit = Arc::clone(&self.permits)
            .try_acquire_owned()
            .map_err(|_| RegistryError::Overloaded)?;
        tokio::task::spawn_blocking(move || {
            let _permit = permit;
            operation()
        })
        .await
        .map_err(|_| RegistryError::RegistryUnavailable)?
    }
}

fn global_audit_admission() -> Arc<Semaphore> {
    static ADMISSION: OnceLock<Arc<Semaphore>> = OnceLock::new();
    Arc::clone(ADMISSION.get_or_init(|| Arc::new(Semaphore::new(MAX_CONCURRENT_REGISTRY_AUDITS))))
}

#[derive(Clone)]
struct PinnedRegistryResolver {
    expected_host: Arc<str>,
    addresses: Arc<tokio::sync::OnceCell<Vec<SocketAddr>>>,
}

impl PinnedRegistryResolver {
    fn new(expected_host: &str) -> Self {
        Self {
            expected_host: Arc::from(expected_host),
            addresses: Arc::new(tokio::sync::OnceCell::new()),
        }
    }
}

impl Resolve for PinnedRegistryResolver {
    fn resolve(&self, name: Name) -> Resolving {
        let requested_host = name.as_str().to_owned();
        let expected_host = Arc::clone(&self.expected_host);
        let addresses = Arc::clone(&self.addresses);
        Box::pin(async move {
            if !requested_host.eq_ignore_ascii_case(&expected_host) {
                return Err(dns_error("registry resolver received an unexpected host"));
            }
            let pinned = addresses
                .get_or_try_init(|| async {
                    let resolved = tokio::time::timeout(
                        DNS_TIMEOUT,
                        tokio::net::lookup_host((expected_host.as_ref(), 0)),
                    )
                    .await
                    .map_err(|_| {
                        io::Error::new(io::ErrorKind::TimedOut, "registry DNS lookup timed out")
                    })?
                    .map_err(|_| io::Error::other("registry DNS lookup failed"))?
                    .take(MAX_RESOLVED_ADDRESSES + 1)
                    .collect();
                    validate_resolved_addresses(resolved)
                })
                .await
                .map_err(|error| dns_error(&error.to_string()))?;
            Ok(Box::new(pinned.clone().into_iter()) as Addrs)
        })
    }
}

fn dns_error(message: &str) -> Box<dyn std::error::Error + Send + Sync> {
    Box::new(io::Error::other(message.to_owned()))
}

fn validate_resolved_addresses(addresses: Vec<SocketAddr>) -> io::Result<Vec<SocketAddr>> {
    if addresses.is_empty() || addresses.len() > MAX_RESOLVED_ADDRESSES {
        return Err(io::Error::other(
            "registry DNS result is outside the allowed bounds",
        ));
    }
    if addresses
        .iter()
        .any(|address| !is_public_network_address(address.ip()))
    {
        return Err(io::Error::other(
            "registry DNS result contains a non-public address",
        ));
    }
    Ok(addresses)
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum AuditDumpOutcome {
    Complete,
    UseRangePages,
}

struct DumpCpuState<A: AuditAccumulator> {
    accumulator: A,
    pending: Vec<u8>,
    index: u64,
}

fn process_dump_chunk<A: AuditAccumulator>(
    mut state: DumpCpuState<A>,
    chunk: Vec<u8>,
    to: u64,
) -> Result<(DumpCpuState<A>, Option<AuditDumpOutcome>)> {
    let mut remaining = chunk.as_slice();
    while !remaining.is_empty() {
        let Some(newline) = remaining.iter().position(|byte| *byte == b'\n') else {
            if state.pending.len().saturating_add(remaining.len()) > MAX_AUDIT_DUMP_LINE_BYTES {
                return Err(RegistryError::MalformedEntry(
                    "registry dump entry exceeds the client limit".into(),
                ));
            }
            state.pending.extend_from_slice(remaining);
            break;
        };
        if state.pending.len().saturating_add(newline) > MAX_AUDIT_DUMP_LINE_BYTES {
            return Err(RegistryError::MalformedEntry(
                "registry dump entry exceeds the client limit".into(),
            ));
        }
        state.pending.extend_from_slice(&remaining[..newline]);
        if state.pending.is_empty() {
            return Err(RegistryError::MalformedEntry(
                "registry dump contains an empty entry".into(),
            ));
        }
        let entry: LogEntry = serde_json::from_slice(&state.pending).map_err(|_| {
            RegistryError::MalformedEntry("registry dump entry is malformed".into())
        })?;
        state.pending.clear();
        if state.index >= to {
            if state.index == to && entry.seq() == state.index && entry.leaf_bytes().is_ok() {
                return Ok((state, Some(AuditDumpOutcome::UseRangePages)));
            }
            return Err(RegistryError::MalformedEntry(
                "registry dump contains entries outside its exact range".into(),
            ));
        }
        state.accumulator.append(entry, state.index)?;
        state.index = state
            .index
            .checked_add(1)
            .ok_or_else(|| RegistryError::MalformedEntry("log index overflowed".into()))?;
        remaining = &remaining[newline + 1..];
    }
    Ok((state, None))
}

/// One durable, out-of-band checkpoint anchor.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct CheckpointPin {
    pub size: u64,
    pub root: Hash,
}

impl From<&Checkpoint> for CheckpointPin {
    fn from(value: &Checkpoint) -> Self {
        Self {
            size: value.size,
            root: value.root,
        }
    }
}

/// Operator-configured trust policy. None of these fields may come from a resolve response.
///
/// The witness threshold is a nonzero strict majority of one roster. That guarantees quorum-set
/// intersection; deployments still need a non-equivocating witness in every possible intersection.
#[derive(Debug, Clone)]
pub struct RegistryTrust {
    expected_origin: String,
    checkpoint_key: VerifyingKey,
    witnesses: Vec<WitnessKey>,
    witness_threshold: usize,
    minimum_checkpoint: CheckpointPin,
    max_cosignature_age_secs: u64,
    future_clock_skew_secs: u64,
}

impl RegistryTrust {
    #[allow(clippy::too_many_arguments)]
    pub fn new(
        expected_origin: impl Into<String>,
        checkpoint_key: [u8; PUBLIC_KEY_LENGTH],
        witnesses: Vec<WitnessKey>,
        witness_threshold: usize,
        minimum_checkpoint: CheckpointPin,
        max_cosignature_age_secs: u64,
        future_clock_skew_secs: u64,
    ) -> Result<Self> {
        let expected_origin = expected_origin.into();
        if expected_origin.is_empty()
            || expected_origin.len() > MAX_URL_BYTES
            || expected_origin
                .bytes()
                .any(|byte| byte.is_ascii_control() || byte.is_ascii_whitespace())
            || !witness_quorum_intersects(witness_threshold, witnesses.len())
            || max_cosignature_age_secs == 0
            || future_clock_skew_secs > max_cosignature_age_secs
            || (minimum_checkpoint.size == 0 && minimum_checkpoint.root != empty_root())
        {
            return Err(RegistryError::InvalidConfiguration(
                "invalid registry trust policy".into(),
            ));
        }
        let checkpoint_key = keys::verifying_key_from_bytes(&checkpoint_key).map_err(|_| {
            RegistryError::InvalidConfiguration("invalid registry checkpoint key".into())
        })?;
        // Checkpoint verification rejects duplicate witnesses too, but rejecting them at setup
        // prevents a configuration that can never satisfy its own threshold from being persisted.
        for (index, witness) in witnesses.iter().enumerate() {
            if witnesses[..index]
                .iter()
                .any(|prior| prior.name() == witness.name() || prior.key() == witness.key())
            {
                return Err(RegistryError::InvalidConfiguration(
                    "duplicate registry witness".into(),
                ));
            }
        }
        Ok(Self {
            expected_origin,
            checkpoint_key,
            witnesses,
            witness_threshold,
            minimum_checkpoint,
            max_cosignature_age_secs,
            future_clock_skew_secs,
        })
    }

    pub fn expected_origin(&self) -> &str {
        &self.expected_origin
    }

    pub fn checkpoint_key(&self) -> &VerifyingKey {
        &self.checkpoint_key
    }

    pub fn witnesses(&self) -> &[WitnessKey] {
        &self.witnesses
    }

    pub const fn witness_threshold(&self) -> usize {
        self.witness_threshold
    }

    pub const fn minimum_checkpoint(&self) -> CheckpointPin {
        self.minimum_checkpoint
    }

    pub const fn max_cosignature_age_secs(&self) -> u64 {
        self.max_cosignature_age_secs
    }

    pub const fn future_clock_skew_secs(&self) -> u64 {
        self.future_clock_skew_secs
    }
}

/// A handle binding authenticated against the caller's trust policy.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct VerifiedHandle {
    handle: Handle,
    pubkey: [u8; PUBLIC_KEY_LENGTH],
    subject: String,
    log_index: u64,
    entry_kind: EntryKind,
    checkpoint: Checkpoint,
    witnessed_at: Option<u64>,
}

/// Relationship between one inclusion-verified latest handle binding and an immutable append
/// receipt. Only a strictly older witnessed leaf may still be waiting for witness promotion;
/// same-index or newer mismatches are terminal conflicts.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum HandlePublication {
    Pending,
    Ready,
    Mismatch,
}

impl VerifiedHandle {
    pub fn handle(&self) -> &Handle {
        &self.handle
    }

    pub const fn pubkey(&self) -> &[u8; PUBLIC_KEY_LENGTH] {
        &self.pubkey
    }

    pub fn subject(&self) -> &str {
        &self.subject
    }

    pub const fn log_index(&self) -> u64 {
        self.log_index
    }

    /// Kind of the exact inclusion-verified leaf behind this latest binding.
    pub const fn entry_kind(&self) -> EntryKind {
        self.entry_kind
    }

    pub fn checkpoint(&self) -> &Checkpoint {
        &self.checkpoint
    }

    pub const fn witnessed_at(&self) -> Option<u64> {
        self.witnessed_at
    }

    /// Compare this exact inclusion-verified leaf with the append receipt a caller is waiting for.
    pub fn publication_against(
        &self,
        expected_index: u64,
        expected_pubkey: &[u8; PUBLIC_KEY_LENGTH],
        expected_entry_kind: &str,
    ) -> HandlePublication {
        if self.log_index < expected_index {
            return HandlePublication::Pending;
        }
        if self.log_index == expected_index
            && &self.pubkey == expected_pubkey
            && self.entry_kind.as_str() == expected_entry_kind
        {
            return HandlePublication::Ready;
        }
        HandlePublication::Mismatch
    }
}

/// Latest binding for one handle, derived from every registry leaf through a witnessed head.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct AuditedHandleBinding {
    pubkey: [u8; PUBLIC_KEY_LENGTH],
    subject: String,
    log_index: u64,
}

impl AuditedHandleBinding {
    pub fn new(
        handle: &Handle,
        pubkey: [u8; PUBLIC_KEY_LENGTH],
        subject: String,
        log_index: u64,
    ) -> Result<Self> {
        let binding = Self {
            pubkey,
            subject,
            log_index,
        };
        validate_audited_handle_binding(handle, &binding)?;
        Ok(binding)
    }

    pub const fn pubkey(&self) -> &[u8; PUBLIC_KEY_LENGTH] {
        &self.pubkey
    }

    pub fn subject(&self) -> &str {
        &self.subject
    }

    pub const fn log_index(&self) -> u64 {
        self.log_index
    }
}

/// One strictly decoded handle mutation encountered during continuous log replay.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AuditedHandleMutation {
    kind: crate::entry::EntryKind,
    handle: String,
    pubkey: [u8; PUBLIC_KEY_LENGTH],
    subject: String,
    log_index: u64,
}

impl AuditedHandleMutation {
    pub const fn kind(&self) -> crate::entry::EntryKind {
        self.kind
    }

    pub fn handle(&self) -> &str {
        &self.handle
    }

    pub const fn pubkey(&self) -> &[u8; PUBLIC_KEY_LENGTH] {
        &self.pubkey
    }

    pub fn subject(&self) -> &str {
        &self.subject
    }

    pub const fn log_index(&self) -> u64 {
        self.log_index
    }
}

/// Transactional projection sink used while a complete handle-log audit is in flight.
///
/// Implementations must keep mutations provisional until the caller receives a successful audit;
/// if network, codec, state-machine, or root verification fails, all mutations must be discarded.
pub trait HandleProjectionStore: Send + 'static {
    /// Remove every projected handle before a fresh replay from leaf zero.
    fn reset(&mut self) -> Result<()>;
    /// Start one provisional streamed segment.
    ///
    /// A delivery failure rolls back only this segment before the same range is replayed through
    /// the bounded JSON compatibility route. Implementations must not expose segment mutations as
    /// durable state before the complete audit succeeds.
    fn begin_segment(&mut self) -> Result<()>;
    /// Retain a completely delivered segment in the still-provisional projection.
    fn commit_segment(&mut self) -> Result<()>;
    /// Discard every mutation made since [`Self::begin_segment`].
    fn rollback_segment(&mut self) -> Result<()>;
    /// Apply one claim or rotation, rejecting invalid per-handle transition history.
    fn apply(&mut self, mutation: &AuditedHandleMutation) -> Result<()>;
    /// Return the latest projected binding for `handle` after all mutations have been applied.
    fn binding(&mut self, handle: &Handle) -> Result<Option<AuditedHandleBinding>>;
}

/// Durable compact proof that the normalized handle projection was derived from a continuous log.
///
/// One global frontier covers all handles: fresh resolution streams the exact witnessed prefix
/// once, while every later resolution inspects only leaves appended after this state. Individual
/// bindings live in the caller's normalized projection store rather than this serialized blob.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct HandleAuditState {
    origin: String,
    size: u64,
    root: Hash,
    checkpoint_note: String,
    witnessed_at: u64,
    frontier: MerkleFrontier,
}

impl HandleAuditState {
    pub fn checkpoint(&self) -> Checkpoint {
        Checkpoint {
            origin: self.origin.clone(),
            size: self.size,
            root: self.root,
        }
    }

    pub const fn size(&self) -> u64 {
        self.size
    }

    pub const fn root(&self) -> &Hash {
        &self.root
    }

    pub fn checkpoint_note(&self) -> &str {
        &self.checkpoint_note
    }

    pub const fn witnessed_at(&self) -> u64 {
        self.witnessed_at
    }

    /// Validate deserialized state before it is used as an incremental audit frontier.
    pub fn validate(&self, expected_origin: &str) -> Result<()> {
        validate_handle_audit_state(self, expected_origin)
    }
}

/// One compliance-key publication authenticated against an exact witnessed registry leaf.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct VerifiedComplianceKey {
    publication: ComplianceKeyPublish,
    public_key: [u8; 32],
    log_index: u64,
}

impl VerifiedComplianceKey {
    pub fn publication(&self) -> &ComplianceKeyPublish {
        &self.publication
    }

    pub const fn public_key(&self) -> &[u8; 32] {
        &self.public_key
    }

    pub const fn log_index(&self) -> u64 {
        self.log_index
    }
}

/// A complete, witnessed compliance-key projection at one append-only checkpoint.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct VerifiedComplianceKeys {
    keys: Vec<VerifiedComplianceKey>,
    checkpoint: Checkpoint,
    witnessed_at: Option<u64>,
}

/// Latest state of one compliance key derived by auditing every registry log leaf.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct AuditedComplianceKey {
    publication: ComplianceKeyPublish,
    public_key: [u8; 32],
    log_index: u64,
}

impl AuditedComplianceKey {
    pub fn publication(&self) -> &ComplianceKeyPublish {
        &self.publication
    }

    pub const fn public_key(&self) -> &[u8; 32] {
        &self.public_key
    }

    pub const fn log_index(&self) -> u64 {
        self.log_index
    }
}

/// Durable compact state proving that every leaf through `size` was audited.
///
/// Inclusion proofs authenticate rows a server chose to return; they do not prove that a later
/// revocation was not omitted. This state closes that gap by incrementally rebuilding the exact
/// RFC 6962 root while deriving compliance state from every log entry.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ComplianceAuditState {
    origin: String,
    size: u64,
    root: Hash,
    checkpoint_note: String,
    witnessed_at: u64,
    frontier: MerkleFrontier,
    keys: Vec<AuditedComplianceKey>,
}

impl ComplianceAuditState {
    pub fn checkpoint(&self) -> Checkpoint {
        Checkpoint {
            origin: self.origin.clone(),
            size: self.size,
            root: self.root,
        }
    }

    pub const fn size(&self) -> u64 {
        self.size
    }

    pub const fn root(&self) -> &Hash {
        &self.root
    }

    pub fn checkpoint_note(&self) -> &str {
        &self.checkpoint_note
    }

    pub const fn witnessed_at(&self) -> u64 {
        self.witnessed_at
    }

    pub fn keys(&self) -> &[AuditedComplianceKey] {
        &self.keys
    }

    /// Validate a deserialized audit state before using it as an offline trust anchor.
    pub fn validate(&self, expected_origin: &str) -> Result<()> {
        validate_audit_state(self, expected_origin)
    }

    /// Re-verify the persisted signed checkpoint and fresh witness quorum against configured
    /// trust before the audit authorizes offline key use.
    pub fn verify_witnesses(
        &self,
        trust: &RegistryTrust,
        now_secs: u64,
    ) -> Result<VerifiedCheckpoint> {
        self.validate(trust.expected_origin())?;
        if trust.witness_threshold == 0 {
            return Err(RegistryError::InvalidConfiguration(
                "compliance audit requires a nonzero witness quorum".into(),
            ));
        }
        if self.size < trust.minimum_checkpoint.size
            || (self.size == trust.minimum_checkpoint.size
                && self.root != trust.minimum_checkpoint.root)
        {
            return Err(RegistryError::MalformedCheckpoint(
                "persisted compliance audit is below the configured checkpoint pin".into(),
            ));
        }
        let verified = Checkpoint::verify_with_fresh_witnesses(
            &self.checkpoint_note,
            &trust.checkpoint_key,
            &trust.witnesses,
            trust.witness_threshold,
            now_secs,
            trust.max_cosignature_age_secs,
            trust.future_clock_skew_secs,
        )?;
        if verified.checkpoint != self.checkpoint()
            || verified.witnessed_at != Some(self.witnessed_at)
        {
            return Err(RegistryError::MalformedCheckpoint(
                "persisted compliance audit checkpoint proof does not match its state".into(),
            ));
        }
        Ok(verified)
    }
}

impl VerifiedComplianceKeys {
    pub fn keys(&self) -> &[VerifiedComplianceKey] {
        &self.keys
    }

    pub fn checkpoint(&self) -> &Checkpoint {
        &self.checkpoint
    }

    pub const fn witnessed_at(&self) -> Option<u64> {
        self.witnessed_at
    }
}

trait AuditAccumulator: Send + 'static {
    fn append(&mut self, entry: LogEntry, index: u64) -> Result<()>;
}

#[cfg(test)]
struct ClosureAuditAccumulator<F>(F);

#[cfg(test)]
impl<F> AuditAccumulator for ClosureAuditAccumulator<F>
where
    F: FnMut(LogEntry, u64) -> Result<()> + Send + 'static,
{
    fn append(&mut self, entry: LogEntry, index: u64) -> Result<()> {
        (self.0)(entry, index)
    }
}

struct ComplianceAuditAccumulator {
    frontier: MerkleFrontier,
    keys: HashMap<ComplianceKeyId, AuditedComplianceKey>,
}

impl AuditAccumulator for ComplianceAuditAccumulator {
    fn append(&mut self, entry: LogEntry, index: u64) -> Result<()> {
        append_audited_entry(&mut self.frontier, &mut self.keys, entry, index)
    }
}

struct HandleAuditAccumulator<P: HandleProjectionStore> {
    frontier: MerkleFrontier,
    projection: P,
    requested_path: String,
    target_binding: Option<AuditedHandleBinding>,
}

impl<P: HandleProjectionStore> HandleAuditAccumulator<P> {
    fn begin_segment(&mut self) -> Result<()> {
        self.projection.begin_segment()
    }

    fn commit_segment(&mut self) -> Result<()> {
        self.projection.commit_segment()
    }

    fn rollback_segment(
        &mut self,
        frontier: MerkleFrontier,
        target_binding: Option<AuditedHandleBinding>,
    ) -> Result<()> {
        self.projection.rollback_segment()?;
        self.frontier = frontier;
        self.target_binding = target_binding;
        Ok(())
    }
}

impl<P: HandleProjectionStore> AuditAccumulator for HandleAuditAccumulator<P> {
    fn append(&mut self, entry: LogEntry, index: u64) -> Result<()> {
        append_handle_audited_entry(
            &mut self.frontier,
            &mut self.projection,
            &self.requested_path,
            &mut self.target_binding,
            entry,
            index,
        )
    }
}

pub struct RegistryClient {
    base_url: Url,
    trust: RegistryTrust,
    http: Client,
    audit_admission: Arc<Semaphore>,
    audit_cpu: AuditCpuLane,
}

impl core::fmt::Debug for RegistryClient {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        f.debug_struct("RegistryClient")
            .field("base_url", &"<withheld>")
            .field("origin", &self.trust.expected_origin)
            .field("witness_threshold", &self.trust.witness_threshold)
            .finish()
    }
}

impl RegistryClient {
    /// Construct a client from an independently provisioned registry trust policy.
    ///
    /// The trust value is the explicit authorization boundary for exact numeric-loopback test
    /// origins. Public DNS names are resolved by this client, every returned address must be public,
    /// and the complete validated answer set is pinned for the lifetime of the client.
    pub fn new(base_url: &str, trust: RegistryTrust) -> Result<Self> {
        let base_url = validate_base_url(base_url)?;
        let host = base_url
            .host_str()
            .ok_or_else(|| RegistryError::InvalidConfiguration("invalid registry URL".into()))?;
        let resolution_host = host.trim_start_matches('[').trim_end_matches(']');
        let mut builder = Client::builder()
            .redirect(reqwest::redirect::Policy::none())
            .no_proxy()
            .connect_timeout(CONNECT_TIMEOUT)
            .timeout(REQUEST_TIMEOUT);
        if resolution_host.parse::<IpAddr>().is_err() {
            builder = builder.dns_resolver(Arc::new(PinnedRegistryResolver::new(resolution_host)));
        }
        let http = builder
            .build()
            .map_err(|_| RegistryError::RegistryUnavailable)?;
        Ok(Self {
            base_url,
            trust,
            http,
            audit_admission: global_audit_admission(),
            audit_cpu: AuditCpuLane::global(),
        })
    }

    fn try_begin_audit(&self) -> Result<OwnedSemaphorePermit> {
        Arc::clone(&self.audit_admission)
            .try_acquire_owned()
            .map_err(|_| RegistryError::Overloaded)
    }

    /// Resolve a handle by auditing the complete witnessed log from leaf zero.
    ///
    /// This compatibility entry point is safe but intentionally does not retain an audit frontier,
    /// so repeated calls cost O(total log). Product clients should use [`Self::resolve_audited`]
    /// and durably persist the returned [`HandleAuditState`].
    pub async fn resolve(
        &self,
        handle: &Handle,
        previous: Option<&Checkpoint>,
        now_secs: u64,
    ) -> Result<VerifiedHandle> {
        let projection = InMemoryHandleProjection::default();
        self.resolve_audited(handle, None, previous, now_secs, projection)
            .await
            .map(|(verified, _, _)| verified)
    }

    /// Resolve a handle only after deriving its latest binding from every registry leaf through
    /// the exact fresh witnessed head.
    ///
    /// A Merkle inclusion proof authenticates whichever historical row a server selected; it does
    /// not prove that row is current. This method independently rebuilds the witnessed root and a
    /// strict claim/rotation state machine, then requires the convenience projection and its exact
    /// included leaf to match the derived latest binding. Fresh calls walk bounded exact NDJSON
    /// ranges from zero; later calls walk the unseen suffix through the same engine. Only a failed
    /// segment is replayed in bounded JSON pages. The operation shares one 120-second deadline.
    pub async fn resolve_audited<P: HandleProjectionStore>(
        &self,
        handle: &Handle,
        previous: Option<&HandleAuditState>,
        accepted_checkpoint: Option<&Checkpoint>,
        now_secs: u64,
        projection: P,
    ) -> Result<(VerifiedHandle, HandleAuditState, P)> {
        self.resolve_audited_with_deadline(
            handle,
            previous,
            accepted_checkpoint,
            now_secs,
            projection,
            REGISTRY_AUDIT_TOTAL_TIMEOUT,
        )
        .await
    }

    async fn resolve_audited_with_deadline<P: HandleProjectionStore>(
        &self,
        handle: &Handle,
        previous: Option<&HandleAuditState>,
        accepted_checkpoint: Option<&Checkpoint>,
        now_secs: u64,
        projection: P,
        deadline: Duration,
    ) -> Result<(VerifiedHandle, HandleAuditState, P)> {
        self.resolve_audited_with_timeouts(
            handle,
            previous,
            accepted_checkpoint,
            now_secs,
            projection,
            deadline,
            AUDIT_SEGMENT_TIMEOUT,
        )
        .await
    }

    #[allow(clippy::too_many_arguments)]
    async fn resolve_audited_with_timeouts<P: HandleProjectionStore>(
        &self,
        handle: &Handle,
        previous: Option<&HandleAuditState>,
        accepted_checkpoint: Option<&Checkpoint>,
        now_secs: u64,
        projection: P,
        total_timeout: Duration,
        dump_timeout: Duration,
    ) -> Result<(VerifiedHandle, HandleAuditState, P)> {
        let _audit = self.try_begin_audit()?;
        tokio::time::timeout(
            total_timeout,
            self.resolve_audited_inner(
                handle,
                previous,
                accepted_checkpoint,
                now_secs,
                projection,
                dump_timeout,
            ),
        )
        .await
        .map_err(|_| RegistryError::RegistryUnavailable)?
    }

    async fn resolve_audited_inner<P: HandleProjectionStore>(
        &self,
        handle: &Handle,
        previous: Option<&HandleAuditState>,
        accepted_checkpoint: Option<&Checkpoint>,
        now_secs: u64,
        projection: P,
        dump_timeout: Duration,
    ) -> Result<(VerifiedHandle, HandleAuditState, P)> {
        let trust = self.trust.clone();
        let previous_for_base = previous.cloned();
        let accepted_for_base = accepted_checkpoint.cloned();
        let base = self
            .audit_cpu
            .run(move || {
                if trust.witness_threshold == 0 {
                    return Err(RegistryError::InvalidConfiguration(
                        "handle resolution requires a nonzero witness quorum".into(),
                    ));
                }
                if let Some(state) = &previous_for_base {
                    state.validate(trust.expected_origin())?;
                }
                select_audit_base(
                    &trust,
                    previous_for_base.as_ref().map(HandleAuditState::checkpoint),
                    accepted_for_base,
                )
            })
            .await?;

        let resolved: ResolveResponse = self
            .get_json(&format!(
                "v1/resolve/{}/{}",
                handle.namespace(),
                handle.name()
            ))
            .await?;
        let trust = self.trust.clone();
        let requested_path = handle.as_path();
        let (resolved, verified, inclusion_path) = self
            .audit_cpu
            .run(move || {
                if resolved.handle != requested_path
                    || resolved.log_index >= resolved.inclusion_proof.tree_size
                {
                    return Err(RegistryError::MalformedEntry(
                        "resolve response does not name the requested leaf".into(),
                    ));
                }
                let root = parse_hex32(&resolved.inclusion_proof.root).ok_or_else(|| {
                    RegistryError::MalformedCheckpoint(
                        "checkpoint root is not canonical hex".into(),
                    )
                })?;
                let inclusion_path = parse_hashes(&resolved.inclusion_proof.path)?;
                let verified = verify_checkpoint_with_trust(
                    &trust,
                    &resolved.inclusion_proof.checkpoint,
                    resolved.inclusion_proof.tree_size,
                    root,
                    now_secs,
                )?;
                Ok((resolved, verified, inclusion_path))
            })
            .await?;
        self.verify_growth(base, &verified.checkpoint).await?;

        let next = resolved
            .log_index
            .checked_add(1)
            .ok_or_else(|| RegistryError::MalformedEntry("log index overflowed".into()))?;
        let page: EntriesResponse = self
            .get_json(&format!(
                "v1/log/entries?from={}&to={next}",
                resolved.log_index
            ))
            .await?;
        let expected_handle = handle.clone();
        let verified_checkpoint = verified.checkpoint.clone();
        let (pubkey, entry_subject, entry_kind, resolved_log_index, checkpoint_note) = self
            .audit_cpu
            .run(move || {
                if page.from != resolved.log_index
                    || page.to != next
                    || page.tree_size != verified_checkpoint.size
                    || parse_hex32(&page.root) != Some(verified_checkpoint.root)
                    || page.checkpoint != resolved.inclusion_proof.checkpoint
                    || page.entries.len() != 1
                {
                    return Err(RegistryError::MalformedEntry(
                        "exact log range disagrees with the resolved checkpoint".into(),
                    ));
                }
                let entry = page.entries.into_iter().next().ok_or_else(|| {
                    RegistryError::MalformedEntry("missing exact log leaf".into())
                })?;
                let entry_kind = entry.kind();
                let (entry_handle, entry_pubkey, entry_subject) =
                    entry.handle_binding().ok_or_else(|| {
                        RegistryError::MalformedEntry(
                            "resolved leaf is not a handle binding".into(),
                        )
                    })?;
                if entry.seq() != resolved.log_index
                    || entry_handle != expected_handle.as_path()
                    || entry_pubkey != resolved.pubkey
                {
                    return Err(RegistryError::MalformedEntry(
                        "resolved projection disagrees with its exact log leaf".into(),
                    ));
                }
                let entry_subject = entry_subject.to_owned();
                let leaf = entry
                    .leaf_bytes()
                    .map_err(|error| RegistryError::MalformedEntry(error.to_string()))?;
                if !verify_inclusion(
                    &leaf_hash(&leaf),
                    resolved.log_index,
                    verified_checkpoint.size,
                    &inclusion_path,
                    &verified_checkpoint.root,
                ) {
                    return Err(RegistryError::MalformedEntry(
                        "handle inclusion proof failed".into(),
                    ));
                }
                let pubkey = parse_hex32(&resolved.pubkey).ok_or_else(|| {
                    RegistryError::MalformedEntry("handle key is not canonical hex".into())
                })?;
                keys::verifying_key_from_bytes(&pubkey)
                    .map_err(|_| RegistryError::MalformedEntry("handle key is invalid".into()))?;
                Ok((
                    pubkey,
                    entry_subject,
                    entry_kind,
                    resolved.log_index,
                    resolved.inclusion_proof.checkpoint,
                ))
            })
            .await?;

        let previous_state = previous.cloned();
        let requested_handle = handle.clone();
        let verified_checkpoint = verified.checkpoint.clone();
        let (mut accumulator, start) = self
            .audit_cpu
            .run(move || match previous_state {
                Some(state) => {
                    if state.size > verified_checkpoint.size
                        || (state.size == verified_checkpoint.size
                            && state.root != verified_checkpoint.root)
                    {
                        return Err(RegistryError::MalformedCheckpoint(
                            "audited handle state does not extend to this checkpoint".into(),
                        ));
                    }
                    let mut projection = projection;
                    let target_binding = projection.binding(&requested_handle)?;
                    Ok((
                        HandleAuditAccumulator {
                            frontier: state.frontier,
                            projection,
                            requested_path: requested_handle.as_path(),
                            target_binding,
                        },
                        state.size,
                    ))
                }
                None => {
                    let mut projection = projection;
                    projection.reset()?;
                    Ok((
                        HandleAuditAccumulator {
                            frontier: MerkleFrontier::new(),
                            projection,
                            requested_path: requested_handle.as_path(),
                            target_binding: None,
                        },
                        0,
                    ))
                }
            })
            .await?;

        let mut from = start;
        while from < verified.checkpoint.size {
            let to = from
                .saturating_add(AUDIT_DUMP_SEGMENT_ENTRIES)
                .min(verified.checkpoint.size);
            let (started, frontier_before, target_before) = self
                .audit_cpu
                .run(move || {
                    let frontier_before = accumulator.frontier.clone();
                    let target_before = accumulator.target_binding.clone();
                    accumulator.begin_segment()?;
                    Ok((accumulator, frontier_before, target_before))
                })
                .await?;
            let (delivered_accumulator, delivered) = self
                .audit_dump_range_accumulator(from, to, dump_timeout, started)
                .await?;
            accumulator = delivered_accumulator;
            match delivered {
                AuditDumpOutcome::Complete => {
                    accumulator = self
                        .audit_cpu
                        .run(move || {
                            accumulator.commit_segment()?;
                            Ok(accumulator)
                        })
                        .await?;
                }
                AuditDumpOutcome::UseRangePages => {
                    accumulator = self
                        .audit_cpu
                        .run(move || {
                            accumulator.rollback_segment(frontier_before, target_before)?;
                            accumulator.begin_segment()?;
                            Ok(accumulator)
                        })
                        .await?;
                    accumulator = self
                        .audit_json_pages_accumulator(from, to, accumulator)
                        .await?;
                    accumulator = self
                        .audit_cpu
                        .run(move || {
                            accumulator.commit_segment()?;
                            Ok(accumulator)
                        })
                        .await?;
                }
            }
            from = to;
        }
        debug_assert_eq!(from, verified.checkpoint.size);

        let requested_handle = handle.clone();
        self.audit_cpu
            .run(move || {
                if accumulator.frontier.size() != verified.checkpoint.size
                    || accumulator.frontier.root() != Some(verified.checkpoint.root)
                {
                    return Err(RegistryError::MalformedCheckpoint(
                        "complete registry audit does not reconstruct the witnessed root".into(),
                    ));
                }
                let binding = accumulator
                    .target_binding
                    .clone()
                    .ok_or(RegistryError::NotFound)?;
                if accumulator.projection.binding(&requested_handle)?.as_ref() != Some(&binding) {
                    return Err(RegistryError::MalformedEntry(
                        "normalized handle projection disagrees with audited target state".into(),
                    ));
                }
                if binding.log_index != resolved_log_index
                    || binding.pubkey != pubkey
                    || binding.subject != entry_subject
                {
                    return Err(RegistryError::MalformedEntry(
                        "resolved projection is not the latest audited handle binding".into(),
                    ));
                }
                let witnessed_at = verified.witnessed_at.ok_or_else(|| {
                    RegistryError::MalformedCheckpoint(
                        "handle audit is missing its witnessed timestamp".into(),
                    )
                })?;
                let audit = HandleAuditState {
                    origin: verified.checkpoint.origin.clone(),
                    size: verified.checkpoint.size,
                    root: verified.checkpoint.root,
                    checkpoint_note,
                    witnessed_at,
                    frontier: accumulator.frontier,
                };
                let verified_handle = VerifiedHandle {
                    handle: requested_handle,
                    pubkey,
                    subject: entry_subject,
                    log_index: resolved_log_index,
                    entry_kind,
                    checkpoint: verified.checkpoint,
                    witnessed_at: Some(witnessed_at),
                };
                Ok((verified_handle, audit, accumulator.projection))
            })
            .await
    }

    /// Fetch a fresh witnessed head and audit every previously unseen registry leaf before
    /// returning compliance keys.
    ///
    /// The first call walks exact streamed segments from leaf zero and safely replays only a failed
    /// segment with bounded pages. Later calls continue from the compact frontier in `previous`
    /// through the same engine, so refresh cost is proportional only to newly appended entries. A server cannot hide a
    /// revocation or substitute an alternate live key: omission changes the recomputed root.
    /// Metadata, consistency, transfer, and verification share one fixed total deadline.
    pub async fn compliance_keys_audited(
        &self,
        previous: Option<Arc<ComplianceAuditState>>,
        accepted_checkpoint: Option<&Checkpoint>,
        now_secs: u64,
    ) -> Result<(VerifiedComplianceKeys, ComplianceAuditState)> {
        self.compliance_keys_audited_with_deadline(
            previous,
            accepted_checkpoint,
            now_secs,
            REGISTRY_AUDIT_TOTAL_TIMEOUT,
        )
        .await
    }

    async fn compliance_keys_audited_with_deadline(
        &self,
        previous: Option<Arc<ComplianceAuditState>>,
        accepted_checkpoint: Option<&Checkpoint>,
        now_secs: u64,
        deadline: Duration,
    ) -> Result<(VerifiedComplianceKeys, ComplianceAuditState)> {
        self.compliance_keys_audited_with_timeouts(
            previous,
            accepted_checkpoint,
            now_secs,
            deadline,
            AUDIT_SEGMENT_TIMEOUT,
        )
        .await
    }

    async fn compliance_keys_audited_with_timeouts(
        &self,
        previous: Option<Arc<ComplianceAuditState>>,
        accepted_checkpoint: Option<&Checkpoint>,
        now_secs: u64,
        total_timeout: Duration,
        dump_timeout: Duration,
    ) -> Result<(VerifiedComplianceKeys, ComplianceAuditState)> {
        let _audit = self.try_begin_audit()?;
        tokio::time::timeout(
            total_timeout,
            self.compliance_keys_audited_inner(
                previous,
                accepted_checkpoint,
                now_secs,
                dump_timeout,
            ),
        )
        .await
        .map_err(|_| RegistryError::RegistryUnavailable)?
    }

    async fn compliance_keys_audited_inner(
        &self,
        previous: Option<Arc<ComplianceAuditState>>,
        accepted_checkpoint: Option<&Checkpoint>,
        now_secs: u64,
        dump_timeout: Duration,
    ) -> Result<(VerifiedComplianceKeys, ComplianceAuditState)> {
        let trust = self.trust.clone();
        let previous_for_base = previous;
        let accepted_for_base = accepted_checkpoint.cloned();
        let (base, previous_state) = self
            .audit_cpu
            .run(move || {
                if trust.witness_threshold == 0 {
                    return Err(RegistryError::InvalidConfiguration(
                        "compliance keys require a nonzero witness quorum".into(),
                    ));
                }
                if let Some(state) = &previous_for_base {
                    state.validate(trust.expected_origin())?;
                }
                let base = select_audit_base(
                    &trust,
                    previous_for_base.as_ref().map(|state| state.checkpoint()),
                    accepted_for_base,
                )?;
                Ok((base, previous_for_base))
            })
            .await?;

        // New servers honor metadata_only and avoid materializing thousands of redundant
        // inclusion proofs. Older servers ignore it; the bounded decoder safely accepts either.
        let metadata: ComplianceKeysResponse = self
            .get_json("v1/compliance-keys?include_inactive=true&metadata_only=true")
            .await?;
        let trust = self.trust.clone();
        let (metadata, verified) = self
            .audit_cpu
            .run(move || {
                if metadata.keys.len() > MAX_COMPLIANCE_KEYS {
                    return Err(RegistryError::MalformedEntry(
                        "compliance-key metadata response exceeds its bound".into(),
                    ));
                }
                let root = parse_hex32(&metadata.root).ok_or_else(|| {
                    RegistryError::MalformedCheckpoint(
                        "checkpoint root is not canonical hex".into(),
                    )
                })?;
                let verified = verify_checkpoint_with_trust(
                    &trust,
                    &metadata.checkpoint,
                    metadata.tree_size,
                    root,
                    now_secs,
                )?;
                Ok((metadata, verified))
            })
            .await?;

        self.verify_growth(base, &verified.checkpoint).await?;

        let verified_checkpoint = verified.checkpoint.clone();
        let (mut accumulator, start) = self
            .audit_cpu
            .run(move || match previous_state {
                Some(state) => {
                    if state.size > verified_checkpoint.size
                        || (state.size == verified_checkpoint.size
                            && state.root != verified_checkpoint.root)
                    {
                        return Err(RegistryError::MalformedCheckpoint(
                            "audited registry state does not extend to this checkpoint".into(),
                        ));
                    }
                    let mut keys = HashMap::with_capacity(state.keys.len());
                    for key in &state.keys {
                        if keys.insert(key.publication.key_id, key.clone()).is_some() {
                            return Err(RegistryError::MalformedEntry(
                                "audited compliance state contains duplicate key ids".into(),
                            ));
                        }
                    }
                    Ok((
                        ComplianceAuditAccumulator {
                            frontier: state.frontier.clone(),
                            keys,
                        },
                        state.size,
                    ))
                }
                None => Ok((
                    ComplianceAuditAccumulator {
                        frontier: MerkleFrontier::new(),
                        keys: HashMap::new(),
                    },
                    0,
                )),
            })
            .await?;

        let mut from = start;
        // Fresh and incremental audits use immutable, CDN-cacheable exact ranges. Each segment
        // either applies in full or restores its compact frontier/derived state before the same
        // range is fetched from the JSON compatibility route. Authenticated-data failures never
        // become delivery fallback.
        while from < verified.checkpoint.size {
            let to = from
                .saturating_add(AUDIT_DUMP_SEGMENT_ENTRIES)
                .min(verified.checkpoint.size);
            let (next_accumulator, frontier_before, keys_before) = self
                .audit_cpu
                .run(move || {
                    let frontier_before = accumulator.frontier.clone();
                    let keys_before = accumulator.keys.clone();
                    Ok((accumulator, frontier_before, keys_before))
                })
                .await?;
            accumulator = next_accumulator;
            let (next_accumulator, outcome) = self
                .audit_dump_range_accumulator(from, to, dump_timeout, accumulator)
                .await?;
            accumulator = next_accumulator;
            match outcome {
                AuditDumpOutcome::Complete => {}
                AuditDumpOutcome::UseRangePages => {
                    accumulator = ComplianceAuditAccumulator {
                        frontier: frontier_before,
                        keys: keys_before,
                    };
                    accumulator = self
                        .audit_json_pages_accumulator(from, to, accumulator)
                        .await?;
                }
            }
            from = to;
        }
        debug_assert_eq!(from, verified.checkpoint.size);

        self.audit_cpu
            .run(move || {
                if accumulator.frontier.size() != verified.checkpoint.size
                    || accumulator.frontier.root() != Some(verified.checkpoint.root)
                {
                    return Err(RegistryError::MalformedCheckpoint(
                        "complete registry audit does not reconstruct the witnessed root".into(),
                    ));
                }

                let mut audited_keys: Vec<_> = accumulator.keys.into_values().collect();
                audited_keys.sort_by_key(|key| key.log_index);
                if audited_keys.len() > MAX_COMPLIANCE_KEYS {
                    return Err(RegistryError::MalformedEntry(
                        "audited compliance-key state exceeds the configured bound".into(),
                    ));
                }
                let returned = audited_keys
                    .iter()
                    .map(|key| VerifiedComplianceKey {
                        publication: key.publication.clone(),
                        public_key: key.public_key,
                        log_index: key.log_index,
                    })
                    .collect();
                let audit = ComplianceAuditState {
                    origin: verified.checkpoint.origin.clone(),
                    size: verified.checkpoint.size,
                    root: verified.checkpoint.root,
                    checkpoint_note: metadata.checkpoint,
                    witnessed_at: verified.witnessed_at.ok_or_else(|| {
                        RegistryError::MalformedCheckpoint(
                            "compliance audit is missing its witnessed timestamp".into(),
                        )
                    })?,
                    frontier: accumulator.frontier,
                    keys: audited_keys,
                };
                Ok((
                    VerifiedComplianceKeys {
                        keys: returned,
                        checkpoint: verified.checkpoint,
                        witnessed_at: verified.witnessed_at,
                    },
                    audit,
                ))
            })
            .await
    }

    /// Stream one immutable exact range while retaining only a bounded transport chunk and entry
    /// line. Delivery failures return `UseRangePages`; authenticated-data and replay failures remain
    /// errors.
    async fn audit_dump_range_accumulator<A>(
        &self,
        from: u64,
        to: u64,
        dump_timeout: Duration,
        accumulator: A,
    ) -> Result<(A, AuditDumpOutcome)>
    where
        A: AuditAccumulator,
    {
        self.audit_dump_range_with_limit_accumulator(
            from,
            to,
            dump_timeout,
            MAX_AUDIT_SEGMENT_BYTES,
            accumulator,
        )
        .await
    }

    async fn audit_dump_range_with_limit_accumulator<A>(
        &self,
        from: u64,
        to: u64,
        dump_timeout: Duration,
        max_transfer_bytes: u64,
        accumulator: A,
    ) -> Result<(A, AuditDumpOutcome)>
    where
        A: AuditAccumulator,
    {
        if from == to {
            return Ok((accumulator, AuditDumpOutcome::Complete));
        }
        if from > to || to - from > AUDIT_DUMP_SEGMENT_ENTRIES {
            return Err(RegistryError::InvalidConfiguration(
                "registry dump segment is outside the client bound".into(),
            ));
        }
        let url = self
            .base_url
            .join(&format!("v1/log/dump?from={from}&to={to}"))
            .map_err(|_| RegistryError::InvalidConfiguration("invalid registry route".into()))?;
        let response = self
            .http
            .get(url)
            .header(reqwest::header::ACCEPT, "application/x-ndjson")
            // The complete audit retains one outer deadline. This smaller attempt budget leaves
            // time inside it for bounded authenticated range replay when streaming is unavailable.
            .timeout(dump_timeout)
            .send()
            .await;
        let Ok(mut response) = response else {
            return Ok((accumulator, AuditDumpOutcome::UseRangePages));
        };
        if matches!(
            response.status(),
            StatusCode::NOT_FOUND
                | StatusCode::METHOD_NOT_ALLOWED
                | StatusCode::NOT_IMPLEMENTED
                | StatusCode::REQUEST_TIMEOUT
                | StatusCode::PAYLOAD_TOO_LARGE
                | StatusCode::TOO_MANY_REQUESTS
                | StatusCode::SERVICE_UNAVAILABLE
                | StatusCode::GATEWAY_TIMEOUT
        ) {
            return Ok((accumulator, AuditDumpOutcome::UseRangePages));
        }
        if !response.status().is_success() {
            return Err(RegistryError::RegistryUnavailable);
        }
        if response
            .content_length()
            .is_some_and(|length| length > max_transfer_bytes)
        {
            return Ok((accumulator, AuditDumpOutcome::UseRangePages));
        }

        let mut state = DumpCpuState {
            accumulator,
            pending: Vec::with_capacity(4 * 1024),
            index: from,
        };
        let mut transferred = 0u64;
        loop {
            let chunk = match response.chunk().await {
                Ok(Some(chunk)) => chunk,
                Ok(None) => {
                    return if state.index == to && state.pending.is_empty() {
                        Ok((state.accumulator, AuditDumpOutcome::Complete))
                    } else if state.index < to {
                        // A cleanly delivered immutable prefix may be older than the checkpoint
                        // metadata at a CDN edge. Re-fetch this exact segment through authenticated
                        // pages rather than converting ordinary cache skew into an outage.
                        Ok((state.accumulator, AuditDumpOutcome::UseRangePages))
                    } else {
                        Err(RegistryError::MalformedEntry(
                            "registry dump contains trailing partial data".into(),
                        ))
                    };
                }
                Err(_) => return Ok((state.accumulator, AuditDumpOutcome::UseRangePages)),
            };
            transferred = match transferred.checked_add(chunk.len() as u64) {
                Some(transferred) if transferred <= max_transfer_bytes => transferred,
                _ => return Ok((state.accumulator, AuditDumpOutcome::UseRangePages)),
            };
            let chunk = chunk.to_vec();
            let (next, outcome) = self
                .audit_cpu
                .run(move || process_dump_chunk(state, chunk, to))
                .await?;
            state = next;
            if let Some(outcome) = outcome {
                return Ok((state.accumulator, outcome));
            }
        }
    }

    #[cfg(test)]
    async fn audit_dump_range<F>(
        &self,
        from: u64,
        to: u64,
        dump_timeout: Duration,
        append: F,
    ) -> Result<AuditDumpOutcome>
    where
        F: FnMut(LogEntry, u64) -> Result<()> + Send + 'static,
    {
        let (_, outcome) = self
            .audit_dump_range_accumulator(from, to, dump_timeout, ClosureAuditAccumulator(append))
            .await?;
        Ok(outcome)
    }

    #[cfg(test)]
    async fn audit_dump_range_with_limit<F>(
        &self,
        from: u64,
        to: u64,
        dump_timeout: Duration,
        max_transfer_bytes: u64,
        append: F,
    ) -> Result<AuditDumpOutcome>
    where
        F: FnMut(LogEntry, u64) -> Result<()> + Send + 'static,
    {
        let (_, outcome) = self
            .audit_dump_range_with_limit_accumulator(
                from,
                to,
                dump_timeout,
                max_transfer_bytes,
                ClosureAuditAccumulator(append),
            )
            .await?;
        Ok(outcome)
    }

    async fn audit_json_pages_accumulator<A>(
        &self,
        mut from: u64,
        to: u64,
        mut accumulator: A,
    ) -> Result<A>
    where
        A: AuditAccumulator,
    {
        while from < to {
            let page_to = from.saturating_add(AUDIT_PAGE_ENTRIES).min(to);
            let page: EntriesResponse = self
                .get_json(&format!("v1/log/entries?from={from}&to={page_to}"))
                .await?;
            accumulator = self
                .audit_cpu
                .run(move || {
                    let expected_len = usize::try_from(page_to - from).map_err(|_| {
                        RegistryError::MalformedEntry(
                            "registry audit page length overflowed".into(),
                        )
                    })?;
                    if page.from != from
                        || page.to != page_to
                        || page.entries.len() != expected_len
                        || page.tree_size < page_to
                    {
                        return Err(RegistryError::MalformedEntry(
                            "registry audit page is not the requested continuous range".into(),
                        ));
                    }
                    for (offset, entry) in page.entries.into_iter().enumerate() {
                        let index = from.checked_add(offset as u64).ok_or_else(|| {
                            RegistryError::MalformedEntry("log index overflowed".into())
                        })?;
                        accumulator.append(entry, index)?;
                    }
                    Ok(accumulator)
                })
                .await?;
            from = page_to;
        }
        Ok(accumulator)
    }

    async fn verify_growth(&self, base: CheckpointPin, next: &Checkpoint) -> Result<()> {
        if next.size < base.size || (next.size == base.size && next.root != base.root) {
            return Err(RegistryError::MalformedCheckpoint(
                "registry checkpoint rolled back or equivocated".into(),
            ));
        }
        if next.size == base.size {
            return Ok(());
        }
        if base.size == 0 {
            if base.root != empty_root() {
                return Err(RegistryError::MalformedCheckpoint(
                    "invalid empty-tree trust anchor".into(),
                ));
            }
            return Ok(());
        }
        let proof: ConsistencyResponse = self
            .get_json(&format!(
                "v1/log/consistency?from={}&to={}",
                base.size, next.size
            ))
            .await?;
        let next = next.clone();
        self.audit_cpu
            .run(move || {
                let proof_root = parse_hex32(&proof.root).ok_or_else(|| {
                    RegistryError::MalformedCheckpoint(
                        "consistency root is not canonical hex".into(),
                    )
                })?;
                let path = parse_hashes(&proof.path)?;
                if proof.from != base.size
                    || proof.to != next.size
                    || proof_root != next.root
                    || !verify_consistency(base.size, &base.root, next.size, &next.root, &path)
                {
                    return Err(RegistryError::MalformedCheckpoint(
                        "registry consistency proof failed".into(),
                    ));
                }
                Ok(())
            })
            .await
    }

    async fn get_json<T: DeserializeOwned + Send + 'static>(&self, relative: &str) -> Result<T> {
        let url = self
            .base_url
            .join(relative)
            .map_err(|_| RegistryError::InvalidConfiguration("invalid registry route".into()))?;
        let mut response = self
            .http
            .get(url)
            .header(reqwest::header::ACCEPT, "application/json")
            .send()
            .await
            .map_err(|_| RegistryError::RegistryUnavailable)?;
        if response.status() == StatusCode::NOT_FOUND {
            return Err(RegistryError::NotFound);
        }
        if !response.status().is_success() {
            return Err(RegistryError::RegistryUnavailable);
        }
        let mut body = Vec::new();
        while let Some(chunk) = response
            .chunk()
            .await
            .map_err(|_| RegistryError::RegistryUnavailable)?
        {
            if body.len().saturating_add(chunk.len()) > MAX_RESPONSE_BYTES {
                return Err(RegistryError::MalformedEntry(
                    "registry response exceeds the client limit".into(),
                ));
            }
            body.extend_from_slice(&chunk);
        }
        self.audit_cpu
            .run(move || serde_json::from_slice(&body).map_err(RegistryError::from))
            .await
    }
}

fn verify_checkpoint_with_trust(
    trust: &RegistryTrust,
    text: &str,
    size: u64,
    root: Hash,
    now_secs: u64,
) -> Result<VerifiedCheckpoint> {
    let verified = if trust.witness_threshold == 0 {
        VerifiedCheckpoint {
            checkpoint: Checkpoint::verify_with_witnesses(
                text,
                &trust.checkpoint_key,
                &trust.witnesses,
                0,
            )?,
            witnessed_at: None,
        }
    } else {
        Checkpoint::verify_with_fresh_witnesses(
            text,
            &trust.checkpoint_key,
            &trust.witnesses,
            trust.witness_threshold,
            now_secs,
            trust.max_cosignature_age_secs,
            trust.future_clock_skew_secs,
        )?
    };
    if verified.checkpoint.origin != trust.expected_origin
        || verified.checkpoint.size != size
        || verified.checkpoint.root != root
    {
        return Err(RegistryError::MalformedCheckpoint(
            "checkpoint does not match the trusted origin or proof root".into(),
        ));
    }
    Ok(verified)
}

fn select_audit_base(
    trust: &RegistryTrust,
    previous: Option<Checkpoint>,
    accepted: Option<Checkpoint>,
) -> Result<CheckpointPin> {
    let mut base = trust.minimum_checkpoint;
    for checkpoint in previous.iter().chain(accepted.iter()) {
        if checkpoint.origin != trust.expected_origin
            || checkpoint.size < trust.minimum_checkpoint.size
            || (checkpoint.size == trust.minimum_checkpoint.size
                && checkpoint.root != trust.minimum_checkpoint.root)
        {
            return Err(RegistryError::MalformedCheckpoint(
                "accepted registry checkpoint conflicts with configured trust".into(),
            ));
        }
        if checkpoint.size > base.size {
            base = CheckpointPin::from(checkpoint);
        } else if checkpoint.size == base.size && checkpoint.root != base.root {
            return Err(RegistryError::MalformedCheckpoint(
                "accepted registry checkpoints equivocate".into(),
            ));
        }
    }
    Ok(base)
}

fn append_audited_entry(
    frontier: &mut MerkleFrontier,
    keys: &mut HashMap<ComplianceKeyId, AuditedComplianceKey>,
    entry: LogEntry,
    index: u64,
) -> Result<()> {
    if entry.seq() != index {
        return Err(RegistryError::MalformedEntry(
            "registry audit changed a log sequence".into(),
        ));
    }
    let leaf = entry
        .leaf_bytes()
        .map_err(|error| RegistryError::MalformedEntry(error.to_string()))?;
    if frontier.append(&leaf) != Some(index) {
        return Err(RegistryError::MalformedCheckpoint(
            "registry audit frontier is malformed".into(),
        ));
    }
    if let Some(publication) = entry.compliance_publication() {
        let audited = audited_compliance_key(publication, index)?;
        validate_compliance_transition(keys.get(&publication.key_id), &audited)?;
        if !keys.contains_key(&publication.key_id) && keys.len() >= MAX_COMPLIANCE_KEYS {
            return Err(RegistryError::MalformedEntry(
                "audited compliance-key state exceeds the configured bound".into(),
            ));
        }
        keys.insert(publication.key_id, audited);
    }
    Ok(())
}

fn append_handle_audited_entry<P: HandleProjectionStore>(
    frontier: &mut MerkleFrontier,
    projection: &mut P,
    requested: &str,
    target_binding: &mut Option<AuditedHandleBinding>,
    entry: LogEntry,
    index: u64,
) -> Result<()> {
    if entry.seq() != index {
        return Err(RegistryError::MalformedEntry(
            "registry audit changed a log sequence".into(),
        ));
    }
    let leaf = entry
        .leaf_bytes()
        .map_err(|error| RegistryError::MalformedEntry(error.to_string()))?;
    if frontier.append(&leaf) != Some(index) {
        return Err(RegistryError::MalformedCheckpoint(
            "registry audit frontier is malformed".into(),
        ));
    }

    let kind = entry.kind();
    let Some((handle, encoded_key, subject)) = entry.handle_binding() else {
        return Ok(());
    };
    // Authenticated v0 migration leaves remain part of the reconstructed root, but their retired
    // `/gh/...` spelling is intentionally not resolvable and therefore is not projected.
    if Handle::parse(handle).is_err() {
        return Ok(());
    }
    let pubkey = parse_hex32(encoded_key)
        .ok_or_else(|| RegistryError::MalformedEntry("handle key is not canonical hex".into()))?;
    keys::verifying_key_from_bytes(&pubkey)
        .map_err(|_| RegistryError::MalformedEntry("handle key is invalid".into()))?;
    let mutation = AuditedHandleMutation {
        kind,
        handle: handle.to_owned(),
        pubkey,
        subject: subject.to_owned(),
        log_index: index,
    };
    if handle == requested {
        validate_handle_transition(target_binding.as_ref(), &mutation)?;
        *target_binding = Some(AuditedHandleBinding {
            pubkey,
            subject: subject.to_owned(),
            log_index: index,
        });
    }
    projection.apply(&mutation)
}

#[derive(Default)]
struct InMemoryHandleProjection {
    bindings: HashMap<String, AuditedHandleBinding>,
    /// The value each handle had before its first mutation in the active segment.
    ///
    /// This is bounded by the distinct handles touched by one segment. Cloning the complete
    /// projection here would make a million-leaf fresh audit quadratic as the projection grows.
    segment_undo: Option<HashMap<String, Option<AuditedHandleBinding>>>,
}

impl HandleProjectionStore for InMemoryHandleProjection {
    fn reset(&mut self) -> Result<()> {
        self.bindings.clear();
        self.segment_undo = None;
        Ok(())
    }

    fn begin_segment(&mut self) -> Result<()> {
        if self.segment_undo.is_some() {
            return Err(RegistryError::InvalidConfiguration(
                "handle projection segment is already active".into(),
            ));
        }
        self.segment_undo = Some(HashMap::new());
        Ok(())
    }

    fn commit_segment(&mut self) -> Result<()> {
        self.segment_undo.take().ok_or_else(|| {
            RegistryError::InvalidConfiguration("handle projection segment is not active".into())
        })?;
        Ok(())
    }

    fn rollback_segment(&mut self) -> Result<()> {
        let undo = self.segment_undo.take().ok_or_else(|| {
            RegistryError::InvalidConfiguration("handle projection segment is not active".into())
        })?;
        for (handle, previous) in undo {
            match previous {
                Some(binding) => {
                    self.bindings.insert(handle, binding);
                }
                None => {
                    self.bindings.remove(&handle);
                }
            }
        }
        Ok(())
    }

    fn apply(&mut self, mutation: &AuditedHandleMutation) -> Result<()> {
        let previous = self.bindings.get(mutation.handle());
        validate_handle_transition(previous, mutation)?;
        if let Some(undo) = &mut self.segment_undo {
            undo.entry(mutation.handle.clone())
                .or_insert_with(|| previous.cloned());
        }
        self.bindings.insert(
            mutation.handle.clone(),
            AuditedHandleBinding {
                pubkey: mutation.pubkey,
                subject: mutation.subject.clone(),
                log_index: mutation.log_index,
            },
        );
        Ok(())
    }

    fn binding(&mut self, handle: &Handle) -> Result<Option<AuditedHandleBinding>> {
        Ok(self.bindings.get(&handle.as_path()).cloned())
    }
}

/// Validate the per-handle state machine used by both in-memory compatibility audits and durable
/// normalized projection stores.
pub fn validate_handle_transition(
    previous: Option<&AuditedHandleBinding>,
    mutation: &AuditedHandleMutation,
) -> Result<()> {
    match mutation.kind {
        crate::entry::EntryKind::HandleClaim => {
            if previous.is_some() {
                return Err(RegistryError::MalformedEntry(
                    "audited handle was claimed more than once".into(),
                ));
            }
        }
        crate::entry::EntryKind::HandleRotation => {
            let previous = previous.ok_or_else(|| {
                RegistryError::MalformedEntry(
                    "audited handle rotation appears before its claim".into(),
                )
            })?;
            if previous.subject != mutation.subject {
                return Err(RegistryError::MalformedEntry(
                    "audited handle rotation changed its provider subject".into(),
                ));
            }
            if previous.pubkey == mutation.pubkey {
                return Err(RegistryError::MalformedEntry(
                    "audited handle rotation did not change its key".into(),
                ));
            }
            if previous.log_index >= mutation.log_index {
                return Err(RegistryError::MalformedEntry(
                    "audited handle rotation did not advance its log index".into(),
                ));
            }
        }
        _ => {
            return Err(RegistryError::MalformedEntry(
                "handle projection received a non-handle mutation".into(),
            ));
        }
    }
    Ok(())
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct ResolveResponse {
    handle: String,
    pubkey: String,
    log_index: u64,
    inclusion_proof: InclusionProof,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct InclusionProof {
    tree_size: u64,
    root: String,
    path: Vec<String>,
    checkpoint: String,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct EntriesResponse {
    from: u64,
    to: u64,
    tree_size: u64,
    root: String,
    checkpoint: String,
    entries: Vec<LogEntry>,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct ConsistencyResponse {
    from: u64,
    to: u64,
    root: String,
    path: Vec<String>,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct ComplianceKeysResponse {
    tree_size: u64,
    root: String,
    checkpoint: String,
    keys: Vec<serde_json::Value>,
}

fn validate_base_url(input: &str) -> Result<Url> {
    if input.is_empty() || input.len() > MAX_URL_BYTES {
        return Err(RegistryError::InvalidConfiguration(
            "invalid registry URL".into(),
        ));
    }
    let mut url = Url::parse(input)
        .map_err(|_| RegistryError::InvalidConfiguration("invalid registry URL".into()))?;
    if url.cannot_be_a_base()
        || !url.username().is_empty()
        || url.password().is_some()
        || url.query().is_some()
        || url.fragment().is_some()
        || url.host_str().is_none()
        || url.port() == Some(0)
        || (url.path() != "/" && !url.path().is_empty())
    {
        return Err(RegistryError::InvalidConfiguration(
            "invalid registry URL".into(),
        ));
    }
    let host = url
        .host_str()
        .ok_or_else(|| RegistryError::InvalidConfiguration("invalid registry URL".into()))?;
    if is_localhost_name(host) {
        return Err(RegistryError::InvalidConfiguration(
            "invalid registry URL".into(),
        ));
    }
    let host = host.trim_start_matches('[').trim_end_matches(']');
    let literal_ip = host.parse::<IpAddr>().ok();
    let loopback = literal_ip.is_some_and(|address| address.is_loopback());
    if literal_ip
        .is_some_and(|address| !address.is_loopback() && !is_public_network_address(address))
    {
        return Err(RegistryError::InvalidConfiguration(
            "registry address is not public".into(),
        ));
    }
    if url.scheme() != "https" && !(url.scheme() == "http" && loopback) {
        return Err(RegistryError::InvalidConfiguration(
            "registry must use HTTPS".into(),
        ));
    }
    let path = format!("{}/", url.path().trim_end_matches('/'));
    url.set_path(&path);
    Ok(url)
}

fn parse_hashes(values: &[String]) -> Result<Vec<Hash>> {
    values
        .iter()
        .map(|value| {
            parse_hex32(value).ok_or_else(|| {
                RegistryError::MalformedCheckpoint("proof hash is not canonical hex".into())
            })
        })
        .collect()
}

fn validate_audit_state(state: &ComplianceAuditState, expected_origin: &str) -> Result<()> {
    if state.origin != expected_origin
        || state.checkpoint_note.is_empty()
        || state.checkpoint_note.len() > MAX_CHECKPOINT_NOTE_BYTES
        || state.witnessed_at == 0
        || state.frontier.size() != state.size
        || !state.frontier.validate()
        || state.frontier.root() != Some(state.root)
        || state.keys.len() > MAX_COMPLIANCE_KEYS
    {
        return Err(RegistryError::MalformedCheckpoint(
            "persisted compliance audit state is malformed".into(),
        ));
    }
    for key in &state.keys {
        if key.log_index >= state.size
            || audited_compliance_key(&key.publication, key.log_index)? != *key
        {
            return Err(RegistryError::MalformedEntry(
                "persisted audited compliance key is malformed".into(),
            ));
        }
    }
    Ok(())
}

fn validate_handle_audit_state(state: &HandleAuditState, expected_origin: &str) -> Result<()> {
    if state.origin != expected_origin
        || state.checkpoint_note.is_empty()
        || state.checkpoint_note.len() > MAX_CHECKPOINT_NOTE_BYTES
        || state.witnessed_at == 0
        || state.frontier.size() != state.size
        || !state.frontier.validate()
        || state.frontier.root() != Some(state.root)
    {
        return Err(RegistryError::MalformedCheckpoint(
            "persisted handle audit state is malformed".into(),
        ));
    }
    Ok(())
}

fn validate_audited_handle_binding(handle: &Handle, binding: &AuditedHandleBinding) -> Result<()> {
    keys::verifying_key_from_bytes(&binding.pubkey)
        .map_err(|_| RegistryError::MalformedEntry("persisted handle key is invalid".into()))?;
    let synthetic = LogEntry::handle_claim(
        binding.log_index,
        handle.as_path(),
        hex(&binding.pubkey),
        binding.subject.clone(),
        1,
    );
    synthetic
        .leaf_bytes()
        .map_err(|error| RegistryError::MalformedEntry(error.to_string()))?;
    Ok(())
}

fn audited_compliance_key(
    publication: &ComplianceKeyPublish,
    log_index: u64,
) -> Result<AuditedComplianceKey> {
    publication
        .key_id
        .encode()
        .map_err(|error| RegistryError::MalformedEntry(error.to_string()))?;
    let public_key = parse_hex32(&publication.public_key).ok_or_else(|| {
        RegistryError::MalformedEntry("compliance public key is not canonical hex".into())
    })?;
    if public_key == [0u8; 32]
        || validate_compliance_epoch(
            &publication.key_id,
            publication.not_before_ms,
            publication.not_after_ms,
        )
        .is_err()
    {
        return Err(RegistryError::MalformedEntry(
            "compliance key has an invalid publication interval".into(),
        ));
    }
    Ok(AuditedComplianceKey {
        publication: publication.clone(),
        public_key,
        log_index,
    })
}

fn validate_compliance_transition(
    previous: Option<&AuditedComplianceKey>,
    next: &AuditedComplianceKey,
) -> Result<()> {
    let Some(previous) = previous else {
        return if next.publication.status == ComplianceKeyStatus::Active {
            Ok(())
        } else {
            Err(RegistryError::MalformedEntry(
                "an audited compliance key must be published active before status changes".into(),
            ))
        };
    };
    if next.log_index <= previous.log_index
        || next.public_key != previous.public_key
        || next.publication.not_before_ms != previous.publication.not_before_ms
        || next.publication.not_after_ms != previous.publication.not_after_ms
        || next.publication.status as u8 <= previous.publication.status as u8
        || !matches!(
            next.publication.status,
            ComplianceKeyStatus::Retired | ComplianceKeyStatus::Revoked
        )
    {
        return Err(RegistryError::MalformedEntry(
            "audited compliance-key status transition is invalid".into(),
        ));
    }
    Ok(())
}

fn parse_hex32(input: &str) -> Option<[u8; 32]> {
    if input.len() != 64 || input.bytes().any(|byte| !byte.is_ascii_hexdigit()) {
        return None;
    }
    let mut out = [0u8; 32];
    for (index, chunk) in input.as_bytes().chunks_exact(2).enumerate() {
        out[index] = u8::from_str_radix(std::str::from_utf8(chunk).ok()?, 16).ok()?;
    }
    (hex(&out) == input).then_some(out)
}

fn hex(bytes: &[u8]) -> String {
    bytes.iter().map(|byte| format!("{byte:02x}")).collect()
}

#[cfg(test)]
mod tests {
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::sync::{Arc, Mutex};

    use axum::body::{Body, Bytes};
    use axum::extract::Query;
    use axum::response::Response;
    use axum::routing::get;
    use axum::{Json, Router};
    use ed25519_dalek::{Signer, SigningKey};
    use serde_json::{json, Value};
    use tokio::io::{AsyncReadExt, AsyncWriteExt};
    use tokio_stream::wrappers::ReceiverStream;

    use super::*;
    use crate::log::MerkleLog;
    use crate::ComplianceKeyStatus;
    use pigeonpost_compliance_format::{ComplianceKeyId, CompliancePurpose, Jurisdiction};
    use pigeonpost_core::{keys, Identity};

    const JANUARY_1970: u64 = 0;
    const FEBRUARY_1970: u64 = 2_678_400_000;

    #[tokio::test(flavor = "current_thread")]
    async fn audit_cpu_lane_is_fail_fast_and_cancellation_keeps_capacity_accounted() {
        let lane = AuditCpuLane::with_limit(1);
        let worker_lane = lane.clone();
        let (started_tx, started_rx) = tokio::sync::oneshot::channel();
        let (release_tx, release_rx) = std::sync::mpsc::channel();
        let worker = tokio::spawn(async move {
            worker_lane
                .run(move || {
                    let _ = started_tx.send(());
                    release_rx
                        .recv()
                        .map_err(|_| RegistryError::RegistryUnavailable)?;
                    Ok(())
                })
                .await
        });
        started_rx.await.unwrap();

        let heartbeat = tokio::spawn(async {
            tokio::task::yield_now().await;
            true
        });
        assert!(tokio::time::timeout(Duration::from_millis(100), heartbeat)
            .await
            .unwrap()
            .unwrap());
        assert!(matches!(
            lane.run(|| Ok(())).await,
            Err(RegistryError::Overloaded)
        ));

        worker.abort();
        assert!(worker.await.unwrap_err().is_cancelled());
        assert!(matches!(
            lane.run(|| Ok(())).await,
            Err(RegistryError::Overloaded)
        ));

        release_tx.send(()).unwrap();
        tokio::time::timeout(Duration::from_secs(60), async {
            loop {
                match lane.run(|| Ok(())).await {
                    Ok(()) => break,
                    Err(RegistryError::Overloaded) => tokio::task::yield_now().await,
                    Err(error) => panic!("unexpected audit lane result: {error}"),
                }
            }
        })
        .await
        .unwrap();
    }

    fn publication_handle(
        log_index: u64,
        pubkey: [u8; PUBLIC_KEY_LENGTH],
        entry_kind: EntryKind,
    ) -> VerifiedHandle {
        VerifiedHandle {
            handle: Handle::parse("/github/alice").unwrap(),
            pubkey,
            subject: "github:alice".into(),
            log_index,
            entry_kind,
            checkpoint: Checkpoint {
                origin: "registry.test/log".into(),
                size: log_index + 1,
                root: [0; 32],
            },
            witnessed_at: Some(1),
        }
    }

    #[test]
    fn publication_receipt_classification_only_retries_strictly_older_bindings() {
        let expected = [7; PUBLIC_KEY_LENGTH];
        assert_eq!(
            publication_handle(3, [1; PUBLIC_KEY_LENGTH], EntryKind::HandleClaim)
                .publication_against(4, &expected, "handle_rotate"),
            HandlePublication::Pending
        );
        assert_eq!(
            publication_handle(4, expected, EntryKind::HandleRotation).publication_against(
                4,
                &expected,
                "handle_rotate",
            ),
            HandlePublication::Ready
        );
        assert_eq!(
            publication_handle(4, [1; PUBLIC_KEY_LENGTH], EntryKind::HandleRotation)
                .publication_against(4, &expected, "handle_rotate"),
            HandlePublication::Mismatch
        );
        assert_eq!(
            publication_handle(4, expected, EntryKind::HandleClaim).publication_against(
                4,
                &expected,
                "handle_rotate",
            ),
            HandlePublication::Mismatch
        );
        assert_eq!(
            publication_handle(5, expected, EntryKind::HandleRotation).publication_against(
                4,
                &expected,
                "handle_rotate",
            ),
            HandlePublication::Mismatch
        );
    }

    #[test]
    fn registry_url_is_an_https_or_numeric_loopback_origin() {
        for accepted in [
            "https://registry.example",
            "https://8.8.8.8",
            "https://[2606:4700:4700::1111]",
            "http://127.0.0.1:7718",
            "http://[::1]:7718",
        ] {
            assert!(validate_base_url(accepted).is_ok(), "rejected {accepted}");
        }
        for rejected in [
            "http://localhost:7718",
            "http://localhost.:7718",
            "https://localhost:7718",
            "https://localhost.:7718",
            "https://api.localhost:7718",
            "http://192.0.2.1:7718",
            "https://10.0.0.1",
            "https://169.254.169.254",
            "https://192.0.2.1",
            "https://[::ffff:127.0.0.1]",
            "https://[2001:db8::1]",
            "https://registry.example:0",
            "https://user@registry.example",
            "https://registry.example/prefix",
            "https://registry.example?query=1",
        ] {
            assert!(validate_base_url(rejected).is_err(), "accepted {rejected}");
        }
    }

    #[test]
    fn registry_dns_rejects_empty_oversized_and_mixed_private_answer_sets() {
        assert!(validate_resolved_addresses(Vec::new()).is_err());
        assert!(validate_resolved_addresses(vec![
            "93.184.216.34:443".parse().unwrap(),
            "10.0.0.1:443".parse().unwrap(),
        ])
        .is_err());
        assert!(
            validate_resolved_addresses(vec!["93.184.216.34:443".parse().unwrap(); 17]).is_err()
        );

        let public = vec![
            "93.184.216.34:443".parse().unwrap(),
            "[2606:4700:4700::1111]:443".parse().unwrap(),
        ];
        assert_eq!(validate_resolved_addresses(public.clone()).unwrap(), public);
    }

    #[tokio::test]
    async fn registry_resolver_returns_only_the_complete_pinned_answer_set() {
        let resolver = PinnedRegistryResolver::new("registry.example");
        let expected = vec![
            "93.184.216.34:443".parse().unwrap(),
            "[2606:4700:4700::1111]:443".parse().unwrap(),
        ];
        resolver.addresses.set(expected.clone()).unwrap();

        for _ in 0..2 {
            let addresses = Resolve::resolve(&resolver, "registry.example".parse().unwrap())
                .await
                .unwrap()
                .collect::<Vec<_>>();
            assert_eq!(addresses, expected);
        }
        assert!(
            Resolve::resolve(&resolver, "attacker.example".parse().unwrap())
                .await
                .is_err()
        );
    }

    #[tokio::test]
    async fn registry_client_never_follows_redirects_to_a_private_target() {
        let target_listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let target_address = target_listener.local_addr().unwrap();
        let target_hits = Arc::new(AtomicUsize::new(0));
        let observed_hits = Arc::clone(&target_hits);
        let target = tokio::spawn(async move {
            if tokio::time::timeout(Duration::from_millis(250), target_listener.accept())
                .await
                .is_ok()
            {
                observed_hits.fetch_add(1, Ordering::SeqCst);
            }
        });

        let redirect_listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let redirect_address = redirect_listener.local_addr().unwrap();
        let redirect = tokio::spawn(async move {
            let (mut stream, _) = redirect_listener.accept().await.unwrap();
            let mut request = [0u8; 2_048];
            let _ = stream.read(&mut request).await;
            let response = format!(
                "HTTP/1.1 302 Found\r\nLocation: http://{target_address}/private\r\nContent-Length: 0\r\nConnection: close\r\n\r\n"
            );
            stream.write_all(response.as_bytes()).await.unwrap();
        });

        let registry_key = SigningKey::from_bytes(&[0x71; 32]);
        let witness = SigningKey::from_bytes(&[0x72; 32]);
        let trust = RegistryTrust::new(
            "registry.test/log",
            registry_key.verifying_key().to_bytes(),
            vec![WitnessKey::new("witness", witness.verifying_key()).unwrap()],
            1,
            CheckpointPin {
                size: 0,
                root: empty_root(),
            },
            60,
            5,
        )
        .unwrap();
        let client = RegistryClient::new(&format!("http://{redirect_address}"), trust).unwrap();
        assert!(client.get_json::<Value>("redirect").await.is_err());
        redirect.await.unwrap();
        target.await.unwrap();
        assert_eq!(target_hits.load(Ordering::SeqCst), 0);
    }

    #[test]
    fn v0_2_network_budget_reserves_the_specified_local_headroom() {
        let segments = V0_2_AUDIT_MAX_LEAVES.div_ceil(AUDIT_DUMP_SEGMENT_ENTRIES);
        let transfer_ms = V0_2_AUDIT_MAX_CANONICAL_BYTES
            .saturating_mul(1_000)
            .div_ceil(V0_2_AUDIT_MIN_EFFECTIVE_BYTES_PER_SEC);
        let rtt_ms = segments.saturating_mul(V0_2_AUDIT_MAX_RTT.as_millis() as u64);
        let network_budget_ms = transfer_ms.saturating_add(rtt_ms);
        let local_headroom_ms = REGISTRY_AUDIT_TOTAL_TIMEOUT
            .as_millis()
            .saturating_sub(u128::from(network_budget_ms));

        assert_eq!(segments, 123);
        assert_eq!(network_budget_ms, 37_900);
        assert_eq!(local_headroom_ms, 82_100);
    }

    #[test]
    fn maximum_valid_json_audit_page_fits_the_response_cap() {
        let loft = SigningKey::from_bytes(&[94u8; 32]);
        let endpoint = format!("https://{}", "a".repeat(2_040));
        assert_eq!(endpoint.len(), 2_048);
        let operator = "\"".repeat(256);
        let loft_pubkey = hex(loft.verifying_key().as_bytes());
        let claim = crate::entry::directory_add_claim_payload(
            &endpoint,
            &loft_pubkey,
            Some(&operator),
            u64::MAX / (1024 * 1024 * 1024),
            u64::MAX,
            true,
            u32::MAX,
            2 * 1024 * 1024,
            u64::MAX,
        )
        .unwrap();
        let payload = crate::DirectoryAdd::authenticated(
            endpoint,
            loft_pubkey,
            Some(operator),
            u64::MAX / (1024 * 1024 * 1024),
            u64::MAX,
            true,
            u32::MAX,
            2 * 1024 * 1024,
            u64::MAX,
            hex(&loft.sign(&claim).to_bytes()),
        )
        .unwrap();
        let maximal_line = serde_json::to_vec(&LogEntry::directory_add(
            u64::MAX,
            payload.clone(),
            u64::MAX,
        ))
        .unwrap()
        .len()
            + 1;
        assert!(maximal_line <= MAX_AUDIT_DUMP_LINE_BYTES);
        assert!(
            (maximal_line as u64).saturating_mul(AUDIT_DUMP_SEGMENT_ENTRIES)
                <= MAX_AUDIT_SEGMENT_BYTES
        );
        let entries: Vec<_> = (0..AUDIT_PAGE_ENTRIES)
            .map(|seq| LogEntry::directory_add(seq, payload.clone(), u64::MAX))
            .inspect(|entry| {
                entry.leaf_bytes().expect("maximal entry must be valid");
            })
            .collect();
        let response = json!({
            "from": 0,
            "to": AUDIT_PAGE_ENTRIES,
            "tree_size": AUDIT_PAGE_ENTRIES,
            "root": "ff".repeat(32),
            "checkpoint": "x".repeat(MAX_CHECKPOINT_NOTE_BYTES),
            "entries": entries,
        });

        let encoded = serde_json::to_vec(&response).unwrap();
        assert!(
            encoded.len() <= MAX_RESPONSE_BYTES,
            "maximum audit page encoded to {} bytes",
            encoded.len()
        );
    }

    #[test]
    fn registry_trust_rejects_weak_checkpoint_and_witness_keys() {
        let weak_bytes = {
            let mut bytes = [0u8; 32];
            bytes[0] = 1;
            bytes
        };
        let weak_key = ed25519_dalek::VerifyingKey::from_bytes(&weak_bytes).unwrap();
        assert!(weak_key.is_weak());
        assert!(WitnessKey::new("weak-witness", weak_key).is_err());
        assert!(RegistryTrust::new(
            "registry.test/log",
            weak_bytes,
            Vec::new(),
            0,
            CheckpointPin {
                size: 0,
                root: empty_root(),
            },
            0,
            0,
        )
        .is_err());
    }

    #[test]
    fn registry_trust_requires_a_strictly_intersecting_quorum() {
        let checkpoint_key = SigningKey::from_bytes(&[70; 32]);
        let build = |count: usize, threshold: usize| {
            let witnesses = (0..count)
                .map(|index| {
                    let seed = u8::try_from(index + 71).unwrap();
                    WitnessKey::new(
                        format!("witness-{index}"),
                        SigningKey::from_bytes(&[seed; 32]).verifying_key(),
                    )
                    .unwrap()
                })
                .collect();
            RegistryTrust::new(
                "registry.test/log",
                checkpoint_key.verifying_key().to_bytes(),
                witnesses,
                threshold,
                CheckpointPin {
                    size: 0,
                    root: empty_root(),
                },
                60,
                5,
            )
        };

        assert!(build(1, 1).is_ok());
        assert!(build(3, 2).is_ok());
        assert!(build(0, 0).is_err());
        assert!(build(2, 1).is_err());
        assert!(build(3, 1).is_err());
    }

    #[tokio::test]
    async fn resolve_requires_fresh_witnesses_exact_leaf_and_inclusion() {
        let registry_key = SigningKey::from_bytes(&[1u8; 32]);
        let witness_a = SigningKey::from_bytes(&[2u8; 32]);
        let witness_b = SigningKey::from_bytes(&[3u8; 32]);
        let agent_key = SigningKey::from_bytes(&[4u8; 32]);
        let handle = Handle::parse("/github/alice").unwrap();
        let pubkey = hex(agent_key.verifying_key().as_bytes());
        let entry = LogEntry::handle_claim(
            0,
            handle.as_path(),
            pubkey.clone(),
            "github:provider-subject".into(),
            9_900_000,
        );
        let leaf = entry.leaf_bytes().unwrap();
        let mut log = MerkleLog::new();
        assert_eq!(log.append(&leaf), 0);
        let checkpoint = Checkpoint {
            origin: "registry.test/log".into(),
            size: 1,
            root: log.root(),
        };
        let mut note = checkpoint.sign(&registry_key);
        note.push_str(
            &checkpoint
                .cosignature_line("witness-a", &witness_a, 9_990)
                .unwrap(),
        );
        note.push_str(
            &checkpoint
                .cosignature_line("witness-b", &witness_b, 9_980)
                .unwrap(),
        );
        let resolve = json!({
            "handle": handle.as_path(),
            "pubkey": pubkey,
            "log_index": 0,
            "inclusion_proof": {
                "tree_size": 1,
                "root": hex(&checkpoint.root),
                "path": [],
                "checkpoint": note.clone(),
            }
        });
        let entries = json!({
            "from": 0,
            "to": 1,
            "tree_size": 1,
            "root": hex(&checkpoint.root),
            "checkpoint": note,
            "entries": [entry],
        });
        let app = static_registry(resolve, entries);
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        let server = tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });

        let witnesses = vec![
            WitnessKey::new("witness-a", witness_a.verifying_key()).unwrap(),
            WitnessKey::new("witness-b", witness_b.verifying_key()).unwrap(),
        ];
        let trust = RegistryTrust::new(
            "registry.test/log",
            registry_key.verifying_key().to_bytes(),
            witnesses,
            2,
            CheckpointPin {
                size: 0,
                root: empty_root(),
            },
            60,
            5,
        )
        .unwrap();
        let client = RegistryClient::new(&format!("http://{address}"), trust).unwrap();
        let verified = client.resolve(&handle, None, 10_000).await.unwrap();
        assert_eq!(verified.handle, handle);
        assert_eq!(verified.pubkey, agent_key.verifying_key().to_bytes());
        assert_eq!(verified.witnessed_at, Some(9_980));

        assert!(client.resolve(&handle, None, 10_100).await.is_err());
        server.abort();
    }

    #[tokio::test]
    async fn resolve_rejects_an_older_included_leaf_at_a_newer_witnessed_head() {
        #[derive(Deserialize)]
        struct Range {
            from: u64,
            to: u64,
        }

        let registry_key = SigningKey::from_bytes(&[81u8; 32]);
        let witness = SigningKey::from_bytes(&[82u8; 32]);
        let old_key = SigningKey::from_bytes(&[83u8; 32]);
        let new_key = SigningKey::from_bytes(&[84u8; 32]);
        let handle = Handle::parse("/github/alice").unwrap();
        let entries = Arc::new(vec![
            LogEntry::handle_claim(
                0,
                handle.as_path(),
                hex(old_key.verifying_key().as_bytes()),
                "github:stable-subject".into(),
                9_000_000,
            ),
            LogEntry::handle_rotation(
                1,
                handle.as_path(),
                hex(new_key.verifying_key().as_bytes()),
                "github:stable-subject".into(),
                9_500_000,
            ),
        ]);
        let mut log = MerkleLog::new();
        let mut dump = String::new();
        for entry in entries.iter() {
            log.append(&entry.leaf_bytes().unwrap());
            dump.push_str(&serde_json::to_string(entry).unwrap());
            dump.push('\n');
        }
        let checkpoint = Checkpoint {
            origin: "registry.test/log".into(),
            size: 2,
            root: log.root(),
        };
        let mut note = checkpoint.sign(&registry_key);
        note.push_str(
            &checkpoint
                .cosignature_line("independent", &witness, 9_990)
                .unwrap(),
        );

        // The old claim is genuinely included in the newer tree. A server that returns only that
        // proof is cryptographically truthful about history but dishonest about current state.
        let resolved = Arc::new(json!({
            "handle": handle.as_path(),
            "pubkey": hex(old_key.verifying_key().as_bytes()),
            "log_index": 0,
            "inclusion_proof": {
                "tree_size": 2,
                "root": hex(&checkpoint.root),
                "path": log.inclusion_proof(0, 2).unwrap()
                    .iter().map(|hash| hex(hash)).collect::<Vec<_>>(),
                "checkpoint": note.clone(),
            }
        }));
        let app = Router::new()
            .route(
                "/v1/resolve/github/alice",
                get({
                    let resolved = Arc::clone(&resolved);
                    move || {
                        let resolved = Arc::clone(&resolved);
                        async move { Json((*resolved).clone()) }
                    }
                }),
            )
            .route(
                "/v1/log/entries",
                get({
                    let entries = Arc::clone(&entries);
                    let checkpoint = checkpoint.clone();
                    let note = note.clone();
                    move |Query(range): Query<Range>| {
                        let entries = Arc::clone(&entries);
                        let checkpoint = checkpoint.clone();
                        let note = note.clone();
                        async move {
                            Json(json!({
                                "from": range.from,
                                "to": range.to,
                                "tree_size": checkpoint.size,
                                "root": hex(&checkpoint.root),
                                "checkpoint": note,
                                "entries": entries[range.from as usize..range.to as usize],
                            }))
                        }
                    }
                }),
            )
            .route("/v1/log/dump", get(move || async move { dump.clone() }));
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        let server = tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
        let trust = RegistryTrust::new(
            "registry.test/log",
            registry_key.verifying_key().to_bytes(),
            vec![WitnessKey::new("independent", witness.verifying_key()).unwrap()],
            1,
            CheckpointPin {
                size: 0,
                root: empty_root(),
            },
            60,
            5,
        )
        .unwrap();
        let client = RegistryClient::new(&format!("http://{address}"), trust).unwrap();
        assert!(matches!(
            client.resolve(&handle, None, 10_000).await,
            Err(RegistryError::MalformedEntry(message))
                if message.contains("not the latest audited")
        ));
        server.abort();
    }

    #[tokio::test]
    async fn resolve_audit_recovers_stale_short_dump_but_rejects_malformed_and_unknown_entries() {
        #[derive(Deserialize)]
        struct Range {
            from: u64,
            to: u64,
        }

        let registry_key = SigningKey::from_bytes(&[85u8; 32]);
        let witness = SigningKey::from_bytes(&[86u8; 32]);
        let identity = SigningKey::from_bytes(&[87u8; 32]);
        let handle = Handle::parse("/github/alice").unwrap();
        let entries = Arc::new(vec![
            LogEntry::handle_claim(
                0,
                handle.as_path(),
                hex(identity.verifying_key().as_bytes()),
                "github:stable-subject".into(),
                9_000_000,
            ),
            LogEntry::handle_claim(
                1,
                "/github/bob".into(),
                hex(SigningKey::from_bytes(&[88u8; 32])
                    .verifying_key()
                    .as_bytes()),
                "github:bob-subject".into(),
                9_100_000,
            ),
        ]);
        let mut log = MerkleLog::new();
        for entry in entries.iter() {
            log.append(&entry.leaf_bytes().unwrap());
        }
        let checkpoint = Checkpoint {
            origin: "registry.test/log".into(),
            size: 2,
            root: log.root(),
        };
        let mut note = checkpoint.sign(&registry_key);
        note.push_str(
            &checkpoint
                .cosignature_line("independent", &witness, 9_990)
                .unwrap(),
        );
        let resolved = Arc::new(json!({
            "handle": handle.as_path(),
            "pubkey": hex(identity.verifying_key().as_bytes()),
            "log_index": 0,
            "inclusion_proof": {
                "tree_size": 2,
                "root": hex(&checkpoint.root),
                "path": log.inclusion_proof(0, 2).unwrap()
                    .iter().map(|hash| hex(hash)).collect::<Vec<_>>(),
                "checkpoint": note.clone(),
            }
        }));
        let valid_first = format!("{}\n", serde_json::to_string(&entries[0]).unwrap());
        let out_of_sequence = format!("{}\n", serde_json::to_string(&entries[1]).unwrap());
        let cases = [
            (valid_first, true),
            (out_of_sequence, false),
            ("{not-json}\n".into(), false),
            (
                "{\"version\":1,\"seq\":0,\"type\":\"future_kind\",\"payload\":{},\"ts_ms\":1}\n"
                    .into(),
                false,
            ),
        ];

        for (body, should_recover) in cases {
            let app = Router::new()
                .route(
                    "/v1/resolve/github/alice",
                    get({
                        let resolved = Arc::clone(&resolved);
                        move || {
                            let resolved = Arc::clone(&resolved);
                            async move { Json((*resolved).clone()) }
                        }
                    }),
                )
                .route(
                    "/v1/log/entries",
                    get({
                        let entries = Arc::clone(&entries);
                        let checkpoint = checkpoint.clone();
                        let note = note.clone();
                        move |Query(range): Query<Range>| {
                            let entries = Arc::clone(&entries);
                            let checkpoint = checkpoint.clone();
                            let note = note.clone();
                            async move {
                                Json(json!({
                                    "from": range.from,
                                    "to": range.to,
                                    "tree_size": checkpoint.size,
                                    "root": hex(&checkpoint.root),
                                    "checkpoint": note,
                                    "entries": entries[range.from as usize..range.to as usize],
                                }))
                            }
                        }
                    }),
                )
                .route("/v1/log/dump", get(move || async move { body.clone() }));
            let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
            let address = listener.local_addr().unwrap();
            let server = tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
            let trust = RegistryTrust::new(
                "registry.test/log",
                registry_key.verifying_key().to_bytes(),
                vec![WitnessKey::new("independent", witness.verifying_key()).unwrap()],
                1,
                CheckpointPin {
                    size: 0,
                    root: empty_root(),
                },
                60,
                5,
            )
            .unwrap();
            let client = RegistryClient::new(&format!("http://{address}"), trust).unwrap();
            let result = client.resolve(&handle, None, 10_000).await;
            if should_recover {
                assert_eq!(result.unwrap().log_index(), 0);
            } else {
                assert!(matches!(result, Err(RegistryError::MalformedEntry(_))));
            }
            server.abort();
        }
    }

    #[tokio::test]
    async fn resolve_audit_rejects_provider_subject_state_machine_violations() {
        let handle = Handle::parse("/github/alice").unwrap();
        let first = AuditedHandleMutation {
            kind: crate::entry::EntryKind::HandleClaim,
            handle: handle.as_path(),
            pubkey: SigningKey::from_bytes(&[89u8; 32])
                .verifying_key()
                .to_bytes(),
            subject: "github:subject-a".into(),
            log_index: 0,
        };
        let first_binding = AuditedHandleBinding::new(
            &handle,
            first.pubkey,
            first.subject.clone(),
            first.log_index,
        )
        .unwrap();
        for invalid in [
            AuditedHandleMutation {
                kind: crate::entry::EntryKind::HandleClaim,
                log_index: 1,
                ..first.clone()
            },
            AuditedHandleMutation {
                kind: crate::entry::EntryKind::HandleRotation,
                pubkey: SigningKey::from_bytes(&[90u8; 32])
                    .verifying_key()
                    .to_bytes(),
                subject: "github:subject-b".into(),
                log_index: 1,
                ..first.clone()
            },
        ] {
            assert!(validate_handle_transition(Some(&first_binding), &invalid).is_err());
        }
        let rotation_first = AuditedHandleMutation {
            kind: crate::entry::EntryKind::HandleRotation,
            pubkey: SigningKey::from_bytes(&[91u8; 32])
                .verifying_key()
                .to_bytes(),
            log_index: 0,
            ..first
        };
        assert!(validate_handle_transition(None, &rotation_first).is_err());
    }

    #[test]
    fn in_memory_segment_rollback_tracks_only_touched_handles() {
        let mut projection = InMemoryHandleProjection::default();
        for index in 0..10_000u64 {
            projection.bindings.insert(
                format!("/github/existing-{index}"),
                AuditedHandleBinding {
                    pubkey: SigningKey::from_bytes(&[92u8; 32])
                        .verifying_key()
                        .to_bytes(),
                    subject: format!("github:existing-subject-{index}"),
                    log_index: index,
                },
            );
        }

        projection.begin_segment().unwrap();
        assert_eq!(projection.segment_undo.as_ref().unwrap().len(), 0);
        let mutation = AuditedHandleMutation {
            kind: crate::entry::EntryKind::HandleClaim,
            handle: "/github/segment-new".into(),
            pubkey: SigningKey::from_bytes(&[93u8; 32])
                .verifying_key()
                .to_bytes(),
            subject: "github:segment-new-subject".into(),
            log_index: 10_000,
        };
        projection.apply(&mutation).unwrap();

        assert_eq!(projection.segment_undo.as_ref().unwrap().len(), 1);
        assert_eq!(projection.bindings.len(), 10_001);
        projection.rollback_segment().unwrap();
        assert_eq!(projection.bindings.len(), 10_000);
        assert!(!projection.bindings.contains_key(mutation.handle()));
    }

    #[tokio::test]
    async fn complete_audit_derives_a_revocation_the_projection_omits() {
        let registry_key = SigningKey::from_bytes(&[21u8; 32]);
        let witness = SigningKey::from_bytes(&[22u8; 32]);
        let holder = Identity::from_seed([23u8; 32]);
        let public_key = keys::x25519_public(&holder);
        let key_id = ComplianceKeyId::new(
            CompliancePurpose::Attribution,
            Jurisdiction::Test,
            [24u8; 32],
            JANUARY_1970,
            1,
        );
        let active = ComplianceKeyPublish {
            key_id,
            public_key: hex(&public_key),
            not_before_ms: JANUARY_1970,
            not_after_ms: FEBRUARY_1970,
            status: ComplianceKeyStatus::Active,
        };
        let mut revoked = active.clone();
        revoked.status = ComplianceKeyStatus::Revoked;
        let active_entry = LogEntry::compliance_key(0, active.clone(), 9_000_000);
        let revoked_entry = LogEntry::compliance_key(1, revoked.clone(), 9_500_000);
        let mut log = MerkleLog::new();
        log.append(&active_entry.leaf_bytes().unwrap());
        log.append(&revoked_entry.leaf_bytes().unwrap());
        let checkpoint = Checkpoint {
            origin: "registry.test/log".into(),
            size: 2,
            root: log.root(),
        };
        let mut note = checkpoint.sign(&registry_key);
        note.push_str(
            &checkpoint
                .cosignature_line("independent", &witness, 9_990)
                .unwrap(),
        );

        // This is a cryptographically valid projection of the first publication, but it omits
        // the later revocation. Inclusion alone cannot establish projection completeness.
        let projection = json!({
            "tree_size": 2,
            "root": hex(&checkpoint.root),
            "checkpoint": note,
            "keys": [{
                "key_id_hex": hex(&key_id.encode().unwrap()),
                "publication": active,
                "log_index": 0,
                "inclusion_path": log.inclusion_proof(0, 2).unwrap()
                    .iter().map(|hash| hex(hash)).collect::<Vec<_>>(),
                "entry": active_entry,
            }],
        });
        let entries = json!({
            "from": 0,
            "to": 2,
            "tree_size": 2,
            "root": hex(&checkpoint.root),
            "checkpoint": note,
            "entries": [active_entry, revoked_entry],
        });
        let app = static_compliance_registry(projection, entries);
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        let server = tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });

        let trust = RegistryTrust::new(
            "registry.test/log",
            registry_key.verifying_key().to_bytes(),
            vec![WitnessKey::new("independent", witness.verifying_key()).unwrap()],
            1,
            CheckpointPin {
                size: 0,
                root: empty_root(),
            },
            60,
            5,
        )
        .unwrap();
        let client = RegistryClient::new(&format!("http://{address}"), trust).unwrap();
        let (verified, audit) = client
            .compliance_keys_audited(None, None, 10_000)
            .await
            .unwrap();
        assert_eq!(verified.keys().len(), 1);
        assert_eq!(verified.keys()[0].log_index(), 1);
        assert_eq!(
            verified.keys()[0].publication().status,
            ComplianceKeyStatus::Revoked
        );
        assert_eq!(audit.checkpoint(), checkpoint);

        // The compact state survives a strict serialization round-trip and avoids replaying old
        // leaves when the witnessed head has not advanced.
        let encoded = serde_json::to_vec(&audit).unwrap();
        let restored: ComplianceAuditState = serde_json::from_slice(&encoded).unwrap();
        let (unchanged, restored) = client
            .compliance_keys_audited(Some(Arc::new(restored)), None, 10_000)
            .await
            .unwrap();
        assert_eq!(unchanged.keys()[0].log_index(), 1);
        assert_eq!(restored, audit);
        server.abort();
    }

    #[tokio::test]
    async fn page_fallback_tampering_fails_final_witnessed_root_authentication() {
        let registry_key = SigningKey::from_bytes(&[97u8; 32]);
        let witness = SigningKey::from_bytes(&[98u8; 32]);
        let original = LogEntry::handle_claim(
            0,
            "/github/original".into(),
            hex(SigningKey::from_bytes(&[99u8; 32])
                .verifying_key()
                .as_bytes()),
            "github:original-subject".into(),
            9_000_000,
        );
        let tampered = LogEntry::handle_claim(
            0,
            "/github/tampered".into(),
            hex(SigningKey::from_bytes(&[100u8; 32])
                .verifying_key()
                .as_bytes()),
            "github:tampered-subject".into(),
            9_000_000,
        );
        let mut log = MerkleLog::new();
        log.append(&original.leaf_bytes().unwrap());
        let checkpoint = Checkpoint {
            origin: "registry.test/log".into(),
            size: 1,
            root: log.root(),
        };
        let mut note = checkpoint.sign(&registry_key);
        note.push_str(
            &checkpoint
                .cosignature_line("independent", &witness, 9_990)
                .unwrap(),
        );
        let projection = json!({
            "tree_size": 1,
            "root": hex(&checkpoint.root),
            "checkpoint": note,
            "keys": [],
        });
        let pages = json!({
            "from": 0,
            "to": 1,
            "tree_size": 1,
            "root": hex(&checkpoint.root),
            "checkpoint": note,
            "entries": [tampered],
        });
        // `static_compliance_registry` deliberately has no dump route, selecting JSON fallback.
        let app = static_compliance_registry(projection, pages);
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        let server = tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
        let trust = RegistryTrust::new(
            "registry.test/log",
            registry_key.verifying_key().to_bytes(),
            vec![WitnessKey::new("independent", witness.verifying_key()).unwrap()],
            1,
            CheckpointPin {
                size: 0,
                root: empty_root(),
            },
            60,
            5,
        )
        .unwrap();
        let client = RegistryClient::new(&format!("http://{address}"), trust).unwrap();

        assert!(matches!(
            client.compliance_keys_audited(None, None, 10_000).await,
            Err(RegistryError::MalformedCheckpoint(message))
                if message.contains("root")
        ));
        server.abort();
    }

    #[tokio::test]
    async fn fresh_audit_streams_one_bounded_dump_and_never_pages_the_full_log() {
        let registry_key = SigningKey::from_bytes(&[25u8; 32]);
        let witness = SigningKey::from_bytes(&[26u8; 32]);
        let key_id = ComplianceKeyId::new(
            CompliancePurpose::Attribution,
            Jurisdiction::Test,
            [27u8; 32],
            JANUARY_1970,
            1,
        );
        let active = ComplianceKeyPublish {
            key_id,
            public_key: "28".repeat(32),
            not_before_ms: JANUARY_1970,
            not_after_ms: FEBRUARY_1970,
            status: ComplianceKeyStatus::Active,
        };
        let mut revoked = active.clone();
        revoked.status = ComplianceKeyStatus::Revoked;
        let mut entries = vec![
            LogEntry::compliance_key(0, active, 9_000_000),
            LogEntry::compliance_key(1, revoked, 9_500_000),
        ];
        // Cross the old 128-entry page boundary so this test would need multiple range calls on the
        // path it is designed to replace.
        for seq in 2..(AUDIT_PAGE_ENTRIES + 50) {
            entries.push(LogEntry::handle_claim(
                seq,
                format!("/github/user{seq}"),
                "29".repeat(32),
                format!("github:subject-{seq}"),
                9_500_000 + seq,
            ));
        }
        let mut log = MerkleLog::new();
        let mut dump = String::new();
        for entry in &entries {
            log.append(&entry.leaf_bytes().unwrap());
            dump.push_str(&serde_json::to_string(entry).unwrap());
            dump.push('\n');
        }
        let checkpoint = Checkpoint {
            origin: "registry.test/log".into(),
            size: entries.len() as u64,
            root: log.root(),
        };
        let mut note = checkpoint.sign(&registry_key);
        note.push_str(
            &checkpoint
                .cosignature_line("independent", &witness, 9_990)
                .unwrap(),
        );
        let projection = Arc::new(json!({
            "tree_size": checkpoint.size,
            "root": hex(&checkpoint.root),
            "checkpoint": note,
            "keys": [],
        }));
        let dump = Arc::new(dump);
        let dump_calls = Arc::new(AtomicUsize::new(0));
        let range_calls = Arc::new(AtomicUsize::new(0));
        let app = Router::new()
            .route(
                "/v1/compliance-keys",
                get({
                    let projection = Arc::clone(&projection);
                    move || {
                        let projection = Arc::clone(&projection);
                        async move { Json((*projection).clone()) }
                    }
                }),
            )
            .route(
                "/v1/log/dump",
                get({
                    let dump = Arc::clone(&dump);
                    let dump_calls = Arc::clone(&dump_calls);
                    move || {
                        let dump = Arc::clone(&dump);
                        let dump_calls = Arc::clone(&dump_calls);
                        async move {
                            dump_calls.fetch_add(1, Ordering::SeqCst);
                            (*dump).clone()
                        }
                    }
                }),
            )
            .route(
                "/v1/log/entries",
                get({
                    let range_calls = Arc::clone(&range_calls);
                    move || {
                        let range_calls = Arc::clone(&range_calls);
                        async move {
                            range_calls.fetch_add(1, Ordering::SeqCst);
                            (
                                StatusCode::INTERNAL_SERVER_ERROR,
                                "unexpected range request",
                            )
                        }
                    }
                }),
            );
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        let server = tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });

        let trust = RegistryTrust::new(
            "registry.test/log",
            registry_key.verifying_key().to_bytes(),
            vec![WitnessKey::new("independent", witness.verifying_key()).unwrap()],
            1,
            CheckpointPin {
                size: 0,
                root: empty_root(),
            },
            60,
            5,
        )
        .unwrap();
        let client = RegistryClient::new(&format!("http://{address}"), trust).unwrap();
        let (verified, audit) = client
            .compliance_keys_audited(None, None, 10_000)
            .await
            .unwrap();
        assert_eq!(audit.checkpoint(), checkpoint);
        assert_eq!(verified.keys().len(), 1);
        assert_eq!(verified.keys()[0].log_index(), 1);
        assert_eq!(
            verified.keys()[0].publication().status,
            ComplianceKeyStatus::Revoked
        );
        assert_eq!(dump_calls.load(Ordering::SeqCst), 1);
        assert_eq!(range_calls.load(Ordering::SeqCst), 0);
        server.abort();
    }

    #[tokio::test]
    async fn fresh_audit_authenticates_multiple_exact_dump_segments() {
        #[derive(Deserialize)]
        struct Range {
            from: u64,
            to: u64,
        }

        let registry_key = SigningKey::from_bytes(&[101u8; 32]);
        let witness = SigningKey::from_bytes(&[102u8; 32]);
        let identity = SigningKey::from_bytes(&[103u8; 32]);
        let entries = Arc::new(
            (0..=AUDIT_DUMP_SEGMENT_ENTRIES)
                .map(|seq| {
                    LogEntry::handle_claim(
                        seq,
                        format!("/github/bulk{seq}"),
                        hex(identity.verifying_key().as_bytes()),
                        format!("github:bulk-subject-{seq}"),
                        9_000_000 + seq,
                    )
                })
                .collect::<Vec<_>>(),
        );
        let mut log = MerkleLog::new();
        for entry in entries.iter() {
            log.append(&entry.leaf_bytes().unwrap());
        }
        let checkpoint = Checkpoint {
            origin: "registry.test/log".into(),
            size: entries.len() as u64,
            root: log.root(),
        };
        let mut note = checkpoint.sign(&registry_key);
        note.push_str(
            &checkpoint
                .cosignature_line("independent", &witness, 9_990)
                .unwrap(),
        );
        let projection = Arc::new(json!({
            "tree_size": checkpoint.size,
            "root": hex(&checkpoint.root),
            "checkpoint": note,
            "keys": [],
        }));
        let requested = Arc::new(Mutex::new(Vec::new()));
        let range_calls = Arc::new(AtomicUsize::new(0));
        let app = Router::new()
            .route(
                "/v1/compliance-keys",
                get({
                    let projection = Arc::clone(&projection);
                    move || {
                        let projection = Arc::clone(&projection);
                        async move { Json((*projection).clone()) }
                    }
                }),
            )
            .route(
                "/v1/log/dump",
                get({
                    let entries = Arc::clone(&entries);
                    let requested = Arc::clone(&requested);
                    move |Query(range): Query<Range>| {
                        let entries = Arc::clone(&entries);
                        let requested = Arc::clone(&requested);
                        async move {
                            requested.lock().unwrap().push((range.from, range.to));
                            let mut body = Vec::new();
                            for entry in &entries[range.from as usize..range.to as usize] {
                                serde_json::to_writer(&mut body, entry).unwrap();
                                body.push(b'\n');
                            }
                            body
                        }
                    }
                }),
            )
            .route(
                "/v1/log/entries",
                get({
                    let range_calls = Arc::clone(&range_calls);
                    move || {
                        let range_calls = Arc::clone(&range_calls);
                        async move {
                            range_calls.fetch_add(1, Ordering::SeqCst);
                            StatusCode::INTERNAL_SERVER_ERROR
                        }
                    }
                }),
            );
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        let server = tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
        let trust = RegistryTrust::new(
            "registry.test/log",
            registry_key.verifying_key().to_bytes(),
            vec![WitnessKey::new("independent", witness.verifying_key()).unwrap()],
            1,
            CheckpointPin {
                size: 0,
                root: empty_root(),
            },
            60,
            5,
        )
        .unwrap();
        let client = RegistryClient::new(&format!("http://{address}"), trust).unwrap();
        let (verified, audit) = client
            .compliance_keys_audited(None, None, 10_000)
            .await
            .unwrap();

        assert!(verified.keys().is_empty());
        assert_eq!(audit.checkpoint(), checkpoint);
        assert_eq!(
            *requested.lock().unwrap(),
            vec![
                (0, AUDIT_DUMP_SEGMENT_ENTRIES),
                (AUDIT_DUMP_SEGMENT_ENTRIES, AUDIT_DUMP_SEGMENT_ENTRIES + 1),
            ]
        );
        assert_eq!(range_calls.load(Ordering::SeqCst), 0);
        server.abort();
    }

    #[tokio::test]
    async fn dump_content_length_over_limit_selects_pages_without_reading_the_body() {
        let registry_key = SigningKey::from_bytes(&[52u8; 32]);
        let witness = SigningKey::from_bytes(&[53u8; 32]);
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        let server = tokio::spawn(async move {
            let (mut socket, _) = listener.accept().await.unwrap();
            let mut request = [0u8; 1024];
            let _ = socket.read(&mut request).await.unwrap();
            let response = format!(
                "HTTP/1.1 200 OK\r\nContent-Type: application/x-ndjson\r\nContent-Length: {}\r\n\r\n",
                MAX_AUDIT_SEGMENT_BYTES + 1
            );
            socket.write_all(response.as_bytes()).await.unwrap();
            socket.flush().await.unwrap();
            tokio::time::sleep(Duration::from_secs(1)).await;
        });
        let trust = RegistryTrust::new(
            "registry.test/log",
            registry_key.verifying_key().to_bytes(),
            vec![WitnessKey::new("independent", witness.verifying_key()).unwrap()],
            1,
            CheckpointPin {
                size: 0,
                root: empty_root(),
            },
            60,
            5,
        )
        .unwrap();
        let client = RegistryClient::new(&format!("http://{address}"), trust).unwrap();
        let outcome = client
            .audit_dump_range(0, 1, Duration::from_millis(500), |_, _| {
                panic!("an oversized dump body must not be consumed")
            })
            .await
            .unwrap();
        assert_eq!(outcome, AuditDumpOutcome::UseRangePages);
        server.abort();
    }

    #[tokio::test]
    async fn dump_delivery_statuses_select_json_fallback() {
        let registry_key = SigningKey::from_bytes(&[107u8; 32]);
        let witness = SigningKey::from_bytes(&[108u8; 32]);
        let statuses = Arc::new(vec![
            StatusCode::NOT_FOUND,
            StatusCode::METHOD_NOT_ALLOWED,
            StatusCode::REQUEST_TIMEOUT,
            StatusCode::PAYLOAD_TOO_LARGE,
            StatusCode::TOO_MANY_REQUESTS,
            StatusCode::SERVICE_UNAVAILABLE,
            StatusCode::GATEWAY_TIMEOUT,
            StatusCode::NOT_IMPLEMENTED,
        ]);
        let calls = Arc::new(AtomicUsize::new(0));
        let app = Router::new().route(
            "/v1/log/dump",
            get({
                let statuses = Arc::clone(&statuses);
                let calls = Arc::clone(&calls);
                move || {
                    let statuses = Arc::clone(&statuses);
                    let calls = Arc::clone(&calls);
                    async move { statuses[calls.fetch_add(1, Ordering::SeqCst)] }
                }
            }),
        );
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        let server = tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
        let trust = RegistryTrust::new(
            "registry.test/log",
            registry_key.verifying_key().to_bytes(),
            vec![WitnessKey::new("independent", witness.verifying_key()).unwrap()],
            1,
            CheckpointPin {
                size: 0,
                root: empty_root(),
            },
            60,
            5,
        )
        .unwrap();
        let client = RegistryClient::new(&format!("http://{address}"), trust).unwrap();

        for _ in 0..statuses.len() {
            assert_eq!(
                client
                    .audit_dump_range(0, 1, Duration::from_secs(1), |_, _| {
                        panic!("delivery status body must not be audited")
                    })
                    .await
                    .unwrap(),
                AuditDumpOutcome::UseRangePages
            );
        }
        assert_eq!(calls.load(Ordering::SeqCst), statuses.len());
        server.abort();
    }

    #[tokio::test]
    async fn dump_transfer_cap_selects_pages_after_a_partial_segment() {
        let registry_key = SigningKey::from_bytes(&[95u8; 32]);
        let witness = SigningKey::from_bytes(&[106u8; 32]);
        let entry = LogEntry::handle_claim(
            0,
            "/github/capped".into(),
            hex(SigningKey::from_bytes(&[96u8; 32])
                .verifying_key()
                .as_bytes()),
            "github:capped-subject".into(),
            1,
        );
        let first_line = Arc::new(format!("{}\n", serde_json::to_string(&entry).unwrap()));
        let app = Router::new().route(
            "/v1/log/dump",
            get(move || {
                let first_line = Arc::clone(&first_line);
                async move {
                    let (sender, receiver) = tokio::sync::mpsc::channel(2);
                    tokio::spawn(async move {
                        sender
                            .send(Ok::<_, std::io::Error>(Bytes::from((*first_line).clone())))
                            .await
                            .unwrap();
                        tokio::time::sleep(Duration::from_millis(20)).await;
                        let _ = sender.send(Ok(Bytes::from(vec![b'x'; 64]))).await;
                    });
                    Response::new(Body::from_stream(ReceiverStream::new(receiver)))
                }
            }),
        );
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        let server = tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
        let trust = RegistryTrust::new(
            "registry.test/log",
            registry_key.verifying_key().to_bytes(),
            vec![WitnessKey::new("independent", witness.verifying_key()).unwrap()],
            1,
            CheckpointPin {
                size: 0,
                root: empty_root(),
            },
            60,
            5,
        )
        .unwrap();
        let client = RegistryClient::new(&format!("http://{address}"), trust).unwrap();
        let appended = Arc::new(AtomicUsize::new(0));
        let outcome = client
            .audit_dump_range_with_limit(
                0,
                2,
                Duration::from_secs(1),
                (serde_json::to_vec(&entry).unwrap().len() + 32) as u64,
                {
                    let appended = Arc::clone(&appended);
                    move |_, _| {
                        appended.fetch_add(1, Ordering::SeqCst);
                        Ok(())
                    }
                },
            )
            .await
            .unwrap();
        assert_eq!(outcome, AuditDumpOutcome::UseRangePages);
        assert_eq!(appended.load(Ordering::SeqCst), 1);
        server.abort();
    }

    #[tokio::test]
    async fn fresh_handle_audit_replays_the_partial_segment_after_dump_timeout() {
        #[derive(Deserialize)]
        struct Range {
            from: u64,
            to: u64,
        }

        let registry_key = SigningKey::from_bytes(&[43u8; 32]);
        let witness = SigningKey::from_bytes(&[44u8; 32]);
        let alice_key = SigningKey::from_bytes(&[45u8; 32]);
        let bob_key = SigningKey::from_bytes(&[46u8; 32]);
        let handle = Handle::parse("/github/alice").unwrap();
        let entries = Arc::new(vec![
            LogEntry::handle_claim(
                0,
                handle.as_path(),
                hex(alice_key.verifying_key().as_bytes()),
                "github:alice-subject".into(),
                9_000_000,
            ),
            LogEntry::handle_claim(
                1,
                "/github/bob".into(),
                hex(bob_key.verifying_key().as_bytes()),
                "github:bob-subject".into(),
                9_100_000,
            ),
        ]);
        let mut log = MerkleLog::new();
        for entry in entries.iter() {
            log.append(&entry.leaf_bytes().unwrap());
        }
        let checkpoint = Checkpoint {
            origin: "registry.test/log".into(),
            size: entries.len() as u64,
            root: log.root(),
        };
        let mut note = checkpoint.sign(&registry_key);
        note.push_str(
            &checkpoint
                .cosignature_line("independent", &witness, 9_990)
                .unwrap(),
        );
        let resolved = Arc::new(json!({
            "handle": handle.as_path(),
            "pubkey": hex(alice_key.verifying_key().as_bytes()),
            "log_index": 0,
            "inclusion_proof": {
                "tree_size": checkpoint.size,
                "root": hex(&checkpoint.root),
                "path": log.inclusion_proof(0, checkpoint.size).unwrap()
                    .iter().map(|hash| hex(hash)).collect::<Vec<_>>(),
                "checkpoint": note.clone(),
            }
        }));
        let first_dump_line =
            Arc::new(format!("{}\n", serde_json::to_string(&entries[0]).unwrap()));
        let second_dump_line =
            Arc::new(format!("{}\n", serde_json::to_string(&entries[1]).unwrap()));
        let dump_calls = Arc::new(AtomicUsize::new(0));
        let range_calls = Arc::new(AtomicUsize::new(0));
        let app = Router::new()
            .route(
                "/v1/resolve/github/alice",
                get({
                    let resolved = Arc::clone(&resolved);
                    move || {
                        let resolved = Arc::clone(&resolved);
                        async move { Json((*resolved).clone()) }
                    }
                }),
            )
            .route(
                "/v1/log/dump",
                get({
                    let first_dump_line = Arc::clone(&first_dump_line);
                    let second_dump_line = Arc::clone(&second_dump_line);
                    let dump_calls = Arc::clone(&dump_calls);
                    move || {
                        let first_dump_line = Arc::clone(&first_dump_line);
                        let second_dump_line = Arc::clone(&second_dump_line);
                        let dump_calls = Arc::clone(&dump_calls);
                        async move {
                            dump_calls.fetch_add(1, Ordering::SeqCst);
                            let (sender, receiver) = tokio::sync::mpsc::channel(2);
                            tokio::spawn(async move {
                                if sender
                                    .send(Ok::<_, std::io::Error>(Bytes::from(
                                        (*first_dump_line).clone(),
                                    )))
                                    .await
                                    .is_err()
                                {
                                    return;
                                }
                                tokio::time::sleep(Duration::from_millis(500)).await;
                                let _ = sender
                                    .send(Ok(Bytes::from((*second_dump_line).clone())))
                                    .await;
                            });
                            Response::new(Body::from_stream(ReceiverStream::new(receiver)))
                        }
                    }
                }),
            )
            .route(
                "/v1/log/entries",
                get({
                    let entries = Arc::clone(&entries);
                    let checkpoint = checkpoint.clone();
                    let note = note.clone();
                    let range_calls = Arc::clone(&range_calls);
                    move |Query(range): Query<Range>| {
                        let entries = Arc::clone(&entries);
                        let checkpoint = checkpoint.clone();
                        let note = note.clone();
                        let range_calls = Arc::clone(&range_calls);
                        async move {
                            range_calls.fetch_add(1, Ordering::SeqCst);
                            Json(json!({
                                "from": range.from,
                                "to": range.to,
                                "tree_size": checkpoint.size,
                                "root": hex(&checkpoint.root),
                                "checkpoint": note,
                                "entries": entries[range.from as usize..range.to as usize],
                            }))
                        }
                    }
                }),
            );
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        let server = tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
        let trust = RegistryTrust::new(
            "registry.test/log",
            registry_key.verifying_key().to_bytes(),
            vec![WitnessKey::new("independent", witness.verifying_key()).unwrap()],
            1,
            CheckpointPin {
                size: 0,
                root: empty_root(),
            },
            60,
            5,
        )
        .unwrap();
        let client = RegistryClient::new(&format!("http://{address}"), trust).unwrap();
        let projection = InMemoryHandleProjection::default();
        let (verified, audit, mut projection) = client
            .resolve_audited_with_timeouts(
                &handle,
                None,
                None,
                10_000,
                projection,
                Duration::from_secs(2),
                Duration::from_millis(100),
            )
            .await
            .unwrap();
        assert_eq!(verified.log_index(), 0);
        assert_eq!(audit.checkpoint(), checkpoint);
        assert_eq!(projection.binding(&handle).unwrap().unwrap().log_index, 0);
        assert_eq!(dump_calls.load(Ordering::SeqCst), 1);
        // One exact-leaf read plus one complete two-entry replay of the failed segment.
        assert_eq!(range_calls.load(Ordering::SeqCst), 2);
        server.abort();
    }

    #[tokio::test]
    async fn oversized_dump_falls_back_to_exact_bounded_compliance_pages() {
        #[derive(Deserialize)]
        struct Range {
            from: u64,
            to: u64,
        }

        let registry_key = SigningKey::from_bytes(&[47u8; 32]);
        let witness = SigningKey::from_bytes(&[48u8; 32]);
        let filler_key = SigningKey::from_bytes(&[49u8; 32]);
        let key_id = ComplianceKeyId::new(
            CompliancePurpose::Attribution,
            Jurisdiction::Test,
            [50u8; 32],
            JANUARY_1970,
            1,
        );
        let active = ComplianceKeyPublish {
            key_id,
            public_key: "51".repeat(32),
            not_before_ms: JANUARY_1970,
            not_after_ms: FEBRUARY_1970,
            status: ComplianceKeyStatus::Active,
        };
        let mut revoked = active.clone();
        revoked.status = ComplianceKeyStatus::Revoked;
        let mut entries = vec![
            LogEntry::compliance_key(0, active, 9_000_000),
            LogEntry::compliance_key(1, revoked, 9_100_000),
        ];
        for seq in 2..(AUDIT_PAGE_ENTRIES + 50) {
            entries.push(LogEntry::handle_claim(
                seq,
                format!("/github/page-user-{seq}"),
                hex(filler_key.verifying_key().as_bytes()),
                format!("github:page-subject-{seq}"),
                9_100_000 + seq,
            ));
        }
        let entries = Arc::new(entries);
        let mut log = MerkleLog::new();
        for entry in entries.iter() {
            log.append(&entry.leaf_bytes().unwrap());
        }
        let checkpoint = Checkpoint {
            origin: "registry.test/log".into(),
            size: entries.len() as u64,
            root: log.root(),
        };
        let mut note = checkpoint.sign(&registry_key);
        note.push_str(
            &checkpoint
                .cosignature_line("independent", &witness, 9_990)
                .unwrap(),
        );
        let projection = Arc::new(json!({
            "tree_size": checkpoint.size,
            "root": hex(&checkpoint.root),
            "checkpoint": note,
            "keys": [],
        }));
        let dump_calls = Arc::new(AtomicUsize::new(0));
        let range_calls = Arc::new(AtomicUsize::new(0));
        let app = Router::new()
            .route(
                "/v1/compliance-keys",
                get({
                    let projection = Arc::clone(&projection);
                    move || {
                        let projection = Arc::clone(&projection);
                        async move { Json((*projection).clone()) }
                    }
                }),
            )
            .route(
                "/v1/log/dump",
                get({
                    let dump_calls = Arc::clone(&dump_calls);
                    move || {
                        let dump_calls = Arc::clone(&dump_calls);
                        async move {
                            dump_calls.fetch_add(1, Ordering::SeqCst);
                            StatusCode::PAYLOAD_TOO_LARGE
                        }
                    }
                }),
            )
            .route(
                "/v1/log/entries",
                get({
                    let entries = Arc::clone(&entries);
                    let checkpoint = checkpoint.clone();
                    let note = note.clone();
                    let range_calls = Arc::clone(&range_calls);
                    move |Query(range): Query<Range>| {
                        let entries = Arc::clone(&entries);
                        let checkpoint = checkpoint.clone();
                        let note = note.clone();
                        let range_calls = Arc::clone(&range_calls);
                        async move {
                            range_calls.fetch_add(1, Ordering::SeqCst);
                            Json(json!({
                                "from": range.from,
                                "to": range.to,
                                "tree_size": checkpoint.size,
                                "root": hex(&checkpoint.root),
                                "checkpoint": note,
                                "entries": entries[range.from as usize..range.to as usize],
                            }))
                        }
                    }
                }),
            );
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        let server = tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
        let trust = RegistryTrust::new(
            "registry.test/log",
            registry_key.verifying_key().to_bytes(),
            vec![WitnessKey::new("independent", witness.verifying_key()).unwrap()],
            1,
            CheckpointPin {
                size: 0,
                root: empty_root(),
            },
            60,
            5,
        )
        .unwrap();
        let client = RegistryClient::new(&format!("http://{address}"), trust).unwrap();
        let (verified, audit) = client
            .compliance_keys_audited(None, None, 10_000)
            .await
            .unwrap();
        assert_eq!(audit.checkpoint(), checkpoint);
        assert_eq!(verified.keys().len(), 1);
        assert_eq!(verified.keys()[0].log_index(), 1);
        assert_eq!(
            verified.keys()[0].publication().status,
            ComplianceKeyStatus::Revoked
        );
        assert_eq!(dump_calls.load(Ordering::SeqCst), 1);
        assert_eq!(range_calls.load(Ordering::SeqCst), 2);
        server.abort();
    }

    #[tokio::test]
    async fn fresh_audit_rejects_an_unbounded_dump_line_before_allocation_growth() {
        let registry_key = SigningKey::from_bytes(&[35u8; 32]);
        let witness = SigningKey::from_bytes(&[36u8; 32]);
        let entry = LogEntry::handle_claim(
            0,
            "/github/bounded".into(),
            "37".repeat(32),
            "github:bounded-subject".into(),
            9_000_000,
        );
        let mut log = MerkleLog::new();
        log.append(&entry.leaf_bytes().unwrap());
        let checkpoint = Checkpoint {
            origin: "registry.test/log".into(),
            size: 1,
            root: log.root(),
        };
        let mut note = checkpoint.sign(&registry_key);
        note.push_str(
            &checkpoint
                .cosignature_line("independent", &witness, 9_990)
                .unwrap(),
        );
        let projection = Arc::new(json!({
            "tree_size": 1,
            "root": hex(&checkpoint.root),
            "checkpoint": note,
            "keys": [],
        }));
        let oversized = Arc::new(format!("{}\n", "x".repeat(MAX_AUDIT_DUMP_LINE_BYTES + 1)));
        let app = Router::new()
            .route(
                "/v1/compliance-keys",
                get({
                    let projection = Arc::clone(&projection);
                    move || {
                        let projection = Arc::clone(&projection);
                        async move { Json((*projection).clone()) }
                    }
                }),
            )
            .route(
                "/v1/log/dump",
                get(move || {
                    let oversized = Arc::clone(&oversized);
                    async move { (*oversized).clone() }
                }),
            );
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        let server = tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
        let trust = RegistryTrust::new(
            "registry.test/log",
            registry_key.verifying_key().to_bytes(),
            vec![WitnessKey::new("independent", witness.verifying_key()).unwrap()],
            1,
            CheckpointPin {
                size: 0,
                root: empty_root(),
            },
            60,
            5,
        )
        .unwrap();
        let client = RegistryClient::new(&format!("http://{address}"), trust).unwrap();
        assert!(matches!(
            client
                .compliance_keys_audited(None, None, 10_000)
                .await,
            Err(RegistryError::MalformedEntry(message))
                if message.contains("exceeds the client limit")
        ));
        server.abort();
    }

    #[tokio::test]
    async fn compliance_audit_deadline_covers_the_complete_operation() {
        let registry_key = SigningKey::from_bytes(&[38u8; 32]);
        let witness = SigningKey::from_bytes(&[39u8; 32]);
        let checkpoint = Checkpoint {
            origin: "registry.test/log".into(),
            size: 0,
            root: empty_root(),
        };
        let mut note = checkpoint.sign(&registry_key);
        note.push_str(
            &checkpoint
                .cosignature_line("independent", &witness, 9_990)
                .unwrap(),
        );
        let projection = Arc::new(json!({
            "tree_size": 0,
            "root": hex(&checkpoint.root),
            "checkpoint": note,
            "keys": [],
        }));
        let app = Router::new().route(
            "/v1/compliance-keys",
            get(move || {
                let projection = Arc::clone(&projection);
                async move {
                    tokio::time::sleep(Duration::from_millis(100)).await;
                    Json((*projection).clone())
                }
            }),
        );
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        let server = tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
        let trust = RegistryTrust::new(
            "registry.test/log",
            registry_key.verifying_key().to_bytes(),
            vec![WitnessKey::new("independent", witness.verifying_key()).unwrap()],
            1,
            CheckpointPin {
                size: 0,
                root: empty_root(),
            },
            60,
            5,
        )
        .unwrap();
        let client = RegistryClient::new(&format!("http://{address}"), trust).unwrap();
        assert!(matches!(
            client
                .compliance_keys_audited_with_deadline(
                    None,
                    None,
                    10_000,
                    Duration::from_millis(10),
                )
                .await,
            Err(RegistryError::RegistryUnavailable)
        ));
        server.abort();
    }

    #[tokio::test]
    async fn persisted_frontier_audits_only_new_leaves_after_growth() {
        #[derive(Deserialize)]
        struct Range {
            from: u64,
            to: u64,
        }

        let registry_key = SigningKey::from_bytes(&[31u8; 32]);
        let witness = SigningKey::from_bytes(&[32u8; 32]);
        let key_id = ComplianceKeyId::new(
            CompliancePurpose::Attribution,
            Jurisdiction::Test,
            [33u8; 32],
            JANUARY_1970,
            1,
        );
        let active = ComplianceKeyPublish {
            key_id,
            public_key: "34".repeat(32),
            not_before_ms: JANUARY_1970,
            not_after_ms: FEBRUARY_1970,
            status: ComplianceKeyStatus::Active,
        };
        let mut retired = active.clone();
        retired.status = ComplianceKeyStatus::Retired;
        let entries = Arc::new(vec![
            LogEntry::compliance_key(0, active, 9_000_000),
            LogEntry::compliance_key(1, retired, 9_500_000),
        ]);
        let mut log = MerkleLog::new();
        for entry in entries.iter() {
            log.append(&entry.leaf_bytes().unwrap());
        }
        let first_root = leaf_hash(&entries[0].leaf_bytes().unwrap());
        let final_root = log.root();
        let checkpoints: Arc<Vec<Checkpoint>> = Arc::new(
            [(1, first_root), (2, final_root)]
                .into_iter()
                .map(|(size, root)| Checkpoint {
                    origin: "registry.test/log".into(),
                    size,
                    root,
                })
                .collect(),
        );
        let notes: Arc<Vec<String>> = Arc::new(
            checkpoints
                .iter()
                .map(|checkpoint| {
                    let mut note = checkpoint.sign(&registry_key);
                    note.push_str(
                        &checkpoint
                            .cosignature_line("independent", &witness, 9_990)
                            .unwrap(),
                    );
                    note
                })
                .collect(),
        );
        let phase = Arc::new(AtomicUsize::new(0));
        let app = Router::new()
            .route(
                "/v1/compliance-keys",
                get({
                    let phase = Arc::clone(&phase);
                    let checkpoints = Arc::clone(&checkpoints);
                    let notes = Arc::clone(&notes);
                    move || {
                        let index = phase.load(Ordering::SeqCst).min(1);
                        let checkpoint = checkpoints[index].clone();
                        let note = notes[index].clone();
                        async move {
                            Json(json!({
                                "tree_size": checkpoint.size,
                                "root": hex(&checkpoint.root),
                                "checkpoint": note,
                                "keys": [],
                            }))
                        }
                    }
                }),
            )
            .route(
                "/v1/log/entries",
                get({
                    let entries = Arc::clone(&entries);
                    let checkpoints = Arc::clone(&checkpoints);
                    let notes = Arc::clone(&notes);
                    move |Query(range): Query<Range>| {
                        let entries = Arc::clone(&entries);
                        let checkpoints = Arc::clone(&checkpoints);
                        let notes = Arc::clone(&notes);
                        async move {
                            let checkpoint = checkpoints[(range.to - 1) as usize].clone();
                            Json(json!({
                                "from": range.from,
                                "to": range.to,
                                "tree_size": checkpoint.size,
                                "root": hex(&checkpoint.root),
                                "checkpoint": notes[(range.to - 1) as usize],
                                "entries": entries[range.from as usize..range.to as usize],
                            }))
                        }
                    }
                }),
            )
            .route(
                "/v1/log/consistency",
                get({
                    let checkpoint = checkpoints[1].clone();
                    let path: Vec<_> = log
                        .consistency_proof(1, 2)
                        .unwrap()
                        .iter()
                        .map(|hash| hex(hash))
                        .collect();
                    move || {
                        let checkpoint = checkpoint.clone();
                        let path = path.clone();
                        async move {
                            Json(json!({
                                "from": 1,
                                "to": 2,
                                "root": hex(&checkpoint.root),
                                "path": path,
                            }))
                        }
                    }
                }),
            );
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        let server = tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
        let trust = RegistryTrust::new(
            "registry.test/log",
            registry_key.verifying_key().to_bytes(),
            vec![WitnessKey::new("independent", witness.verifying_key()).unwrap()],
            1,
            CheckpointPin {
                size: 0,
                root: empty_root(),
            },
            60,
            5,
        )
        .unwrap();
        let client = RegistryClient::new(&format!("http://{address}"), trust).unwrap();
        let (first, audit) = client
            .compliance_keys_audited(None, None, 10_000)
            .await
            .unwrap();
        assert_eq!(
            first.keys()[0].publication().status,
            ComplianceKeyStatus::Active
        );

        phase.store(1, Ordering::SeqCst);
        let (second, audit) = client
            .compliance_keys_audited(Some(Arc::new(audit)), None, 10_000)
            .await
            .unwrap();
        assert_eq!(audit.size(), 2);
        assert_eq!(second.keys()[0].log_index(), 1);
        assert_eq!(
            second.keys()[0].publication().status,
            ComplianceKeyStatus::Retired
        );
        server.abort();
    }

    #[tokio::test]
    async fn persisted_handle_and_compliance_frontiers_stream_large_deltas_in_segments() {
        #[derive(Deserialize)]
        struct Range {
            from: u64,
            to: u64,
        }

        let registry_key = SigningKey::from_bytes(&[109u8; 32]);
        let witness = SigningKey::from_bytes(&[110u8; 32]);
        let alice_key = SigningKey::from_bytes(&[111u8; 32]);
        let filler_key = SigningKey::from_bytes(&[112u8; 32]);
        let handle = Handle::parse("/github/alice").unwrap();
        let entries = Arc::new(
            std::iter::once(LogEntry::handle_claim(
                0,
                handle.as_path(),
                hex(alice_key.verifying_key().as_bytes()),
                "github:alice-subject".into(),
                9_000_000,
            ))
            .chain((1..=AUDIT_DUMP_SEGMENT_ENTRIES + 1).map(|seq| {
                LogEntry::handle_claim(
                    seq,
                    format!("/github/delta{seq}"),
                    hex(filler_key.verifying_key().as_bytes()),
                    format!("github:delta-subject-{seq}"),
                    9_000_000 + seq,
                )
            }))
            .collect::<Vec<_>>(),
        );
        let first_leaf = entries[0].leaf_bytes().unwrap();
        let mut prior_frontier = MerkleFrontier::new();
        assert_eq!(prior_frontier.append(&first_leaf), Some(0));
        let prior_checkpoint = Checkpoint {
            origin: "registry.test/log".into(),
            size: 1,
            root: prior_frontier.root().unwrap(),
        };
        let mut prior_note = prior_checkpoint.sign(&registry_key);
        prior_note.push_str(
            &prior_checkpoint
                .cosignature_line("independent", &witness, 9_990)
                .unwrap(),
        );
        let previous_handle = HandleAuditState {
            origin: prior_checkpoint.origin.clone(),
            size: 1,
            root: prior_checkpoint.root,
            checkpoint_note: prior_note.clone(),
            witnessed_at: 9_990,
            frontier: prior_frontier.clone(),
        };
        let previous_compliance = ComplianceAuditState {
            origin: prior_checkpoint.origin.clone(),
            size: 1,
            root: prior_checkpoint.root,
            checkpoint_note: prior_note,
            witnessed_at: 9_990,
            frontier: prior_frontier,
            keys: Vec::new(),
        };

        let mut log = MerkleLog::new();
        for entry in entries.iter() {
            log.append(&entry.leaf_bytes().unwrap());
        }
        let checkpoint = Checkpoint {
            origin: prior_checkpoint.origin.clone(),
            size: entries.len() as u64,
            root: log.root(),
        };
        let mut note = checkpoint.sign(&registry_key);
        note.push_str(
            &checkpoint
                .cosignature_line("independent", &witness, 9_990)
                .unwrap(),
        );
        let resolved = Arc::new(json!({
            "handle": handle.as_path(),
            "pubkey": hex(alice_key.verifying_key().as_bytes()),
            "log_index": 0,
            "inclusion_proof": {
                "tree_size": checkpoint.size,
                "root": hex(&checkpoint.root),
                "path": log.inclusion_proof(0, checkpoint.size).unwrap()
                    .iter().map(|hash| hex(hash)).collect::<Vec<_>>(),
                "checkpoint": note,
            }
        }));
        let metadata = Arc::new(json!({
            "tree_size": checkpoint.size,
            "root": hex(&checkpoint.root),
            "checkpoint": note,
            "keys": [],
        }));
        let consistency_path = Arc::new(
            log.consistency_proof(1, checkpoint.size)
                .unwrap()
                .iter()
                .map(|hash| hex(hash))
                .collect::<Vec<_>>(),
        );
        let dump_ranges = Arc::new(Mutex::new(Vec::new()));
        let page_calls = Arc::new(AtomicUsize::new(0));
        let app = Router::new()
            .route(
                "/v1/resolve/github/alice",
                get({
                    let resolved = Arc::clone(&resolved);
                    move || {
                        let resolved = Arc::clone(&resolved);
                        async move { Json((*resolved).clone()) }
                    }
                }),
            )
            .route(
                "/v1/compliance-keys",
                get({
                    let metadata = Arc::clone(&metadata);
                    move || {
                        let metadata = Arc::clone(&metadata);
                        async move { Json((*metadata).clone()) }
                    }
                }),
            )
            .route(
                "/v1/log/entries",
                get({
                    let entries = Arc::clone(&entries);
                    let checkpoint = checkpoint.clone();
                    let note = note.clone();
                    let page_calls = Arc::clone(&page_calls);
                    move |Query(range): Query<Range>| {
                        let entries = Arc::clone(&entries);
                        let checkpoint = checkpoint.clone();
                        let note = note.clone();
                        let page_calls = Arc::clone(&page_calls);
                        async move {
                            page_calls.fetch_add(1, Ordering::SeqCst);
                            Json(json!({
                                "from": range.from,
                                "to": range.to,
                                "tree_size": checkpoint.size,
                                "root": hex(&checkpoint.root),
                                "checkpoint": note,
                                "entries": entries[range.from as usize..range.to as usize],
                            }))
                        }
                    }
                }),
            )
            .route(
                "/v1/log/consistency",
                get({
                    let checkpoint = checkpoint.clone();
                    let consistency_path = Arc::clone(&consistency_path);
                    move || {
                        let checkpoint = checkpoint.clone();
                        let consistency_path = Arc::clone(&consistency_path);
                        async move {
                            Json(json!({
                                "from": 1,
                                "to": checkpoint.size,
                                "root": hex(&checkpoint.root),
                                "path": *consistency_path,
                            }))
                        }
                    }
                }),
            )
            .route(
                "/v1/log/dump",
                get({
                    let entries = Arc::clone(&entries);
                    let dump_ranges = Arc::clone(&dump_ranges);
                    move |Query(range): Query<Range>| {
                        let entries = Arc::clone(&entries);
                        let dump_ranges = Arc::clone(&dump_ranges);
                        async move {
                            dump_ranges.lock().unwrap().push((range.from, range.to));
                            let mut body = Vec::new();
                            for entry in &entries[range.from as usize..range.to as usize] {
                                serde_json::to_writer(&mut body, entry).unwrap();
                                body.push(b'\n');
                            }
                            body
                        }
                    }
                }),
            );
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        let server = tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
        let trust = RegistryTrust::new(
            "registry.test/log",
            registry_key.verifying_key().to_bytes(),
            vec![WitnessKey::new("independent", witness.verifying_key()).unwrap()],
            1,
            CheckpointPin {
                size: 0,
                root: empty_root(),
            },
            60,
            5,
        )
        .unwrap();
        let client = RegistryClient::new(&format!("http://{address}"), trust).unwrap();

        let (keys, compliance_audit) = client
            .compliance_keys_audited(Some(Arc::new(previous_compliance)), None, 10_000)
            .await
            .unwrap();
        assert!(keys.keys().is_empty());
        assert_eq!(compliance_audit.checkpoint(), checkpoint);

        let mut projection = InMemoryHandleProjection::default();
        projection.bindings.insert(
            handle.as_path(),
            AuditedHandleBinding::new(
                &handle,
                alice_key.verifying_key().to_bytes(),
                "github:alice-subject".into(),
                0,
            )
            .unwrap(),
        );
        let (verified, handle_audit, projection) = client
            .resolve_audited(&handle, Some(&previous_handle), None, 10_000, projection)
            .await
            .unwrap();
        assert_eq!(verified.log_index(), 0);
        assert_eq!(handle_audit.checkpoint(), checkpoint);
        assert_eq!(projection.bindings.len(), entries.len());
        assert_eq!(page_calls.load(Ordering::SeqCst), 1);
        assert_eq!(
            *dump_ranges.lock().unwrap(),
            vec![
                (1, 1 + AUDIT_DUMP_SEGMENT_ENTRIES),
                (
                    1 + AUDIT_DUMP_SEGMENT_ENTRIES,
                    2 + AUDIT_DUMP_SEGMENT_ENTRIES,
                ),
                (1, 1 + AUDIT_DUMP_SEGMENT_ENTRIES),
                (
                    1 + AUDIT_DUMP_SEGMENT_ENTRIES,
                    2 + AUDIT_DUMP_SEGMENT_ENTRIES,
                ),
            ]
        );
        server.abort();
    }

    #[test]
    fn audit_rejects_a_key_that_was_never_published_active() {
        let key_id = ComplianceKeyId::new(
            CompliancePurpose::Attribution,
            Jurisdiction::Test,
            [41u8; 32],
            JANUARY_1970,
            1,
        );
        let retired = audited_compliance_key(
            &ComplianceKeyPublish {
                key_id,
                public_key: "42".repeat(32),
                not_before_ms: JANUARY_1970,
                not_after_ms: FEBRUARY_1970,
                status: ComplianceKeyStatus::Retired,
            },
            0,
        )
        .unwrap();
        assert!(validate_compliance_transition(None, &retired).is_err());
    }

    fn static_registry(resolve: Value, entries: Value) -> Router {
        let resolve = Arc::new(resolve);
        let entries = Arc::new(entries);
        Router::new()
            .route(
                "/v1/resolve/github/alice",
                get({
                    let resolve = Arc::clone(&resolve);
                    move || {
                        let resolve = Arc::clone(&resolve);
                        async move { Json((*resolve).clone()) }
                    }
                }),
            )
            .route(
                "/v1/log/entries",
                get(move || {
                    let entries = Arc::clone(&entries);
                    async move { Json((*entries).clone()) }
                }),
            )
    }

    fn static_compliance_registry(projection: Value, entries: Value) -> Router {
        let projection = Arc::new(projection);
        let entries = Arc::new(entries);
        Router::new()
            .route(
                "/v1/compliance-keys",
                get({
                    let projection = Arc::clone(&projection);
                    move || {
                        let projection = Arc::clone(&projection);
                        async move { Json((*projection).clone()) }
                    }
                }),
            )
            .route(
                "/v1/log/entries",
                get(move || {
                    let entries = Arc::clone(&entries);
                    async move { Json((*entries).clone()) }
                }),
            )
    }
}
