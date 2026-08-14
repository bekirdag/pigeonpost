//! Fail-closed publication of directory mutations into the shared registry transparency log.
//!
//! A successful POST receipt is not trusted by itself. The exact loft-authenticated leaf must be
//! included at a checkpoint carrying a fresh, nonzero witness quorum, and that checkpoint must be
//! consistent with both the operator-configured minimum and the directory's last durable pin.

use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use pigeonpost_core::network::{is_localhost_name, is_numeric_loopback_host};
use pigeonpost_registry::directory_publisher::{
    mutation_request_bytes, DirectoryMutationOperation, DIRECTORY_PUBLISHER_KEY_HEADER,
    DIRECTORY_PUBLISHER_SIGNATURE_HEADER, MAX_DIRECTORY_MUTATION_BODY_BYTES,
};
use pigeonpost_registry::entry::{DirectoryAdd, DirectoryRemove};
use pigeonpost_registry::log::{empty_root, leaf_hash, Hash};
use pigeonpost_registry::{
    verify_consistency, verify_inclusion, Checkpoint, CheckpointPin, LogEntry, RegistryError,
    RegistryTrust,
};
use reqwest::{Client, StatusCode, Url};
use serde::de::DeserializeOwned;
use serde::{Deserialize, Serialize};

use crate::directory::{Directory, PendingMutation, PersistedRegistryCheckpoint};
use crate::entry::hex;
use crate::error::{DirectoryError, Result};

const MAX_URL_BYTES: usize = 2_048;
const MAX_RESPONSE_BYTES: usize = 1024 * 1024;
const MAX_PROOF_ITEMS: usize = 64;
const CONNECT_TIMEOUT: Duration = Duration::from_secs(5);
const REQUEST_TIMEOUT: Duration = Duration::from_secs(10);
const DEFAULT_WITNESS_WAIT: Duration = Duration::from_secs(15);
const RETRY_INTERVAL: Duration = Duration::from_millis(200);
const MAX_MUTATION_RETRY_INTERVAL: Duration = Duration::from_secs(15);

/// A receipt the directory has independently authenticated, not merely the registry's claim.
#[derive(Debug, Clone, Serialize)]
pub struct DirectoryMutationReceipt {
    pub entry: LogEntry,
    pub log_index: u64,
    pub appended: bool,
    pub inclusion_proof: MutationInclusionProof,
}

#[derive(Debug, Clone, Serialize)]
pub struct MutationInclusionProof {
    pub tree_size: u64,
    pub root: String,
    pub path: Vec<String>,
    pub checkpoint: String,
    pub witnessed_at: u64,
}

impl DirectoryMutationReceipt {
    pub(crate) fn persisted_checkpoint(&self) -> Result<PersistedRegistryCheckpoint> {
        let root = parse_hex32(&self.inclusion_proof.root).ok_or_else(|| {
            DirectoryError::RegistryProof("verified checkpoint root became malformed".into())
        })?;
        Ok(PersistedRegistryCheckpoint {
            version: 1,
            origin: checkpoint_origin(&self.inclusion_proof.checkpoint)?,
            size: self.inclusion_proof.tree_size,
            root,
            note: self.inclusion_proof.checkpoint.clone(),
            witnessed_at: self.inclusion_proof.witnessed_at,
        })
    }
}

/// Hardened writer/verifier. Independent witness software is expected to cosign responses; until
/// it does, the operation returns unavailable and the local directory remains unchanged.
pub struct RegistryLogClient {
    base_url: Url,
    trust: RegistryTrust,
    http: Client,
    witness_wait: Duration,
}

impl core::fmt::Debug for RegistryLogClient {
    fn fmt(&self, formatter: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        formatter
            .debug_struct("RegistryLogClient")
            .field("base_url", &"<withheld>")
            .field("expected_origin", &self.trust.expected_origin())
            .field("witness_threshold", &self.trust.witness_threshold())
            .field("witness_wait", &self.witness_wait)
            .finish()
    }
}

impl RegistryLogClient {
    pub fn new(base_url: &str, trust: RegistryTrust) -> Result<Self> {
        Self::with_witness_wait(base_url, trust, DEFAULT_WITNESS_WAIT)
    }

    pub fn with_witness_wait(
        base_url: &str,
        trust: RegistryTrust,
        witness_wait: Duration,
    ) -> Result<Self> {
        if trust.witness_threshold() == 0
            || witness_wait.is_zero()
            || witness_wait > Duration::from_secs(5 * 60)
        {
            return Err(DirectoryError::Malformed(
                "directory registry logging requires a bounded nonzero witness policy".into(),
            ));
        }
        let base_url = validate_base_url(base_url)?;
        let http = Client::builder()
            .redirect(reqwest::redirect::Policy::none())
            .no_proxy()
            .connect_timeout(CONNECT_TIMEOUT)
            .timeout(REQUEST_TIMEOUT)
            .user_agent(concat!("pigeonpost-directory/", env!("CARGO_PKG_VERSION")))
            .build()
            .map_err(|_| DirectoryError::Unavailable)?;
        Ok(Self {
            base_url,
            trust,
            http,
            witness_wait,
        })
    }

    pub(crate) fn witness_wait(&self) -> Duration {
        self.witness_wait
    }

    pub(crate) async fn append_add(
        &self,
        publisher: &Directory,
        mutation: &DirectoryAdd,
        previous: Option<&PersistedRegistryCheckpoint>,
    ) -> Result<DirectoryMutationReceipt> {
        self.append(
            publisher,
            DirectoryMutationOperation::Add,
            "v1/directory/add",
            mutation,
            ExpectedMutation::Add(mutation),
            previous,
        )
        .await
    }

    pub(crate) async fn append_remove(
        &self,
        publisher: &Directory,
        mutation: &DirectoryRemove,
        previous: Option<&PersistedRegistryCheckpoint>,
    ) -> Result<DirectoryMutationReceipt> {
        self.append(
            publisher,
            DirectoryMutationOperation::Remove,
            "v1/directory/remove",
            mutation,
            ExpectedMutation::Remove(mutation),
            previous,
        )
        .await
    }

    /// Publish one already-admitted mutation without touching SQLite.
    ///
    /// The server owns reservation reads, checkpoint reads, and finalization so each of those
    /// potentially blocking operations runs through its bounded blocking executor. This helper
    /// deliberately performs only signing, bounded HTTP, and proof verification.
    pub(crate) async fn append_pending(
        &self,
        publisher: &Directory,
        pending: &PendingMutation,
        previous: Option<&PersistedRegistryCheckpoint>,
    ) -> Result<DirectoryMutationReceipt> {
        match pending {
            PendingMutation::Add { mutation, .. } => {
                self.append_add(publisher, mutation, previous).await
            }
            PendingMutation::Drain { mutation, .. } => {
                self.append_remove(publisher, mutation, previous).await
            }
        }
    }

    /// Prove that the registry is publishing without lag at a freshly witnessed checkpoint that
    /// is consistent with both configured trust and this directory's last durable pin.
    pub(crate) async fn readiness(
        &self,
        previous: Option<&PersistedRegistryCheckpoint>,
    ) -> Result<()> {
        let status: RegistryStatus = self.get_json("v1/log/status").await?;
        if !status.ready
            || status.lag_entries != 0
            || status.committed_size != status.published_size
            || status.witnessed_at.is_none()
        {
            return Err(DirectoryError::NotReady);
        }
        let note = self.get_bytes("v1/log/checkpoint").await?;
        let note = std::str::from_utf8(&note).map_err(|_| {
            DirectoryError::RegistryProof("registry checkpoint is malformed".into())
        })?;
        let verified = Checkpoint::verify_with_fresh_witnesses(
            note,
            self.trust.checkpoint_key(),
            self.trust.witnesses(),
            self.trust.witness_threshold(),
            now_secs(),
            self.trust.max_cosignature_age_secs(),
            self.trust.future_clock_skew_secs(),
        )
        .map_err(registry_proof)?;
        if verified.checkpoint.origin != self.trust.expected_origin()
            || verified.checkpoint.size < status.published_size
        {
            return Err(DirectoryError::NotReady);
        }
        for base in self.consistency_bases(previous)? {
            self.verify_growth(base, &verified.checkpoint).await?;
        }
        Ok(())
    }

    async fn append<T: Serialize + ?Sized>(
        &self,
        publisher: &Directory,
        operation: DirectoryMutationOperation,
        relative: &str,
        mutation: &T,
        expected: ExpectedMutation<'_>,
        previous: Option<&PersistedRegistryCheckpoint>,
    ) -> Result<DirectoryMutationReceipt> {
        let consistency_bases = self.consistency_bases(previous)?;
        let body = serde_json::to_vec(mutation)?;
        if body.len() > MAX_DIRECTORY_MUTATION_BODY_BYTES {
            return Err(DirectoryError::Malformed(
                "registry mutation request exceeds the client limit".into(),
            ));
        }
        let request = mutation_request_bytes(self.trust.expected_origin(), operation, &body)
            .ok_or_else(|| {
                DirectoryError::Malformed("registry mutation request has invalid bounds".into())
            })?;
        let publisher_key = hex(&publisher.signing_public_key());
        let publisher_signature = hex(&publisher.sign(&request));
        let deadline = Instant::now() + self.witness_wait;
        let mut pinned_response: Option<(u64, LogEntry)> = None;
        let mut mutation_retry_interval = RETRY_INTERVAL;
        loop {
            let now = Instant::now();
            if now >= deadline {
                return Err(DirectoryError::RegistryPublicationTimeout);
            }
            let outcome = tokio::time::timeout(deadline.saturating_duration_since(now), async {
                let response: AppendResponse = match self
                    .post_json(relative, &body, &publisher_key, &publisher_signature)
                    .await
                {
                    Ok(response) => response,
                    Err(DirectoryError::Unavailable) => return Ok(None),
                    Err(error) => return Err(error),
                };
                self.verify_response_identity(&response, expected, pinned_response.as_ref())?;
                pinned_response.get_or_insert_with(|| (response.log_index, response.entry.clone()));

                if response.log_index < response.inclusion_proof.tree_size {
                    self.verify_final_response(response, &consistency_bases)
                        .await
                        .map(Some)
                } else {
                    self.verify_pending_response(&response, &consistency_bases)
                        .await?;
                    Ok(None)
                }
            })
            .await
            .map_err(|_| DirectoryError::RegistryPublicationTimeout)??;
            if let Some(receipt) = outcome {
                return Ok(receipt);
            }
            self.wait_for_publication(previous, deadline).await?;
            let now = Instant::now();
            if now >= deadline {
                return Err(DirectoryError::RegistryPublicationTimeout);
            }
            tokio::time::sleep(
                mutation_retry_interval.min(deadline.saturating_duration_since(now)),
            )
            .await;
            mutation_retry_interval = mutation_retry_interval
                .saturating_mul(2)
                .min(MAX_MUTATION_RETRY_INTERVAL);
        }
    }

    async fn wait_for_publication(
        &self,
        previous: Option<&PersistedRegistryCheckpoint>,
        deadline: Instant,
    ) -> Result<()> {
        loop {
            let now = Instant::now();
            if now >= deadline {
                return Err(DirectoryError::RegistryPublicationTimeout);
            }
            match tokio::time::timeout(
                deadline.saturating_duration_since(now),
                self.readiness(previous),
            )
            .await
            {
                Ok(Ok(())) => return Ok(()),
                Ok(Err(DirectoryError::NotReady | DirectoryError::Unavailable)) => {}
                Ok(Err(error)) => return Err(error),
                Err(_) => return Err(DirectoryError::RegistryPublicationTimeout),
            }
            let now = Instant::now();
            if now >= deadline {
                return Err(DirectoryError::RegistryPublicationTimeout);
            }
            tokio::time::sleep(RETRY_INTERVAL.min(deadline.saturating_duration_since(now))).await;
        }
    }

    fn verify_response_identity(
        &self,
        response: &AppendResponse,
        expected: ExpectedMutation<'_>,
        pinned: Option<&(u64, LogEntry)>,
    ) -> Result<()> {
        if response.entry.seq() != response.log_index
            || response.inclusion_proof.path.len() > MAX_PROOF_ITEMS
            || !expected.matches(&response.entry)
        {
            return Err(DirectoryError::RegistryProof(
                "registry append receipt does not contain the requested exact leaf".into(),
            ));
        }
        if pinned
            .is_some_and(|(index, entry)| *index != response.log_index || entry != &response.entry)
        {
            return Err(DirectoryError::RegistryProof(
                "registry changed the immutable append receipt while it was pending".into(),
            ));
        }
        Ok(())
    }

    fn consistency_bases(
        &self,
        previous: Option<&PersistedRegistryCheckpoint>,
    ) -> Result<Vec<CheckpointPin>> {
        let minimum = self.trust.minimum_checkpoint();
        let mut bases = vec![minimum];
        if let Some(previous) = previous {
            let verified = Checkpoint::verify_with_witnesses(
                &previous.note,
                self.trust.checkpoint_key(),
                self.trust.witnesses(),
                self.trust.witness_threshold(),
            )
            .map_err(registry_proof)?;
            if verified.origin != self.trust.expected_origin()
                || verified.size != previous.size
                || verified.root != previous.root
                || previous.origin != self.trust.expected_origin()
                || previous.witnessed_at == 0
            {
                return Err(DirectoryError::RegistryProof(
                    "persisted registry checkpoint does not satisfy current trust pins".into(),
                ));
            }
            let previous_pin = CheckpointPin::from(&verified);
            if previous_pin.size == minimum.size && previous_pin.root != minimum.root {
                return Err(DirectoryError::RegistryProof(
                    "persisted and configured registry pins equivocate".into(),
                ));
            }
            if previous_pin != minimum {
                bases.push(previous_pin);
            }
        }
        Ok(bases)
    }

    async fn verify_pending_response(
        &self,
        response: &AppendResponse,
        consistency_bases: &[CheckpointPin],
    ) -> Result<()> {
        if !response.inclusion_proof.path.is_empty() {
            return Err(DirectoryError::RegistryProof(
                "pending registry append carried a false inclusion path".into(),
            ));
        }
        self.verify_checkpoint(&response.inclusion_proof, consistency_bases)
            .await?;
        Ok(())
    }

    async fn verify_final_response(
        &self,
        response: AppendResponse,
        consistency_bases: &[CheckpointPin],
    ) -> Result<DirectoryMutationReceipt> {
        debug_assert!(response.log_index < response.inclusion_proof.tree_size);
        let (checkpoint, witnessed_at, path) = self
            .verify_checkpoint(&response.inclusion_proof, consistency_bases)
            .await?;
        let leaf = response
            .entry
            .leaf_bytes()
            .map_err(|error| DirectoryError::RegistryProof(error.to_string()))?;
        if !verify_inclusion(
            &leaf_hash(&leaf),
            response.log_index,
            checkpoint.size,
            &path,
            &checkpoint.root,
        ) {
            return Err(DirectoryError::RegistryProof(
                "registry directory inclusion proof failed".into(),
            ));
        }
        Ok(DirectoryMutationReceipt {
            entry: response.entry,
            log_index: response.log_index,
            appended: response.appended,
            inclusion_proof: MutationInclusionProof {
                tree_size: checkpoint.size,
                root: hex(&checkpoint.root),
                path: path.iter().map(|hash| hex(hash)).collect(),
                checkpoint: response.inclusion_proof.checkpoint,
                witnessed_at,
            },
        })
    }

    async fn verify_checkpoint(
        &self,
        proof: &InclusionProof,
        consistency_bases: &[CheckpointPin],
    ) -> Result<(Checkpoint, u64, Vec<Hash>)> {
        let root = parse_hex32(&proof.root).ok_or_else(|| {
            DirectoryError::RegistryProof("registry checkpoint root is not canonical hex".into())
        })?;
        let path = parse_hashes(&proof.path)?;
        let verified = Checkpoint::verify_with_fresh_witnesses(
            &proof.checkpoint,
            self.trust.checkpoint_key(),
            self.trust.witnesses(),
            self.trust.witness_threshold(),
            now_secs(),
            self.trust.max_cosignature_age_secs(),
            self.trust.future_clock_skew_secs(),
        )
        .map_err(registry_proof)?;
        let checkpoint = verified.checkpoint;
        if checkpoint.origin != self.trust.expected_origin()
            || checkpoint.size != proof.tree_size
            || checkpoint.root != root
        {
            return Err(DirectoryError::RegistryProof(
                "registry proof is not bound to the expected witnessed checkpoint".into(),
            ));
        }
        for base in consistency_bases {
            self.verify_growth(*base, &checkpoint).await?;
        }
        let witnessed_at = verified.witnessed_at.ok_or_else(|| {
            DirectoryError::RegistryProof(
                "registry checkpoint was not independently witnessed".into(),
            )
        })?;
        Ok((checkpoint, witnessed_at, path))
    }

    async fn verify_growth(&self, base: CheckpointPin, next: &Checkpoint) -> Result<()> {
        if next.size < base.size || (next.size == base.size && next.root != base.root) {
            return Err(DirectoryError::RegistryProof(
                "registry checkpoint rolled back or equivocated".into(),
            ));
        }
        if next.size == base.size {
            return Ok(());
        }
        if base.size == 0 {
            if base.root != empty_root() {
                return Err(DirectoryError::RegistryProof(
                    "configured empty-tree registry pin is malformed".into(),
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
        let proof_root = parse_hex32(&proof.root).ok_or_else(|| {
            DirectoryError::RegistryProof("registry consistency root is malformed".into())
        })?;
        if proof.path.len() > MAX_PROOF_ITEMS {
            return Err(DirectoryError::RegistryProof(
                "registry consistency proof exceeds the client limit".into(),
            ));
        }
        let path = parse_hashes(&proof.path)?;
        if proof.from != base.size
            || proof.to != next.size
            || proof_root != next.root
            || !verify_consistency(base.size, &base.root, next.size, &next.root, &path)
        {
            return Err(DirectoryError::RegistryProof(
                "registry consistency proof failed".into(),
            ));
        }
        Ok(())
    }

    async fn post_json<R>(
        &self,
        relative: &str,
        body: &[u8],
        publisher_key: &str,
        publisher_signature: &str,
    ) -> Result<R>
    where
        R: DeserializeOwned,
    {
        let url = self.route(relative)?;
        let response = self
            .http
            .post(url)
            .header(reqwest::header::ACCEPT, "application/json")
            .header(reqwest::header::CONTENT_TYPE, "application/json")
            .header(DIRECTORY_PUBLISHER_KEY_HEADER, publisher_key)
            .header(DIRECTORY_PUBLISHER_SIGNATURE_HEADER, publisher_signature)
            .body(body.to_vec())
            .send()
            .await
            .map_err(|_| DirectoryError::Unavailable)?;
        self.decode_response(response).await
    }

    async fn get_json<R: DeserializeOwned>(&self, relative: &str) -> Result<R> {
        let response = self
            .http
            .get(self.route(relative)?)
            .header(reqwest::header::ACCEPT, "application/json")
            .send()
            .await
            .map_err(|_| DirectoryError::Unavailable)?;
        self.decode_response(response).await
    }

    async fn get_bytes(&self, relative: &str) -> Result<Vec<u8>> {
        let mut response = self
            .http
            .get(self.route(relative)?)
            .send()
            .await
            .map_err(|_| DirectoryError::Unavailable)?;
        if !response.status().is_success() {
            return Err(DirectoryError::Unavailable);
        }
        let mut body = Vec::new();
        while let Some(chunk) = response
            .chunk()
            .await
            .map_err(|_| DirectoryError::Unavailable)?
        {
            if body.len().saturating_add(chunk.len()) > MAX_RESPONSE_BYTES {
                return Err(DirectoryError::RegistryProof(
                    "registry response exceeds the client limit".into(),
                ));
            }
            body.extend_from_slice(&chunk);
        }
        Ok(body)
    }

    fn route(&self, relative: &str) -> Result<Url> {
        self.base_url
            .join(relative)
            .map_err(|_| DirectoryError::Malformed("invalid registry route".into()))
    }

    async fn decode_response<R: DeserializeOwned>(
        &self,
        mut response: reqwest::Response,
    ) -> Result<R> {
        match response.status() {
            status if status.is_success() => {}
            StatusCode::UNAUTHORIZED => return Err(DirectoryError::KeyMismatch),
            StatusCode::NOT_FOUND => return Err(DirectoryError::NotFound),
            StatusCode::CONFLICT => return Err(DirectoryError::Replay),
            StatusCode::BAD_REQUEST => {
                return Err(DirectoryError::Malformed(
                    "registry rejected the directory mutation".into(),
                ));
            }
            _ => return Err(DirectoryError::Unavailable),
        }
        let mut body = Vec::new();
        while let Some(chunk) = response
            .chunk()
            .await
            .map_err(|_| DirectoryError::Unavailable)?
        {
            if body.len().saturating_add(chunk.len()) > MAX_RESPONSE_BYTES {
                return Err(DirectoryError::RegistryProof(
                    "registry response exceeds the client limit".into(),
                ));
            }
            body.extend_from_slice(&chunk);
        }
        serde_json::from_slice(&body)
            .map_err(|_| DirectoryError::RegistryProof("registry response is malformed".into()))
    }
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct AppendResponse {
    entry: LogEntry,
    log_index: u64,
    appended: bool,
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
struct ConsistencyResponse {
    from: u64,
    to: u64,
    root: String,
    path: Vec<String>,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct RegistryStatus {
    ready: bool,
    committed_size: u64,
    published_size: u64,
    lag_entries: u64,
    witnessed_at: Option<u64>,
}

#[derive(Clone, Copy)]
enum ExpectedMutation<'a> {
    Add(&'a DirectoryAdd),
    Remove(&'a DirectoryRemove),
}

impl ExpectedMutation<'_> {
    fn matches(self, entry: &LogEntry) -> bool {
        match self {
            Self::Add(expected) => entry.directory_addition() == Some(expected),
            Self::Remove(expected) => entry.directory_removal() == Some(expected),
        }
    }
}

fn validate_base_url(input: &str) -> Result<Url> {
    if input.is_empty() || input.len() > MAX_URL_BYTES {
        return Err(DirectoryError::Malformed("invalid registry URL".into()));
    }
    let mut url =
        Url::parse(input).map_err(|_| DirectoryError::Malformed("invalid registry URL".into()))?;
    if url.cannot_be_a_base()
        || !url.username().is_empty()
        || url.password().is_some()
        || url.query().is_some()
        || url.fragment().is_some()
        || url.host_str().is_none()
        || url.port() == Some(0)
        || (url.path() != "/" && !url.path().is_empty())
    {
        return Err(DirectoryError::Malformed("invalid registry URL".into()));
    }
    let host = url
        .host_str()
        .ok_or_else(|| DirectoryError::Malformed("invalid registry URL".into()))?;
    if is_localhost_name(host) {
        return Err(DirectoryError::Malformed("invalid registry URL".into()));
    }
    let loopback = is_numeric_loopback_host(host);
    if url.scheme() != "https" && !(url.scheme() == "http" && loopback) {
        return Err(DirectoryError::Malformed(
            "registry must use HTTPS (HTTP is loopback-only)".into(),
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
                DirectoryError::RegistryProof("proof hash is not canonical hex".into())
            })
        })
        .collect()
}

fn parse_hex32(input: &str) -> Option<Hash> {
    if input.len() != 64
        || input
            .bytes()
            .any(|byte| !byte.is_ascii_digit() && !(b'a'..=b'f').contains(&byte))
    {
        return None;
    }
    let mut out = [0u8; 32];
    for (index, chunk) in input.as_bytes().chunks_exact(2).enumerate() {
        out[index] = u8::from_str_radix(std::str::from_utf8(chunk).ok()?, 16).ok()?;
    }
    Some(out)
}

fn checkpoint_origin(note: &str) -> Result<String> {
    note.lines()
        .next()
        .filter(|origin| !origin.is_empty())
        .map(str::to_owned)
        .ok_or_else(|| DirectoryError::RegistryProof("checkpoint origin is missing".into()))
}

fn registry_proof(error: RegistryError) -> DirectoryError {
    DirectoryError::RegistryProof(error.to_string())
}

fn now_secs() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|duration| duration.as_secs())
        .unwrap_or(0)
}

#[cfg(test)]
mod tests {
    use std::sync::atomic::AtomicUsize;
    use std::sync::atomic::{AtomicBool, Ordering};
    use std::sync::Arc;

    use axum::extract::State;
    use axum::routing::{get, post};
    use axum::{Json, Router};
    use ed25519_dalek::SigningKey;
    use pigeonpost_registry::{ProofBundle, Registry, RegistryConfig, WitnessKey};

    use super::*;
    use crate::entry::{DirectoryEntry, DrainAuthorization, LoftPolicy, LoftState};
    use crate::server;
    use crate::Directory;

    const ORIGIN: &str = "pigeonpost.test/registry";

    fn publisher() -> Directory {
        Directory::in_memory().unwrap()
    }

    #[test]
    fn registry_url_is_an_https_or_numeric_loopback_origin() {
        for accepted in [
            "https://registry.example",
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
            "https://registry.example:0",
            "https://user@registry.example",
            "https://registry.example/prefix",
            "https://registry.example?query=1",
        ] {
            assert!(validate_base_url(rejected).is_err(), "accepted {rejected}");
        }
    }

    struct WitnessProxy {
        registry: Arc<Registry>,
        witness_name: &'static str,
        witness_key: SigningKey,
        cosign: AtomicBool,
        tamper_path: AtomicBool,
    }

    struct PendingProxy {
        witness: WitnessProxy,
        requests: AtomicUsize,
        status_requests: AtomicUsize,
        publish_after: usize,
        publish_after_status: Option<usize>,
        ready_after_status: usize,
        unavailable_on: Option<usize>,
        drift_index_on: Option<usize>,
        response_delay: Duration,
    }

    #[derive(Serialize)]
    struct TestAppendResponse {
        entry: LogEntry,
        log_index: u64,
        appended: bool,
        inclusion_proof: TestInclusionProof,
    }

    #[derive(Serialize)]
    struct TestInclusionProof {
        tree_size: u64,
        root: String,
        path: Vec<String>,
        checkpoint: String,
    }

    #[derive(Serialize)]
    struct TestConsistencyResponse {
        from: u64,
        to: u64,
        root: String,
        path: Vec<String>,
    }

    #[derive(Serialize)]
    struct TestRegistryStatus {
        ready: bool,
        committed_size: u64,
        published_size: u64,
        lag_entries: u64,
        witnessed_at: Option<u64>,
    }

    fn proxy_router(state: Arc<WitnessProxy>) -> Router {
        Router::new()
            .route("/v1/directory/add", post(proxy_add))
            .route("/v1/directory/remove", post(proxy_remove))
            .route("/v1/log/consistency", get(proxy_consistency))
            .route("/v1/log/status", get(proxy_status))
            .route("/v1/log/checkpoint", get(proxy_checkpoint))
            .with_state(state)
    }

    fn pending_router(state: Arc<PendingProxy>) -> Router {
        Router::new()
            .route("/v1/directory/add", post(pending_add))
            .route("/v1/log/consistency", get(pending_consistency))
            .route("/v1/log/status", get(pending_status))
            .route("/v1/log/checkpoint", get(pending_checkpoint))
            .with_state(state)
    }

    async fn pending_add(
        State(state): State<Arc<PendingProxy>>,
        Json(mutation): Json<DirectoryAdd>,
    ) -> std::result::Result<Json<TestAppendResponse>, StatusCode> {
        let request = state.requests.fetch_add(1, Ordering::SeqCst);
        if state.unavailable_on == Some(request) {
            return Err(StatusCode::SERVICE_UNAVAILABLE);
        }
        let mutation_copy = mutation.clone();
        let logged = state
            .witness
            .registry
            .append_directory_add(mutation)
            .unwrap();
        if !state.response_delay.is_zero() {
            tokio::time::sleep(state.response_delay).await;
        }
        let mut response = proxy_receipt(&state.witness, logged);
        let publication_pending = state
            .publish_after_status
            .map_or(request < state.publish_after, |status| {
                state.status_requests.load(Ordering::SeqCst) <= status
            });
        if publication_pending {
            let operator = SigningKey::from_bytes(&[41; 32]);
            let checkpoint = Checkpoint {
                origin: ORIGIN.into(),
                size: 0,
                root: empty_root(),
            };
            let mut note = checkpoint.sign(&operator);
            note.push_str(
                &checkpoint
                    .cosignature_line(
                        state.witness.witness_name,
                        &state.witness.witness_key,
                        now_secs(),
                    )
                    .unwrap(),
            );
            response.inclusion_proof = TestInclusionProof {
                tree_size: 0,
                root: hex(&empty_root()),
                path: Vec::new(),
                checkpoint: note,
            };
        }
        if state.drift_index_on == Some(request) {
            let changed_index = response.log_index + 1;
            response.entry =
                LogEntry::directory_add(changed_index, mutation_copy, response.entry.ts_ms());
            response.log_index = changed_index;
            response.inclusion_proof.path.clear();
        }
        Ok(Json(response))
    }

    async fn pending_status(State(state): State<Arc<PendingProxy>>) -> Json<TestRegistryStatus> {
        let request = state.status_requests.fetch_add(1, Ordering::SeqCst);
        let ready = request >= state.ready_after_status;
        let head = state.witness.registry.head().unwrap();
        Json(TestRegistryStatus {
            ready,
            committed_size: head.size,
            published_size: if ready { head.size } else { 0 },
            lag_entries: if ready { 0 } else { head.size },
            witnessed_at: ready.then(now_secs),
        })
    }

    async fn pending_checkpoint(State(state): State<Arc<PendingProxy>>) -> String {
        let head = state.witness.registry.head().unwrap();
        witnessed_note(&state.witness, &head)
    }

    async fn pending_consistency(
        State(state): State<Arc<PendingProxy>>,
        axum::extract::Query(query): axum::extract::Query<ConsistencyQuery>,
    ) -> Json<TestConsistencyResponse> {
        let head = state.witness.registry.head().unwrap();
        assert_eq!(query.to, head.size);
        let path = state
            .witness
            .registry
            .consistency_proof_between(query.from, query.to)
            .unwrap();
        Json(TestConsistencyResponse {
            from: query.from,
            to: query.to,
            root: hex(&head.root),
            path: path.iter().map(|hash| hex(hash)).collect(),
        })
    }

    async fn proxy_add(
        State(state): State<Arc<WitnessProxy>>,
        Json(mutation): Json<DirectoryAdd>,
    ) -> Json<TestAppendResponse> {
        let logged = state.registry.append_directory_add(mutation).unwrap();
        Json(proxy_receipt(&state, logged))
    }

    async fn proxy_remove(
        State(state): State<Arc<WitnessProxy>>,
        Json(mutation): Json<DirectoryRemove>,
    ) -> Json<TestAppendResponse> {
        let logged = state.registry.append_directory_remove(mutation).unwrap();
        Json(proxy_receipt(&state, logged))
    }

    async fn proxy_consistency(
        State(state): State<Arc<WitnessProxy>>,
        axum::extract::Query(query): axum::extract::Query<ConsistencyQuery>,
    ) -> Json<TestConsistencyResponse> {
        let head = state.registry.head().unwrap();
        assert_eq!(query.to, head.size);
        let path = state
            .registry
            .consistency_proof_between(query.from, query.to)
            .unwrap();
        Json(TestConsistencyResponse {
            from: query.from,
            to: query.to,
            root: hex(&head.root),
            path: path.iter().map(|hash| hex(hash)).collect(),
        })
    }

    async fn proxy_status(State(state): State<Arc<WitnessProxy>>) -> Json<TestRegistryStatus> {
        let head = state.registry.head().unwrap();
        let ready = state.cosign.load(Ordering::SeqCst);
        Json(TestRegistryStatus {
            ready,
            committed_size: head.size,
            published_size: head.size,
            lag_entries: 0,
            witnessed_at: ready.then(now_secs),
        })
    }

    async fn proxy_checkpoint(State(state): State<Arc<WitnessProxy>>) -> String {
        let head = state.registry.head().unwrap();
        witnessed_note(&state, &head)
    }

    #[derive(Deserialize)]
    struct ConsistencyQuery {
        from: u64,
        to: u64,
    }

    fn proxy_receipt(
        state: &WitnessProxy,
        logged: pigeonpost_registry::LoggedDirectoryMutation,
    ) -> TestAppendResponse {
        let checkpoint = witnessed_note(state, &logged.inclusion);
        let mut proof = logged.inclusion.path;
        if state.tamper_path.load(Ordering::SeqCst) {
            if let Some(first) = proof.first_mut() {
                first[0] ^= 0x80;
            } else {
                proof.push([0x80; 32]);
            }
        }
        TestAppendResponse {
            entry: logged.entry,
            log_index: logged.index,
            appended: logged.appended,
            inclusion_proof: TestInclusionProof {
                tree_size: logged.inclusion.size,
                root: hex(&logged.inclusion.root),
                path: proof.iter().map(|hash| hex(hash)).collect(),
                checkpoint,
            },
        }
    }

    fn witnessed_note(state: &WitnessProxy, proof: &ProofBundle) -> String {
        if !state.cosign.load(Ordering::SeqCst) {
            return proof.checkpoint.clone();
        }
        let checkpoint =
            Checkpoint::verify(&proof.checkpoint, &state.registry.verifying_key()).unwrap();
        format!(
            "{}{}",
            proof.checkpoint,
            checkpoint
                .cosignature_line(state.witness_name, &state.witness_key, now_secs())
                .unwrap()
        )
    }

    fn registry() -> Arc<Registry> {
        Arc::new(
            Registry::in_memory(RegistryConfig {
                origin: ORIGIN.into(),
                signing_key: SigningKey::from_bytes(&[41; 32]),
                allow_mock_identities: false,
            })
            .unwrap(),
        )
    }

    fn trust(registry: &Registry, witness_key: &SigningKey) -> RegistryTrust {
        RegistryTrust::new(
            ORIGIN,
            registry.verifying_key().to_bytes(),
            vec![WitnessKey::new("witness.test", witness_key.verifying_key()).unwrap()],
            1,
            CheckpointPin {
                size: 0,
                root: empty_root(),
            },
            60,
            5,
        )
        .unwrap()
    }

    fn entry(key: &SigningKey) -> DirectoryEntry {
        DirectoryEntry::signed(
            key,
            "https://loft.example",
            None,
            10,
            30,
            LoftPolicy {
                open: true,
                pow_floor: 0,
                max_event_bytes: 65_536,
            },
            0.0,
        )
    }

    async fn spawn(app: Router) -> (String, tokio::task::JoinHandle<()>) {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let url = format!("http://{}", listener.local_addr().unwrap());
        let task = tokio::spawn(async move {
            axum::serve(
                listener,
                app.into_make_service_with_connect_info::<std::net::SocketAddr>(),
            )
            .await
            .unwrap();
        });
        (url, task)
    }

    #[tokio::test]
    async fn readiness_requires_fresh_witnessed_publication_without_lag() {
        let registry = registry();
        let witness_key = SigningKey::from_bytes(&[42; 32]);
        let proxy = Arc::new(WitnessProxy {
            registry: Arc::clone(&registry),
            witness_name: "witness.test",
            witness_key: witness_key.clone(),
            cosign: AtomicBool::new(true),
            tamper_path: AtomicBool::new(false),
        });
        let (registry_url, task) = spawn(proxy_router(Arc::clone(&proxy))).await;
        let client = Arc::new(
            RegistryLogClient::with_witness_wait(
                &registry_url,
                trust(&registry, &witness_key),
                Duration::from_secs(1),
            )
            .unwrap(),
        );

        assert!(client.readiness(None).await.is_ok());
        let directory = Arc::new(Directory::in_memory().unwrap());
        directory.mark_probe_sweep(now_secs()).unwrap();
        let (directory_url, directory_task) = spawn(server::router_with_registry_log(
            directory,
            Arc::clone(&client),
        ))
        .await;
        assert_eq!(
            Client::new()
                .get(format!("{directory_url}/ready"))
                .send()
                .await
                .unwrap()
                .status(),
            StatusCode::OK
        );
        proxy.cosign.store(false, Ordering::SeqCst);
        assert!(matches!(
            client.readiness(None).await,
            Err(DirectoryError::NotReady)
        ));
        assert_eq!(
            Client::new()
                .get(format!("{directory_url}/ready"))
                .send()
                .await
                .unwrap()
                .status(),
            StatusCode::SERVICE_UNAVAILABLE
        );
        directory_task.abort();
        task.abort();
    }

    #[tokio::test]
    async fn authenticated_pending_receipt_is_polled_until_exact_leaf_is_published() {
        let registry = registry();
        let witness_key = SigningKey::from_bytes(&[42; 32]);
        let proxy = Arc::new(PendingProxy {
            witness: WitnessProxy {
                registry: Arc::clone(&registry),
                witness_name: "witness.test",
                witness_key: witness_key.clone(),
                cosign: AtomicBool::new(true),
                tamper_path: AtomicBool::new(false),
            },
            requests: AtomicUsize::new(0),
            status_requests: AtomicUsize::new(0),
            publish_after: 1,
            publish_after_status: None,
            ready_after_status: 0,
            unavailable_on: None,
            drift_index_on: None,
            response_delay: Duration::ZERO,
        });
        let (registry_url, task) = spawn(pending_router(Arc::clone(&proxy))).await;
        let client = RegistryLogClient::with_witness_wait(
            &registry_url,
            trust(&registry, &witness_key),
            Duration::from_secs(1),
        )
        .unwrap();
        let mutation = entry(&SigningKey::from_bytes(&[15; 32]))
            .registry_addition()
            .unwrap();

        let receipt = client
            .append_add(&publisher(), &mutation, None)
            .await
            .unwrap();
        assert_eq!(receipt.log_index, 0);
        assert_eq!(receipt.inclusion_proof.tree_size, 1);
        assert_eq!(proxy.requests.load(Ordering::SeqCst), 2);
        assert_eq!(registry.size().unwrap(), 1, "polling must be idempotent");
        task.abort();
    }

    #[tokio::test]
    async fn transient_unavailable_during_publication_retries_the_exact_leaf() {
        let registry = registry();
        let witness_key = SigningKey::from_bytes(&[42; 32]);
        let proxy = Arc::new(PendingProxy {
            witness: WitnessProxy {
                registry: Arc::clone(&registry),
                witness_name: "witness.test",
                witness_key: witness_key.clone(),
                cosign: AtomicBool::new(true),
                tamper_path: AtomicBool::new(false),
            },
            requests: AtomicUsize::new(0),
            status_requests: AtomicUsize::new(0),
            publish_after: 1,
            publish_after_status: None,
            ready_after_status: 0,
            unavailable_on: Some(1),
            drift_index_on: None,
            response_delay: Duration::ZERO,
        });
        let (registry_url, task) = spawn(pending_router(Arc::clone(&proxy))).await;
        let client = RegistryLogClient::with_witness_wait(
            &registry_url,
            trust(&registry, &witness_key),
            Duration::from_secs(1),
        )
        .unwrap();
        let mutation = entry(&SigningKey::from_bytes(&[19; 32]))
            .registry_addition()
            .unwrap();

        let receipt = client
            .append_add(&publisher(), &mutation, None)
            .await
            .unwrap();
        assert_eq!(receipt.log_index, 0);
        assert_eq!(receipt.entry.directory_addition(), Some(&mutation));
        assert_eq!(receipt.inclusion_proof.tree_size, 1);
        assert_eq!(proxy.requests.load(Ordering::SeqCst), 3);
        assert_eq!(registry.size().unwrap(), 1, "retries must be idempotent");
        task.abort();
    }

    #[tokio::test]
    async fn publication_wait_polls_reads_without_spending_the_mutation_budget() {
        let registry = registry();
        let witness_key = SigningKey::from_bytes(&[42; 32]);
        let proxy = Arc::new(PendingProxy {
            witness: WitnessProxy {
                registry: Arc::clone(&registry),
                witness_name: "witness.test",
                witness_key: witness_key.clone(),
                cosign: AtomicBool::new(true),
                tamper_path: AtomicBool::new(false),
            },
            requests: AtomicUsize::new(0),
            status_requests: AtomicUsize::new(0),
            publish_after: usize::MAX,
            publish_after_status: Some(12),
            ready_after_status: 12,
            unavailable_on: None,
            drift_index_on: None,
            response_delay: Duration::ZERO,
        });
        let (registry_url, task) = spawn(pending_router(Arc::clone(&proxy))).await;
        let client = RegistryLogClient::with_witness_wait(
            &registry_url,
            trust(&registry, &witness_key),
            Duration::from_secs(4),
        )
        .unwrap();
        let mutation = entry(&SigningKey::from_bytes(&[20; 32]))
            .registry_addition()
            .unwrap();

        let receipt = client
            .append_add(&publisher(), &mutation, None)
            .await
            .unwrap();
        assert_eq!(receipt.entry.directory_addition(), Some(&mutation));
        assert_eq!(proxy.requests.load(Ordering::SeqCst), 2);
        assert!(proxy.status_requests.load(Ordering::SeqCst) > 10);
        assert_eq!(registry.size().unwrap(), 1, "polling must be idempotent");
        task.abort();
    }

    #[tokio::test]
    async fn pending_publication_timeout_is_typed_and_bounded() {
        let registry = registry();
        let witness_key = SigningKey::from_bytes(&[42; 32]);
        let proxy = Arc::new(PendingProxy {
            witness: WitnessProxy {
                registry: Arc::clone(&registry),
                witness_name: "witness.test",
                witness_key: witness_key.clone(),
                cosign: AtomicBool::new(true),
                tamper_path: AtomicBool::new(false),
            },
            requests: AtomicUsize::new(0),
            status_requests: AtomicUsize::new(0),
            publish_after: usize::MAX,
            publish_after_status: None,
            ready_after_status: 0,
            unavailable_on: None,
            drift_index_on: None,
            response_delay: Duration::from_secs(1),
        });
        let (registry_url, task) = spawn(pending_router(Arc::clone(&proxy))).await;
        let client = RegistryLogClient::with_witness_wait(
            &registry_url,
            trust(&registry, &witness_key),
            Duration::from_millis(250),
        )
        .unwrap();
        let mutation = entry(&SigningKey::from_bytes(&[16; 32]))
            .registry_addition()
            .unwrap();

        let started = Instant::now();
        assert!(matches!(
            client.append_add(&publisher(), &mutation, None).await,
            Err(DirectoryError::RegistryPublicationTimeout)
        ));
        assert!(started.elapsed() < Duration::from_secs(2));
        assert_eq!(proxy.requests.load(Ordering::SeqCst), 1);
        assert_eq!(registry.size().unwrap(), 1);
        task.abort();
    }

    #[tokio::test]
    async fn cancelled_request_is_recovered_without_a_second_loft_request() {
        let registry = registry();
        let witness_key = SigningKey::from_bytes(&[42; 32]);
        let proxy = Arc::new(PendingProxy {
            witness: WitnessProxy {
                registry: Arc::clone(&registry),
                witness_name: "witness.test",
                witness_key: witness_key.clone(),
                cosign: AtomicBool::new(true),
                tamper_path: AtomicBool::new(false),
            },
            requests: AtomicUsize::new(0),
            status_requests: AtomicUsize::new(0),
            publish_after: 0,
            publish_after_status: None,
            ready_after_status: 0,
            unavailable_on: None,
            drift_index_on: None,
            response_delay: Duration::from_millis(200),
        });
        let (registry_url, registry_task) = spawn(pending_router(proxy)).await;
        let log = Arc::new(
            RegistryLogClient::with_witness_wait(
                &registry_url,
                trust(&registry, &witness_key),
                Duration::from_secs(1),
            )
            .unwrap(),
        );
        let directory = Arc::new(Directory::in_memory().unwrap());
        let config = server::DirectoryHttpConfig::direct()
            .with_limits(server::DirectoryLimits {
                request_timeout_ms: 50,
                ..server::DirectoryLimits::default()
            })
            .unwrap();
        let (directory_url, directory_task) = spawn(server::router_with_registry_log_and_config(
            Arc::clone(&directory),
            log,
            config,
        ))
        .await;

        let response = Client::new()
            .post(format!("{directory_url}/v1/directory/submit"))
            .json(&serde_json::json!({
                "entry": entry(&SigningKey::from_bytes(&[18; 32]))
            }))
            .send()
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::SERVICE_UNAVAILABLE);
        assert!(matches!(
            directory.entry("https://loft.example"),
            Err(DirectoryError::NotFound)
        ));

        // Wait for the end state, not for one step on the way to it. Recovery consumes the
        // reservation, publishes the entry, and appends the leaf; polling only the first and then
        // asserting the other two is a race, and it is the race that made this test flaky rather
        // than the length of the budget. The deadline is a ceiling: the loop leaves as soon as
        // recovery is complete, so a generous bound costs a correct build nothing.
        let recovery_deadline = Instant::now() + Duration::from_secs(60);
        loop {
            let recovered = !directory.has_pending_mutations().unwrap()
                && directory.entry("https://loft.example").is_ok()
                && registry.size().unwrap() == 1;
            if recovered || Instant::now() >= recovery_deadline {
                break;
            }
            tokio::time::sleep(Duration::from_millis(25)).await;
        }
        // Restated exactly, so a timeout still fails on the specific thing that did not happen.
        assert!(
            !directory.has_pending_mutations().unwrap(),
            "the supervisor must consume the orphaned reservation"
        );
        assert!(
            directory.entry("https://loft.example").is_ok(),
            "recovery must publish the entry it reserved"
        );
        assert_eq!(
            registry.size().unwrap(),
            1,
            "recovery must replay one exact leaf"
        );

        directory_task.abort();
        registry_task.abort();
    }

    #[tokio::test]
    async fn startup_supervisor_recovers_a_persistent_orphan_without_a_loft_retry() {
        let registry = registry();
        let witness_key = SigningKey::from_bytes(&[42; 32]);
        let proxy = Arc::new(WitnessProxy {
            registry: Arc::clone(&registry),
            witness_name: "witness.test",
            witness_key: witness_key.clone(),
            cosign: AtomicBool::new(true),
            tamper_path: AtomicBool::new(false),
        });
        let (registry_url, registry_task) = spawn(proxy_router(proxy)).await;
        let log = Arc::new(
            RegistryLogClient::with_witness_wait(
                &registry_url,
                trust(&registry, &witness_key),
                Duration::from_secs(1),
            )
            .unwrap(),
        );
        let temp = tempfile::tempdir().unwrap();
        let private_parent = temp.path().join("private");
        #[cfg(not(windows))]
        std::fs::create_dir_all(&private_parent).unwrap();
        let database = private_parent.join("directory.db");
        let submission = entry(&SigningKey::from_bytes(&[27; 32]));
        {
            let directory = Directory::open(database.to_str().unwrap()).unwrap();
            directory.reserve_add(&submission, now_secs()).unwrap();
            assert!(directory.has_pending_mutations().unwrap());
        }

        let directory = Arc::new(Directory::open(database.to_str().unwrap()).unwrap());
        let (_directory_url, directory_task) = spawn(server::router_with_registry_log(
            Arc::clone(&directory),
            log,
        ))
        .await;
        // Same shape as the recovery test above: wait for every part of the end state, or a fast
        // runner observes the gap between consuming the reservation and publishing the entry.
        let deadline = Instant::now() + Duration::from_secs(60);
        loop {
            let recovered = !directory.has_pending_mutations().unwrap()
                && directory.entry("https://loft.example").is_ok()
                && registry.size().unwrap() == 1;
            if recovered || Instant::now() >= deadline {
                break;
            }
            tokio::time::sleep(Duration::from_millis(25)).await;
        }
        assert!(!directory.has_pending_mutations().unwrap());
        assert!(directory.entry("https://loft.example").is_ok());
        assert_eq!(registry.size().unwrap(), 1);

        directory_task.abort();
        registry_task.abort();
    }

    #[tokio::test]
    async fn pending_receipt_index_drift_is_rejected_without_further_polling() {
        let registry = registry();
        let witness_key = SigningKey::from_bytes(&[42; 32]);
        let proxy = Arc::new(PendingProxy {
            witness: WitnessProxy {
                registry: Arc::clone(&registry),
                witness_name: "witness.test",
                witness_key: witness_key.clone(),
                cosign: AtomicBool::new(true),
                tamper_path: AtomicBool::new(false),
            },
            requests: AtomicUsize::new(0),
            status_requests: AtomicUsize::new(0),
            publish_after: 1,
            publish_after_status: None,
            ready_after_status: 0,
            unavailable_on: None,
            drift_index_on: Some(1),
            response_delay: Duration::ZERO,
        });
        let (registry_url, task) = spawn(pending_router(Arc::clone(&proxy))).await;
        let client = RegistryLogClient::with_witness_wait(
            &registry_url,
            trust(&registry, &witness_key),
            Duration::from_secs(1),
        )
        .unwrap();
        let mutation = entry(&SigningKey::from_bytes(&[17; 32]))
            .registry_addition()
            .unwrap();

        assert!(matches!(
            client.append_add(&publisher(), &mutation, None).await,
            Err(DirectoryError::RegistryProof(_))
        ));
        assert_eq!(proxy.requests.load(Ordering::SeqCst), 2);
        task.abort();
    }

    #[tokio::test]
    async fn rejected_local_admission_never_reaches_the_registry() {
        let registry = registry();
        let witness_key = SigningKey::from_bytes(&[42; 32]);
        let proxy = Arc::new(WitnessProxy {
            registry: Arc::clone(&registry),
            witness_name: "witness.test",
            witness_key: witness_key.clone(),
            cosign: AtomicBool::new(true),
            tamper_path: AtomicBool::new(false),
        });
        let (registry_url, registry_task) = spawn(proxy_router(proxy)).await;
        let log = Arc::new(
            RegistryLogClient::with_witness_wait(
                &registry_url,
                trust(&registry, &witness_key),
                Duration::from_secs(1),
            )
            .unwrap(),
        );
        let directory = Arc::new(Directory::in_memory().unwrap());
        let (directory_url, directory_task) = spawn(server::router_with_registry_log(
            Arc::clone(&directory),
            log,
        ))
        .await;
        let http = Client::new();
        let key = SigningKey::from_bytes(&[7; 32]);

        let future = DirectoryEntry::signed_with_sequence(
            &key,
            "https://loft.example",
            None,
            10,
            30,
            LoftPolicy {
                open: true,
                pow_floor: 0,
                max_event_bytes: 65_536,
            },
            0.0,
            2,
        );
        assert_eq!(
            http.post(format!("{directory_url}/v1/directory/submit"))
                .json(&serde_json::json!({ "entry": future }))
                .send()
                .await
                .unwrap()
                .status(),
            StatusCode::CONFLICT
        );

        let mut malformed = entry(&key);
        malformed.capacity_gb += 1;
        assert_eq!(
            http.post(format!("{directory_url}/v1/directory/submit"))
                .json(&serde_json::json!({ "entry": malformed }))
                .send()
                .await
                .unwrap()
                .status(),
            StatusCode::UNAUTHORIZED
        );

        directory.submit(entry(&key), now_secs()).unwrap();
        directory
            .set_state("https://loft.example", LoftState::Active)
            .unwrap();
        let other_key = SigningKey::from_bytes(&[8; 32]);
        assert_eq!(
            http.post(format!("{directory_url}/v1/directory/submit"))
                .json(&serde_json::json!({ "entry": entry(&other_key) }))
                .send()
                .await
                .unwrap()
                .status(),
            StatusCode::UNAUTHORIZED
        );

        let pending = DirectoryEntry::signed(
            &other_key,
            "https://pending.example",
            None,
            10,
            30,
            LoftPolicy {
                open: true,
                pow_floor: 0,
                max_event_bytes: 65_536,
            },
            0.0,
        );
        directory.submit(pending, now_secs()).unwrap();
        let drain = DrainAuthorization::signed(
            &other_key,
            "https://pending.example",
            now_secs() + 3_600,
            2,
        );
        assert_eq!(
            http.post(format!("{directory_url}/v1/directory/drain"))
                .json(&drain)
                .send()
                .await
                .unwrap()
                .status(),
            StatusCode::SERVICE_UNAVAILABLE
        );

        assert_eq!(registry.size().unwrap(), 0);
        assert!(!directory.has_pending_mutations().unwrap());
        directory_task.abort();
        registry_task.abort();
    }

    #[tokio::test]
    async fn exhausted_loft_bucket_cannot_leave_a_drain_reservation_or_registry_leaf() {
        let registry = registry();
        let witness_key = SigningKey::from_bytes(&[42; 32]);
        let proxy = Arc::new(WitnessProxy {
            registry: Arc::clone(&registry),
            witness_name: "witness.test",
            witness_key: witness_key.clone(),
            cosign: AtomicBool::new(true),
            tamper_path: AtomicBool::new(false),
        });
        let (registry_url, registry_task) = spawn(proxy_router(proxy)).await;
        let log = Arc::new(
            RegistryLogClient::with_witness_wait(
                &registry_url,
                trust(&registry, &witness_key),
                Duration::from_secs(1),
            )
            .unwrap(),
        );
        let directory = Arc::new(Directory::in_memory().unwrap());
        let loft_key = SigningKey::from_bytes(&[7; 32]);
        directory.submit(entry(&loft_key), now_secs()).unwrap();
        directory
            .set_state("https://loft.example", LoftState::Active)
            .unwrap();
        let limits = server::DirectoryLimits {
            loft_mutations_per_minute: 1,
            ..server::DirectoryLimits::default()
        };
        let config = server::DirectoryHttpConfig::direct()
            .with_limits(limits)
            .unwrap();
        let (directory_url, directory_task) = spawn(server::router_with_registry_log_and_config(
            Arc::clone(&directory),
            log,
            config,
        ))
        .await;
        let http = Client::new();

        // A validly signed but stale request consumes the loft bucket, then fails the full
        // transactional preflight without creating a reservation.
        let stale =
            DrainAuthorization::signed(&loft_key, "https://loft.example", now_secs() + 3_600, 1);
        assert_eq!(
            http.post(format!("{directory_url}/v1/directory/drain"))
                .json(&stale)
                .send()
                .await
                .unwrap()
                .status(),
            StatusCode::CONFLICT
        );

        let valid =
            DrainAuthorization::signed(&loft_key, "https://loft.example", now_secs() + 3_600, 2);
        assert_eq!(
            http.post(format!("{directory_url}/v1/directory/drain"))
                .json(&valid)
                .send()
                .await
                .unwrap()
                .status(),
            StatusCode::TOO_MANY_REQUESTS
        );

        assert_eq!(registry.size().unwrap(), 0);
        assert!(!directory.has_pending_mutations().unwrap());
        let unchanged = directory.entry("https://loft.example").unwrap();
        assert_eq!(unchanged.state, LoftState::Active);
        assert_eq!(unchanged.drain_after, None);
        assert_eq!(unchanged.last_mutation_sequence, 1);
        directory_task.abort();
        registry_task.abort();
    }

    #[tokio::test]
    async fn cosigned_proxy_proves_add_retry_and_remove_before_local_commit() {
        let registry = registry();
        let witness_key = SigningKey::from_bytes(&[42; 32]);
        let proxy = Arc::new(WitnessProxy {
            registry: Arc::clone(&registry),
            witness_name: "witness.test",
            witness_key: witness_key.clone(),
            cosign: AtomicBool::new(true),
            tamper_path: AtomicBool::new(false),
        });
        let (registry_url, registry_task) = spawn(proxy_router(proxy)).await;
        let log = Arc::new(
            RegistryLogClient::with_witness_wait(
                &registry_url,
                trust(&registry, &witness_key),
                Duration::from_secs(1),
            )
            .unwrap(),
        );
        let directory = Arc::new(Directory::in_memory().unwrap());
        let (directory_url, directory_task) = spawn(server::router_with_registry_log(
            Arc::clone(&directory),
            log,
        ))
        .await;
        let loft_key = SigningKey::from_bytes(&[7; 32]);
        let submission = entry(&loft_key);
        let http = Client::new();

        for _ in 0..2 {
            let response = http
                .post(format!("{directory_url}/v1/directory/submit"))
                .json(&serde_json::json!({ "entry": submission }))
                .send()
                .await
                .unwrap();
            assert_eq!(response.status(), StatusCode::OK);
        }
        assert_eq!(registry.size().unwrap(), 1, "an exact retry adds no leaf");
        assert_eq!(
            directory.entry("https://loft.example").unwrap().state,
            LoftState::Pending
        );
        assert!(directory.verify_registry_logging_ready().is_ok());
        directory
            .set_state("https://loft.example", LoftState::Active)
            .unwrap();

        let drain =
            DrainAuthorization::signed(&loft_key, "https://loft.example", now_secs() + 3_600, 2);
        let response = http
            .post(format!("{directory_url}/v1/directory/drain"))
            .json(&drain)
            .send()
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::OK);
        assert_eq!(registry.size().unwrap(), 2);
        assert_eq!(
            directory.entry("https://loft.example").unwrap().state,
            LoftState::Draining
        );
        assert_eq!(directory.registry_checkpoint().unwrap().unwrap().size, 2);

        directory
            .record_probe(
                &crate::directory::ProbeResult {
                    endpoint: "https://loft.example".into(),
                    at: drain.after,
                    reachable: true,
                    stored_and_returned: true,
                    utilization: 0.1,
                    retention_age_secs: None,
                    retention_ok: None,
                    detail: None,
                },
                drain.after,
            )
            .unwrap();
        assert_eq!(
            directory.entry("https://loft.example").unwrap().state,
            LoftState::Removed
        );
        let retry = http
            .post(format!("{directory_url}/v1/directory/drain"))
            .json(&drain)
            .send()
            .await
            .unwrap();
        assert_eq!(retry.status(), StatusCode::OK);
        assert_eq!(
            registry.size().unwrap(),
            2,
            "a post-deadline exact drain retry must not append another leaf"
        );

        directory_task.abort();
        registry_task.abort();
    }

    #[tokio::test]
    async fn missing_witness_fails_closed_then_exact_retry_can_finish() {
        let registry = registry();
        let witness_key = SigningKey::from_bytes(&[42; 32]);
        let proxy = Arc::new(WitnessProxy {
            registry: Arc::clone(&registry),
            witness_name: "witness.test",
            witness_key: witness_key.clone(),
            cosign: AtomicBool::new(false),
            tamper_path: AtomicBool::new(false),
        });
        let (registry_url, registry_task) = spawn(proxy_router(Arc::clone(&proxy))).await;
        let log = Arc::new(
            RegistryLogClient::with_witness_wait(
                &registry_url,
                trust(&registry, &witness_key),
                Duration::from_millis(450),
            )
            .unwrap(),
        );
        let directory = Arc::new(Directory::in_memory().unwrap());
        let (directory_url, directory_task) = spawn(server::router_with_registry_log(
            Arc::clone(&directory),
            log,
        ))
        .await;
        let submission = entry(&SigningKey::from_bytes(&[9; 32]));
        let http = Client::new();
        let request = || {
            http.post(format!("{directory_url}/v1/directory/submit"))
                .json(&serde_json::json!({ "entry": submission }))
        };

        assert_eq!(
            request().send().await.unwrap().status(),
            StatusCode::BAD_GATEWAY
        );
        assert!(matches!(
            directory.entry("https://loft.example"),
            Err(DirectoryError::NotFound)
        ));
        assert_eq!(registry.size().unwrap(), 1);

        let mut divergent = submission.clone();
        divergent.capacity_gb += 1;
        // Re-sign so this is a valid but different exact mutation at the reserved endpoint and
        // sequence, rather than merely a malformed signature.
        divergent = DirectoryEntry::signed_with_sequence(
            &SigningKey::from_bytes(&[9; 32]),
            "https://loft.example",
            None,
            divergent.capacity_gb,
            divergent.retention_days,
            divergent.policy,
            0.0,
            divergent.sequence,
        );
        let divergent_status = http
            .post(format!("{directory_url}/v1/directory/submit"))
            .json(&serde_json::json!({ "entry": divergent }))
            .send()
            .await
            .unwrap()
            .status();
        assert!(matches!(
            divergent_status,
            StatusCode::BAD_GATEWAY | StatusCode::SERVICE_UNAVAILABLE
        ));
        assert_eq!(registry.size().unwrap(), 1);
        assert!(directory.has_pending_mutations().unwrap());

        proxy.cosign.store(true, Ordering::SeqCst);
        assert_eq!(request().send().await.unwrap().status(), StatusCode::OK);
        assert!(directory.entry("https://loft.example").is_ok());
        assert_eq!(registry.size().unwrap(), 1);

        directory_task.abort();
        registry_task.abort();
    }

    #[tokio::test]
    async fn unconfigured_or_tampered_transparency_never_changes_local_state() {
        let directory = Arc::new(Directory::in_memory().unwrap());
        let (plain_url, plain_task) = spawn(server::router(Arc::clone(&directory))).await;
        let submission = entry(&SigningKey::from_bytes(&[10; 32]));
        let http = Client::new();
        assert_eq!(
            http.post(format!("{plain_url}/v1/directory/submit"))
                .json(&serde_json::json!({ "entry": submission }))
                .send()
                .await
                .unwrap()
                .status(),
            StatusCode::SERVICE_UNAVAILABLE
        );
        assert!(directory.entries().unwrap().is_empty());
        plain_task.abort();

        let registry = registry();
        let witness_key = SigningKey::from_bytes(&[42; 32]);
        let proxy = Arc::new(WitnessProxy {
            registry: Arc::clone(&registry),
            witness_name: "witness.test",
            witness_key: witness_key.clone(),
            cosign: AtomicBool::new(true),
            tamper_path: AtomicBool::new(true),
        });
        let (registry_url, registry_task) = spawn(proxy_router(proxy)).await;
        let log = Arc::new(
            RegistryLogClient::with_witness_wait(
                &registry_url,
                trust(&registry, &witness_key),
                Duration::from_millis(450),
            )
            .unwrap(),
        );
        let directory = Arc::new(Directory::in_memory().unwrap());
        let (directory_url, directory_task) = spawn(server::router_with_registry_log(
            Arc::clone(&directory),
            log,
        ))
        .await;
        assert_eq!(
            http.post(format!("{directory_url}/v1/directory/submit"))
                .json(&serde_json::json!({ "entry": submission }))
                .send()
                .await
                .unwrap()
                .status(),
            StatusCode::BAD_GATEWAY
        );
        assert_eq!(
            registry.size().unwrap(),
            1,
            "the exact 502 must come from rejecting the tampered published proof"
        );
        assert!(directory.entries().unwrap().is_empty());

        directory_task.abort();
        registry_task.abort();
    }
}
