//! Offline `ppcompliance` operator surface.
//!
//! The binary deliberately has no network client. Production approvals, custody agreement, and
//! destruction are capabilities exposed by bounded local subprocess adapters using exact binary
//! stdin/stdout protocols. The subprocess executable and approver roster are pinned in a private
//! offline configuration; no shell is involved and stderr is discarded.

use std::collections::HashMap;
use std::env;
use std::ffi::OsString;
use std::fs::File;
#[cfg(any(not(unix), test))]
use std::fs::{self, OpenOptions};
use std::io::{BufRead, BufReader, Read, Write};
use std::net::IpAddr;
use std::path::{Path, PathBuf};
use std::process::{Command as ProcessCommand, Stdio};
#[cfg(not(unix))]
use std::sync::mpsc;
use std::thread;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use ed25519_dalek::{SigningKey, VerifyingKey};
use fs2::FileExt;
use pigeonpost_compliance_format::{
    compliance_epoch_end_ms, validate_compliance_epoch, validate_trace_epoch, ComplianceKeyId,
    CompliancePurpose, Jurisdiction, COMPLIANCE_KEY_ID_LEN,
};
use pigeonpost_compliance_seal::{IdentityProvider, NetworkOperation, TraceIp};
use pigeonpost_core::envelope::AttributionBlock;
use pigeonpost_core::keys;
use pigeonpost_core::Wrap;
use pigeonpost_registry::{
    verify_consistency, witness_quorum_intersects, Checkpoint, ComplianceKeyPublish,
    ComplianceKeyStatus, LogEntry, MerkleFrontier, WitnessKey,
};
#[cfg(unix)]
use pigeonpost_unix_custody::{
    CustodyError, DirPolicy, FilePolicy, GuardedDir, GuardedFile, LeafName, NormalizedPath,
    OpenAccess,
};
use rand_core::{OsRng, RngCore};
use serde::de::DeserializeOwned;
use serde::Deserialize;
use serde_json::json;
use sha2::{Digest, Sha256};
use zeroize::{Zeroize, Zeroizing};

use crate::ledger::disclosure_state_path;
use crate::retention::{LegalHoldApproval, LegalHoldAuthorization};
use crate::trace_epoch::TraceBodyIntegrity;
use crate::{
    custody::disclose_trace_segment_selected_bounded, seal_private_audit_record,
    seal_private_audit_record_with_terminal_manifest, unseal_attribution, AuthenticatedTraceEpoch,
    ComplianceError, CopyState, CustodyBackend, DestructionInventory, DisclosedTraceRecord,
    DisclosureLedger, DisclosureOutput, DisclosureRequest, InventoryState, KeyCopy, KeyCopyKind,
    PrivateAuditKey, PrivateAuditMaterial, RetentionPolicy, SensitiveRequestMaterial,
    SoftwareCustodyKey, TraceEpochExpectation, TraceIntegrityEvidence, TraceIntegrityStatus,
};

const CONFIG_VERSION: u8 = 2;
const CONFIG_MAX_BYTES: u64 = 256 * 1024;
const SECRET_KEY_BYTES: u64 = 32;
const WRAP_MAX_BYTES: u64 = 1024 * 1024;
const INVENTORY_MAX_BYTES: u64 = 128 * 1024;
const INVENTORY_DECLARATION_MAX_BYTES: u64 = 256 * 1024;
const INVENTORY_DECLARATION_VERSION: u8 = 1;
const PRIVATE_REQUEST_MAX_BYTES: u64 = 32 * 1024;
const PRIVATE_REQUEST_VERSION: u8 = 1;
const RETENTION_POLICY_CONFIG_VERSION: u8 = 1;
const MAX_DECLARED_COPIES: usize = 64;
const MAX_EPOCHS: usize = 4096;
const MAX_ATTRIBUTION_ARTIFACTS_PER_EPOCH: usize = 64;
const MAX_COMMAND_ARGS: usize = 32;
const MAX_COMMAND_ARG_BYTES: usize = 4096;
const MAX_ENVIRONMENT_KEYS: usize = 16;
const MIN_COMMAND_TIMEOUT_MS: u64 = 100;
const MAX_COMMAND_TIMEOUT_MS: u64 = 5 * 60 * 1_000;
const MAX_APPROVAL_TTL_MS: u64 = 60 * 60 * 1_000;
const MAX_PRIVATE_FIELD_BYTES: usize = 4 * 1024;
const MAX_DISCLOSED_RECORDS: usize = 1_000;
const MAX_DISCLOSURE_OUTPUT_BYTES: usize = 4 * 1024 * 1024;
const MAX_TOTAL_ATTRIBUTION_ARTIFACT_BYTES: u64 = 64 * 1024 * 1024;
const DISCLOSURE_CHECKPOINT_MAX_BYTES: u64 = 64 * 1024;
const REGISTRY_CHECKPOINT_MAX_BYTES: u64 = 64 * 1024;
const REGISTRY_AUDIT_MAX_BYTES: u64 = 512 * 1024 * 1024;
const REGISTRY_AUDIT_ENTRY_MAX_BYTES: u64 = 64 * 1024;
const MAX_REGISTRY_AUDIT_ENTRIES: u64 = 1_000_000;
const MAX_REGISTRY_AUDIT_KEYS: usize = 4_096;
const MAX_REGISTRY_WITNESSES: usize = 32;
const APPROVAL_REQUEST_MAGIC: &[u8; 8] = b"PPAPREQ\0";
const APPROVAL_RESPONSE_MAGIC: &[u8; 8] = b"PPAPRES\0";
const CUSTODY_REQUEST_MAGIC: &[u8; 8] = b"PPCUSTQ\0";
const CUSTODY_RESPONSE_MAGIC: &[u8; 8] = b"PPCUSTR\0";
const DESTRUCTION_REQUEST_MAGIC: &[u8; 8] = b"PPSHRED\0";
const DESTRUCTION_RESPONSE_MAGIC: &[u8; 8] = b"PPSHRES\0";
const ADAPTER_PROTOCOL_VERSION: u8 = 1;

/// Coarse operator failures. Underlying paths, process errors, case data, and custody responses
/// are intentionally never included in the display form.
#[derive(Debug, thiserror::Error, Clone, Copy, PartialEq, Eq)]
pub enum OperatorError {
    #[error("offline compliance operations are unsupported on this platform")]
    UnsupportedPlatform,
    #[error("invalid command usage")]
    Usage,
    #[error("offline compliance configuration is invalid")]
    Configuration,
    #[error("offline compliance storage failed")]
    Storage,
    #[error("two-person authorization was refused")]
    Authorization,
    #[error("custody operation was refused")]
    Custody,
    #[error("configured compliance artifact is invalid")]
    Artifact,
    #[error("compliance state blocks this operation")]
    State,
    #[error("compliance operation exceeded a safety bound")]
    Limit,
}

type OperatorResult<T> = core::result::Result<T, OperatorError>;

impl From<ComplianceError> for OperatorError {
    fn from(value: ComplianceError) -> Self {
        match value {
            ComplianceError::UnsupportedPlatform => Self::UnsupportedPlatform,
            ComplianceError::Unauthorized
            | ComplianceError::BadApproval
            | ComplianceError::Expired => Self::Authorization,
            ComplianceError::Storage => Self::Storage,
            ComplianceError::LimitExceeded => Self::Limit,
            ComplianceError::SegmentInvalid | ComplianceError::AttributionInvalid => Self::Artifact,
            ComplianceError::RetentionActive
            | ComplianceError::LegalHoldActive
            | ComplianceError::IncompleteInventory
            | ComplianceError::UnknownCopy
            | ComplianceError::StateConflict => Self::State,
            _ => Self::Custody,
        }
    }
}

#[derive(Clone, Deserialize)]
#[serde(deny_unknown_fields)]
struct CommandConfig {
    executable: PathBuf,
    #[serde(default)]
    args: Vec<String>,
    timeout_ms: u64,
    #[serde(default)]
    inherit_environment: Vec<String>,
}

#[derive(Clone, Deserialize)]
#[serde(deny_unknown_fields)]
struct ApproverConfig {
    public_key: String,
    identity: String,
}

#[derive(Clone, Deserialize)]
#[serde(deny_unknown_fields)]
struct ApprovalConfig {
    command: CommandConfig,
    request_ttl_ms: u64,
    approvers: Vec<ApproverConfig>,
}

#[derive(Debug, Clone, Copy, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
enum ArtifactKind {
    TraceSegments,
    AttributionWraps,
}

#[derive(Clone, Deserialize)]
#[serde(deny_unknown_fields)]
struct ArtifactConfig {
    kind: ArtifactKind,
    #[serde(default)]
    paths: Vec<PathBuf>,
    /// Dedicated directory containing exactly one terminal manifest and its declared trace files.
    directory: Option<PathBuf>,
    /// Pinned producer identity for trace plaintext. Required only for trace segments.
    expected_node_id: Option<String>,
    /// Pinned Ed25519 segment signer. Required only for trace segments.
    expected_signer_public_key: Option<String>,
    /// Independently provisioned SHA-256 digest of the trace custody public key.
    expected_custody_key_digest: Option<String>,
}

#[derive(Debug, Clone, Copy, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
enum CustodyMode {
    External,
    SoftwareDevelopment,
}

#[derive(Clone, Deserialize)]
#[serde(deny_unknown_fields)]
struct CustodyConfig {
    mode: CustodyMode,
    public_key: Option<String>,
    command: Option<CommandConfig>,
    secret_key_path: Option<PathBuf>,
}

#[derive(Clone, Deserialize)]
#[serde(deny_unknown_fields)]
struct RetentionPolicyConfig {
    version: u8,
    tr_days: u16,
    counsel_approval_commitment: String,
}

#[derive(Clone, Deserialize)]
#[serde(deny_unknown_fields)]
struct EpochConfig {
    key_id: String,
    inventory_path: PathBuf,
    inventory_declaration_path: PathBuf,
    inventory_staging_path: PathBuf,
    inventory_import_path: PathBuf,
    retention_policy: RetentionPolicyConfig,
    artifact: ArtifactConfig,
    custody: CustodyConfig,
}

#[derive(Clone, Deserialize)]
#[serde(deny_unknown_fields)]
struct RegistryWitnessConfig {
    name: String,
    public_key: String,
}

#[derive(Clone, Deserialize)]
#[serde(deny_unknown_fields)]
struct RegistryAuditConfig {
    log_path: PathBuf,
    checkpoint_path: PathBuf,
    expected_origin: String,
    checkpoint_key: String,
    witnesses: Vec<RegistryWitnessConfig>,
    witness_threshold: usize,
    minimum_checkpoint_size: u64,
    minimum_checkpoint_root: String,
    max_cosignature_age_seconds: u64,
    future_clock_skew_seconds: u64,
}

#[derive(Clone, Deserialize)]
#[serde(deny_unknown_fields)]
struct OperatorConfig {
    version: u8,
    ledger_path: PathBuf,
    private_audit_directory: PathBuf,
    private_audit_key_path: PathBuf,
    checkpoint_origin: String,
    checkpoint_signing_key_path: PathBuf,
    /// Owner-only handoff file consumed by the separately provisioned public checkpoint publisher.
    checkpoint_output_path: PathBuf,
    registry_audit: RegistryAuditConfig,
    approval: ApprovalConfig,
    destruction_command: Option<CommandConfig>,
    epochs: Vec<EpochConfig>,
}

impl OperatorConfig {
    fn load(home: &Path) -> OperatorResult<Self> {
        if !home.is_absolute() {
            return Err(OperatorError::Configuration);
        }
        validate_private_directory(home, false)?;
        let bytes = read_private_file(&home.join("config.toml"), CONFIG_MAX_BYTES)?;
        let text = core::str::from_utf8(&bytes).map_err(|_| OperatorError::Configuration)?;
        let config: Self = toml::from_str(text).map_err(|_| OperatorError::Configuration)?;
        config.validate()?;
        config.validate_inventory_roots(home)?;
        Ok(config)
    }

    fn validate_inventory_roots(&self, home: &Path) -> OperatorResult<()> {
        validate_absolute_path(home)?;
        #[cfg(unix)]
        let canonical_home = GuardedDir::open_existing(home, DirPolicy::private())
            .map_err(map_operator_custody)?
            .absolute_path()
            .to_path_buf();
        #[cfg(not(unix))]
        let canonical_home = fs::canonicalize(home).map_err(|_| OperatorError::Configuration)?;
        let ledger_state_path = disclosure_state_path(&self.ledger_path)?;
        let mut occupied = vec![
            canonical_home.join("config.toml"),
            canonical_home.join("operator.lock"),
        ];
        let mut reserved = vec![
            &self.ledger_path,
            &ledger_state_path,
            &self.private_audit_directory,
            &self.private_audit_key_path,
            &self.checkpoint_signing_key_path,
            &self.checkpoint_output_path,
            &self.registry_audit.log_path,
            &self.registry_audit.checkpoint_path,
            &self.approval.command.executable,
        ];
        if let Some(command) = &self.destruction_command {
            reserved.push(&command.executable);
        }
        for epoch in &self.epochs {
            reserved.extend(epoch.artifact.paths.iter());
            if let Some(directory) = &epoch.artifact.directory {
                reserved.push(directory);
            }
            if let Some(path) = &epoch.custody.secret_key_path {
                reserved.push(path);
            }
            if let Some(command) = &epoch.custody.command {
                reserved.push(&command.executable);
            }
        }
        for path in reserved {
            if let Ok(target) = canonical_named_path(path) {
                occupied.push(target);
            }
        }

        for epoch in &self.epochs {
            for path in [
                &epoch.inventory_path,
                &epoch.inventory_declaration_path,
                &epoch.inventory_staging_path,
                &epoch.inventory_import_path,
            ] {
                let parent = path.parent().ok_or(OperatorError::Configuration)?;
                validate_private_directory(parent, false)
                    .map_err(|_| OperatorError::Configuration)?;
                let target = canonical_named_path(path)?;
                if target == canonical_home
                    || !target.starts_with(&canonical_home)
                    || occupied.contains(&target)
                {
                    return Err(OperatorError::Configuration);
                }
                occupied.push(target);
            }
        }
        Ok(())
    }

    fn validate(&self) -> OperatorResult<()> {
        if self.version != CONFIG_VERSION
            || self.epochs.is_empty()
            || self.epochs.len() > MAX_EPOCHS
            || self.checkpoint_origin.is_empty()
            || self.checkpoint_origin.len() > 256
            || self
                .checkpoint_origin
                .bytes()
                .any(|byte| byte.is_ascii_control())
            || self.approval.request_ttl_ms == 0
            || self.approval.request_ttl_ms > MAX_APPROVAL_TTL_MS
            || self.approval.approvers.len() < 2
            || self.approval.approvers.len() > 32
        {
            return Err(OperatorError::Configuration);
        }
        let ledger_state_path = disclosure_state_path(&self.ledger_path)?;
        for path in [
            &self.ledger_path,
            &ledger_state_path,
            &self.private_audit_directory,
            &self.private_audit_key_path,
            &self.checkpoint_signing_key_path,
            &self.checkpoint_output_path,
            &self.registry_audit.log_path,
            &self.registry_audit.checkpoint_path,
        ] {
            validate_absolute_path(path)?;
        }
        self.registry_audit.validate()?;
        validate_command(&self.approval.command)?;
        if let Some(command) = &self.destruction_command {
            validate_command(command)?;
        }
        let mut reserved_paths = vec![
            self.ledger_path.clone(),
            ledger_state_path,
            self.private_audit_directory.clone(),
            self.private_audit_key_path.clone(),
            self.checkpoint_signing_key_path.clone(),
            self.checkpoint_output_path.clone(),
            self.registry_audit.log_path.clone(),
            self.registry_audit.checkpoint_path.clone(),
            self.approval.command.executable.clone(),
        ];
        if let Some(command) = &self.destruction_command {
            reserved_paths.push(command.executable.clone());
        }
        let mut distinct_reserved_paths = Vec::with_capacity(reserved_paths.len());
        for path in &reserved_paths {
            if distinct_reserved_paths.contains(path) {
                return Err(OperatorError::Configuration);
            }
            distinct_reserved_paths.push(path.clone());
        }
        let mut approver_keys = Vec::with_capacity(self.approval.approvers.len());
        for approver in &self.approval.approvers {
            let key = decode_hex_array::<32>(&approver.public_key)
                .map_err(|_| OperatorError::Configuration)?;
            keys::verifying_key_from_bytes(&key).map_err(|_| OperatorError::Configuration)?;
            if approver.identity.is_empty()
                || approver.identity.len() > MAX_PRIVATE_FIELD_BYTES
                || approver.identity.bytes().any(|byte| byte == 0)
                || approver_keys.contains(&key)
            {
                return Err(OperatorError::Configuration);
            }
            approver_keys.push(key);
        }
        let mut epoch_ids = Vec::with_capacity(self.epochs.len());
        for epoch in &self.epochs {
            let key_id = parse_key_id(&epoch.key_id)?;
            if compliance_epoch_end_ms(&key_id).is_err() || epoch_ids.contains(&key_id) {
                return Err(OperatorError::Configuration);
            }
            epoch_ids.push(key_id);
            let inventory_paths = [
                &epoch.inventory_path,
                &epoch.inventory_declaration_path,
                &epoch.inventory_staging_path,
                &epoch.inventory_import_path,
            ];
            for path in &inventory_paths {
                validate_absolute_path(path)?;
                if reserved_paths.contains(path) {
                    return Err(OperatorError::Configuration);
                }
                reserved_paths.push((*path).clone());
            }
            epoch.retention_policy.policy()?;
            match (key_id.purpose, epoch.artifact.kind) {
                (CompliancePurpose::Attribution, ArtifactKind::AttributionWraps)
                | (
                    CompliancePurpose::NetworkTrace | CompliancePurpose::IdentityTrace,
                    ArtifactKind::TraceSegments,
                ) => {}
                _ => return Err(OperatorError::Configuration),
            }
            match epoch.artifact.kind {
                ArtifactKind::TraceSegments => {
                    if !epoch.artifact.paths.is_empty() {
                        return Err(OperatorError::Configuration);
                    }
                    let directory = epoch
                        .artifact
                        .directory
                        .as_ref()
                        .ok_or(OperatorError::Configuration)?;
                    validate_absolute_path(directory)?;
                    if reserved_paths.contains(directory) {
                        return Err(OperatorError::Configuration);
                    }
                    reserved_paths.push(directory.clone());
                    let node_id = parse_canonical_hex32(
                        epoch
                            .artifact
                            .expected_node_id
                            .as_deref()
                            .ok_or(OperatorError::Configuration)?,
                    )?;
                    let signer = parse_canonical_hex32(
                        epoch
                            .artifact
                            .expected_signer_public_key
                            .as_deref()
                            .ok_or(OperatorError::Configuration)?,
                    )?;
                    let custody_digest = parse_canonical_hex32(
                        epoch
                            .artifact
                            .expected_custody_key_digest
                            .as_deref()
                            .ok_or(OperatorError::Configuration)?,
                    )?;
                    if node_id == [0u8; 32]
                        || custody_digest == [0u8; 32]
                        || keys::verifying_key_from_bytes(&signer)
                            .map_err(|_| OperatorError::Configuration)?
                            .is_weak()
                    {
                        return Err(OperatorError::Configuration);
                    }
                }
                ArtifactKind::AttributionWraps => {
                    if epoch.artifact.directory.is_some()
                        || epoch.artifact.paths.is_empty()
                        || epoch.artifact.paths.len() > MAX_ATTRIBUTION_ARTIFACTS_PER_EPOCH
                        || epoch.artifact.expected_node_id.is_some()
                        || epoch.artifact.expected_signer_public_key.is_some()
                        || epoch.artifact.expected_custody_key_digest.is_some()
                    {
                        return Err(OperatorError::Configuration);
                    }
                    let mut paths = Vec::with_capacity(epoch.artifact.paths.len());
                    for path in &epoch.artifact.paths {
                        validate_absolute_path(path)?;
                        if paths.contains(path) || reserved_paths.contains(path) {
                            return Err(OperatorError::Configuration);
                        }
                        paths.push(path.clone());
                        reserved_paths.push(path.clone());
                    }
                }
            }
            epoch.custody.validate(key_id)?;
            if epoch.artifact.kind == ArtifactKind::TraceSegments
                && epoch.custody.mode == CustodyMode::External
            {
                let public_key = parse_canonical_hex32(
                    epoch
                        .custody
                        .public_key
                        .as_deref()
                        .ok_or(OperatorError::Configuration)?,
                )?;
                let expected_digest = parse_canonical_hex32(
                    epoch
                        .artifact
                        .expected_custody_key_digest
                        .as_deref()
                        .ok_or(OperatorError::Configuration)?,
                )?;
                let actual_digest: [u8; 32] = Sha256::digest(public_key).into();
                if actual_digest != expected_digest {
                    return Err(OperatorError::Configuration);
                }
            }
            if epoch
                .custody
                .secret_key_path
                .as_ref()
                .is_some_and(|path| reserved_paths.contains(path))
                || epoch
                    .custody
                    .command
                    .as_ref()
                    .is_some_and(|command| reserved_paths.contains(&command.executable))
            {
                return Err(OperatorError::Configuration);
            }
            if let Some(path) = &epoch.custody.secret_key_path {
                reserved_paths.push(path.clone());
            }
            if let Some(command) = &epoch.custody.command {
                reserved_paths.push(command.executable.clone());
            }
        }
        Ok(())
    }

    fn epoch(&self, key_id: ComplianceKeyId) -> OperatorResult<&EpochConfig> {
        self.epochs
            .iter()
            .find(|epoch| parse_key_id(&epoch.key_id).ok() == Some(key_id))
            .ok_or(OperatorError::Configuration)
    }
}

impl RegistryAuditConfig {
    fn validate(&self) -> OperatorResult<()> {
        if self.expected_origin.is_empty()
            || self.expected_origin.len() > 256
            || self
                .expected_origin
                .bytes()
                .any(|byte| byte.is_ascii_control() || byte.is_ascii_whitespace())
            || self.witnesses.is_empty()
            || self.witnesses.len() > MAX_REGISTRY_WITNESSES
            || !witness_quorum_intersects(self.witness_threshold, self.witnesses.len())
            || self.max_cosignature_age_seconds == 0
            || self.future_clock_skew_seconds > self.max_cosignature_age_seconds
            || self.log_path == self.checkpoint_path
        {
            return Err(OperatorError::Configuration);
        }
        let checkpoint_key = parse_canonical_hex32(&self.checkpoint_key)?;
        keys::verifying_key_from_bytes(&checkpoint_key)
            .map_err(|_| OperatorError::Configuration)?;
        let minimum_root = parse_canonical_hex32(&self.minimum_checkpoint_root)?;
        let empty_root = pigeonpost_registry::log::empty_root();
        if (self.minimum_checkpoint_size == 0 && minimum_root != empty_root)
            || (self.minimum_checkpoint_size != 0 && minimum_root == empty_root)
        {
            return Err(OperatorError::Configuration);
        }
        let mut names = Vec::with_capacity(self.witnesses.len());
        let mut keys = Vec::with_capacity(self.witnesses.len());
        for witness in &self.witnesses {
            let key = parse_canonical_hex32(&witness.public_key)?;
            let verifying =
                keys::verifying_key_from_bytes(&key).map_err(|_| OperatorError::Configuration)?;
            WitnessKey::new(witness.name.clone(), verifying)
                .map_err(|_| OperatorError::Configuration)?;
            if key == checkpoint_key || names.contains(&witness.name) || keys.contains(&key) {
                return Err(OperatorError::Configuration);
            }
            names.push(witness.name.clone());
            keys.push(key);
        }
        Ok(())
    }

    fn checkpoint_key(&self) -> OperatorResult<VerifyingKey> {
        keys::verifying_key_from_bytes(&parse_canonical_hex32(&self.checkpoint_key)?)
            .map_err(|_| OperatorError::Configuration)
    }

    fn witness_keys(&self) -> OperatorResult<Vec<WitnessKey>> {
        self.witnesses
            .iter()
            .map(|witness| {
                let key =
                    keys::verifying_key_from_bytes(&parse_canonical_hex32(&witness.public_key)?)
                        .map_err(|_| OperatorError::Configuration)?;
                WitnessKey::new(witness.name.clone(), key).map_err(|_| OperatorError::Configuration)
            })
            .collect()
    }
}

impl CustodyConfig {
    fn validate(&self, key_id: ComplianceKeyId) -> OperatorResult<()> {
        match self.mode {
            CustodyMode::External => {
                let public_key = self
                    .public_key
                    .as_deref()
                    .ok_or(OperatorError::Configuration)?;
                let public_key = parse_canonical_hex32(public_key)?;
                if public_key == [0u8; 32]
                    || self.secret_key_path.is_some()
                    || self.command.is_none()
                {
                    return Err(OperatorError::Configuration);
                }
                validate_command(self.command.as_ref().expect("checked"))?;
            }
            CustodyMode::SoftwareDevelopment => {
                // A raw software custody secret is never accepted for a real jurisdiction.
                if key_id.jurisdiction != Jurisdiction::Test
                    || self.public_key.is_some()
                    || self.command.is_some()
                {
                    return Err(OperatorError::Configuration);
                }
                validate_absolute_path(
                    self.secret_key_path
                        .as_ref()
                        .ok_or(OperatorError::Configuration)?,
                )?;
            }
        }
        Ok(())
    }
}

impl RetentionPolicyConfig {
    fn policy(&self) -> OperatorResult<RetentionPolicy> {
        if self.version != RETENTION_POLICY_CONFIG_VERSION {
            return Err(OperatorError::Configuration);
        }
        let approval = parse_canonical_hex32(&self.counsel_approval_commitment)?;
        RetentionPolicy::new(self.tr_days, approval).map_err(|_| OperatorError::Configuration)
    }
}

fn validate_command(config: &CommandConfig) -> OperatorResult<()> {
    validate_absolute_path(&config.executable)?;
    if config.args.len() > MAX_COMMAND_ARGS
        || config
            .args
            .iter()
            .any(|arg| arg.len() > MAX_COMMAND_ARG_BYTES || arg.as_bytes().contains(&0))
        || !(MIN_COMMAND_TIMEOUT_MS..=MAX_COMMAND_TIMEOUT_MS).contains(&config.timeout_ms)
        || config.inherit_environment.len() > MAX_ENVIRONMENT_KEYS
    {
        return Err(OperatorError::Configuration);
    }
    let mut seen = Vec::new();
    for key in &config.inherit_environment {
        if key.is_empty()
            || key.len() > 128
            || !key
                .bytes()
                .all(|byte| byte.is_ascii_uppercase() || byte.is_ascii_digit() || byte == b'_')
            || seen.contains(key)
        {
            return Err(OperatorError::Configuration);
        }
        seen.push(key.clone());
    }
    validate_executable(&config.executable)?;
    Ok(())
}

fn validate_executable(path: &Path) -> OperatorResult<()> {
    drop(open_validated_executable(path)?);
    Ok(())
}

struct ValidatedExecutable {
    file: File,
    #[cfg(unix)]
    guard: GuardedFile,
    #[cfg(not(unix))]
    path: PathBuf,
}

impl ValidatedExecutable {
    fn verify_named(&self) -> OperatorResult<()> {
        #[cfg(unix)]
        {
            self.file
                .metadata()
                .map_err(|_| OperatorError::Configuration)?;
            self.guard.verify_named().map_err(map_operator_custody)
        }
        #[cfg(not(unix))]
        if named_file_matches(&self.file, &self.path) {
            Ok(())
        } else {
            Err(OperatorError::Configuration)
        }
    }
}

fn open_validated_executable(path: &Path) -> OperatorResult<ValidatedExecutable> {
    #[cfg(unix)]
    let (file, guard) = {
        let guard = open_guarded_regular(path, false, u64::MAX, false)
            .map_err(|_| OperatorError::Configuration)?;
        let file = guard
            .file()
            .try_clone()
            .map_err(|_| OperatorError::Configuration)?;
        (file, guard)
    };
    #[cfg(not(unix))]
    let file = open_existing_regular(path, false, u64::MAX, false)
        .map_err(|_| OperatorError::Configuration)?;
    #[cfg(unix)]
    let metadata = file.metadata().map_err(|_| OperatorError::Configuration)?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::{MetadataExt, PermissionsExt};

        let mode = metadata.permissions().mode();
        let owner = metadata.uid();
        if mode & 0o111 == 0
            || mode & 0o022 != 0
            || (owner != 0 && owner != rustix::process::geteuid().as_raw())
            || (owner != 0 && metadata.nlink() != 1)
        {
            return Err(OperatorError::Configuration);
        }

        guard
            .verify_named()
            .map_err(|_| OperatorError::Configuration)?;
    }
    Ok(ValidatedExecutable {
        file,
        #[cfg(unix)]
        guard,
        #[cfg(not(unix))]
        path: path.to_owned(),
    })
}

trait CommandRunner {
    fn run(
        &self,
        config: &CommandConfig,
        input: &[u8],
        max_output: usize,
    ) -> OperatorResult<Vec<u8>>;

    fn completion_time_ms(&self) -> OperatorResult<u64> {
        now_ms()
    }
}

#[derive(Debug, Default)]
struct ProcessRunner;

impl CommandRunner for ProcessRunner {
    fn run(
        &self,
        config: &CommandConfig,
        input: &[u8],
        max_output: usize,
    ) -> OperatorResult<Vec<u8>> {
        validate_command(config)?;
        let executable = open_validated_executable(&config.executable)?;
        let mut command = ProcessCommand::new(&config.executable);
        command
            .args(&config.args)
            .env_clear()
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::null());
        #[cfg(unix)]
        {
            use std::os::unix::process::CommandExt;
            // A dedicated process group lets the deadline terminate helpers forked by an adapter,
            // not merely the immediate child returned by `spawn`.
            command.process_group(0);
        }
        for key in &config.inherit_environment {
            if let Some(value) = env::var_os(key) {
                command.env(key, value);
            }
        }
        executable.verify_named()?;
        let mut child = command.spawn().map_err(|_| OperatorError::Custody)?;
        if executable.verify_named().is_err() {
            terminate_adapter_tree(&mut child);
            return Err(OperatorError::Custody);
        }
        let Some(stdout) = child.stdout.take() else {
            terminate_adapter_tree(&mut child);
            return Err(OperatorError::Custody);
        };
        let Some(stdin) = child.stdin.take() else {
            terminate_adapter_tree(&mut child);
            return Err(OperatorError::Custody);
        };

        #[cfg(unix)]
        {
            collect_adapter_output_unix(
                &mut child,
                stdin,
                stdout,
                input,
                config.timeout_ms,
                max_output,
            )
        }

        #[cfg(not(unix))]
        {
            let mut stdin = stdin;
            if stdin.write_all(input).is_err() {
                terminate_adapter_tree(&mut child);
                return Err(OperatorError::Custody);
            }
            drop(stdin);
            collect_adapter_output_threaded(&mut child, stdout, config.timeout_ms, max_output)
        }
    }
}

/// Read the pipe non-blockingly so even a helper that detaches from the adapter process group and
/// retains stdout cannot strand an unbounded reader thread after the deadline.
#[cfg(unix)]
fn collect_adapter_output_unix(
    child: &mut std::process::Child,
    stdin: std::process::ChildStdin,
    mut stdout: std::process::ChildStdout,
    input: &[u8],
    timeout_ms: u64,
    max_output: usize,
) -> OperatorResult<Vec<u8>> {
    use rustix::fs::OFlags;

    let nonblocking = (|| {
        let flags = rustix::fs::fcntl_getfl(&stdout)?;
        rustix::fs::fcntl_setfl(&stdout, flags | OFlags::NONBLOCK)?;
        let flags = rustix::fs::fcntl_getfl(&stdin)?;
        rustix::fs::fcntl_setfl(&stdin, flags | OFlags::NONBLOCK)
    })();
    if nonblocking.is_err() {
        terminate_adapter_tree(child);
        return Err(OperatorError::Custody);
    }
    let deadline = Instant::now() + Duration::from_millis(timeout_ms);
    let mut status = None;
    let mut output = Zeroizing::new(Vec::with_capacity(max_output.min(8 * 1024)));
    let mut input_offset = 0usize;
    let mut stdin = Some(stdin);
    let mut eof = false;
    let mut descendants_terminated = false;
    let mut buffer = [0u8; 8 * 1024];
    loop {
        while stdin.is_some() && input_offset < input.len() {
            match stdin
                .as_mut()
                .expect("checked as present")
                .write(&input[input_offset..])
            {
                Ok(0) => {
                    terminate_adapter_tree(child);
                    return Err(OperatorError::Custody);
                }
                Ok(written) => input_offset += written,
                Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => break,
                Err(error) if error.kind() == std::io::ErrorKind::Interrupted => continue,
                Err(_) => {
                    terminate_adapter_tree(child);
                    return Err(OperatorError::Custody);
                }
            }
        }
        if stdin.is_some() && input_offset == input.len() {
            drop(stdin.take());
        }
        while !eof {
            let remaining = max_output.saturating_add(1).saturating_sub(output.len());
            if remaining == 0 {
                terminate_adapter_tree(child);
                return Err(OperatorError::Custody);
            }
            let read_limit = remaining.min(buffer.len());
            match stdout.read(&mut buffer[..read_limit]) {
                Ok(0) => eof = true,
                Ok(read) => {
                    output.extend_from_slice(&buffer[..read]);
                    if output.len() > max_output {
                        terminate_adapter_tree(child);
                        return Err(OperatorError::Custody);
                    }
                }
                Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => break,
                Err(_) => {
                    terminate_adapter_tree(child);
                    return Err(OperatorError::Custody);
                }
            }
        }
        if status.is_none() {
            status = match child.try_wait() {
                Ok(status) => status,
                Err(_) => {
                    terminate_adapter_tree(child);
                    return Err(OperatorError::Custody);
                }
            };
        }
        if status.is_some() && input_offset != input.len() {
            terminate_adapter_descendants(child);
            return Err(OperatorError::Custody);
        }
        if status.is_some() && !eof && !descendants_terminated {
            terminate_adapter_descendants(child);
            descendants_terminated = true;
        }
        if status.is_some() && eof {
            break;
        }
        if Instant::now() >= deadline {
            terminate_adapter_tree(child);
            return Err(OperatorError::Custody);
        }
        thread::sleep(Duration::from_millis(10));
    }
    let status = status.ok_or(OperatorError::Custody)?;
    if !status.success() {
        return Err(OperatorError::Custody);
    }
    Ok(core::mem::take(&mut *output))
}

#[cfg(not(unix))]
fn collect_adapter_output_threaded(
    child: &mut std::process::Child,
    stdout: std::process::ChildStdout,
    timeout_ms: u64,
    max_output: usize,
) -> OperatorResult<Vec<u8>> {
    let (reader_tx, reader_rx) = mpsc::sync_channel(1);
    let reader = thread::spawn(move || {
        let mut bytes = Zeroizing::new(Vec::new());
        let result = stdout
            .take(max_output.saturating_add(1) as u64)
            .read_to_end(&mut bytes)
            .map(|_| bytes);
        let _ = reader_tx.send(result);
    });
    let deadline = Instant::now() + Duration::from_millis(timeout_ms);
    let mut status = None;
    let mut output = None;
    loop {
        if status.is_none() {
            status = child.try_wait().map_err(|_| OperatorError::Custody)?;
        }
        if output.is_none() {
            match reader_rx.try_recv() {
                Ok(result) => output = Some(result.map_err(|_| OperatorError::Custody)?),
                Err(mpsc::TryRecvError::Empty) => {}
                Err(mpsc::TryRecvError::Disconnected) => {
                    terminate_adapter_tree(child);
                    let _ = reader.join();
                    return Err(OperatorError::Custody);
                }
            }
        }
        if status.is_some() && output.is_some() {
            break;
        }
        if Instant::now() >= deadline {
            terminate_adapter_tree(child);
            let _ = reader.join();
            return Err(OperatorError::Custody);
        }
        thread::sleep(Duration::from_millis(10));
    }
    terminate_adapter_descendants(child);
    reader.join().map_err(|_| OperatorError::Custody)?;
    let status = status.ok_or(OperatorError::Custody)?;
    let mut output = output.ok_or(OperatorError::Custody)?;
    if !status.success() || output.len() > max_output {
        return Err(OperatorError::Custody);
    }
    Ok(core::mem::take(&mut *output))
}

fn terminate_adapter_tree(child: &mut std::process::Child) {
    #[cfg(unix)]
    terminate_adapter_descendants(child);
    let _ = child.kill();
    let _ = child.wait();
}

#[cfg(unix)]
fn terminate_adapter_descendants(child: &std::process::Child) {
    if let Some(group) = rustix::process::Pid::from_raw(child.id() as i32) {
        let _ = rustix::process::kill_process_group(group, rustix::process::Signal::KILL);
    }
}

#[cfg(not(unix))]
fn terminate_adapter_descendants(_child: &std::process::Child) {}

struct ProcessCustodyBackend<'a> {
    purpose: CompliancePurpose,
    jurisdiction: Jurisdiction,
    key_id: ComplianceKeyId,
    public_key: [u8; 32],
    request_id: [u8; 32],
    command: &'a CommandConfig,
    runner: &'a dyn CommandRunner,
}

impl CustodyBackend for ProcessCustodyBackend<'_> {
    fn purpose(&self) -> CompliancePurpose {
        self.purpose
    }

    fn jurisdiction(&self) -> Jurisdiction {
        self.jurisdiction
    }

    fn public_key(&self) -> [u8; 32] {
        self.public_key
    }

    fn agree(&self, peer_public_key: &[u8; 32]) -> crate::Result<[u8; 32]> {
        let mut input = Zeroizing::new(Vec::with_capacity(153));
        input.extend_from_slice(CUSTODY_REQUEST_MAGIC);
        input.push(ADAPTER_PROTOCOL_VERSION);
        input.extend_from_slice(&self.request_id);
        input.extend_from_slice(
            &self
                .key_id
                .encode()
                .map_err(|_| ComplianceError::InvalidRequest)?,
        );
        input.extend_from_slice(&self.public_key);
        input.extend_from_slice(peer_public_key);
        let output = Zeroizing::new(
            self.runner
                .run(self.command, &input, CUSTODY_RESPONSE_MAGIC.len() + 1 + 32)
                .map_err(|_| ComplianceError::Crypto)?,
        );
        if output.len() != CUSTODY_RESPONSE_MAGIC.len() + 1 + 32
            || &output[..8] != CUSTODY_RESPONSE_MAGIC
            || output[8] != ADAPTER_PROTOCOL_VERSION
        {
            return Err(ComplianceError::Crypto);
        }
        let shared: [u8; 32] = output[9..].try_into().expect("checked exact length");
        if shared == [0u8; 32] {
            return Err(ComplianceError::InvalidKey);
        }
        Ok(shared)
    }
}

impl core::fmt::Debug for ProcessCustodyBackend<'_> {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        f.debug_struct("ProcessCustodyBackend")
            .field("purpose", &self.purpose)
            .field("jurisdiction", &self.jurisdiction)
            .field("key_id", &self.key_id)
            .field("adapter", &"<withheld>")
            .finish()
    }
}

enum RuntimeCustody<'a> {
    External(ProcessCustodyBackend<'a>),
    Software(SoftwareCustodyKey),
}

impl CustodyBackend for RuntimeCustody<'_> {
    fn purpose(&self) -> CompliancePurpose {
        match self {
            Self::External(value) => value.purpose(),
            Self::Software(value) => value.purpose(),
        }
    }

    fn jurisdiction(&self) -> Jurisdiction {
        match self {
            Self::External(value) => value.jurisdiction(),
            Self::Software(value) => value.jurisdiction(),
        }
    }

    fn public_key(&self) -> [u8; 32] {
        match self {
            Self::External(value) => value.public_key(),
            Self::Software(value) => value.public_key(),
        }
    }

    fn agree(&self, peer_public_key: &[u8; 32]) -> crate::Result<[u8; 32]> {
        match self {
            Self::External(value) => value.agree(peer_public_key),
            Self::Software(value) => value.agree(peer_public_key),
        }
    }
}

impl core::fmt::Debug for RuntimeCustody<'_> {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        match self {
            Self::External(_) => f.write_str("RuntimeCustody::External(<withheld>)"),
            Self::Software(_) => f.write_str("RuntimeCustody::SoftwareDevelopment(<withheld>)"),
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum InventoryAction {
    Create,
    Provision,
    Import,
    Update,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct InventoryDeclaration {
    version: u8,
    key_id: String,
    created_at_ms: u64,
    copies: Vec<InventoryCopyDeclaration>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct InventoryCopyDeclaration {
    kind: DeclaredCopyKind,
    state: DeclaredCopyState,
    nonce: String,
    private_material: SensitiveString,
}

#[derive(Debug, Clone, Copy, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
enum DeclaredCopyKind {
    LiveMetadata,
    SqliteWal,
    Sidecar,
    Snapshot,
    Backup,
    KmsVersion,
    ShamirShare,
}

impl From<DeclaredCopyKind> for KeyCopyKind {
    fn from(value: DeclaredCopyKind) -> Self {
        match value {
            DeclaredCopyKind::LiveMetadata => Self::LiveMetadata,
            DeclaredCopyKind::SqliteWal => Self::SqliteWal,
            DeclaredCopyKind::Sidecar => Self::Sidecar,
            DeclaredCopyKind::Snapshot => Self::Snapshot,
            DeclaredCopyKind::Backup => Self::Backup,
            DeclaredCopyKind::KmsVersion => Self::KmsVersion,
            DeclaredCopyKind::ShamirShare => Self::ShamirShare,
        }
    }
}

#[derive(Debug, Clone, Copy, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
enum DeclaredCopyState {
    Present,
    VerifiedAbsent,
}

#[derive(Deserialize)]
#[serde(transparent)]
struct SensitiveString(String);

impl SensitiveString {
    fn as_bytes(&self) -> &[u8] {
        self.0.as_bytes()
    }

    fn into_zeroizing(mut self) -> Zeroizing<String> {
        Zeroizing::new(core::mem::take(&mut self.0))
    }
}

impl Drop for SensitiveString {
    fn drop(&mut self) {
        self.0.zeroize();
    }
}

enum HoldAction {
    Place {
        until: String,
    },
    Renew {
        prior_hold_id: [u8; 32],
        until: String,
    },
    Release {
        hold_id: [u8; 32],
    },
}

enum ParsedCommand {
    Help,
    Version,
    Status,
    Unseal {
        epoch: String,
        order_reference: Zeroizing<String>,
        selectors: Vec<Zeroizing<String>>,
    },
    UnsealRequest {
        epoch: String,
    },
    Shred {
        before: String,
        execute: bool,
    },
    Hold {
        epoch: String,
        order_reference: Zeroizing<String>,
        action: HoldAction,
    },
    HoldRequest {
        epoch: String,
        action: HoldAction,
    },
    Inventory {
        action: InventoryAction,
        epoch: String,
    },
    Checkpoint,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct UnsealRequestDeclaration {
    version: u8,
    order_reference: SensitiveString,
    requester_identity: SensitiveString,
    selectors: Vec<SensitiveString>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct HoldRequestDeclaration {
    version: u8,
    order_reference: SensitiveString,
}

struct SelectorPair {
    key: String,
    value: String,
}

impl Drop for SelectorPair {
    fn drop(&mut self) {
        self.key.zeroize();
        self.value.zeroize();
    }
}

struct SelectorSet {
    pairs: Vec<SelectorPair>,
    canonical: Zeroizing<Vec<u8>>,
}

impl SelectorSet {
    fn parse(values: Vec<Zeroizing<String>>, purpose: CompliancePurpose) -> OperatorResult<Self> {
        if values.is_empty() || values.len() > 8 {
            return Err(OperatorError::Usage);
        }
        let mut pairs = Vec::with_capacity(values.len());
        for raw in values {
            if raw.len() > 1024 || raw.bytes().any(|byte| byte == 0 || byte.is_ascii_control()) {
                return Err(OperatorError::Usage);
            }
            let (key, value) = raw.split_once('=').ok_or(OperatorError::Usage)?;
            if key.is_empty()
                || value.is_empty()
                || pairs.iter().any(|pair: &SelectorPair| pair.key == key)
            {
                return Err(OperatorError::Usage);
            }
            validate_selector_value(purpose, key, value)?;
            pairs.push(SelectorPair {
                key: key.to_owned(),
                value: value.to_owned(),
            });
        }
        pairs.sort_by(|left, right| left.key.cmp(&right.key));
        let has_join_key = pairs.iter().any(|pair| match purpose {
            CompliancePurpose::NetworkTrace => matches!(
                pair.key.as_str(),
                "event_id" | "recipient" | "owner" | "correlation_commitment"
            ),
            CompliancePurpose::IdentityTrace => pair.key == "correlation_commitment",
            CompliancePurpose::Attribution => matches!(pair.key.as_str(), "event_id" | "recipient"),
        });
        if !has_join_key {
            return Err(OperatorError::Usage);
        }
        let mut canonical = Zeroizing::new(Vec::new());
        for (index, pair) in pairs.iter().enumerate() {
            if index != 0 {
                canonical.push(0);
            }
            canonical.extend_from_slice(pair.key.as_bytes());
            canonical.push(b'=');
            canonical.extend_from_slice(pair.value.as_bytes());
        }
        Ok(Self { pairs, canonical })
    }

    fn canonical(&self) -> &[u8] {
        &self.canonical
    }

    fn matches_trace(&self, record: &DisclosedTraceRecord) -> bool {
        self.pairs
            .iter()
            .all(|pair| match (record, pair.key.as_str()) {
                (DisclosedTraceRecord::Network(value), "event_id") => {
                    option_hex_eq(value.event_id, &pair.value)
                }
                (DisclosedTraceRecord::Network(value), "recipient") => {
                    option_hex_eq(value.recipient, &pair.value)
                }
                (DisclosedTraceRecord::Network(value), "owner") => {
                    option_hex_eq(value.owner, &pair.value)
                }
                (DisclosedTraceRecord::Network(value), "correlation_commitment") => {
                    option_hex_eq(value.correlation_id, &pair.value)
                }
                (DisclosedTraceRecord::Network(value), "operation") => {
                    operation_name(value.operation) == pair.value
                }
                (DisclosedTraceRecord::Identity(value), "correlation_commitment") => {
                    hex_encode(&value.correlation_id) == pair.value
                }
                _ => false,
            })
    }

    fn matches_attribution_public(&self, wrap: &Wrap) -> bool {
        self.pairs.iter().all(|pair| match pair.key.as_str() {
            "event_id" => hex_encode(&wrap.id()) == pair.value,
            "recipient" => hex_encode(&wrap.recipient) == pair.value,
            _ => false,
        })
    }
}

impl core::fmt::Debug for SelectorSet {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        f.write_str("SelectorSet(<withheld>)")
    }
}

fn validate_selector_value(
    purpose: CompliancePurpose,
    key: &str,
    value: &str,
) -> OperatorResult<()> {
    let id_key = match purpose {
        CompliancePurpose::NetworkTrace => matches!(
            key,
            "event_id" | "recipient" | "owner" | "correlation_commitment"
        ),
        CompliancePurpose::IdentityTrace => key == "correlation_commitment",
        CompliancePurpose::Attribution => matches!(key, "event_id" | "recipient"),
    };
    if id_key {
        let decoded = decode_hex_array::<32>(value).map_err(|_| OperatorError::Usage)?;
        if decoded == [0u8; 32] || hex_encode(&decoded) != value {
            return Err(OperatorError::Usage);
        }
        return Ok(());
    }
    if purpose == CompliancePurpose::NetworkTrace
        && key == "operation"
        && matches!(value, "publish" | "fetch" | "put_agent" | "claim")
    {
        return Ok(());
    }
    Err(OperatorError::Usage)
}

fn option_hex_eq(value: Option<[u8; 32]>, expected: &str) -> bool {
    value.is_some_and(|value| hex_encode(&value) == expected)
}

fn operation_name(value: NetworkOperation) -> &'static str {
    match value {
        NetworkOperation::Publish => "publish",
        NetworkOperation::Fetch => "fetch",
        NetworkOperation::PutAgent => "put_agent",
        NetworkOperation::Claim => "claim",
    }
}

fn parse_command(args: impl IntoIterator<Item = OsString>) -> OperatorResult<ParsedCommand> {
    let args: Vec<String> = args
        .into_iter()
        .map(|arg| arg.into_string().map_err(|_| OperatorError::Usage))
        .collect::<OperatorResult<_>>()?;
    let Some(command) = args.first().map(String::as_str) else {
        return Err(OperatorError::Usage);
    };
    if matches!(command, "help" | "--help" | "-h") {
        return (args.len() == 1)
            .then_some(ParsedCommand::Help)
            .ok_or(OperatorError::Usage);
    }
    if matches!(command, "--version" | "-V") {
        return (args.len() == 1)
            .then_some(ParsedCommand::Version)
            .ok_or(OperatorError::Usage);
    }
    match command {
        "status" if args.len() == 1 => Ok(ParsedCommand::Status),
        "checkpoint" if args.len() == 1 => Ok(ParsedCommand::Checkpoint),
        "unseal" if args.len() == 3 && args[1] == "--epoch" => {
            parse_cli_key_id(&args[2])?;
            Ok(ParsedCommand::UnsealRequest {
                epoch: args[2].clone(),
            })
        }
        "hold" if args.len() == 5 && args[1] == "--epoch" && args[3] == "--until" => {
            parse_cli_key_id(&args[2])?;
            Ok(ParsedCommand::HoldRequest {
                epoch: args[2].clone(),
                action: HoldAction::Place {
                    until: args[4].clone(),
                },
            })
        }
        "hold"
            if args.len() == 8
                && args[1] == "renew"
                && args[2] == "--epoch"
                && args[4] == "--hold"
                && args[6] == "--until" =>
        {
            parse_cli_key_id(&args[3])?;
            Ok(ParsedCommand::HoldRequest {
                epoch: args[3].clone(),
                action: HoldAction::Renew {
                    prior_hold_id: parse_hold_id(&args[5])?,
                    until: args[7].clone(),
                },
            })
        }
        "hold"
            if args.len() == 6
                && args[1] == "release"
                && args[2] == "--epoch"
                && args[4] == "--hold" =>
        {
            parse_cli_key_id(&args[3])?;
            Ok(ParsedCommand::HoldRequest {
                epoch: args[3].clone(),
                action: HoldAction::Release {
                    hold_id: parse_hold_id(&args[5])?,
                },
            })
        }
        "inventory" if args.len() == 4 && args[2] == "--epoch" => {
            let action = match args[1].as_str() {
                "create" => InventoryAction::Create,
                "provision" => InventoryAction::Provision,
                "import" => InventoryAction::Import,
                "update" => InventoryAction::Update,
                _ => return Err(OperatorError::Usage),
            };
            parse_cli_key_id(&args[3])?;
            Ok(ParsedCommand::Inventory {
                action,
                epoch: args[3].clone(),
            })
        }
        "shred" => {
            let mut before = None;
            let mut execute = false;
            let mut explicitly_dry = false;
            let mut index = 1;
            while index < args.len() {
                match args[index].as_str() {
                    "--before" if before.is_none() => {
                        before = Some(args.get(index + 1).ok_or(OperatorError::Usage)?.clone());
                        index += 2;
                    }
                    "--dry-run" if !explicitly_dry => {
                        explicitly_dry = true;
                        index += 1;
                    }
                    "--execute" if !execute => {
                        execute = true;
                        index += 1;
                    }
                    _ => return Err(OperatorError::Usage),
                }
            }
            if execute && explicitly_dry {
                return Err(OperatorError::Usage);
            }
            Ok(ParsedCommand::Shred {
                before: before.ok_or(OperatorError::Usage)?,
                execute,
            })
        }
        _ => Err(OperatorError::Usage),
    }
}

fn validate_absolute_path(path: &Path) -> OperatorResult<()> {
    use std::path::Component;
    if !path.is_absolute()
        || path
            .components()
            .any(|part| matches!(part, Component::CurDir | Component::ParentDir))
    {
        return Err(OperatorError::Configuration);
    }
    Ok(())
}

/// Resolve a configured named entry through its existing parent without following the final entry.
/// The latter is validated independently when it is opened. This catches intermediate symlinks
/// while retaining the no-follow property for the inventory file itself.
fn canonical_named_path(path: &Path) -> OperatorResult<PathBuf> {
    validate_absolute_path(path)?;
    #[cfg(unix)]
    {
        let (parent, name) =
            guarded_path(path, false, false).map_err(|_| OperatorError::Configuration)?;
        Ok(parent.absolute_path().join(name.as_os_str()))
    }
    #[cfg(not(unix))]
    {
        let parent = path.parent().ok_or(OperatorError::Configuration)?;
        let file_name = path.file_name().ok_or(OperatorError::Configuration)?;
        let canonical_parent =
            fs::canonicalize(parent).map_err(|_| OperatorError::Configuration)?;
        Ok(canonical_parent.join(file_name))
    }
}

fn validate_private_directory(path: &Path, create: bool) -> OperatorResult<()> {
    #[cfg(unix)]
    {
        let directory = if create {
            GuardedDir::create_private(path)
        } else {
            GuardedDir::open_existing(path, DirPolicy::private())
        }
        .map_err(map_operator_custody)?;
        directory.verify_named().map_err(map_operator_custody)?;
        Ok(())
    }
    #[cfg(not(unix))]
    {
        if create && !path.exists() {
            fs::create_dir_all(path).map_err(|_| OperatorError::Storage)?;
        }
        let metadata = fs::symlink_metadata(path).map_err(|_| OperatorError::Storage)?;
        if metadata.file_type().is_symlink() || !metadata.is_dir() {
            return Err(OperatorError::Storage);
        }
        Ok(())
    }
}

fn validate_regular_file(path: &Path, private: bool, max_bytes: u64) -> OperatorResult<u64> {
    #[cfg(unix)]
    {
        let guard = open_guarded_regular(path, private, max_bytes, false)?;
        guard
            .metadata()
            .map(|metadata| metadata.len)
            .map_err(map_operator_custody)
    }
    #[cfg(not(unix))]
    {
        let file = open_existing_regular(path, private, max_bytes, false)?;
        file.metadata()
            .map(|metadata| metadata.len())
            .map_err(|_| OperatorError::Storage)
    }
}

fn read_private_file(path: &Path, max_bytes: u64) -> OperatorResult<Zeroizing<Vec<u8>>> {
    read_bounded_file(path, true, max_bytes)
}

fn read_artifact(path: &Path, max_bytes: u64) -> OperatorResult<Zeroizing<Vec<u8>>> {
    read_bounded_file(path, false, max_bytes)
}

fn read_bounded_file(
    path: &Path,
    private: bool,
    max_bytes: u64,
) -> OperatorResult<Zeroizing<Vec<u8>>> {
    #[cfg(unix)]
    {
        let guard = open_guarded_regular(path, private, max_bytes, false)?;
        guard.verify_named().map_err(map_operator_custody)?;
        let metadata = guard.metadata().map_err(map_operator_custody)?;
        let mut file = guard
            .file()
            .try_clone()
            .map_err(|_| OperatorError::Storage)?;
        use std::io::{Seek, SeekFrom};
        file.seek(SeekFrom::Start(0))
            .map_err(|_| OperatorError::Storage)?;
        let mut bytes = Zeroizing::new(Vec::with_capacity(
            usize::try_from(metadata.len).map_err(|_| OperatorError::Limit)?,
        ));
        file.take(max_bytes.saturating_add(1))
            .read_to_end(&mut bytes)
            .map_err(|_| OperatorError::Storage)?;
        guard.verify_named().map_err(map_operator_custody)?;
        if bytes.len() as u64 != metadata.len
            || bytes.len() as u64 > max_bytes
            || guard.metadata().map_err(map_operator_custody)? != metadata
        {
            return Err(OperatorError::Storage);
        }
        Ok(bytes)
    }
    #[cfg(not(unix))]
    {
        let file = open_existing_regular(path, private, max_bytes, false)?;
        let length = file.metadata().map_err(|_| OperatorError::Storage)?.len();
        let mut bytes = Zeroizing::new(Vec::with_capacity(length as usize));
        file.take(max_bytes.saturating_add(1))
            .read_to_end(&mut bytes)
            .map_err(|_| OperatorError::Storage)?;
        if bytes.len() as u64 != length || bytes.len() as u64 > max_bytes {
            return Err(OperatorError::Storage);
        }
        Ok(bytes)
    }
}

#[cfg(unix)]
fn guarded_path(
    path: &Path,
    private: bool,
    create_parent: bool,
) -> OperatorResult<(GuardedDir, LeafName)> {
    validate_absolute_path(path)?;
    NormalizedPath::new(path).map_err(map_operator_custody)?;
    let parent_path = path.parent().ok_or(OperatorError::Storage)?;
    let directory = if create_parent {
        if !private {
            return Err(OperatorError::Configuration);
        }
        GuardedDir::create_private(parent_path)
    } else {
        GuardedDir::open_existing(
            parent_path,
            if private {
                DirPolicy::private()
            } else {
                DirPolicy::trusted()
            },
        )
    }
    .map_err(map_operator_custody)?;
    let name = LeafName::new(path.file_name().ok_or(OperatorError::Storage)?)
        .map_err(map_operator_custody)?;
    Ok((directory, name))
}

#[cfg(unix)]
fn open_guarded_regular(
    path: &Path,
    private: bool,
    max_bytes: u64,
    writable: bool,
) -> OperatorResult<GuardedFile> {
    let (parent, name) = guarded_path(path, private, false)?;
    parent
        .open_file(
            &name,
            if writable {
                OpenAccess::ReadWrite
            } else {
                OpenAccess::ReadOnly
            },
            if private {
                FilePolicy::private(max_bytes)
            } else {
                FilePolicy::trusted(max_bytes)
            },
        )
        .map_err(map_operator_custody)
}

fn regular_entry_exists(path: &Path, private: bool) -> OperatorResult<bool> {
    #[cfg(unix)]
    {
        let (parent, name) = guarded_path(path, private, false)?;
        parent
            .entry_metadata(&name)
            .map(|metadata| metadata.is_some())
            .map_err(map_operator_custody)
    }
    #[cfg(not(unix))]
    {
        let _ = private;
        path.try_exists().map_err(|_| OperatorError::Storage)
    }
}

#[cfg(unix)]
fn map_operator_custody(error: CustodyError) -> OperatorError {
    match error {
        CustodyError::AlreadyExists => OperatorError::State,
        CustodyError::LimitExceeded(_) => OperatorError::Limit,
        _ => OperatorError::Storage,
    }
}

#[cfg(windows)]
fn open_existing_regular(
    path: &Path,
    _private: bool,
    max_bytes: u64,
    writable: bool,
) -> OperatorResult<File> {
    use std::os::windows::fs::{MetadataExt, OpenOptionsExt};

    const FILE_ATTRIBUTE_REPARSE_POINT: u32 = 0x0000_0400;
    const FILE_FLAG_OPEN_REPARSE_POINT: u32 = 0x0020_0000;

    let mut options = OpenOptions::new();
    options
        .read(true)
        .write(writable)
        .custom_flags(FILE_FLAG_OPEN_REPARSE_POINT);
    let file = options.open(path).map_err(|_| OperatorError::Storage)?;
    let opened = file.metadata().map_err(|_| OperatorError::Storage)?;
    if !opened.is_file()
        || opened.file_attributes() & FILE_ATTRIBUTE_REPARSE_POINT != 0
        || opened.len() > max_bytes
    {
        return Err(OperatorError::Storage);
    }
    Ok(file)
}

#[cfg(not(any(unix, windows)))]
fn open_existing_regular(
    path: &Path,
    _private: bool,
    max_bytes: u64,
    writable: bool,
) -> OperatorResult<File> {
    let before = fs::symlink_metadata(path).map_err(|_| OperatorError::Storage)?;
    if before.file_type().is_symlink() || !before.is_file() || before.len() > max_bytes {
        return Err(OperatorError::Storage);
    }
    let mut options = OpenOptions::new();
    options.read(true).write(writable);
    let file = options.open(path).map_err(|_| OperatorError::Storage)?;
    let opened = file.metadata().map_err(|_| OperatorError::Storage)?;
    if !opened.is_file() || opened.len() > max_bytes {
        return Err(OperatorError::Storage);
    }
    Ok(file)
}

#[cfg(not(unix))]
fn named_file_matches(file: &File, path: &Path) -> bool {
    file.metadata().is_ok_and(|metadata| metadata.is_file())
        && fs::symlink_metadata(path)
            .is_ok_and(|metadata| !metadata.file_type().is_symlink() && metadata.is_file())
}

fn write_private_atomic(path: &Path, bytes: &[u8], overwrite: bool) -> OperatorResult<()> {
    #[cfg(unix)]
    {
        let (parent, name) = guarded_path(path, true, true)?;
        let existing = parent
            .open_file_optional(&name, OpenAccess::ReadOnly, FilePolicy::private(u64::MAX))
            .map_err(map_operator_custody)?;
        if !overwrite && existing.is_some() {
            return Err(OperatorError::State);
        }
        if existing.is_none()
            && parent
                .entry_metadata(&name)
                .map_err(map_operator_custody)?
                .is_some()
        {
            return Err(OperatorError::Storage);
        }
        let mut random = [0u8; 16];
        OsRng.fill_bytes(&mut random);
        let temp_name = LeafName::new(format!(
            ".{}.{}.tmp",
            name.as_os_str().to_string_lossy(),
            hex_encode(&random)
        ))
        .map_err(map_operator_custody)?;
        let mut temp = parent
            .create_file(&temp_name, FilePolicy::private(bytes.len() as u64))
            .map_err(map_operator_custody)?;
        let cleanup = parent
            .open_file(
                &temp_name,
                OpenAccess::ReadOnly,
                FilePolicy::private(bytes.len() as u64),
            )
            .map_err(map_operator_custody)?;
        let result = (|| {
            temp.write_all(bytes).map_err(|_| OperatorError::Storage)?;
            temp.sync_all().map_err(map_operator_custody)?;
            if temp.metadata().map_err(map_operator_custody)?.len != bytes.len() as u64 {
                return Err(OperatorError::Storage);
            }
            let published = if let Some(existing) = existing.as_ref() {
                existing.verify_named().map_err(map_operator_custody)?;
                parent
                    .rename_replace(temp, &parent, &name)
                    .map_err(map_operator_custody)?
            } else {
                parent
                    .publish_no_replace(temp, &parent, &name)
                    .map_err(map_operator_custody)?
            };
            published.verify_named().map_err(map_operator_custody)?;
            if published.metadata().map_err(map_operator_custody)?.len != bytes.len() as u64 {
                return Err(OperatorError::Storage);
            }
            parent.sync().map_err(map_operator_custody)
        })();
        if result.is_err() {
            let _ = parent.unlink_file(cleanup);
        }
        result
    }

    #[cfg(not(unix))]
    {
        let parent = path.parent().ok_or(OperatorError::Storage)?;
        validate_private_directory(parent, true)?;
        if !overwrite && path.exists() {
            return Err(OperatorError::State);
        }
        if path.exists() {
            validate_regular_file(path, true, u64::MAX)?;
        }
        let file_name = path
            .file_name()
            .and_then(|value| value.to_str())
            .ok_or(OperatorError::Storage)?;
        let mut random = [0u8; 16];
        OsRng.fill_bytes(&mut random);
        let temp = parent.join(format!(".{file_name}.{}.tmp", hex_encode(&random)));
        let mut options = OpenOptions::new();
        options.create_new(true).write(true);
        #[cfg(unix)]
        {
            use std::os::unix::fs::OpenOptionsExt;
            options.mode(0o600);
        }
        let result = (|| {
            let mut file = options.open(&temp).map_err(|_| OperatorError::Storage)?;
            file.write_all(bytes).map_err(|_| OperatorError::Storage)?;
            file.sync_all().map_err(|_| OperatorError::Storage)?;
            if overwrite {
                fs::rename(&temp, path).map_err(|_| OperatorError::Storage)?;
            } else {
                // A hard-link publication is an atomic no-replace operation on the same filesystem.
                // Removing the temporary name immediately restores the required single-link custody
                // invariant while retaining the fully fsynced inode at its final name.
                fs::hard_link(&temp, path).map_err(|error| {
                    if error.kind() == std::io::ErrorKind::AlreadyExists {
                        OperatorError::State
                    } else {
                        OperatorError::Storage
                    }
                })?;
                fs::remove_file(&temp).map_err(|_| OperatorError::Storage)?;
            }
            validate_regular_file(path, true, bytes.len() as u64)?;
            sync_directory(parent)
        })();
        if result.is_err() {
            let _ = fs::remove_file(&temp);
        }
        result
    }
}

#[cfg(not(unix))]
fn sync_directory(path: &Path) -> OperatorResult<()> {
    let _ = path;
    Ok(())
}

struct OperatorLock {
    file: File,
    #[cfg(unix)]
    _guard: GuardedFile,
}

impl OperatorLock {
    fn acquire(home: &Path) -> OperatorResult<Self> {
        #[cfg(unix)]
        let (file, guard) = {
            let directory = GuardedDir::open_existing(home, DirPolicy::private())
                .map_err(map_operator_custody)?;
            let name = LeafName::new("operator.lock").map_err(map_operator_custody)?;
            let guard = directory
                .open_or_create_file(&name, OpenAccess::ReadWrite, FilePolicy::private_exact(0))
                .map_err(map_operator_custody)?;
            guard.verify_named().map_err(map_operator_custody)?;
            let file = guard
                .file()
                .try_clone()
                .map_err(|_| OperatorError::Storage)?;
            (file, guard)
        };
        #[cfg(not(unix))]
        let file = {
            validate_private_directory(home, false)?;
            open_private_lock(&home.join("operator.lock"))?
        };
        file.lock_exclusive().map_err(|_| OperatorError::Storage)?;
        #[cfg(unix)]
        guard.verify_named().map_err(map_operator_custody)?;
        Ok(Self {
            file,
            #[cfg(unix)]
            _guard: guard,
        })
    }
}

#[cfg(not(unix))]
fn open_private_lock(path: &Path) -> OperatorResult<File> {
    let mut options = OpenOptions::new();
    options.create(true).read(true).write(true);
    let file = options.open(path).map_err(|_| OperatorError::Storage)?;
    if file.metadata().map_err(|_| OperatorError::Storage)?.len() != 0
        || !named_file_matches(&file, path)
    {
        return Err(OperatorError::Storage);
    }
    Ok(file)
}

impl Drop for OperatorLock {
    fn drop(&mut self) {
        let _ = FileExt::unlock(&self.file);
    }
}

fn utc_date_start_ms(value: &str) -> OperatorResult<u64> {
    let bytes = value.as_bytes();
    if bytes.len() != 10 || bytes[4] != b'-' || bytes[7] != b'-' {
        return Err(OperatorError::Usage);
    }
    let year = parse_decimal(&bytes[..4])? as i64;
    let month = parse_decimal(&bytes[5..7])?;
    let day = parse_decimal(&bytes[8..])?;
    if year < 1970 || !(1..=12).contains(&month) || day == 0 || day > days_in_month(year, month) {
        return Err(OperatorError::Usage);
    }
    let days = days_from_civil(year, month, day);
    u64::try_from(days)
        .ok()
        .and_then(|days| days.checked_mul(86_400_000))
        .ok_or(OperatorError::Usage)
}

fn utc_date_end_ms(value: &str) -> OperatorResult<u64> {
    utc_date_start_ms(value)?
        .checked_add(86_400_000 - 1)
        .ok_or(OperatorError::Usage)
}

fn parse_decimal(bytes: &[u8]) -> OperatorResult<u32> {
    if bytes.is_empty() || !bytes.iter().all(u8::is_ascii_digit) {
        return Err(OperatorError::Usage);
    }
    bytes.iter().try_fold(0u32, |value, digit| {
        value
            .checked_mul(10)
            .and_then(|value| value.checked_add(u32::from(digit - b'0')))
            .ok_or(OperatorError::Usage)
    })
}

fn days_in_month(year: i64, month: u32) -> u32 {
    match month {
        1 | 3 | 5 | 7 | 8 | 10 | 12 => 31,
        4 | 6 | 9 | 11 => 30,
        2 if year % 4 == 0 && (year % 100 != 0 || year % 400 == 0) => 29,
        2 => 28,
        _ => 0,
    }
}

fn days_from_civil(year: i64, month: u32, day: u32) -> i64 {
    let adjusted_year = year - i64::from(month <= 2);
    let era = if adjusted_year >= 0 {
        adjusted_year
    } else {
        adjusted_year - 399
    } / 400;
    let year_of_era = adjusted_year - era * 400;
    let shifted_month = i64::from(month) + if month > 2 { -3 } else { 9 };
    let day_of_year = (153 * shifted_month + 2) / 5 + i64::from(day) - 1;
    let day_of_era = year_of_era * 365 + year_of_era / 4 - year_of_era / 100 + day_of_year;
    era * 146_097 + day_of_era - 719_468
}

fn now_ms() -> OperatorResult<u64> {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_err(|_| OperatorError::State)?
        .as_millis()
        .try_into()
        .map_err(|_| OperatorError::State)
}

fn parse_key_id(value: &str) -> OperatorResult<ComplianceKeyId> {
    let bytes = decode_hex_array::<COMPLIANCE_KEY_ID_LEN>(value)
        .map_err(|_| OperatorError::Configuration)?;
    let key_id = ComplianceKeyId::decode(&bytes).map_err(|_| OperatorError::Configuration)?;
    if hex_encode(&bytes) != value {
        return Err(OperatorError::Configuration);
    }
    Ok(key_id)
}

fn parse_canonical_hex32(value: &str) -> OperatorResult<[u8; 32]> {
    let decoded = decode_hex_array::<32>(value).map_err(|_| OperatorError::Configuration)?;
    if hex_encode(&decoded) != value {
        return Err(OperatorError::Configuration);
    }
    Ok(decoded)
}

fn decode_hex_array<const N: usize>(value: &str) -> core::result::Result<[u8; N], ()> {
    if value.len() != N * 2 || !value.bytes().all(|byte| byte.is_ascii_hexdigit()) {
        return Err(());
    }
    let mut out = [0u8; N];
    for (index, pair) in value.as_bytes().chunks_exact(2).enumerate() {
        out[index] = (hex_nibble(pair[0])? << 4) | hex_nibble(pair[1])?;
    }
    Ok(out)
}

fn hex_nibble(value: u8) -> core::result::Result<u8, ()> {
    match value {
        b'0'..=b'9' => Ok(value - b'0'),
        b'a'..=b'f' => Ok(value - b'a' + 10),
        _ => Err(()),
    }
}

fn hex_encode(bytes: &[u8]) -> String {
    const HEX: &[u8; 16] = b"0123456789abcdef";
    let mut out = String::with_capacity(bytes.len() * 2);
    for byte in bytes {
        out.push(HEX[(byte >> 4) as usize] as char);
        out.push(HEX[(byte & 0x0f) as usize] as char);
    }
    out
}

fn read_private_request<T: DeserializeOwned>(input: &mut impl Read) -> OperatorResult<T> {
    let mut encoded = Zeroizing::new(Vec::new());
    input
        .take(PRIVATE_REQUEST_MAX_BYTES + 1)
        .read_to_end(&mut encoded)
        .map_err(|_| OperatorError::Storage)?;
    if encoded.is_empty() {
        return Err(OperatorError::Usage);
    }
    if encoded.len() as u64 > PRIVATE_REQUEST_MAX_BYTES {
        return Err(OperatorError::Limit);
    }
    let text = core::str::from_utf8(&encoded).map_err(|_| OperatorError::Usage)?;
    toml::from_str(text).map_err(|_| OperatorError::Usage)
}

fn validate_private_request_field(value: &SensitiveString) -> OperatorResult<()> {
    if value.0.is_empty()
        || value.0.len() > MAX_PRIVATE_FIELD_BYTES
        || value
            .0
            .bytes()
            .any(|byte| byte == 0 || byte.is_ascii_control())
    {
        return Err(OperatorError::Usage);
    }
    Ok(())
}

fn hydrate_private_command(
    command: ParsedCommand,
    input: &mut impl Read,
) -> OperatorResult<(ParsedCommand, Option<Zeroizing<String>>)> {
    match command {
        ParsedCommand::UnsealRequest { epoch } => {
            let declaration: UnsealRequestDeclaration = read_private_request(input)?;
            if declaration.version != PRIVATE_REQUEST_VERSION
                || declaration.selectors.is_empty()
                || declaration.selectors.len() > 8
            {
                return Err(OperatorError::Usage);
            }
            validate_private_request_field(&declaration.order_reference)?;
            validate_private_request_field(&declaration.requester_identity)?;
            let order_reference = declaration.order_reference.into_zeroizing();
            let requester = declaration.requester_identity.into_zeroizing();
            let selectors = declaration
                .selectors
                .into_iter()
                .map(SensitiveString::into_zeroizing)
                .collect();
            Ok((
                ParsedCommand::Unseal {
                    epoch,
                    order_reference,
                    selectors,
                },
                Some(requester),
            ))
        }
        ParsedCommand::HoldRequest { epoch, action } => {
            let declaration: HoldRequestDeclaration = read_private_request(input)?;
            if declaration.version != PRIVATE_REQUEST_VERSION {
                return Err(OperatorError::Usage);
            }
            validate_private_request_field(&declaration.order_reference)?;
            Ok((
                ParsedCommand::Hold {
                    epoch,
                    order_reference: declaration.order_reference.into_zeroizing(),
                    action,
                },
                None,
            ))
        }
        command => Ok((command, None)),
    }
}

/// Execute the offline operator CLI using `PIGEONPOST_COMPLIANCE_HOME`.
///
/// Sensitive case values for `unseal` and `hold` are accepted only as a bounded, strict TOML
/// declaration on `input`; they are never accepted through arguments or environment variables.
pub fn run_from_env(
    args: impl IntoIterator<Item = OsString>,
    input: &mut impl Read,
    output: &mut impl Write,
) -> OperatorResult<()> {
    run_from_env_for_platform(
        crate::platform::OfflinePlatform::current(),
        args,
        input,
        output,
    )
}

fn run_from_env_for_platform(
    platform: crate::platform::OfflinePlatform,
    args: impl IntoIterator<Item = OsString>,
    input: &mut impl Read,
    output: &mut impl Write,
) -> OperatorResult<()> {
    let command = parse_command(args)?;
    if matches!(command, ParsedCommand::Help) {
        output
            .write_all(help_text().as_bytes())
            .map_err(|_| OperatorError::Storage)?;
        return Ok(());
    }
    if matches!(command, ParsedCommand::Version) {
        writeln!(output, "ppcompliance {}", env!("CARGO_PKG_VERSION"))
            .map_err(|_| OperatorError::Storage)?;
        return Ok(());
    }
    platform.require().map_err(OperatorError::from)?;
    let home = env::var_os("PIGEONPOST_COMPLIANCE_HOME")
        .map(PathBuf::from)
        .ok_or(OperatorError::Configuration)?;
    let (command, requester) = hydrate_private_command(command, input)?;
    run_command(
        &home,
        command,
        requester.as_deref().map(String::as_bytes),
        now_ms()?,
        &ProcessRunner,
        output,
    )
}

fn run_command(
    home: &Path,
    command: ParsedCommand,
    requester: Option<&[u8]>,
    now_ms: u64,
    runner: &dyn CommandRunner,
    output: &mut dyn Write,
) -> OperatorResult<()> {
    let config = OperatorConfig::load(home)?;
    match command {
        ParsedCommand::Help
        | ParsedCommand::Version
        | ParsedCommand::UnsealRequest { .. }
        | ParsedCommand::HoldRequest { .. } => {
            unreachable!("informational commands return before configuration loading")
        }
        ParsedCommand::Status => command_status(home, &config, now_ms, output),
        ParsedCommand::Checkpoint => command_checkpoint(home, &config, output),
        ParsedCommand::Inventory { action, epoch } => {
            command_inventory(home, &config, action, &epoch, now_ms, output)
        }
        ParsedCommand::Hold {
            epoch,
            order_reference,
            action,
        } => command_hold(
            home,
            &config,
            &epoch,
            order_reference.as_bytes(),
            action,
            now_ms,
            runner,
            output,
        ),
        ParsedCommand::Shred { before, execute } => {
            command_shred(home, &config, &before, execute, now_ms, runner, output)
        }
        ParsedCommand::Unseal {
            epoch,
            order_reference,
            selectors,
        } => command_unseal(
            home,
            &config,
            &epoch,
            order_reference.as_bytes(),
            selectors,
            requester.ok_or(OperatorError::Authorization)?,
            now_ms,
            runner,
            output,
        ),
    }
}

fn command_inventory(
    home: &Path,
    config: &OperatorConfig,
    action: InventoryAction,
    epoch_text: &str,
    now_ms: u64,
    output: &mut dyn Write,
) -> OperatorResult<()> {
    let key_id = parse_cli_key_id(epoch_text)?;
    let epoch = config.epoch(key_id)?;
    let policy = epoch.retention_policy.policy()?;
    let _lock = OperatorLock::acquire(home)?;
    let trace_guard = authenticate_trace_epoch(epoch, key_id, now_ms)?;
    if let Some(trace) = &trace_guard {
        trace.verify_all()?;
    }

    let result = match action {
        InventoryAction::Create => {
            if regular_entry_exists(&epoch.inventory_path, true)? {
                return Err(OperatorError::State);
            }
            let inventory = inventory_from_declaration(epoch, key_id, policy)?;
            write_private_atomic(&epoch.inventory_staging_path, &inventory.encode()?, false)?;
            writeln!(output, "inventory=staged").map_err(|_| OperatorError::Storage)
        }
        InventoryAction::Provision => {
            let inventory = load_inventory(&epoch.inventory_staging_path, key_id, policy)?;
            write_private_atomic(&epoch.inventory_path, &inventory.encode()?, false)?;
            writeln!(output, "inventory=provisioned").map_err(|_| OperatorError::Storage)
        }
        InventoryAction::Import => {
            let inventory = load_inventory(&epoch.inventory_import_path, key_id, policy)?;
            write_private_atomic(&epoch.inventory_path, &inventory.encode()?, false)?;
            writeln!(output, "inventory=imported").map_err(|_| OperatorError::Storage)
        }
        InventoryAction::Update => {
            let proposed = inventory_from_declaration(epoch, key_id, policy)?;
            let mut inventory = load_inventory_without_policy(&epoch.inventory_path, key_id)?;
            let (policy_updated, added) =
                inventory.update_policy_and_copies_monotonic(policy, proposed.copies().to_vec())?;
            persist_inventory(&epoch.inventory_path, &mut inventory)?;
            writeln!(output, "inventory=updated").map_err(|_| OperatorError::Storage)?;
            writeln!(output, "policy_updated={policy_updated}")
                .map_err(|_| OperatorError::Storage)?;
            writeln!(output, "added_copies={added}").map_err(|_| OperatorError::Storage)
        }
    };
    drop(trace_guard);
    result
}

fn command_status(
    home: &Path,
    config: &OperatorConfig,
    now_ms: u64,
    output: &mut dyn Write,
) -> OperatorResult<()> {
    // Opening a ledger performs crash-tail recovery, so even this observational command must not
    // race an append. The same lock also gives status one coherent inventory snapshot.
    let _lock = OperatorLock::acquire(home)?;
    let state_secret = Zeroizing::new(read_exact_secret(&config.checkpoint_signing_key_path)?);
    let checkpoint_key = SigningKey::from_bytes(&state_secret).verifying_key();
    let (leaf_count, incomplete, root) = if regular_entry_exists(&config.ledger_path, true)? {
        validate_regular_file(&config.ledger_path, true, 512 * 1024 * 1024)?;
        let ledger = DisclosureLedger::open(&config.ledger_path, &state_secret)?;
        verify_disclosure_checkpoint_floor(config, &ledger, &checkpoint_key)?;
        (
            ledger.leaf_count(),
            ledger.incomplete_request_ids().len(),
            ledger.root(),
        )
    } else {
        verify_empty_disclosure_checkpoint_floor(config, &checkpoint_key)?;
        (0, 0, pigeonpost_registry::log::empty_root())
    };
    let mut retained = 0usize;
    let mut shredding = 0usize;
    let mut shredded = 0usize;
    let mut active_holds = 0usize;
    let mut external = 0usize;
    let mut development = 0usize;
    for epoch in &config.epochs {
        let key_id = parse_key_id(&epoch.key_id)?;
        let inventory = load_inventory(
            &epoch.inventory_path,
            key_id,
            epoch.retention_policy.policy()?,
        )?;
        match inventory.state() {
            InventoryState::Retained => retained += 1,
            InventoryState::Shredding => shredding += 1,
            InventoryState::Shredded => shredded += 1,
        }
        active_holds += inventory
            .holds()
            .iter()
            .filter(|hold| hold.active_at(now_ms))
            .count();
        match epoch.custody.mode {
            CustodyMode::External => external += 1,
            CustodyMode::SoftwareDevelopment => development += 1,
        }
    }
    writeln!(output, "status=ready").map_err(|_| OperatorError::Storage)?;
    writeln!(output, "disclosure_leaves={leaf_count}").map_err(|_| OperatorError::Storage)?;
    writeln!(output, "incomplete_disclosures={incomplete}").map_err(|_| OperatorError::Storage)?;
    writeln!(output, "disclosure_root={}", hex_encode(&root))
        .map_err(|_| OperatorError::Storage)?;
    writeln!(output, "inventories_retained={retained}").map_err(|_| OperatorError::Storage)?;
    writeln!(output, "inventories_shredding={shredding}").map_err(|_| OperatorError::Storage)?;
    writeln!(output, "inventories_shredded={shredded}").map_err(|_| OperatorError::Storage)?;
    writeln!(output, "active_holds={active_holds}").map_err(|_| OperatorError::Storage)?;
    writeln!(output, "external_custody_epochs={external}").map_err(|_| OperatorError::Storage)?;
    writeln!(output, "development_custody_epochs={development}")
        .map_err(|_| OperatorError::Storage)?;
    Ok(())
}

fn command_checkpoint(
    home: &Path,
    config: &OperatorConfig,
    output: &mut dyn Write,
) -> OperatorResult<()> {
    let _lock = OperatorLock::acquire(home)?;
    validate_regular_file(&config.ledger_path, true, 512 * 1024 * 1024)?;
    let secret = Zeroizing::new(read_exact_secret(&config.checkpoint_signing_key_path)?);
    let ledger = DisclosureLedger::open(&config.ledger_path, &secret)?;
    let signing = SigningKey::from_bytes(&secret);
    let signed = publish_disclosure_checkpoint(config, &ledger, &signing)?;
    output
        .write_all(signed.as_bytes())
        .map_err(|_| OperatorError::Storage)
}

#[allow(clippy::too_many_arguments)]
fn command_hold(
    home: &Path,
    config: &OperatorConfig,
    epoch_text: &str,
    order_reference: &[u8],
    action: HoldAction,
    now_ms: u64,
    runner: &dyn CommandRunner,
    output: &mut dyn Write,
) -> OperatorResult<()> {
    let key_id = parse_cli_key_id(epoch_text)?;
    let epoch = config.epoch(key_id)?;
    let (action_commitment, expires_at_ms) = match &action {
        HoldAction::Place { until } => {
            let expires_at_ms = utc_date_end_ms(until)?;
            let mut commitment = Vec::from(&b"place"[..]);
            commitment.extend_from_slice(&expires_at_ms.to_be_bytes());
            (Zeroizing::new(commitment), Some(expires_at_ms))
        }
        HoldAction::Renew {
            prior_hold_id,
            until,
        } => {
            let expires_at_ms = utc_date_end_ms(until)?;
            let mut commitment = Vec::from(&b"renew"[..]);
            commitment.extend_from_slice(prior_hold_id);
            commitment.extend_from_slice(&expires_at_ms.to_be_bytes());
            (Zeroizing::new(commitment), Some(expires_at_ms))
        }
        HoldAction::Release { hold_id } => {
            let mut commitment = Vec::from(&b"release"[..]);
            commitment.extend_from_slice(hold_id);
            (Zeroizing::new(commitment), None)
        }
    };
    if expires_at_ms.is_some_and(|expires| expires <= now_ms) {
        return Err(OperatorError::Usage);
    }
    let _lock = OperatorLock::acquire(home)?;
    let mut inventory = load_inventory(
        &epoch.inventory_path,
        key_id,
        epoch.retention_policy.policy()?,
    )?;
    let authorization = authorize_hold_action(
        key_id,
        order_reference,
        &action_commitment,
        now_ms,
        &config.approval,
        runner,
    )?;
    let output_line = match action {
        HoldAction::Place { .. } => {
            let hold_id = inventory.place_hold(
                now_ms,
                expires_at_ms.expect("place has expiry"),
                authorization,
            )?;
            format!("hold_id={}\n", hex_encode(&hold_id))
        }
        HoldAction::Renew { prior_hold_id, .. } => {
            let hold_id = inventory.renew_hold(
                prior_hold_id,
                now_ms,
                expires_at_ms.expect("renewal has expiry"),
                authorization,
            )?;
            format!("hold_id={}\n", hex_encode(&hold_id))
        }
        HoldAction::Release { hold_id } => {
            inventory.release_hold(hold_id, now_ms, authorization)?;
            format!("released_hold_id={}\n", hex_encode(&hold_id))
        }
    };
    persist_inventory(&epoch.inventory_path, &mut inventory)?;
    output
        .write_all(output_line.as_bytes())
        .map_err(|_| OperatorError::Storage)
}

fn authorize_hold_action(
    key_id: ComplianceKeyId,
    order_reference: &[u8],
    action_commitment: &[u8],
    now_ms: u64,
    config: &ApprovalConfig,
    runner: &dyn CommandRunner,
) -> OperatorResult<LegalHoldAuthorization> {
    let expires_at_ms = now_ms
        .checked_add(config.request_ttl_ms)
        .ok_or(OperatorError::State)?;
    let (mut request, openings) = DisclosureRequest::new(
        key_id.jurisdiction,
        key_id.purpose,
        vec![key_id],
        now_ms,
        expires_at_ms,
        SensitiveRequestMaterial {
            order_reference,
            requester_identity: b"legal-process-intake",
            selectors: action_commitment,
        },
    )?;
    let evidence = authorize_request(
        &mut request,
        config,
        order_reference,
        b"legal-process-intake",
        action_commitment,
        now_ms,
        runner,
    )?;
    request.ensure_authorized(now_ms)?;
    LegalHoldAuthorization::from_request(&request, &openings, evidence.approvals)
        .map_err(Into::into)
}

fn command_shred(
    home: &Path,
    config: &OperatorConfig,
    before: &str,
    execute: bool,
    now_ms: u64,
    runner: &dyn CommandRunner,
    output: &mut dyn Write,
) -> OperatorResult<()> {
    let before_ms = utc_date_start_ms(before)?;
    let _lock = OperatorLock::acquire(home)?;
    let mut epochs: Vec<_> = config
        .epochs
        .iter()
        .map(|epoch| parse_key_id(&epoch.key_id).map(|key_id| (key_id, epoch)))
        .collect::<OperatorResult<_>>()?;
    epochs.sort_unstable_by_key(|(key_id, _)| {
        key_id
            .encode()
            .expect("validated configured compliance key id")
    });
    // Dry-run retains no per-epoch state. Execute retains only a bounded, copyable summary for
    // each eligible epoch; authenticated manifests and their directory guards are released after
    // each discovery check and re-opened one at a time during destruction.
    let mut selected = Vec::new();
    let mut eligible = 0usize;
    let mut held = 0usize;
    let mut unexpired = 0usize;
    let mut already_shredded = 0usize;
    let mut resumed = 0usize;
    let mut discovery_failed = 0usize;
    let mut trace_integrity_degraded = 0usize;
    let mut first_error = None;
    for (key_id, epoch) in epochs {
        if key_id.epoch_start_ms >= before_ms {
            continue;
        }
        let inventory = match load_inventory(
            &epoch.inventory_path,
            key_id,
            epoch.retention_policy.policy()?,
        ) {
            Ok(inventory) => inventory,
            Err(error) => {
                discovery_failed += 1;
                first_error.get_or_insert(error);
                continue;
            }
        };
        let trace = if inventory.state() == InventoryState::Shredded {
            None
        } else {
            match assess_configured_trace_epoch(epoch, key_id, now_ms) {
                Ok(trace) => trace,
                Err(error) => {
                    discovery_failed += 1;
                    first_error.get_or_insert(error);
                    continue;
                }
            }
        };
        let trace_is_degraded = trace
            .as_ref()
            .is_some_and(ShredTraceAssessment::is_degraded);
        match inventory.state() {
            InventoryState::Shredded => already_shredded += 1,
            InventoryState::Shredding => {
                resumed += 1;
                trace_integrity_degraded += usize::from(trace_is_degraded);
                eligible += 1;
                if execute {
                    selected.push(ShredCandidate {
                        epoch,
                        key_id,
                        trace,
                    });
                }
            }
            InventoryState::Retained => {
                let mut candidate = inventory.clone();
                match candidate.begin_shred(now_ms) {
                    Ok(()) => {
                        trace_integrity_degraded += usize::from(trace_is_degraded);
                        eligible += 1;
                        if execute {
                            selected.push(ShredCandidate {
                                epoch,
                                key_id,
                                trace,
                            });
                        }
                    }
                    Err(ComplianceError::LegalHoldActive) => held += 1,
                    Err(ComplianceError::RetentionActive) => unexpired += 1,
                    Err(error) => {
                        discovery_failed += 1;
                        first_error.get_or_insert(error.into());
                    }
                }
            }
        }
    }
    if !execute {
        writeln!(output, "mode=dry_run").map_err(|_| OperatorError::Storage)?;
        writeln!(output, "eligible_epochs={eligible}").map_err(|_| OperatorError::Storage)?;
        writeln!(output, "resumed_epochs={resumed}").map_err(|_| OperatorError::Storage)?;
        writeln!(output, "skipped_held_epochs={held}").map_err(|_| OperatorError::Storage)?;
        writeln!(output, "skipped_unexpired_epochs={unexpired}")
            .map_err(|_| OperatorError::Storage)?;
        writeln!(output, "already_shredded_epochs={already_shredded}")
            .map_err(|_| OperatorError::Storage)?;
        writeln!(output, "discovery_failed_epochs={discovery_failed}")
            .map_err(|_| OperatorError::Storage)?;
        writeln!(
            output,
            "trace_integrity_degraded_epochs={trace_integrity_degraded}"
        )
        .map_err(|_| OperatorError::Storage)?;
        if let Some(error) = first_error {
            return Err(error);
        }
        return Ok(());
    }
    let command = if selected.is_empty() {
        None
    } else {
        Some(
            config
                .destruction_command
                .as_ref()
                .ok_or(OperatorError::Configuration)?,
        )
    };
    let mut completed = 0usize;
    let mut failed = 0usize;
    for candidate in selected {
        let initially_degraded = candidate.trace.is_some_and(|trace| trace.is_degraded());
        let mut degraded = initially_degraded;
        let result = (|| {
            let mut inventory = load_inventory(
                &candidate.epoch.inventory_path,
                candidate.key_id,
                candidate.epoch.retention_policy.policy()?,
            )?;
            shred_epoch(
                candidate.epoch,
                &mut inventory,
                now_ms,
                command.expect("selected epochs require a configured destruction adapter"),
                runner,
                candidate.trace,
                &mut degraded,
            )
        })();
        if degraded && !initially_degraded {
            trace_integrity_degraded += 1;
        }
        match result {
            Ok(()) => completed += 1,
            Err(error) => {
                failed += 1;
                first_error.get_or_insert(error);
            }
        }
    }
    writeln!(output, "mode=execute").map_err(|_| OperatorError::Storage)?;
    writeln!(output, "shredded_epochs={completed}").map_err(|_| OperatorError::Storage)?;
    writeln!(output, "resumed_epochs={resumed}").map_err(|_| OperatorError::Storage)?;
    writeln!(output, "skipped_held_epochs={held}").map_err(|_| OperatorError::Storage)?;
    writeln!(output, "skipped_unexpired_epochs={unexpired}").map_err(|_| OperatorError::Storage)?;
    writeln!(output, "already_shredded_epochs={already_shredded}")
        .map_err(|_| OperatorError::Storage)?;
    writeln!(output, "discovery_failed_epochs={discovery_failed}")
        .map_err(|_| OperatorError::Storage)?;
    writeln!(output, "execution_failed_epochs={failed}").map_err(|_| OperatorError::Storage)?;
    writeln!(
        output,
        "trace_integrity_degraded_epochs={trace_integrity_degraded}"
    )
    .map_err(|_| OperatorError::Storage)?;
    writeln!(output, "failed_epochs={}", discovery_failed + failed)
        .map_err(|_| OperatorError::Storage)?;
    if let Some(error) = first_error {
        Err(error)
    } else {
        Ok(())
    }
}

fn shred_epoch(
    epoch: &EpochConfig,
    inventory: &mut LoadedInventory,
    now_ms: u64,
    command: &CommandConfig,
    runner: &dyn CommandRunner,
    initial_trace: Option<ShredTraceAssessment>,
    degraded: &mut bool,
) -> OperatorResult<()> {
    let trace_guard = if let Some(initial) = initial_trace {
        let (trace, _) = open_trace_epoch_for_destruction(epoch, inventory.key_id(), now_ms)?;
        let current = trace.assess_body_integrity()?;
        if trace.manifest_commitment() != initial.manifest_commitment {
            return Err(OperatorError::Artifact);
        }
        let status = if matches!(
            (initial.integrity, current),
            (TraceBodyIntegrity::Degraded, _) | (_, TraceBodyIntegrity::Degraded)
        ) {
            TraceIntegrityStatus::Degraded
        } else {
            TraceIntegrityStatus::Verified
        };
        inventory.record_trace_integrity(TraceIntegrityEvidence::new(
            status,
            initial.manifest_commitment,
        )?)?;
        // The integrity result must survive a crash before any key-copy destruction begins.
        persist_inventory(&epoch.inventory_path, inventory)?;
        *degraded = inventory
            .trace_integrity()
            .is_some_and(|evidence| evidence.status() == TraceIntegrityStatus::Degraded);
        Some(trace)
    } else {
        None
    };
    #[cfg(unix)]
    if let Some(trace) = trace_guard.as_ref() {
        trace.verify_custody()?;
    }
    if inventory.state() == InventoryState::Retained {
        inventory.begin_shred(now_ms)?;
        persist_inventory(&epoch.inventory_path, inventory)?;
    }
    if inventory.state() != InventoryState::Shredding {
        return Err(OperatorError::State);
    }
    let pending: Vec<_> = inventory
        .copies()
        .iter()
        .filter(|copy| copy.state() == CopyState::DestructionRequested)
        .map(|copy| (copy.copy_id(), copy.kind()))
        .collect();
    for (copy_id, kind) in pending {
        #[cfg(unix)]
        if let Some(trace) = trace_guard.as_ref() {
            trace.verify_custody()?;
        }
        let (destroyed, commitment) =
            execute_destruction(command, inventory.key_id(), copy_id, kind as u8, runner)?;
        #[cfg(unix)]
        if let Some(trace) = trace_guard.as_ref() {
            trace.verify_custody()?;
        }
        if destroyed {
            inventory.record_destroyed(copy_id, commitment)?;
        } else {
            inventory.record_verified_absent(copy_id, commitment)?;
        }
        persist_inventory(&epoch.inventory_path, inventory)?;
    }
    #[cfg(unix)]
    if let Some(trace) = trace_guard.as_ref() {
        trace.verify_custody()?;
    }
    inventory.complete_shred()?;
    persist_inventory(&epoch.inventory_path, inventory)?;
    drop(trace_guard);
    Ok(())
}

#[allow(clippy::too_many_arguments)]
fn command_unseal(
    home: &Path,
    config: &OperatorConfig,
    epoch_text: &str,
    order_reference: &[u8],
    selector_values: Vec<Zeroizing<String>>,
    requester: &[u8],
    now_ms: u64,
    runner: &dyn CommandRunner,
    output: &mut dyn Write,
) -> OperatorResult<()> {
    if requester.is_empty() || requester.len() > MAX_PRIVATE_FIELD_BYTES {
        return Err(OperatorError::Authorization);
    }
    let key_id = parse_cli_key_id(epoch_text)?;
    let epoch = config.epoch(key_id)?;
    let selectors = SelectorSet::parse(selector_values, key_id.purpose)?;
    let _lock = OperatorLock::acquire(home)?;
    let inventory = load_inventory(
        &epoch.inventory_path,
        key_id,
        epoch.retention_policy.policy()?,
    )?;
    if inventory.state() != InventoryState::Retained {
        return Err(OperatorError::State);
    }
    let custody_public_key = configured_custody_public_key(epoch, key_id)?;
    let registry_key =
        verify_registry_key(&config.registry_audit, key_id, custody_public_key, now_ms)?;
    let artifacts = prepare_artifacts(epoch, key_id, &registry_key, now_ms)?;
    let expires_at_ms = now_ms
        .checked_add(config.approval.request_ttl_ms)
        .ok_or(OperatorError::State)?;
    let (mut request, openings) = DisclosureRequest::new(
        key_id.jurisdiction,
        key_id.purpose,
        vec![key_id],
        now_ms,
        expires_at_ms,
        SensitiveRequestMaterial {
            order_reference,
            requester_identity: requester,
            selectors: selectors.canonical(),
        },
    )?;
    let approval_evidence = authorize_request(
        &mut request,
        &config.approval,
        order_reference,
        requester,
        selectors.canonical(),
        now_ms,
        runner,
    )?;
    let mut audit_secret = read_exact_secret(&config.private_audit_key_path)?;
    let audit_key = PrivateAuditKey::from_bytes(audit_secret)?;
    audit_secret.zeroize();
    let private_audit_material = PrivateAuditMaterial {
        order_reference,
        requester_identity: requester,
        selectors: selectors.canonical(),
        approver_identities: [
            config.approval.approvers[approval_evidence.approver_indices[0]]
                .identity
                .as_bytes(),
            config.approval.approvers[approval_evidence.approver_indices[1]]
                .identity
                .as_bytes(),
        ],
    };
    let terminal_manifest_commitment = artifacts.terminal_manifest_commitment();
    let encrypted_audit = match terminal_manifest_commitment {
        Some(commitment) => seal_private_audit_record_with_terminal_manifest(
            &audit_key,
            &request,
            &openings,
            private_audit_material,
            commitment,
        )?,
        None => seal_private_audit_record(&audit_key, &request, &openings, private_audit_material)?,
    };
    let encrypted_audit = Zeroizing::new(encrypted_audit.encode()?);
    let audit_path = config
        .private_audit_directory
        .join(format!("{}.ppaudit", hex_encode(&request.request_id())));
    let custody = runtime_custody(epoch, key_id, request.request_id(), runner)?;
    let state_secret = Zeroizing::new(read_exact_secret(&config.checkpoint_signing_key_path)?);
    let mut ledger = open_or_create_ledger(&config.ledger_path, &state_secret)?;
    let checkpoint_signing_key = SigningKey::from_bytes(&state_secret);
    verify_disclosure_checkpoint_floor(config, &ledger, &checkpoint_signing_key.verifying_key())?;
    // Establish a signed empty floor before the first intent so a crash between the first log
    // fsync and post-operation publication cannot leave a nonempty ledger without an anchor.
    publish_disclosure_checkpoint(config, &ledger, &checkpoint_signing_key)?;
    for incomplete in ledger.incomplete_request_ids() {
        ledger.close_incomplete_failure(incomplete, now_ms)?;
    }
    let disclosed = ledger
        .execute(
            &mut request,
            now_ms,
            || {
                runner
                    .completion_time_ms()
                    .map_err(|_| ComplianceError::Storage)
            },
            |authorization| {
                write_private_atomic(&audit_path, &encrypted_audit, false)
                    .map_err(|_| ComplianceError::Storage)?;
                let (bytes, record_count) = disclose_selected(
                    authorization,
                    key_id,
                    &custody,
                    &artifacts,
                    &selectors,
                    &registry_key,
                )?;
                let result_commitment = match terminal_manifest_commitment {
                    Some(commitment) => audit_key.result_commitment_with_terminal_manifest(
                        &authorization.request_id(),
                        &commitment,
                        &bytes,
                    )?,
                    None => audit_key.result_commitment(&authorization.request_id(), &bytes),
                };
                Ok(DisclosureOutput {
                    result_commitment,
                    value: bytes,
                    record_count,
                })
            },
        )
        .map(Zeroizing::new);
    // Publish the new monotonic handoff before releasing any disclosure bytes. This runs for both
    // success and failure because `execute` can advance the ledger even when no artifact returns.
    publish_disclosure_checkpoint(config, &ledger, &checkpoint_signing_key)?;
    let disclosed = disclosed?;
    output
        .write_all(&disclosed)
        .and_then(|_| output.flush())
        .map_err(|_| OperatorError::Storage)
}

enum PreparedArtifacts {
    Trace {
        epoch: Box<AuthenticatedTraceEpoch>,
        expected_node_id: [u8; 32],
    },
    Attribution(Vec<Wrap>),
}

impl PreparedArtifacts {
    fn terminal_manifest_commitment(&self) -> Option<[u8; 32]> {
        match self {
            Self::Trace { epoch, .. } => Some(epoch.manifest_commitment()),
            Self::Attribution(_) => None,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct RegistryKeyEvidence {
    not_before_ms: u64,
    not_after_ms: u64,
    public_key: [u8; 32],
}

impl RegistryKeyEvidence {
    fn permits_timestamp(&self, timestamp_ms: u64) -> bool {
        self.not_before_ms <= timestamp_ms && timestamp_ms < self.not_after_ms
    }
}

#[derive(Debug, Clone)]
struct AuditedRegistryKey {
    publication: ComplianceKeyPublish,
    public_key: [u8; 32],
    log_index: u64,
}

fn configured_custody_public_key(
    epoch: &EpochConfig,
    key_id: ComplianceKeyId,
) -> OperatorResult<[u8; 32]> {
    match epoch.custody.mode {
        CustodyMode::External => parse_canonical_hex32(
            epoch
                .custody
                .public_key
                .as_deref()
                .ok_or(OperatorError::Configuration)?,
        ),
        CustodyMode::SoftwareDevelopment => {
            let mut secret = read_exact_secret(
                epoch
                    .custody
                    .secret_key_path
                    .as_ref()
                    .ok_or(OperatorError::Configuration)?,
            )?;
            let custody =
                SoftwareCustodyKey::from_bytes(key_id.purpose, key_id.jurisdiction, secret)?;
            secret.zeroize();
            Ok(custody.public_key())
        }
    }
}

/// Independently authenticate the selected custody key from a complete registry dump. The
/// checkpoint proves the root; replaying only a projected key row is insufficient because it could
/// omit a later revocation. The compact frontier recomputes both the pinned prefix and final root
/// without retaining the full identity/name log in memory.
fn verify_registry_key(
    config: &RegistryAuditConfig,
    requested_key_id: ComplianceKeyId,
    custody_public_key: [u8; 32],
    now_ms: u64,
) -> OperatorResult<RegistryKeyEvidence> {
    let note_bytes = read_artifact(&config.checkpoint_path, REGISTRY_CHECKPOINT_MAX_BYTES)?;
    let note = core::str::from_utf8(&note_bytes).map_err(|_| OperatorError::Authorization)?;
    let checkpoint_key = config.checkpoint_key()?;
    let witnesses = config.witness_keys()?;
    let verified = Checkpoint::verify_with_fresh_witnesses(
        note,
        &checkpoint_key,
        &witnesses,
        config.witness_threshold,
        now_ms / 1_000,
        config.max_cosignature_age_seconds,
        config.future_clock_skew_seconds,
    )
    .map_err(|_| OperatorError::Authorization)?;
    let checkpoint = verified.checkpoint;
    if checkpoint.origin != config.expected_origin
        || checkpoint.size > MAX_REGISTRY_AUDIT_ENTRIES
        || checkpoint.size < config.minimum_checkpoint_size
    {
        return Err(OperatorError::Authorization);
    }

    let minimum_root = parse_canonical_hex32(&config.minimum_checkpoint_root)?;
    #[cfg(unix)]
    let log_guard = open_guarded_regular(&config.log_path, false, REGISTRY_AUDIT_MAX_BYTES, false)?;
    #[cfg(unix)]
    let file = log_guard
        .file()
        .try_clone()
        .map_err(|_| OperatorError::Storage)?;
    #[cfg(not(unix))]
    let file = open_existing_regular(&config.log_path, false, REGISTRY_AUDIT_MAX_BYTES, false)?;
    let expected_bytes = file.metadata().map_err(|_| OperatorError::Storage)?.len();
    let mut reader = BufReader::new(file);
    let mut frontier = MerkleFrontier::new();
    if config.minimum_checkpoint_size == 0 && frontier.root() != Some(minimum_root) {
        return Err(OperatorError::Authorization);
    }
    let mut keys = HashMap::new();
    let mut bytes_read = 0u64;
    loop {
        let mut line = Vec::new();
        let read = (&mut reader)
            .take(REGISTRY_AUDIT_ENTRY_MAX_BYTES.saturating_add(1))
            .read_until(b'\n', &mut line)
            .map_err(|_| OperatorError::Storage)?;
        if read == 0 {
            break;
        }
        bytes_read = bytes_read
            .checked_add(read as u64)
            .filter(|total| *total <= REGISTRY_AUDIT_MAX_BYTES)
            .ok_or(OperatorError::Limit)?;
        if line.len() as u64 > REGISTRY_AUDIT_ENTRY_MAX_BYTES
            || line.last() != Some(&b'\n')
            || line.get(line.len().saturating_sub(2)) == Some(&b'\r')
        {
            return Err(OperatorError::Authorization);
        }
        line.pop();
        let entry: LogEntry =
            serde_json::from_slice(&line).map_err(|_| OperatorError::Authorization)?;
        let expected_index = frontier.size();
        if entry.seq() != expected_index || expected_index >= checkpoint.size {
            return Err(OperatorError::Authorization);
        }
        let leaf = entry
            .leaf_bytes()
            .map_err(|_| OperatorError::Authorization)?;
        if frontier.append(&leaf) != Some(expected_index) {
            return Err(OperatorError::Authorization);
        }
        if let Some(publication) = entry.compliance_publication() {
            apply_registry_publication(&mut keys, publication, expected_index)?;
        }
        if frontier.size() == config.minimum_checkpoint_size
            && frontier.root() != Some(minimum_root)
        {
            return Err(OperatorError::Authorization);
        }
    }
    #[cfg(unix)]
    let name_stable = log_guard.verify_named().is_ok()
        && log_guard
            .metadata()
            .is_ok_and(|metadata| metadata.len == expected_bytes);
    #[cfg(not(unix))]
    let name_stable = named_file_matches(reader.get_ref(), &config.log_path);
    if bytes_read != expected_bytes
        || !name_stable
        || frontier.size() != checkpoint.size
        || frontier.root() != Some(checkpoint.root)
    {
        return Err(OperatorError::Authorization);
    }

    let key = keys
        .get(&requested_key_id)
        .ok_or(OperatorError::Authorization)?;
    // Retired epochs remain disclosable for their historical retention window. Full-log
    // transition validation below proves they were first published Active; Revoked never is.
    if key.public_key != custody_public_key
        || key.publication.status == ComplianceKeyStatus::Revoked
    {
        return Err(OperatorError::Authorization);
    }
    Ok(RegistryKeyEvidence {
        not_before_ms: key.publication.not_before_ms,
        not_after_ms: key.publication.not_after_ms,
        public_key: key.public_key,
    })
}

fn apply_registry_publication(
    keys: &mut HashMap<ComplianceKeyId, AuditedRegistryKey>,
    publication: &ComplianceKeyPublish,
    log_index: u64,
) -> OperatorResult<()> {
    publication
        .key_id
        .encode()
        .map_err(|_| OperatorError::Authorization)?;
    let public_key = decode_hex_array::<32>(&publication.public_key)
        .map_err(|_| OperatorError::Authorization)?;
    if public_key == [0u8; 32]
        || hex_encode(&public_key) != publication.public_key
        || validate_compliance_epoch(
            &publication.key_id,
            publication.not_before_ms,
            publication.not_after_ms,
        )
        .is_err()
    {
        return Err(OperatorError::Authorization);
    }
    let next = AuditedRegistryKey {
        publication: publication.clone(),
        public_key,
        log_index,
    };
    if let Some(previous) = keys.get(&publication.key_id) {
        if next.log_index <= previous.log_index
            || next.public_key != previous.public_key
            || next.publication.not_before_ms != previous.publication.not_before_ms
            || next.publication.not_after_ms != previous.publication.not_after_ms
            || !matches!(
                (previous.publication.status, next.publication.status),
                (ComplianceKeyStatus::Active, ComplianceKeyStatus::Retired)
                    | (ComplianceKeyStatus::Active, ComplianceKeyStatus::Revoked)
                    | (ComplianceKeyStatus::Retired, ComplianceKeyStatus::Revoked)
            )
        {
            return Err(OperatorError::Authorization);
        }
    } else {
        if publication.status != ComplianceKeyStatus::Active {
            return Err(OperatorError::Authorization);
        }
        if keys.len() >= MAX_REGISTRY_AUDIT_KEYS {
            return Err(OperatorError::Limit);
        }
    }
    keys.insert(publication.key_id, next);
    Ok(())
}

fn prepare_artifacts(
    epoch: &EpochConfig,
    key_id: ComplianceKeyId,
    registry_key: &RegistryKeyEvidence,
    observed_at_ms: u64,
) -> OperatorResult<PreparedArtifacts> {
    match epoch.artifact.kind {
        ArtifactKind::TraceSegments => {
            if validate_trace_epoch(
                &key_id,
                registry_key.not_before_ms,
                registry_key.not_after_ms,
            )
            .is_err()
            {
                return Err(OperatorError::Authorization);
            }
            let expected_custody_digest: [u8; 32] = Sha256::digest(registry_key.public_key).into();
            let (trace, expected_node_id) = open_trace_epoch(epoch, key_id, observed_at_ms)?;
            if trace.key_id() != key_id
                || parse_canonical_hex32(
                    epoch
                        .artifact
                        .expected_custody_key_digest
                        .as_deref()
                        .ok_or(OperatorError::Configuration)?,
                )? != expected_custody_digest
            {
                return Err(OperatorError::Authorization);
            }
            trace.for_each_segment(|_, segment| {
                if !registry_key.permits_timestamp(segment.header.opened_at_ms())
                    || !registry_key.permits_timestamp(segment.footer.closed_at_ms())
                {
                    return Err(ComplianceError::Unauthorized);
                }
                Ok(())
            })?;
            Ok(PreparedArtifacts::Trace {
                epoch: Box::new(trace),
                expected_node_id,
            })
        }
        ArtifactKind::AttributionWraps => {
            let mut wraps = Vec::with_capacity(epoch.artifact.paths.len());
            let mut total_bytes = 0u64;
            for path in &epoch.artifact.paths {
                let bytes = read_artifact(path, WRAP_MAX_BYTES)?;
                total_bytes = total_bytes
                    .checked_add(bytes.len() as u64)
                    .filter(|total| *total <= MAX_TOTAL_ATTRIBUTION_ARTIFACT_BYTES)
                    .ok_or(OperatorError::Limit)?;
                let wrap: Wrap =
                    serde_json::from_slice(&bytes).map_err(|_| OperatorError::Artifact)?;
                wrap.verify_public().map_err(|_| OperatorError::Artifact)?;
                if !matches!(
                    wrap.attribution.as_ref(),
                    Some(AttributionBlock::V3(block)) if block.key_id == key_id
                ) {
                    return Err(OperatorError::Artifact);
                }
                wraps.push(wrap);
            }
            Ok(PreparedArtifacts::Attribution(wraps))
        }
    }
}

fn authenticate_trace_epoch(
    epoch: &EpochConfig,
    key_id: ComplianceKeyId,
    observed_at_ms: u64,
) -> OperatorResult<Option<AuthenticatedTraceEpoch>> {
    if epoch.artifact.kind != ArtifactKind::TraceSegments {
        return Ok(None);
    }
    let (trace, _) = open_trace_epoch(epoch, key_id, observed_at_ms)?;
    Ok(Some(trace))
}

#[derive(Clone, Copy)]
struct ShredTraceAssessment {
    manifest_commitment: [u8; 32],
    integrity: TraceBodyIntegrity,
}

impl ShredTraceAssessment {
    fn is_degraded(&self) -> bool {
        self.integrity == TraceBodyIntegrity::Degraded
    }
}

#[derive(Clone, Copy)]
struct ShredCandidate<'a> {
    epoch: &'a EpochConfig,
    key_id: ComplianceKeyId,
    trace: Option<ShredTraceAssessment>,
}

fn assess_configured_trace_epoch(
    epoch: &EpochConfig,
    key_id: ComplianceKeyId,
    observed_at_ms: u64,
) -> OperatorResult<Option<ShredTraceAssessment>> {
    if epoch.artifact.kind != ArtifactKind::TraceSegments {
        return Ok(None);
    }
    let (trace, _) = open_trace_epoch_for_destruction(epoch, key_id, observed_at_ms)?;
    let integrity = trace.assess_body_integrity()?;
    Ok(Some(ShredTraceAssessment {
        manifest_commitment: trace.manifest_commitment(),
        integrity,
    }))
}

fn open_trace_epoch(
    epoch: &EpochConfig,
    key_id: ComplianceKeyId,
    observed_at_ms: u64,
) -> OperatorResult<(AuthenticatedTraceEpoch, [u8; 32])> {
    open_trace_epoch_inner(epoch, key_id, observed_at_ms, false)
}

fn open_trace_epoch_for_destruction(
    epoch: &EpochConfig,
    key_id: ComplianceKeyId,
    observed_at_ms: u64,
) -> OperatorResult<(AuthenticatedTraceEpoch, [u8; 32])> {
    open_trace_epoch_inner(epoch, key_id, observed_at_ms, true)
}

fn open_trace_epoch_inner(
    epoch: &EpochConfig,
    key_id: ComplianceKeyId,
    observed_at_ms: u64,
    destruction: bool,
) -> OperatorResult<(AuthenticatedTraceEpoch, [u8; 32])> {
    let expected_node_id = parse_canonical_hex32(
        epoch
            .artifact
            .expected_node_id
            .as_deref()
            .ok_or(OperatorError::Configuration)?,
    )?;
    let expected_signer = parse_canonical_hex32(
        epoch
            .artifact
            .expected_signer_public_key
            .as_deref()
            .ok_or(OperatorError::Configuration)?,
    )?;
    let directory = epoch
        .artifact
        .directory
        .as_ref()
        .ok_or(OperatorError::Configuration)?;
    let custody_key_digest = parse_canonical_hex32(
        epoch
            .artifact
            .expected_custody_key_digest
            .as_deref()
            .ok_or(OperatorError::Configuration)?,
    )?;
    let expectation = TraceEpochExpectation {
        key_id,
        producer_node_id: expected_node_id,
        signer_public_key: expected_signer,
        custody_key_digest,
        observed_at_ms,
    };
    let trace = if destruction {
        AuthenticatedTraceEpoch::open_for_destruction(directory, expectation)
    } else {
        AuthenticatedTraceEpoch::open(directory, expectation)
    }
    .map_err(|_| OperatorError::Artifact)?;
    Ok((trace, expected_node_id))
}

struct AuthorizationEvidence {
    approver_indices: [usize; 2],
    approvals: [LegalHoldApproval; 2],
}

fn authorize_request(
    request: &mut DisclosureRequest,
    config: &ApprovalConfig,
    order_reference: &[u8],
    requester: &[u8],
    selectors: &[u8],
    now_ms: u64,
    runner: &dyn CommandRunner,
) -> OperatorResult<AuthorizationEvidence> {
    let mut input = Zeroizing::new(Vec::new());
    input.extend_from_slice(APPROVAL_REQUEST_MAGIC);
    input.push(ADAPTER_PROTOCOL_VERSION);
    input.extend_from_slice(&request.request_id());
    input.push(request.jurisdiction().into());
    input.push(request.purpose().into());
    input.extend_from_slice(&request.created_at_ms().to_be_bytes());
    input.extend_from_slice(&request.expires_at_ms().to_be_bytes());
    input.push(request.key_ids().len() as u8);
    for key_id in request.key_ids() {
        input.extend_from_slice(&key_id.encode().map_err(|_| OperatorError::Authorization)?);
    }
    push_private_field(&mut input, order_reference)?;
    push_private_field(&mut input, requester)?;
    push_private_field(&mut input, selectors)?;
    let expected_len = APPROVAL_RESPONSE_MAGIC.len() + 1 + 2 * (32 + 8 + 64);
    let response = Zeroizing::new(
        runner
            .run(&config.command, &input, expected_len)
            .map_err(|_| OperatorError::Authorization)?,
    );
    if response.len() != expected_len
        || &response[..8] != APPROVAL_RESPONSE_MAGIC
        || response[8] != ADAPTER_PROTOCOL_VERSION
    {
        return Err(OperatorError::Authorization);
    }
    let mut cursor = 9;
    let mut indices = [usize::MAX; 2];
    let mut approvals = Vec::with_capacity(2);
    for slot_index in 0..indices.len() {
        let public_key: [u8; 32] = response[cursor..cursor + 32]
            .try_into()
            .expect("checked exact response");
        cursor += 32;
        let approved_at_ms = u64::from_be_bytes(
            response[cursor..cursor + 8]
                .try_into()
                .expect("checked exact response"),
        );
        cursor += 8;
        let signature: [u8; 64] = response[cursor..cursor + 64]
            .try_into()
            .expect("checked exact response");
        cursor += 64;
        let index = config
            .approvers
            .iter()
            .position(|candidate| {
                decode_hex_array::<32>(&candidate.public_key).ok() == Some(public_key)
            })
            .ok_or(OperatorError::Authorization)?;
        if indices[..slot_index].contains(&index)
            || approved_at_ms < request.created_at_ms()
            || approved_at_ms > now_ms.saturating_add(5 * 60 * 1_000)
            || approved_at_ms > request.expires_at_ms()
        {
            return Err(OperatorError::Authorization);
        }
        request
            .approve(public_key, approved_at_ms, signature)
            .map_err(|_| OperatorError::Authorization)?;
        approvals.push(
            LegalHoldApproval::new(public_key, approved_at_ms, signature)
                .map_err(|_| OperatorError::Authorization)?,
        );
        indices[slot_index] = index;
    }
    Ok(AuthorizationEvidence {
        approver_indices: indices,
        approvals: approvals.try_into().expect("exact approval count"),
    })
}

fn push_private_field(out: &mut Vec<u8>, value: &[u8]) -> OperatorResult<()> {
    if value.is_empty() || value.len() > MAX_PRIVATE_FIELD_BYTES {
        return Err(OperatorError::Authorization);
    }
    let length: u16 = value
        .len()
        .try_into()
        .map_err(|_| OperatorError::Authorization)?;
    out.extend_from_slice(&length.to_be_bytes());
    out.extend_from_slice(value);
    Ok(())
}

fn runtime_custody<'a>(
    epoch: &'a EpochConfig,
    key_id: ComplianceKeyId,
    request_id: [u8; 32],
    runner: &'a dyn CommandRunner,
) -> OperatorResult<RuntimeCustody<'a>> {
    match epoch.custody.mode {
        CustodyMode::External => Ok(RuntimeCustody::External(ProcessCustodyBackend {
            purpose: key_id.purpose,
            jurisdiction: key_id.jurisdiction,
            key_id,
            public_key: parse_canonical_hex32(
                epoch
                    .custody
                    .public_key
                    .as_deref()
                    .ok_or(OperatorError::Configuration)?,
            )?,
            request_id,
            command: epoch
                .custody
                .command
                .as_ref()
                .ok_or(OperatorError::Configuration)?,
            runner,
        })),
        CustodyMode::SoftwareDevelopment => {
            let mut secret = read_exact_secret(
                epoch
                    .custody
                    .secret_key_path
                    .as_ref()
                    .ok_or(OperatorError::Configuration)?,
            )?;
            let custody =
                SoftwareCustodyKey::from_bytes(key_id.purpose, key_id.jurisdiction, secret)?;
            secret.zeroize();
            Ok(RuntimeCustody::Software(custody))
        }
    }
}

fn disclose_selected(
    authorization: &crate::AuthorizedDisclosure,
    key_id: ComplianceKeyId,
    custody: &impl CustodyBackend,
    artifacts: &PreparedArtifacts,
    selectors: &SelectorSet,
    registry_key: &RegistryKeyEvidence,
) -> crate::Result<(Zeroizing<Vec<u8>>, u32)> {
    let mut output = Zeroizing::new(Vec::new());
    let mut count = 0usize;
    match artifacts {
        PreparedArtifacts::Trace {
            epoch,
            expected_node_id,
        } => {
            epoch.for_each_segment(|_, segment| {
                let records = disclose_trace_segment_selected_bounded(
                    authorization,
                    key_id.purpose,
                    key_id.jurisdiction,
                    custody,
                    segment,
                    *expected_node_id,
                    MAX_DISCLOSED_RECORDS.saturating_sub(count),
                    |record| selectors.matches_trace(record),
                )?;
                for record in records {
                    let timestamp_ms = match &record {
                        DisclosedTraceRecord::Network(record) => record.timestamp_ms,
                        DisclosedTraceRecord::Identity(record) => record.timestamp_ms,
                    };
                    if !registry_key.permits_timestamp(timestamp_ms) {
                        return Err(ComplianceError::Unauthorized);
                    }
                    append_trace_json(&mut output, &record)?;
                    count += 1;
                    enforce_disclosure_bounds(count, output.len())?;
                }
                Ok(())
            })?;
        }
        PreparedArtifacts::Attribution(wraps) => {
            for wrap in wraps {
                if !selectors.matches_attribution_public(wrap) {
                    continue;
                }
                let disclosure = unseal_attribution(authorization, &key_id, custody, wrap)?;
                if !registry_key.permits_timestamp(disclosure.sent_at_ms) {
                    return Err(ComplianceError::Unauthorized);
                }
                let value = json!({
                    "kind": "attribution",
                    "key_id": hex_encode(&disclosure.key_id.encode().map_err(|_| ComplianceError::InvalidRequest)?),
                    "event_id": hex_encode(&disclosure.event_id),
                    "recipient": hex_encode(&disclosure.recipient),
                    "sender_public_key": hex_encode(&disclosure.sender_public_key),
                    "sent_at_ms": disclosure.sent_at_ms,
                });
                append_json_line(&mut output, value)?;
                count += 1;
                enforce_disclosure_bounds(count, output.len())?;
            }
        }
    }
    Ok((output, count as u32))
}

fn append_trace_json(output: &mut Vec<u8>, record: &DisclosedTraceRecord) -> crate::Result<()> {
    let value = match record {
        DisclosedTraceRecord::Network(record) => {
            let source = match record.source_ip {
                TraceIp::V4(value) => IpAddr::V4(value).to_string(),
                TraceIp::V6(value) => IpAddr::V6(value).to_string(),
            };
            json!({
                "kind": "network_trace",
                "jurisdiction": jurisdiction_name(record.jurisdiction),
                "operation": operation_name(record.operation),
                "timestamp_ms": record.timestamp_ms,
                "node_id": hex_encode(&record.node_id),
                "source_address": source,
                "source_port": record.source_port,
                "event_id": record.event_id.map(|value| hex_encode(&value)),
                "recipient": record.recipient.map(|value| hex_encode(&value)),
                "owner": record.owner.map(|value| hex_encode(&value)),
                "size_bytes": record.size_bytes,
                "correlation_commitment": record.correlation_id.map(|value| hex_encode(&value)),
            })
        }
        DisclosedTraceRecord::Identity(record) => json!({
            "kind": "identity_trace",
            "jurisdiction": jurisdiction_name(record.jurisdiction),
            "timestamp_ms": record.timestamp_ms,
            "node_id": hex_encode(&record.node_id),
            "correlation_commitment": hex_encode(&record.correlation_id),
            "provider": provider_name(record.provider),
            "provider_subject": record.provider_subject,
        }),
    };
    append_json_line(output, value)
}

fn append_json_line(output: &mut Vec<u8>, value: serde_json::Value) -> crate::Result<()> {
    serde_json::to_writer(&mut *output, &value).map_err(|_| ComplianceError::Storage)?;
    output.push(b'\n');
    Ok(())
}

fn enforce_disclosure_bounds(count: usize, bytes: usize) -> crate::Result<()> {
    if count > MAX_DISCLOSED_RECORDS || bytes > MAX_DISCLOSURE_OUTPUT_BYTES {
        Err(ComplianceError::LimitExceeded)
    } else {
        Ok(())
    }
}

fn jurisdiction_name(value: Jurisdiction) -> &'static str {
    match value {
        Jurisdiction::Us => "us",
        Jurisdiction::Eu => "eu",
        Jurisdiction::Tr => "tr",
        Jurisdiction::Test => "test",
    }
}

fn provider_name(value: IdentityProvider) -> &'static str {
    match value {
        IdentityProvider::Oidc => "oidc",
        IdentityProvider::Saml => "saml",
        IdentityProvider::LocalDirectory => "local_directory",
        IdentityProvider::Oauth2 => "oauth2",
    }
}

fn execute_destruction(
    command: &CommandConfig,
    key_id: ComplianceKeyId,
    copy_id: [u8; 32],
    kind: u8,
    runner: &dyn CommandRunner,
) -> OperatorResult<(bool, [u8; 32])> {
    let mut input = Vec::with_capacity(8 + 1 + COMPLIANCE_KEY_ID_LEN + 32 + 1);
    input.extend_from_slice(DESTRUCTION_REQUEST_MAGIC);
    input.push(ADAPTER_PROTOCOL_VERSION);
    input.extend_from_slice(&key_id.encode().map_err(|_| OperatorError::Configuration)?);
    input.extend_from_slice(&copy_id);
    input.push(kind);
    let expected = DESTRUCTION_RESPONSE_MAGIC.len() + 1 + 1 + 32;
    let response = runner.run(command, &input, expected)?;
    if response.len() != expected
        || &response[..8] != DESTRUCTION_RESPONSE_MAGIC
        || response[8] != ADAPTER_PROTOCOL_VERSION
    {
        return Err(OperatorError::Custody);
    }
    let destroyed = match response[9] {
        1 => true,
        2 => false,
        _ => return Err(OperatorError::Custody),
    };
    let commitment: [u8; 32] = response[10..].try_into().expect("checked exact length");
    if commitment == [0u8; 32] {
        return Err(OperatorError::Custody);
    }
    Ok((destroyed, commitment))
}

fn inventory_from_declaration(
    epoch: &EpochConfig,
    expected: ComplianceKeyId,
    policy: RetentionPolicy,
) -> OperatorResult<DestructionInventory> {
    let parent = epoch
        .inventory_declaration_path
        .parent()
        .ok_or(OperatorError::Storage)?;
    validate_private_directory(parent, false)?;
    let bytes = read_private_file(
        &epoch.inventory_declaration_path,
        INVENTORY_DECLARATION_MAX_BYTES,
    )?;
    let text = core::str::from_utf8(&bytes).map_err(|_| OperatorError::State)?;
    let declaration: InventoryDeclaration =
        toml::from_str(text).map_err(|_| OperatorError::State)?;
    if declaration.version != INVENTORY_DECLARATION_VERSION
        || declaration.created_at_ms != expected.epoch_start_ms
        || declaration.copies.is_empty()
        || declaration.copies.len() > MAX_DECLARED_COPIES
        || parse_key_id(&declaration.key_id).ok() != Some(expected)
    {
        return Err(OperatorError::State);
    }

    let mut nonces = Vec::with_capacity(declaration.copies.len());
    let mut copies = Vec::with_capacity(declaration.copies.len());
    for copy in declaration.copies {
        let nonce = decode_hex_array::<32>(&copy.nonce).map_err(|_| OperatorError::State)?;
        if nonce == [0u8; 32]
            || hex_encode(&nonce) != copy.nonce
            || nonces.contains(&nonce)
            || copy.private_material.as_bytes().is_empty()
            || copy.private_material.as_bytes().len() > MAX_PRIVATE_FIELD_BYTES
        {
            return Err(OperatorError::State);
        }
        nonces.push(nonce);
        let kind = KeyCopyKind::from(copy.kind);
        let declared = match copy.state {
            DeclaredCopyState::Present => {
                KeyCopy::present_with_nonce(kind, nonce, copy.private_material.as_bytes())
            }
            DeclaredCopyState::VerifiedAbsent => {
                KeyCopy::verified_absent_with_nonce(kind, nonce, copy.private_material.as_bytes())
            }
        }
        .map_err(|_| OperatorError::State)?;
        copies.push(declared);
    }
    DestructionInventory::new(expected, declaration.created_at_ms, policy, copies)
        .map_err(|_| OperatorError::State)
}

struct LoadedInventory {
    value: DestructionInventory,
    #[cfg(unix)]
    directory: GuardedDir,
    #[cfg(unix)]
    name: LeafName,
    #[cfg(unix)]
    guard: GuardedFile,
}

impl core::ops::Deref for LoadedInventory {
    type Target = DestructionInventory;

    fn deref(&self) -> &Self::Target {
        &self.value
    }
}

impl core::ops::DerefMut for LoadedInventory {
    fn deref_mut(&mut self) -> &mut Self::Target {
        &mut self.value
    }
}

fn load_inventory(
    path: &Path,
    expected: ComplianceKeyId,
    expected_policy: RetentionPolicy,
) -> OperatorResult<LoadedInventory> {
    let inventory = load_inventory_without_policy(path, expected)?;
    if inventory.retention_policy() != expected_policy {
        return Err(OperatorError::State);
    }
    Ok(inventory)
}

fn load_inventory_without_policy(
    path: &Path,
    expected: ComplianceKeyId,
) -> OperatorResult<LoadedInventory> {
    #[cfg(unix)]
    let (guard, bytes) = {
        let guard = open_guarded_regular(path, true, INVENTORY_MAX_BYTES, false)?;
        guard.verify_named().map_err(map_operator_custody)?;
        let metadata = guard.metadata().map_err(map_operator_custody)?;
        let mut file = guard
            .file()
            .try_clone()
            .map_err(|_| OperatorError::Storage)?;
        use std::io::{Seek, SeekFrom};
        file.seek(SeekFrom::Start(0))
            .map_err(|_| OperatorError::Storage)?;
        let mut bytes = Zeroizing::new(Vec::with_capacity(
            usize::try_from(metadata.len).map_err(|_| OperatorError::Limit)?,
        ));
        file.take(INVENTORY_MAX_BYTES + 1)
            .read_to_end(&mut bytes)
            .map_err(|_| OperatorError::Storage)?;
        guard.verify_named().map_err(map_operator_custody)?;
        if bytes.len() as u64 != metadata.len
            || guard.metadata().map_err(map_operator_custody)? != metadata
        {
            return Err(OperatorError::Storage);
        }
        (guard, bytes)
    };
    #[cfg(not(unix))]
    let bytes = {
        validate_private_directory(path.parent().ok_or(OperatorError::Storage)?, false)?;
        read_private_file(path, INVENTORY_MAX_BYTES)?
    };
    let inventory = DestructionInventory::decode(&bytes).map_err(|_| OperatorError::State)?;
    if inventory.key_id() != expected {
        return Err(OperatorError::State);
    }
    Ok(LoadedInventory {
        value: inventory,
        #[cfg(unix)]
        directory: guard.parent().clone(),
        #[cfg(unix)]
        name: guard.name().clone(),
        #[cfg(unix)]
        guard,
    })
}

fn persist_inventory(path: &Path, inventory: &mut LoadedInventory) -> OperatorResult<()> {
    let encoded = inventory.encode()?;
    #[cfg(unix)]
    {
        inventory
            .guard
            .verify_named()
            .map_err(map_operator_custody)?;
        let mut random = [0u8; 16];
        OsRng.fill_bytes(&mut random);
        let temp_name = LeafName::new(format!(
            ".{}.{}.tmp",
            inventory.name.as_os_str().to_string_lossy(),
            hex_encode(&random)
        ))
        .map_err(map_operator_custody)?;
        let mut temp = inventory
            .directory
            .create_file(&temp_name, FilePolicy::private(encoded.len() as u64))
            .map_err(map_operator_custody)?;
        let cleanup = inventory
            .directory
            .open_file(
                &temp_name,
                OpenAccess::ReadOnly,
                FilePolicy::private(encoded.len() as u64),
            )
            .map_err(map_operator_custody)?;
        let result = (|| {
            temp.write_all(&encoded)
                .map_err(|_| OperatorError::Storage)?;
            temp.sync_all().map_err(map_operator_custody)?;
            inventory
                .guard
                .verify_named()
                .map_err(map_operator_custody)?;
            let published = inventory
                .directory
                .rename_replace(temp, &inventory.directory, &inventory.name)
                .map_err(map_operator_custody)?;
            published.verify_named().map_err(map_operator_custody)?;
            if published.metadata().map_err(map_operator_custody)?.len != encoded.len() as u64 {
                return Err(OperatorError::Storage);
            }
            inventory.directory.sync().map_err(map_operator_custody)?;
            inventory.guard = published;
            Ok(())
        })();
        if result.is_err() {
            let _ = inventory.directory.unlink_file(cleanup);
        }
        let _ = path;
        result
    }
    #[cfg(not(unix))]
    write_private_atomic(path, &encoded, true)
}

fn read_exact_secret(path: &Path) -> OperatorResult<[u8; 32]> {
    let mut bytes = read_private_file(path, SECRET_KEY_BYTES)?;
    if bytes.len() != SECRET_KEY_BYTES as usize {
        return Err(OperatorError::Configuration);
    }
    let secret: [u8; 32] = bytes[..].try_into().expect("checked exact length");
    bytes.zeroize();
    if secret == [0u8; 32] {
        return Err(OperatorError::Configuration);
    }
    Ok(secret)
}

fn open_or_create_ledger(path: &Path, state_secret: &[u8; 32]) -> OperatorResult<DisclosureLedger> {
    if regular_entry_exists(path, true)? {
        validate_regular_file(path, true, 512 * 1024 * 1024)?;
        DisclosureLedger::open(path, state_secret).map_err(Into::into)
    } else {
        let parent = path.parent().ok_or(OperatorError::Storage)?;
        validate_private_directory(parent, true)?;
        DisclosureLedger::create(path, state_secret).map_err(Into::into)
    }
}

fn read_disclosure_checkpoint_floor(
    config: &OperatorConfig,
    verifying_key: &VerifyingKey,
) -> OperatorResult<Option<Checkpoint>> {
    if !regular_entry_exists(&config.checkpoint_output_path, true)? {
        return Ok(None);
    }
    let bytes = read_private_file(
        &config.checkpoint_output_path,
        DISCLOSURE_CHECKPOINT_MAX_BYTES,
    )?;
    let note = core::str::from_utf8(&bytes).map_err(|_| OperatorError::State)?;
    let checkpoint = Checkpoint::verify(note, verifying_key).map_err(|_| OperatorError::State)?;
    if checkpoint.origin != config.checkpoint_origin
        || (checkpoint.size == 0 && checkpoint.root != pigeonpost_registry::log::empty_root())
    {
        return Err(OperatorError::State);
    }
    Ok(Some(checkpoint))
}

fn verify_empty_disclosure_checkpoint_floor(
    config: &OperatorConfig,
    verifying_key: &VerifyingKey,
) -> OperatorResult<()> {
    if read_disclosure_checkpoint_floor(config, verifying_key)?
        .is_some_and(|checkpoint| checkpoint.size != 0)
    {
        return Err(OperatorError::State);
    }
    Ok(())
}

fn verify_disclosure_checkpoint_floor(
    config: &OperatorConfig,
    ledger: &DisclosureLedger,
    verifying_key: &VerifyingKey,
) -> OperatorResult<()> {
    let Some(previous) = read_disclosure_checkpoint_floor(config, verifying_key)? else {
        return if ledger.leaf_count() == 0 {
            Ok(())
        } else {
            Err(OperatorError::State)
        };
    };
    let current_size = ledger.leaf_count();
    let current_root = ledger.root();
    if previous.size > current_size
        || (previous.size == current_size && previous.root != current_root)
    {
        return Err(OperatorError::State);
    }
    if previous.size == 0 || previous.size == current_size {
        return Ok(());
    }
    let proof = ledger
        .consistency_proof(previous.size)?
        .ok_or(OperatorError::State)?;
    if !verify_consistency(
        previous.size,
        &previous.root,
        current_size,
        &current_root,
        &proof,
    ) {
        return Err(OperatorError::State);
    }
    Ok(())
}

fn publish_disclosure_checkpoint(
    config: &OperatorConfig,
    ledger: &DisclosureLedger,
    signing_key: &SigningKey,
) -> OperatorResult<String> {
    verify_disclosure_checkpoint_floor(config, ledger, &signing_key.verifying_key())?;
    let signed = ledger
        .checkpoint(config.checkpoint_origin.clone())
        .sign(signing_key);
    write_private_atomic(&config.checkpoint_output_path, signed.as_bytes(), true)?;
    Ok(signed)
}

fn parse_cli_key_id(value: &str) -> OperatorResult<ComplianceKeyId> {
    parse_key_id(value).map_err(|_| OperatorError::Usage)
}

fn parse_hold_id(value: &str) -> OperatorResult<[u8; 32]> {
    let id = decode_hex_array::<32>(value).map_err(|_| OperatorError::Usage)?;
    if id == [0u8; 32] || hex_encode(&id) != value {
        return Err(OperatorError::Usage);
    }
    Ok(id)
}

fn help_text() -> &'static str {
    "ppcompliance status\n\
ppcompliance inventory create --epoch <id>\n\
ppcompliance inventory provision --epoch <id>\n\
ppcompliance inventory import --epoch <id>\n\
ppcompliance inventory update --epoch <id>\n\
ppcompliance unseal --epoch <id> < private-request.toml\n\
ppcompliance shred --before <YYYY-MM-DD> [--dry-run|--execute]\n\
ppcompliance hold --epoch <id> --until <YYYY-MM-DD> < private-request.toml\n\
ppcompliance hold renew --epoch <id> --hold <id> --until <YYYY-MM-DD> < private-request.toml\n\
ppcompliance hold release --epoch <id> --hold <id> < private-request.toml\n\
ppcompliance checkpoint\n\n\
The compliance home must be an absolute 0700 directory named by\n\
PIGEONPOST_COMPLIANCE_HOME. Unseal and hold read a bounded version-1 TOML request from stdin;\n\
raw order references, selectors, and requester identities are rejected in argv and the\n\
environment. Inventory commands read only the distinct private paths\n\
configured for that epoch; never pass custody locators as arguments. Create stages,\n\
provision/import refuse replacement, and update is monotonic. Shred is a dry-run unless\n\
--execute is explicit.\n"
}

#[cfg(test)]
mod tests {
    use std::io::Read;
    use std::net::Ipv4Addr;
    use std::sync::{Arc, Mutex};

    use curve25519_dalek::montgomery::MontgomeryPoint;
    use ed25519_dalek::Signer;
    use pigeonpost_compliance_seal::{
        epoch_manifest_path, publish_epoch_manifest, EpochManifest, EpochSealingKey,
        EpochSegmentEntry, SegmentWriter, TraceRecord,
    };
    use tempfile::TempDir;

    use super::*;
    use crate::{CompletionStatus, DisclosureLeaf};

    const DAY_MS: u64 = 86_400_000;
    const TEST_NOW: u64 = DAY_MS + 1_000;

    struct PanicInput;

    impl Read for PanicInput {
        fn read(&mut self, _buffer: &mut [u8]) -> std::io::Result<usize> {
            panic!("unsupported operator must not read private input")
        }
    }

    #[test]
    fn unsupported_platform_rejects_operations_before_env_config_or_private_input() {
        let key_id = ComplianceKeyId::new(
            CompliancePurpose::NetworkTrace,
            Jurisdiction::Test,
            [7; 32],
            DAY_MS,
            1,
        );
        let epoch = hex_encode(&key_id.encode().unwrap());
        let mut input = PanicInput;
        let mut output = Vec::new();

        assert_eq!(
            run_from_env_for_platform(
                crate::platform::OfflinePlatform::unsupported_for_test(),
                [
                    OsString::from("unseal"),
                    OsString::from("--epoch"),
                    OsString::from(epoch),
                ],
                &mut input,
                &mut output,
            ),
            Err(OperatorError::UnsupportedPlatform)
        );
        assert!(output.is_empty());

        run_from_env_for_platform(
            crate::platform::OfflinePlatform::unsupported_for_test(),
            [OsString::from("--version")],
            &mut input,
            &mut output,
        )
        .unwrap();
        assert!(String::from_utf8(output)
            .unwrap()
            .starts_with("ppcompliance "));
    }

    fn registry_audit_config(count: usize, threshold: usize) -> RegistryAuditConfig {
        let checkpoint = SigningKey::from_bytes(&[51; 32]);
        RegistryAuditConfig {
            log_path: PathBuf::from("registry.ndjson"),
            checkpoint_path: PathBuf::from("registry.checkpoint"),
            expected_origin: "registry.test/log".into(),
            checkpoint_key: hex_encode(checkpoint.verifying_key().as_bytes()),
            witnesses: (0..count)
                .map(|index| {
                    let seed = u8::try_from(index + 52).unwrap();
                    let witness = SigningKey::from_bytes(&[seed; 32]);
                    RegistryWitnessConfig {
                        name: format!("witness-{index}"),
                        public_key: hex_encode(witness.verifying_key().as_bytes()),
                    }
                })
                .collect(),
            witness_threshold: threshold,
            minimum_checkpoint_size: 0,
            minimum_checkpoint_root: hex_encode(&pigeonpost_registry::log::empty_root()),
            max_cosignature_age_seconds: 60,
            future_clock_skew_seconds: 5,
        }
    }

    #[test]
    fn offline_registry_audit_requires_a_strictly_intersecting_quorum() {
        assert!(registry_audit_config(1, 1).validate().is_ok());
        assert!(registry_audit_config(3, 2).validate().is_ok());
        assert!(registry_audit_config(2, 1).validate().is_err());
        assert!(registry_audit_config(3, 1).validate().is_err());
    }

    struct Fixture {
        _temp: TempDir,
        home: PathBuf,
        ledger: PathBuf,
        audit_dir: PathBuf,
        inventory: PathBuf,
        inventory_declaration: PathBuf,
        inventory_staging: PathBuf,
        inventory_import: PathBuf,
        trace_directory: PathBuf,
        segment_path: PathBuf,
        checkpoint_output: PathBuf,
        retention_policy: RetentionPolicy,
        key_id: ComplianceKeyId,
        event_id: [u8; 32],
        checkpoint_secret: [u8; 32],
        custody_secret: [u8; 32],
        approvers: [SigningKey; 2],
    }

    #[derive(Default)]
    struct FakeState {
        approval_calls: usize,
        custody_calls: usize,
        destruction_calls: usize,
        intent_seen_before_custody: bool,
        completion_clock_calls: usize,
        completion_clock_seen_after_custody: bool,
    }

    struct FakeRunner {
        approvers: [SigningKey; 2],
        custody_secret: [u8; 32],
        ledger_path: PathBuf,
        ledger_state_secret: [u8; 32],
        duplicate_approval: bool,
        fail_destruction_call: Option<usize>,
        completion_at_ms: u64,
        state: Arc<Mutex<FakeState>>,
    }

    impl CommandRunner for FakeRunner {
        fn run(
            &self,
            _config: &CommandConfig,
            input: &[u8],
            _max_output: usize,
        ) -> OperatorResult<Vec<u8>> {
            if input.starts_with(APPROVAL_REQUEST_MAGIC) {
                self.state.lock().unwrap().approval_calls += 1;
                let request_id: [u8; 32] = input[9..41].try_into().unwrap();
                let approved_at_ms = u64::from_be_bytes(input[43..51].try_into().unwrap());
                let mut output = Vec::new();
                output.extend_from_slice(APPROVAL_RESPONSE_MAGIC);
                output.push(ADAPTER_PROTOCOL_VERSION);
                for index in 0..2 {
                    let signer = if self.duplicate_approval {
                        &self.approvers[0]
                    } else {
                        &self.approvers[index]
                    };
                    let mut preimage = Vec::new();
                    preimage.extend_from_slice(b"pigeonpost/disclosure-approval/v1");
                    preimage.extend_from_slice(&request_id);
                    preimage.extend_from_slice(&approved_at_ms.to_be_bytes());
                    preimage.push(1);
                    output.extend_from_slice(&signer.verifying_key().to_bytes());
                    output.extend_from_slice(&approved_at_ms.to_be_bytes());
                    output.extend_from_slice(&signer.sign(&preimage).to_bytes());
                }
                return Ok(output);
            }
            if input.starts_with(CUSTODY_REQUEST_MAGIC) {
                let ledger = DisclosureLedger::open(&self.ledger_path, &self.ledger_state_secret)
                    .expect("the durable intent must exist before custody");
                let mut state = self.state.lock().unwrap();
                state.custody_calls += 1;
                state.intent_seen_before_custody = matches!(
                    ledger.leaf(ledger.leaf_count().saturating_sub(1)),
                    Ok(Some(DisclosureLeaf::Intent(_)))
                );
                drop(state);
                let peer: [u8; 32] = input[input.len() - 32..].try_into().unwrap();
                let shared = MontgomeryPoint(peer)
                    .mul_clamped(self.custody_secret)
                    .to_bytes();
                let mut output = Vec::new();
                output.extend_from_slice(CUSTODY_RESPONSE_MAGIC);
                output.push(ADAPTER_PROTOCOL_VERSION);
                output.extend_from_slice(&shared);
                return Ok(output);
            }
            if input.starts_with(DESTRUCTION_REQUEST_MAGIC) {
                let call = {
                    let mut state = self.state.lock().unwrap();
                    state.destruction_calls += 1;
                    state.destruction_calls
                };
                if self.fail_destruction_call == Some(call) {
                    return Err(OperatorError::Custody);
                }
                let mut hash = Sha256::new();
                hash.update(b"test-destruction-receipt");
                hash.update(input);
                let mut output = Vec::new();
                output.extend_from_slice(DESTRUCTION_RESPONSE_MAGIC);
                output.push(ADAPTER_PROTOCOL_VERSION);
                output.push(1);
                output.extend_from_slice(&hash.finalize());
                return Ok(output);
            }
            Err(OperatorError::Custody)
        }

        fn completion_time_ms(&self) -> OperatorResult<u64> {
            let mut state = self.state.lock().unwrap();
            state.completion_clock_calls += 1;
            state.completion_clock_seen_after_custody = state.custody_calls > 0;
            Ok(self.completion_at_ms)
        }
    }

    impl Fixture {
        fn new() -> Self {
            let temp = tempfile::tempdir().unwrap();
            let home = temp.path().canonicalize().unwrap().join("compliance");
            make_private_dir(&home);
            let audit_dir = home.join("private-audit");
            make_private_dir(&audit_dir);
            let ledger = home.join("disclosure.log");
            let inventory = home.join("epoch.ppinv");
            let inventory_declaration = home.join("epoch-inventory.toml");
            let inventory_staging = home.join("epoch-staged.ppinv");
            let inventory_import = home.join("epoch-import.ppinv");
            let trace_directory = home.join("trace-epoch");
            make_private_dir(&trace_directory);
            let checkpoint_key_path = home.join("checkpoint.key");
            let checkpoint_output_path = home.join("disclosure.checkpoint");
            let audit_key_path = home.join("audit.key");
            let approval_adapter = home.join("approval-adapter");
            let custody_adapter = home.join("custody-adapter");
            let destruction_adapter = home.join("destruction-adapter");
            let checkpoint_secret = [31u8; 32];
            write_private(&checkpoint_key_path, &checkpoint_secret);
            write_private(&audit_key_path, &[32u8; 32]);
            write_executable(&approval_adapter);
            write_executable(&custody_adapter);
            write_executable(&destruction_adapter);

            let custody_secret = [5u8; 32];
            let custody_public = MontgomeryPoint::mul_base_clamped(custody_secret).to_bytes();
            let key_id = ComplianceKeyId::new(
                CompliancePurpose::NetworkTrace,
                Jurisdiction::Test,
                [9; 32],
                0,
                1,
            );
            let segment_path = trace_directory.join(trace_segment_file_name(key_id, 0));
            let registry_log = home.join("registry.ndjson");
            let registry_checkpoint = home.join("registry.checkpoint");
            let registry_signer = SigningKey::from_bytes(&[41u8; 32]);
            let registry_witness = SigningKey::from_bytes(&[42u8; 32]);
            let registry_root = write_registry_audit(
                &registry_log,
                &registry_checkpoint,
                key_id,
                custody_public,
                &registry_signer,
                &registry_witness,
                TEST_NOW,
            );
            let event_id = [2u8; 32];
            let epoch_secret = [6; 32];
            let epoch = EpochSealingKey::from_bytes(key_id, epoch_secret).unwrap();
            let segment_signer = SigningKey::from_bytes(&[7; 32]);
            let mut writer = SegmentWriter::create(
                &segment_path,
                epoch,
                &custody_public,
                segment_signer.verifying_key().to_bytes(),
                100,
                10,
            )
            .unwrap();
            writer
                .append_network(&TraceRecord {
                    jurisdiction: Jurisdiction::Test,
                    operation: NetworkOperation::Publish,
                    timestamp_ms: 101,
                    node_id: [1; 32],
                    source_ip: TraceIp::V4(Ipv4Addr::new(192, 0, 2, 4)),
                    source_port: 9999,
                    event_id: Some(event_id),
                    recipient: Some([3; 32]),
                    owner: None,
                    size_bytes: 4,
                    correlation_id: None,
                })
                .unwrap();
            let segment = writer.finalize(200, &segment_signer).unwrap();
            publish_test_manifest(
                &trace_directory,
                key_id,
                epoch_secret,
                custody_public,
                &segment_signer,
                vec![segment],
            );

            let retention_policy = RetentionPolicy::new(365, [55; 32]).unwrap();
            let inventory_state = DestructionInventory::new(
                key_id,
                key_id.epoch_start_ms,
                retention_policy,
                all_copies(),
            )
            .unwrap();
            write_private(&inventory, &inventory_state.encode().unwrap());

            let approvers = [
                SigningKey::from_bytes(&[21u8; 32]),
                SigningKey::from_bytes(&[22u8; 32]),
            ];
            let config = format!(
                "version = 2\n\
ledger_path = {:?}\n\
private_audit_directory = {:?}\n\
private_audit_key_path = {:?}\n\
checkpoint_origin = \"pigeonpost.test/disclosures\"\n\
checkpoint_signing_key_path = {:?}\n\n\
checkpoint_output_path = {:?}\n\n\
[registry_audit]\n\
log_path = {:?}\n\
checkpoint_path = {:?}\n\
expected_origin = \"pigeonpost.test/registry\"\n\
checkpoint_key = \"{}\"\n\
witness_threshold = 1\n\
minimum_checkpoint_size = 1\n\
minimum_checkpoint_root = \"{}\"\n\
max_cosignature_age_seconds = 60\n\
future_clock_skew_seconds = 1\n\n\
[[registry_audit.witnesses]]\n\
name = \"independent.test\"\n\
public_key = \"{}\"\n\n\
[approval]\n\
request_ttl_ms = 60000\n\n\
[[approval.approvers]]\n\
public_key = \"{}\"\n\
identity = \"officer-one\"\n\n\
[[approval.approvers]]\n\
public_key = \"{}\"\n\
identity = \"outside-counsel\"\n\n\
[approval.command]\n\
executable = {:?}\n\
args = []\n\
timeout_ms = 1000\n\n\
[destruction_command]\n\
executable = {:?}\n\
args = []\n\
timeout_ms = 1000\n\n\
[[epochs]]\n\
key_id = \"{}\"\n\
inventory_path = {:?}\n\n\
inventory_declaration_path = {:?}\n\
inventory_staging_path = {:?}\n\
inventory_import_path = {:?}\n\n\
[epochs.retention_policy]\n\
version = 1\n\
tr_days = 365\n\
counsel_approval_commitment = \"{}\"\n\n\
[epochs.artifact]\n\
kind = \"trace_segments\"\n\
expected_node_id = \"{}\"\n\
expected_signer_public_key = \"{}\"\n\
expected_custody_key_digest = \"{}\"\n\
directory = {:?}\n\n\
[epochs.custody]\n\
mode = \"external\"\n\
public_key = \"{}\"\n\n\
[epochs.custody.command]\n\
executable = {:?}\n\
args = []\n\
timeout_ms = 1000\n",
                ledger,
                audit_dir,
                audit_key_path,
                checkpoint_key_path,
                checkpoint_output_path,
                registry_log,
                registry_checkpoint,
                hex_encode(&registry_signer.verifying_key().to_bytes()),
                hex_encode(&registry_root),
                hex_encode(&registry_witness.verifying_key().to_bytes()),
                hex_encode(&approvers[0].verifying_key().to_bytes()),
                hex_encode(&approvers[1].verifying_key().to_bytes()),
                approval_adapter,
                destruction_adapter,
                hex_encode(&key_id.encode().unwrap()),
                inventory,
                inventory_declaration,
                inventory_staging,
                inventory_import,
                hex_encode(&retention_policy.counsel_approval_commitment()),
                hex_encode(&[1; 32]),
                hex_encode(&segment_signer.verifying_key().to_bytes()),
                hex_encode(&Sha256::digest(custody_public)),
                trace_directory,
                hex_encode(&custody_public),
                custody_adapter,
            );
            write_private(&home.join("config.toml"), config.as_bytes());
            Self {
                _temp: temp,
                home,
                ledger,
                audit_dir,
                inventory,
                inventory_declaration,
                inventory_staging,
                inventory_import,
                trace_directory,
                segment_path,
                checkpoint_output: checkpoint_output_path,
                retention_policy,
                key_id,
                event_id,
                checkpoint_secret,
                custody_secret,
                approvers,
            }
        }

        fn runner(&self, duplicate_approval: bool) -> (FakeRunner, Arc<Mutex<FakeState>>) {
            let state = Arc::new(Mutex::new(FakeState::default()));
            (
                FakeRunner {
                    approvers: [
                        SigningKey::from_bytes(&self.approvers[0].to_bytes()),
                        SigningKey::from_bytes(&self.approvers[1].to_bytes()),
                    ],
                    custody_secret: self.custody_secret,
                    ledger_path: self.ledger.clone(),
                    ledger_state_secret: self.checkpoint_secret,
                    duplicate_approval,
                    fail_destruction_call: None,
                    completion_at_ms: TEST_NOW + 7,
                    state: Arc::clone(&state),
                },
                state,
            )
        }
    }

    fn make_private_dir(path: &Path) {
        fs::create_dir_all(path).unwrap();
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            fs::set_permissions(path, fs::Permissions::from_mode(0o700)).unwrap();
        }
    }

    fn write_private(path: &Path, bytes: &[u8]) {
        let mut options = OpenOptions::new();
        options.create_new(true).write(true);
        #[cfg(unix)]
        {
            use std::os::unix::fs::OpenOptionsExt;
            options.mode(0o600);
        }
        let mut file = options.open(path).unwrap();
        file.write_all(bytes).unwrap();
        file.sync_all().unwrap();
    }

    fn write_executable(path: &Path) {
        write_private(path, b"offline test adapter placeholder");
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            fs::set_permissions(path, fs::Permissions::from_mode(0o700)).unwrap();
        }
    }

    fn write_registry_audit(
        log_path: &Path,
        checkpoint_path: &Path,
        key_id: ComplianceKeyId,
        public_key: [u8; 32],
        registry_signer: &SigningKey,
        witness: &SigningKey,
        now_ms: u64,
    ) -> [u8; 32] {
        let entry = LogEntry::compliance_key(
            0,
            ComplianceKeyPublish {
                key_id,
                public_key: hex_encode(&public_key),
                not_before_ms: key_id.epoch_start_ms,
                not_after_ms: key_id
                    .epoch_start_ms
                    .checked_add(pigeonpost_compliance_seal::TRACE_EPOCH_DURATION_MS)
                    .unwrap(),
                status: ComplianceKeyStatus::Active,
            },
            now_ms,
        );
        let mut encoded = serde_json::to_vec(&entry).unwrap();
        encoded.push(b'\n');
        write_private(log_path, &encoded);

        let mut frontier = MerkleFrontier::new();
        frontier.append(&entry.leaf_bytes().unwrap()).unwrap();
        let root = frontier.root().unwrap();
        let checkpoint = Checkpoint {
            origin: "pigeonpost.test/registry".into(),
            size: 1,
            root,
        };
        let mut note = checkpoint.sign(registry_signer);
        note.push_str(
            &checkpoint
                .cosignature_line("independent.test", witness, now_ms / 1_000)
                .unwrap(),
        );
        write_private(checkpoint_path, note.as_bytes());
        root
    }

    fn all_copies() -> Vec<KeyCopy> {
        [
            KeyCopyKind::LiveMetadata,
            KeyCopyKind::SqliteWal,
            KeyCopyKind::Sidecar,
            KeyCopyKind::Snapshot,
            KeyCopyKind::Backup,
            KeyCopyKind::KmsVersion,
            KeyCopyKind::ShamirShare,
        ]
        .into_iter()
        .map(|kind| {
            if matches!(kind, KeyCopyKind::LiveMetadata | KeyCopyKind::Backup) {
                KeyCopy::present(kind, format!("private-{kind:?}").as_bytes()).unwrap()
            } else {
                KeyCopy::verified_absent(kind, format!("absent-{kind:?}").as_bytes()).unwrap()
            }
        })
        .collect()
    }

    fn declaration_text(key_id: ComplianceKeyId, extra_backup: bool) -> String {
        use core::fmt::Write as _;

        let declarations = [
            ("live_metadata", "present"),
            ("sqlite_wal", "verified_absent"),
            ("sidecar", "verified_absent"),
            ("snapshot", "verified_absent"),
            ("backup", "present"),
            ("kms_version", "verified_absent"),
            ("shamir_share", "verified_absent"),
        ];
        let mut manifest = format!(
            "version = 1\nkey_id = \"{}\"\ncreated_at_ms = {}\n",
            hex_encode(&key_id.encode().unwrap()),
            key_id.epoch_start_ms
        );
        for (index, (kind, state)) in declarations.iter().enumerate() {
            writeln!(
                manifest,
                "\n[[copies]]\nkind = \"{kind}\"\nstate = \"{state}\"\nnonce = \"{}\"\nprivate_material = \"private-{kind}\"",
                hex_encode(&[(index + 1) as u8; 32])
            )
            .unwrap();
        }
        if extra_backup {
            writeln!(
                manifest,
                "\n[[copies]]\nkind = \"backup\"\nstate = \"present\"\nnonce = \"{}\"\nprivate_material = \"private-second-backup\"",
                hex_encode(&[8u8; 32])
            )
            .unwrap();
        }
        manifest
    }

    fn replace_private(path: &Path, bytes: &[u8]) {
        if path.exists() {
            fs::remove_file(path).unwrap();
        }
        write_private(path, bytes);
    }

    fn trace_segment_file_name(key_id: ComplianceKeyId, index: u32) -> String {
        let purpose = match key_id.purpose {
            CompliancePurpose::NetworkTrace => "network",
            CompliancePurpose::IdentityTrace => "identity",
            CompliancePurpose::Attribution => panic!("attribution does not use trace files"),
        };
        format!(
            "{purpose}-{}-{index:08}.pptrace",
            hex_encode(&key_id.encode().unwrap())
        )
    }

    fn publish_test_manifest(
        directory: &Path,
        key_id: ComplianceKeyId,
        epoch_secret: [u8; 32],
        custody_public: [u8; 32],
        signer: &SigningKey,
        segments: Vec<pigeonpost_compliance_seal::VerifiedSegment>,
    ) {
        let entries = segments
            .iter()
            .enumerate()
            .map(|(index, segment)| {
                EpochSegmentEntry::from_verified(index as u32, segment).unwrap()
            })
            .collect();
        let manifest = EpochManifest::new_signed(
            key_id,
            [1; 32],
            Sha256::digest(custody_public).into(),
            Sha256::digest(epoch_secret).into(),
            entries,
            signer,
        )
        .unwrap();
        let path = epoch_manifest_path(directory, &key_id).unwrap();
        if path.exists() {
            fs::remove_file(&path).unwrap();
        }
        publish_epoch_manifest(path, &manifest).unwrap();
    }

    fn write_trace_segment(
        path: &Path,
        key_id: ComplianceKeyId,
        epoch_secret: [u8; 32],
        custody_public: [u8; 32],
        signer_secret: [u8; 32],
        node_id: [u8; 32],
        event_id: [u8; 32],
    ) {
        if path.exists() {
            fs::remove_file(path).unwrap();
        }
        let signer = SigningKey::from_bytes(&signer_secret);
        let epoch = EpochSealingKey::from_bytes(key_id, epoch_secret).unwrap();
        let mut writer = SegmentWriter::create(
            path,
            epoch,
            &custody_public,
            signer.verifying_key().to_bytes(),
            key_id.epoch_start_ms + 100,
            10,
        )
        .unwrap();
        writer
            .append_network(&TraceRecord {
                jurisdiction: key_id.jurisdiction,
                operation: NetworkOperation::Publish,
                timestamp_ms: key_id.epoch_start_ms + 101,
                node_id,
                source_ip: TraceIp::V4(Ipv4Addr::new(192, 0, 2, 4)),
                source_port: 9999,
                event_id: Some(event_id),
                recipient: Some([3; 32]),
                owner: None,
                size_bytes: 4,
                correlation_id: None,
            })
            .unwrap();
        let segment = writer
            .finalize(key_id.epoch_start_ms + 200, &signer)
            .unwrap();
        publish_test_manifest(
            path.parent().unwrap(),
            key_id,
            epoch_secret,
            custody_public,
            &signer,
            vec![segment],
        );
    }

    fn configure_trace_paths(fixture: &Fixture, paths: &[PathBuf]) {
        assert_eq!(paths.first(), Some(&fixture.segment_path));
        for (index, path) in paths.iter().enumerate().skip(1) {
            fs::rename(
                path,
                fixture
                    .trace_directory
                    .join(format!("unexpected-{index:08}.pptrace")),
            )
            .unwrap();
        }
    }

    fn append_test_epoch(
        fixture: &Fixture,
        epoch_start_ms: u64,
        generation: u32,
        label: &str,
    ) -> (ComplianceKeyId, PathBuf) {
        let key_id = ComplianceKeyId::new(
            CompliancePurpose::NetworkTrace,
            Jurisdiction::Test,
            [10; 32],
            epoch_start_ms,
            generation,
        );
        let inventory = fixture.home.join(format!("{label}.ppinv"));
        let declaration = fixture.home.join(format!("{label}.toml"));
        let staging = fixture.home.join(format!("{label}.staged.ppinv"));
        let import = fixture.home.join(format!("{label}.import.ppinv"));
        let trace_directory = fixture.home.join(format!("{label}-trace-epoch"));
        make_private_dir(&trace_directory);
        let segment = trace_directory.join(trace_segment_file_name(key_id, 0));
        let custody_adapter = fixture.home.join(format!("{label}-custody-adapter"));
        write_executable(&custody_adapter);
        let custody_public = MontgomeryPoint::mul_base_clamped(fixture.custody_secret).to_bytes();
        write_trace_segment(
            &segment,
            key_id,
            [8; 32],
            custody_public,
            [7; 32],
            [1; 32],
            [12; 32],
        );
        let state = DestructionInventory::new(
            key_id,
            key_id.epoch_start_ms,
            fixture.retention_policy,
            all_copies(),
        )
        .unwrap();
        write_private(&inventory, &state.encode().unwrap());

        let extra = format!(
            "\n[[epochs]]\n\
key_id = \"{}\"\n\
inventory_path = {:?}\n\
inventory_declaration_path = {:?}\n\
inventory_staging_path = {:?}\n\
inventory_import_path = {:?}\n\n\
[epochs.retention_policy]\n\
version = 1\n\
tr_days = 365\n\
counsel_approval_commitment = \"{}\"\n\n\
[epochs.artifact]\n\
kind = \"trace_segments\"\n\
expected_node_id = \"{}\"\n\
expected_signer_public_key = \"{}\"\n\
expected_custody_key_digest = \"{}\"\n\
directory = {:?}\n\n\
[epochs.custody]\n\
mode = \"external\"\n\
public_key = \"{}\"\n\n\
[epochs.custody.command]\n\
executable = {:?}\n\
args = []\n\
timeout_ms = 1000\n",
            hex_encode(&key_id.encode().unwrap()),
            inventory,
            declaration,
            staging,
            import,
            hex_encode(&fixture.retention_policy.counsel_approval_commitment()),
            hex_encode(&[1; 32]),
            hex_encode(&SigningKey::from_bytes(&[7; 32]).verifying_key().to_bytes()),
            hex_encode(&Sha256::digest(custody_public)),
            trace_directory,
            hex_encode(&custody_public),
            custody_adapter,
        );
        let config_path = fixture.home.join("config.toml");
        let mut config = fs::read_to_string(&config_path).unwrap();
        config.push_str(&extra);
        replace_private(&config_path, config.as_bytes());
        (key_id, inventory)
    }

    fn unseal_command(fixture: &Fixture) -> ParsedCommand {
        ParsedCommand::Unseal {
            epoch: hex_encode(&fixture.key_id.encode().unwrap()),
            order_reference: Zeroizing::new("order-123".to_string()),
            selectors: vec![Zeroizing::new(format!(
                "event_id={}",
                hex_encode(&fixture.event_id)
            ))],
        }
    }

    #[cfg_attr(
        not(any(target_os = "linux", target_os = "macos")),
        ignore = "persistent offline custody is supported only on Linux and macOS"
    )]
    #[test]
    fn status_serializes_with_ledger_and_inventory_mutations() {
        let fixture = Fixture::new();
        let config = OperatorConfig::load(&fixture.home).unwrap();
        let lock = OperatorLock::acquire(&fixture.home).unwrap();
        let home = fixture.home.clone();
        let (sender, receiver) = std::sync::mpsc::channel();
        let worker = std::thread::spawn(move || {
            let mut output = Vec::new();
            sender
                .send(command_status(&home, &config, TEST_NOW, &mut output))
                .unwrap();
        });

        assert!(receiver.recv_timeout(Duration::from_millis(100)).is_err());
        drop(lock);
        assert!(receiver
            .recv_timeout(Duration::from_secs(2))
            .expect("status resumes after the mutation lock is released")
            .is_ok());
        worker.join().unwrap();
    }

    #[cfg_attr(
        not(any(target_os = "linux", target_os = "macos")),
        ignore = "persistent offline custody is supported only on Linux and macOS"
    )]
    #[test]
    fn inventory_create_provision_and_update_are_locked_monotonic_ceremonies() {
        let fixture = Fixture::new();
        fs::remove_file(&fixture.inventory).unwrap();
        write_private(
            &fixture.inventory_declaration,
            declaration_text(fixture.key_id, false).as_bytes(),
        );
        let (runner, _) = fixture.runner(false);
        let epoch = hex_encode(&fixture.key_id.encode().unwrap());
        let mut output = Vec::new();

        run_command(
            &fixture.home,
            ParsedCommand::Inventory {
                action: InventoryAction::Create,
                epoch: epoch.clone(),
            },
            None,
            TEST_NOW,
            &runner,
            &mut output,
        )
        .unwrap();
        assert_eq!(output, b"inventory=staged\n");
        assert!(!fixture.inventory.exists());
        assert_eq!(
            load_inventory(
                &fixture.inventory_staging,
                fixture.key_id,
                fixture.retention_policy,
            )
            .unwrap()
            .copies()
            .len(),
            7
        );

        output.clear();
        run_command(
            &fixture.home,
            ParsedCommand::Inventory {
                action: InventoryAction::Provision,
                epoch: epoch.clone(),
            },
            None,
            TEST_NOW,
            &runner,
            &mut output,
        )
        .unwrap();
        assert_eq!(output, b"inventory=provisioned\n");
        assert_eq!(
            run_command(
                &fixture.home,
                ParsedCommand::Inventory {
                    action: InventoryAction::Provision,
                    epoch: epoch.clone(),
                },
                None,
                TEST_NOW,
                &runner,
                &mut Vec::new(),
            ),
            Err(OperatorError::State)
        );

        let config_path = fixture.home.join("config.toml");
        let mut config = fs::read_to_string(&config_path).unwrap();
        config = config.replace("tr_days = 365", "tr_days = 500");
        config = config.replace(&hex_encode(&[55; 32]), &hex_encode(&[56; 32]));
        replace_private(&config_path, config.as_bytes());
        let updated_policy = RetentionPolicy::new(500, [56; 32]).unwrap();
        output.clear();
        run_command(
            &fixture.home,
            ParsedCommand::Inventory {
                action: InventoryAction::Update,
                epoch: epoch.clone(),
            },
            None,
            TEST_NOW,
            &runner,
            &mut output,
        )
        .unwrap();
        assert_eq!(
            output,
            b"inventory=updated\npolicy_updated=true\nadded_copies=0\n"
        );

        replace_private(
            &fixture.inventory_declaration,
            declaration_text(fixture.key_id, true).as_bytes(),
        );
        output.clear();
        run_command(
            &fixture.home,
            ParsedCommand::Inventory {
                action: InventoryAction::Update,
                epoch: epoch.clone(),
            },
            None,
            TEST_NOW,
            &runner,
            &mut output,
        )
        .unwrap();
        assert_eq!(
            output,
            b"inventory=updated\npolicy_updated=false\nadded_copies=1\n"
        );
        assert_eq!(
            load_inventory(&fixture.inventory, fixture.key_id, updated_policy,)
                .unwrap()
                .copies()
                .len(),
            8
        );

        replace_private(
            &fixture.inventory_declaration,
            declaration_text(fixture.key_id, false).as_bytes(),
        );
        assert_eq!(
            run_command(
                &fixture.home,
                ParsedCommand::Inventory {
                    action: InventoryAction::Update,
                    epoch,
                },
                None,
                TEST_NOW,
                &runner,
                &mut Vec::new(),
            ),
            Err(OperatorError::State)
        );
    }

    #[cfg_attr(
        not(any(target_os = "linux", target_os = "macos")),
        ignore = "persistent offline custody is supported only on Linux and macOS"
    )]
    #[test]
    fn inventory_import_refuses_overwrite_and_policy_mismatch() {
        let fixture = Fixture::new();
        let encoded = fs::read(&fixture.inventory).unwrap();
        write_private(&fixture.inventory_import, &encoded);
        fs::remove_file(&fixture.inventory).unwrap();
        let (runner, _) = fixture.runner(false);
        let epoch = hex_encode(&fixture.key_id.encode().unwrap());
        run_command(
            &fixture.home,
            ParsedCommand::Inventory {
                action: InventoryAction::Import,
                epoch: epoch.clone(),
            },
            None,
            TEST_NOW,
            &runner,
            &mut Vec::new(),
        )
        .unwrap();
        assert_eq!(
            run_command(
                &fixture.home,
                ParsedCommand::Inventory {
                    action: InventoryAction::Import,
                    epoch,
                },
                None,
                TEST_NOW,
                &runner,
                &mut Vec::new(),
            ),
            Err(OperatorError::State)
        );

        let mismatch = Fixture::new();
        let wrong = DestructionInventory::new(
            mismatch.key_id,
            mismatch.key_id.epoch_start_ms,
            RetentionPolicy::new(365, [99; 32]).unwrap(),
            all_copies(),
        )
        .unwrap();
        write_private(&mismatch.inventory_import, &wrong.encode().unwrap());
        fs::remove_file(&mismatch.inventory).unwrap();
        let (runner, _) = mismatch.runner(false);
        assert_eq!(
            run_command(
                &mismatch.home,
                ParsedCommand::Inventory {
                    action: InventoryAction::Import,
                    epoch: hex_encode(&mismatch.key_id.encode().unwrap()),
                },
                None,
                TEST_NOW,
                &runner,
                &mut Vec::new(),
            ),
            Err(OperatorError::State)
        );
        assert!(!mismatch.inventory.exists());
    }

    #[cfg_attr(
        not(any(target_os = "linux", target_os = "macos")),
        ignore = "persistent offline custody is supported only on Linux and macOS"
    )]
    #[test]
    fn inventory_declaration_parser_fails_closed_without_disclosing_private_material() {
        let fixture = Fixture::new();
        fs::remove_file(&fixture.inventory).unwrap();
        let (runner, _) = fixture.runner(false);
        let epoch = hex_encode(&fixture.key_id.encode().unwrap());

        let mut missing_class = declaration_text(fixture.key_id, false);
        missing_class.truncate(missing_class.rfind("\n[[copies]]").unwrap());
        write_private(&fixture.inventory_declaration, missing_class.as_bytes());
        let mut output = Vec::new();
        let error = run_command(
            &fixture.home,
            ParsedCommand::Inventory {
                action: InventoryAction::Create,
                epoch: epoch.clone(),
            },
            None,
            TEST_NOW,
            &runner,
            &mut output,
        )
        .unwrap_err();
        assert_eq!(error, OperatorError::State);
        assert!(!error.to_string().contains("private-"));
        assert!(output.is_empty());
        assert!(!fixture.inventory_staging.exists());

        let duplicate_nonce = declaration_text(fixture.key_id, false).replacen(
            &hex_encode(&[2u8; 32]),
            &hex_encode(&[1u8; 32]),
            1,
        );
        replace_private(&fixture.inventory_declaration, duplicate_nonce.as_bytes());
        assert_eq!(
            run_command(
                &fixture.home,
                ParsedCommand::Inventory {
                    action: InventoryAction::Create,
                    epoch: epoch.clone(),
                },
                None,
                TEST_NOW,
                &runner,
                &mut Vec::new(),
            ),
            Err(OperatorError::State)
        );

        let zero_nonce = declaration_text(fixture.key_id, false).replacen(
            &hex_encode(&[1u8; 32]),
            &hex_encode(&[0u8; 32]),
            1,
        );
        replace_private(&fixture.inventory_declaration, zero_nonce.as_bytes());
        assert_eq!(
            run_command(
                &fixture.home,
                ParsedCommand::Inventory {
                    action: InventoryAction::Create,
                    epoch: epoch.clone(),
                },
                None,
                TEST_NOW,
                &runner,
                &mut Vec::new(),
            ),
            Err(OperatorError::State)
        );

        let mut unknown_field = declaration_text(fixture.key_id, false);
        unknown_field.push_str("unexpected = true\n");
        replace_private(&fixture.inventory_declaration, unknown_field.as_bytes());
        assert_eq!(
            run_command(
                &fixture.home,
                ParsedCommand::Inventory {
                    action: InventoryAction::Create,
                    epoch: epoch.clone(),
                },
                None,
                TEST_NOW,
                &runner,
                &mut Vec::new(),
            ),
            Err(OperatorError::State)
        );

        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;

            replace_private(
                &fixture.inventory_declaration,
                declaration_text(fixture.key_id, false).as_bytes(),
            );
            fs::set_permissions(
                &fixture.inventory_declaration,
                fs::Permissions::from_mode(0o644),
            )
            .unwrap();
            assert_eq!(
                run_command(
                    &fixture.home,
                    ParsedCommand::Inventory {
                        action: InventoryAction::Create,
                        epoch,
                    },
                    None,
                    TEST_NOW,
                    &runner,
                    &mut Vec::new(),
                ),
                Err(OperatorError::Storage)
            );
        }
    }

    #[cfg_attr(
        not(any(target_os = "linux", target_os = "macos")),
        ignore = "persistent offline custody is supported only on Linux and macOS"
    )]
    #[test]
    fn inventory_config_rejects_legacy_policy_and_aliased_paths() {
        let legacy = Fixture::new();
        let config_path = legacy.home.join("config.toml");
        let config =
            fs::read_to_string(&config_path)
                .unwrap()
                .replacen("version = 2", "version = 1", 1);
        replace_private(&config_path, config.as_bytes());
        assert!(matches!(
            OperatorConfig::load(&legacy.home),
            Err(OperatorError::Configuration)
        ));

        let legacy_trace_paths = Fixture::new();
        let config_path = legacy_trace_paths.home.join("config.toml");
        let config = fs::read_to_string(&config_path).unwrap().replace(
            &format!("directory = {:?}", legacy_trace_paths.trace_directory),
            &format!("paths = [{:?}]", legacy_trace_paths.segment_path),
        );
        replace_private(&config_path, config.as_bytes());
        assert!(matches!(
            OperatorConfig::load(&legacy_trace_paths.home),
            Err(OperatorError::Configuration)
        ));

        let invalid_policy = Fixture::new();
        let config_path = invalid_policy.home.join("config.toml");
        let config = fs::read_to_string(&config_path)
            .unwrap()
            .replace("tr_days = 365", "tr_days = 364");
        replace_private(&config_path, config.as_bytes());
        assert!(matches!(
            OperatorConfig::load(&invalid_policy.home),
            Err(OperatorError::Configuration)
        ));

        let aliased = Fixture::new();
        let config_path = aliased.home.join("config.toml");
        let config = fs::read_to_string(&config_path).unwrap().replace(
            &format!("inventory_import_path = {:?}", aliased.inventory_import),
            &format!("inventory_import_path = {:?}", aliased.inventory_staging),
        );
        replace_private(&config_path, config.as_bytes());
        assert!(matches!(
            OperatorConfig::load(&aliased.home),
            Err(OperatorError::Configuration)
        ));

        let reserved = Fixture::new();
        let config_path = reserved.home.join("config.toml");
        let config = fs::read_to_string(&config_path).unwrap().replace(
            &format!("inventory_import_path = {:?}", reserved.inventory_import),
            &format!("inventory_import_path = {config_path:?}"),
        );
        replace_private(&config_path, config.as_bytes());
        assert!(matches!(
            OperatorConfig::load(&reserved.home),
            Err(OperatorError::Configuration)
        ));

        let state_reserved = Fixture::new();
        let config_path = state_reserved.home.join("config.toml");
        let ledger_state = disclosure_state_path(&state_reserved.ledger).unwrap();
        let config = fs::read_to_string(&config_path).unwrap().replace(
            &format!(
                "inventory_import_path = {:?}",
                state_reserved.inventory_import
            ),
            &format!("inventory_import_path = {ledger_state:?}"),
        );
        replace_private(&config_path, config.as_bytes());
        assert!(matches!(
            OperatorConfig::load(&state_reserved.home),
            Err(OperatorError::Configuration)
        ));

        #[cfg(unix)]
        {
            let escaped = Fixture::new();
            let outside = escaped.home.parent().unwrap().join("outside");
            let outside_nested = outside.join("nested");
            make_private_dir(&outside_nested);
            let escape_link = escaped.home.join("escape");
            std::os::unix::fs::symlink(&outside, &escape_link).unwrap();
            let escaped_import = escape_link.join("nested/import.ppinv");
            let config_path = escaped.home.join("config.toml");
            let config = fs::read_to_string(&config_path).unwrap().replace(
                &format!("inventory_import_path = {:?}", escaped.inventory_import),
                &format!("inventory_import_path = {escaped_import:?}"),
            );
            replace_private(&config_path, config.as_bytes());
            assert!(matches!(
                OperatorConfig::load(&escaped.home),
                Err(OperatorError::Configuration)
            ));
        }
    }

    #[cfg_attr(
        not(any(target_os = "linux", target_os = "macos")),
        ignore = "persistent offline custody is supported only on Linux and macOS"
    )]
    #[test]
    fn unseal_has_two_approvals_and_durable_intent_before_plaintext() {
        let fixture = Fixture::new();
        let (runner, state) = fixture.runner(false);
        let mut output = Vec::new();
        run_command(
            &fixture.home,
            unseal_command(&fixture),
            Some(b"authorized-requester"),
            TEST_NOW,
            &runner,
            &mut output,
        )
        .unwrap();

        let state = state.lock().unwrap();
        assert_eq!(state.approval_calls, 1);
        assert_eq!(state.custody_calls, 1);
        assert!(state.intent_seen_before_custody);
        assert_eq!(state.completion_clock_calls, 1);
        assert!(state.completion_clock_seen_after_custody);
        drop(state);
        let disclosed_bytes = output.clone();
        let text = String::from_utf8(output).unwrap();
        assert!(text.contains("192.0.2.4"));
        assert!(text.contains(&hex_encode(&fixture.event_id)));
        let ledger = DisclosureLedger::open(&fixture.ledger, &fixture.checkpoint_secret).unwrap();
        assert_eq!(ledger.leaf_count(), 2);
        let checkpoint_key = SigningKey::from_bytes(&fixture.checkpoint_secret);
        let published = Checkpoint::verify(
            &fs::read_to_string(&fixture.checkpoint_output).unwrap(),
            &checkpoint_key.verifying_key(),
        )
        .unwrap();
        assert_eq!(published.size, ledger.leaf_count());
        assert_eq!(published.root, ledger.root());
        assert!(matches!(
            ledger.leaf(0).unwrap(),
            Some(DisclosureLeaf::Intent(_))
        ));
        let Some(DisclosureLeaf::Completion(completion)) = ledger.leaf(1).unwrap() else {
            panic!("the second disclosure leaf must be its completion");
        };
        assert_eq!(completion.status, CompletionStatus::Succeeded);
        assert_eq!(completion.timestamp_ms, TEST_NOW + 7);
        let config = OperatorConfig::load(&fixture.home).unwrap();
        let epoch = config.epoch(fixture.key_id).unwrap();
        let trace = authenticate_trace_epoch(epoch, fixture.key_id, TEST_NOW)
            .unwrap()
            .unwrap();
        let audit_key = PrivateAuditKey::from_bytes([32; 32]).unwrap();
        assert_eq!(
            completion.result_commitment,
            audit_key
                .result_commitment_with_terminal_manifest(
                    &completion.request_id,
                    &trace.manifest_commitment(),
                    &disclosed_bytes,
                )
                .unwrap()
        );
        assert_eq!(fs::read_dir(&fixture.audit_dir).unwrap().count(), 1);
    }

    #[cfg_attr(
        not(any(target_os = "linux", target_os = "macos")),
        ignore = "persistent offline custody is supported only on Linux and macOS"
    )]
    #[test]
    fn trace_intake_rejects_self_declared_signers_and_wrong_custody_binding() {
        let fixture = Fixture::new();
        let custody_public = MontgomeryPoint::mul_base_clamped(fixture.custody_secret).to_bytes();
        write_trace_segment(
            &fixture.segment_path,
            fixture.key_id,
            [6; 32],
            custody_public,
            [8; 32],
            [1; 32],
            fixture.event_id,
        );
        let (runner, state) = fixture.runner(false);
        let mut output = Vec::new();
        assert_eq!(
            run_command(
                &fixture.home,
                unseal_command(&fixture),
                Some(b"authorized-requester"),
                TEST_NOW,
                &runner,
                &mut output,
            ),
            Err(OperatorError::Artifact)
        );
        assert!(output.is_empty());
        assert_eq!(state.lock().unwrap().approval_calls, 0);
        assert!(!fixture.ledger.exists());

        let fixture = Fixture::new();
        let wrong_custody = MontgomeryPoint::mul_base_clamped([11; 32]).to_bytes();
        write_trace_segment(
            &fixture.segment_path,
            fixture.key_id,
            [6; 32],
            wrong_custody,
            [7; 32],
            [1; 32],
            fixture.event_id,
        );
        let (runner, state) = fixture.runner(false);
        let mut output = Vec::new();
        assert_eq!(
            run_command(
                &fixture.home,
                unseal_command(&fixture),
                Some(b"authorized-requester"),
                TEST_NOW,
                &runner,
                &mut output,
            ),
            Err(OperatorError::Artifact)
        );
        assert!(output.is_empty());
        assert_eq!(state.lock().unwrap().approval_calls, 0);
        assert!(!fixture.ledger.exists());
    }

    #[cfg_attr(
        not(any(target_os = "linux", target_os = "macos")),
        ignore = "persistent offline custody is supported only on Linux and macOS"
    )]
    #[test]
    fn trace_intake_rejects_duplicate_segments_and_mixed_epoch_commitments() {
        let fixture = Fixture::new();
        let duplicate = fixture.home.join("duplicate.ppseg");
        write_private(&duplicate, &fs::read(&fixture.segment_path).unwrap());
        configure_trace_paths(&fixture, &[fixture.segment_path.clone(), duplicate]);
        let (runner, state) = fixture.runner(false);
        let mut output = Vec::new();
        assert_eq!(
            run_command(
                &fixture.home,
                unseal_command(&fixture),
                Some(b"authorized-requester"),
                TEST_NOW,
                &runner,
                &mut output,
            ),
            Err(OperatorError::Artifact)
        );
        assert!(output.is_empty());
        assert_eq!(state.lock().unwrap().approval_calls, 0);

        let fixture = Fixture::new();
        let second = fixture.home.join("mixed-key.ppseg");
        let custody_public = MontgomeryPoint::mul_base_clamped(fixture.custody_secret).to_bytes();
        write_trace_segment(
            &second,
            fixture.key_id,
            [8; 32],
            custody_public,
            [7; 32],
            [1; 32],
            [13; 32],
        );
        configure_trace_paths(&fixture, &[fixture.segment_path.clone(), second]);
        let (runner, state) = fixture.runner(false);
        let mut output = Vec::new();
        assert_eq!(
            run_command(
                &fixture.home,
                unseal_command(&fixture),
                Some(b"authorized-requester"),
                TEST_NOW,
                &runner,
                &mut output,
            ),
            Err(OperatorError::Artifact)
        );
        assert!(output.is_empty());
        assert_eq!(state.lock().unwrap().approval_calls, 0);
    }

    #[cfg_attr(
        not(any(target_os = "linux", target_os = "macos")),
        ignore = "persistent offline custody is supported only on Linux and macOS"
    )]
    #[test]
    fn terminal_manifest_is_mandatory_but_body_degradation_does_not_block_shred() {
        let fixture = Fixture::new();
        fs::remove_file(epoch_manifest_path(&fixture.trace_directory, &fixture.key_id).unwrap())
            .unwrap();
        let (runner, state) = fixture.runner(false);
        let mut output = Vec::new();
        assert_eq!(
            run_command(
                &fixture.home,
                ParsedCommand::Inventory {
                    action: InventoryAction::Update,
                    epoch: hex_encode(&fixture.key_id.encode().unwrap()),
                },
                None,
                TEST_NOW,
                &runner,
                &mut output,
            ),
            Err(OperatorError::Artifact)
        );
        assert!(output.is_empty());
        assert_eq!(state.lock().unwrap().destruction_calls, 0);

        // A valid signed terminal manifest remains the authority for destruction. Extra, missing,
        // or corrupt ciphertext bodies are durable degradation evidence, not a way to make the
        // corresponding decryption key immortal. Disclosure remains strict.
        let fixture = Fixture::new();
        write_private(&fixture.trace_directory.join("unexpected"), b"extra");
        let (runner, state) = fixture.runner(false);
        let mut output = Vec::new();
        assert_eq!(
            run_command(
                &fixture.home,
                unseal_command(&fixture),
                Some(b"authorized-requester"),
                TEST_NOW,
                &runner,
                &mut output,
            ),
            Err(OperatorError::Artifact)
        );
        assert!(output.is_empty());
        assert_eq!(state.lock().unwrap().approval_calls, 0);
        run_command(
            &fixture.home,
            ParsedCommand::Shred {
                before: "1970-01-02".to_string(),
                execute: true,
            },
            None,
            2 * DAY_MS + 2,
            &runner,
            &mut output,
        )
        .unwrap();
        assert!(String::from_utf8_lossy(&output).contains("trace_integrity_degraded_epochs=1"));
        assert_eq!(state.lock().unwrap().destruction_calls, 2);
        let inventory =
            load_inventory(&fixture.inventory, fixture.key_id, fixture.retention_policy).unwrap();
        assert_eq!(inventory.state(), InventoryState::Shredded);
        assert_eq!(
            inventory
                .trace_integrity()
                .map(|evidence| evidence.status()),
            Some(TraceIntegrityStatus::Degraded)
        );

        let fixture = Fixture::new();
        fs::remove_file(&fixture.segment_path).unwrap();
        let (runner, state) = fixture.runner(false);
        let mut output = Vec::new();
        run_command(
            &fixture.home,
            ParsedCommand::Shred {
                before: "1970-01-02".to_string(),
                execute: false,
            },
            None,
            2 * DAY_MS + 2,
            &runner,
            &mut output,
        )
        .unwrap();
        assert!(String::from_utf8_lossy(&output).contains("trace_integrity_degraded_epochs=1"));
        assert_eq!(state.lock().unwrap().destruction_calls, 0);
        assert_eq!(
            load_inventory(&fixture.inventory, fixture.key_id, fixture.retention_policy)
                .unwrap()
                .trace_integrity(),
            None
        );
        output.clear();
        run_command(
            &fixture.home,
            ParsedCommand::Shred {
                before: "1970-01-02".to_string(),
                execute: true,
            },
            None,
            2 * DAY_MS + 3,
            &runner,
            &mut output,
        )
        .unwrap();
        assert_eq!(state.lock().unwrap().destruction_calls, 2);
        assert_eq!(
            load_inventory(&fixture.inventory, fixture.key_id, fixture.retention_policy)
                .unwrap()
                .trace_integrity()
                .map(|evidence| evidence.status()),
            Some(TraceIntegrityStatus::Degraded)
        );

        let fixture = Fixture::new();
        replace_private(&fixture.segment_path, b"corrupt ciphertext body");
        let (runner, state) = fixture.runner(false);
        let mut output = Vec::new();
        assert_eq!(
            run_command(
                &fixture.home,
                unseal_command(&fixture),
                Some(b"authorized-requester"),
                TEST_NOW,
                &runner,
                &mut output,
            ),
            Err(OperatorError::Artifact)
        );
        assert_eq!(state.lock().unwrap().approval_calls, 0);
        run_command(
            &fixture.home,
            ParsedCommand::Shred {
                before: "1970-01-02".to_string(),
                execute: true,
            },
            None,
            2 * DAY_MS + 2,
            &runner,
            &mut output,
        )
        .unwrap();
        assert_eq!(state.lock().unwrap().destruction_calls, 2);
        let inventory =
            load_inventory(&fixture.inventory, fixture.key_id, fixture.retention_policy).unwrap();
        assert_eq!(inventory.state(), InventoryState::Shredded);
        assert_eq!(
            inventory
                .trace_integrity()
                .map(|evidence| evidence.status()),
            Some(TraceIntegrityStatus::Degraded)
        );
    }

    #[cfg_attr(
        not(any(target_os = "linux", target_os = "macos")),
        ignore = "persistent offline custody is supported only on Linux and macOS"
    )]
    #[test]
    fn decrypted_trace_records_must_match_the_pinned_producer_node() {
        let fixture = Fixture::new();
        let custody_public = MontgomeryPoint::mul_base_clamped(fixture.custody_secret).to_bytes();
        write_trace_segment(
            &fixture.segment_path,
            fixture.key_id,
            [6; 32],
            custody_public,
            [7; 32],
            [2; 32],
            fixture.event_id,
        );
        let (runner, state) = fixture.runner(false);
        let mut output = Vec::new();
        assert_eq!(
            run_command(
                &fixture.home,
                unseal_command(&fixture),
                Some(b"authorized-requester"),
                TEST_NOW,
                &runner,
                &mut output,
            ),
            Err(OperatorError::Artifact)
        );
        assert!(output.is_empty());
        let state = state.lock().unwrap();
        assert_eq!(state.approval_calls, 1);
        assert_eq!(state.custody_calls, 1);
        drop(state);
        let ledger = DisclosureLedger::open(&fixture.ledger, &fixture.checkpoint_secret).unwrap();
        assert!(matches!(
            ledger.leaf(1).unwrap(),
            Some(DisclosureLeaf::Completion(ref completion))
                if completion.status == CompletionStatus::Failed
                    && completion.record_count == 0
        ));
    }

    #[cfg_attr(
        not(any(target_os = "linux", target_os = "macos")),
        ignore = "persistent offline custody is supported only on Linux and macOS"
    )]
    #[test]
    fn locally_configured_but_unpublished_custody_key_is_refused() {
        let fixture = Fixture::new();
        let config = OperatorConfig::load(&fixture.home).unwrap();
        let published =
            configured_custody_public_key(config.epoch(fixture.key_id).unwrap(), fixture.key_id)
                .unwrap();
        assert!(
            verify_registry_key(&config.registry_audit, fixture.key_id, published, TEST_NOW,)
                .is_ok()
        );

        let forged = ComplianceKeyId::new(
            fixture.key_id.purpose,
            fixture.key_id.jurisdiction,
            [0xA4; 32],
            fixture.key_id.epoch_start_ms,
            fixture.key_id.generation,
        );
        assert_eq!(
            verify_registry_key(&config.registry_audit, forged, published, TEST_NOW),
            Err(OperatorError::Authorization)
        );
        assert_eq!(
            verify_registry_key(&config.registry_audit, fixture.key_id, [0x5A; 32], TEST_NOW,),
            Err(OperatorError::Authorization)
        );
    }

    #[cfg_attr(
        not(any(target_os = "linux", target_os = "macos")),
        ignore = "persistent offline custody is supported only on Linux and macOS"
    )]
    #[test]
    fn complete_registry_replay_refuses_a_later_revocation() {
        let fixture = Fixture::new();
        let config = OperatorConfig::load(&fixture.home).unwrap();
        let public_key =
            configured_custody_public_key(config.epoch(fixture.key_id).unwrap(), fixture.key_id)
                .unwrap();
        let publications = [ComplianceKeyStatus::Active, ComplianceKeyStatus::Revoked]
            .into_iter()
            .enumerate()
            .map(|(seq, status)| {
                LogEntry::compliance_key(
                    seq as u64,
                    ComplianceKeyPublish {
                        key_id: fixture.key_id,
                        public_key: hex_encode(&public_key),
                        not_before_ms: fixture.key_id.epoch_start_ms,
                        not_after_ms: fixture
                            .key_id
                            .epoch_start_ms
                            .checked_add(pigeonpost_compliance_seal::TRACE_EPOCH_DURATION_MS)
                            .unwrap(),
                        status,
                    },
                    TEST_NOW,
                )
            })
            .collect::<Vec<_>>();
        let mut frontier = MerkleFrontier::new();
        let mut log = Vec::new();
        for entry in &publications {
            serde_json::to_writer(&mut log, entry).unwrap();
            log.push(b'\n');
            frontier.append(&entry.leaf_bytes().unwrap()).unwrap();
        }
        fs::remove_file(&config.registry_audit.log_path).unwrap();
        write_private(&config.registry_audit.log_path, &log);
        let checkpoint = Checkpoint {
            origin: config.registry_audit.expected_origin.clone(),
            size: frontier.size(),
            root: frontier.root().unwrap(),
        };
        let registry_signer = SigningKey::from_bytes(&[41u8; 32]);
        let witness = SigningKey::from_bytes(&[42u8; 32]);
        let mut note = checkpoint.sign(&registry_signer);
        note.push_str(
            &checkpoint
                .cosignature_line("independent.test", &witness, TEST_NOW / 1_000)
                .unwrap(),
        );
        fs::remove_file(&config.registry_audit.checkpoint_path).unwrap();
        write_private(&config.registry_audit.checkpoint_path, note.as_bytes());

        assert_eq!(
            verify_registry_key(&config.registry_audit, fixture.key_id, public_key, TEST_NOW,),
            Err(OperatorError::Authorization)
        );
    }

    #[cfg_attr(
        not(any(target_os = "linux", target_os = "macos")),
        ignore = "persistent offline custody is supported only on Linux and macOS"
    )]
    #[test]
    fn registry_replay_rejects_noncanonical_trace_and_attribution_epochs() {
        let fixture = Fixture::new();
        let public_key = MontgomeryPoint::mul_base_clamped(fixture.custody_secret).to_bytes();
        let publication = |key_id, not_after_ms| ComplianceKeyPublish {
            key_id,
            public_key: hex_encode(&public_key),
            not_before_ms: key_id.epoch_start_ms,
            not_after_ms,
            status: ComplianceKeyStatus::Active,
        };
        let mut keys = HashMap::new();
        assert_eq!(
            apply_registry_publication(&mut keys, &publication(fixture.key_id, 2 * DAY_MS), 0,),
            Err(OperatorError::Authorization)
        );
        let unaligned = ComplianceKeyId::new(
            CompliancePurpose::NetworkTrace,
            Jurisdiction::Test,
            [44; 32],
            1,
            1,
        );
        assert_eq!(
            apply_registry_publication(&mut keys, &publication(unaligned, DAY_MS + 1), 0),
            Err(OperatorError::Authorization)
        );

        const FEBRUARY_2024: u64 = 1_706_745_600_000;
        const MARCH_2024: u64 = 1_709_251_200_000;
        let attribution = ComplianceKeyId::new(
            CompliancePurpose::Attribution,
            Jurisdiction::Eu,
            [45; 32],
            FEBRUARY_2024,
            1,
        );
        assert!(apply_registry_publication(
            &mut HashMap::new(),
            &publication(attribution, MARCH_2024),
            0,
        )
        .is_ok());
        assert_eq!(
            apply_registry_publication(
                &mut HashMap::new(),
                &publication(attribution, FEBRUARY_2024 + 31 * DAY_MS),
                0,
            ),
            Err(OperatorError::Authorization)
        );
        let mid_month = ComplianceKeyId::new(
            CompliancePurpose::Attribution,
            Jurisdiction::Eu,
            [46; 32],
            FEBRUARY_2024 + DAY_MS,
            1,
        );
        assert_eq!(
            apply_registry_publication(&mut HashMap::new(), &publication(mid_month, MARCH_2024), 0,),
            Err(OperatorError::Authorization)
        );
    }

    #[cfg_attr(
        not(any(target_os = "linux", target_os = "macos")),
        ignore = "persistent offline custody is supported only on Linux and macOS"
    )]
    #[test]
    fn duplicate_approver_is_refused_before_intent_or_output() {
        let fixture = Fixture::new();
        let (runner, state) = fixture.runner(true);
        let mut output = Vec::new();
        assert_eq!(
            run_command(
                &fixture.home,
                unseal_command(&fixture),
                Some(b"authorized-requester"),
                TEST_NOW,
                &runner,
                &mut output,
            ),
            Err(OperatorError::Authorization)
        );
        assert!(output.is_empty());
        assert!(!fixture.ledger.exists());
        assert_eq!(state.lock().unwrap().custody_calls, 0);
    }

    #[cfg_attr(
        not(any(target_os = "linux", target_os = "macos")),
        ignore = "persistent offline custody is supported only on Linux and macOS"
    )]
    #[test]
    fn custody_failure_gets_a_durable_failure_leaf_and_no_plaintext() {
        let fixture = Fixture::new();
        let (mut runner, state) = fixture.runner(false);
        runner.custody_secret = [99u8; 32];
        let mut output = Vec::new();
        assert_eq!(
            run_command(
                &fixture.home,
                unseal_command(&fixture),
                Some(b"authorized-requester"),
                TEST_NOW,
                &runner,
                &mut output,
            ),
            Err(OperatorError::Custody)
        );
        assert!(output.is_empty());
        assert!(state.lock().unwrap().intent_seen_before_custody);
        let ledger = DisclosureLedger::open(&fixture.ledger, &fixture.checkpoint_secret).unwrap();
        assert_eq!(ledger.leaf_count(), 2);
        assert!(matches!(
            ledger.leaf(1).unwrap(),
            Some(DisclosureLeaf::Completion(ref completion))
                if completion.status == CompletionStatus::Failed
                    && completion.record_count == 0
        ));
    }

    #[cfg_attr(
        not(any(target_os = "linux", target_os = "macos")),
        ignore = "persistent offline custody is supported only on Linux and macOS"
    )]
    #[test]
    fn hold_persists_and_blocks_the_default_shred_dry_run() {
        let fixture = Fixture::new();
        let (runner, state) = fixture.runner(false);
        let mut output = Vec::new();
        run_command(
            &fixture.home,
            ParsedCommand::Hold {
                epoch: hex_encode(&fixture.key_id.encode().unwrap()),
                order_reference: Zeroizing::new("preservation-order".to_string()),
                action: HoldAction::Place {
                    until: "1970-01-04".to_string(),
                },
            },
            None,
            2 * DAY_MS + 2,
            &runner,
            &mut output,
        )
        .unwrap();
        output.clear();
        run_command(
            &fixture.home,
            ParsedCommand::Shred {
                before: "1970-01-02".to_string(),
                execute: false,
            },
            None,
            2 * DAY_MS + 3,
            &runner,
            &mut output,
        )
        .unwrap();
        assert!(String::from_utf8(output)
            .unwrap()
            .contains("skipped_held_epochs=1"));
        let inventory =
            load_inventory(&fixture.inventory, fixture.key_id, fixture.retention_policy).unwrap();
        assert_eq!(inventory.state(), InventoryState::Retained);
        assert_eq!(inventory.holds().len(), 1);
        assert_eq!(state.lock().unwrap().approval_calls, 1);
    }

    #[cfg_attr(
        not(any(target_os = "linux", target_os = "macos")),
        ignore = "persistent offline custody is supported only on Linux and macOS"
    )]
    #[test]
    fn hold_renewal_and_release_are_authorized_durable_and_preserve_lineage() {
        let fixture = Fixture::new();
        let (runner, state) = fixture.runner(false);
        let epoch = hex_encode(&fixture.key_id.encode().unwrap());
        let mut output = Vec::new();
        run_command(
            &fixture.home,
            ParsedCommand::Hold {
                epoch: epoch.clone(),
                order_reference: Zeroizing::new("preservation-order".to_string()),
                action: HoldAction::Place {
                    until: "1970-01-03".to_string(),
                },
            },
            None,
            DAY_MS + 2,
            &runner,
            &mut output,
        )
        .unwrap();
        let placed = String::from_utf8(output).unwrap();
        let placed = parse_hold_id(placed.trim().strip_prefix("hold_id=").unwrap()).unwrap();

        let mut output = Vec::new();
        run_command(
            &fixture.home,
            ParsedCommand::Hold {
                epoch: epoch.clone(),
                order_reference: Zeroizing::new("renewal-order".to_string()),
                action: HoldAction::Renew {
                    prior_hold_id: placed,
                    until: "1970-01-04".to_string(),
                },
            },
            None,
            DAY_MS + 3,
            &runner,
            &mut output,
        )
        .unwrap();
        let renewed = String::from_utf8(output).unwrap();
        let renewed = parse_hold_id(renewed.trim().strip_prefix("hold_id=").unwrap()).unwrap();
        let inventory =
            load_inventory(&fixture.inventory, fixture.key_id, fixture.retention_policy).unwrap();
        assert_eq!(inventory.holds().len(), 2);
        assert_eq!(inventory.holds()[1].renews(), Some(placed));

        let mut output = Vec::new();
        run_command(
            &fixture.home,
            ParsedCommand::Hold {
                epoch,
                order_reference: Zeroizing::new("release-order".to_string()),
                action: HoldAction::Release { hold_id: renewed },
            },
            None,
            DAY_MS * 3 + 2,
            &runner,
            &mut output,
        )
        .unwrap();
        assert_eq!(
            String::from_utf8(output).unwrap(),
            format!("released_hold_id={}\n", hex_encode(&renewed))
        );
        let inventory =
            load_inventory(&fixture.inventory, fixture.key_id, fixture.retention_policy).unwrap();
        assert_eq!(inventory.holds()[1].released_at_ms(), Some(DAY_MS * 3 + 2));
        assert_eq!(state.lock().unwrap().approval_calls, 3);

        let mut output = Vec::new();
        run_command(
            &fixture.home,
            ParsedCommand::Shred {
                before: "1970-01-02".to_string(),
                execute: true,
            },
            None,
            DAY_MS * 3 + 3,
            &runner,
            &mut output,
        )
        .unwrap();
        assert_eq!(
            load_inventory(&fixture.inventory, fixture.key_id, fixture.retention_policy)
                .unwrap()
                .state(),
            InventoryState::Shredded
        );
    }

    #[cfg_attr(
        not(any(target_os = "linux", target_os = "macos")),
        ignore = "persistent offline custody is supported only on Linux and macOS"
    )]
    #[test]
    fn duplicate_hold_approvers_are_rejected_without_mutating_inventory() {
        let fixture = Fixture::new();
        let (runner, state) = fixture.runner(true);
        let mut output = Vec::new();
        assert_eq!(
            run_command(
                &fixture.home,
                ParsedCommand::Hold {
                    epoch: hex_encode(&fixture.key_id.encode().unwrap()),
                    order_reference: Zeroizing::new("preservation-order".to_string()),
                    action: HoldAction::Place {
                        until: "1970-01-03".to_string(),
                    },
                },
                None,
                DAY_MS + 2,
                &runner,
                &mut output,
            ),
            Err(OperatorError::Authorization)
        );
        assert!(output.is_empty());
        let inventory =
            load_inventory(&fixture.inventory, fixture.key_id, fixture.retention_policy).unwrap();
        assert!(inventory.holds().is_empty());
        assert_eq!(state.lock().unwrap().approval_calls, 1);
    }

    #[cfg_attr(
        not(any(target_os = "linux", target_os = "macos")),
        ignore = "persistent offline custody is supported only on Linux and macOS"
    )]
    #[test]
    fn shred_is_dry_by_default_and_execute_receipts_every_present_copy() {
        let fixture = Fixture::new();
        let (runner, state) = fixture.runner(false);
        let mut output = Vec::new();
        run_command(
            &fixture.home,
            ParsedCommand::Shred {
                before: "1970-01-02".to_string(),
                execute: false,
            },
            None,
            2 * DAY_MS + 2,
            &runner,
            &mut output,
        )
        .unwrap();
        assert!(String::from_utf8(output).unwrap().contains("mode=dry_run"));
        assert_eq!(
            load_inventory(&fixture.inventory, fixture.key_id, fixture.retention_policy,)
                .unwrap()
                .state(),
            InventoryState::Retained
        );
        assert_eq!(state.lock().unwrap().destruction_calls, 0);

        let mut output = Vec::new();
        run_command(
            &fixture.home,
            ParsedCommand::Shred {
                before: "1970-01-02".to_string(),
                execute: true,
            },
            None,
            2 * DAY_MS + 2,
            &runner,
            &mut output,
        )
        .unwrap();
        assert!(String::from_utf8(output)
            .unwrap()
            .contains("shredded_epochs=1"));
        assert_eq!(state.lock().unwrap().destruction_calls, 2);
        assert_eq!(
            load_inventory(&fixture.inventory, fixture.key_id, fixture.retention_policy,)
                .unwrap()
                .state(),
            InventoryState::Shredded
        );
    }

    #[cfg_attr(
        not(any(target_os = "linux", target_os = "macos")),
        ignore = "persistent offline custody is supported only on Linux and macOS"
    )]
    #[test]
    fn shred_discovery_retains_only_bounded_copyable_epoch_summaries() {
        fn assert_copy<T: Copy>() {}

        // This is a structural regression: an authenticated epoch owns an open directory guard
        // and its decoded manifest, so it cannot be copied into either bounded discovery type.
        // Keeping both types small also prevents replacing the handle with another manifest-sized
        // owned value while preserving deterministic, platform-independent coverage.
        assert_copy::<ShredTraceAssessment>();
        assert_copy::<ShredCandidate<'static>>();
        assert!(core::mem::size_of::<ShredTraceAssessment>() <= 64);
        assert!(core::mem::size_of::<ShredCandidate<'static>>() <= 128);

        let fixture = Fixture::new();
        let mut inventories = vec![(fixture.key_id, fixture.inventory.clone())];
        for day in 1..=8u64 {
            inventories.push(append_test_epoch(
                &fixture,
                day * DAY_MS,
                day as u32,
                &format!("stream-{day}"),
            ));
        }
        let (runner, state) = fixture.runner(false);
        let now_ms = 10 * DAY_MS + 2;
        let before = "1970-01-10";

        let mut output = Vec::new();
        run_command(
            &fixture.home,
            ParsedCommand::Shred {
                before: before.to_string(),
                execute: false,
            },
            None,
            now_ms,
            &runner,
            &mut output,
        )
        .unwrap();
        let report = String::from_utf8(output).unwrap();
        assert!(report.contains("eligible_epochs=9"));
        assert!(report.contains("trace_integrity_degraded_epochs=0"));
        assert_eq!(state.lock().unwrap().destruction_calls, 0);

        let mut output = Vec::new();
        run_command(
            &fixture.home,
            ParsedCommand::Shred {
                before: before.to_string(),
                execute: true,
            },
            None,
            now_ms,
            &runner,
            &mut output,
        )
        .unwrap();
        let report = String::from_utf8(output).unwrap();
        assert!(report.contains("shredded_epochs=9"));
        assert!(report.contains("failed_epochs=0"));
        assert_eq!(state.lock().unwrap().destruction_calls, 18);
        for (key_id, path) in inventories {
            assert_eq!(
                load_inventory(&path, key_id, fixture.retention_policy)
                    .unwrap()
                    .state(),
                InventoryState::Shredded
            );
        }
    }

    #[cfg_attr(
        not(any(target_os = "linux", target_os = "macos")),
        ignore = "persistent offline custody is supported only on Linux and macOS"
    )]
    #[test]
    fn shred_rejects_manifest_change_between_discovery_and_execution() {
        let fixture = Fixture::new();
        let config = OperatorConfig::load(&fixture.home).unwrap();
        let epoch = config.epoch(fixture.key_id).unwrap();
        let initial = assess_configured_trace_epoch(epoch, fixture.key_id, TEST_NOW)
            .unwrap()
            .unwrap();

        // Replace the terminal marker with a different, otherwise valid manifest signed by the
        // same pinned producer key. Its body set is degraded because the prior segment remains,
        // but destruction must fail on the commitment change before recording evidence or calling
        // the custodian.
        let custody_public = MontgomeryPoint::mul_base_clamped(fixture.custody_secret).to_bytes();
        publish_test_manifest(
            &fixture.trace_directory,
            fixture.key_id,
            [6; 32],
            custody_public,
            &SigningKey::from_bytes(&[7; 32]),
            Vec::new(),
        );

        let mut inventory =
            load_inventory(&fixture.inventory, fixture.key_id, fixture.retention_policy).unwrap();
        let (runner, state) = fixture.runner(false);
        let mut degraded = initial.is_degraded();
        assert_eq!(
            shred_epoch(
                epoch,
                &mut inventory,
                TEST_NOW,
                config.destruction_command.as_ref().unwrap(),
                &runner,
                Some(initial),
                &mut degraded,
            ),
            Err(OperatorError::Artifact)
        );
        assert_eq!(state.lock().unwrap().destruction_calls, 0);
        assert_eq!(inventory.state(), InventoryState::Retained);
        assert_eq!(inventory.trace_integrity(), None);
        let persisted =
            load_inventory(&fixture.inventory, fixture.key_id, fixture.retention_policy).unwrap();
        assert_eq!(persisted.state(), InventoryState::Retained);
        assert_eq!(persisted.trace_integrity(), None);
    }

    #[cfg_attr(
        not(any(target_os = "linux", target_os = "macos")),
        ignore = "persistent offline custody is supported only on Linux and macOS"
    )]
    #[test]
    fn shred_skips_a_held_epoch_but_completes_an_independent_eligible_epoch() {
        let fixture = Fixture::new();
        let (second_key, second_inventory) = append_test_epoch(&fixture, DAY_MS, 1, "second");
        let (runner, state) = fixture.runner(false);
        let mut output = Vec::new();
        run_command(
            &fixture.home,
            ParsedCommand::Hold {
                epoch: hex_encode(&fixture.key_id.encode().unwrap()),
                order_reference: Zeroizing::new("preservation-order".to_string()),
                action: HoldAction::Place {
                    until: "1970-01-05".to_string(),
                },
            },
            None,
            DAY_MS * 3 + 2,
            &runner,
            &mut output,
        )
        .unwrap();
        output.clear();
        run_command(
            &fixture.home,
            ParsedCommand::Shred {
                before: "1970-01-03".to_string(),
                execute: true,
            },
            None,
            DAY_MS * 3 + 3,
            &runner,
            &mut output,
        )
        .unwrap();
        let output = String::from_utf8(output).unwrap();
        assert!(output.contains("shredded_epochs=1"));
        assert!(output.contains("skipped_held_epochs=1"));
        assert_eq!(
            load_inventory(&fixture.inventory, fixture.key_id, fixture.retention_policy)
                .unwrap()
                .state(),
            InventoryState::Retained
        );
        assert_eq!(
            load_inventory(&second_inventory, second_key, fixture.retention_policy)
                .unwrap()
                .state(),
            InventoryState::Shredded
        );
        assert_eq!(state.lock().unwrap().destruction_calls, 2);
    }

    #[cfg_attr(
        not(any(target_os = "linux", target_os = "macos")),
        ignore = "persistent offline custody is supported only on Linux and macOS"
    )]
    #[test]
    fn shredding_resumes_after_a_partial_adapter_failure() {
        let fixture = Fixture::new();
        let (mut runner, state) = fixture.runner(false);
        runner.fail_destruction_call = Some(2);
        let mut output = Vec::new();
        assert_eq!(
            run_command(
                &fixture.home,
                ParsedCommand::Shred {
                    before: "1970-01-02".to_string(),
                    execute: true,
                },
                None,
                2 * DAY_MS + 2,
                &runner,
                &mut output,
            ),
            Err(OperatorError::Custody)
        );
        let inventory =
            load_inventory(&fixture.inventory, fixture.key_id, fixture.retention_policy).unwrap();
        assert_eq!(inventory.state(), InventoryState::Shredding);
        assert_eq!(
            inventory
                .copies()
                .iter()
                .filter(|copy| copy.state() == CopyState::Destroyed)
                .count(),
            1
        );

        let mut output = Vec::new();
        run_command(
            &fixture.home,
            ParsedCommand::Shred {
                before: "1970-01-02".to_string(),
                execute: true,
            },
            None,
            2 * DAY_MS + 3,
            &runner,
            &mut output,
        )
        .unwrap();
        assert!(String::from_utf8(output)
            .unwrap()
            .contains("resumed_epochs=1"));
        assert_eq!(
            load_inventory(&fixture.inventory, fixture.key_id, fixture.retention_policy)
                .unwrap()
                .state(),
            InventoryState::Shredded
        );
        assert_eq!(state.lock().unwrap().destruction_calls, 3);
    }

    #[cfg_attr(
        not(any(target_os = "linux", target_os = "macos")),
        ignore = "persistent offline custody is supported only on Linux and macOS"
    )]
    #[test]
    fn one_failed_epoch_does_not_prevent_other_eligible_epochs_from_shredding() {
        let fixture = Fixture::new();
        let (second_key, second_inventory) = append_test_epoch(&fixture, DAY_MS, 1, "second");
        let (mut runner, _) = fixture.runner(false);
        runner.fail_destruction_call = Some(1);
        let mut output = Vec::new();
        assert_eq!(
            run_command(
                &fixture.home,
                ParsedCommand::Shred {
                    before: "1970-01-03".to_string(),
                    execute: true,
                },
                None,
                DAY_MS * 3 + 2,
                &runner,
                &mut output,
            ),
            Err(OperatorError::Custody)
        );
        let output = String::from_utf8(output).unwrap();
        assert!(output.contains("shredded_epochs=1"));
        assert!(output.contains("failed_epochs=1"));
        assert_eq!(
            load_inventory(&fixture.inventory, fixture.key_id, fixture.retention_policy)
                .unwrap()
                .state(),
            InventoryState::Shredding
        );
        assert_eq!(
            load_inventory(&second_inventory, second_key, fixture.retention_policy)
                .unwrap()
                .state(),
            InventoryState::Shredded
        );
    }

    #[cfg_attr(
        not(any(target_os = "linux", target_os = "macos")),
        ignore = "persistent offline custody is supported only on Linux and macOS"
    )]
    #[test]
    fn corrupt_inventory_is_reported_without_blocking_later_eligible_epochs() {
        let fixture = Fixture::new();
        let (second_key, second_inventory) = append_test_epoch(&fixture, DAY_MS, 1, "second");
        replace_private(&fixture.inventory, b"corrupt inventory");
        let (runner, state) = fixture.runner(false);
        let mut output = Vec::new();

        assert_eq!(
            run_command(
                &fixture.home,
                ParsedCommand::Shred {
                    before: "1970-01-03".to_string(),
                    execute: true,
                },
                None,
                DAY_MS * 3 + 2,
                &runner,
                &mut output,
            ),
            Err(OperatorError::State)
        );

        assert_eq!(
            String::from_utf8(output).unwrap(),
            "mode=execute\nshredded_epochs=1\nresumed_epochs=0\n\
             skipped_held_epochs=0\nskipped_unexpired_epochs=0\n\
             already_shredded_epochs=0\ndiscovery_failed_epochs=1\n\
             execution_failed_epochs=0\ntrace_integrity_degraded_epochs=0\n\
             failed_epochs=1\n"
        );
        assert_eq!(state.lock().unwrap().destruction_calls, 2);
        assert_eq!(
            load_inventory(&second_inventory, second_key, fixture.retention_policy)
                .unwrap()
                .state(),
            InventoryState::Shredded
        );
    }

    #[cfg_attr(
        not(any(target_os = "linux", target_os = "macos")),
        ignore = "persistent offline custody is supported only on Linux and macOS"
    )]
    #[test]
    fn checkpoint_is_signed_with_the_dedicated_offline_key() {
        let fixture = Fixture::new();
        DisclosureLedger::create(&fixture.ledger, &fixture.checkpoint_secret).unwrap();
        let config = OperatorConfig::load(&fixture.home).unwrap();
        let mut output = Vec::new();
        command_checkpoint(&fixture.home, &config, &mut output).unwrap();
        let text = String::from_utf8(output).unwrap();
        assert_eq!(
            fs::read_to_string(&fixture.checkpoint_output).unwrap(),
            text
        );
        let key = SigningKey::from_bytes(&fixture.checkpoint_secret);
        let parsed = pigeonpost_registry::Checkpoint::verify(&text, &key.verifying_key()).unwrap();
        assert_eq!(parsed.origin, "pigeonpost.test/disclosures");
        assert_eq!(parsed.size, 0);

        let mut second_output = Vec::new();
        command_checkpoint(&fixture.home, &config, &mut second_output).unwrap();
        assert_eq!(second_output, text.as_bytes());
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            assert_eq!(
                fs::metadata(&fixture.checkpoint_output)
                    .unwrap()
                    .permissions()
                    .mode()
                    & 0o777,
                0o600
            );
        }
    }

    #[cfg_attr(
        not(any(target_os = "linux", target_os = "macos")),
        ignore = "persistent offline custody is supported only on Linux and macOS"
    )]
    #[test]
    fn checkpoint_and_status_refuse_a_newer_signed_floor_than_the_ledger() {
        let fixture = Fixture::new();
        DisclosureLedger::create(&fixture.ledger, &fixture.checkpoint_secret).unwrap();
        let key = SigningKey::from_bytes(&fixture.checkpoint_secret);
        let newer = Checkpoint {
            origin: "pigeonpost.test/disclosures".to_owned(),
            size: 1,
            root: [0xA5; 32],
        }
        .sign(&key);
        write_private(&fixture.checkpoint_output, newer.as_bytes());
        let config = OperatorConfig::load(&fixture.home).unwrap();

        assert_eq!(
            command_checkpoint(&fixture.home, &config, &mut Vec::new()),
            Err(OperatorError::State)
        );
        assert_eq!(
            command_status(&fixture.home, &config, TEST_NOW, &mut Vec::new()),
            Err(OperatorError::State)
        );
        assert_eq!(
            fs::read_to_string(&fixture.checkpoint_output).unwrap(),
            newer
        );
    }

    #[cfg_attr(
        not(any(target_os = "linux", target_os = "macos")),
        ignore = "persistent offline custody is supported only on Linux and macOS"
    )]
    #[test]
    fn nonempty_ledger_refuses_a_missing_checkpoint_floor() {
        let fixture = Fixture::new();
        let (runner, _) = fixture.runner(false);
        run_command(
            &fixture.home,
            unseal_command(&fixture),
            Some(b"authorized-requester"),
            TEST_NOW,
            &runner,
            &mut Vec::new(),
        )
        .unwrap();
        fs::remove_file(&fixture.checkpoint_output).unwrap();
        let config = OperatorConfig::load(&fixture.home).unwrap();

        assert_eq!(
            command_status(&fixture.home, &config, TEST_NOW, &mut Vec::new()),
            Err(OperatorError::State)
        );
    }

    #[cfg_attr(
        not(any(target_os = "linux", target_os = "macos")),
        ignore = "persistent offline custody is supported only on Linux and macOS"
    )]
    #[test]
    fn parsing_and_date_edges_preserve_safe_defaults() {
        let mut version = Vec::new();
        run_from_env([OsString::from("--version")], &mut &b""[..], &mut version).unwrap();
        assert_eq!(
            String::from_utf8(version).unwrap(),
            format!("ppcompliance {}\n", env!("CARGO_PKG_VERSION"))
        );
        assert!(parse_command([OsString::from("-V"), OsString::from("extra")]).is_err());

        assert!(matches!(
            parse_command([
                OsString::from("shred"),
                OsString::from("--before"),
                OsString::from("2026-08-09")
            ])
            .unwrap(),
            ParsedCommand::Shred { execute: false, .. }
        ));
        assert!(parse_command([
            OsString::from("unseal"),
            OsString::from("--epoch"),
            OsString::from("00"),
            OsString::from("--selector"),
            OsString::from("event_id=00"),
        ])
        .is_err());
        let fixture = Fixture::new();
        let epoch = hex_encode(&fixture.key_id.encode().unwrap());
        assert!(matches!(
            parse_command([
                OsString::from("inventory"),
                OsString::from("create"),
                OsString::from("--epoch"),
                OsString::from(&epoch),
            ])
            .unwrap(),
            ParsedCommand::Inventory {
                action: InventoryAction::Create,
                ..
            }
        ));
        assert!(parse_command([
            OsString::from("inventory"),
            OsString::from("replace"),
            OsString::from("--epoch"),
            OsString::from(&epoch),
        ])
        .is_err());
        assert!(parse_command([
            OsString::from("inventory"),
            OsString::from("import"),
            OsString::from("--epoch"),
            OsString::from(&epoch),
            OsString::from("extra"),
        ])
        .is_err());
        let hold_id = hex_encode(&[4; 32]);
        assert!(matches!(
            parse_command([
                OsString::from("hold"),
                OsString::from("renew"),
                OsString::from("--epoch"),
                OsString::from(&epoch),
                OsString::from("--hold"),
                OsString::from(&hold_id),
                OsString::from("--until"),
                OsString::from("2026-09-01"),
            ])
            .unwrap(),
            ParsedCommand::HoldRequest {
                action: HoldAction::Renew { .. },
                ..
            }
        ));
        assert!(matches!(
            parse_command([
                OsString::from("hold"),
                OsString::from("release"),
                OsString::from("--epoch"),
                OsString::from(&epoch),
                OsString::from("--hold"),
                OsString::from(&hold_id),
            ])
            .unwrap(),
            ParsedCommand::HoldRequest {
                action: HoldAction::Release { .. },
                ..
            }
        ));
        assert!(parse_command([
            OsString::from("hold"),
            OsString::from("release"),
            OsString::from("--epoch"),
            OsString::from(&epoch),
            OsString::from("--hold"),
            OsString::from(hex_encode(&[0; 32])),
        ])
        .is_err());
        assert_eq!(utc_date_start_ms("1970-01-01").unwrap(), 0);
        assert_eq!(utc_date_start_ms("1970-01-02").unwrap(), DAY_MS);
        assert!(utc_date_start_ms("2025-02-29").is_err());
        assert!(utc_date_start_ms("2024-02-29").is_ok());
    }

    #[cfg_attr(
        not(any(target_os = "linux", target_os = "macos")),
        ignore = "persistent offline custody is supported only on Linux and macOS"
    )]
    #[test]
    fn sensitive_case_values_are_strict_bounded_stdin_only() {
        let fixture = Fixture::new();
        let epoch = hex_encode(&fixture.key_id.encode().unwrap());
        let parsed = parse_command([
            OsString::from("unseal"),
            OsString::from("--epoch"),
            OsString::from(&epoch),
        ])
        .unwrap();
        let declaration = b"version = 1\norder_reference = \"case-42\"\nrequester_identity = \"authorized investigator\"\nselectors = [\"event_id=00112233445566778899aabbccddeeff00112233445566778899aabbccddeeff\"]\n";
        let (hydrated, requester) = hydrate_private_command(parsed, &mut &declaration[..]).unwrap();
        assert_eq!(
            requester.as_deref().map(String::as_str),
            Some("authorized investigator")
        );
        assert!(matches!(
            hydrated,
            ParsedCommand::Unseal {
                order_reference,
                selectors,
                ..
            } if order_reference.as_str() == "case-42" && selectors.len() == 1
        ));

        assert!(parse_command([
            OsString::from("unseal"),
            OsString::from("--epoch"),
            OsString::from(&epoch),
            OsString::from("--order-ref"),
            OsString::from("must-not-enter-argv"),
        ])
        .is_err());
        assert!(parse_command([
            OsString::from("hold"),
            OsString::from("--epoch"),
            OsString::from(&epoch),
            OsString::from("--order-ref"),
            OsString::from("must-not-enter-argv"),
            OsString::from("--until"),
            OsString::from("2026-09-01"),
        ])
        .is_err());

        let command = ParsedCommand::HoldRequest {
            epoch: epoch.clone(),
            action: HoldAction::Place {
                until: "2026-09-01".to_string(),
            },
        };
        assert!(hydrate_private_command(
            command,
            &mut &b"version = 2\norder_reference = \"case-42\"\n"[..],
        )
        .is_err());
        let command = ParsedCommand::HoldRequest {
            epoch,
            action: HoldAction::Place {
                until: "2026-09-01".to_string(),
            },
        };
        assert!(hydrate_private_command(
            command,
            &mut &b"version = 1\norder_reference = \"case-42\"\nunexpected = true\n"[..],
        )
        .is_err());

        let oversized = vec![b'x'; PRIVATE_REQUEST_MAX_BYTES as usize + 1];
        assert!(matches!(
            read_private_request::<HoldRequestDeclaration>(&mut &oversized[..]),
            Err(OperatorError::Limit)
        ));
    }

    #[test]
    fn software_custody_is_test_only_and_explicit() {
        #[cfg(windows)]
        let secret_key_path = PathBuf::from(r"C:\offline\dev.key");
        #[cfg(not(windows))]
        let secret_key_path = PathBuf::from("/offline/dev.key");
        let config = CustodyConfig {
            mode: CustodyMode::SoftwareDevelopment,
            public_key: None,
            command: None,
            secret_key_path: Some(secret_key_path),
        };
        let production = ComplianceKeyId::new(
            CompliancePurpose::NetworkTrace,
            Jurisdiction::Us,
            [1; 32],
            1,
            1,
        );
        assert_eq!(
            config.validate(production),
            Err(OperatorError::Configuration)
        );
        let test = ComplianceKeyId::new(
            CompliancePurpose::NetworkTrace,
            Jurisdiction::Test,
            [1; 32],
            1,
            1,
        );
        assert!(config.validate(test).is_ok());
    }

    #[cfg(unix)]
    #[test]
    fn process_adapter_bounds_output_and_enforces_deadline_without_a_shell() {
        let runner = ProcessRunner;
        let oversized = CommandConfig {
            executable: PathBuf::from("/usr/bin/printf").canonicalize().unwrap(),
            args: vec!["abcde".to_string()],
            timeout_ms: 1_000,
            inherit_environment: Vec::new(),
        };
        assert_eq!(runner.run(&oversized, b"", 4), Err(OperatorError::Custody));
        let sleeping = CommandConfig {
            executable: PathBuf::from("/bin/sleep").canonicalize().unwrap(),
            args: vec!["1".to_string()],
            timeout_ms: MIN_COMMAND_TIMEOUT_MS,
            inherit_environment: Vec::new(),
        };
        assert_eq!(runner.run(&sleeping, b"", 32), Err(OperatorError::Custody));
    }

    #[cfg(unix)]
    #[test]
    fn adapter_parent_exit_terminates_a_descendant_that_retains_stdout() {
        let runner = ProcessRunner;
        let command = CommandConfig {
            executable: PathBuf::from("/bin/sh").canonicalize().unwrap(),
            args: vec!["-c".into(), "sleep 10 & printf ok".into()],
            timeout_ms: 1_000,
            inherit_environment: Vec::new(),
        };
        assert_eq!(runner.run(&command, b"", 32).unwrap(), b"ok");
    }

    #[cfg(unix)]
    #[test]
    fn adapter_deadline_covers_simultaneous_stdin_and_stdout_backpressure() {
        let runner = ProcessRunner;
        let command = CommandConfig {
            executable: PathBuf::from("/bin/sh").canonicalize().unwrap(),
            args: vec![
                "-c".into(),
                "/bin/dd if=/dev/zero bs=65536 count=2 2>/dev/null; /bin/cat >/dev/null".into(),
            ],
            timeout_ms: 1_000,
            inherit_environment: Vec::new(),
        };
        let input = vec![0xA5; 256 * 1024];
        let output = runner.run(&command, &input, 128 * 1024).unwrap();
        assert_eq!(output.len(), 128 * 1024);
        assert!(output.iter().all(|byte| *byte == 0));
    }

    #[cfg(unix)]
    #[test]
    fn adapter_executable_rejects_symlinks_and_mutable_hardlinks() {
        use std::os::unix::fs::{symlink, PermissionsExt};

        let temp = tempfile::tempdir().unwrap();
        let root = temp.path().canonicalize().unwrap();
        let executable = root.join("adapter");
        write_private(&executable, b"placeholder");
        fs::set_permissions(&executable, fs::Permissions::from_mode(0o700)).unwrap();
        let alias = root.join("adapter-hardlink");
        fs::hard_link(&executable, &alias).unwrap();
        assert_eq!(
            validate_executable(&executable),
            Err(OperatorError::Configuration)
        );

        let link = root.join("adapter-symlink");
        symlink(
            PathBuf::from("/usr/bin/printf").canonicalize().unwrap(),
            &link,
        )
        .unwrap();
        assert_eq!(
            validate_executable(&link),
            Err(OperatorError::Configuration)
        );
    }
}
