//! Strict C2SP `tlog-witness` client used by the registry operator.
//!
//! Witnesses are independently operated services. This module contains only the client-side
//! protocol and durable receipt shape; it never creates witness keys and never treats a registry
//! operator signature as an independent cosignature.

use std::collections::HashSet;
use std::fmt;
use std::future::Future;
use std::sync::Arc;
use std::time::{Duration, Instant};

use ed25519_dalek::VerifyingKey;
use pigeonpost_core::network::is_localhost_name;
use reqwest::header::{ACCEPT, CONTENT_TYPE};
use reqwest::{Client, StatusCode, Url};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use thiserror::Error;
use tokio::sync::{watch, Mutex, Semaphore};
use tokio::task::JoinSet;
use url::Host;

use crate::checkpoint::{witness_quorum_intersects, Checkpoint, VerifiedCheckpoint, WitnessKey};
use crate::log::{hash_eq, verify_consistency, Hash};
use crate::registry::{Registry, WitnessPublicationStatus};

const MAX_URL_BYTES: usize = 2_048;
const MAX_CHECKPOINT_BYTES: usize = 64 * 1024;
const MAX_RESPONSE_BYTES: usize = 64 * 1024;
const MAX_REQUEST_BYTES: usize = 72 * 1024;
const MAX_CONSISTENCY_HASHES: usize = 63;
const MAX_SIGNATURE_LINES: usize = 64;
const MAX_SIGNATURE_LINE_BYTES: usize = 512;
const MAX_CONFLICT_BODY_BYTES: usize = 32;
const RECEIPT_VERSION: u8 = 1;

/// Stable witness failures. Display strings deliberately contain no endpoint, address, or raw
/// transport cause so they are safe for ordinary service logs.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Error)]
pub enum WitnessError {
    #[error("invalid witness configuration")]
    InvalidConfiguration,
    #[error("invalid operator checkpoint")]
    InvalidCheckpoint,
    #[error("invalid witness consistency proof")]
    InvalidConsistencyProof,
    #[error("witness request exceeds the protocol limit")]
    RequestTooLarge,
    #[error("witness response exceeds the protocol limit")]
    ResponseTooLarge,
    #[error("witness transport unavailable")]
    TransportUnavailable,
    #[error("witness rejected the checkpoint")]
    Rejected,
    #[error("witness returned an invalid cosignature")]
    InvalidCosignature,
    #[error("witness checkpoint rolled back")]
    Rollback,
    #[error("witness checkpoint equivocated")]
    Equivocation,
    #[error("witness is ahead of the submitted checkpoint")]
    WitnessAhead,
    #[error("witness conflict could not be recovered")]
    RecoveryUnavailable,
    #[error("witness retry deadline expired")]
    DeadlineExceeded,
    #[error("registry consistency proof is unavailable")]
    ProofUnavailable,
    #[error("witness publication is no longer ready")]
    PublicationUnavailable,
}

pub type WitnessResult<T> = std::result::Result<T, WitnessError>;

/// Independently provisioned witness identity and service prefixes.
#[derive(Clone)]
pub struct WitnessConfig {
    witness: WitnessKey,
    add_checkpoint_url: Url,
    checkpoint_url: Url,
}

impl fmt::Debug for WitnessConfig {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("WitnessConfig")
            .field("name", &self.witness.name())
            .field("submission_prefix", &"<withheld>")
            .field("monitoring_prefix", &"<withheld>")
            .finish()
    }
}

impl WitnessConfig {
    pub fn new(
        name: impl Into<String>,
        key: VerifyingKey,
        submission_prefix: &str,
        monitoring_prefix: &str,
        log_origin: &str,
    ) -> WitnessResult<Self> {
        let witness = WitnessKey::new(name, key).map_err(|_| WitnessError::InvalidConfiguration)?;
        if witness.name() == log_origin || !valid_origin(log_origin) {
            return Err(WitnessError::InvalidConfiguration);
        }
        let submission_prefix = validated_prefix(submission_prefix)?;
        let monitoring_prefix = validated_prefix(monitoring_prefix)?;
        let add_checkpoint_url = submission_prefix
            .join("add-checkpoint")
            .map_err(|_| WitnessError::InvalidConfiguration)?;
        let origin_hash = lower_hex(&Sha256::digest(log_origin.as_bytes()));
        let checkpoint_url = monitoring_prefix
            .join(&format!("{origin_hash}/checkpoint"))
            .map_err(|_| WitnessError::InvalidConfiguration)?;
        if add_checkpoint_url.as_str().len() > MAX_URL_BYTES
            || checkpoint_url.as_str().len() > MAX_URL_BYTES
        {
            return Err(WitnessError::InvalidConfiguration);
        }
        Ok(Self {
            witness,
            add_checkpoint_url,
            checkpoint_url,
        })
    }

    pub fn name(&self) -> &str {
        self.witness.name()
    }

    pub const fn key(&self) -> &VerifyingKey {
        self.witness.key()
    }

    pub fn witness_key(&self) -> WitnessKey {
        self.witness.clone()
    }
}

/// Network deadlines, signature freshness, and bounded retry policy.
#[derive(Debug, Clone, Copy)]
pub struct WitnessTiming {
    pub connect_timeout: Duration,
    pub request_timeout: Duration,
    pub max_cosignature_age: Duration,
    pub future_clock_skew: Duration,
    pub retry_initial: Duration,
    pub retry_max: Duration,
    pub retry_deadline: Duration,
}

impl Default for WitnessTiming {
    fn default() -> Self {
        Self {
            connect_timeout: Duration::from_secs(5),
            request_timeout: Duration::from_secs(15),
            max_cosignature_age: Duration::from_secs(10 * 60),
            future_clock_skew: Duration::from_secs(30),
            retry_initial: Duration::from_millis(250),
            retry_max: Duration::from_secs(15),
            retry_deadline: Duration::from_secs(60),
        }
    }
}

impl WitnessTiming {
    fn validate(self) -> WitnessResult<Self> {
        if self.connect_timeout.is_zero()
            || self.connect_timeout > Duration::from_secs(30)
            || self.request_timeout < self.connect_timeout
            || self.request_timeout > Duration::from_secs(60)
            || self.max_cosignature_age.is_zero()
            || self.max_cosignature_age > Duration::from_secs(24 * 60 * 60)
            || self.future_clock_skew > self.max_cosignature_age
            || self.retry_initial.is_zero()
            || self.retry_initial > self.retry_max
            || self.retry_max > Duration::from_secs(30)
            || self.retry_deadline < self.request_timeout
            || self.retry_deadline > Duration::from_secs(5 * 60)
        {
            return Err(WitnessError::InvalidConfiguration);
        }
        Ok(self)
    }
}

/// Quorum and freshness policy enforced by the registry's public checkpoint surface.
///
/// Construction requires a strict-majority threshold. This guarantees set intersection for the
/// configured roster but cannot establish that an intersection witness will not equivocate.
#[derive(Debug, Clone)]
pub struct WitnessPolicy {
    witnesses: Vec<WitnessKey>,
    threshold: usize,
    max_cosignature_age_secs: u64,
    future_clock_skew_secs: u64,
    max_lag_entries: u64,
}

impl WitnessPolicy {
    pub fn new(
        witnesses: Vec<WitnessKey>,
        threshold: usize,
        max_cosignature_age_secs: u64,
        future_clock_skew_secs: u64,
        max_lag_entries: u64,
    ) -> WitnessResult<Self> {
        if witnesses.is_empty()
            || witnesses.len() > 64
            || !witness_quorum_intersects(threshold, witnesses.len())
            || max_cosignature_age_secs == 0
            || max_cosignature_age_secs > 24 * 60 * 60
            || future_clock_skew_secs > max_cosignature_age_secs
        {
            return Err(WitnessError::InvalidConfiguration);
        }
        for (index, witness) in witnesses.iter().enumerate() {
            if witnesses[..index].iter().any(|prior| {
                prior.name() == witness.name() || prior.key().as_bytes() == witness.key().as_bytes()
            }) {
                return Err(WitnessError::InvalidConfiguration);
            }
        }
        Ok(Self {
            witnesses,
            threshold,
            max_cosignature_age_secs,
            future_clock_skew_secs,
            max_lag_entries,
        })
    }

    pub fn witnesses(&self) -> &[WitnessKey] {
        &self.witnesses
    }

    pub const fn threshold(&self) -> usize {
        self.threshold
    }

    pub const fn max_cosignature_age_secs(&self) -> u64 {
        self.max_cosignature_age_secs
    }

    pub const fn future_clock_skew_secs(&self) -> u64 {
        self.future_clock_skew_secs
    }

    pub const fn max_lag_entries(&self) -> u64 {
        self.max_lag_entries
    }

    pub fn verify_checkpoint(
        &self,
        note: &str,
        operator_key: &VerifyingKey,
        now_secs: u64,
    ) -> WitnessResult<VerifiedCheckpoint> {
        Checkpoint::verify_with_fresh_witnesses(
            note,
            operator_key,
            &self.witnesses,
            self.threshold,
            now_secs,
            self.max_cosignature_age_secs,
            self.future_clock_skew_secs,
        )
        .map_err(|_| WitnessError::InvalidCosignature)
    }

    /// Assemble independently verified per-witness receipts into one quorum note.
    pub fn assemble_checkpoint(
        &self,
        operator_note: &str,
        operator_key: &VerifyingKey,
        receipts: &[WitnessReceipt],
        now_secs: u64,
    ) -> WitnessResult<(VerifiedCheckpoint, String)> {
        if receipts.len() > self.witnesses.len() || operator_note.len() > MAX_CHECKPOINT_BYTES {
            return Err(WitnessError::InvalidCosignature);
        }
        let target = Checkpoint::verify(operator_note, operator_key)
            .map_err(|_| WitnessError::InvalidCheckpoint)?;
        let mut note = operator_note.to_owned();
        let mut names = HashSet::new();
        for receipt in receipts {
            let Some(witness) = self
                .witnesses
                .iter()
                .find(|witness| witness.name() == receipt.witness_name())
            else {
                continue;
            };
            if !names.insert(witness.name())
                || receipt.version != RECEIPT_VERSION
                || receipt.origin != target.origin
                || receipt.size != target.size
                || !hash_eq(&receipt.root, &target.root)
            {
                return Err(WitnessError::InvalidCosignature);
            }
            let verified = Checkpoint::verify_with_fresh_witnesses(
                &receipt.note,
                operator_key,
                &[witness.clone()],
                1,
                now_secs,
                self.max_cosignature_age_secs,
                self.future_clock_skew_secs,
            )
            .map_err(|_| WitnessError::InvalidCosignature)?;
            if verified.witnessed_at != Some(receipt.witnessed_at) {
                return Err(WitnessError::InvalidCosignature);
            }
            let (_, signatures) = receipt
                .note
                .split_once("\n\n")
                .ok_or(WitnessError::InvalidCosignature)?;
            let prefix = format!("— {} ", witness.name());
            let line = signatures
                .lines()
                .find(|line| line.starts_with(&prefix))
                .ok_or(WitnessError::InvalidCosignature)?;
            note = append_signature(&note, line)?;
        }
        let verified = self.verify_checkpoint(&note, operator_key, now_secs)?;
        Ok((verified, note))
    }
}

/// One independently verified, persistable witness receipt.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct WitnessReceipt {
    version: u8,
    witness_name: String,
    origin: String,
    size: u64,
    root: Hash,
    note: String,
    witnessed_at: u64,
}

impl WitnessReceipt {
    pub fn witness_name(&self) -> &str {
        &self.witness_name
    }

    pub fn origin(&self) -> &str {
        &self.origin
    }

    pub const fn size(&self) -> u64 {
        self.size
    }

    pub const fn root(&self) -> &Hash {
        &self.root
    }

    pub fn note(&self) -> &str {
        &self.note
    }

    pub const fn witnessed_at(&self) -> u64 {
        self.witnessed_at
    }
}

/// A no-proxy, no-redirect C2SP client for one pinned witness.
#[derive(Clone)]
pub struct WitnessClient {
    config: WitnessConfig,
    origin: String,
    operator_key: VerifyingKey,
    timing: WitnessTiming,
    http: Client,
    gate: Arc<Mutex<()>>,
}

impl fmt::Debug for WitnessClient {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("WitnessClient")
            .field("name", &self.config.name())
            .field("origin", &self.origin)
            .field("endpoints", &"<withheld>")
            .field("timing", &self.timing)
            .finish()
    }
}

impl WitnessClient {
    pub fn new(
        config: WitnessConfig,
        origin: impl Into<String>,
        operator_key: VerifyingKey,
        timing: WitnessTiming,
    ) -> WitnessResult<Self> {
        let origin = origin.into();
        let timing = timing.validate()?;
        if !valid_origin(&origin) || origin == config.name() {
            return Err(WitnessError::InvalidConfiguration);
        }
        let http = Client::builder()
            .redirect(reqwest::redirect::Policy::none())
            .no_proxy()
            .connect_timeout(timing.connect_timeout)
            .timeout(timing.request_timeout)
            .user_agent(concat!("pigeonpost-registry/", env!("CARGO_PKG_VERSION")))
            .build()
            .map_err(|_| WitnessError::InvalidConfiguration)?;
        Ok(Self {
            config,
            origin,
            operator_key,
            timing,
            http,
            gate: Arc::new(Mutex::new(())),
        })
    }

    pub fn name(&self) -> &str {
        self.config.name()
    }

    pub fn witness_key(&self) -> WitnessKey {
        self.config.witness_key()
    }

    /// Submit one exact operator-signed checkpoint and recover a crash-after-submit conflict through
    /// the witness monitoring endpoint. `proof_for` must return the local log's RFC 6962 proof for
    /// the requested sizes and is re-checked before any request is sent.
    pub async fn cosign_with<F>(
        &self,
        operator_note: &str,
        previous: Option<&WitnessReceipt>,
        now_secs: u64,
        mut proof_for: F,
    ) -> WitnessResult<WitnessReceipt>
    where
        F: FnMut(u64, u64) -> WitnessResult<Vec<Hash>>,
    {
        let _guard = self.gate.lock().await;
        let mut proof_for = move |old, new| std::future::ready(proof_for(old, new));
        self.cosign_attempt(operator_note, previous, now_secs, &mut proof_for)
            .await
    }

    /// Retry transient witness failures with exponential backoff inside one bounded deadline.
    pub async fn cosign_with_retry<F>(
        &self,
        operator_note: &str,
        previous: Option<&WitnessReceipt>,
        now_secs: u64,
        mut proof_for: F,
    ) -> WitnessResult<WitnessReceipt>
    where
        F: FnMut(u64, u64) -> WitnessResult<Vec<Hash>>,
    {
        let _guard = self.gate.lock().await;
        let mut proof_for = move |old, new| std::future::ready(proof_for(old, new));
        self.cosign_retry(operator_note, previous, now_secs, &mut proof_for)
            .await
    }

    pub(crate) async fn cosign_with_retry_async<F, Fut>(
        &self,
        operator_note: &str,
        previous: Option<&WitnessReceipt>,
        now_secs: u64,
        mut proof_for: F,
    ) -> WitnessResult<WitnessReceipt>
    where
        F: FnMut(u64, u64) -> Fut,
        Fut: Future<Output = WitnessResult<Vec<Hash>>>,
    {
        let _guard = self.gate.lock().await;
        self.cosign_retry(operator_note, previous, now_secs, &mut proof_for)
            .await
    }

    async fn cosign_retry<F, Fut>(
        &self,
        operator_note: &str,
        previous: Option<&WitnessReceipt>,
        now_secs: u64,
        proof_for: &mut F,
    ) -> WitnessResult<WitnessReceipt>
    where
        F: FnMut(u64, u64) -> Fut,
        Fut: Future<Output = WitnessResult<Vec<Hash>>>,
    {
        let started = Instant::now();
        let mut delay = self.timing.retry_initial;
        loop {
            let remaining = self.timing.retry_deadline.saturating_sub(started.elapsed());
            if remaining.is_zero() {
                return Err(WitnessError::DeadlineExceeded);
            }
            let attempt_now = now_secs.saturating_add(started.elapsed().as_secs());
            let attempt = tokio::time::timeout(
                remaining,
                self.cosign_attempt(operator_note, previous, attempt_now, proof_for),
            )
            .await
            .map_err(|_| WitnessError::DeadlineExceeded)?;
            match attempt {
                Ok(receipt) => return Ok(receipt),
                Err(error) if !retryable(error) => return Err(error),
                Err(_) => {}
            }
            let remaining = self.timing.retry_deadline.saturating_sub(started.elapsed());
            if remaining <= delay {
                return Err(WitnessError::DeadlineExceeded);
            }
            tokio::time::sleep(delay).await;
            delay = delay.saturating_mul(2).min(self.timing.retry_max);
        }
    }

    async fn cosign_attempt<F, Fut>(
        &self,
        operator_note: &str,
        previous: Option<&WitnessReceipt>,
        now_secs: u64,
        proof_for: &mut F,
    ) -> WitnessResult<WitnessReceipt>
    where
        F: FnMut(u64, u64) -> Fut,
        Fut: Future<Output = WitnessResult<Vec<Hash>>>,
    {
        let target = self.verify_operator_note(operator_note)?;
        let previous_checkpoint = match previous {
            Some(receipt) => Some(self.verify_stored_receipt(receipt)?),
            None => None,
        };
        if let Some(old) = &previous_checkpoint {
            ensure_order(old, &target)?;
        }
        let old_size = previous_checkpoint.as_ref().map_or(0, |old| old.size);
        let proof = checked_proof(previous_checkpoint.as_ref(), &target, proof_for).await?;
        match self.submit(old_size, &proof, operator_note).await? {
            SubmitResponse::Cosignatures(lines) => {
                self.receipt_from_lines(operator_note, &target, &lines, now_secs)
            }
            SubmitResponse::Conflict(expected_size) => {
                self.recover_conflict(
                    expected_size,
                    operator_note,
                    &target,
                    previous_checkpoint.as_ref(),
                    now_secs,
                    proof_for,
                )
                .await
            }
        }
    }

    #[allow(clippy::too_many_arguments)]
    async fn recover_conflict<F, Fut>(
        &self,
        expected_size: u64,
        operator_note: &str,
        target: &Checkpoint,
        previous: Option<&Checkpoint>,
        now_secs: u64,
        proof_for: &mut F,
    ) -> WitnessResult<WitnessReceipt>
    where
        F: FnMut(u64, u64) -> Fut,
        Fut: Future<Output = WitnessResult<Vec<Hash>>>,
    {
        if expected_size > target.size {
            return Err(WitnessError::WitnessAhead);
        }
        if expected_size == target.size {
            // This is the normal crash-after-submit case. Retrying at the witness-reported size
            // works with the stable v1 protocol even when the optional monitoring endpoint is
            // delayed or unavailable. A rejection falls through to monitoring so an equivocated
            // same-size checkpoint can still be diagnosed precisely.
            match self.submit(expected_size, &[], operator_note).await {
                Ok(SubmitResponse::Cosignatures(lines)) => {
                    return self.receipt_from_lines(operator_note, target, &lines, now_secs);
                }
                Ok(SubmitResponse::Conflict(_)) | Err(WitnessError::Rejected) => {}
                Err(error) => return Err(error),
            }
        }
        let monitored_note = self.fetch_monitored_checkpoint().await?;
        let monitored = self.receipt_from_note(&monitored_note, now_secs)?;
        if monitored.size != expected_size {
            return Err(WitnessError::RecoveryUnavailable);
        }
        let monitored_checkpoint = Checkpoint {
            origin: monitored.origin.clone(),
            size: monitored.size,
            root: monitored.root,
        };
        if monitored_checkpoint.size > target.size {
            return Err(WitnessError::WitnessAhead);
        }
        if let Some(previous) = previous {
            ensure_order(previous, &monitored_checkpoint)?;
            if monitored.size > previous.size {
                let proof = proof_for(previous.size, monitored.size)
                    .await
                    .map_err(|_| WitnessError::ProofUnavailable)?;
                if proof.len() > MAX_CONSISTENCY_HASHES
                    || !verify_consistency(
                        previous.size,
                        &previous.root,
                        monitored.size,
                        &monitored.root,
                        &proof,
                    )
                {
                    return Err(WitnessError::Equivocation);
                }
            }
        }
        ensure_order(&monitored_checkpoint, target)?;
        if monitored.size == target.size {
            return Ok(monitored);
        }

        let proof = checked_proof(Some(&monitored_checkpoint), target, proof_for).await?;
        match self.submit(monitored.size, &proof, operator_note).await? {
            SubmitResponse::Cosignatures(lines) => {
                self.receipt_from_lines(operator_note, target, &lines, now_secs)
            }
            SubmitResponse::Conflict(_) => Err(WitnessError::RecoveryUnavailable),
        }
    }

    fn verify_operator_note(&self, note: &str) -> WitnessResult<Checkpoint> {
        if note.is_empty()
            || note.len() > MAX_CHECKPOINT_BYTES
            || !note.ends_with('\n')
            || !only_named_signatures(note, &self.origin)?
        {
            return Err(WitnessError::InvalidCheckpoint);
        }
        let checkpoint = Checkpoint::verify(note, &self.operator_key)
            .map_err(|_| WitnessError::InvalidCheckpoint)?;
        if checkpoint.origin != self.origin {
            return Err(WitnessError::InvalidCheckpoint);
        }
        Ok(checkpoint)
    }

    fn verify_stored_receipt(&self, receipt: &WitnessReceipt) -> WitnessResult<Checkpoint> {
        if receipt.version != RECEIPT_VERSION
            || receipt.witness_name != self.config.name()
            || receipt.origin != self.origin
            || receipt.witnessed_at == 0
            || receipt.note.len() > MAX_CHECKPOINT_BYTES
        {
            return Err(WitnessError::InvalidCosignature);
        }
        let verified = Checkpoint::verify_with_fresh_witnesses(
            &receipt.note,
            &self.operator_key,
            &[self.config.witness_key()],
            1,
            i64::MAX as u64,
            i64::MAX as u64,
            0,
        )
        .map_err(|_| WitnessError::InvalidCosignature)?;
        if verified.witnessed_at != Some(receipt.witnessed_at)
            || verified.checkpoint.origin != receipt.origin
            || verified.checkpoint.size != receipt.size
            || !hash_eq(&verified.checkpoint.root, &receipt.root)
        {
            return Err(WitnessError::InvalidCosignature);
        }
        Ok(verified.checkpoint)
    }

    fn receipt_from_note(&self, note: &str, now_secs: u64) -> WitnessResult<WitnessReceipt> {
        if note.is_empty() || note.len() > MAX_RESPONSE_BYTES || !note.ends_with('\n') {
            return Err(WitnessError::InvalidCosignature);
        }
        let operator_note = operator_only_note(note, &self.origin)?;
        let checkpoint = self.verify_operator_note(&operator_note)?;
        let (_, signatures) = note
            .split_once("\n\n")
            .ok_or(WitnessError::InvalidCosignature)?;
        self.receipt_from_lines(&operator_note, &checkpoint, signatures, now_secs)
    }

    fn receipt_from_lines(
        &self,
        operator_note: &str,
        checkpoint: &Checkpoint,
        lines: &str,
        now_secs: u64,
    ) -> WitnessResult<WitnessReceipt> {
        if lines.len() > MAX_RESPONSE_BYTES {
            return Err(WitnessError::ResponseTooLarge);
        }
        if lines.is_empty() || !lines.ends_with('\n') {
            return Err(WitnessError::InvalidCosignature);
        }
        let prefix = format!("— {} ", self.config.name());
        let mut accepted: Option<(u64, String)> = None;
        let mut count = 0usize;
        for line in lines.lines() {
            count += 1;
            if count > MAX_SIGNATURE_LINES || line.len() > MAX_SIGNATURE_LINE_BYTES {
                return Err(WitnessError::InvalidCosignature);
            }
            if !line.starts_with(&prefix) {
                continue;
            }
            let candidate_note = append_signature(operator_note, line)?;
            let Ok(verified) = Checkpoint::verify_with_fresh_witnesses(
                &candidate_note,
                &self.operator_key,
                &[self.config.witness_key()],
                1,
                now_secs,
                self.timing.max_cosignature_age.as_secs(),
                self.timing.future_clock_skew.as_secs(),
            ) else {
                continue;
            };
            let Some(timestamp) = verified.witnessed_at else {
                continue;
            };
            if timestamp == 0
                || verified.checkpoint.origin != checkpoint.origin
                || verified.checkpoint.size != checkpoint.size
                || !hash_eq(&verified.checkpoint.root, &checkpoint.root)
            {
                continue;
            }
            if accepted
                .as_ref()
                .is_none_or(|(accepted_at, _)| timestamp > *accepted_at)
            {
                accepted = Some((timestamp, candidate_note));
            }
        }
        let (witnessed_at, note) = accepted.ok_or(WitnessError::InvalidCosignature)?;
        Ok(WitnessReceipt {
            version: RECEIPT_VERSION,
            witness_name: self.config.name().to_owned(),
            origin: checkpoint.origin.clone(),
            size: checkpoint.size,
            root: checkpoint.root,
            note,
            witnessed_at,
        })
    }

    async fn submit(
        &self,
        old_size: u64,
        proof: &[Hash],
        operator_note: &str,
    ) -> WitnessResult<SubmitResponse> {
        let body = request_body(old_size, proof, operator_note)?;
        let response = tokio::time::timeout(
            self.timing.request_timeout,
            self.http
                .post(self.config.add_checkpoint_url.clone())
                .header(CONTENT_TYPE, "text/plain; charset=utf-8")
                .header(ACCEPT, "text/plain")
                .body(body)
                .send(),
        )
        .await
        .map_err(|_| WitnessError::TransportUnavailable)?
        .map_err(|_| WitnessError::TransportUnavailable)?;
        match response.status() {
            StatusCode::OK => {
                let body = read_limited(response, MAX_RESPONSE_BYTES).await?;
                let text = String::from_utf8(body).map_err(|_| WitnessError::InvalidCosignature)?;
                Ok(SubmitResponse::Cosignatures(text))
            }
            StatusCode::CONFLICT => {
                if !response
                    .headers()
                    .get(CONTENT_TYPE)
                    .and_then(|value| value.to_str().ok())
                    .is_some_and(|value| value.eq_ignore_ascii_case("text/x.tlog.size"))
                {
                    return Err(WitnessError::RecoveryUnavailable);
                }
                let body = read_limited(response, MAX_CONFLICT_BODY_BYTES).await?;
                let size = parse_size_line(&body).ok_or(WitnessError::RecoveryUnavailable)?;
                Ok(SubmitResponse::Conflict(size))
            }
            status if status.is_server_error() || status == StatusCode::TOO_MANY_REQUESTS => {
                Err(WitnessError::TransportUnavailable)
            }
            _ => Err(WitnessError::Rejected),
        }
    }

    async fn fetch_monitored_checkpoint(&self) -> WitnessResult<String> {
        let response = tokio::time::timeout(
            self.timing.request_timeout,
            self.http
                .get(self.config.checkpoint_url.clone())
                .header(ACCEPT, "text/plain")
                .send(),
        )
        .await
        .map_err(|_| WitnessError::TransportUnavailable)?
        .map_err(|_| WitnessError::TransportUnavailable)?;
        if response.status() == StatusCode::NOT_FOUND {
            return Err(WitnessError::RecoveryUnavailable);
        }
        if response.status().is_server_error() || response.status() == StatusCode::TOO_MANY_REQUESTS
        {
            return Err(WitnessError::TransportUnavailable);
        }
        if response.status() != StatusCode::OK {
            return Err(WitnessError::Rejected);
        }
        let body = read_limited(response, MAX_RESPONSE_BYTES).await?;
        String::from_utf8(body).map_err(|_| WitnessError::InvalidCosignature)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct WitnessSyncReport {
    pub attempted: usize,
    pub succeeded: usize,
    pub failed: usize,
    pub promoted: bool,
    pub publication: WitnessPublicationStatus,
}

#[derive(Clone)]
struct WitnessRegistryLane {
    registry: Arc<Registry>,
    permit: Arc<Semaphore>,
}

impl WitnessRegistryLane {
    fn new(registry: Arc<Registry>) -> Self {
        Self {
            registry,
            // Registry owns one SQLite connection. A single worker avoids consuming blocking
            // threads that would only wait on its mutex while still keeping every phase off Tokio.
            permit: Arc::new(Semaphore::new(1)),
        }
    }

    async fn run<T, F>(&self, operation: F) -> crate::Result<T>
    where
        T: Send + 'static,
        F: FnOnce(&Registry) -> crate::Result<T> + Send + 'static,
    {
        let permit = Arc::clone(&self.permit)
            .acquire_owned()
            .await
            .map_err(|_| crate::RegistryError::RegistryUnavailable)?;
        let registry = Arc::clone(&self.registry);
        tokio::task::spawn_blocking(move || {
            let _permit = permit;
            operation(&registry)
        })
        .await
        .map_err(|_| crate::RegistryError::RegistryUnavailable)?
    }
}

/// Background coordinator for all independently configured witnesses.
///
/// Each witness is contacted concurrently, but requests to the same witness remain serialized by
/// `WitnessClient`. Successful receipts are durable before quorum publication. A process crash at
/// any point is recovered from the per-witness receipt or the C2SP monitoring endpoint.
pub struct WitnessSupervisor {
    storage: WitnessRegistryLane,
    sync_gate: Mutex<()>,
    clients: Vec<WitnessClient>,
    poll_interval: Duration,
    failure_backoff_initial: Duration,
    failure_backoff_max: Duration,
}

impl fmt::Debug for WitnessSupervisor {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("WitnessSupervisor")
            .field("witness_count", &self.clients.len())
            .field("poll_interval", &self.poll_interval)
            .field("failure_backoff_initial", &self.failure_backoff_initial)
            .field("failure_backoff_max", &self.failure_backoff_max)
            .finish()
    }
}

impl WitnessSupervisor {
    pub fn new(
        registry: Arc<Registry>,
        clients: Vec<WitnessClient>,
        poll_interval: Duration,
        failure_backoff_initial: Duration,
        failure_backoff_max: Duration,
    ) -> WitnessResult<Self> {
        let policy = registry
            .witness_policy()
            .ok_or(WitnessError::InvalidConfiguration)?;
        if clients.len() != policy.witnesses().len()
            || poll_interval.is_zero()
            || poll_interval > Duration::from_secs(5 * 60)
            || failure_backoff_initial.is_zero()
            || failure_backoff_initial > failure_backoff_max
            || failure_backoff_max > Duration::from_secs(30)
        {
            return Err(WitnessError::InvalidConfiguration);
        }
        for witness in policy.witnesses() {
            let matches = clients.iter().filter(|client| {
                client.name() == witness.name()
                    && client.witness_key().key().as_bytes() == witness.key().as_bytes()
            });
            if matches.count() != 1 {
                return Err(WitnessError::InvalidConfiguration);
            }
        }
        Ok(Self {
            storage: WitnessRegistryLane::new(registry),
            sync_gate: Mutex::new(()),
            clients,
            poll_interval,
            failure_backoff_initial,
            failure_backoff_max,
        })
    }

    pub async fn sync_once(&self, now_secs: u64) -> WitnessResult<WitnessSyncReport> {
        let _sync = self.sync_gate.lock().await;
        let head = self
            .storage
            .run(|registry| registry.committed_head())
            .await
            .map_err(|_| WitnessError::ProofUnavailable)?;
        let names = self
            .clients
            .iter()
            .map(|client| client.name().to_owned())
            .collect::<Vec<_>>();
        let previous = self
            .storage
            .run(move |registry| {
                names
                    .iter()
                    .map(|name| registry.witness_receipt(name))
                    .collect::<crate::Result<Vec<_>>>()
            })
            .await
            .map_err(|_| WitnessError::ProofUnavailable)?;
        let mut tasks = JoinSet::new();
        for (client, previous) in self.clients.iter().zip(previous) {
            let client = client.clone();
            let storage = self.storage.clone();
            let note = head.checkpoint.clone();
            tasks.spawn(async move {
                let receipt = client
                    .cosign_with_retry_async(&note, previous.as_ref(), now_secs, move |old, new| {
                        let storage = storage.clone();
                        async move {
                            storage
                                .run(move |registry| registry.consistency_proof_between(old, new))
                                .await
                                .map_err(|_| WitnessError::ProofUnavailable)
                        }
                    })
                    .await?;
                Ok::<_, WitnessError>(receipt)
            });
        }

        let attempted = self.clients.len();
        let mut receipts = Vec::new();
        let mut failed = 0usize;
        while let Some(result) = tasks.join_next().await {
            match result {
                Ok(Ok(receipt)) => receipts.push(receipt),
                Ok(Err(_)) | Err(_) => failed += 1,
            }
        }
        let (succeeded, save_failures) = self
            .storage
            .run(move |registry| {
                let mut succeeded = 0usize;
                let mut failed = 0usize;
                for receipt in receipts {
                    if registry.save_witness_receipt(&receipt, now_secs).is_ok() {
                        succeeded += 1;
                    } else {
                        failed += 1;
                    }
                }
                Ok((succeeded, failed))
            })
            .await
            .map_err(|_| WitnessError::ProofUnavailable)?;
        failed = failed.saturating_add(save_failures);
        let (promoted, publication) = self
            .storage
            .run(move |registry| {
                let promoted = match registry.promote_witnessed_head(now_secs) {
                    Ok(promoted) => promoted,
                    Err(crate::RegistryError::WitnessUnavailable) => false,
                    Err(error) => return Err(error),
                };
                Ok((promoted, registry.witness_publication_status()?))
            })
            .await
            .map_err(|_| WitnessError::ProofUnavailable)?;
        Ok(WitnessSyncReport {
            attempted,
            succeeded,
            failed,
            promoted,
            publication,
        })
    }

    /// Run until the watched shutdown flag becomes true or all senders are dropped.
    pub async fn run(&self, mut shutdown: watch::Receiver<bool>) -> WitnessResult<()> {
        let mut failure_delay = self.failure_backoff_initial;
        loop {
            if *shutdown.borrow() {
                return Ok(());
            }
            let now_secs = system_now_secs();
            let result = self.sync_once(now_secs).await;
            let readiness = self
                .storage
                .run(move |registry| Ok(registry.witness_readiness(now_secs).is_ok()))
                .await;
            let delay = match (result, readiness) {
                (Ok(report), Ok(ready))
                    if report.failed == 0 && report.succeeded == report.attempted =>
                {
                    failure_delay = self.failure_backoff_initial;
                    // The sync and readiness checks intentionally use separate storage
                    // snapshots. A mutation may commit between them, so a complete sync of
                    // the captured head can legitimately be unready for the newer head. Retry
                    // promptly; failed or partial syncs still fail closed while unready below.
                    if ready {
                        self.poll_interval
                    } else {
                        self.failure_backoff_initial
                    }
                }
                (Ok(_) | Err(_), Ok(true)) => {
                    let delay = failure_delay;
                    failure_delay = failure_delay
                        .saturating_mul(2)
                        .min(self.failure_backoff_max);
                    delay
                }
                // A failed readiness storage operation is not the benign split-snapshot race
                // above. Keep the original fail-closed process-supervision behavior for a dead
                // blocking lane or unreadable local registry state.
                (Ok(_) | Err(_), Ok(false) | Err(_)) => {
                    return Err(WitnessError::PublicationUnavailable);
                }
            };
            tokio::select! {
                _ = tokio::time::sleep(delay) => {}
                changed = shutdown.changed() => {
                    if changed.is_err() || *shutdown.borrow() {
                        return Ok(());
                    }
                }
            }
        }
    }
}

enum SubmitResponse {
    Cosignatures(String),
    Conflict(u64),
}

async fn checked_proof<F, Fut>(
    old: Option<&Checkpoint>,
    target: &Checkpoint,
    proof_for: &mut F,
) -> WitnessResult<Vec<Hash>>
where
    F: FnMut(u64, u64) -> Fut,
    Fut: Future<Output = WitnessResult<Vec<Hash>>>,
{
    let Some(old) = old else {
        return Ok(Vec::new());
    };
    ensure_order(old, target)?;
    // RFC 6962 defines consistency from the empty tree without a proof.  Keep
    // this check after `ensure_order` so an empty checkpoint cannot bypass the
    // origin and rollback invariants.
    if old.size == 0 || old.size == target.size {
        return Ok(Vec::new());
    }
    let proof = proof_for(old.size, target.size)
        .await
        .map_err(|_| WitnessError::ProofUnavailable)?;
    if proof.len() > MAX_CONSISTENCY_HASHES
        || !verify_consistency(old.size, &old.root, target.size, &target.root, &proof)
    {
        return Err(WitnessError::InvalidConsistencyProof);
    }
    Ok(proof)
}

fn ensure_order(old: &Checkpoint, new: &Checkpoint) -> WitnessResult<()> {
    if old.origin != new.origin || old.size > new.size {
        return Err(WitnessError::Rollback);
    }
    if old.size == new.size && !hash_eq(&old.root, &new.root) {
        return Err(WitnessError::Equivocation);
    }
    Ok(())
}

fn request_body(old_size: u64, proof: &[Hash], operator_note: &str) -> WitnessResult<Vec<u8>> {
    if proof.len() > MAX_CONSISTENCY_HASHES
        || (old_size == 0 && !proof.is_empty())
        || operator_note.is_empty()
        || operator_note.len() > MAX_CHECKPOINT_BYTES
        || !operator_note.ends_with('\n')
    {
        return Err(WitnessError::InvalidConsistencyProof);
    }
    let mut body = format!("old {old_size}\n");
    for hash in proof {
        body.push_str(&base64(hash));
        body.push('\n');
    }
    body.push('\n');
    body.push_str(operator_note);
    if body.len() > MAX_REQUEST_BYTES {
        return Err(WitnessError::RequestTooLarge);
    }
    Ok(body.into_bytes())
}

fn append_signature(operator_note: &str, line: &str) -> WitnessResult<String> {
    if !operator_note.ends_with('\n')
        || line.is_empty()
        || line.contains('\n')
        || line.contains('\r')
    {
        return Err(WitnessError::InvalidCosignature);
    }
    let size = operator_note
        .len()
        .checked_add(line.len())
        .and_then(|value| value.checked_add(1))
        .ok_or(WitnessError::ResponseTooLarge)?;
    if size > MAX_CHECKPOINT_BYTES {
        return Err(WitnessError::ResponseTooLarge);
    }
    let mut note = String::with_capacity(size);
    note.push_str(operator_note);
    note.push_str(line);
    note.push('\n');
    Ok(note)
}

fn operator_only_note(note: &str, operator_name: &str) -> WitnessResult<String> {
    let (body, signatures) = note
        .split_once("\n\n")
        .ok_or(WitnessError::InvalidCheckpoint)?;
    let prefix = format!("— {operator_name} ");
    let mut out = format!("{body}\n\n");
    let mut found = false;
    for line in signatures.lines() {
        if line.starts_with(&prefix) {
            if line.len() > MAX_SIGNATURE_LINE_BYTES {
                return Err(WitnessError::InvalidCheckpoint);
            }
            out.push_str(line);
            out.push('\n');
            found = true;
        }
    }
    found.then_some(out).ok_or(WitnessError::InvalidCheckpoint)
}

fn only_named_signatures(note: &str, expected_name: &str) -> WitnessResult<bool> {
    let (_, signatures) = note
        .split_once("\n\n")
        .ok_or(WitnessError::InvalidCheckpoint)?;
    let prefix = format!("— {expected_name} ");
    let mut count = 0usize;
    for line in signatures.lines() {
        count += 1;
        if count > MAX_SIGNATURE_LINES
            || line.len() > MAX_SIGNATURE_LINE_BYTES
            || !line.starts_with(&prefix)
        {
            return Ok(false);
        }
    }
    Ok(count > 0)
}

async fn read_limited(mut response: reqwest::Response, limit: usize) -> WitnessResult<Vec<u8>> {
    if response
        .content_length()
        .is_some_and(|length| length > limit as u64)
    {
        return Err(WitnessError::ResponseTooLarge);
    }
    let mut body = Vec::new();
    while let Some(chunk) = response
        .chunk()
        .await
        .map_err(|_| WitnessError::TransportUnavailable)?
    {
        if body.len().saturating_add(chunk.len()) > limit {
            return Err(WitnessError::ResponseTooLarge);
        }
        body.extend_from_slice(&chunk);
    }
    Ok(body)
}

fn parse_size_line(body: &[u8]) -> Option<u64> {
    let text = std::str::from_utf8(body).ok()?;
    let digits = text.strip_suffix('\n')?;
    if digits.is_empty()
        || (digits.len() > 1 && digits.starts_with('0'))
        || !digits.bytes().all(|byte| byte.is_ascii_digit())
    {
        return None;
    }
    digits.parse().ok()
}

fn validated_prefix(value: &str) -> WitnessResult<Url> {
    if value.is_empty() || value.len() > MAX_URL_BYTES {
        return Err(WitnessError::InvalidConfiguration);
    }
    let mut url = Url::parse(value).map_err(|_| WitnessError::InvalidConfiguration)?;
    if url.cannot_be_a_base()
        || url.host().is_none()
        || !url.username().is_empty()
        || url.password().is_some()
        || url.query().is_some()
        || url.fragment().is_some()
        || url.port() == Some(0)
        || url.host_str().is_some_and(is_localhost_name)
    {
        return Err(WitnessError::InvalidConfiguration);
    }
    match url.scheme() {
        "https" => {}
        "http" if literal_loopback(&url) => {}
        _ => return Err(WitnessError::InvalidConfiguration),
    }
    if !url.path().ends_with('/') {
        let mut path = url.path().to_owned();
        path.push('/');
        url.set_path(&path);
    }
    Ok(url)
}

fn literal_loopback(url: &Url) -> bool {
    match url.host() {
        Some(Host::Ipv4(address)) => address.is_loopback(),
        Some(Host::Ipv6(address)) => address.is_loopback(),
        _ => false,
    }
}

fn valid_origin(origin: &str) -> bool {
    !origin.is_empty() && origin.len() <= 256 && origin.bytes().all(|byte| byte.is_ascii_graphic())
}

fn retryable(error: WitnessError) -> bool {
    matches!(
        error,
        WitnessError::TransportUnavailable | WitnessError::RecoveryUnavailable
    )
}

fn system_now_secs() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|duration| duration.as_secs())
        .unwrap_or(0)
}

fn lower_hex(bytes: &[u8]) -> String {
    const HEX: &[u8; 16] = b"0123456789abcdef";
    let mut out = String::with_capacity(bytes.len() * 2);
    for byte in bytes {
        out.push(HEX[(byte >> 4) as usize] as char);
        out.push(HEX[(byte & 0x0f) as usize] as char);
    }
    out
}

fn base64(input: &[u8]) -> String {
    const ALPHABET: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
    let mut out = String::with_capacity(input.len().div_ceil(3) * 4);
    for chunk in input.chunks(3) {
        let bytes = [
            chunk[0],
            *chunk.get(1).unwrap_or(&0),
            *chunk.get(2).unwrap_or(&0),
        ];
        let value = (u32::from(bytes[0]) << 16) | (u32::from(bytes[1]) << 8) | u32::from(bytes[2]);
        out.push(ALPHABET[((value >> 18) & 63) as usize] as char);
        out.push(ALPHABET[((value >> 12) & 63) as usize] as char);
        out.push(if chunk.len() > 1 {
            ALPHABET[((value >> 6) & 63) as usize] as char
        } else {
            '='
        });
        out.push(if chunk.len() > 2 {
            ALPHABET[(value & 63) as usize] as char
        } else {
            '='
        });
    }
    out
}

#[cfg(test)]
mod tests {
    use ed25519_dalek::SigningKey;

    use super::*;

    fn witness(name: &str, seed: u8) -> WitnessKey {
        WitnessKey::new(name, SigningKey::from_bytes(&[seed; 32]).verifying_key()).unwrap()
    }

    #[tokio::test(flavor = "current_thread")]
    async fn registry_storage_lane_is_serial_and_keeps_the_executor_responsive() {
        let registry = Arc::new(
            Registry::in_memory(crate::RegistryConfig {
                origin: "pigeonpost.dev/witness-storage-lane-test".into(),
                signing_key: SigningKey::from_bytes(&[91; 32]),
                allow_mock_identities: false,
            })
            .unwrap(),
        );
        let lane = WitnessRegistryLane::new(registry);
        let (reached_tx, reached_rx) = tokio::sync::oneshot::channel();
        let (release_tx, release_rx) = std::sync::mpsc::channel();
        let occupied = tokio::spawn({
            let lane = lane.clone();
            async move {
                lane.run(move |_| {
                    let _ = reached_tx.send(());
                    let _ = release_rx.recv_timeout(Duration::from_secs(1));
                    Ok(())
                })
                .await
            }
        });
        tokio::time::timeout(Duration::from_secs(1), reached_rx)
            .await
            .expect("the witness storage phase must start")
            .unwrap();

        let second_started = Arc::new(std::sync::atomic::AtomicBool::new(false));
        let queued = tokio::spawn({
            let lane = lane.clone();
            let second_started = Arc::clone(&second_started);
            async move {
                lane.run(move |_| {
                    second_started.store(true, std::sync::atomic::Ordering::Release);
                    Ok(())
                })
                .await
            }
        });
        tokio::time::timeout(Duration::from_millis(100), tokio::task::yield_now())
            .await
            .expect("witness storage work must not occupy the current-thread executor");
        assert!(!second_started.load(std::sync::atomic::Ordering::Acquire));

        release_tx.send(()).unwrap();
        occupied.await.unwrap().unwrap();
        queued.await.unwrap().unwrap();
        assert!(second_started.load(std::sync::atomic::Ordering::Acquire));
    }

    #[test]
    fn policy_requires_a_strictly_intersecting_quorum() {
        assert!(WitnessPolicy::new(vec![witness("one", 1)], 1, 60, 5, 0).is_ok());
        assert!(
            WitnessPolicy::new(vec![witness("one", 1), witness("two", 2)], 1, 60, 5, 0,).is_err()
        );
        assert!(WitnessPolicy::new(
            vec![witness("one", 1), witness("two", 2), witness("three", 3),],
            1,
            60,
            5,
            0,
        )
        .is_err());
        assert!(WitnessPolicy::new(
            vec![witness("one", 1), witness("two", 2), witness("three", 3),],
            2,
            60,
            5,
            0,
        )
        .is_ok());
    }

    #[test]
    fn prefixes_require_https_or_a_literal_loopback() {
        assert!(validated_prefix("https://witness.example/submission").is_ok());
        assert!(validated_prefix("http://127.0.0.1:3000/submission").is_ok());
        assert!(validated_prefix("http://[::1]:3000/submission").is_ok());
        assert!(validated_prefix("http://localhost:3000/submission").is_err());
        assert!(validated_prefix("https://localhost:3000/submission").is_err());
        assert!(validated_prefix("https://localhost.:3000/submission").is_err());
        assert!(validated_prefix("https://api.localhost:3000/submission").is_err());
        assert!(validated_prefix("https://witness.example:0/submission").is_err());
        assert!(validated_prefix("http://witness.example/submission").is_err());
        assert!(validated_prefix("https://user@witness.example/submission").is_err());
        assert!(validated_prefix("https://witness.example/submission?q=1").is_err());
    }

    #[test]
    fn request_body_is_exact_and_bounded() {
        let note = "origin\n1\nAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA=\n\n— origin sig\n";
        let proof = [[7u8; 32]];
        let request = request_body(1, &proof, note).unwrap();
        assert_eq!(
            std::str::from_utf8(&request).unwrap(),
            format!("old 1\n{}\n\n{note}", base64(&proof[0]))
        );
        assert_eq!(
            request_body(0, &proof, note),
            Err(WitnessError::InvalidConsistencyProof)
        );
        assert_eq!(
            request_body(1, &vec![[0u8; 32]; 64], note),
            Err(WitnessError::InvalidConsistencyProof)
        );
    }

    #[test]
    fn size_lines_are_canonical() {
        assert_eq!(parse_size_line(b"0\n"), Some(0));
        assert_eq!(parse_size_line(b"42\n"), Some(42));
        assert_eq!(parse_size_line(b"042\n"), None);
        assert_eq!(parse_size_line(b"42"), None);
        assert_eq!(parse_size_line(b"-1\n"), None);
    }

    #[test]
    fn empty_tree_root_matches_protocol_anchor() {
        assert_eq!(crate::log::empty_root(), Sha256::digest([]).as_slice());
    }
}
